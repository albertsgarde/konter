//! Merging a source's files into one history: where a transaction's place in
//! its day comes from, which file stands for it, and what's said when files
//! disagree (docs/spec.md §4). properties.rs checks that however a history
//! is split into exports, the result is the same; these pin single cases.

mod common;

use common::{NORDEA, config, nordea_csv, nordea_rel};
use konter_engine::config::Config;
use konter_engine::findings::{Findings, Severity};
use konter_engine::sources::{self, SourceData};
use rust_decimal::Decimal;

/// Merges `files` (path relative to data/, text) of `source`.
fn merge(config: &Config, source: &str, files: &[(&str, &str)]) -> (SourceData, Findings) {
    let mut findings = Findings::default();
    let raw = files
        .iter()
        .map(|(rel, text)| sources::parse_raw(config, source, rel, text, None).expect("the test's file parses"))
        .collect();
    let data = sources::merge_source(config, source, raw, &mut findings);
    (data, findings)
}

/// The errors about `category`, each with its details.
fn errors(findings: &Findings, category: &str) -> Vec<String> {
    let found = findings.diags.iter().filter(|d| d.severity == Severity::Error && d.category == category);
    found.map(|d| format!("{}\n{}", d.message, d.details.join("\n"))).collect()
}

fn descriptions(data: &SourceData) -> Vec<&str> {
    data.txs.iter().map(|t| t.description.as_str()).collect()
}

const LUNAR: &str = "[books]\nstart = 2026-01-01\n\n[sources.\"lunar:b\"]\nbank = \"lunar\"\nnumber = \"555\"\n\
     ledger = \"assets:bank:b\"\nid_prefix = \"lb\"\nformats = [\"lunar-csv\"]\nunmatched = \"allow\"\n";

fn lunar() -> Config {
    config(LUNAR)
}

const LUNAR_HEADER: &str = "Date,Time,Title,Amount,Balance,Transaction ID\n";

/// Each transaction's ID with what it reads.
fn history(data: &SourceData) -> Vec<(&str, &str)> {
    data.txs.iter().map(|t| (t.id.as_str(), t.description.as_str())).collect()
}

/// A deposit and then a charge of the same amount chain either way round,
/// and with no day before or after them nothing but the files' order says
/// which it was. Where a row is in a file can only be compared within that
/// file, so the day's order is the one in the export that has most of it.
#[test]
fn a_days_order_comes_from_the_export_with_most_of_the_day() {
    let config = config(NORDEA);
    // Taken after the deposit, before the charge.
    let morning = nordea_csv(&[("2026/01/05", "50,00", "Deposit", "1.000,00")]);
    let evening =
        nordea_csv(&[("2026/01/05", "-50,00", "Charge", "950,00"), ("2026/01/05", "50,00", "Deposit", "1.000,00")]);
    let (morning_rel, evening_rel) = (nordea_rel("2026-01-05 10.00.00"), nordea_rel("2026-01-05 20.00.00"));
    let (data, findings) = merge(&config, "bank:a", &[(&morning_rel, &morning), (&evening_rel, &evening)]);
    assert!(!findings.has_errors(), "{}", findings.render(true));
    assert_eq!(descriptions(&data), ["Deposit", "Charge"]);
    assert_eq!(data.opening, Some(Decimal::from(950)));
}

/// "All files this transaction appears in; the first one is its representative."
#[test]
fn a_transaction_in_two_files_is_represented_by_the_first() {
    let config = config(NORDEA);
    let export = nordea_csv(&[("2026/01/05", "-50,00", "Shop", "950,00")]);
    let (first, second) = (nordea_rel("2026-01-10 10.00.00"), nordea_rel("2026-01-20 10.00.00"));
    let (data, _) = merge(&config, "bank:a", &[(&first, &export), (&second, &export)]);
    let files: Vec<&str> = data.txs[0].origins.iter().map(|o| o.file.as_str()).collect();
    assert_eq!(files, [first.as_str(), second.as_str()]);
}

/// A file may start and end partway through a day, so only a transaction
/// strictly between a file's first and last day is missing from it.
#[test]
fn a_transaction_is_missing_from_a_file_only_strictly_inside_its_days() {
    let config = config(NORDEA);
    let early = nordea_csv(&[
        ("2026/01/10", "-50,00", "Third", "850,00"),
        ("2026/01/05", "-50,00", "Second", "900,00"),
        ("2026/01/05", "-50,00", "First", "950,00"),
    ]);
    // Lacks "Second", on its first day.
    let late = nordea_csv(&[
        ("2026/01/15", "-50,00", "Fourth", "800,00"),
        ("2026/01/10", "-50,00", "Third", "850,00"),
        ("2026/01/05", "-50,00", "First", "950,00"),
    ]);
    let (early_rel, late_rel) = (nordea_rel("2026-01-10 10.00.00"), nordea_rel("2026-01-15 10.00.00"));
    let (data, findings) = merge(&config, "bank:a", &[(&early_rel, &early), (&late_rel, &late)]);
    assert!(!findings.has_errors(), "{}", findings.render(true));
    assert_eq!(descriptions(&data), ["First", "Second", "Third", "Fourth"]);

    // Lacks "Third", between its first day and its last.
    let holed =
        nordea_csv(&[("2026/01/15", "-50,00", "Fourth", "800,00"), ("2026/01/05", "-50,00", "First", "950,00")]);
    let (_, findings) = merge(&config, "bank:a", &[(&early_rel, &early), (&late_rel, &holed)]);
    let conflicts = errors(&findings, "overlap conflict");
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    assert!(conflicts[0].starts_with("bank:a: files disagree on 1 transactions"), "{conflicts:?}");
    let missing = format!("2026-01-10     -50.00 \"Third\" (from {early_rel}) is missing from {late_rel}");
    assert!(conflicts[0].contains(&missing), "{conflicts:?}");
    // One format: nothing a cutover would help with.
    assert!(!conflicts[0].contains("several formats"), "{conflicts:?}");
}

/// Files that disagree about a transaction are named, and so is a file that
/// uses one bank reference for two.
#[test]
fn copies_that_disagree_are_reported_with_their_files() {
    let config = lunar();
    let one = format!("{LUNAR_HEADER}05.01.2026,09.00,Coffee,\"-10,00\",\"990,00\",r1\n");
    let other = format!("{LUNAR_HEADER}05.01.2026,09.00,Coffee,\"-12,00\",\"988,00\",r1\n");
    let (_, findings) = merge(&config, "lunar:b", &[("lunar/555/a.csv", &one), ("lunar/555/b.csv", &other)]);
    let conflicts = errors(&findings, "overlap conflict").concat();
    assert!(
        conflicts
            .contains("2026-01-05     -10.00 \"Coffee\": lunar/555/a.csv vs lunar/555/b.csv: amount: -10.00 vs -12.00"),
        "{conflicts}"
    );

    let twice = format!(
        "{LUNAR_HEADER}06.01.2026,09.00,Tea,\"-5,00\",\"985,00\",r1\n05.01.2026,09.00,Coffee,\"-10,00\",\"990,00\",r1\n"
    );
    let (_, findings) = merge(&config, "lunar:b", &[("lunar/555/a.csv", &twice)]);
    let conflicts = errors(&findings, "overlap conflict").concat();
    assert!(
        conflicts.contains(
            "2026-01-05     -10.00 \"Coffee\": lunar/555/a.csv uses its bank reference r1 again for 2026-01-06      \
             -5.00 \"Tea\""
        ),
        "{conflicts}"
    );
}

/// A fetch of the same three purchases as `EXPORT`, without running balances.
const FETCH: &str = r#"{"acct_fetch": 1, "fetched_at": "2026-01-16T09:00:00Z", "pages": [{"transactions": [
  {"booking_date": "2026-01-05", "transaction_amount": {"currency": "DKK", "amount": "50.00"},
   "credit_debit_indicator": "DBIT", "remittance_information": ["First"]},
  {"booking_date": "2026-01-10", "transaction_amount": {"currency": "DKK", "amount": "50.00"},
   "credit_debit_indicator": "DBIT", "remittance_information": ["Third"]},
  {"booking_date": "2026-01-15", "transaction_amount": {"currency": "DKK", "amount": "50.00"},
   "credit_debit_indicator": "DBIT", "remittance_information": ["Fourth"]}
]}]}"#;

/// Copies in two formats can't be matched up without running balances. The
/// conflicts that gives come with what to do about it: set a cutover. With
/// one set, a conflict is about something else.
#[test]
fn conflicts_between_formats_say_to_set_a_cutover() {
    let both = NORDEA.replace("formats = [\"nordea-csv\"]", "formats = [\"nordea-csv\", \"enable-banking\"]");
    let export = nordea_csv(&[
        ("2026/01/15", "-50,00", "Fourth", "800,00"),
        ("2026/01/10", "-50,00", "Third", "850,00"),
        ("2026/01/05", "-50,00", "First", "950,00"),
    ]);
    let rel = nordea_rel("2026-01-15 10.00.00");
    let files = [(rel.as_str(), export.as_str()), ("nordea/1234567890/fetch.json", FETCH)];
    let (_, findings) = merge(&config(&both), "bank:a", &files);
    let conflicts = errors(&findings, "overlap conflict").concat();
    assert!(conflicts.contains("bank:a has data in several formats"), "{conflicts}");
    assert!(conflicts.contains("Set `cutover` for it in config/sources.toml"), "{conflicts}");

    // From the 12th on the fetch counts, and before it the exports, which disagree.
    let cut = both.replace("unmatched = \"allow\"", "unmatched = \"allow\"\ncutover = 2026-01-12");
    let holed =
        nordea_csv(&[("2026/01/11", "-50,00", "Fourth", "800,00"), ("2026/01/05", "-50,00", "First", "950,00")]);
    let early = nordea_csv(&[
        ("2026/01/10", "-50,00", "Third", "850,00"),
        ("2026/01/07", "-50,00", "Second", "900,00"),
        ("2026/01/05", "-50,00", "First", "950,00"),
    ]);
    let early_rel = nordea_rel("2026-01-10 10.00.00");
    let files =
        [(early_rel.as_str(), early.as_str()), (rel.as_str(), holed.as_str()), ("nordea/1234567890/fetch.json", FETCH)];
    let (data, findings) = merge(&config(&cut), "bank:a", &files);
    assert!(descriptions(&data).contains(&"Fourth"), "{:?}", descriptions(&data));
    let conflicts = errors(&findings, "overlap conflict").concat();
    assert!(conflicts.contains("is missing from"), "{conflicts}");
    assert!(!conflicts.contains("several formats"), "{conflicts}");
}

/// A day whose rows can't be put in an order that follows the running
/// balance is reported, also when there are too many orders to try them all:
/// eight deposits and eight charges that undo each other, and one row that
/// fits nowhere.
#[test]
fn a_day_that_doesnt_chain_is_reported_without_trying_every_order() {
    let config = config(NORDEA);
    let mut rows = vec![("2026/01/05", "-7,00", "Stray", "5.000,00")];
    for _ in 0..8 {
        rows.push(("2026/01/05", "-1,00", "Charge", "1.000,00"));
        rows.push(("2026/01/05", "1,00", "Deposit", "1.001,00"));
    }
    let rel = nordea_rel("2026-01-05 20.00.00");
    let (data, findings) = merge(&config, "bank:a", &[(&rel, &nordea_csv(&rows))]);
    assert_eq!(data.txs.len(), 17);
    let breaks = errors(&findings, "balance chain").concat();
    assert!(breaks.contains("on 2026-01-05: the day's transactions don't form a chain"), "{breaks}");
}

/// Rows that look alike (same day, amount and running balance) are told
/// apart by counting them from the day's beginning, which every export of a
/// day has. Neither bank makes a file that starts partway through a day, and
/// one that does can't pass unnoticed: its later row is taken for the earlier
/// one, and either the two read differently, or a transaction goes missing
/// and the running balance breaks. Only when a longer file has both rows and
/// they read the same is nothing said, and then nothing is wrong either.
#[test]
fn a_file_that_starts_partway_through_a_day_cant_swap_rows_that_look_alike_unnoticed() {
    let config = config(NORDEA);
    // Two payments of 100 come in on the 12th, with a dinner paid between them.
    let day = |first: &'static str, second: &'static str| {
        [
            ("2026/01/14", "-20,00", "Coffee", "180,00"),
            ("2026/01/12", "100,00", second, "200,00"),
            ("2026/01/12", "-100,00", "Club dinner", "100,00"),
            ("2026/01/12", "100,00", first, "200,00"),
            ("2026/01/05", "-50,00", "Groceries", "100,00"),
        ]
    };
    let (early_rel, late_rel) = (nordea_rel("2026-01-13 10.00.00"), nordea_rel("2026-01-15 10.00.00"));

    // From two people: the file without the first payment has the second in its place.
    let rows = day("MobilePay Alice", "MobilePay Carol");
    let (whole, tail) = (nordea_csv(&rows), nordea_csv(&rows[..3]));
    let (_, findings) = merge(&config, "bank:a", &[(&early_rel, &whole), (&late_rel, &tail)]);
    let conflicts = errors(&findings, "overlap conflict").concat();
    assert!(conflicts.contains("description: \"MobilePay Alice\" vs \"MobilePay Carol\""), "{}", findings.render(true));

    // Both from one person, and no file with the whole day: the second
    // payment is taken for a copy of the first, and the balance doesn't add up.
    let rows = day("MobilePay Alice", "MobilePay Alice");
    let (head, tail) = (nordea_csv(&rows[3..]), nordea_csv(&rows[..3]));
    let (data, findings) = merge(&config, "bank:a", &[(&early_rel, &head), (&late_rel, &tail)]);
    assert_eq!(data.txs.len(), 4, "{:?}", descriptions(&data));
    assert_eq!(errors(&findings, "balance chain").len(), 1, "{}", findings.render(true));

    // Both from one person, and a file with the whole day: the same history
    // as from that file alone.
    let whole = nordea_csv(&rows);
    let (both, findings) = merge(&config, "bank:a", &[(&early_rel, &whole), (&late_rel, &tail)]);
    assert!(!findings.has_errors(), "{}", findings.render(true));
    let (alone, _) = merge(&config, "bank:a", &[(&early_rel, &whole)]);
    let history = |data: &SourceData| -> Vec<(String, String)> {
        data.txs.iter().map(|t| (t.id.clone(), t.description.clone())).collect()
    };
    assert_eq!(history(&both), history(&alone));
}

/// Lunar's rows from one minute aren't reliably in order, so a later export
/// can list two that look alike the other way round, and it's the one that
/// places them, having more of the day. Each still keeps the place it was
/// first counted in, and with it its ID.
#[test]
fn look_alikes_keep_their_ids_when_a_longer_export_lists_them_the_other_way_round() {
    let config = lunar();
    // A purchase, its refund and another purchase, all at 12.00.
    let first = format!(
        "{LUNAR_HEADER}04.01.2026,12.00,Kiosk,\"-50,00\",\"950,00\",r3\n\
         04.01.2026,12.00,Refund,\"50,00\",\"1000,00\",r2\n\
         04.01.2026,12.00,Bakery,\"-50,00\",\"950,00\",r1\n\
         03.01.2026,09.00,Salary,\"1000,00\",\"1000,00\",r0\n"
    );
    let second = format!(
        "{LUNAR_HEADER}04.01.2026,13.00,Coffee,\"-10,00\",\"940,00\",r4\n\
         04.01.2026,12.00,Bakery,\"-50,00\",\"950,00\",r1\n\
         04.01.2026,12.00,Refund,\"50,00\",\"1000,00\",r2\n\
         04.01.2026,12.00,Kiosk,\"-50,00\",\"950,00\",r3\n\
         03.01.2026,09.00,Salary,\"1000,00\",\"1000,00\",r0\n"
    );
    let (alone, _) = merge(&config, "lunar:b", &[("lunar/555/a.csv", &first)]);
    assert_eq!(descriptions(&alone), ["Salary", "Bakery", "Refund", "Kiosk"]);
    let (both, findings) = merge(&config, "lunar:b", &[("lunar/555/a.csv", &first), ("lunar/555/b.csv", &second)]);
    assert!(!findings.has_errors(), "{}", findings.render(true));
    assert_eq!(history(&both)[..4], history(&alone));
    assert_eq!(descriptions(&both)[4], "Coffee");
}

/// A fetch of the same account as `LUNAR_HEADER`'s exports, with running
/// balances: two payments of 100 on the 12th, and a dinner paid between them.
const FETCH_WITH_BALANCES: &str = r#"{"acct_fetch": 1, "fetched_at": "2026-01-16T09:00:00Z", "pages": [{"transactions": [
  {"booking_date": "2026-01-05", "transaction_amount": {"currency": "DKK", "amount": "50.00"}, "entry_reference": "r1",
   "credit_debit_indicator": "DBIT", "remittance_information": ["Groceries"],
   "balance_after_transaction": {"currency": "DKK", "amount": "100.00"}},
  {"booking_date": "2026-01-12", "transaction_amount": {"currency": "DKK", "amount": "100.00"}, "entry_reference": "r2",
   "credit_debit_indicator": "CRDT", "remittance_information": ["MobilePay Alice"],
   "balance_after_transaction": {"currency": "DKK", "amount": "200.00"}},
  {"booking_date": "2026-01-12", "transaction_amount": {"currency": "DKK", "amount": "100.00"}, "entry_reference": "r3",
   "credit_debit_indicator": "DBIT", "remittance_information": ["Club dinner"],
   "balance_after_transaction": {"currency": "DKK", "amount": "100.00"}},
  {"booking_date": "2026-01-12", "transaction_amount": {"currency": "DKK", "amount": "100.00"}, "entry_reference": "r4",
   "credit_debit_indicator": "CRDT", "remittance_information": ["MobilePay Carol"],
   "balance_after_transaction": {"currency": "DKK", "amount": "200.00"}}
]}]}"#;

/// A transaction keeps the time of day of its first copy, and takes its place
/// from the fullest file. An export has times and fetched data has none, so
/// after a fetch of the whole day the first payment has a time and the second,
/// which only the fetch has, doesn't, and sorts before it. The first was
/// counted first, and keeps its ID.
#[test]
fn look_alikes_keep_their_ids_when_only_one_of_them_has_a_time_of_day() {
    let both_formats = LUNAR.replace("formats = [\"lunar-csv\"]", "formats = [\"lunar-csv\", \"enable-banking\"]");
    let config = config(&both_formats);
    // Exported on the 12th, after the first payment only.
    let morning = format!(
        "{LUNAR_HEADER}12.01.2026,09.00,MobilePay Alice,\"100,00\",\"200,00\",r2\n\
         05.01.2026,09.00,Groceries,\"-50,00\",\"100,00\",r1\n"
    );
    let (alone, _) = merge(&config, "lunar:b", &[("lunar/555/a.csv", &morning)]);
    assert_eq!(descriptions(&alone), ["Groceries", "MobilePay Alice"]);
    let files = [("lunar/555/a.csv", morning.as_str()), ("lunar/555/fetch.json", FETCH_WITH_BALANCES)];
    let (both, findings) = merge(&config, "lunar:b", &files);
    assert!(!findings.has_errors(), "{}", findings.render(true));
    assert_eq!(descriptions(&both), ["Groceries", "MobilePay Alice", "Club dinner", "MobilePay Carol"]);
    assert_eq!(history(&both)[..2], history(&alone));
}
