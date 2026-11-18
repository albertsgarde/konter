//! Merging a source's raw files (docs/spec.md §4):
//!
//! 1. remove duplicates across overlapping files, reporting any disagreement,
//!    and any file that lacks a row before one it has that looks the same,
//! 2. report transactions that a file covering their date is missing,
//! 3. order transactions so the running balance chains, reporting breaks,
//! 4. assign stable IDs.

use std::collections::{BTreeMap, BTreeSet};

use jiff::civil::Date;
use rust_decimal::Decimal;

use super::RawFile;
use crate::config::{Format, Source};
use crate::findings::Findings;
use crate::hash::sha256_hex_start;
use crate::model::BankTransaction;
use crate::money::{canonical, fmt_num};

/// How transactions are identified across files, and how IDs are derived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    /// Booking date + amount + running balance (+ occurrence counter).
    Balance,
    /// The bank's own transaction reference.
    BankRef,
    /// Booking date + amount + description (+ occurrence counter).
    Text,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Balance(Date, Decimal, Decimal, u32),
    BankRef(String),
    Text(Date, Decimal, String, u32),
}

impl Key {
    /// The place among rows that look alike (0 for a bank reference).
    fn occurrence(&self) -> u32 {
        match self {
            Key::Balance(.., n) | Key::Text(.., n) => *n,
            Key::BankRef(_) => 0,
        }
    }

    /// The `n`-th of the rows that look like this one (a bank reference is
    /// one row's alone).
    fn nth(&self, n: u32) -> Key {
        match self {
            Key::Balance(date, amount, balance, _) => Key::Balance(*date, *amount, *balance, n),
            Key::Text(date, amount, text, _) => Key::Text(*date, *amount, text.clone(), n),
            Key::BankRef(reference) => Key::BankRef(reference.clone()),
        }
    }
}

/// Summary of one raw file after merging.
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

/// A source's transactions after merging, in booking order.
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

fn short(t: &BankTransaction) -> String {
    format!("{} {:>10} {:?}", t.booked, fmt_num(t.amount), t.description)
}

/// Field-by-field differences between two copies of the same transaction.
fn differences(a: &BankTransaction, b: &BankTransaction) -> Vec<String> {
    let mut out = Vec::new();
    if a.format != b.format {
        return out; // different formats describe the same transaction differently
    }
    macro_rules! cmp {
        ($field:ident) => {
            if a.$field != b.$field {
                out.push(format!("{}: {:?} vs {:?}", stringify!($field), a.$field, b.$field));
            }
        };
    }
    // Part of the key, except for a bank reference: a bank that used one
    // reference twice would otherwise lose a transaction without a word.
    cmp!(booked);
    cmp!(amount);
    cmp!(description);
    cmp!(currency);
    cmp!(counterparty);
    cmp!(counterparty_name);
    cmp!(bank_ref);
    cmp!(time);
    cmp!(value_date);
    cmp!(transaction_date);
    cmp!(original_amount);
    cmp!(original_currency);
    out
}

/// Each row is identified by the best data it has (docs/spec.md §4).
fn scheme_of(t: &BankTransaction) -> Scheme {
    if t.balance_after.is_some() {
        Scheme::Balance
    } else if t.bank_ref.is_some() {
        Scheme::BankRef
    } else {
        Scheme::Text
    }
}

// What `scheme_of` has checked, for the code that goes by its answer.
const HAS_BALANCE: &str = "the balance scheme is for rows with a running balance";
const HAS_BANK_REF: &str = "the reference scheme is for rows with a bank reference";

/// With a `cutover`, CSV data is used before it and API data from it on.
fn in_window(format: Format, date: Date, cutover: Option<Date>) -> bool {
    match cutover {
        None => true,
        Some(c) if format == Format::EnableBanking => date >= c,
        Some(c) => date < c,
    }
}

pub fn merge(name: &str, source: &Source, start: Date, mut files: Vec<RawFile>, findings: &mut Findings) -> SourceData {
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    let cutover = source.cutover.map(|d| d.0);
    let mut snapshots = Vec::new();
    for file in &mut files {
        let format = file.format;
        file.rows.retain(|t| t.booked >= start && in_window(format, t.booked, cutover));
        snapshots.extend(file.snapshots.iter().filter(|s| s.date >= start).cloned());
    }
    snapshots.sort_by(|a, b| (a.date, &a.file).cmp(&(b.date, &b.file)));

    let formats: BTreeSet<Format> = files.iter().filter(|f| !f.rows.is_empty()).map(|f| f.format).collect();

    let Union { txs: union, per_file, mut conflicts, out_of_step } = deduplicate(files);
    if !out_of_step.is_empty() {
        findings
            .error(
                "incomplete day",
                format!("{name}: {} rows that look alike don't match up between files", out_of_step.len()),
            )
            .details(out_of_step)
            .detail(
                "→ rows with the same day, amount and running balance are told apart by counting them from the \
                 start of the day, so a file must have every transaction of a day up to the last one it has. \
                 One of these files doesn't, and the IDs of those rows can't be relied on",
            );
    }
    conflicts.extend(missing(&union, &per_file));
    if !conflicts.is_empty() {
        let d =
            findings.error("overlap conflict", format!("{name}: files disagree on {} transactions", conflicts.len()));
        d.details(conflicts);
        if formats.len() > 1 && cutover.is_none() {
            d.detail(format!(
                "→ {name} has data in several formats; without running balances they can't be matched up. \
                 Set `cutover` for it in config/sources.toml (see docs/fetching.md)"
            ));
        }
    }

    let (occurrences, mut txs): (Vec<u32>, Vec<BankTransaction>) =
        union.into_iter().map(|(key, t)| (key.occurrence(), t)).unzip();
    let has_running_balance = !txs.is_empty() && txs.iter().all(|t| t.balance_after.is_some());
    let mut chain_ok = true;
    if has_running_balance {
        (txs, chain_ok) = in_chain_order(name, txs, &occurrences, findings);
    } else {
        txs.sort_by_key(|t| (t.booked, t.time, t.order_hint));
    }
    assign_ids(name, source.id_prefix.as_deref().unwrap_or_default(), &mut txs, findings);

    let opening = if has_running_balance {
        txs.first().and_then(|t| t.balance_before())
    } else {
        // Work back from the latest snapshot.
        snapshots
            .last()
            .map(|s| s.balance - txs.iter().filter(|t| t.booked <= s.date).map(|t| t.amount).sum::<Decimal>())
    };
    SourceData {
        txs,
        has_running_balance,
        opening,
        snapshots,
        files: per_file.into_iter().map(|(s, _)| s).collect(),
        chain_ok,
    }
}

/// Every transaction in a source's files, once.
struct Union {
    txs: BTreeMap<Key, BankTransaction>,
    /// Each file's summary, and which transactions it has.
    per_file: Vec<(FileSummary, BTreeSet<Key>)>,
    /// Where copies of a transaction disagree.
    conflicts: Vec<String>,
    /// Where a file has a row without one that looks the same and comes
    /// before it in another file.
    out_of_step: Vec<String>,
}

/// Step 1: one copy of each transaction, from files sorted by path.
fn deduplicate(files: Vec<RawFile>) -> Union {
    let mut union: BTreeMap<Key, BankTransaction> = BTreeMap::new();
    // A bank reference identifies a transaction across formats too (Lunar's
    // CSV "Transaction ID" is its API entry_reference).
    let mut by_ref: BTreeMap<String, Key> = BTreeMap::new();
    let mut per_file: Vec<(FileSummary, BTreeSet<Key>)> = Vec::new();
    let mut conflicts = Vec::new();
    let mut out_of_step = Vec::new();
    // Where in a file a row is (`order_hint`) can only be compared within that
    // file. So a transaction takes its place from the file with the most rows
    // on its day: every export of a day starts at its beginning, so that one
    // has all of them. Here: how many rows that file has on the day.
    let mut placed_by: BTreeMap<Key, usize> = BTreeMap::new();
    for file in files {
        let mut on_day: BTreeMap<Date, usize> = BTreeMap::new();
        for row in &file.rows {
            *on_day.entry(row.booked).or_default() += 1;
        }
        let mut occurrences: BTreeMap<Key, u32> = BTreeMap::new();
        let mut keys = BTreeSet::new();
        let summary = FileSummary {
            rel: file.rel.clone(),
            count: file.rows.len(),
            first: file.rows.iter().map(|t| t.booked).min(),
            last: file.rows.iter().map(|t| t.booked).max(),
            exported: file.exported,
        };
        // Rows that look alike are told apart by counting them, oldest first:
        // every export of a day starts at the day's beginning, but one taken
        // during the day ends partway, so counting from the newest (the order
        // most files are in) would pair up different rows.
        let mut rows = file.rows;
        rows.sort_by_key(|t| (t.booked, t.time, t.order_hint));
        for row in rows {
            let base = match scheme_of(&row) {
                Scheme::Balance => Key::Balance(row.booked, row.amount, row.balance_after.expect(HAS_BALANCE), 0),
                Scheme::BankRef => Key::BankRef(row.bank_ref.clone().expect(HAS_BANK_REF)),
                Scheme::Text => Key::Text(row.booked, row.amount, row.description.clone(), 0),
            };
            let n = occurrences.entry(base.clone()).or_insert(0);
            let key = base.nth(*n);
            *n += 1;
            let mut key = match row.bank_ref.as_ref().and_then(|r| by_ref.get(r)) {
                Some(known) => known.clone(),
                None => key,
            };
            // Counting pairs this row with one the bank gave another
            // reference: one of the two files lacks a row that the other has
            // before it. The row goes after the ones already there, so none
            // is taken for a copy of another.
            while let Some(other) = union.get(&key)
                && other.bank_ref.is_some()
                && row.bank_ref.is_some()
                && other.bank_ref != row.bank_ref
            {
                out_of_step.push(format!(
                    "{} has {} but not {}, which looks the same and takes its place in {}",
                    other.origin().file,
                    short(other),
                    short(&row),
                    file.rel
                ));
                key = key.nth(key.occurrence() + 1);
            }
            if let Some(r) = &row.bank_ref {
                by_ref.entry(r.clone()).or_insert_with(|| key.clone());
            }
            keys.insert(key.clone());
            let rows_on_day = on_day[&row.booked];
            match union.get_mut(&key) {
                Some(existing) => {
                    let place = if rows_on_day > placed_by[&key] {
                        placed_by.insert(key.clone(), rows_on_day);
                        row.order_hint
                    } else {
                        existing.order_hint
                    };
                    conflicts.extend(disagreement(existing, &row, &file.rel));
                    if existing.balance_after.is_none() && row.balance_after.is_some() {
                        // Prefer the copy with a running balance.
                        let earlier = std::mem::take(&mut existing.origins);
                        *existing = row;
                        existing.origins.extend(earlier);
                    } else {
                        existing.origins.extend(row.origins);
                    }
                    existing.order_hint = place;
                }
                None => {
                    placed_by.insert(key.clone(), rows_on_day);
                    union.insert(key, row);
                }
            }
        }
        // A file that has the n-th of a kind has the ones before it.
        for key in &keys {
            let Some(n) = key.occurrence().checked_sub(1) else { continue };
            let before = key.nth(n);
            if !keys.contains(&before)
                && let (Some(row), Some(earlier)) = (union.get(key), union.get(&before))
            {
                out_of_step.push(format!(
                    "{} has {} but not {}, which looks the same and comes before it in {}",
                    summary.rel,
                    short(row),
                    short(earlier),
                    earlier.origin().file
                ));
            }
        }
        per_file.push((summary, keys));
    }
    Union { txs: union, per_file, conflicts, out_of_step }
}

/// What `row`, in the file `rel`, says differently from the copy of the same
/// transaction that's already there, if anything.
fn disagreement(existing: &BankTransaction, row: &BankTransaction, rel: &str) -> Option<String> {
    let diffs = differences(existing, row);
    if diffs.is_empty() {
        return None;
    }
    Some(if existing.origins.iter().any(|o| o.file == rel) {
        format!(
            "{}: {} uses its bank reference {} again for {}: {}",
            short(existing),
            rel,
            row.bank_ref.as_deref().unwrap_or("?"),
            short(row),
            diffs.join("; ")
        )
    } else {
        format!("{}: {} vs {}: {}", short(existing), existing.origin().file, rel, diffs.join("; "))
    })
}

/// Step 2: transactions missing from a file that covers their date. A file's
/// first and last day may be partial (export taken mid-day), so only days
/// strictly inside count.
fn missing(union: &BTreeMap<Key, BankTransaction>, per_file: &[(FileSummary, BTreeSet<Key>)]) -> Vec<String> {
    let mut out = Vec::new();
    for (summary, keys) in per_file {
        let (Some(first), Some(last)) = (summary.first, summary.last) else {
            continue;
        };
        for (key, t) in union {
            if first < t.booked && t.booked < last && !keys.contains(key) {
                out.push(format!("{} (from {}) is missing from {}", short(t), t.origin().file, summary.rel));
            }
        }
    }
    out
}

/// Step 3, for transactions that all have a running balance: puts them in the
/// order the balance chains in, and reports where it doesn't. `occurrences`:
/// see [`chain`]. Returns them with whether the chain is unbroken.
fn in_chain_order(
    name: &str,
    txs: Vec<BankTransaction>,
    occurrences: &[u32],
    findings: &mut Findings,
) -> (Vec<BankTransaction>, bool) {
    let (order, breaks) = chain(&txs, occurrences);
    let mut txs: Vec<Option<BankTransaction>> = txs.into_iter().map(Some).collect();
    let ordered = order.into_iter().map(|i| txs[i].take().expect("the chain has every transaction once")).collect();
    let chain_ok = breaks.is_empty();
    if !chain_ok {
        findings
            .error("balance chain", format!("{name}: running balance breaks {} times", breaks.len()))
            .details(breaks);
    }
    (ordered, chain_ok)
}

/// Step 4: gives each transaction, in order, its ID (docs/spec.md §4).
fn assign_ids(name: &str, prefix: &str, txs: &mut [BankTransaction], findings: &mut Findings) {
    let mut occurrences: BTreeMap<(Date, Decimal, Option<Decimal>, String), u32> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for t in txs {
        let scheme = scheme_of(t);
        let text_part = if scheme == Scheme::Text { t.description.clone() } else { String::new() };
        let n = occurrences.entry((t.booked, t.amount, t.balance_after, text_part)).or_insert(0);
        let material = match scheme {
            Scheme::Balance => format!(
                "{name}\x1f{}\x1f{}\x1f{}\x1f{n}",
                t.booked,
                canonical(t.amount),
                canonical(t.balance_after.expect(HAS_BALANCE))
            ),
            Scheme::BankRef => format!("{name}\x1fref\x1f{}", t.bank_ref.as_deref().expect(HAS_BANK_REF)),
            Scheme::Text => format!("{name}\x1f{}\x1f{}\x1f{}\x1f{n}", t.booked, canonical(t.amount), t.description),
        };
        *n += 1;
        t.id = format!("{prefix}-{}-{}", t.booked, sha256_hex_start(material.as_bytes(), 6));
        if !seen.insert(t.id.clone()) {
            findings.error("ids", format!("{name}: duplicate transaction ID {} (hash collision)", t.id));
        }
    }
}

/// The running balance after a transaction that is being chained.
fn after(t: &BankTransaction) -> Decimal {
    t.balance_after.expect("only transactions with a running balance are chained")
}

/// The running balance before a transaction that is being chained.
fn before(t: &BankTransaction) -> Decimal {
    after(t) - t.amount
}

/// `chain` makes a day only for a date that has a transaction, and an order
/// of a day has all of them.
const DAY_HAS_ONE: &str = "a day that is chained has a transaction";

/// Orders transactions (all with running balances) so each one starts from the
/// previous one's balance. `occurrences`: each one's place among those that
/// look alike, from merging. Returns the order (indices) and descriptions of breaks.
fn chain(txs: &[BankTransaction], occurrences: &[u32]) -> (Vec<usize>, Vec<String>) {
    let mut days: BTreeMap<Date, Vec<usize>> = BTreeMap::new();
    for (i, t) in txs.iter().enumerate() {
        days.entry(t.booked).or_default().push(i);
    }
    let mut days: Vec<(Date, Vec<usize>)> = days.into_iter().collect();
    for (_, idxs) in &mut days {
        idxs.sort_by_key(|&i| (txs[i].time, txs[i].order_hint));
        alike_in_order(txs, occurrences, idxs);
    }
    let mut order = Vec::with_capacity(txs.len());
    let mut breaks = Vec::new();
    let mut current: Option<(Date, Decimal)> = None;
    for (k, (day, idxs)) in days.iter().enumerate() {
        let day = *day;
        let day_order = match current.and_then(|(_, b)| chain_day(txs, idxs, Some(b), None)) {
            Some(o) => o,
            None => {
                let own = chain_day_unanchored(txs, idxs, days.get(k + 1).map(|(_, next)| next.as_slice()));
                match (&own, current) {
                    (Some(o), Some((prev_day, b))) => {
                        let starts = before(&txs[o[0]]);
                        breaks.push(format!(
                            "between {prev_day} and {day}: balance was {}, next transaction starts from {} \
                             (difference {}); a transaction is probably missing",
                            fmt_num(b),
                            fmt_num(starts),
                            fmt_num(starts - b)
                        ));
                    }
                    (Some(_), None) => {}
                    (None, _) => breaks.push(format!("on {day}: the day's transactions don't form a chain")),
                }
                own.unwrap_or_else(|| idxs.clone())
            }
        };
        current = Some((day, after(&txs[*day_order.last().expect(DAY_HAS_ONE)])));
        order.extend(day_order);
    }
    (order, breaks)
}

/// Puts a day's transactions that look alike (same amount and running
/// balance) in the order they were counted in when merging, keeping the places
/// they take among the others. The running balance can't tell them apart, so
/// whichever is tried first goes first; and when their copies come from
/// different files, their positions in those files can't be compared. Then
/// adding an export could swap their IDs, and a pin would move from one to the
/// other.
fn alike_in_order(txs: &[BankTransaction], occurrences: &[u32], idxs: &mut [usize]) {
    let mut groups: BTreeMap<(Decimal, Option<Decimal>), Vec<usize>> = BTreeMap::new();
    for (place, &i) in idxs.iter().enumerate() {
        groups.entry((txs[i].amount, txs[i].balance_after)).or_default().push(place);
    }
    for places in groups.values() {
        let mut members: Vec<usize> = places.iter().map(|&p| idxs[p]).collect();
        members.sort_by_key(|&i| occurrences[i]);
        for (&place, i) in places.iter().zip(members) {
            idxs[place] = i;
        }
    }
}

/// An order for a day with no balance before it to start from: the first day,
/// or the one after a break. Several orders can chain (a charge and its refund,
/// either way round), so if the first one found doesn't lead into the next day,
/// prefer one that ends where the next day starts.
fn chain_day_unanchored(txs: &[BankTransaction], idxs: &[usize], next: Option<&[usize]>) -> Option<Vec<usize>> {
    let found = chain_day(txs, idxs, None, None)?;
    let Some(next) = next else { return Some(found) };
    let leads_on = |end: Decimal| chain_day(txs, next, Some(end), None).is_some();
    if leads_on(after(&txs[*found.last().expect(DAY_HAS_ONE)])) {
        return Some(found);
    }
    let mut tried = Vec::new();
    for &j in next {
        let end = before(&txs[j]);
        if tried.contains(&end) {
            continue;
        }
        tried.push(end);
        if let Some(o) = chain_day(txs, idxs, None, Some(end))
            && leads_on(end)
        {
            return Some(o);
        }
    }
    Some(found)
}

/// Finds an order of one day's transactions where each starts from the previous
/// one's balance, starting from `start` (or from any transaction if `None`) and
/// ending on `end`, if given. Candidates are tried in the given order, so ties
/// follow time and file order.
fn chain_day(
    txs: &[BankTransaction],
    idxs: &[usize],
    start: Option<Decimal>,
    end: Option<Decimal>,
) -> Option<Vec<usize>> {
    fn dfs(
        txs: &[BankTransaction],
        idxs: &[usize],
        current: Decimal,
        end: Option<Decimal>,
        used: &mut [bool],
        path: &mut Vec<usize>,
        budget: &mut u32,
    ) -> bool {
        if path.len() == idxs.len() {
            return end.is_none_or(|e| e == current);
        }
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        for (j, &i) in idxs.iter().enumerate() {
            if !used[j] && txs[i].balance_before() == Some(current) {
                used[j] = true;
                path.push(i);
                if dfs(txs, idxs, after(&txs[i]), end, used, path, budget) {
                    return true;
                }
                used[j] = false;
                path.pop();
            }
        }
        false
    }
    let starts: Vec<Decimal> = match start {
        Some(b) => vec![b],
        None => idxs.iter().map(|&i| before(&txs[i])).collect(),
    };
    for b in starts {
        let mut used = vec![false; idxs.len()];
        let mut path = Vec::new();
        let mut budget = 100_000;
        if dfs(txs, idxs, b, end, &mut used, &mut path, &mut budget) {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Policy;
    use crate::model::Origin;
    use jiff::civil::date;
    use std::str::FromStr;

    fn source() -> Source {
        Source {
            bank: "test".into(),
            number: Some("1".into()),
            iban: None,
            ledger: "assets:bank:test".into(),
            id_prefix: Some("t".into()),
            formats: vec![Format::LunarCsv],
            unmatched: Policy::Error,
            start: None,
            before_start: None,
            currency: None,
            cutover: None,
            set_aside: false,
        }
    }

    fn tx(file: &str, line: u64, d: Date, amount: &str, balance: &str, desc: &str) -> BankTransaction {
        BankTransaction {
            id: String::new(),
            source: "test:a".into(),
            format: Format::LunarCsv,
            booked: d,
            time: None,
            value_date: None,
            transaction_date: None,
            amount: Decimal::from_str(amount).expect("the test's amount is a decimal"),
            currency: "DKK".into(),
            original_amount: None,
            original_currency: None,
            description: desc.into(),
            counterparty: None,
            counterparty_name: None,
            balance_after: Some(Decimal::from_str(balance).expect("the test's balance is a decimal")),
            bank_ref: None,
            raw: BTreeMap::new(),
            origins: vec![Origin { file: file.into(), line }],
            order_hint: -i64::try_from(line).expect("a small line number"),
        }
    }

    fn file(rel: &str, rows: Vec<BankTransaction>) -> RawFile {
        RawFile { rel: rel.into(), format: Format::LunarCsv, rows, snapshots: Vec::new(), exported: None }
    }

    #[test]
    fn orders_same_day_by_balance_not_file_order() {
        // Newest first, but the two same-day rows are in the wrong order (as in
        // real Lunar exports).
        let f = file(
            "a.csv",
            vec![
                tx("a.csv", 2, date(2025, 12, 10), "-150.25", "1889.75", "CAFE"),
                tx("a.csv", 3, date(2025, 12, 8), "1000", "2040", "LUNAR"),
                tx("a.csv", 4, date(2025, 12, 8), "1000", "1040", "LUNAR"),
                tx("a.csv", 5, date(2025, 12, 1), "-10", "40", "X"),
            ],
        );
        let mut findings = Findings::default();
        let data = merge("test:a", &source(), date(2025, 1, 1), vec![f], &mut findings);
        assert!(!findings.has_errors(), "{}", findings.render(true));
        let balances: Vec<String> = data
            .txs
            .iter()
            .map(|t| fmt_num(t.balance_after.expect("the test's rows have a running balance")))
            .collect();
        assert_eq!(balances, ["40.00", "1040.00", "2040.00", "1889.75"]);
        assert_eq!(data.opening, Some(Decimal::from_str("50").expect("a decimal")));
        // Two identical-looking rows still get different IDs.
        assert_ne!(data.txs[1].id, data.txs[2].id);
    }

    #[test]
    fn dedups_overlap_and_ids_are_stable() {
        let old = file(
            "old.csv",
            vec![
                tx("old.csv", 2, date(2025, 1, 3), "-5", "95", "B"),
                tx("old.csv", 3, date(2025, 1, 2), "100", "100", "A"),
            ],
        );
        let new = file(
            "new.csv",
            vec![
                tx("new.csv", 2, date(2025, 1, 4), "-5", "90", "C"),
                tx("new.csv", 3, date(2025, 1, 3), "-5", "95", "B"),
                tx("new.csv", 4, date(2025, 1, 2), "100", "100", "A"),
            ],
        );
        let mut r1 = Findings::default();
        let both = merge("test:a", &source(), date(2025, 1, 1), vec![old, new], &mut r1);
        assert!(!r1.has_errors(), "{}", r1.render(true));
        assert_eq!(both.txs.len(), 3);
        assert_eq!(both.txs[1].origins.len(), 2);
        // The same transaction gets the same ID whichever files it comes from.
        let only_new = file("new.csv", vec![tx("new.csv", 2, date(2025, 1, 3), "-5", "95", "B")]);
        let mut r2 = Findings::default();
        let single = merge("test:a", &source(), date(2025, 1, 1), vec![only_new], &mut r2);
        assert_eq!(single.txs[0].id, both.txs[1].id);
    }

    #[test]
    fn file_order_doesnt_matter() {
        let a = || file("a.csv", vec![tx("a.csv", 2, date(2025, 1, 3), "-5", "95", "B")]);
        let b = || {
            file(
                "b.csv",
                vec![
                    tx("b.csv", 2, date(2025, 1, 3), "-5", "95", "B"),
                    tx("b.csv", 3, date(2025, 1, 2), "100", "100", "A"),
                ],
            )
        };
        let mut r = Findings::default();
        let x = merge("test:a", &source(), date(2025, 1, 1), vec![a(), b()], &mut r);
        let y = merge("test:a", &source(), date(2025, 1, 1), vec![b(), a()], &mut r);
        let ids = |d: &SourceData| d.txs.iter().map(|t| (t.id.clone(), t.origin().file.clone())).collect::<Vec<_>>();
        assert_eq!(ids(&x), ids(&y));
    }

    #[test]
    fn reports_conflicts_missing_rows_and_gaps() {
        let a = file(
            "a.csv",
            vec![
                tx("a.csv", 2, date(2025, 1, 9), "-1", "97", "D"),
                tx("a.csv", 3, date(2025, 1, 5), "-1", "98", "C"),
                tx("a.csv", 4, date(2025, 1, 3), "-1", "99", "B"),
                tx("a.csv", 5, date(2025, 1, 1), "100", "100", "A"),
            ],
        );
        let b = file(
            "b.csv",
            vec![
                tx("b.csv", 2, date(2025, 1, 9), "-1", "97", "D"),
                // C missing although b covers 2025-01-05
                tx("b.csv", 3, date(2025, 1, 3), "-1", "99", "B reworded"),
                tx("b.csv", 4, date(2025, 1, 1), "100", "100", "A"),
            ],
        );
        let mut findings = Findings::default();
        merge("test:a", &source(), date(2025, 1, 1), vec![a, b], &mut findings);
        let text = findings.render(true);
        assert!(text.contains("missing from b.csv"), "{text}");
        assert!(text.contains("\"B\" vs \"B reworded\""), "{text}");

        let gap = file(
            "g.csv",
            vec![
                tx("g.csv", 2, date(2025, 1, 9), "-1", "50", "after gap"),
                tx("g.csv", 3, date(2025, 1, 1), "100", "100", "A"),
            ],
        );
        let mut findings = Findings::default();
        let data = merge("test:a", &source(), date(2025, 1, 1), vec![gap], &mut findings);
        assert!(!data.chain_ok);
        assert!(findings.render(true).contains("difference -49.00"), "{}", findings.render(true));
    }
}
