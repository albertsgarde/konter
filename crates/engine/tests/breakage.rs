//! Ways to make konter misbehave through its normal inputs (config, rules and
//! bank files as text), each the smallest case found. Each test asserts what
//! docs/spec.md says should happen. Those that still fail are ignored until
//! fixed: `cargo test -p konter-engine --test breakage -- --ignored`.

mod common;

use std::panic::AssertUnwindSafe;

use common::{NORDEA, build, config, nordea_csv, nordea_rel, posted, rules};
use jiff::civil::date;
use konter_engine::config::Config;
use konter_engine::findings::Severity;
use konter_engine::pipeline::Build;
use konter_engine::rules::Rules;
use konter_engine::transfers::{Leg, pair};
use rust_decimal::Decimal;

/// `konter unmatched` prints `[pin."<id>"]` for every unmatched transaction,
/// but pins (and later marks) only take IDs whose prefix is lowercase letters,
/// while sources.toml takes any `id_prefix`. A second account named `n2`, or
/// one called `nordea-main`, can never have a transaction pinned.
#[test]
#[ignore = "bug: sources.toml takes any id_prefix, but pins only take lowercase letters"]
fn every_id_konter_assigns_can_be_pinned() {
    for prefix in ["n2", "nordea-main"] {
        let text = NORDEA.replace("id_prefix = \"ba\"", &format!("id_prefix = {prefix:?}"));
        // Refusing the prefix up front would be fine too.
        let Ok(config) = Config::parse(&text, "config/sources.toml") else { continue };
        let csv = nordea_csv(&[("2026/01/05", "-50,00", "Groceries", "950,00")]);
        let rel = nordea_rel("2026-01-31 10.00.00");
        let build = build(&config, rules(&config, &[]), &[], &[("bank:a", &rel, &csv)], date(2026, 1, 31));
        let id = &build.loaded.sources["bank:a"].txs[0].id;
        let stub = format!("[pin.{id:?}]\naccount = \"expenses:groceries\"\n");
        if let Err(e) = Rules::load_str(&stub, "config/rules.toml", &config) {
            panic!("id_prefix {prefix:?} is accepted, but its IDs can't be pinned: {e:#}");
        }
    }
}

/// Transfer legs are paired greedily, closest first, so one leg can take the
/// only partner another leg has. Both legs are then reported as an error,
/// although every leg has a partner within the window.
#[test]
#[ignore = "bug: greedy pairing leaves legs unpaired that a full pairing would pair"]
fn transfers_pair_whenever_every_leg_has_a_partner() {
    let leg = |from: &str, to: &str, day: i8, amount: i64, id: &str| Leg {
        ledger_idx: 0,
        from: from.into(),
        to: to.into(),
        date: date(2026, 1, day),
        amount: Decimal::from(amount),
        id: id.into(),
    };
    // Two top-ups from lunar:b to bank:a. The first is sent on Friday the 9th
    // and arrives on Monday the 12th; the second is sent on the 13th and
    // arrives on the 15th. Both are well inside the default 5-day window.
    let legs = [
        leg("lunar:b", "bank:a", 9, -500, "sent-9th"),
        leg("bank:a", "lunar:b", 12, 500, "arrived-12th"),
        leg("lunar:b", "bank:a", 13, -500, "sent-13th"),
        leg("bank:a", "lunar:b", 15, 500, "arrived-15th"),
    ];
    let (pairs, unpaired) = pair(&legs, 5);
    let pairs: Vec<(&str, &str)> = pairs.iter().map(|&(i, j)| (legs[i].id.as_str(), legs[j].id.as_str())).collect();
    let unpaired: Vec<&str> = unpaired.iter().map(|&i| legs[i].id.as_str()).collect();
    // Today: the arrival on the 12th is paired with the money sent on the
    // 13th, after it arrived, and the other two are an "unpaired transfer" error.
    assert!(unpaired.is_empty(), "paired {pairs:?}, left unpaired {unpaired:?}");
}

const ENABLE_BANKING: &str = r#"
[books]
start = 2026-01-01

[sources."other:c"]
bank = "other"
number = "42"
ledger = "assets:bank:c"
id_prefix = "oc"
formats = ["enable-banking"]
unmatched = "allow"
"#;

/// Without running balances, rows are told apart by the bank's reference
/// alone: two rows with the same `entry_reference` are taken to be one, even
/// within one file and with different dates and amounts. When the amount and
/// date weren't compared, the second purchase disappeared, and the opening
/// balance, worked back from the bank's reported balance, quietly absorbed
/// it, so every check passed.
#[test]
fn a_reference_the_bank_repeats_doesnt_swallow_a_transaction() {
    let config = config(ENABLE_BANKING);
    let fetched = r#"{
      "acct_fetch": 1,
      "fetched_at": "2026-02-03T10:00:00Z",
      "pages": [{"transactions": [
        {"entry_reference": "R1", "status": "BOOK", "booking_date": "2026-01-05",
         "transaction_amount": {"currency": "DKK", "amount": "40.00"}, "credit_debit_indicator": "DBIT",
         "creditor": {"name": "Bakery"}},
        {"entry_reference": "R1", "status": "BOOK", "booking_date": "2026-01-25",
         "transaction_amount": {"currency": "DKK", "amount": "60.00"}, "credit_debit_indicator": "DBIT",
         "creditor": {"name": "Bakery"}}
      ]}],
      "balances": {"balances": [
        {"balance_type": "CLBD", "reference_date": "2026-02-02",
         "balance_amount": {"currency": "DKK", "amount": "400.00"}}
      ]}
    }"#;
    let files = [("other:c", "other/42/eb-2026-02-03T10-00-00.json", fetched)];
    let build = build(&config, rules(&config, &[]), &[], &files, date(2026, 2, 5));
    let data = &build.loaded.sources["other:c"];
    let kept: Vec<String> = data.txs.iter().map(|t| format!("{} {}", t.booked, t.amount)).collect();
    // Without an error, only the 40.00 would be kept, and the opening balance
    // would be 440 instead of 500.
    assert!(
        data.txs.len() == 2 || build.findings.has_errors(),
        "kept {kept:?}, opening balance {:?}, and no error:\n{}",
        data.opening,
        build.findings.render(true)
    );
}

/// Subscriptions due on the same day are written in the order they're loaded,
/// which is the order of the files they're in. Moving one to another file
/// rewrites the journals, although "the order of rules and the files they're
/// in never matter" (docs/spec.md §1).
#[test]
#[ignore = "bug: subscriptions due the same day are written in file order"]
fn which_file_a_subscription_is_in_doesnt_change_the_journals() {
    let config = config("[books]\nstart = 2026-01-01\n");
    let music = r#"
[[subscription]]
name = "music"
kind = "expense"
account = "expenses:music"
every = "1 month"
first = 2026-01-03
prices = [{ from = 2026-01-01, amount = 99 }]
"#;
    let video = music.replace("music", "video").replace("99", "79");
    let books = |a: &str, b: &str| {
        let rules = rules(&config, &[("config/rules/a.toml", a), ("config/rules/b.toml", b)]);
        build(&config, rules, &["expenses:music", "expenses:video"], &[], date(2026, 2, 15))
    };
    let (one, other) = (books(music, &video), books(&video, music));
    let lines = |text: &str| text.lines().map(str::to_string).collect::<Vec<_>>();
    assert_eq!(lines(&one.journal), lines(&other.journal));
    assert_eq!(lines(&one.forecast), lines(&other.forecast));
}

/// `before_start` says where transfers to an account go before its `start`
/// (docs/spec.md §3). It was only looked at when the account is imported; for
/// one without data the setting was accepted and silently ignored, and the
/// money booked as if the account had been yours all along.
#[test]
fn transfers_before_an_accounts_start_go_to_before_start_even_without_its_data() {
    let text = format!(
        "{NORDEA}\n[sources.\"bank:savings\"]\nbank = \"nordea\"\nledger = \"assets:bank:savings\"\n\
         start = 2026-02-01\nbefore_start = \"assets:receivable:friend\"\n"
    );
    // Refusing before_start for an account that isn't imported would be fine too.
    let Ok(config) = Config::parse(&text, "config/sources.toml") else { return };
    let rule = "[[rule]]\nname = \"savings\"\nwhen = { description = \"Savings\" }\ntransfer = \"bank:savings\"\n";
    let csv = nordea_csv(&[("2026/01/05", "-1.000,00", "Savings", "9.000,00")]);
    let rel = nordea_rel("2026-01-31 10.00.00");
    let build = build(
        &config,
        rules(&config, &[("config/rules.toml", rule)]),
        &["assets:receivable:friend"],
        &[("bank:a", &rel, &csv)],
        date(2026, 1, 31),
    );
    let tx = build.books.iter().find(|tx| tx.date == date(2026, 1, 5)).unwrap();
    let accounts: Vec<&str> = tx.postings.iter().map(|p| p.account.as_str()).collect();
    assert_eq!(accounts, ["assets:bank:a", "assets:receivable:friend"]);
}

/// A variable subscription settles one due date per payment, so "refunds go
/// straight to the expense account" (docs/spec.md §8). Nothing used to check
/// that. A rule that matches the company's name also matches its yearly
/// settlement refund, which then settles a due date as a negative bill and
/// pushes every later payment one period on.
#[test]
fn a_refund_paid_to_a_variable_subscription_isnt_taken_for_a_bill() {
    let config = config(NORDEA);
    let subscription = r#"
[[subscription]]
name = "power"
kind = "expense"
account = "expenses:power"
every = "1 month"
first = 2026-01-01
variable = true
prices = [{ from = 2026-01-01, amount = 500 }]

[[rule]]
name = "power"
when = { description = "Power Co" }
subscription = "power"
"#;
    // Three monthly bills of 500, and a refund of 300 after the yearly settlement.
    let csv = nordea_csv(&[
        ("2026/03/03", "-500,00", "Power Co", "4.800,00"),
        ("2026/02/10", "300,00", "Power Co", "5.300,00"),
        ("2026/02/03", "-500,00", "Power Co", "5.000,00"),
        ("2026/01/03", "-500,00", "Power Co", "5.500,00"),
    ]);
    let rel = nordea_rel("2026-03-15 10.00.00");
    let as_of = date(2026, 3, 15);
    let build = build(
        &config,
        rules(&config, &[("config/rules.toml", subscription)]),
        &["expenses:power"],
        &[("bank:a", &rel, &csv)],
        as_of,
    );
    let spent = posted(&build, "expenses:power", as_of);
    let mentioned = |build: &Build| {
        build.findings.diags.iter().any(|d| {
            d.severity <= Severity::Warning
                && (d.message.contains("power") || d.details.iter().any(|l| l.contains("power")))
        })
    };
    // Mapped like this, the books say 700 (500 + 500 - 300; the March bill
    // counts as April's). Either the books say 1200 or the build says so.
    assert!(
        spent == Decimal::from(1200) || mentioned(&build),
        "electricity up to {as_of} is booked as {spent}, not 1200, without a word:\n{}",
        build.findings.render(true)
    );
}

/// Rows that look alike (same day, amount and running balance) are told
/// apart by counting them. Counted in file order, which is newest first, an
/// export taken mid-day counts the day's rows differently from a later export
/// of the whole day, so a later row was matched with an earlier one: an
/// "overlap conflict" error that nothing can fix (raw files are never
/// edited), and one transaction took the other's description.
#[test]
fn a_mid_day_export_and_a_later_one_agree() {
    let config = config(NORDEA);
    // Alice sends 100 for a club dinner, it's paid on, then Carol sends 100.
    let full = nordea_csv(&[
        ("2026/01/14", "-20,00", "Coffee", "180,00"),
        ("2026/01/12", "100,00", "MobilePay Carol", "200,00"),
        ("2026/01/12", "-100,00", "Club dinner", "100,00"),
        ("2026/01/12", "100,00", "MobilePay Alice", "200,00"),
        ("2026/01/05", "-50,00", "Groceries", "100,00"),
    ]);
    // Exported at 10:00 on the 12th, before Carol's payment was booked.
    let morning = nordea_csv(&[
        ("2026/01/12", "-100,00", "Club dinner", "100,00"),
        ("2026/01/12", "100,00", "MobilePay Alice", "200,00"),
        ("2026/01/05", "-50,00", "Groceries", "100,00"),
    ]);
    let (morning_rel, full_rel) = (nordea_rel("2026-01-12 10.00.00"), nordea_rel("2026-01-15 09.00.00"));
    let as_of = date(2026, 1, 20);
    let both = build(
        &config,
        rules(&config, &[]),
        &[],
        &[("bank:a", &morning_rel, &morning), ("bank:a", &full_rel, &full)],
        as_of,
    );
    let alone = build(&config, rules(&config, &[]), &[], &[("bank:a", &full_rel, &full)], as_of);
    let rows = |build: &Build| -> Vec<(String, String)> {
        build.loaded.sources["bank:a"].txs.iter().map(|t| (t.id.clone(), t.description.clone())).collect()
    };
    assert_eq!(rows(&both), rows(&alone));
    assert!(!both.findings.has_errors(), "{}", both.findings.render(true));
}

/// Found by `merging_exports_taken_during_the_day_is_like_merging_one` in
/// properties.rs. Lunar's same-minute rows "aren't reliably in order; the
/// merge step orders them by running balance" (lunar_csv.rs). It can't on the
/// first day, which has no balance before it to start from: if that day's
/// rows add up to nothing (a charge and its refund), either order chains, and
/// file order used to pick one without looking at the next day. The wrong one
/// gives a wrong opening balance and errors about a missing transaction, an
/// ignored one and a failed assertion. Two Nordea exports, one taken between
/// the charge and the refund, did the same.
#[test]
fn the_first_days_order_follows_the_running_balance() {
    let config = config(
        "[books]\nstart = 2026-01-01\n\n[sources.\"lunar:b\"]\nbank = \"lunar\"\nnumber = \"555\"\n\
         ledger = \"assets:bank:b\"\nid_prefix = \"lb\"\nformats = [\"lunar-csv\"]\nunmatched = \"allow\"\n",
    );
    // From 1000: a charge of 50 and its refund in the same minute, then a coffee.
    let statement = "Date,Time,Title,Amount,Balance,Transaction ID\n\
        05.01.2026,09.00,Coffee,\"-10,00\",\"990,00\",r3\n\
        04.01.2026,12.00,Charge,\"-50,00\",\"950,00\",r1\n\
        04.01.2026,12.00,Refund,\"50,00\",\"1000,00\",r2\n";
    let files = [("lunar:b", "lunar/555/statement.csv", statement)];
    let build = build(&config, rules(&config, &[]), &[], &files, date(2026, 1, 10));
    assert_eq!(build.loaded.sources["lunar:b"].opening, Some(Decimal::from(1000)));
    assert!(!build.findings.has_errors(), "{}", build.findings.render(true));
}

/// Amounts past what a Decimal holds, or schedules past year 9999, make the
/// build panic instead of reporting what's wrong.
#[test]
#[ignore = "bug: out-of-range amounts and dates panic"]
fn out_of_range_inputs_are_reported_not_panics() {
    let config = &config(NORDEA);
    let csv = nordea_csv(&[("2026/01/05", "-50,00", "Groceries", "950,00")]);
    let rel = nordea_rel("2026-01-31 10.00.00");
    let cases = [
        (
            "a split beyond Decimal's range",
            r#"
[[rule]]
name = "groceries"
when = { description = "Groceries" }
split = [
  { account = "expenses:groceries", amount = "70000000000000000000000000000" },
  { account = "expenses:groceries", amount = "70000000000000000000000000000" },
  { account = "expenses:other", rest = true },
]
"#,
        ),
        (
            "a subscription whose second due date is after year 9999",
            r#"
[[subscription]]
name = "forever"
kind = "expense"
account = "expenses:other"
every = "8000 years"
first = 2026-01-01
prices = [{ from = 2026-01-01, amount = 1 }]
"#,
        ),
    ];
    let mut panicked = Vec::new();
    for (what, text) in cases {
        // Refusing the rules would be fine too.
        let Ok(rules) = Rules::load_str(text, "config/rules.toml", config) else { continue };
        let accounts = ["expenses:groceries", "expenses:other"];
        let files = [("bank:a", rel.as_str(), csv.as_str())];
        let result =
            std::panic::catch_unwind(AssertUnwindSafe(|| build(config, rules, &accounts, &files, date(2026, 2, 1))));
        if result.is_err() {
            panicked.push(what);
        }
    }
    assert!(panicked.is_empty(), "the build panicked on {panicked:?}");
}

// ── History the bank will forget ───────────────────────────────────────
//
// What a bank no longer has can't be had again ("history lost from Nordea
// can't be fetched again", docs/spec.md §4), so this is the one loss that's
// permanent. The build warns before it happens, and once only did so about
// where each imported source's data *ends*.

/// The non-Ok findings that mention `what`, as text.
fn findings_about(build: &Build, what: &str) -> Vec<String> {
    build
        .findings
        .diags
        .iter()
        .filter(|d| d.severity != Severity::Ok)
        .map(|d| format!("{} {}", d.message, d.details.join(" ")))
        .filter(|text| text.contains(what))
        .collect()
}

/// An account at a bank with `history_days` that isn't imported yet (no
/// `formats`) once got no warning at all, however much of its history the
/// bank had already forgotten.
#[test]
fn history_of_an_account_not_imported_yet_is_warned_about_before_its_gone() {
    let text = format!(
        "{NORDEA}\n[banks.nordea]\nhistory_days = 540\n\n\
         [sources.\"bank:savings\"]\nbank = \"nordea\"\nledger = \"assets:bank:savings\"\n"
    );
    let config = config(&text);
    let csv = nordea_csv(&[("2027/06/18", "-50,00", "Groceries", "950,00")]);
    let rel = nordea_rel("2027-06-19 10.00.00");
    // The books start on 2026-01-01, so bank:savings' history from then on
    // starts to go 540 days later.
    let lost = config.start().saturating_add(jiff::Span::new().days(540));
    let as_of = date(2027, 6, 20);
    assert!(as_of < lost);
    let build = build(&config, rules(&config, &[]), &[], &[("bank:a", &rel, &csv)], as_of);
    let about = findings_about(&build, "bank:savings");
    assert!(about.iter().any(|f| f.contains(&lost.to_string())), "nothing says {lost} for bank:savings: {about:?}");
}

/// An imported source whose data starts after the books do (the first fetch
/// didn't reach back that far, say) was once only warned about where its data
/// ends. The deadline given was later than the day the missing beginning
/// starts to go, and the one remark about the beginning said to move `start`,
/// which hides the hole rather than filling it.
#[test]
fn history_missing_from_the_start_is_warned_about_before_its_gone() {
    let text = NORDEA.replace("start = 2026-01-01", "start = 2025-12-01") + "\n[banks.nordea]\nhistory_days = 540\n";
    let config = config(&text);
    let csv =
        nordea_csv(&[("2026/01/31", "-25,00", "Coffee", "925,00"), ("2026/01/02", "-50,00", "Groceries", "950,00")]);
    let rel = nordea_rel("2026-01-31 10.00.00");
    // December 2025 was never downloaded; it starts to go on this day.
    let lost = config.start().saturating_add(jiff::Span::new().days(540));
    let as_of = date(2027, 5, 20);
    assert!(as_of < lost);
    let build = build(&config, rules(&config, &[]), &[], &[("bank:a", &rel, &csv)], as_of);
    let about = findings_about(&build, "bank:a");
    assert!(about.iter().any(|f| f.contains(&lost.to_string())), "nothing says {lost} for bank:a: {about:#?}");
}
