//! `konter gaps`: how much of your income and spending is mapped, and where the
//! unmapped money is, ranked by amount rather than by number of transactions.
//! Similar transactions are grouped the way `konter suggest` groups them, so the
//! top groups are the rules worth writing first.
//!
//! Income and spending is what lands in `income:` and `expenses:` accounts,
//! including subscription payments. Transfers between your own accounts and
//! balance-sheet moves (buying a home, loans, deposits) are left out, so
//! a few huge ones don't drown everything else. Unmapped transactions count in
//! full until they're mapped.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use jiff::civil::Date;
use rust_decimal::Decimal;

use crate::suggest::{category_hint, group_key};
use konter_engine::config::{Config, Policy};
use konter_engine::dates::days_between;
use konter_engine::engine::{Generated, Status, UNKNOWN_IN, UNKNOWN_OUT, is_fallback_account};
use konter_engine::model::BankTransaction;
use konter_engine::model::name_instead;
use konter_engine::sources::Loaded;
use konter_engine::tags;

mod html;

/// Which bank transactions the report looks at.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    /// Only these sources (all when empty).
    pub sources: Vec<String>,
    pub skip: Vec<String>,
    pub from: Option<Date>,
    pub to: Option<Date>,
}

impl Filter {
    fn includes(&self, t: &BankTransaction) -> bool {
        (self.sources.is_empty() || self.sources.contains(&t.source))
            && !self.skip.contains(&t.source)
            && self.from.is_none_or(|d| t.booked >= d)
            && self.to.is_none_or(|d| t.booked <= d)
    }
}

/// Money moved by some transactions, out of and into the account.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flow {
    pub out: Decimal,
    pub into: Decimal,
    pub count: usize,
}

impl Flow {
    fn add(&mut self, amount: Decimal) {
        if amount < Decimal::ZERO {
            self.out -= amount;
        } else {
            self.into += amount;
        }
        self.count += 1;
    }

    /// Money moved either way: what the ranking goes by.
    pub fn gross(&self) -> Decimal {
        self.out + self.into
    }
}

pub struct SourceGap<'a> {
    pub name: &'a str,
    /// All bank transactions, mapped or not.
    pub txs: usize,
    /// Mapped income and spending, as seen from the bank account.
    pub mapped: Flow,
    pub unmapped: Flow,
}

pub struct Group<'a> {
    pub source: &'a str,
    /// In booking order.
    pub txs: Vec<&'a BankTransaction>,
    pub flow: Flow,
    /// How many of `txs` look like a transfer with each other account.
    pub transfers: BTreeMap<&'a str, usize>,
    /// The money in those: once mapped as transfers, it isn't income or spending.
    pub transfer_money: Decimal,
    /// How many of `txs` are marked later.
    pub later: usize,
}

pub struct Gaps<'a> {
    pub first: Option<Date>,
    pub last: Option<Date>,
    /// Most unmapped money first.
    pub sources: Vec<SourceGap<'a>>,
    /// Most money first.
    pub groups: Vec<Group<'a>>,
    /// Booked to an `…:unmatched:…` account by a rule rather than as a
    /// fallback, e.g. transfers from before the other account's data starts.
    /// Counter-account → money as seen from the bank account.
    pub by_rule: BTreeMap<String, Flow>,
    /// Marked won't-map (`unknown = …`).
    pub wont_map: Flow,
    /// The part of the unmapped money that looks like transfers between your
    /// own accounts (see `own_transfers`).
    pub transfers: Flow,
    /// Unmapped transactions that look like one side of a transfer: ID → the
    /// other account (see `own_transfers`).
    pub paired: BTreeMap<&'a str, &'a str>,
    /// The part of the unmapped money marked later (`[later."<id>"]`).
    pub later: Flow,
    /// Unmapped transactions marked later: ID → the mark's note.
    pub later_notes: BTreeMap<&'a str, Option<String>>,
    pub window: i64,
}

/// Unmapped transactions that look like one side of a transfer between two of
/// your accounts: another account has an unmapped transaction with the
/// opposite amount at most `window` days away. Closest dates pair first.
/// Returns ID → the other account.
pub fn own_transfers<'a>(loaded: &'a Loaded, generated: &Generated, window: i64) -> BTreeMap<&'a str, &'a str> {
    let mut txs: Vec<&BankTransaction> =
        loaded.transactions().filter(|t| !generated.outcomes[&t.id].status.is_mapped()).collect();
    txs.sort_by(|a, b| (a.booked, &a.id).cmp(&(b.booked, &b.id)));
    let mut used = vec![false; txs.len()];
    let mut out = BTreeMap::new();
    for i in 0..txs.len() {
        let a = txs[i];
        if used[i] || a.amount >= Decimal::ZERO {
            continue;
        }
        let best = (0..txs.len())
            .filter(|&j| {
                let b = txs[j];
                !used[j] && b.source != a.source && b.amount == -a.amount && days_between(a.booked, b.booked) <= window
            })
            .min_by_key(|&j| (days_between(a.booked, txs[j].booked), txs[j].booked, &txs[j].id));
        if let Some(j) = best {
            used[i] = true;
            used[j] = true;
            out.insert(a.id.as_str(), txs[j].source.as_str());
            out.insert(txs[j].id.as_str(), a.source.as_str());
        }
    }
    out
}

pub fn gaps<'a>(config: &Config, loaded: &'a Loaded, generated: &Generated, filter: &Filter) -> Gaps<'a> {
    let window = config.books.transfer_window_days;
    let own = own_transfers(loaded, generated, window);
    let mut sources: BTreeMap<&str, SourceGap> = BTreeMap::new();
    let mut groups: BTreeMap<(&str, String), Group> = BTreeMap::new();
    let mut by_rule: BTreeMap<String, Flow> = BTreeMap::new();
    let mut wont_map = Flow::default();
    let mut transfers = Flow::default();
    let mut later = Flow::default();
    let mut later_notes = BTreeMap::new();
    let (mut first, mut last) = (None::<Date>, None::<Date>);
    for t in loaded.transactions().filter(|t| filter.includes(t)) {
        first = Some(first.map_or(t.booked, |d| d.min(t.booked)));
        last = Some(last.map_or(t.booked, |d| d.max(t.booked)));
        let outcome = &generated.outcomes[&t.id];
        let source = sources.entry(&t.source).or_insert_with(|| SourceGap {
            name: &t.source,
            txs: 0,
            mapped: Flow::default(),
            unmapped: Flow::default(),
        });
        source.txs += 1;
        if !outcome.status.is_mapped() {
            source.unmapped.add(t.amount);
            let group = groups.entry((&t.source, group_key(t))).or_insert_with(|| Group {
                source: &t.source,
                txs: Vec::new(),
                flow: Flow::default(),
                transfers: BTreeMap::new(),
                transfer_money: Decimal::ZERO,
                later: 0,
            });
            group.txs.push(t);
            group.flow.add(t.amount);
            if let Status::Later(note) = &outcome.status {
                group.later += 1;
                later.add(t.amount);
                later_notes.insert(t.id.as_str(), note.clone());
            }
            if let Some(other) = own.get(t.id.as_str()) {
                *group.transfers.entry(other).or_default() += 1;
                group.transfer_money += t.amount.abs();
                transfers.add(t.amount);
            }
            continue;
        }
        let bank = &config.sources[&t.source].ledger;
        for &i in &outcome.ledger {
            let tx = &generated.txs[i];
            // A payment settles the subscription's balance account, which
            // stands in for its income or expense account.
            let subscription = tx.tags.iter().any(|(name, _)| name == tags::SUBSCRIPTION);
            for p in tx.postings.iter().filter(|p| p.account != *bank) {
                if is_fallback_account(&p.account) {
                    by_rule.entry(p.account.clone()).or_default().add(-p.amount);
                } else if p.account == UNKNOWN_IN || p.account == UNKNOWN_OUT {
                    wont_map.add(-p.amount);
                } else if subscription || p.account.starts_with("income:") || p.account.starts_with("expenses:") {
                    source.mapped.add(-p.amount);
                }
            }
        }
    }
    let mut sources: Vec<SourceGap> = sources.into_values().collect();
    sources.sort_by_key(|s| std::cmp::Reverse(s.unmapped.gross()));
    let mut groups: Vec<Group> = groups.into_values().collect();
    groups.sort_by_key(|g| std::cmp::Reverse(g.flow.gross()));
    Gaps { first, last, sources, groups, by_rule, wont_map, transfers, paired: own, later, later_notes, window }
}

/// Whole currency units with thousands separators, e.g. `12,345`.
fn money(d: Decimal) -> String {
    let digits = d.abs().round().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A group's money with its direction: `-` out of the account, `+` into it.
fn signed(flow: &Flow) -> String {
    let sign = match (flow.out.is_zero(), flow.into.is_zero()) {
        (false, true) => "-",
        (true, false) => "+",
        _ => "±",
    };
    format!("{sign}{}", money(flow.gross()))
}

fn percent(part: Decimal, whole: Decimal) -> String {
    if whole.is_zero() { "-".into() } else { format!("{}%", (part / whole * Decimal::ONE_HUNDRED).round()) }
}

/// A mapped share with one decimal, rounded down so that 100.0% means
/// everything is mapped, e.g. `99.9%` for 99.99%.
fn mapped_percent(mapped: Decimal, whole: Decimal) -> String {
    if whole.is_zero() {
        return "-".into();
    }
    let tenths = (mapped / whole * Decimal::from(1000)).floor();
    format!("{:.1}%", tenths / Decimal::TEN)
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(width - 1).collect::<String>())
    }
}

/// What a transaction is called: its description, or the counterparty name
/// where the description is only a reference.
fn raw_description(t: &BankTransaction) -> &str {
    name_instead(t).unwrap_or(&t.description)
}

/// A description with its runs of spaces made single.
pub(crate) fn tidy(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The description most of the group has, and how many others there are.
fn label(txs: &[&BankTransaction]) -> (String, usize) {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for t in txs {
        *counts.entry(raw_description(t)).or_default() += 1;
    }
    let others = counts.len() - 1;
    let top = counts.into_iter().max_by_key(|(_, n)| *n).map(|(d, _)| d).unwrap_or_default();
    (tidy(top), others)
}

/// That a transaction is marked later, with the mark's note.
fn later_note(note: &Option<String>) -> String {
    match note {
        Some(note) => format!("later: {note}"),
        None => "later".into(),
    }
}

/// Marked later, likely transfers with other accounts, then the reference
/// export's category.
fn notes(
    g: &Group,
    later_notes: &BTreeMap<&str, Option<String>>,
    categories: &BTreeMap<(Date, Decimal), String>,
) -> String {
    let mut parts = Vec::new();
    if g.later > 0 {
        parts.push(match (g.txs.len(), later_notes.get(g.txs[0].id.as_str())) {
            (1, Some(note)) => later_note(note),
            (n, _) if g.later == n => "later".into(),
            (n, _) => format!("later ({} of {n})", g.later),
        });
    }
    if g.txs.iter().any(|t| t.unsettled.is_some()) {
        parts.push("not settled".into());
    }
    parts.extend(g.transfers.iter().map(|(other, n)| {
        if *n == g.txs.len() { format!("↔ {other}") } else { format!("↔ {other} ({n} of {})", g.txs.len()) }
    }));
    parts.extend(category_hint(categories, &g.txs).map(str::to_string));
    parts.join(" · ")
}

/// The ID for a single transaction, else the months the group spans.
fn when(txs: &[&BankTransaction]) -> String {
    let (first, last) = (txs[0], txs[txs.len() - 1]);
    if txs.len() == 1 {
        return first.id.clone();
    }
    let (a, b) = (first.booked.strftime("%Y-%m").to_string(), last.booked.strftime("%Y-%m").to_string());
    if a == b { a } else { format!("{a}…{b}") }
}

/// Unmapped money across all sources, and what that is a share of.
pub struct Totals {
    pub unmapped: Flow,
    pub mapped: Decimal,
    /// Mapped plus unmapped: all income and spending.
    pub whole: Decimal,
    /// All bank transactions, mapped or not.
    pub txs: usize,
}

/// One row of the groups table, worked out once for the text and the page.
pub struct GroupLine<'g> {
    pub group: &'g Group<'g>,
    /// The description most of the group has.
    pub description: String,
    /// How many other descriptions there are in the group.
    pub others: usize,
    /// The ID for a single transaction, else the months the group spans.
    pub when: String,
    /// The money with its direction, e.g. `-250`, `+100` or `±119`.
    pub amount: String,
    /// Of all unmapped money.
    pub share: String,
    /// How much of income and spending is mapped once this group and the ones
    /// above it are.
    pub mapped_after: String,
    pub notes: String,
}

fn policy(config: &Config, source: &str) -> &'static str {
    match config.sources.get(source).map(|c| c.unmatched) {
        Some(Policy::Error) => "error",
        Some(Policy::Warn) => "warn",
        Some(Policy::Allow) => "allow",
        None => "?",
    }
}

impl<'a> Gaps<'a> {
    pub fn totals(&self) -> Totals {
        let unmapped = self.sources.iter().fold(Flow::default(), |mut acc, s| {
            acc.out += s.unmapped.out;
            acc.into += s.unmapped.into;
            acc.count += s.unmapped.count;
            acc
        });
        let mapped: Decimal = self.sources.iter().map(|s| s.mapped.gross()).sum();
        let txs = self.sources.iter().map(|s| s.txs).sum();
        Totals { unmapped, mapped, whole: mapped + unmapped.gross(), txs }
    }

    /// Every group, most money first.
    pub fn group_lines(&self, categories: &BTreeMap<(Date, Decimal), String>) -> Vec<GroupLine<'_>> {
        let totals = self.totals();
        // Mapping a group adds it to the mapped money, except for likely
        // transfers, which leave income and spending altogether.
        let (mut mapped, mut whole) = (totals.mapped, totals.whole);
        self.groups
            .iter()
            .map(|g| {
                mapped += g.flow.gross() - g.transfer_money;
                whole -= g.transfer_money;
                let (description, others) = label(&g.txs);
                GroupLine {
                    group: g,
                    description,
                    others,
                    when: when(&g.txs),
                    amount: signed(&g.flow),
                    share: percent(g.flow.gross(), totals.unmapped.gross()),
                    mapped_after: mapped_percent(mapped, whole),
                    notes: notes(g, &self.later_notes, categories),
                }
            })
            .collect()
    }

    /// The unmapped money in whole units, e.g. `14,325`.
    pub fn unmapped_money(&self) -> String {
        money(self.totals().unmapped.gross())
    }

    /// One line on how much is mapped, e.g. for an overview.
    pub fn headline(&self, currency: &str) -> String {
        let totals = self.totals();
        format!(
            "{} of income and spending is mapped. Unmapped: {} {currency} in {} of {} transactions.",
            mapped_percent(totals.mapped, totals.whole),
            money(totals.unmapped.gross()),
            totals.unmapped.count,
            totals.txs,
        )
    }

    /// `top` limits the groups shown; 0 shows all of them.
    #[expect(clippy::too_many_lines, reason = "one page of text, written from the top down")]
    pub fn render(&self, config: &Config, categories: &BTreeMap<(Date, Decimal), String>, top: usize) -> String {
        let mut out = String::new();
        let (Some(first), Some(last)) = (self.first, self.last) else {
            return "no bank transactions to report on\n".into();
        };
        let currency = &config.books.currency;
        let Totals { unmapped: total, mapped, whole, txs: all } = self.totals();
        let _ = writeln!(out, "Mapped income and spending, {first} … {last}, in whole {currency}\n");
        let _ = writeln!(
            out,
            "  {} of income and spending is mapped: {} of {}. Unmapped: {} in {} of {} transactions.\n",
            mapped_percent(mapped, whole),
            money(mapped),
            money(whole),
            money(total.gross()),
            total.count,
            all,
        );
        let width = self.sources.iter().map(|s| s.name.chars().count()).max().unwrap_or(0).max(6);
        let row = |out: &mut String, name: &str, policy: &str, mapped: Decimal, unmapped: &Flow, txs: usize| {
            let _ = writeln!(
                out,
                "  {name:<width$}  {policy:<6}  {:>8}  {:>15}  {:>11}  {:>10}  {:>11}",
                mapped_percent(mapped, mapped + unmapped.gross()),
                money(mapped + unmapped.gross()),
                money(mapped),
                money(unmapped.gross()),
                format!("{}/{txs}", txs - unmapped.count),
            );
        };
        let _ = writeln!(
            out,
            "  {:<width$}  {:<6}  {:>8}  {:>15}  {:>11}  {:>10}  {:>11}",
            "source", "policy", "% mapped", "income+spending", "mapped", "unmapped", "txs mapped"
        );
        for s in &self.sources {
            row(&mut out, s.name, policy(config, s.name), s.mapped.gross(), &s.unmapped, s.txs);
        }
        row(&mut out, "total", "", mapped, &total, all);
        let _ = writeln!(
            out,
            "\n  income+spending: money in and out of income and expense accounts, subscriptions included.\n  \
             Transfers between your accounts and balance-sheet moves (buying a home, loans, deposits) are left\n  \
             out. Unmapped transactions count in full until mapped. 100.0% only when everything is mapped."
        );
        if self.transfers.count > 0 {
            let _ = writeln!(
                out,
                "  {} of the unmapped money ({}, {}) looks like transfers between your own accounts: unmapped on \
                 both sides,\n  opposite amounts at most {} days apart. Marked ↔ below; they map with \
                 `transfer = \"<account>\"`\n  and then drop out of income and spending.",
                money(self.transfers.gross()),
                percent(self.transfers.gross(), total.gross()),
                plural(self.transfers.count, "transaction"),
                self.window,
            );
        }
        if self.later.count > 0 {
            let _ = writeln!(
                out,
                "  {} of the unmapped money ({}, {}) is marked later: known about, to be mapped some other \
                 time\n  (`[later.\"<id>\"]`). Marked \"later\" below; `konter unmatched` leaves them out.",
                money(self.later.gross()),
                percent(self.later.gross(), total.gross()),
                plural(self.later.count, "transaction"),
            );
        }

        if self.groups.is_empty() {
            let _ = writeln!(out, "\nEverything is mapped.");
        } else {
            let _ = writeln!(out, "\nLargest groups of similar unmapped transactions (- out, + in)\n");
            let swidth = self.groups.iter().map(|g| g.source.chars().count()).max().unwrap_or(0).max(6);
            let _ = writeln!(
                out,
                "  {:>3}  {:<swidth$}  {:<34}  {:>4}  {:<20}  {:>9}  {:>5}  {:>8}  notes",
                "#", "source", "description", "txs", "id / months", "amount", "share", "→ mapped"
            );
            let shown = if top == 0 { self.groups.len() } else { top.min(self.groups.len()) };
            for (i, line) in self.group_lines(categories).into_iter().take(shown).enumerate() {
                let description = if line.others > 0 {
                    let more = format!(" (+{})", line.others);
                    format!("{}{more}", truncate(&line.description, 34 - more.chars().count()))
                } else {
                    truncate(&line.description, 34)
                };
                let _ = writeln!(
                    out,
                    "  {:>3}  {:<swidth$}  {:<34}  {:>4}  {:<20}  {:>9}  {:>5}  {:>8}  {}",
                    i + 1,
                    line.group.source,
                    description,
                    line.group.txs.len(),
                    line.when,
                    line.amount,
                    line.share,
                    line.mapped_after,
                    line.notes,
                );
            }
            if shown < self.groups.len() {
                let rest = &self.groups[shown..];
                let money_left: Decimal = rest.iter().map(|g| g.flow.gross()).sum();
                let _ = writeln!(
                    out,
                    "\n  … and {} ({}, {} or {} of the unmapped money); --top 0 shows all",
                    plural(rest.len(), "smaller group"),
                    plural(rest.iter().map(|g| g.txs.len()).sum::<usize>(), "transaction"),
                    money(money_left),
                    percent(money_left, total.gross()),
                );
            }
            let _ = writeln!(
                out,
                "  (+n): n other descriptions in the group. share: of all unmapped money. → mapped: how much \
                 of income and\n  spending is mapped once this group and the ones above it are. ↔: looks like a \
                 transfer with that account.\n  Categories come from the reference export \
                 (data/reference/transactions.csv)."
            );
        }

        if !self.by_rule.is_empty() || self.wont_map.count > 0 {
            let _ = writeln!(out, "\nNot counted above");
            for (account, f) in &self.by_rule {
                let _ = writeln!(
                    out,
                    "  {:>9}  in {} a rule books to {account} (e.g. transfers from before an \
                     account's data starts)",
                    signed(f),
                    plural(f.count, "transaction")
                );
            }
            if self.wont_map.count > 0 {
                let _ = writeln!(
                    out,
                    "  {:>9}  in {} marked won't-map (expenses:unknown, income:unknown)",
                    signed(&self.wont_map),
                    plural(self.wont_map.count, "transaction")
                );
            }
        }
        let _ = writeln!(
            out,
            "\n→ `konter suggest <source>` proposes rules for groups, `konter unmatched <source>` prints pin stubs, \
             `konter explain <id>` shows one transaction."
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting() {
        assert_eq!(money(Decimal::new(123456789, 2)), "1,234,568");
        assert_eq!(money(Decimal::new(-99949, 2)), "999");
        assert_eq!(money(Decimal::ZERO), "0");
        assert_eq!(money(Decimal::from(100000)), "100,000");
        assert_eq!(truncate("Lønoverførsel", 6), "Lønov…");
        assert_eq!(percent(Decimal::ONE, Decimal::ZERO), "-");
        assert_eq!(percent(Decimal::ONE, Decimal::from(3)), "33%");
        assert_eq!(mapped_percent(Decimal::from(9999), Decimal::from(10000)), "99.9%");
        assert_eq!(mapped_percent(Decimal::from(10000), Decimal::from(10000)), "100.0%");
        assert_eq!(mapped_percent(Decimal::ONE, Decimal::from(3)), "33.3%");
        assert_eq!(mapped_percent(Decimal::ZERO, Decimal::from(3)), "0.0%");
        assert_eq!(mapped_percent(Decimal::ONE, Decimal::ZERO), "-");
        let mut f = Flow::default();
        f.add(Decimal::from(-5));
        assert_eq!(signed(&f), "-5");
        f.add(Decimal::from(7));
        assert_eq!(signed(&f), "±12");
    }
}
