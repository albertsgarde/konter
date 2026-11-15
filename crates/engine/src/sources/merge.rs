//! Merging a source's raw files (docs/spec.md §4):
//!
//! 1. remove duplicates across overlapping files, reporting any disagreement,
//! 2. report transactions that a file covering their date is missing,
//! 3. order transactions so the running balance chains, reporting breaks,
//! 4. assign stable IDs.

use std::collections::{BTreeMap, BTreeSet};

use jiff::civil::Date;
use rust_decimal::Decimal;

use super::RawFile;
use crate::config::{Format, Source};
use crate::findings::Findings;
use crate::hash::sha256_hex;
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
    snapshots.sort_by_key(|s| (s.date, s.file.clone()));

    let formats: BTreeSet<Format> = files.iter().filter(|f| !f.rows.is_empty()).map(|f| f.format).collect();

    // 1. Deduplicate.
    let mut union: BTreeMap<Key, BankTransaction> = BTreeMap::new();
    // A bank reference identifies a transaction across formats too (Lunar's
    // CSV "Transaction ID" is its API entry_reference).
    let mut by_ref: BTreeMap<String, Key> = BTreeMap::new();
    let mut per_file: Vec<(FileSummary, BTreeSet<Key>)> = Vec::new();
    let mut conflicts = Vec::new();
    for file in files {
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
                Scheme::Balance => Key::Balance(row.booked, row.amount, row.balance_after.unwrap(), 0),
                Scheme::BankRef => Key::BankRef(row.bank_ref.clone().unwrap()),
                Scheme::Text => Key::Text(row.booked, row.amount, row.description.clone(), 0),
            };
            let n = occurrences.entry(base.clone()).or_insert(0);
            let key = match base {
                Key::Balance(d, a, b, _) => Key::Balance(d, a, b, *n),
                Key::Text(d, a, s, _) => Key::Text(d, a, s, *n),
                k => k,
            };
            *n += 1;
            let key = match row.bank_ref.as_ref().and_then(|r| by_ref.get(r)) {
                Some(known) => known.clone(),
                None => key,
            };
            if let Some(r) = &row.bank_ref {
                by_ref.entry(r.clone()).or_insert_with(|| key.clone());
            }
            keys.insert(key.clone());
            match union.get_mut(&key) {
                Some(existing) => {
                    let diffs = differences(existing, &row);
                    if !diffs.is_empty() && existing.origins.iter().any(|o| o.file == file.rel) {
                        conflicts.push(format!(
                            "{}: {} uses its bank reference {} again for {}: {}",
                            short(existing),
                            file.rel,
                            row.bank_ref.as_deref().unwrap_or("?"),
                            short(&row),
                            diffs.join("; ")
                        ));
                    } else if !diffs.is_empty() {
                        conflicts.push(format!(
                            "{}: {} vs {}: {}",
                            short(existing),
                            existing.origin().file,
                            file.rel,
                            diffs.join("; ")
                        ));
                    }
                    if existing.balance_after.is_none() && row.balance_after.is_some() {
                        // Prefer the copy with a running balance.
                        let earlier = std::mem::take(&mut existing.origins);
                        *existing = row;
                        existing.origins.extend(earlier);
                    } else {
                        existing.origins.extend(row.origins);
                    }
                }
                None => {
                    union.insert(key, row);
                }
            }
        }
        per_file.push((summary, keys));
    }

    // 2. Missing from a file that covers the date. A file's first and last day
    // may be partial (export taken mid-day), so only days strictly inside count.
    for (summary, keys) in &per_file {
        let (Some(first), Some(last)) = (summary.first, summary.last) else {
            continue;
        };
        for (key, t) in &union {
            if first < t.booked && t.booked < last && !keys.contains(key) {
                conflicts.push(format!("{} (from {}) is missing from {}", short(t), t.origin().file, summary.rel));
            }
        }
    }
    if !conflicts.is_empty() {
        let d =
            findings.error("overlap conflict", format!("{name}: files disagree on {} transactions", conflicts.len()));
        for c in conflicts {
            d.detail(c);
        }
        if formats.len() > 1 && cutover.is_none() {
            d.detail(format!(
                "→ {name} has data in several formats; without running balances they can't be matched up. \
                 Set `cutover` for it in config/sources.toml (see docs/fetching.md)"
            ));
        }
    }

    // 3. Order.
    let mut txs: Vec<BankTransaction> = union.into_values().collect();
    let has_running_balance = !txs.is_empty() && txs.iter().all(|t| t.balance_after.is_some());
    let mut chain_ok = true;
    if has_running_balance {
        let (ordered, breaks) = chain(&txs);
        txs = ordered.into_iter().map(|i| txs[i].clone()).collect();
        if !breaks.is_empty() {
            chain_ok = false;
            let d = findings.error("balance chain", format!("{name}: running balance breaks {} times", breaks.len()));
            for b in breaks {
                d.detail(b);
            }
        }
    } else {
        txs.sort_by_key(|t| (t.booked, t.time, t.order_hint));
    }

    // 4. IDs.
    let prefix = source.id_prefix.clone().unwrap_or_default();
    let mut occurrences: BTreeMap<(Date, Decimal, Option<Decimal>, String), u32> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for t in &mut txs {
        let scheme = scheme_of(t);
        let text_part = if scheme == Scheme::Text { t.description.clone() } else { String::new() };
        let n = occurrences.entry((t.booked, t.amount, t.balance_after, text_part)).or_insert(0);
        let material = match scheme {
            Scheme::Balance => format!(
                "{name}\x1f{}\x1f{}\x1f{}\x1f{n}",
                t.booked,
                canonical(t.amount),
                canonical(t.balance_after.unwrap())
            ),
            Scheme::BankRef => format!("{name}\x1fref\x1f{}", t.bank_ref.as_deref().unwrap()),
            Scheme::Text => format!("{name}\x1f{}\x1f{}\x1f{}\x1f{n}", t.booked, canonical(t.amount), t.description),
        };
        *n += 1;
        t.id = format!("{prefix}-{}-{}", t.booked, &sha256_hex(material.as_bytes())[..6]);
        if !seen.insert(t.id.clone()) {
            findings.error("ids", format!("{name}: duplicate transaction ID {} (hash collision)", t.id));
        }
    }

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

/// Orders transactions (all with running balances) so each one starts from the
/// previous one's balance. Returns the order (indices) and descriptions of breaks.
fn chain(txs: &[BankTransaction]) -> (Vec<usize>, Vec<String>) {
    let mut days: BTreeMap<Date, Vec<usize>> = BTreeMap::new();
    for (i, t) in txs.iter().enumerate() {
        days.entry(t.booked).or_default().push(i);
    }
    let mut order = Vec::with_capacity(txs.len());
    let mut breaks = Vec::new();
    let mut current: Option<(Date, Decimal)> = None;
    for (day, mut idxs) in days {
        idxs.sort_by_key(|&i| (txs[i].time, txs[i].order_hint));
        let day_order = match current.and_then(|(_, b)| chain_day(txs, &idxs, Some(b))) {
            Some(o) => o,
            None => {
                let own = chain_day(txs, &idxs, None);
                match (&own, current) {
                    (Some(o), Some((prev_day, b))) => {
                        let starts = txs[o[0]].balance_before().unwrap();
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
                own.unwrap_or(idxs)
            }
        };
        current = Some((day, txs[*day_order.last().unwrap()].balance_after.unwrap()));
        order.extend(day_order);
    }
    (order, breaks)
}

/// Finds an order of one day's transactions where each starts from the previous
/// one's balance, starting from `start` (or from any transaction if `None`).
/// Candidates are tried in the given order, so ties follow time and file order.
fn chain_day(txs: &[BankTransaction], idxs: &[usize], start: Option<Decimal>) -> Option<Vec<usize>> {
    fn dfs(
        txs: &[BankTransaction],
        idxs: &[usize],
        current: Decimal,
        used: &mut [bool],
        path: &mut Vec<usize>,
        budget: &mut u32,
    ) -> bool {
        if path.len() == idxs.len() {
            return true;
        }
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        for (j, &i) in idxs.iter().enumerate() {
            if !used[j] && txs[i].balance_before() == Some(current) {
                used[j] = true;
                path.push(i);
                if dfs(txs, idxs, txs[i].balance_after.unwrap(), used, path, budget) {
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
        None => idxs.iter().map(|&i| txs[i].balance_before().unwrap()).collect(),
    };
    for b in starts {
        let mut used = vec![false; idxs.len()];
        let mut path = Vec::new();
        let mut budget = 100_000;
        if dfs(txs, idxs, b, &mut used, &mut path, &mut budget) {
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
            amount: Decimal::from_str(amount).unwrap(),
            currency: "DKK".into(),
            original_amount: None,
            original_currency: None,
            description: desc.into(),
            counterparty: None,
            counterparty_name: None,
            balance_after: Some(Decimal::from_str(balance).unwrap()),
            bank_ref: None,
            raw: BTreeMap::new(),
            origins: vec![Origin { file: file.into(), line }],
            order_hint: -(line as i64),
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
        let balances: Vec<String> = data.txs.iter().map(|t| fmt_num(t.balance_after.unwrap())).collect();
        assert_eq!(balances, ["40.00", "1040.00", "2040.00", "1889.75"]);
        assert_eq!(data.opening, Some(Decimal::from_str("50").unwrap()));
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
