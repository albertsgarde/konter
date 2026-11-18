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

/// Fetched data: (reference, date, amount, text, running balance after),
/// oldest first.
fn fetched(rows: &[(&str, &str, &str, &str, Option<&str>)]) -> String {
    let rows: Vec<String> = rows
        .iter()
        .map(|(reference, date, amount, text, balance)| {
            let (indicator, amount) = match amount.strip_prefix('-') {
                Some(amount) => ("DBIT", amount),
                None => ("CRDT", *amount),
            };
            let balance = balance.map_or_else(String::new, |b| {
                format!(r#", "balance_after_transaction": {{"currency": "DKK", "amount": "{b}"}}"#)
            });
            format!(
                r#"{{"entry_reference": "{reference}", "booking_date": "{date}", "status": "BOOK", "credit_debit_indicator": "{indicator}", "transaction_amount": {{"currency": "DKK", "amount": "{amount}"}}, "remittance_information": ["{text}"]{balance}}}"#
            )
        })
        .collect();
    format!(
        r#"{{"acct_fetch": 1, "fetched_at": "2026-01-16T09:00:00Z", "pages": [{{"transactions": [{}]}}]}}"#,
        rows.join(", ")
    )
}

/// A Nordea account that is fetched as well as exported.
fn nordea_with_fetches() -> Config {
    config(&NORDEA.replace("formats = [\"nordea-csv\"]", "formats = [\"nordea-csv\", \"enable-banking\"]"))
}

const GROCERIES: (&str, &str, &str, &str, Option<&str>) = ("r1", "2026-01-05", "-50.00", "Groceries", Some("100.00"));
const ALICE: (&str, &str, &str, &str, Option<&str>) = ("r2", "2026-01-12", "100.00", "MobilePay Alice", Some("200.00"));
const DINNER: (&str, &str, &str, &str, Option<&str>) = ("r3", "2026-01-12", "-100.00", "Club dinner", Some("100.00"));
const CAROL: (&str, &str, &str, &str, Option<&str>) = ("r4", "2026-01-12", "100.00", "MobilePay Carol", Some("200.00"));
const COFFEE: (&str, &str, &str, &str, Option<&str>) = ("r5", "2026-01-14", "-20.00", "Coffee", Some("180.00"));

/// Two rows that look alike can be listed either way round in two files, and
/// the longer file is the one that places them. Each still keeps the place it
/// was first counted in, and with it its ID.
#[test]
fn look_alikes_keep_their_ids_when_a_longer_file_lists_them_the_other_way_round() {
    let config = nordea_with_fetches();
    // A purchase, its refund and another purchase on one day.
    let salary = ("s0", "2026-01-03", "1000.00", "Salary", Some("1000.00"));
    let bakery = ("s1", "2026-01-04", "-50.00", "Bakery", Some("950.00"));
    let refund = ("s2", "2026-01-04", "50.00", "Refund", Some("1000.00"));
    let kiosk = ("s3", "2026-01-04", "-50.00", "Kiosk", Some("950.00"));
    let coffee = ("s4", "2026-01-04", "-10.00", "Coffee", Some("940.00"));
    let first = fetched(&[salary, bakery, refund, kiosk]);
    let second = fetched(&[salary, kiosk, refund, bakery, coffee]);
    let (one, two) = ("nordea/1234567890/eb-1.json", "nordea/1234567890/eb-2.json");
    let (alone, _) = merge(&config, "bank:a", &[(one, &first)]);
    assert_eq!(descriptions(&alone), ["Salary", "Bakery", "Refund", "Kiosk"]);
    let (both, findings) = merge(&config, "bank:a", &[(one, &first), (two, &second)]);
    assert!(!findings.has_errors(), "{}", findings.render(true));
    assert_eq!(history(&both)[..4], history(&alone));
    assert_eq!(descriptions(&both)[4], "Coffee");
}

/// Rows that look alike are told apart by counting them from the start of
/// the day, so a file has to have every transaction of a day up to the last
/// one it has. One that has a payment without an earlier one that looks the
/// same would give the earlier one its ID. Fetched data has the bank's
/// references, which show it: an error that names both rows, whichever of
/// the files comes first.
#[test]
fn a_file_without_an_earlier_look_alike_is_reported() {
    let config = nordea_with_fetches();
    // The second fetch has a payment and a dinner before the payment that
    // the first one had as the day's only row.
    let morning = fetched(&[GROCERIES, ALICE]);
    let later = fetched(&[GROCERIES, CAROL, DINNER, ALICE, COFFEE]);
    let (one, two) = ("nordea/1234567890/eb-1.json", "nordea/1234567890/eb-2.json");
    let (alone, _) = merge(&config, "bank:a", &[(one, &morning)]);
    let (alice, carol) = ("2026-01-12     100.00 \"MobilePay Alice\"", "2026-01-12     100.00 \"MobilePay Carol\"");

    // The first fetch first: the later one's first payment counts as the
    // one already there, and has another reference.
    let (data, findings) = merge(&config, "bank:a", &[(one, &morning), (two, &later)]);
    let reported = errors(&findings, "incomplete day");
    assert_eq!(reported.len(), 1, "{}", findings.render(true));
    assert!(reported[0].contains("bank:a: 1 rows that look alike don't match up between files"), "{reported:?}");
    let named = format!("{one} has {alice} but not {carol}, which looks the same and takes its place in {two}");
    assert!(reported[0].contains(&named), "{reported:?}");
    assert!(reported[0].contains("a file must have every transaction of a day up to the last one it has"));
    // Nothing else is wrong: every row is there once, and the balance chains.
    assert_eq!(findings.count(Severity::Error), 1, "{}", findings.render(true));
    assert_eq!(descriptions(&data), ["Groceries", "MobilePay Alice", "Club dinner", "MobilePay Carol", "Coffee"]);
    assert_eq!(history(&data)[..2], history(&alone));

    // The later fetch first: the first one's payment is the second of its
    // kind, and the first fetch doesn't have the one before it.
    let (data, findings) = merge(&config, "bank:a", &[(one, &later), (two, &morning)]);
    let reported = errors(&findings, "incomplete day");
    let named = format!("{two} has {alice} but not {carol}, which looks the same and comes before it in {one}");
    assert!(reported.concat().contains(&named), "{}", findings.render(true));
    assert_eq!(findings.count(Severity::Error), 1, "{}", findings.render(true));
    assert_eq!(descriptions(&data), ["Groceries", "MobilePay Carol", "Club dinner", "MobilePay Alice", "Coffee"]);
}

/// A row's reference is what places it when an earlier file had it. A fetch
/// without running balances that has the day's first payment gives it a
/// place by its reference, so in a later fetch the second payment is the
/// only one that's counted: the second of its kind with no first, and
/// nothing wrong.
#[test]
fn a_look_alike_placed_by_its_reference_isnt_missed() {
    let config = nordea_with_fetches();
    let first = fetched(&[("r2", "2026-01-12", "100.00", "MobilePay Alice", None)]);
    let second = fetched(&[ALICE, DINNER, CAROL]);
    let files = [("nordea/1234567890/eb-1.json", first.as_str()), ("nordea/1234567890/eb-2.json", second.as_str())];
    let (data, findings) = merge(&config, "bank:a", &files);
    assert!(!findings.has_errors(), "{}", findings.render(true));
    assert_eq!(descriptions(&data), ["MobilePay Alice", "Club dinner", "MobilePay Carol"]);
}

/// Only two references that differ say that two rows are two transactions.
/// Nordea's exports have none and its fetched data has them, so their copies
/// of two payments that look alike pair up by counting, whichever comes first.
#[test]
fn copies_with_and_without_a_reference_are_the_same_rows() {
    let config = nordea_with_fetches();
    let export = nordea_csv(&[
        ("2026/01/12", "100,00", "MobilePay Carol", "200,00"),
        ("2026/01/12", "-100,00", "Club dinner", "100,00"),
        ("2026/01/12", "100,00", "MobilePay Alice", "200,00"),
        ("2026/01/05", "-50,00", "Groceries", "100,00"),
    ]);
    let fetch = fetched(&[GROCERIES, ALICE, DINNER, CAROL]);
    let rel = nordea_rel("2026-01-13 10.00.00");
    for fetch_rel in ["nordea/1234567890/fetch.json", "nordea/1234567890/A fetch.json"] {
        let files = [(rel.as_str(), export.as_str()), (fetch_rel, fetch.as_str())];
        let (data, findings) = merge(&config, "bank:a", &files);
        assert!(!findings.has_errors(), "{fetch_rel}: {}", findings.render(true));
        assert_eq!(data.txs.len(), 4, "{fetch_rel}: {:?}", descriptions(&data));
        assert!(data.txs.iter().all(|t| t.origins.len() == 2), "{fetch_rel}");
    }
}

/// A Lunar account that is fetched as well as exported.
fn lunar_with_fetches() -> Config {
    config(&LUNAR.replace("formats = [\"lunar-csv\"]", "formats = [\"lunar-csv\", \"enable-banking\"]"))
}

/// Each transaction's ID by what it reads, whatever order they're in.
fn ids(data: &SourceData) -> Vec<(&str, &str)> {
    let mut ids: Vec<(&str, &str)> = data.txs.iter().map(|t| (t.description.as_str(), t.id.as_str())).collect();
    ids.sort_unstable();
    ids
}

/// An ID mustn't depend on which files a transaction is in. Lunar's fetched
/// data has no running balance and its exports have one, and both have the
/// bank's reference, so that's what a Lunar ID is made from: the same from a
/// fetch alone, from an export alone and from both, also for two payments
/// that look alike and that the two files list the other way round.
#[test]
fn a_lunar_id_is_the_same_from_a_fetch_an_export_or_both() {
    let config = lunar_with_fetches();
    let no_balance =
        |(reference, date, amount, text, _): (&'static str, &'static str, &'static str, &'static str, _)| {
            (reference, date, amount, text, None)
        };
    let fetch = fetched(&[GROCERIES, ALICE, DINNER, CAROL].map(no_balance));
    let export = format!(
        "{LUNAR_HEADER}12.01.2026,09.00,MobilePay Alice,\"100,00\",\"200,00\",r2\n\
         12.01.2026,09.00,Club dinner,\"-100,00\",\"100,00\",r3\n\
         12.01.2026,09.00,MobilePay Carol,\"100,00\",\"200,00\",r4\n\
         05.01.2026,09.00,Groceries,\"-50,00\",\"100,00\",r1\n"
    );
    let (fetch_rel, export_rel) = ("lunar/555/eb.json", "lunar/555/statement.csv");
    let (fetched_only, findings) = merge(&config, "lunar:b", &[(fetch_rel, &fetch)]);
    assert!(!findings.has_errors(), "{}", findings.render(true));
    assert_eq!(fetched_only.txs.len(), 4);
    let (exported_only, findings) = merge(&config, "lunar:b", &[(export_rel, &export)]);
    assert!(!findings.has_errors(), "{}", findings.render(true));
    let (both, findings) = merge(&config, "lunar:b", &[(fetch_rel, &fetch), (export_rel, &export)]);
    assert!(!findings.has_errors(), "{}", findings.render(true));
    assert_eq!(ids(&exported_only), ids(&fetched_only));
    assert_eq!(ids(&both), ids(&fetched_only));
    // With the export, the balance is known and checked.
    assert!(both.has_running_balance && both.chain_ok);
}

/// A transaction that lacks what its account's IDs are made from would get
/// another ID once a file that has it arrives, so it's an error: a Lunar row
/// without a reference, a Nordea row without a running balance. With a
/// cutover each format has its own dates and no such file can arrive.
#[test]
fn a_transaction_without_what_its_id_is_made_from_is_reported() {
    let export = format!("{LUNAR_HEADER}05.01.2026,09.00,Groceries,\"-50,00\",\"100,00\",\n");
    let (data, findings) = merge(&lunar(), "lunar:b", &[("lunar/555/statement.csv", &export)]);
    assert_eq!(data.txs.len(), 1);
    let reported = errors(&findings, "ids").concat();
    assert!(
        reported.contains("lunar:b: 1 transactions have no bank reference, which this account's IDs are made from"),
        "{}",
        findings.render(true)
    );
    assert!(reported.contains("2026-01-05     -50.00 \"Groceries\" (from lunar/555/statement.csv)"), "{reported}");

    let fetch = fetched(&[("r1", "2026-01-05", "-50.00", "Groceries", None)]);
    let files = [("nordea/1234567890/eb.json", fetch.as_str())];
    let (_, findings) = merge(&nordea_with_fetches(), "bank:a", &files);
    let reported = errors(&findings, "ids").concat();
    assert!(reported.contains("bank:a: 1 transactions have no running balance"), "{}", findings.render(true));
    assert!(reported.contains("the ID such a transaction gets would change when a file that has its running balance"));

    let cut = NORDEA
        .replace("formats = [\"nordea-csv\"]", "formats = [\"nordea-csv\", \"enable-banking\"]\ncutover = 2026-01-01");
    let (_, findings) = merge(&config(&cut), "bank:a", &files);
    assert!(!findings.has_errors(), "{}", findings.render(true));
}

/// Copies in two formats may describe a transaction differently, but not say
/// that it was booked on another day or was another amount: the day is in
/// the ID.
#[test]
fn copies_in_two_formats_agree_on_the_day_and_the_amount() {
    let config = lunar_with_fetches();
    let fetch = fetched(&[("r1", "2026-01-05", "-50.00", "GROCERIES APS", None)]);
    let agreeing = format!("{LUNAR_HEADER}05.01.2026,09.00,Groceries,\"-50,00\",\"100,00\",r1\n");
    let files = [("lunar/555/eb.json", fetch.as_str()), ("lunar/555/statement.csv", agreeing.as_str())];
    let (_, findings) = merge(&config, "lunar:b", &files);
    assert!(!findings.has_errors(), "{}", findings.render(true));

    let a_day_later = format!("{LUNAR_HEADER}06.01.2026,09.00,Groceries,\"-55,00\",\"95,00\",r1\n");
    let files = [("lunar/555/eb.json", fetch.as_str()), ("lunar/555/statement.csv", a_day_later.as_str())];
    let (_, findings) = merge(&config, "lunar:b", &files);
    let conflicts = errors(&findings, "overlap conflict").concat();
    assert!(
        conflicts.contains(
            "lunar/555/eb.json vs lunar/555/statement.csv: booked: 2026-01-05 vs 2026-01-06; amount: -50.00 vs -55.00"
        ),
        "{}",
        findings.render(true)
    );
}
