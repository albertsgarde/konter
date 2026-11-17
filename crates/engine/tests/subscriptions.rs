//! Where a subscription stands and what the build says about it, at the
//! edges: the day something starts, ends or falls due (docs/spec.md §8).

mod common;

use common::{NORDEA, build, config, danish, nordea_csv, nordea_rel};
use jiff::civil::{Date, date};
use konter_engine::findings::Severity;
use konter_engine::pipeline::Build;
use konter_engine::rules::Rules;
use rust_decimal::Decimal;

/// A gym at 100 a month, due on the 1st, from the books' first day, with
/// `more` lines added, and a rule that maps "Gym" to it.
fn gym(more: &str) -> String {
    format!(
        "[[subscription]]\nname = \"gym\"\nkind = \"expense\"\naccount = \"expenses:gym\"\nevery = \"1 month\"\n\
         first = 2026-01-01\nprices = [{{ from = 2026-01-01, amount = 100 }}]\n{more}\n\n\
         [[rule]]\nname = \"gym\"\nwhen = {{ description = \"Gym\" }}\nsubscription = \"gym\"\n"
    )
}

/// Builds the books as of `as_of` with `rules` and one "Gym" bank transaction
/// for each of `payments`: (day in January, amount paid; negative = refunded).
fn books(rules: &str, payments: &[(i8, i64)], as_of: Date) -> Build {
    let config = config(NORDEA);
    let rules = Rules::load_str(rules, "config/rules.toml", &config).expect("the test's rules load");
    let mut balance = Decimal::from(10_000);
    let mut rows: Vec<(String, String, String)> = Vec::new();
    for (day, paid) in payments {
        balance -= Decimal::from(*paid);
        rows.push((format!("2026/01/{day:02}"), danish(Decimal::from(-paid)), danish(balance)));
    }
    // Newest first, as the bank writes them.
    let rows: Vec<(&str, &str, &str, &str)> = rows
        .iter()
        .rev()
        .map(|(day, amount, balance)| (day.as_str(), amount.as_str(), "Gym", balance.as_str()))
        .collect();
    let rel = nordea_rel("2026-01-31 10.00.00");
    build(&config, rules, &["expenses:gym"], &[("bank:a", &rel, &nordea_csv(&rows))], as_of)
}

/// The warnings about `category`.
fn warnings(build: &Build, category: &str) -> Vec<String> {
    let found = build.findings.diags.iter().filter(|d| d.severity == Severity::Warning && d.category == category);
    found.map(|d| d.message.clone()).collect()
}

fn end_of_january() -> Date {
    date(2026, 1, 31)
}

#[test]
fn a_due_date_on_the_books_first_day_is_one_to_pay() {
    let build = books(&gym(""), &[(5, 100)], end_of_january());
    let state = &build.subscriptions[0];
    assert_eq!(state.paid_until, Some(date(2026, 1, 1)));
    assert_eq!((state.credit, state.oldest_unpaid), (Decimal::ZERO, None));
    assert_eq!(state.next_due, Some(date(2026, 2, 1)));
}

/// A due date is in deficit once it's been unpaid for more than the grace
/// period, a week unless the subscription says otherwise.
#[test]
fn a_deficit_is_warned_about_after_the_grace_period() {
    for (as_of, late) in [(date(2026, 1, 8), false), (date(2026, 1, 9), true)] {
        let build = books(&gym(""), &[], as_of);
        assert_eq!(build.subscriptions[0].oldest_unpaid, Some(date(2026, 1, 1)));
        let deficits = warnings(&build, "deficit");
        assert_eq!(deficits.len(), usize::from(late), "as of {as_of}: {deficits:?}");
        if late {
            assert!(deficits[0].starts_with("gym: 100.00 DKK owed, unpaid since 2026-01-01"), "{deficits:?}");
        }
    }
}

/// One period paid ahead is paying early. More than one looks like paying twice.
#[test]
fn more_than_one_period_paid_ahead_is_warned_about() {
    for (paid, twice) in [(200, false), (300, true)] {
        let build = books(&gym(""), &[(5, paid)], end_of_january());
        assert_eq!(build.subscriptions[0].credit, Decimal::from(paid - 100));
        let ahead = warnings(&build, "subscription");
        assert_eq!(ahead.len(), usize::from(twice), "{paid} paid: {ahead:?}");
        if twice {
            assert_eq!(ahead[0], "gym: paid 200.00 ahead, more than one period; paid twice?");
        }
    }
}

/// A variable amount can't be compared to the price, so there it's the due
/// dates after `--as-of` that payments settle which are counted. One settled
/// on the day itself isn't ahead.
#[test]
fn more_than_one_due_date_settled_ahead_is_warned_about() {
    let variable = gym("variable = true");
    for (payments, as_of, twice) in [
        (&[(5, 90), (6, 110)][..], end_of_january(), false),
        (&[(5, 90), (6, 110), (7, 95)][..], end_of_january(), true),
        (&[(5, 90), (6, 110), (7, 95)][..], date(2026, 2, 1), false),
    ] {
        let build = books(&variable, payments, as_of);
        let ahead = warnings(&build, "subscription");
        assert_eq!(ahead.len(), usize::from(twice), "{payments:?} as of {as_of}: {ahead:?}");
    }
}

/// Payments settle a variable subscription's due dates up to its last one,
/// the day it ends included.
#[test]
fn a_variable_subscription_is_settled_up_to_the_day_it_ends() {
    let build =
        books(&gym("variable = true\nend = 2026-03-01"), &[(5, 90), (6, 110), (7, 95), (8, 105)], end_of_january());
    assert_eq!(build.subscriptions[0].paid_until, Some(date(2026, 3, 1)));
}

/// "Refunds go straight to the expense account": one mapped to a variable
/// subscription is warned about, with where it should go. A payment of
/// nothing isn't a refund.
#[test]
fn a_refund_to_a_variable_subscription_is_warned_about() {
    let build = books(&gym("variable = true"), &[(5, 100), (6, 0), (7, -30)], end_of_january());
    // (The three of them settle three due dates, which is warned about too.)
    let mut refunds = warnings(&build, "subscription");
    refunds.retain(|warning| warning.contains("refund"));
    assert_eq!(refunds.len(), 1, "{refunds:?}");
    assert!(refunds[0].starts_with("gym: 30.00 on 2026-01-07 is a refund"), "{refunds:?}");
    assert!(refunds[0].contains("map it straight to expenses:gym (config/rules.toml)"), "{refunds:?}");
}

/// What's paid beyond a subscription's end is warned about once it has ended.
#[test]
fn money_paid_beyond_the_end_is_warned_about_after_it() {
    let ended = gym("end = 2026-01-15");
    for (paid, as_of, beyond) in [
        (200, end_of_january(), true),
        // Not on its last day,
        (200, date(2026, 1, 15), false),
        // and not when everything paid was owed.
        (100, end_of_january(), false),
    ] {
        let build = books(&ended, &[(5, paid)], as_of);
        let said = warnings(&build, "subscription");
        assert_eq!(said.len(), usize::from(beyond), "{paid} paid, as of {as_of}: {said:?}");
        if beyond {
            assert_eq!(said[0], "gym: ended 2026-01-15, but 100.00 was paid beyond that");
        }
    }
}

/// With a notice period, you're committed until it's over, or until the end
/// if that comes first. On the last day there's nothing left to be committed to.
#[test]
fn the_notice_period_commits_until_it_is_over_or_the_subscription_ends() {
    for (more, committed_until) in [
        ("notice = \"1 month\"", Some(date(2026, 2, 28))),
        ("notice = \"1 month\"\nend = 2026-02-10", Some(date(2026, 2, 10))),
        ("notice = \"1 month\"\nend = 2026-01-31", None),
        ("", None),
    ] {
        let build = books(&gym(more), &[(5, 100)], end_of_january());
        assert_eq!(build.subscriptions[0].committed_until, committed_until, "{more}");
    }
}

#[test]
fn a_subscription_can_end_on_its_first_due_date_but_not_before() {
    let config = config(NORDEA);
    Rules::load_str(&gym("end = 2026-01-01"), "config/rules.toml", &config).expect("one due date is enough");
    let Err(error) = Rules::load_str(&gym("end = 2025-12-31"), "config/rules.toml", &config) else {
        panic!("a subscription that ends before it starts loads");
    };
    let error = format!("{error:#}");
    assert!(error.contains("subscription \"gym\"") && error.contains("end is before first"), "{error}");
}

#[test]
fn a_subscription_knows_its_kind_and_its_account() {
    let config =
        config(&format!("{NORDEA}\n[sources.\"bank:savings\"]\nbank = \"nordea\"\nledger = \"assets:bank:savings\"\n"));
    let schedule = "every = \"1 month\"\nfirst = 2026-01-01\nprices = [{ from = 2026-01-01, amount = 100 }]\n";
    let rules = format!(
        "[[subscription]]\nname = \"gym\"\nkind = \"expense\"\naccount = \"expenses:gym\"\n{schedule}\n\
         [[subscription]]\nname = \"salary\"\nkind = \"income\"\naccount = \"income:salary\"\n{schedule}\n\
         [[subscription]]\nname = \"saving\"\nkind = \"transfer\"\nfrom = \"bank:a\"\nto = \"bank:savings\"\n{schedule}\n"
    );
    let rules = Rules::load_str(&rules, "config/rules.toml", &config).expect("the test's rules load");
    let about = |name: &str| {
        let subscription = rules.subscription(name).expect("the subscription is there");
        (subscription.kind_name(), subscription.account())
    };
    assert_eq!(about("gym"), ("expense", Some("expenses:gym")));
    assert_eq!(about("salary"), ("income", Some("income:salary")));
    assert_eq!(about("saving"), ("transfer", None));
}

/// The forecast has a periodic transaction for each price, up to the last
/// due date: one on the day the subscription ends is in it, and a price
/// that no due date is left for isn't.
#[test]
fn the_forecast_follows_price_changes_up_to_the_last_due_date() {
    let periodic = |more: &str, prices: &str| {
        let rules = gym(more).replace("prices = [{ from = 2026-01-01, amount = 100 }]", prices);
        let build = books(&rules, &[(5, 100)], end_of_january());
        build.forecast.lines().filter(|line| line.starts_with("~ ")).map(str::to_string).collect::<Vec<_>>()
    };
    let rise = "prices = [{ from = 2026-01-01, amount = 100 }, { from = 2026-03-01, amount = 120 }]";
    assert_eq!(
        periodic("end = 2026-03-01", rise),
        [
            "~ every 1 months from 2026-02-01 to 2026-03-01  gym  ; subscription:gym",
            "~ every 1 months from 2026-03-01 to 2026-03-02  gym  ; subscription:gym",
        ]
    );
    let late_rise = "prices = [{ from = 2026-01-01, amount = 100 }, { from = 2026-03-15, amount = 120 }]";
    assert_eq!(
        periodic("end = 2026-03-20", late_rise),
        ["~ every 1 months from 2026-02-01 to 2026-03-15  gym  ; subscription:gym"]
    );
}
