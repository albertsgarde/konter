//! A source's transactions from the store's table (docs/spec.md §4), ready
//! for the books:
//!
//! 1. take the source's transactions from the table, leaving out rows from
//!    before its start, rows joined to another, and rows we aren't sure of,
//!    which count as the transaction they may be until someone settles them,
//! 2. report transactions that a file covering their date is missing,
//! 3. order them so the running balance chains, reporting breaks.
//!
//! The table says which transactions there are and what each one's ID is.
//! The raw files are where its rows came from: they give a transaction's
//! original fields and its place within its day, and say how far the data
//! reaches.

use std::collections::{BTreeMap, BTreeSet};

use jiff::civil::Date;
use rust_decimal::Decimal;

use super::RawFile;
use super::chain::{in_chain_order, short};
use crate::findings::Findings;
use crate::model::{BankTransaction, Origin};
use crate::store::{Stored, Table};

/// Summary of one raw file.
#[derive(Clone, Debug)]
pub struct FileSummary {
    pub rel: String,
    pub count: usize,
    pub first: Option<Date>,
    pub last: Option<Date>,
    /// When the file was exported or fetched, if known.
    pub exported: Option<Date>,
}

/// A balance the bank reported at the end of a date (from an API fetch or
/// recorded with `add-export --balance`).
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub date: Date,
    pub balance: Decimal,
    pub file: String,
}

/// A source's transactions, in booking order.
#[derive(Debug)]
pub struct SourceData {
    pub txs: Vec<BankTransaction>,
    /// Whether every transaction has a running balance (then the chain is checked).
    pub has_running_balance: bool,
    /// Balance before the first transaction, from running balances or a snapshot.
    pub opening: Option<Decimal>,
    /// Balance snapshots, used as assertions when there are no running balances.
    pub snapshots: Vec<Snapshot>,
    pub files: Vec<FileSummary>,
    pub chain_ok: bool,
    /// The IDs of the table's rows that count as another transaction (joined
    /// to it, or maybe it and not settled yet), each with that one's ID.
    pub aliases: BTreeMap<String, String>,
    /// The IDs of the table's transactions from before the source's start.
    pub before_start: BTreeSet<String>,
}

impl SourceData {
    pub fn first(&self) -> Option<Date> {
        self.txs.first().map(|t| t.booked)
    }

    pub fn last(&self) -> Option<Date> {
        self.txs.last().map(|t| t.booked)
    }

    /// The last date the data is known to be complete for: the latest export
    /// or fetch date, or else the last transaction.
    pub fn covered_until(&self) -> Option<Date> {
        let exported = self.files.iter().filter_map(|f| f.exported).max();
        match (exported, self.last()) {
            (Some(e), Some(l)) => Some(e.max(l)),
            (e, l) => e.or(l),
        }
    }

    /// Whether `date` lies within the period this source has data for.
    pub fn covers(&self, date: Date) -> bool {
        matches!((self.first(), self.last()), (Some(a), Some(b)) if a <= date && date <= b)
    }
}

/// A raw row, and how many rows its file has on its day.
type Copy<'a> = (&'a BankTransaction, usize);

/// The transaction of `name` that the table's row `stored` is, with what its
/// copies in the raw files add. `others`: the rows that count as it.
fn transaction(
    name: &str,
    stored: &Stored,
    others: &[&Stored],
    copies: &BTreeMap<&Origin, Copy<'_>>,
) -> BankTransaction {
    let mut origins = stored.sources.clone();
    for origin in others.iter().flat_map(|other| &other.sources) {
        if !origins.contains(origin) {
            origins.push(origin.clone());
        }
    }
    let found: Vec<Copy<'_>> = origins.iter().filter_map(|origin| copies.get(origin).copied()).collect();
    // Where in a file a row is can only be compared within that file. So a
    // transaction takes its place from the file with the most rows on its
    // day: every export of a day starts at its beginning, so that one has
    // all of them.
    let mut place: Option<Copy<'_>> = None;
    for copy in &found {
        if place.is_none_or(|(_, most)| copy.1 > most) {
            place = Some(*copy);
        }
    }
    let first = found.first().map(|(copy, _)| *copy);
    BankTransaction {
        id: stored.id.to_string(),
        source: name.to_owned(),
        format: first.and_then(|copy| copy.format),
        booked: stored.date,
        time: stored.time,
        value_date: stored.value_date,
        transaction_date: stored.transaction_date,
        amount: stored.amount,
        currency: stored.currency.clone().unwrap_or_default(),
        original_amount: stored.original_amount,
        original_currency: stored.original_currency.clone(),
        description: stored.text.clone().unwrap_or_default(),
        counterparty: stored.counterparty.clone(),
        counterparty_name: stored.counterparty_name.clone(),
        balance_after: stored.balance,
        bank_ref: stored.reference.clone(),
        raw: first.map(|copy| copy.raw.clone()).unwrap_or_default(),
        origins,
        order_hint: place.map_or(0, |(copy, _)| copy.order_hint),
    }
}

/// Step 2: transactions missing from a file that covers their date. A file's
/// first and last day may be partial (export taken mid-day), so only days
/// strictly inside count.
fn missing(txs: &[BankTransaction], files: &[FileSummary]) -> Vec<String> {
    let mut out = Vec::new();
    for file in files {
        let (Some(first), Some(last)) = (file.first, file.last) else {
            continue;
        };
        for t in txs {
            if first < t.booked && t.booked < last && !t.origins.iter().any(|origin| origin.file == file.rel) {
                out.push(format!("{} (from {}) is missing from {}", short(t), t.origin().file, file.rel));
            }
        }
    }
    out
}

/// The transactions of source `name` that the table has, from `start` on.
/// `files`: the source's raw files, parsed.
pub fn from_table(
    name: &str,
    start: Date,
    table: &Table,
    mut files: Vec<RawFile>,
    findings: &mut Findings,
) -> SourceData {
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    let read = table.files();
    let unread: Vec<&str> =
        files.iter().filter(|f| !f.rows.is_empty() && !read.contains(f.rel.as_str())).map(|f| f.rel.as_str()).collect();
    if !unread.is_empty() {
        findings
            .error("store", format!("{name}: {} raw files haven't been read into the table", unread.len()))
            .details(unread.iter().map(|rel| format!("data/{rel}")))
            .detail("→ the books are built without their rows; `konter store` reads the ones data/MANIFEST lists");
    }

    let mut copies: BTreeMap<&Origin, Copy<'_>> = BTreeMap::new();
    let mut summaries = Vec::new();
    let mut snapshots = Vec::new();
    for file in &files {
        let mut on_day: BTreeMap<Date, usize> = BTreeMap::new();
        for row in &file.rows {
            *on_day.entry(row.booked).or_default() += 1;
        }
        for row in &file.rows {
            for origin in &row.origins {
                copies.insert(origin, (row, on_day[&row.booked]));
            }
        }
        let rows = || file.rows.iter().filter(|t| t.booked >= start);
        summaries.push(FileSummary {
            rel: file.rel.clone(),
            count: rows().count(),
            first: rows().map(|t| t.booked).min(),
            last: rows().map(|t| t.booked).max(),
            exported: file.exported,
        });
        snapshots.extend(file.snapshots.iter().filter(|s| s.date >= start).cloned());
    }
    snapshots.sort_by(|a, b| (a.date, &a.file).cmp(&(b.date, &b.file)));

    // Step 1. The rows that count as another, by the row they count as.
    let mut others: BTreeMap<&crate::store::TransactionId, Vec<&Stored>> = BTreeMap::new();
    let mut aliases = BTreeMap::new();
    let mut unsure = Vec::new();
    for row in table.rows().filter(|row| row.account == name) {
        let Some(used) = table.stands_for(&row.id).filter(|used| used.id != row.id) else { continue };
        others.entry(&used.id).or_default().push(row);
        aliases.insert(row.id.to_string(), used.id.to_string());
        if row.same_as.is_none() {
            unsure.push(format!("{} {} counts as {} {}", row.id, row.short(), used.id, used.short()));
        }
    }
    if !unsure.is_empty() {
        findings
            .warn("store", format!("{name}: {} rows may be transactions the table already had", unsure.len()))
            .details(unsure)
            .detail("→ until someone settles them, each counts as the transaction it may be (the site's front page)");
    }
    let mut before_start = BTreeSet::new();
    let mut txs = Vec::new();
    for row in table.rows().filter(|row| row.account == name && !aliases.contains_key(row.id.as_str())) {
        if row.date < start {
            before_start.insert(row.id.to_string());
            continue;
        }
        let others = others.get(&row.id).map(Vec::as_slice).unwrap_or_default();
        txs.push(transaction(name, row, others, &copies));
    }
    txs.sort_by(|a, b| (a.booked, a.time, a.order_hint, &a.id).cmp(&(b.booked, b.time, b.order_hint, &b.id)));

    let conflicts = missing(&txs, &summaries);
    if !conflicts.is_empty() {
        findings
            .error("overlap conflict", format!("{name}: files disagree on {} transactions", conflicts.len()))
            .details(conflicts);
    }

    // Step 3.
    let has_running_balance = !txs.is_empty() && txs.iter().all(|t| t.balance_after.is_some());
    let mut chain_ok = true;
    if has_running_balance {
        (txs, chain_ok) = in_chain_order(name, txs, findings);
    }
    let opening = if has_running_balance {
        txs.first().and_then(|t| t.balance_before())
    } else {
        // Work back from the latest snapshot.
        snapshots
            .last()
            .map(|s| s.balance - txs.iter().filter(|t| t.booked <= s.date).map(|t| t.amount).sum::<Decimal>())
    };
    SourceData { txs, has_running_balance, opening, snapshots, files: summaries, chain_ok, aliases, before_start }
}
