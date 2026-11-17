//! What a build says about how fresh and how complete the bank data is: data
//! after `--as-of`, data that's getting old, and history the bank is about to
//! forget (docs/spec.md §3, §7). Each is tested on the day it starts to hold.

mod common;

use common::{NORDEA, build, config, nordea_csv, nordea_rel};
use jiff::civil::{Date, date};
use konter_engine::findings::Severity;
use konter_engine::rules::Rules;

/// The warnings about `category` when `bank:a` in `sources` has one export,
/// taken on 31 January, of `rows` (newest first), and the books are built as
/// of `as_of`.
fn warnings(sources: &str, rows: &[(&str, &str, &str, &str)], as_of: Date, category: &str) -> Vec<String> {
    let config = config(sources);
    let rules = Rules::load_str("", "config/rules.toml", &config).expect("no rules load");
    let rel = nordea_rel("2026-01-31 10.00.00");
    let files = [("bank:a", rel.as_str(), &*nordea_csv(rows))];
    let build = build(&config, rules, &[], if rows.is_empty() { &[] } else { &files }, as_of);
    let found = build.findings.diags.iter().filter(|d| d.severity == Severity::Warning && d.category == category);
    found.map(|d| d.message.clone()).collect()
}

/// A purchase on 20 January, on an account that had 1000 before it.
const ONE: [(&str, &str, &str, &str); 1] = [("2026/01/20", "-50,00", "Shop", "950,00")];

/// The bank keeps `days` days of history.
fn history(days: i64) -> String {
    format!("{NORDEA}\n[banks.nordea]\nhistory_days = {days}\n")
}

#[test]
fn data_after_as_of_is_warned_about() {
    assert_eq!(warnings(NORDEA, &ONE, date(2026, 1, 20), "as-of"), [""; 0]);
    assert_eq!(
        warnings(NORDEA, &ONE, date(2026, 1, 19), "as-of"),
        ["bank:a has data after --as-of 2026-01-19 (until 2026-01-20)"]
    );
}

/// Exported data is stale after more than 90 days, counted from the export.
#[test]
fn an_export_is_stale_after_ninety_days() {
    assert_eq!(warnings(NORDEA, &ONE, date(2026, 5, 1), "stale data"), [""; 0]);
    let stale = ["bank:a: data covers until 2026-01-31 (91 days ago); export bank:a again and `konter add-export` it"];
    assert_eq!(warnings(NORDEA, &ONE, date(2026, 5, 2), "stale data"), stale);

    // Also at a bank that Enable Banking knows, when this account isn't
    // fetched from it: there's nothing to run `konter fetch` for.
    let known = format!("{NORDEA}\n[banks.nordea]\nenable_banking = {{ aspsp = \"Nordea\", country = \"DK\" }}\n");
    assert_eq!(warnings(&known, &ONE, date(2026, 5, 1), "stale data"), [""; 0]);
    assert_eq!(warnings(&known, &ONE, date(2026, 5, 2), "stale data"), stale);
}

/// A bank that only keeps so much history has to be downloaded from again
/// before the last download is that old. The warning comes a year ahead.
#[test]
fn the_next_download_is_asked_for_a_year_before_the_banks_history_passes_the_last() {
    // 400 days of history: the export from 31 January is safe until 7 March 2027.
    let said = |as_of| {
        let mut said = warnings(&history(400), &ONE, as_of, "download soon");
        said.retain(|warning| warning.contains("data covers until"));
        said
    };
    assert_eq!(said(date(2026, 3, 7)), [""; 0]);
    assert_eq!(
        said(date(2026, 3, 8)),
        ["bank:a: data covers until 2026-01-31; export bank:a again and `konter add-export` it before 2027-03-07, \
          when the bank's 400-day history passes it"]
    );
    // And not as stale data on top of that.
    assert_eq!(warnings(&history(400), &ONE, date(2026, 6, 1), "stale data"), [""; 0]);
}

/// The warnings that the beginning of an account's history isn't downloaded.
fn beginning(config: &str, rows: &[(&str, &str, &str, &str)], as_of: Date) -> Vec<String> {
    let mut said = warnings(config, rows, as_of, "download soon");
    said.retain(|warning| !warning.contains("data covers until"));
    said
}

/// The books start on 1 January and the data on the 20th: what's between
/// them is asked for while the bank still has it, and more urgently after.
#[test]
fn history_missing_before_the_data_is_asked_for() {
    let end_of_january = date(2026, 1, 31);
    // With 100 days of history, 1 January starts to go on 11 April.
    assert_eq!(
        beginning(&history(100), &ONE, end_of_january),
        ["bank:a: nothing before 2026-01-20 is downloaded, though its start is 2026-01-01; export bank:a from \
          2026-01-01 and `konter add-export` it before 2026-04-11, when the bank's 100-day history passes \
          2026-01-01, or set `start` to 2026-01-20 if there's nothing before it"]
    );
    for as_of in [date(2026, 4, 11), date(2026, 4, 29)] {
        let said = beginning(&history(100), &ONE, as_of);
        assert_eq!(said.len(), 1, "as of {as_of}: {said:?}");
        assert!(said[0].contains("history passed 2026-01-01 on 2026-04-11"), "as of {as_of}: {said:?}");
    }
    // Once the bank has nothing from before the data either, it's too late to ask.
    assert_eq!(beginning(&history(100), &ONE, date(2026, 4, 30)), [""; 0]);
}

/// Nothing is missing when the data starts within a week of the account's
/// start, or when the account was new (it had nothing before its first
/// transaction). Either is enough on its own.
#[test]
fn history_isnt_missing_when_the_data_starts_at_the_accounts_start() {
    let end_of_january = date(2026, 1, 31);
    // A week after the start is a quiet start; eight days is a gap.
    let on = |day: &'static str| [(day, "-50,00", "Shop", "950,00")];
    assert_eq!(beginning(&history(100), &on("2026/01/08"), end_of_january), [""; 0]);
    assert_eq!(beginning(&history(100), &on("2026/01/09"), end_of_january).len(), 1);

    // An account that had nothing before its first transaction.
    let new = [("2026/01/20", "-50,00", "Shop", "-50,00")];
    assert_eq!(beginning(&history(100), &new, end_of_january), [""; 0]);
}

/// An account with nothing downloaded is asked for too, and one that isn't
/// imported at all is told apart from it.
#[test]
fn accounts_without_data_are_asked_for() {
    let with_savings =
        format!("{}\n[sources.\"bank:savings\"]\nbank = \"nordea\"\nledger = \"assets:bank:savings\"\n", history(100));
    let said = beginning(&with_savings, &[], date(2026, 1, 31));
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(said[0].starts_with("bank:a: nothing is downloaded yet; export bank:a from 2026-01-01"), "{said:?}");
    assert!(said[0].ends_with("or set a later `start`"), "{said:?}");
    assert!(
        said[1].starts_with("bank:savings isn't imported yet; import it (give it `formats` in config/sources.toml)"),
        "{said:?}"
    );
}
