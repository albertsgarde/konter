//! Property tests: invariants from docs/spec.md, checked on many generated
//! inputs instead of a few hand-picked ones. Small inputs are enumerated
//! exhaustively, smallest first, so a failure names a minimal case; larger
//! ones come from a seeded generator, so every failure can be replayed. They
//! need no dependencies. The ones that fail today are ignored, like those in
//! breakage.rs: `cargo test -p konter-engine --test properties -- --ignored`.

mod common;

use std::ops::Range;

use common::{NORDEA, Rng, build, config, danish, nordea_csv, nordea_rel, rules};
use jiff::civil::date;
use konter_engine::config::Config;
use konter_engine::dates::days_between;
use konter_engine::findings::Findings;
use konter_engine::money::{Amount, fmt_num, parse_danish, parse_plain};
use konter_engine::sources::{self, SourceData};
use konter_engine::transfers::{Leg, pair};
use rust_decimal::Decimal;

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
            for code in 0..choices.len().pow(n as u32) {
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

/// Up to 10 legs between three accounts over a month.
fn random_legs(rng: &mut Rng) -> Vec<Leg> {
    let accounts = ["a", "b", "c"];
    (0..1 + rng.below(10))
        .map(|k| {
            let from = *rng.pick(&accounts);
            let to = *rng.pick(&accounts.iter().copied().filter(|a| *a != from).collect::<Vec<_>>());
            let amount = *rng.pick(&[-500, 500, -300, 300]);
            leg(from, to, 1 + rng.below(28) as i8, amount, k)
        })
        .collect()
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
    let mut rng = Rng::new(1);
    for _ in 0..5000 {
        let legs = random_legs(&mut rng);
        if let Err(e) = check_pairs(&legs, 5) {
            panic!("{e}\n{}", describe(&legs));
        }
    }
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
    let mut rng = Rng::new(2);
    for _ in 0..5000 {
        let legs = random_legs(&mut rng);
        let (pairs, _) = pair(&legs, 5);
        assert_eq!(pairs.len(), most_pairs(&legs, 5), "\n{}", describe(&legs));
    }
}

// ── Merging overlapping exports (docs/spec.md §4) ──────────────────────

/// One row of a bank history: (day in January, amount, description, balance after).
type Row = (i8, Decimal, &'static str, Decimal);

/// A week on an account with a running balance. Few distinct amounts, so the
/// same day, amount and balance come up again, as with money collected and
/// passed on.
fn history(rng: &mut Rng) -> Vec<Row> {
    const KINDS: [(i64, &str); 5] =
        [(100, "MobilePay Alice"), (100, "MobilePay Carol"), (-100, "Club dinner"), (-50, "Groceries"), (50, "Refund")];
    let mut balance = Decimal::from(1000);
    let mut rows = Vec::new();
    for day in 1..=7 {
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
/// else at the end of a day.
fn exports(rng: &mut Rng, rows: &[Row], mid_day: bool) -> Vec<Range<usize>> {
    let day_starts: Vec<usize> = (0..rows.len()).filter(|&i| i == 0 || rows[i].0 != rows[i - 1].0).collect();
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

fn merge(config: &Config, rows: &[Row], exports: &[Range<usize>]) -> (SourceData, Findings) {
    let mut findings = Findings::default();
    let raw = exports
        .iter()
        .enumerate()
        .map(|(k, range)| {
            let rel = nordea_rel(&format!("2026-02-{:02} 10.00.00", k + 1));
            sources::parse_raw(config, "bank:a", &rel, &export_csv(&rows[range.clone()]), None).unwrap()
        })
        .collect();
    let data = sources::merge_source(config, "bank:a", raw, &mut findings);
    (data, findings)
}

/// What a merge gives: every transaction with its ID, in booking order.
fn merged_rows(data: &SourceData) -> Vec<String> {
    data.txs.iter().map(|t| format!("{} {} {} {:?}", t.id, t.amount, t.balance_after.unwrap(), t.description)).collect()
}

/// "Any number of overlapping files is fine; merging sorts them out": the
/// same history, however it's split into exports, gives the same
/// transactions with the same IDs as one export of all of it, and no errors.
/// Returns the smallest counterexample found.
fn check_merging(mid_day: bool, seeds: u64) -> Option<String> {
    let config = config(NORDEA);
    let mut smallest: Option<(usize, String)> = None;
    for seed in 0..seeds {
        let mut rng = Rng::new(seed);
        let rows = history(&mut rng);
        if rows.is_empty() {
            continue;
        }
        let exports = exports(&mut rng, &rows, mid_day);
        // One export of the whole history.
        let (alone, _) = merge(&config, &rows, std::slice::from_ref(&(0..rows.len())));
        let (split, findings) = merge(&config, &rows, &exports);
        let (want, got) = (merged_rows(&alone), merged_rows(&split));
        if want == got && !findings.has_errors() {
            continue;
        }
        if smallest.as_ref().is_some_and(|(n, _)| *n <= rows.len()) {
            continue;
        }
        let history: Vec<String> =
            rows.iter().map(|r| format!("  day {}: {} {:?}, balance {}", r.0, r.1, r.2, r.3)).collect();
        let report = format!(
            "seed {seed}\nhistory:\n{}\nexports (row ranges): {exports:?}\nexpected:\n  {}\ngot:\n  {}\n{}",
            history.join("\n"),
            want.join("\n  "),
            got.join("\n  "),
            findings.render(true)
        );
        smallest = Some((rows.len(), report));
    }
    smallest.map(|(_, report)| report)
}

#[test]
fn merging_whole_day_exports_is_like_merging_one() {
    if let Some(report) = check_merging(false, 500) {
        panic!("{report}");
    }
}

#[test]
fn merging_exports_taken_during_the_day_is_like_merging_one() {
    if let Some(report) = check_merging(true, 500) {
        panic!("{report}");
    }
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
    #[derive(serde::Deserialize)]
    struct InRules {
        amount: Amount,
    }
    let mut rng = Rng::new(4);
    for _ in 0..5000 {
        let digits = 1 + rng.below(13) as u32;
        let mantissa = (rng.next() % 10u64.pow(digits)) as i64;
        let sign = if rng.below(2) == 0 { 1 } else { -1 };
        let d = Decimal::new(sign * mantissa, rng.below(3) as u32);
        assert_eq!(parse_plain(&fmt_num(d)).unwrap(), d, "{d}");
        assert_eq!(parse_danish(&danish(d)).unwrap(), d, "{}", danish(d));
        for written in [fmt_num(d), format!("\"{}\"", fmt_num(d))] {
            let read: InRules = toml::from_str(&format!("amount = {written}")).unwrap();
            assert_eq!(read.amount.0, d, "amount = {written}");
        }
    }
}
