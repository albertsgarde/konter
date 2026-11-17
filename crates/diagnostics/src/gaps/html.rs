//! The gaps report as the body of a web page: the same sections as the text,
//! as tables. The page around it (look, navigation) belongs to the site.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use jiff::civil::Date;
use rust_decimal::Decimal;

use super::{
    Flow, Gaps, Totals, later_note, mapped_percent, money, percent, plural, policy, raw_description, signed, tidy,
};
use crate::suggest::category_hint;
use konter_engine::config::Config;
use konter_engine::model::BankTransaction;

/// Sorts the tables and switches between the views (see the file).
const SCRIPT: &str = include_str!("page.js");

/// Text for HTML, with `&`, `<`, `>` and `"` escaped.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

impl Gaps<'_> {
    /// What the page's header says under the title.
    pub fn subtitle(&self, currency: &str) -> String {
        match (self.first, self.last) {
            (Some(first), Some(last)) => format!("Income and spending {first} … {last} · amounts in whole {currency}"),
            _ => "No bank transactions".into(),
        }
    }

    /// The page's body, with every group (the text shows only the top ones).
    #[expect(clippy::too_many_lines, reason = "one page, written from the top down, and most of it is markup")]
    pub fn html(&self, config: &Config, categories: &BTreeMap<(Date, Decimal), String>) -> String {
        let mut out = String::new();
        if self.first.is_none() {
            return "<p>No bank transactions to report on.</p>\n".into();
        }
        let currency = esc(&config.books.currency);
        let Totals { unmapped: total, mapped, whole, txs: all } = self.totals();

        let _ = writeln!(
            out,
            "<p class=\"lead\"><strong>{}</strong> of income and spending is mapped: {} of {} {currency}. \
             Unmapped: {} {currency} in {} of {} transactions.</p>",
            mapped_percent(mapped, whole),
            money(mapped),
            money(whole),
            money(total.gross()),
            total.count,
            all,
        );

        out.push_str(
            "<div class=\"scroll\"><table class=\"data\">\n<thead><tr><th>source</th><th>policy</th>\
             <th class=\"num\">% mapped</th><th class=\"num\">income+spending</th><th class=\"num\">mapped</th>\
             <th class=\"num\">unmapped</th><th class=\"num\">txs mapped</th></tr></thead>\n<tbody>\n",
        );
        let row = |out: &mut String, class: &str, name: &str, policy: &str, mapped: Decimal, unmapped: &Flow, txs| {
            let _ = writeln!(
                out,
                "<tr{class}><td>{}</td><td>{policy}</td><td class=\"num\">{}</td><td class=\"num\">{}</td>\
                 <td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}/{txs}</td></tr>",
                esc(name),
                mapped_percent(mapped, mapped + unmapped.gross()),
                money(mapped + unmapped.gross()),
                money(mapped),
                money(unmapped.gross()),
                txs - unmapped.count,
            );
        };
        for s in &self.sources {
            row(&mut out, "", s.name, policy(config, s.name), s.mapped.gross(), &s.unmapped, s.txs);
        }
        out.push_str("</tbody>\n<tfoot>\n");
        row(&mut out, " class=\"total\"", "total", "", mapped, &total, all);
        out.push_str("</tfoot></table></div>\n");
        out.push_str(
            "<p class=\"note\"><b>income+spending:</b> money in and out of income and expense accounts, \
             subscriptions included. Transfers between your accounts and balance-sheet moves (buying a home, \
             loans, deposits) are left out. Unmapped transactions count in full until mapped. 100.0% only when \
             everything is mapped.</p>\n",
        );
        if self.transfers.count > 0 {
            let _ = writeln!(
                out,
                "<p class=\"note\">{} {currency} of the unmapped money ({}, {}) looks like transfers between your \
                 own accounts: unmapped on both sides, opposite amounts at most {} days apart. Marked ↔ below; \
                 they map with <code>transfer = \"&lt;account&gt;\"</code> and then drop out of income and \
                 spending.</p>",
                money(self.transfers.gross()),
                percent(self.transfers.gross(), total.gross()),
                plural(self.transfers.count, "transaction"),
                self.window,
            );
        }
        if self.later.count > 0 {
            let _ = writeln!(
                out,
                "<p class=\"note\">{} {currency} of the unmapped money ({}, {}) is marked later: known about, to \
                 be mapped some other time (<code>[later.\"&lt;id&gt;\"]</code>). Marked “later” below; \
                 <code>konter unmatched</code> leaves them out.</p>",
                money(self.later.gross()),
                percent(self.later.gross(), total.gross()),
                plural(self.later.count, "transaction"),
            );
        }

        out.push_str("<h2>Unmapped transactions</h2>\n");
        if self.groups.is_empty() {
            out.push_str("<p>Everything is mapped.</p>\n");
        } else {
            self.groups_and_transactions(&mut out, categories, mapped, whole, total.gross());
        }

        if !self.by_rule.is_empty() || self.wont_map.count > 0 {
            out.push_str("<h2>Not counted above</h2>\n<div class=\"scroll\"><table class=\"data\"><tbody>\n");
            for (account, f) in &self.by_rule {
                let _ = writeln!(
                    out,
                    "<tr><td class=\"num\">{}</td><td>in {} a rule books to <code>{}</code> (e.g. transfers from \
                     before an account's data starts)</td></tr>",
                    signed(f).replacen('-', "−", 1),
                    plural(f.count, "transaction"),
                    esc(account),
                );
            }
            if self.wont_map.count > 0 {
                let _ = writeln!(
                    out,
                    "<tr><td class=\"num\">{}</td><td>in {} marked won't-map (<code>expenses:unknown</code>, \
                     <code>income:unknown</code>)</td></tr>",
                    signed(&self.wont_map).replacen('-', "−", 1),
                    plural(self.wont_map.count, "transaction"),
                );
            }
            out.push_str("</tbody></table></div>\n");
        }
        out.push_str(
            "<p class=\"note\">→ <code>konter suggest &lt;source&gt;</code> proposes rules for groups, \
             <code>konter unmatched &lt;source&gt;</code> prints pin stubs, <code>konter explain &lt;id&gt;</code> \
             shows one transaction.</p>\n",
        );
        let _ = writeln!(out, "<script>\n{SCRIPT}</script>");
        out
    }
}

/// Money in øre (hundredths), for the page's script to add up exactly.
fn ore(d: Decimal) -> String {
    (d * Decimal::ONE_HUNDRED).round().to_string()
}

/// The dates a group spans: one date, or the first and the last.
fn dates(txs: &[&BankTransaction]) -> String {
    let (first, last) = (txs[0].booked, txs[txs.len() - 1].booked);
    if first == last { first.to_string() } else { format!("{first}…{last}") }
}

impl Gaps<'_> {
    /// The unmapped money two ways, one shown at a time: as groups of similar
    /// transactions, largest first, and as single transactions, newest first.
    /// Both sort by any column. The page's script works out → mapped down the
    /// groups in whatever order they're shown, from each group's money and
    /// what of it looks like transfers.
    #[expect(clippy::too_many_lines, reason = "two tables, and most of it is their markup")]
    fn groups_and_transactions(
        &self,
        out: &mut String,
        categories: &BTreeMap<(Date, Decimal), String>,
        mapped: Decimal,
        whole: Decimal,
        unmapped: Decimal,
    ) {
        out.push_str(
            "<div class=\"seg views\" role=\"group\" aria-label=\"Show the unmapped money as\" id=\"view\" hidden>\
             <button data-view=\"groups\" aria-pressed=\"true\">Groups</button>\
             <button data-view=\"txs\" aria-pressed=\"false\">Transactions</button></div>\n",
        );

        let lines = self.group_lines(categories);
        let _ = writeln!(
            out,
            "<section id=\"groups-view\">\n<p class=\"note\">Similar unmapped transactions, grouped. Amounts: − out \
             of the account, + into it, ± both. Click a heading to sort by it.</p>\n\
             <div class=\"scroll\"><table class=\"data sortable\" id=\"groups\" data-default=\"0:asc\" \
             data-mapped=\"{}\" data-whole=\"{}\">\n<thead><tr>\
             <th class=\"num\" data-sort=\"num\" data-first=\"asc\" title=\"Largest first\">#</th>\
             <th data-sort=\"text\">source</th><th data-sort=\"text\">description</th>\
             <th class=\"num\" data-sort=\"num\">txs</th>\
             <th data-sort=\"text\" data-first=\"desc\" title=\"Sorts by the latest transaction\">dates</th>\
             <th class=\"num\" data-sort=\"num\">amount</th><th class=\"num\" data-sort=\"num\">share</th>\
             <th class=\"num\" title=\"Mapped once this group and the ones above it are\">→ mapped</th>\
             <th data-sort=\"text\">notes</th></tr></thead>\n<tbody>",
            ore(mapped),
            ore(whole),
        );
        for (i, line) in lines.iter().enumerate() {
            let g = line.group;
            let others = if line.others > 0 {
                format!(" <span class=\"muted\">(+{})</span>", line.others)
            } else {
                String::new()
            };
            // A single transaction's ID shows on hover; the transactions view lists it.
            let id = if g.txs.len() == 1 { format!(" title=\"{}\"", esc(&g.txs[0].id)) } else { String::new() };
            let _ = writeln!(
                out,
                "<tr data-gross=\"{gross}\" data-transfer=\"{}\"><td class=\"num\">{}</td><td>{}</td>\
                 <td class=\"desc\" data-v=\"{}\">{}{others}</td><td class=\"num\">{}</td>\
                 <td data-v=\"{}\"{id}>{}</td><td class=\"num\" data-v=\"{gross}\">{}</td>\
                 <td class=\"num\" data-v=\"{gross}\">{}</td><td class=\"num after\">{}</td>\
                 <td class=\"muted\">{}</td></tr>",
                ore(g.transfer_money),
                i + 1,
                esc(g.source),
                esc(&line.description),
                esc(&line.description),
                g.txs.len(),
                g.txs[g.txs.len() - 1].booked,
                dates(&g.txs),
                line.amount.replacen('-', "−", 1),
                line.share,
                line.mapped_after,
                esc(&line.notes),
                gross = ore(g.flow.gross()),
            );
        }
        out.push_str("</tbody></table></div>\n");
        out.push_str(
            "<p class=\"note\"><b>(+n):</b> n other descriptions in the group. <b>dates:</b> of the first and \
             the last transaction (hover a single one for its ID); sorting goes by the last. <b>share:</b> of all \
             unmapped money. <b>→ mapped:</b> how much of income and spending is mapped once this group and the \
             ones above it are, in the order shown. <b>↔:</b> looks like a transfer with that account. \
             Categories come from the reference export (<code>data/reference/transactions.csv</code>).</p>\n\
             </section>\n",
        );

        // Every unmapped transaction, newest first, with the group it's in.
        let mut txs: Vec<(usize, &BankTransaction)> =
            lines.iter().enumerate().flat_map(|(i, l)| l.group.txs.iter().map(move |t| (i + 1, *t))).collect();
        txs.sort_by(|(_, a), (_, b)| (b.booked, &b.id).cmp(&(a.booked, &a.id)));
        out.push_str(
            "<section id=\"txs-view\">\n<p class=\"note\">Every unmapped transaction. Amounts: − out of the \
             account, + into it. Click a heading to sort by it.</p>\n\
             <div class=\"scroll\"><table class=\"data sortable\" id=\"txs\" data-default=\"0:desc\">\n<thead><tr>\
             <th data-sort=\"text\" data-first=\"desc\">date</th><th data-sort=\"text\">source</th>\
             <th data-sort=\"text\">id</th><th data-sort=\"text\">description</th>\
             <th class=\"num\" data-sort=\"num\" title=\"Sorts by the money moved, either way\">amount</th>\
             <th class=\"num\" data-sort=\"num\" data-first=\"asc\" title=\"The group it\u{2019}s in\">group</th>\
             <th data-sort=\"text\">notes</th></tr></thead>\n<tbody>",
        );
        for (group, t) in txs {
            let description = tidy(raw_description(t));
            let sign = if t.amount < Decimal::ZERO { "−" } else { "+" };
            let mut notes: Vec<String> = Vec::new();
            if let Some(note) = self.later_notes.get(t.id.as_str()) {
                notes.push(later_note(note));
            }
            if let Some(other) = self.paired.get(t.id.as_str()) {
                notes.push(format!("↔ {other}"));
            }
            notes.extend(category_hint(categories, &[t]).map(str::to_string));
            let _ = writeln!(
                out,
                "<tr><td>{}</td><td>{}</td><td class=\"id\">{}</td><td class=\"desc\">{}</td>\
                 <td class=\"num\" data-v=\"{}\">{sign}{}</td><td class=\"num\">{group}</td>\
                 <td class=\"muted\">{}</td></tr>",
                t.booked,
                esc(&t.source),
                esc(&t.id),
                esc(&description),
                ore(t.amount.abs()),
                money(t.amount),
                esc(&notes.join(" · ")),
            );
        }
        let _ = writeln!(
            out,
            "</tbody></table></div>\n<p class=\"note\">{} of all unmapped money in {}. <b>group:</b> its number \
             in the groups view.</p>\n</section>",
            money(unmapped),
            plural(lines.iter().map(|l| l.group.txs.len()).sum::<usize>(), "transaction"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::esc;

    #[test]
    fn escapes_markup() {
        assert_eq!(esc(r#"<b>"A&B"</b>"#), "&lt;b&gt;&quot;A&amp;B&quot;&lt;/b&gt;");
    }
}
