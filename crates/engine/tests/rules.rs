//! Where the money goes: which transactions each condition matches, what
//! each output books, and which rule files are refused (docs/spec.md §5, §6).
//! The property tests check that the books add up whatever the rules are;
//! these check that a rule does what it says.

mod common;

use common::{NORDEA, build, config, nordea_csv, nordea_rel};
use jiff::civil::date;
use konter_engine::config::Config;
use konter_engine::engine::Status;
use konter_engine::findings::Severity;
use konter_engine::pipeline::Build;
use konter_engine::rules::Rules;
use rust_decimal::Decimal;

const ACCOUNTS: [&str; 4] = ["expenses:shop", "expenses:other", "assets:prepaid", "assets:receivable:friend"];

/// Builds the books of `bank:a` from `rows` (newest first) with `rules`.
fn books(config: &Config, rules: &str, rows: &[(&str, &str, &str, &str)]) -> Build {
    let rules = Rules::load_str(rules, "config/rules.toml", config).expect("the test's rules load");
    let rel = nordea_rel("2026-01-31 10.00.00");
    build(config, rules, &ACCOUNTS, &[("bank:a", &rel, &nordea_csv(rows))], date(2026, 1, 31))
}

/// The findings of `severity` about `category`, each with its details.
fn findings(build: &Build, severity: Severity, category: &str) -> Vec<String> {
    let found = build.findings.diags.iter().filter(|d| d.severity == severity && d.category == category);
    found.map(|d| format!("{} {:?}", d.message, d.details)).collect()
}

/// What the transaction on `day` of January was booked as: its status, and
/// its postings as (account, amount), in every ledger transaction made of it.
fn booked(build: &Build, day: i8) -> (Status, Vec<(String, Decimal)>) {
    let t = build.loaded.sources["bank:a"]
        .txs
        .iter()
        .find(|t| t.booked == date(2026, 1, day))
        .expect("a bank transaction on that day");
    let outcome = &build.generated.outcomes[&t.id];
    let postings = outcome
        .ledger
        .iter()
        .flat_map(|&i| &build.generated.txs[i].postings)
        .map(|p| (p.account.clone(), p.amount))
        .collect();
    (outcome.status.clone(), postings)
}

fn posting(account: &str, amount: i64) -> (String, Decimal) {
    (account.to_string(), Decimal::from(amount))
}

/// Five purchases and payments at one shop: out, out, in, out, and one of
/// nothing at all.
const SHOP: [(&str, &str, &str, &str); 5] = [
    ("2026/01/25", "0,00", "Shop", "800,00"),
    ("2026/01/20", "-150,00", "Shop", "800,00"),
    ("2026/01/15", "100,00", "Shop", "950,00"),
    ("2026/01/10", "-100,00", "Shop", "850,00"),
    ("2026/01/05", "-50,00", "Shop", "950,00"),
];

#[test]
fn each_condition_matches_what_it_says() {
    let config = config(NORDEA);
    for (when, days) in [
        ("description = \"Shop\"", vec![5, 10, 15, 20, 25]),
        ("amount = -100", vec![10]),
        ("amount = 100", vec![15]),
        // Both ends of a range are in it.
        ("amount_range = [-100, -50]", vec![5, 10]),
        ("amount_range = [-99, 100]", vec![5, 15, 25]),
        ("date = 2026-01-10", vec![10]),
        // And so are the days a period starts and ends on.
        ("date_from = 2026-01-10", vec![10, 15, 20, 25]),
        ("date_to = 2026-01-10", vec![5, 10]),
        ("date_from = 2026-01-10, date_to = 2026-01-20", vec![10, 15, 20]),
        // Nothing is neither in nor out.
        ("direction = \"out\"", vec![5, 10, 20]),
        ("direction = \"in\"", vec![15]),
        ("not = { direction = \"out\" }", vec![15, 25]),
        ("any = [{ amount = -50 }, { date_from = 2026-01-20 }]", vec![5, 20, 25]),
    ] {
        let rule = format!("[[rule]]\nname = \"shop\"\nwhen = {{ {when} }}\naccount = \"expenses:shop\"\n");
        let build = books(&config, &rule, &SHOP);
        let matched: Vec<i8> = [5, 10, 15, 20, 25]
            .into_iter()
            .filter(|&day| booked(&build, day).0 == Status::Rule("shop".to_string()))
            .collect();
        assert_eq!(matched, days, "when = {{ {when} }}");
    }
}

#[test]
fn outputs_book_what_they_say() {
    let config = config(NORDEA);
    let rules = r#"
[[rule]]
name = "forgotten"
when = { date_to = 2026-01-15 }
unknown = "nobody remembers"

[[rule]]
name = "prepaid"
when = { date = 2026-01-20 }
transactions = [
  { postings = [
      { account = "@bank" },
      { account = "assets:prepaid", amount = "-@amount" } ] },
  { date = 2026-02-01, postings = [
      { account = "assets:prepaid", amount = "@amount" },
      { account = "expenses:shop", amount = "40" },
      { account = "expenses:other", rest = true } ] },
]
"#;
    let build = books(&config, rules, &SHOP);
    assert!(!build.findings.has_errors(), "{}", build.findings.render(true));

    // What won't be mapped is an expense when money went out, income when it came in.
    assert_eq!(booked(&build, 10).1, [posting("assets:bank:a", -100), posting("expenses:unknown", 100)]);
    assert_eq!(booked(&build, 15).1, [posting("assets:bank:a", 100), posting("income:unknown", -100)]);

    // `-@amount` is the bank's amount the other way, `@amount` the bank's
    // amount, and an amount in quotes is that amount.
    let (status, postings) = booked(&build, 20);
    assert_eq!(status, Status::Rule("prepaid".to_string()));
    assert_eq!(
        postings,
        [
            posting("assets:bank:a", -150),
            posting("assets:prepaid", 150),
            posting("assets:prepaid", -150),
            posting("expenses:shop", 40),
            posting("expenses:other", 110),
        ]
    );

    // What no rule matches is an expense, unless money came in. Nothing at
    // all is an expense of nothing, with or without a reason.
    let build = books(&config, "", &SHOP);
    assert_eq!(booked(&build, 10).1, [posting("assets:bank:a", -100), posting("expenses:unmatched:bank:a", 100)]);
    assert_eq!(booked(&build, 15).1, [posting("assets:bank:a", 100), posting("income:unmatched:bank:a", -100)]);
    assert_eq!(booked(&build, 25).1, [posting("assets:bank:a", 0), posting("expenses:unmatched:bank:a", 0)]);
    let rule = "[[rule]]\nname = \"nothing\"\nwhen = { amount = 0 }\nunknown = \"a receipt for nothing\"\n";
    let build = books(&config, rule, &SHOP);
    assert_eq!(booked(&build, 25).1, [posting("assets:bank:a", 0), posting("expenses:unknown", 0)]);
}

/// `transactions` without a `rest` must add up for the transaction they're
/// applied to. When they don't, the rule is invalid for it, and it's booked
/// as unmatched.
#[test]
fn transactions_that_dont_add_up_are_invalid_for_that_transaction() {
    let config = config(NORDEA);
    let rules = r#"
[[rule]]
name = "exactly-fifty"
when = { description = "Shop", direction = "out", date_to = 2026-01-10 }
transactions = [
  { postings = [{ account = "@bank" }, { account = "expenses:shop", amount = 50 }] },
]
"#;
    let build = books(&config, rules, &SHOP);
    assert_eq!(
        booked(&build, 5),
        (Status::Rule("exactly-fifty".to_string()), vec![posting("assets:bank:a", -50), posting("expenses:shop", 50)])
    );
    let (status, postings) = booked(&build, 10);
    let Status::Invalid { rule, error } = status else { panic!("the rule fits 100 as well as 50: {status:?}") };
    assert_eq!(rule, "exactly-fifty");
    assert!(error.contains("doesn't balance (off by -50.00)"), "{error}");
    assert_eq!(postings, [posting("assets:bank:a", -100), posting("expenses:unmatched:bank:a", 100)]);
    assert_eq!(findings(&build, Severity::Error, "invalid rule").len(), 1);
}

#[test]
fn rules_written_wrong_dont_load() {
    let config = config(NORDEA);
    let rule = |more: &str| format!("[[rule]]\nname = \"shop\"\nwhen = {{ description = \"Shop\" }}\n{more}\n");
    let transactions = |postings: &str| rule(&format!("transactions = [{{ postings = [{postings}] }}]"));
    for (rules, expected) in [
        (
            "[[rule]]\nname = \"shop\"\nwhen = { date = 2026-01-10, date_from = 2026-01-01 }\naccount = \"expenses:shop\"\n"
                .to_string(),
            "date",
        ),
        (
            "[[rule]]\nname = \"shop\"\nwhen = { date = 2026-01-10, date_to = 2026-01-31 }\naccount = \"expenses:shop\"\n"
                .to_string(),
            "date",
        ),
        (rule("account = \"expenses:shop\"\ntags = { \"two words\" = \"x\" }"), "invalid tag"),
        (rule("account = \"expenses:shop\"\ntags = { \"a:b\" = \"x\" }"), "invalid tag"),
        (rule("account = \"expenses:shop\"\ntags = { \"\" = \"x\" }"), "invalid tag"),
        (rule("account = \"expenses:shop\"\ntags = { trip = \"here, there\" }"), "invalid tag"),
        (rule("account = \"expenses:shop\"\ntags = { trip = \"here; there\" }"), "invalid tag"),
        (transactions("{ account = \"@bank\" }, { account = \"expenses:shop\" }"), "needs an amount or rest = true"),
        (transactions("{ account = \"@bank\" }"), "at least two postings"),
        (
            transactions(
                "{ account = \"@bank\" }, { account = \"expenses:shop\", rest = true }, \
                 { account = \"expenses:other\", rest = true }",
            ),
            "at most one rest = true",
        ),
        (
            "[trip.backwards]\naccount = \"expenses:shop\"\nfrom = 2026-01-10\nto = 2026-01-09\n".to_string(),
            "`from` (2026-01-10) is after `to` (2026-01-09)",
        ),
    ] {
        let Err(error) = Rules::load_str(&rules, "config/rules.toml", &config) else {
            panic!("these rules load:\n{rules}");
        };
        let error = format!("{error:#}");
        assert!(error.contains(expected), "{error}\nfor:\n{rules}");
    }

    // And these are fine: a tag, three postings with one rest, a trip of one day.
    for rules in [
        rule("account = \"expenses:shop\"\ntags = { trip = \"lisbon\" }"),
        transactions(
            "{ account = \"@bank\" }, { account = \"expenses:shop\", amount = 40 }, \
             { account = \"expenses:other\", rest = true }",
        ),
        "[trip.day-out]\naccount = \"expenses:shop\"\nfrom = 2026-01-10\nto = 2026-01-10\n".to_string(),
    ] {
        if let Err(error) = Rules::load_str(&rules, "config/rules.toml", &config) {
            panic!("{error:#}\nfor:\n{rules}");
        }
    }
}

/// "Transfers to or from it before then go to `before_start`": before, and
/// not on, the day the account became yours.
#[test]
fn a_transfer_on_the_day_an_account_starts_goes_to_the_account() {
    let text = format!(
        "{NORDEA}\n[sources.\"bank:savings\"]\nbank = \"nordea\"\nledger = \"assets:bank:savings\"\n\
         start = 2026-01-10\nbefore_start = \"assets:receivable:friend\"\n"
    );
    let config = config(&text);
    let rule = "[[rule]]\nname = \"savings\"\nwhen = { direction = \"out\" }\ntransfer = \"bank:savings\"\n";
    let build = books(&config, rule, &SHOP);
    assert_eq!(booked(&build, 5).1, [posting("assets:bank:a", -50), posting("assets:receivable:friend", 50)]);
    assert_eq!(booked(&build, 10).1, [posting("assets:bank:a", -100), posting("assets:bank:savings", 100)]);
}

/// "An unpaired side is an error inside both accounts' covered dates, and a
/// warning at the edges": the other side may yet come, or have been before
/// the other account's data began, when either is within the window.
#[test]
fn an_unpaired_side_is_a_warning_within_the_window_of_either_end_of_the_other_accounts_data() {
    let text = format!(
        "{NORDEA}\n[sources.\"bank:b\"]\nbank = \"nordea\"\nnumber = \"2222222222\"\nledger = \"assets:bank:b\"\n\
         id_prefix = \"bb\"\nformats = [\"nordea-csv\"]\nunmatched = \"allow\"\n"
    );
    let config = config(&text);
    let rule =
        "[[rule]]\nname = \"to-b\"\nwhen = { source = \"bank:a\", description = \"To B\" }\ntransfer = \"bank:b\"\n";
    let rules = || Rules::load_str(rule, "config/rules.toml", &config).expect("the test's rules load");
    // bank:b has data from the 10th to the 20th, and no other side for any of these.
    let b = nordea_csv(&[("2026/01/20", "-1,00", "Fee", "98,00"), ("2026/01/10", "-1,00", "Fee", "99,00")]);
    let b_rel = common::nordea_rel_of("2222222222", "2026-01-31 10.00.00");
    for (day, inside) in [(9, false), (12, false), (15, true), (18, false), (21, false)] {
        let a = nordea_csv(&[(&format!("2026/01/{day:02}"), "-100,00", "To B", "900,00")]);
        let files = [("bank:a", nordea_rel("2026-01-31 10.00.00"), a), ("bank:b", b_rel.clone(), b.clone())];
        let files: Vec<(&str, &str, &str)> =
            files.iter().map(|(s, rel, text)| (*s, rel.as_str(), text.as_str())).collect();
        let build = build(&config, rules(), &ACCOUNTS, &files, date(2026, 1, 31));
        let errors = findings(&build, Severity::Error, "unpaired transfer").len();
        let warnings = findings(&build, Severity::Warning, "unpaired transfer").len();
        assert_eq!((errors, warnings), if inside { (1, 0) } else { (0, 1) }, "a transfer on day {day}");
    }
}

/// The opening balance is booked at the books' start. Data that begins more
/// than a week after it is warned about: the account may be newer than that.
#[test]
fn an_opening_balance_more_than_a_week_before_the_data_is_warned_about() {
    let config = config(NORDEA);
    for (first, warned) in [("2026/01/08", false), ("2026/01/09", true)] {
        let build = books(&config, "", &[(first, "-50,00", "Shop", "950,00")]);
        assert_eq!(findings(&build, Severity::Warning, "opening balance").len(), usize::from(warned), "{first}");
    }
}

#[test]
fn the_mapping_is_summed_up_for_each_account_with_transactions() {
    let config = config(NORDEA);
    let everything = "[[rule]]\nname = \"shop\"\nwhen = { description = \"Shop\" }\naccount = \"expenses:shop\"\n";
    let build = books(&config, everything, &SHOP);
    assert_eq!(findings(&build, Severity::Ok, "mapping"), ["bank:a: all 5 transactions mapped []"]);

    // One left for later: said, and still fine.
    let most = "[[rule]]\nname = \"shop\"\nwhen = { description = \"Shop\", date_from = 2026-01-10 }\n\
                account = \"expenses:shop\"\n";
    let id = &books(&config, most, &SHOP).loaded.sources["bank:a"].txs[0].id;
    let build = books(&config, &format!("{most}\n[later.{id:?}]\n"), &SHOP);
    assert_eq!(
        findings(&build, Severity::Ok, "mapping"),
        ["bank:a: all 5 transactions mapped or marked later (1 later) []"]
    );

    // An account without transactions has no mapping to sum up.
    let rel = nordea_rel("2026-01-31 10.00.00");
    let rules = Rules::load_str("", "config/rules.toml", &config).expect("no rules load");
    let build = common::build(&config, rules, &ACCOUNTS, &[("bank:a", &rel, &nordea_csv(&[]))], date(2026, 1, 31));
    assert_eq!(findings(&build, Severity::Ok, "mapping"), [""; 0]);
}

/// A pin or a later mark for no transaction is dead, unless it's for a
/// transaction from before its account's start: those are in the store, and
/// left out of the books on purpose.
#[test]
fn a_pin_for_a_transaction_before_its_accounts_start_isnt_dead() {
    let config = config(NORDEA);
    // The export reaches back to the day before the books start.
    let mut rows = SHOP.to_vec();
    rows.push(("2025/12/31", "-10,00", "Shop", "1000,00"));
    let early = common::id(1).to_string();
    let none = "00000000-0000-4000-8000-ffffffffffff";
    let pin = |id: &str| format!("[pin.{id:?}]\naccount = \"expenses:shop\"\n");
    let rules = [pin(&early), pin(none), format!("[later.{:?}]\n", "00000000-0000-4000-8000-eeeeeeeeeeee")].concat();
    let build = books(&config, &rules, &rows);
    assert!(build.loaded.is_before_start(&early) && build.loaded.find(&early).is_none());
    let dead = findings(&build, Severity::Warning, "dead rule").concat();
    assert!(dead.starts_with("2 rules match nothing"), "{dead}");
    assert!(!dead.contains(&early), "{dead}");
    assert!(dead.contains(none) && dead.contains("00000000-0000-4000-8000-eeeeeeeeeeee"), "{dead}");
}
