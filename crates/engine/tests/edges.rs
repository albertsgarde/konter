//! Small promises of the engine's own files and messages: what the config
//! refuses, how findings are printed, and what a journal's first line holds.

mod common;

use common::{NORDEA, config};
use jiff::civil::date;
use konter_engine::config::{Config, Format};
use konter_engine::findings::Findings;
use konter_engine::journal;
use konter_engine::rules::Rules;

#[test]
fn two_sources_cant_book_to_one_ledger_account() {
    let twice = format!(
        "{NORDEA}\n[sources.\"bank:b\"]\nbank = \"nordea\"\nnumber = \"2222222222\"\nledger = \"assets:bank:a\"\n\
         formats = [\"nordea-csv\"]\n"
    );
    let error = Config::parse(&twice, "config/sources.toml").expect_err("the config is refused");
    assert!(format!("{error:#}").contains("ledger account \"assets:bank:a\" is used by several sources"), "{error:#}");
}

#[test]
fn a_format_is_written_as_it_is_in_the_config() {
    let formats = [Format::NordeaCsv, Format::LunarCsv, Format::EnableBanking].map(|format| format.to_string());
    assert_eq!(formats, ["nordea-csv", "lunar-csv", "enable-banking"]);
}

/// "The first and last day away, both included."
#[test]
fn a_trip_includes_its_first_and_last_day() {
    let config = config(NORDEA);
    let trips = "[trip.lisbon]\naccount = \"expenses:trips:lisbon\"\nfrom = 2026-01-10\nto = 2026-01-12\n";
    let rules = Rules::load_str(trips, "config/rules.toml", &config).expect("the trip loads");
    let trip = &rules.trips["lisbon"];
    let away: Vec<bool> = (9..=13).map(|day| trip.contains(date(2026, 1, day))).collect();
    assert_eq!(away, [false, true, true, true, false]);
}

#[test]
fn a_journals_first_line_gives_its_as_of_back() {
    let header = journal::header_line(date(2026, 1, 31));
    assert_eq!(journal::as_of_from_header(&format!("{header}\n\n2026-01-05 Shop\n")), Some(date(2026, 1, 31)));
    assert_eq!(journal::as_of_from_header("2026-01-05 Shop\n"), None);
    assert_eq!(journal::as_of_from_header(""), None);
}

/// Long lists of details are cut short unless everything is asked for.
#[test]
fn findings_are_printed_with_their_details_cut_at_twenty_five() {
    let listed = |details: usize, full: bool| {
        let mut findings = Findings::default();
        findings.warn("dead rule", "rules match nothing").details((0..details).map(|n| format!("rule {n}")));
        findings.render(full)
    };
    let cut = listed(27, false);
    assert!(cut.contains("rule 24\n      … and 2 more\n"), "{cut}");
    assert!(!cut.contains("rule 25"), "{cut}");
    for (details, full) in [(25, false), (27, true)] {
        let all = listed(details, full);
        assert!(!all.contains("more"), "{all}");
        assert!(all.contains(&format!("rule {}\n\n0 errors, 1 warnings", details - 1)), "{all}");
    }

    // One set of findings added to another comes after it.
    let mut findings = Findings::default();
    findings.error("books", "first");
    let mut more = Findings::default();
    more.error("parse", "second");
    findings.extend(more);
    let messages: Vec<&str> = findings.diags.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(messages, ["first", "second"]);
}
