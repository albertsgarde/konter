//! Property tests: invariants from docs/spec.md, checked on many generated
//! inputs instead of a few hand-picked ones. Small inputs are enumerated
//! exhaustively, smallest first, so a failure names a minimal case. Larger
//! ones are generated anew on every run, and one that fails is saved in
//! tests/cases/ and checked first on every run after it (`check_cases`).
//! The ones that fail today are ignored, like those in breakage.rs:
//! `cargo test -p konter-engine --test properties -- --ignored`.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use common::{
    NORDEA, Rng, build, check_cases, config, danish, nordea_csv, nordea_rel, nordea_rel_of, posted, rules,
    try_build_with,
};
use jiff::civil::{Date, date};
use konter_engine::config::Config;
use konter_engine::dates::days_between;
use konter_engine::findings::{Findings, Severity};
use konter_engine::money::{Amount, fmt_num, parse_danish, parse_plain};
use konter_engine::pipeline::Build;
use konter_engine::rules::Rules;
use konter_engine::sources::{self, SourceData};
use konter_engine::store::{Table, TransactionId};
use konter_engine::tags;
use konter_engine::transfers::{Leg, pair};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

// ── Transfer pairing (docs/spec.md §6) ─────────────────────────────────

fn leg(from: &str, to: &str, day: i8, amount: i64, n: usize) -> Leg {
    Leg {
        ledger_idx: n,
        from: from.into(),
        to: to.into(),
        date: date(2026, 1, day),
        amount: Decimal::from(amount),
        id: format!("{n}"),
    }
}

fn describe(legs: &[Leg]) -> String {
    let lines: Vec<String> = legs
        .iter()
        .map(|l| format!("  #{} {} → {} on day {}: {}", l.id, l.from, l.to, l.date.day(), l.amount))
        .collect();
    lines.join("\n")
}

fn fits(a: &Leg, b: &Leg, window: i64) -> bool {
    b.from == a.to && b.to == a.from && b.amount == -a.amount && days_between(a.date, b.date) <= window
}

/// The most pairs there can be: a maximum bipartite matching (Kuhn's algorithm).
fn most_pairs(legs: &[Leg], window: i64) -> usize {
    let forward: Vec<usize> = (0..legs.len()).filter(|&i| legs[i].from < legs[i].to).collect();
    let partners: Vec<Vec<usize>> =
        forward.iter().map(|&i| (0..legs.len()).filter(|&j| fits(&legs[i], &legs[j], window)).collect()).collect();
    fn augment(u: usize, partners: &[Vec<usize>], seen: &mut [bool], matched: &mut [Option<usize>]) -> bool {
        for &v in &partners[u] {
            if seen[v] {
                continue;
            }
            seen[v] = true;
            let free = match matched[v] {
                None => true,
                Some(w) => augment(w, partners, seen, matched),
            };
            if free {
                matched[v] = Some(u);
                return true;
            }
        }
        false
    }
    let mut matched = vec![None; legs.len()];
    (0..forward.len()).filter(|&u| augment(u, &partners, &mut vec![false; legs.len()], &mut matched)).count()
}

/// Calls `f` with every set of up to `max_legs` legs between `a` and `b` with
/// dates on days 1..=`days` and amounts of ±500, fewest legs first.
fn small_cases(max_legs: usize, days: i8, mut f: impl FnMut(&[Leg])) {
    let choices: Vec<(i8, i64)> = (1..=days).flat_map(|d| [(d, -500), (d, 500)]).collect();
    for n in 1..=max_legs {
        for forward in 0..=n {
            for code in 0..choices.len().pow(u32::try_from(n).expect("a handful of legs")) {
                let mut c = code;
                let legs: Vec<Leg> = (0..n)
                    .map(|k| {
                        let (day, amount) = choices[c % choices.len()];
                        c /= choices.len();
                        if k < forward { leg("a", "b", day, amount, k) } else { leg("b", "a", day, amount, k) }
                    })
                    .collect();
                f(&legs);
            }
        }
    }
}

/// Legs to pair, as a case is saved: (from, to, day in January, amount).
#[derive(Serialize, Deserialize)]
struct Legs {
    legs: Vec<(String, String, i8, i64)>,
}

impl Legs {
    fn legs(&self) -> Vec<Leg> {
        self.legs.iter().enumerate().map(|(n, (from, to, day, amount))| leg(from, to, *day, *amount, n)).collect()
    }
}

/// Up to 10 legs between three accounts over a month.
fn random_legs(rng: &mut Rng, _size: usize) -> Legs {
    let accounts = ["a", "b", "c"];
    let legs = (0..1 + rng.below(10))
        .map(|_| {
            let from = *rng.pick(&accounts);
            let to = *rng.pick(&accounts.iter().copied().filter(|a| *a != from).collect::<Vec<_>>());
            let amount = *rng.pick(&[-500, 500, -300, 300]);
            (from.to_string(), to.to_string(), 1 + i8::try_from(rng.below(28)).expect("below 28"), amount)
        })
        .collect();
    Legs { legs }
}

/// What pair() must always do: pair each leg at most once, and only legs
/// that go opposite ways between the same accounts, with opposite amounts,
/// within the window.
fn check_pairs(legs: &[Leg], window: i64) -> Result<(), String> {
    let (pairs, unpaired) = pair(legs, window);
    let mut seen = vec![0; legs.len()];
    for &(i, j) in &pairs {
        seen[i] += 1;
        seen[j] += 1;
        if !fits(&legs[i], &legs[j], window) {
            return Err(format!("paired #{i} with #{j}, which don't fit"));
        }
    }
    for &i in &unpaired {
        seen[i] += 1;
    }
    match seen.iter().position(|&n| n != 1) {
        Some(i) => Err(format!("leg #{i} is paired or left unpaired {} times", seen[i])),
        None => Ok(()),
    }
}

#[test]
fn pairs_are_opposite_legs_within_the_window() {
    for window in [0, 1, 2, 5] {
        small_cases(4, 7, |legs| {
            if let Err(e) = check_pairs(legs, window) {
                panic!("window {window}: {e}\n{}", describe(legs));
            }
        });
    }
    check_cases("pairs-fit", 5000, random_legs, |case| {
        let legs = case.legs();
        check_pairs(&legs, 5).map_err(|e| format!("{e}\n{}", describe(&legs)))
    });
}

/// "An unpaired side is an error inside both accounts' covered dates": so
/// every leg that can be paired must be.
#[test]
fn pairing_pairs_as_many_legs_as_can_be_paired() {
    for window in [0, 1, 2, 5] {
        small_cases(4, 7, |legs| {
            let (pairs, _) = pair(legs, window);
            let best = most_pairs(legs, window);
            if pairs.len() < best {
                panic!("window {window}: {} pairs where {best} are possible\n{}", pairs.len(), describe(legs));
            }
        });
    }
    check_cases("pairs-most", 5000, random_legs, |case| {
        let legs = case.legs();
        let (pairs, best) = (pair(&legs, 5).0.len(), most_pairs(&legs, 5));
        if pairs == best {
            Ok(())
        } else {
            Err(format!("{pairs} pairs where {best} are possible\n{}", describe(&legs)))
        }
    });
}

// ── Merging overlapping exports (docs/spec.md §4) ──────────────────────

/// One row of a bank history: (day in January, amount, description, balance after).
type Row = (i8, Decimal, &'static str, Decimal);

/// Up to a week on an account with a running balance. Few distinct amounts,
/// so the same day, amount and balance come up again, as with money collected
/// and passed on.
fn history(rng: &mut Rng, days: i8) -> Vec<Row> {
    const KINDS: [(i64, &str); 5] =
        [(100, "MobilePay Alice"), (100, "MobilePay Carol"), (-100, "Club dinner"), (-50, "Groceries"), (50, "Refund")];
    let mut balance = Decimal::from(1000);
    let mut rows = Vec::new();
    for day in 1..=days {
        for _ in 0..rng.below(4) {
            let (amount, what) = *rng.pick(&KINDS);
            balance += Decimal::from(amount);
            rows.push((day, Decimal::from(amount), what, balance));
        }
    }
    rows
}

/// Exports that together cover `rows`, as row ranges in booking order. An
/// export starts at the beginning of a day. With `mid_day` it may end
/// anywhere (an export taken during the day has that day's rows so far),
/// else at the end of a day. With `any_start` it may also start anywhere,
/// which no bank's file does.
fn exports(rng: &mut Rng, rows: &[Row], mid_day: bool, any_start: bool) -> Vec<Range<usize>> {
    let day_starts: Vec<usize> =
        (0..rows.len()).filter(|&i| any_start || i == 0 || rows[i].0 != rows[i - 1].0).collect();
    let ends: Vec<usize> = if mid_day {
        (1..=rows.len()).collect()
    } else {
        day_starts[1..].iter().copied().chain([rows.len()]).collect()
    };
    let mut out = Vec::new();
    let mut covered = 0;
    while covered < rows.len() {
        let start = *rng.pick(&day_starts.iter().copied().filter(|&s| s <= covered).collect::<Vec<_>>());
        let end = *rng.pick(&ends.iter().copied().filter(|&e| e > covered).collect::<Vec<_>>());
        out.push(start..end);
        covered = end;
    }
    // And some exports of what's already covered, made again later.
    for _ in 0..rng.below(3) {
        let start = *rng.pick(&day_starts);
        let end = *rng.pick(&ends.iter().copied().filter(|&e| e > start).collect::<Vec<_>>());
        out.push(start..end);
    }
    rng.shuffle(&mut out);
    out
}

/// A Nordea export of `rows`, newest first, as the bank writes it.
fn export_csv(rows: &[Row]) -> String {
    let rows: Vec<(String, String, &str, String)> = rows
        .iter()
        .rev()
        .map(|(day, amount, what, balance)| (format!("2026/01/{day:02}"), danish(*amount), *what, danish(*balance)))
        .collect();
    let rows: Vec<(&str, &str, &str, &str)> =
        rows.iter().map(|(d, a, w, b)| (d.as_str(), a.as_str(), *w, b.as_str())).collect();
    nordea_csv(&rows)
}

/// A history in exports, as a case is saved: the text of the files.
#[derive(Serialize, Deserialize)]
struct Exports {
    /// One export of the whole history.
    whole: String,
    /// Exports that together cover it.
    parts: Vec<String>,
}

/// A history of up to `size` days, at most a week, and exports of it.
fn split_history(rng: &mut Rng, size: usize, mid_day: bool) -> Exports {
    let days = i8::try_from(size.min(7)).expect("at most 7");
    let rows = loop {
        let rows = history(rng, days);
        if !rows.is_empty() {
            break rows;
        }
    };
    let parts = exports(rng, &rows, mid_day, false).into_iter().map(|range| export_csv(&rows[range])).collect();
    Exports { whole: export_csv(&rows), parts }
}

fn merge(config: &Config, exports: &[String]) -> (SourceData, Findings) {
    let mut findings = Findings::default();
    let raw = exports
        .iter()
        .enumerate()
        .map(|(k, export)| {
            let rel = nordea_rel(&format!("2026-02-{:02} 10.00.00", k + 1));
            sources::parse_raw(config, "bank:a", &rel, export, None).expect("the generated export parses")
        })
        .collect();
    let data = sources::merge_source(config, "bank:a", raw, &mut findings);
    (data, findings)
}

/// What a merge gives: every transaction with its ID, in booking order.
fn merged_rows(data: &SourceData) -> Vec<String> {
    data.txs
        .iter()
        .map(|t| {
            format!(
                "{} {} {} {:?}",
                t.id,
                t.amount,
                t.balance_after.expect("the generated rows have a running balance"),
                t.description
            )
        })
        .collect()
}

/// "Any number of overlapping files is fine; merging sorts them out": the
/// same history, however it's split into exports, gives the same
/// transactions with the same IDs as one export of all of it, and no errors.
fn check_merging(case: &Exports) -> Result<(), String> {
    let config = config(NORDEA);
    let (alone, _) = merge(&config, std::slice::from_ref(&case.whole));
    let (split, findings) = merge(&config, &case.parts);
    let (want, got) = (merged_rows(&alone), merged_rows(&split));
    if want == got && !findings.has_errors() {
        return Ok(());
    }
    Err(format!("expected:\n  {}\ngot:\n  {}\n{}", want.join("\n  "), got.join("\n  "), findings.render(true)))
}

#[test]
fn merging_whole_day_exports_is_like_merging_one() {
    check_cases("merging-whole-days", 500, |rng, size| split_history(rng, size, false), check_merging);
}

#[test]
fn merging_exports_taken_during_the_day_is_like_merging_one() {
    check_cases("merging-mid-day", 500, |rng, size| split_history(rng, size, true), check_merging);
}

// ── IDs don't depend on which files there are (docs/spec.md §4) ────────

/// A Lunar account, exported and fetched.
const LUNAR: &str = r#"
[books]
start = 2026-01-01

[sources."lunar:b"]
bank = "lunar"
number = "555"
ledger = "assets:bank:b"
id_prefix = "lb"
formats = ["lunar-csv", "enable-banking"]
unmatched = "allow"
"#;

/// A Lunar export of `rows`, newest first. `first` is the place of the first
/// of them in the whole history, which gives each row its reference. Lunar's
/// exports have a time of day to the minute; here every row has the same.
fn lunar_csv(rows: &[Row], first: usize) -> String {
    let mut out = String::from("Date,Time,Title,Amount,Balance,Transaction ID\n");
    for (i, (day, amount, what, balance)) in rows.iter().enumerate().rev() {
        out.push_str(&format!("{day:02}.01.2026,12.00,{what},\"{amount},00\",\"{balance},00\",r{}\n", first + i));
    }
    out
}

/// Fetched data of `rows`, oldest first, with the references of `lunar_csv`.
/// Nordea's has running balances and Lunar's has none.
fn fetched(rows: &[Row], first: usize, balances: bool) -> String {
    let rows: Vec<String> = rows
        .iter()
        .enumerate()
        .map(|(i, (day, amount, what, balance))| {
            let indicator = if amount.is_sign_negative() { "DBIT" } else { "CRDT" };
            let balance = if balances {
                format!(r#", "balance_after_transaction": {{"currency": "DKK", "amount": "{balance}"}}"#)
            } else {
                String::new()
            };
            format!(
                r#"{{"entry_reference": "r{}", "booking_date": "2026-01-{day:02}", "status": "BOOK", "credit_debit_indicator": "{indicator}", "transaction_amount": {{"currency": "DKK", "amount": "{}"}}, "remittance_information": ["{what}"]{balance}}}"#,
                first + i,
                amount.abs()
            )
        })
        .collect();
    format!(
        r#"{{"acct_fetch": 1, "fetched_at": "2026-02-01T09:00:00Z", "pages": [{{"transactions": [{}]}}]}}"#,
        rows.join(", ")
    )
}

/// One file of an account: its path under data/, and its text.
#[derive(Serialize, Deserialize)]
struct File {
    rel: String,
    text: String,
}

/// A history in exports and fetches, as a case is saved.
#[derive(Serialize, Deserialize)]
struct Arrivals {
    /// Whether the account is with Lunar (else with Nordea).
    lunar: bool,
    /// One export of the whole history.
    whole: File,
    /// Exports and fetches that together cover it, in the order they arrive.
    parts: Vec<File>,
}

/// A history of up to `size` days, at most a week, in exports and fetches
/// that each start at the beginning of a day and may end during one. Where a
/// file sorts among the others has nothing to do with when it arrives.
///
/// With `any_part` a file may start partway through a day too, and no two
/// rows of the history are equal in everything: those nothing can tell apart
/// once they're in different files.
fn arrivals(rng: &mut Rng, size: usize, lunar: bool, any_part: bool) -> Arrivals {
    let days = i8::try_from(size.min(7)).expect("at most 7");
    let rows = loop {
        let rows = history(rng, days);
        let twice = rows.iter().enumerate().any(|(i, row)| rows[..i].contains(row));
        if !rows.is_empty() && !(any_part && twice) {
            break rows;
        }
    };
    let file = |k: usize, range: Range<usize>, fetch: bool| {
        let (first, rows) = (range.start, &rows[range]);
        let (folder, text) = match (lunar, fetch) {
            (true, false) => ("lunar/555", lunar_csv(rows, first)),
            (false, false) => ("nordea/1234567890", export_csv(rows)),
            (_, true) => (if lunar { "lunar/555" } else { "nordea/1234567890" }, fetched(rows, first, !lunar)),
        };
        File { rel: format!("{folder}/{k:02}.{}", if fetch { "json" } else { "csv" }), text }
    };
    let ranges = exports(rng, &rows, true, any_part);
    let mut names: Vec<usize> = (1..=ranges.len()).collect();
    rng.shuffle(&mut names);
    let parts = ranges.into_iter().zip(names).map(|(range, k)| file(k, range, rng.below(2) == 1)).collect();
    Arrivals { lunar, whole: file(0, 0..rows.len(), false), parts }
}

/// Every transaction with its ID, sorted: what a pin on that ID would map.
fn identified(data: &SourceData) -> Vec<String> {
    let mut rows: Vec<String> =
        data.txs.iter().map(|t| format!("{} {} {} {:?}", t.id, t.booked, fmt_num(t.amount), t.description)).collect();
    rows.sort_unstable();
    rows
}

/// "IDs must never change": as the files of a history arrive one by one,
/// exports and fetches in any order, every transaction that's there has the
/// ID it has in one export of the whole history. With all of them, the
/// transactions are those of that export, and nothing is wrong.
fn check_arrivals(case: &Arrivals) -> Result<(), String> {
    let (config, source) = if case.lunar {
        (config(LUNAR), "lunar:b")
    } else {
        (config(&NORDEA.replace("[\"nordea-csv\"]", "[\"nordea-csv\", \"enable-banking\"]")), "bank:a")
    };
    let merge = |files: &[File]| {
        let mut findings = Findings::default();
        let raw = files
            .iter()
            .map(|f| sources::parse_raw(&config, source, &f.rel, &f.text, None).expect("the generated file parses"))
            .collect();
        (sources::merge_source(&config, source, raw, &mut findings), findings)
    };
    let (whole, _) = merge(std::slice::from_ref(&case.whole));
    let want = identified(&whole);
    for arrived in 1..=case.parts.len() {
        let (data, findings) = merge(&case.parts[..arrived]);
        let got = identified(&data);
        let mut left = want.clone();
        let all_known = got.iter().all(|row| left.iter().position(|w| w == row).map(|i| left.swap_remove(i)).is_some());
        let complete = arrived < case.parts.len() || (left.is_empty() && !findings.has_errors());
        if !all_known || !complete {
            return Err(format!(
                "with {arrived} of {} files:\n  {}\nfrom the whole history:\n  {}\n{}",
                case.parts.len(),
                got.join("\n  "),
                want.join("\n  "),
                findings.render(true)
            ));
        }
    }
    Ok(())
}

#[test]
fn a_nordea_id_is_the_same_whichever_files_have_arrived() {
    check_cases("ids-nordea", 500, |rng, size| arrivals(rng, size, false, false), check_arrivals);
}

#[test]
fn a_lunar_id_is_the_same_whichever_files_have_arrived() {
    check_cases("ids-lunar", 500, |rng, size| arrivals(rng, size, true, false), check_arrivals);
}

/// What the store of transactions is for: as the files of a history are read
/// into it one by one, exports and fetches in any order, an ID keeps meaning
/// the transaction it was given to, and in the end the table has the
/// transactions of the whole history, each once. Unless `may_be_unsure`,
/// the reader is never unsure about a row either.
fn check_store(case: &Arrivals, may_be_unsure: bool) -> Result<(), String> {
    let (config, source) = if case.lunar {
        (config(LUNAR), "lunar:b")
    } else {
        (config(&NORDEA.replace("[\"nordea-csv\"]", "[\"nordea-csv\", \"enable-banking\"]")), "bank:a")
    };
    let rows_of = |f: &File| sources::parse_raw(&config, source, &f.rel, &f.text, None).expect("the file parses").rows;
    let mut minted = 0u128;
    let mut mint = || {
        minted += 1;
        TransactionId::from_random(minted.to_be_bytes())
    };
    let mut table = Table::default();
    let mut given: BTreeMap<TransactionId, String> = BTreeMap::new();
    for (arrived, file) in case.parts.iter().enumerate() {
        let reading =
            table.read(source, &rows_of(file), &mut mint).map_err(|e| format!("reading {}: {e:#}", file.rel))?;
        if !(reading.unsure.is_empty() || may_be_unsure) {
            return Err(format!("reading {} (file {} to arrive): {reading:?}", file.rel, arrived + 1));
        }
        for t in table.transactions() {
            let now = format!("{} {} {:?}", t.date, fmt_num(t.amount), t.text.as_deref().unwrap_or(""));
            if let Some(was) = given.insert(t.id.clone(), now.clone())
                && was != now
            {
                return Err(format!(
                    "after {} (file {} to arrive), {} is {now}; it was {was}",
                    file.rel,
                    arrived + 1,
                    t.id
                ));
            }
        }
        if table.transactions().count() != given.len() {
            return Err(format!("after {} (file {} to arrive), a transaction is gone", file.rel, arrived + 1));
        }
    }
    let mut want: Vec<String> = rows_of(&case.whole)
        .iter()
        .map(|t| format!("{} {} {:?}", t.booked, fmt_num(t.amount), t.description))
        .collect();
    want.sort_unstable();
    let mut got: Vec<String> = given.into_values().collect();
    got.sort_unstable();
    if want != got {
        return Err(format!("the table has:\n  {}\nthe whole history:\n  {}", got.join("\n  "), want.join("\n  ")));
    }
    Ok(())
}

#[test]
fn the_store_keeps_every_id_as_a_nordea_accounts_files_arrive() {
    check_cases("store-nordea", 500, |rng, size| arrivals(rng, size, false, false), |case| check_store(case, false));
}

#[test]
fn the_store_keeps_every_id_as_a_lunar_accounts_files_arrive() {
    check_cases("store-lunar", 500, |rng, size| arrivals(rng, size, true, false), |case| check_store(case, false));
}

/// The case counting can't handle: a file that has a transaction without the
/// ones before it that day, so that one that looks the same turns up earlier
/// later on. The store may say it isn't sure, but each transaction still gets
/// one ID and keeps it.
#[test]
fn the_store_keeps_every_id_when_nordea_files_start_anywhere() {
    check_cases(
        "store-nordea-parts",
        500,
        |rng, size| arrivals(rng, size, false, true),
        |case| check_store(case, true),
    );
}

#[test]
fn the_store_keeps_every_id_when_lunar_files_start_anywhere() {
    check_cases("store-lunar-parts", 500, |rng, size| arrivals(rng, size, true, true), |case| check_store(case, true));
}

// ── Order-independent rules (docs/spec.md §1) ──────────────────────────

const RULES: [&str; 4] = [
    r#"
[[rule]]
name = "coffee"
when = { description_re = '(?i)coffee' }
account = "expenses:coffee"
"#,
    r#"
[[rule]]
name = "coffee-shop"
when = { description = "Coffee Shop" }
priority = 1
account = "expenses:coffee:shop"
"#,
    r#"
[[rule]]
name = "rent"
when = { description = "Rent" }
subscription = "rent"
"#,
    r#"
[[subscription]]
name = "rent"
kind = "expense"
account = "expenses:rent"
every = "1 month"
first = 2026-01-01
prices = [{ from = 2026-01-01, amount = 5000 }]
"#,
];

/// Two more subscriptions, due the same day.
const SAME_DAY: [&str; 2] = [
    r#"
[[subscription]]
name = "music"
kind = "expense"
account = "expenses:music"
every = "1 month"
first = 2026-01-03
prices = [{ from = 2026-01-01, amount = 99 }]
"#,
    r#"
[[subscription]]
name = "video"
kind = "expense"
account = "expenses:video"
every = "1 month"
first = 2026-01-03
prices = [{ from = 2026-01-01, amount = 79 }]
"#,
];

/// "The order of rules and the files they're in never matter": shuffling
/// `snippets` and spreading them over up to three files gives the same
/// journals. Returns the first difference found.
fn check_rule_order(snippets: &[&str]) -> Option<String> {
    let config = config(NORDEA);
    let csv = nordea_csv(&[
        ("2026/01/20", "-40,00", "Coffee Shop", "4.730,00"),
        ("2026/01/12", "-30,00", "Coffee", "4.770,00"),
        ("2026/01/02", "-5.000,00", "Rent", "4.800,00"),
    ]);
    let rel = nordea_rel("2026-01-31 10.00.00");
    let accounts = ["expenses:coffee", "expenses:coffee:shop", "expenses:rent", "expenses:music", "expenses:video"];
    let books = |files: &[(String, String)]| {
        let files: Vec<(&str, &str)> = files.iter().map(|(f, t)| (f.as_str(), t.as_str())).collect();
        let b = build(&config, rules(&config, &files), &accounts, &[("bank:a", &rel, &csv)], date(2026, 2, 15));
        (b.journal, b.forecast)
    };
    let canonical = books(&[("config/rules.toml".into(), snippets.concat())]);
    let mut rng = Rng::new(3);
    for _ in 0..50 {
        let mut order: Vec<&str> = snippets.to_vec();
        rng.shuffle(&mut order);
        let mut files: Vec<(String, String)> =
            (0..3).map(|k| (format!("config/rules/{k}.toml"), String::new())).collect();
        for snippet in &order {
            files[rng.below(3)].1.push_str(snippet);
        }
        let got = books(&files);
        if got != canonical {
            let names: Vec<String> = files.iter().map(|(f, t)| format!("{f}:{t}")).collect();
            let first = |a: &str, b: &str| {
                a.lines().zip(b.lines()).find(|(x, y)| x != y).map(|(x, y)| format!("{x:?}\n  vs {y:?}"))
            };
            let diff = first(&canonical.0, &got.0).or_else(|| first(&canonical.1, &got.1));
            return Some(format!("first difference: {diff:?}\nwith files:\n{}", names.join("\n")));
        }
    }
    None
}

#[test]
fn pattern_rules_and_a_subscription_dont_depend_on_order() {
    if let Some(report) = check_rule_order(&RULES) {
        panic!("{report}");
    }
}

#[test]
fn subscriptions_due_the_same_day_dont_depend_on_order() {
    let snippets: Vec<&str> = RULES.iter().chain(&SAME_DAY).copied().collect();
    if let Some(report) = check_rule_order(&snippets) {
        panic!("{report}");
    }
}

// ── Amounts (docs/spec.md §5: "plain hledger posting amounts") ─────────

/// Amounts of up to 13 digits, with up to two decimals, come back exactly
/// from how konter writes them, from Danish bank notation, and from a rule
/// file, whether written as a number or a string.
#[test]
fn amounts_read_back_exactly() {
    #[derive(Deserialize)]
    struct InRules {
        amount: Amount,
    }
    /// An amount, as a case is saved: `digits` × 10^-`decimals`.
    #[derive(Serialize, Deserialize)]
    struct Written {
        digits: i64,
        decimals: u32,
    }
    let generate = |rng: &mut Rng, _size: usize| {
        let digits = 1 + u32::try_from(rng.below(13)).expect("below 13");
        let mantissa = i64::try_from(rng.next() % 10u64.pow(digits)).expect("at most 13 digits");
        let sign = if rng.below(2) == 0 { 1 } else { -1 };
        Written { digits: sign * mantissa, decimals: u32::try_from(rng.below(3)).expect("below 3") }
    };
    check_cases("amounts", 5000, generate, |case| {
        let d = Decimal::new(case.digits, case.decimals);
        assert_eq!(parse_plain(&fmt_num(d)).expect("what fmt_num writes parses"), d, "{d}");
        assert_eq!(parse_danish(&danish(d)).expect("Danish notation parses"), d, "{}", danish(d));
        for written in [fmt_num(d), format!("\"{}\"", fmt_num(d))] {
            let read: InRules = toml::from_str(&format!("amount = {written}")).expect("the amount reads in the rules");
            assert_eq!(read.amount.0, d, "amount = {written}");
        }
        Ok(())
    });
}

// ── Whole builds (docs/spec.md §1 and §7) ──────────────────────────────

/// Two imported accounts and one without data.
const TWO_BANKS: &str = r#"
[books]
start = 2026-01-01

[sources."bank:a"]
bank = "nordea"
number = "1234567890"
ledger = "assets:bank:a"
id_prefix = "ba"
formats = ["nordea-csv"]
unmatched = "allow"

[sources."bank:b"]
bank = "nordea"
number = "2222222222"
ledger = "assets:bank:b"
id_prefix = "bb"
formats = ["nordea-csv"]
unmatched = "allow"

[sources."bank:savings"]
bank = "nordea"
ledger = "assets:bank:savings"
"#;

/// The imported accounts: (source, account number).
const IMPORTED: [(&str, &str); 2] = [("bank:a", "1234567890"), ("bank:b", "2222222222")];

const ACCOUNTS: [&str; 11] = [
    "expenses:groceries",
    "expenses:coffee",
    "expenses:coffee:shop",
    "income:salary",
    "expenses:dinner",
    "assets:receivable:friend",
    "assets:prepaid",
    "expenses:insurance",
    "expenses:rent",
    "expenses:kiosk",
    "expenses:gifts",
];

/// What a bank row can be, other than a transfer, a card hold or rent: its
/// description, and whether the money comes in.
const KINDS: [(&str, bool); 9] = [
    ("Groceries", false),
    ("Coffee", false),
    ("Coffee Shop", false),
    ("Salary", true),
    ("Dinner", false),
    ("Insurance", false),
    ("Mystery", false),
    ("Savings", false),
    ("Kiosk", false),
];

/// Rules that fit the rows above and never disagree: any of these topics can
/// be left out, and what's left still maps every transaction it matches in
/// one way. Between them they use every kind of output and most conditions.
const TOPICS: [&str; 10] = [
    r#"
[[rule]]
name = "groceries"
when = { description = "Groceries" }
account = "expenses:groceries"
tags = { shop = "corner" }
"#,
    r#"
[[rule]]
name = "coffee"
when = { description_re = '(?i)^coffee' }
account = "expenses:coffee"
"#,
    r#"
[[rule]]
name = "coffee-shop"
when = { description = "Coffee Shop" }
priority = 1
account = "expenses:coffee:shop"
payee = "The coffee shop"
"#,
    r#"
[[rule]]
name = "salary"
when = { description = "Salary", direction = "in" }
account = "income:salary"
"#,
    r#"
[[rule]]
name = "dinner"
when = { description = "Dinner" }
note = "A friend pays the rest back"
split = [
  { account = "expenses:dinner", amount = 40 },
  { account = "assets:receivable:friend", rest = true },
]
"#,
    r#"
[[rule]]
name = "insurance"
when = { description = "Insurance" }
transactions = [
  { payee = "Insurance (prepaid)", postings = [
      { account = "@bank" },
      { account = "assets:prepaid", rest = true } ] },
  { date = 2026-02-01, payee = "Insurance", postings = [
      { account = "assets:prepaid", amount = "@amount" },
      { account = "expenses:insurance", rest = true } ] },
]
"#,
    r#"
[[rule]]
name = "mystery"
when = { description = "Mystery" }
unknown = "nobody remembers"
"#,
    r#"
[[rule]]
name = "savings"
when = { description = "Savings" }
transfer = "bank:savings"
"#,
    r#"
[[rule]]
name = "card-hold"
when = { description = "Card hold" }
ignore = "the hold and its release cancel out"
"#,
    r#"
[[rule]]
name = "kiosk-small"
when = { description = "Kiosk", amount_range = [-60, 0] }
account = "expenses:kiosk"

[[rule]]
name = "kiosk-late"
when = { description = "Kiosk", not = { amount_range = [-60, 0] }, any = [{ date_from = 2026-01-15 }, { source = "bank:b" }] }
account = "expenses:kiosk"
"#,
];

/// Rent and its subscription, due every `every`.
fn rent(every: &str) -> String {
    format!(
        r#"
[[rule]]
name = "rent"
when = {{ description = "Rent" }}
subscription = "rent"

[[subscription]]
name = "rent"
kind = "expense"
account = "expenses:rent"
every = "{every}"
first = 2026-01-01
prices = [{{ from = 2026-01-01, amount = 5000 }}, {{ from = 2026-02-01, amount = 5200 }}]
"#
    )
}

/// The two sides of the transfers between the imported accounts, one rule each.
const TRANSFERS: [&str; 4] = [
    "\n[[rule]]\nname = \"to-b\"\nwhen = { source = \"bank:a\", description = \"To B\" }\ntransfer = \"bank:b\"\n",
    "\n[[rule]]\nname = \"from-a\"\nwhen = { source = \"bank:b\", description = \"From A\" }\ntransfer = \"bank:a\"\n",
    "\n[[rule]]\nname = \"to-a\"\nwhen = { source = \"bank:b\", description = \"To A\" }\ntransfer = \"bank:a\"\n",
    "\n[[rule]]\nname = \"from-b\"\nwhen = { source = \"bank:a\", description = \"From B\" }\ntransfer = \"bank:b\"\n",
];

/// Rules that disagree with the ones above, or don't fit what they match:
/// each should give an error among the findings, and still books that add up.
const TROUBLE: [&str; 4] = [
    // Ambiguous with "coffee".
    "\n[[rule]]\nname = \"coffee-twin\"\nwhen = { description = \"Coffee\" }\naccount = \"expenses:coffee\"\n",
    // Invalid unless the groceries cost 60.
    r#"
[[rule]]
name = "groceries-halves"
when = { description = "Groceries" }
priority = 5
split = [
  { account = "expenses:groceries", amount = 30 },
  { account = "expenses:gifts", amount = 30 },
]
"#,
    // Ignores purchases that nothing cancels out.
    "\n[[rule]]\nname = \"kiosk-gone\"\nwhen = { description = \"Kiosk\" }\npriority = 9\nignore = \"never happened\"\n",
    // A transfer whose other side isn't one.
    "\n[[rule]]\nname = \"dinner-moved\"\nwhen = { source = \"bank:a\", description = \"Dinner\" }\npriority = 9\ntransfer = \"bank:b\"\n",
];

/// Everything a build is made from, as a case is saved.
#[derive(Serialize, Deserialize)]
struct World {
    /// Whether nothing in it is wrong, so that it builds without errors.
    sound: bool,
    as_of: Date,
    /// Decides which files the rules are moved to, to see that it doesn't matter.
    placement: u32,
    config: String,
    /// Pieces of rule files. Their order and the files they're in shouldn't matter.
    rules: Vec<String>,
    /// config/transactions.toml.
    transactions: String,
    /// Where each imported account ends at the bank: (source, balance).
    ends: Vec<(String, String)>,
    /// The raw files: (source, path, text).
    files: Vec<(String, String, String)>,
}

impl World {
    /// Builds the books with the rules in `rule_files`: (file, text).
    fn build(&self, rule_files: &[(String, String)]) -> Result<Build, String> {
        let config = config(&self.config);
        let rule_files = rule_files.iter().map(|(file, text)| (file.as_str(), text.as_str()));
        let rules = Rules::from_files(rule_files, &config).map_err(|e| format!("the rules don't load: {e:#}"))?;
        let files: Vec<(&str, &str, &str)> =
            self.files.iter().map(|(source, rel, text)| (source.as_str(), rel.as_str(), text.as_str())).collect();
        try_build_with(&config, rules, &ACCOUNTS, &self.transactions, &files, self.as_of)
            .map_err(|e| format!("the books don't build: {e:#}"))
    }
}

fn decimal(text: &str) -> Decimal {
    parse_plain(text).expect("the test's amount is a decimal")
}

/// An amount to pay or receive. With `extremes`, some are as long as amounts
/// get: 15 digits, 8 decimals.
fn some_amount(rng: &mut Rng, extremes: bool) -> Decimal {
    const PLAIN: [&str; 6] = ["12.50", "49.95", "60", "100", "100", "1234.56"];
    const EXTREME: [&str; 4] = ["499999999999999.99999999", "123456789012345.12345678", "0.00000001", "0.01"];
    if extremes && rng.below(3) == 0 { decimal(rng.pick(&EXTREME)) } else { decimal(rng.pick(&PLAIN)) }
}

/// Up to `days` days on the two imported accounts, from 2 January: purchases
/// and income, transfers between the two (the other side arrives up to five
/// days later, which is as far apart as two sides pair), card holds released
/// the same day, and rent.
fn histories(rng: &mut Rng, days: i8, extremes: bool) -> Vec<Vec<Row>> {
    // (day, amount, description, whether the sign is free to change)
    let mut events: [Vec<(i8, Decimal, &'static str, bool)>; 2] = [Vec::new(), Vec::new()];
    for day in 2..2 + days {
        for account in 0..2 {
            for _ in 0..rng.below(3) {
                match rng.below(12) {
                    0 => {
                        let amount = some_amount(rng, false);
                        let lag = i8::try_from(rng.below(6)).expect("below 6");
                        let (to, from) = if account == 0 { ("To B", "From A") } else { ("To A", "From B") };
                        events[account].push((day, -amount, to, false));
                        events[1 - account].push((day + lag, amount, from, false));
                    }
                    1 => {
                        let amount = some_amount(rng, false);
                        events[account].push((day, -amount, "Card hold", false));
                        events[account].push((day, amount, "Card hold", false));
                    }
                    2 => events[account].push((
                        day,
                        decimal(rng.pick(&["-5000", "-5000", "-4000", "-6000"])),
                        "Rent",
                        false,
                    )),
                    _ => {
                        let (what, incoming) = *rng.pick(&KINDS);
                        let amount = some_amount(rng, extremes);
                        events[account].push((day, if incoming { amount } else { -amount }, what, true));
                    }
                }
            }
        }
    }
    // A balance stays under half of the largest amount there is, give or take
    // the plain amounts: an extreme one that would take it past that goes the
    // other way.
    let half = decimal("500000000000000");
    events
        .into_iter()
        .map(|mut events| {
            // Stable, so a hold and its release stay together.
            events.sort_by_key(|event| event.0);
            let mut balance = decimal(rng.pick(&["1000", "0", "-250.75"]));
            let mut rows = Vec::new();
            for (day, mut amount, what, free) in events {
                if free && (balance + amount).abs() >= half {
                    amount = -amount;
                }
                balance += amount;
                rows.push((day, amount, what, balance));
            }
            rows
        })
        .collect()
}

/// config/transactions.toml: sometimes a loan that's written off, and in a
/// `wild` world sometimes a transaction that doesn't balance and one whose
/// balance assertion fails.
fn hand_written(rng: &mut Rng, wild: bool) -> String {
    let mut transactions = String::new();
    if rng.below(2) == 0 {
        transactions.push_str(
            r#"
[[transaction]]
date = 2026-01-20
payee = "A gift, lent and written off"
tags = { to = "friend" }
postings = [
  { account = "assets:receivable:friend", amount = 25 },
  { account = "expenses:gifts", rest = true },
]

[[transaction]]
date = 2026-01-25
payee = "Written off"
postings = [
  { account = "assets:receivable:friend", balance = 0 },
  { account = "expenses:gifts", rest = true },
]
"#,
        );
    }
    if wild && rng.below(4) == 0 {
        // One that doesn't balance.
        transactions.push_str(
            r#"
[[transaction]]
date = 2026-01-22
payee = "Lopsided"
postings = [
  { account = "expenses:gifts", amount = 10 },
  { account = "assets:prepaid", amount = -9 },
]
"#,
        );
    }
    if wild && rng.below(4) == 0 {
        // One whose assertion fails.
        transactions.push_str(
            r#"
[[transaction]]
date = 2026-01-23
payee = "Miscounted"
postings = [
  { account = "expenses:gifts", amount = 10, balance = 999 },
  { account = "assets:prepaid", rest = true },
]
"#,
        );
    }
    transactions
}

/// Adds up to two pins or later marks to the world's rules. Unless it's
/// `wild` they're on purchases that nothing else depends on, and a pin only
/// books to an account.
fn pin_and_mark(rng: &mut Rng, world: &mut World, wild: bool) {
    // Pins and later marks go by ID, and IDs come from a build. If there's
    // none to be had, checking the world says why.
    let Ok(build) = world.build(&[]) else { return };
    let all: Vec<(&str, &str)> = build
        .loaded
        .sources
        .values()
        .flat_map(|data| &data.txs)
        .map(|t| (t.id.as_str(), t.description.as_str()))
        .collect();
    // A pin on one of these is never wrong: nothing else depends on them.
    let plain: Vec<&str> =
        all.iter().filter(|(_, what)| ["Kiosk", "Groceries", "Dinner"].contains(what)).map(|t| t.0).collect();
    let mut taken = BTreeSet::new();
    for _ in 0..rng.below(3) {
        let pool: Vec<&str> = if wild { all.iter().map(|t| t.0).collect() } else { plain.clone() };
        if pool.is_empty() {
            continue;
        }
        let id = *rng.pick(&pool);
        if !taken.insert(id) {
            continue;
        }
        let outputs = [
            "account = \"expenses:gifts\"\npayee = \"A present\"",
            "later",
            "unknown = \"lost the receipt\"",
            "ignore = \"never happened\"",
        ];
        match *rng.pick(if wild { &outputs[..] } else { &outputs[..2] }) {
            "later" => world.rules.push(format!("\n[later.{id:?}]\nnote = \"ask about it\"\n")),
            output => world.rules.push(format!("\n[pin.{id:?}]\n{output}\n")),
        }
    }
    if wild && rng.below(4) == 0 {
        world.rules.push("\n[pin.\"ba-2026-01-31-000000\"]\naccount = \"expenses:gifts\"\n".to_string());
    }
}

/// A world of the given `size`. Unless it's `wild`, nothing in it is wrong:
/// the rules agree, every transfer has both its sides, and what's written by
/// hand adds up. A wild one also has some of what shouldn't be there.
fn world(rng: &mut Rng, size: usize, wild: bool) -> World {
    let days = i8::try_from((2 * size).min(24)).expect("at most 24");
    let extremes = rng.below(4) == 0;
    let (mut files, mut ends) = (Vec::new(), Vec::new());
    for ((source, number), rows) in IMPORTED.iter().zip(histories(rng, days, extremes)) {
        let Some(last) = rows.last() else { continue };
        ends.push(((*source).to_string(), last.3.to_string()));
        // Half of them taken during the day, as exports usually are.
        let mid_day = rng.below(2) == 0;
        for (k, range) in exports(rng, &rows, mid_day, false).into_iter().enumerate() {
            let rel = nordea_rel_of(number, &format!("2026-02-{:02} 10.00.00", k + 1));
            files.push(((*source).to_string(), rel, export_csv(&rows[range])));
        }
    }

    let mut rules: Vec<String> = TOPICS.iter().filter(|_| rng.below(4) > 0).map(|topic| (*topic).to_string()).collect();
    if rng.below(4) > 0 {
        let every = if wild { *rng.pick(&["1 month", "1 week", "10 days", "1 year"]) } else { "1 month" };
        rules.push(rent(every));
    }
    if rng.below(4) > 0 {
        rules.extend(TRANSFERS.iter().map(|rule| (*rule).to_string()));
        if wild && rng.below(3) == 0 {
            rules.remove(rules.len() - 1 - rng.below(4));
        }
    }
    if wild {
        rules.extend(TROUBLE.iter().filter(|_| rng.below(4) == 0).map(|rule| (*rule).to_string()));
    }

    let transactions = hand_written(rng, wild);
    let mut config = TWO_BANKS.to_string();
    if wild && rng.below(3) == 0 {
        config = config.replacen("unmatched = \"allow\"", "unmatched = \"error\"", 1);
    }
    let as_of =
        if wild && rng.below(4) == 0 { date(2026, 1, 15) } else { *rng.pick(&[date(2026, 1, 31), date(2026, 3, 1)]) };
    let placement = u32::try_from(rng.below(1 << 30)).expect("below 2^30");
    let mut world = World { sound: !wild, as_of, placement, config, rules, transactions, ends, files };
    pin_and_mark(rng, &mut world, wild);
    world
}

/// The errors among a build's findings, as `category: message`.
fn errors(build: &Build) -> Vec<String> {
    let errors = build.findings.diags.iter().filter(|d| d.severity == Severity::Error);
    errors.map(|d| format!("{}: {} {:?}", d.category, d.message, d.details)).collect()
}

/// "Every bank transaction is booked exactly once": one posting to its
/// account, with the bank's amount and the bank's balance after it. Only
/// an ignored one is not in the books.
fn booked_once(config: &Config, build: &Build) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for (name, data) in &build.loaded.sources {
        let ledger = &config.sources[name].ledger;
        for t in &data.txs {
            ids.insert(t.id.as_str());
            let Some(outcome) = build.generated.outcomes.get(&t.id) else {
                return Err(format!("{} has no outcome", t.id));
            };
            let made = build.books.iter().filter(|tx| tx.tags.contains(&(tags::BANK_ID.to_string(), t.id.clone())));
            let bank: Vec<_> = made.flat_map(|tx| &tx.postings).filter(|p| p.account == *ledger).collect();
            let booked_once = match bank.as_slice() {
                [] => outcome.ignored.is_some(),
                [p] => outcome.ignored.is_none() && p.amount == t.amount && p.assert_balance == t.balance_after,
                _ => false,
            };
            if !booked_once {
                return Err(format!("{} ({} {:?}) isn't booked exactly once: {bank:?}", t.id, t.amount, t.description));
            }
        }
    }
    for tx in &build.books {
        if let Some((_, id)) = tx.tags.iter().find(|(tag, _)| tag == tags::BANK_ID)
            && !ids.contains(id.as_str())
        {
            return Err(format!("{} {:?} is booked from {id}, which the bank doesn't have", tx.date, tx.description));
        }
    }
    Ok(())
}

/// "Every build checks what `hledger check --strict` would": going through
/// the books again, in their order, finds something wrong exactly when the
/// build says so.
fn books_add_up(build: &Build) -> Result<(), String> {
    let mut balances: BTreeMap<&str, Decimal> = BTreeMap::new();
    let mut wrong = Vec::new();
    for tx in &build.books {
        let sum: Decimal = tx.postings.iter().map(|p| p.amount).sum();
        if !sum.is_zero() {
            wrong.push(format!("{} {:?} is off by {sum}", tx.date, tx.description));
        }
        for p in &tx.postings {
            let balance = balances.entry(&p.account).or_default();
            *balance += p.amount;
            if p.assert_balance.is_some_and(|asserted| asserted != *balance) {
                wrong.push(format!(
                    "{} {:?}: {} is {balance}, not {:?}",
                    tx.date, tx.description, p.account, p.assert_balance
                ));
            }
        }
    }
    let reported = build.findings.diags.iter().any(|d| d.severity == Severity::Error && d.category == "books");
    if wrong.is_empty() == reported {
        return Err(format!("the build's check of the books says {reported}, but going through them finds {wrong:?}"));
    }
    Ok(())
}

/// What docs/spec.md promises of every build, whatever the inputs are like.
/// A sound world also builds without errors.
fn check_world(world: &World) -> Result<(), String> {
    let config = config(&world.config);
    let one_file = [("config/rules.toml".to_string(), world.rules.concat())];
    let build = world.build(&one_file)?;
    let errors = errors(&build);
    if world.sound && !errors.is_empty() {
        return Err(format!("nothing is wrong with the inputs, but the build has errors:\n  {}", errors.join("\n  ")));
    }

    booked_once(&config, &build)?;
    books_add_up(&build)?;

    // "The bank is ground truth": without errors, each account ends where
    // the bank says it does.
    if errors.is_empty() {
        for (source, end) in &world.ends {
            let books = posted(&build, &config.sources[source].ledger, date(9999, 12, 31));
            if books != decimal(end) {
                return Err(format!("{source} ends at {end} at the bank and at {books} in the books"));
            }
        }
    }

    // "The same inputs and `--as-of` give byte-identical journals."
    let again = world.build(&one_file)?;
    if (&again.journal, &again.forecast) != (&build.journal, &build.forecast)
        || again.findings.render(true) != build.findings.render(true)
    {
        return Err("building twice gives two results".to_string());
    }

    // "The order of rules and the files they're in never matter."
    let mut rng = Rng::new(u64::from(world.placement));
    let mut order: Vec<&String> = world.rules.iter().collect();
    rng.shuffle(&mut order);
    let mut files: Vec<(String, String)> = (0..3).map(|k| (format!("config/rules/{k}.toml"), String::new())).collect();
    for piece in order {
        files[rng.below(3)].1.push_str(piece);
    }
    let moved = world.build(&files)?;
    if (&moved.journal, &moved.forecast) != (&build.journal, &build.forecast) {
        let first =
            |a: &str, b: &str| a.lines().zip(b.lines()).find(|(x, y)| x != y).map(|(x, y)| format!("{x:?} vs {y:?}"));
        let difference = first(&build.journal, &moved.journal).or_else(|| first(&build.forecast, &moved.forecast));
        let files: Vec<String> = files.iter().map(|(file, text)| format!("{file}:{text}")).collect();
        return Err(format!("the journals depend on where the rules are: {difference:?}\n{}", files.join("\n")));
    }
    Ok(())
}

/// Books from inputs with nothing wrong in them build without errors, and
/// keep every promise `check_world` knows.
#[test]
fn sound_inputs_build_without_errors_and_keep_the_specs_promises() {
    check_cases("sound-books", 300, |rng, size| world(rng, size, false), check_world);
}

/// Rules that disagree, transfers with one side, ignored purchases and
/// hand-written transactions that don't add up are reported, and the books
/// still keep every promise.
#[test]
fn unsound_inputs_are_reported_and_the_books_still_keep_the_specs_promises() {
    check_cases("unsound-books", 300, |rng, size| world(rng, size, true), check_world);
}
