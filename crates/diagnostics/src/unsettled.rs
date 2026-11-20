//! What in the store of transactions is left for a person to settle, as a
//! section of a web page: rows that may be transactions the table already
//! had, and copies that say something else than the table (docs/spec.md §4).
//! The page around it belongs to the site.

use std::fmt::Write as _;

use crate::esc;
use konter_engine::model::BankTransaction;
use konter_engine::money::fmt_num;
use konter_engine::store::{Stored, Table};

const HEADING: &str = "<h2>Bank transactions to settle</h2>\n";

/// The cells that say which transaction a row is: account, date, amount and text.
fn which(t: &Stored) -> String {
    let text = esc(t.text.as_deref().unwrap_or_default());
    format!(
        "<td>{}</td><td>{}</td><td class=\"num\">{}</td><td class=\"desc\" title=\"{text}\">{text}</td>",
        esc(&t.account),
        t.date,
        fmt_num(t.amount),
    )
}

/// One row of a pair we aren't sure of, with what there is to tell them apart by.
fn of_a_pair(out: &mut String, class: &str, label: &str, t: &Stored) {
    let places: Vec<String> = t.sources.iter().map(|o| esc(&format!("{}:{}", o.file, o.line))).collect();
    let _ = writeln!(
        out,
        "<tr{class}><td class=\"muted\">{label}</td>{}<td>{}</td><td class=\"num\">{}</td><td>{}</td>\
         <td class=\"id\">{}</td><td>{}</td></tr>",
        which(t),
        esc(t.counterparty_name.as_deref().unwrap_or_default()),
        t.balance.map(fmt_num).unwrap_or_default(),
        esc(t.reference.as_deref().unwrap_or_default()),
        t.id,
        places.join("<br>"),
    );
}

/// The section. `copies` are the raw rows at the places the table's
/// conflicts name, as far as their files can still be read: they have what
/// the copies say, which the table doesn't keep.
pub fn html(table: &Table, copies: &[BankTransaction]) -> String {
    let (unsure, conflicts) = (table.unsure(), table.conflicts());
    let mut out = String::from(HEADING);
    if unsure.is_empty() && conflicts.is_empty() {
        out.push_str("<p class=\"muted\">Nothing to settle.</p>\n");
        return out;
    }
    if !unsure.is_empty() {
        let _ = writeln!(out, "<h3>May be a transaction the table already had ({})</h3>", unsure.len());
        out.push_str(
            "<p class=\"note\">A bank's file had a row that could be a transaction already in the table, and \
             nothing settled it. The row was added as a transaction of its own, with the other's ID in \
             <code>maybe_same_as</code>.</p>\n\
             <div class=\"scroll\"><table class=\"data\">\n<thead><tr><th></th><th>account</th><th>date</th>\
             <th class=\"num\">amount</th><th>text</th><th>counterparty</th><th class=\"num\">balance</th>\
             <th>reference</th><th>id</th><th>read from</th></tr></thead>\n<tbody>\n",
        );
        for (row, other) in unsure {
            of_a_pair(&mut out, " class=\"first\"", "added", row);
            of_a_pair(&mut out, "", "may be", other);
        }
        out.push_str("</tbody></table></div>\n");
    }
    if !conflicts.is_empty() {
        let _ = writeln!(out, "<h3>Copies that say something else ({})</h3>", conflicts.len());
        out.push_str(
            "<p class=\"note\">A copy with a transaction's reference, day and amount says something else in a \
             field both have. The table kept what it had, and the transaction has the copy in \
             <code>conflicts</code>.</p>\n\
             <div class=\"scroll\"><table class=\"data\">\n<thead><tr><th>account</th><th>date</th>\
             <th class=\"num\">amount</th><th>text</th><th>column</th><th>the table has</th><th>the copy says</th>\
             <th>the copy</th><th>id</th></tr></thead>\n<tbody>\n",
        );
        for (row, conflict) in conflicts {
            let copy = copies.iter().find(|copy| copy.origins.contains(&conflict.place)).map(|copy| row.read_as(copy));
            for column in &conflict.columns {
                let says = match copy.as_ref().and_then(|copy| copy.cell(column)) {
                    Some(says) => esc(&says),
                    None => "<span class=\"muted\">can't be read any more</span>".to_owned(),
                };
                let _ = writeln!(
                    out,
                    "<tr>{}<td>{}</td><td>{}</td><td>{says}</td><td>{}</td><td class=\"id\">{}</td></tr>",
                    which(row),
                    esc(column),
                    esc(&row.cell(column).unwrap_or_default()),
                    esc(&format!("{}:{}", conflict.place.file, conflict.place.line)),
                    row.id,
                );
            }
        }
        out.push_str("</tbody></table></div>\n");
    }
    out
}
