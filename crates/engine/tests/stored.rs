//! The books' bank transactions as the store's table has them (docs/spec.md
//! §4): which rows of the table are transactions of a build, what a pin on
//! one of the others means, and what a build says when the table and the raw
//! files don't match.

mod common;

use common::{NORDEA, config, id, ids, load, load_settled, nordea_csv, nordea_rel};
use jiff::civil::date;
use konter_engine::config::{Config, Format};
use konter_engine::engine::Status;
use konter_engine::findings::{Findings, Severity};
use konter_engine::manual::Manual;
use konter_engine::pipeline::{self, Build};
use konter_engine::rules::Rules;
use konter_engine::sources::{self, Loaded};
use konter_engine::store::Table;

/// `bank:a` with both an export and fetched data.
fn both_formats() -> Config {
    config(&NORDEA.replace("formats = [\"nordea-csv\"]", "formats = [\"nordea-csv\", \"enable-banking\"]"))
}

const EXPORT: [(&str, &str, &str, &str); 2] =
    [("2026/01/06", "-20,00", "Coffee", "80,00"), ("2026/01/05", "-50,00", "Groceries", "100,00")];

/// The export's two transactions as fetched: the coffee reads the same, and
/// the groceries are worded differently, which an export has no reference to
/// settle. So the table has the groceries twice, the second row marked.
const FETCH: &str = r#"{"acct_fetch": 1, "fetched_at": "2026-01-16T09:00:00Z", "pages": [{"transactions": [
  {"entry_reference": "r1", "booking_date": "2026-01-05", "status": "BOOK", "credit_debit_indicator": "DBIT",
   "transaction_amount": {"currency": "DKK", "amount": "50.00"}, "remittance_information": ["GROCERIES APS"],
   "balance_after_transaction": {"currency": "DKK", "amount": "100.00"}},
  {"entry_reference": "r2", "booking_date": "2026-01-06", "status": "BOOK", "credit_debit_indicator": "DBIT",
   "transaction_amount": {"currency": "DKK", "amount": "20.00"}, "remittance_information": ["Coffee"],
   "balance_after_transaction": {"currency": "DKK", "amount": "80.00"}}]}]}"#;

const FETCHED: &str = "nordea/1234567890/eb-2026-01-16T09-00-00.json";

fn files(export: &str) -> [(&'static str, String, String); 2] {
    [("bank:a", nordea_rel("2026-01-07 10.00.00"), export.to_owned()), ("bank:a", FETCHED.to_owned(), FETCH.to_owned())]
}

fn borrowed<'a>(files: &'a [(&'static str, String, String)]) -> Vec<(&'static str, &'a str, &'a str)> {
    files.iter().map(|(source, rel, text)| (*source, rel.as_str(), text.as_str())).collect()
}

/// The findings of `severity` about `category`, each with its details.
fn found(findings: &Findings, severity: Severity, category: &str) -> Vec<String> {
    let found = findings.diags.iter().filter(|d| d.severity == severity && d.category == category);
    found.map(|d| format!("{} {:?}", d.message, d.details)).collect()
}

/// The books of `loaded` with `rules`.
fn books(config: &Config, rules: &str, loaded: Loaded, findings: Findings) -> Build {
    let rules = Rules::load_str(rules, "config/rules.toml", config).expect("the test's rules load");
    let manual = Manual::parse("accounts = [\"expenses:food\", \"expenses:other\"]", "").expect("the accounts parse");
    pipeline::assemble(config, rules, loaded, &manual, date(2026, 1, 31), findings).expect("the books build")
}

fn pin(id: &str, account: &str) -> String {
    format!("[pin.{id:?}]\naccount = {account:?}\n")
}

/// A row we aren't sure of counts as the transaction it may be until someone
/// settles it: the books have that transaction once, the build says so, and
/// neither file is said to lack a transaction. If the two were two
/// transactions after all, the running balance wouldn't add up.
#[test]
fn a_row_we_arent_sure_of_counts_as_the_transaction_it_may_be() {
    let config = both_formats();
    let files = files(&nordea_csv(&EXPORT));
    let mut findings = Findings::default();
    let loaded = load(&config, &borrowed(&files), &mut findings);
    let (groceries, coffee, unsure) = (id(1).to_string(), id(2).to_string(), id(3).to_string());
    let a = &loaded.sources["bank:a"];
    let had: Vec<(&str, &str, usize)> =
        a.txs.iter().map(|t| (t.id.as_str(), t.description.as_str(), t.origins.len())).collect();
    assert_eq!(had, [(groceries.as_str(), "Groceries", 2), (coffee.as_str(), "Coffee", 2)]);
    assert!(a.has_running_balance && a.chain_ok);
    assert_eq!(a.opening, Some(150.into()));
    // The coffee gained the fetched copy's reference, and the groceries
    // didn't: the row that has it isn't known to be them.
    assert_eq!((a.txs[0].bank_ref.as_deref(), a.txs[1].bank_ref.as_deref()), (None, Some("r2")));
    assert_eq!(
        found(&findings, Severity::Warning, "store"),
        [format!(
            "bank:a: 1 rows may be transactions the table already had [\"{unsure} bank:a 2026-01-05 -50 \
             \\\"GROCERIES APS\\\" counts as {groceries} bank:a 2026-01-05 -50 \\\"Groceries\\\"\", \"→ until \
             someone settles them, each counts as the transaction it may be (the site's front page)\"]"
        )]
    );
    assert_eq!(found(&findings, Severity::Error, "overlap conflict"), [""; 0]);
    assert_eq!(found(&findings, Severity::Error, "balance chain"), [""; 0]);
    // The row's ID means the transaction, to `explain` and to a pin.
    assert_eq!(loaded.id_of(&unsure), groceries);
    assert_eq!(loaded.id_of(&groceries), groceries);
    assert_eq!(loaded.find(&unsure).map(|t| t.description.as_str()), Some("Groceries"));
    let build = books(&config, &pin(&unsure, "expenses:food"), loaded, findings);
    assert_eq!(build.generated.outcomes[&groceries].status, Status::Pin);
    assert_eq!(found(&build.findings, Severity::Warning, "dead rule"), [""; 0]);
}

/// A row joined to another is that transaction. What was pinned on its ID
/// stays pinned, and marked later stays marked; only a transaction that has
/// a pin of its own can't take over another.
#[test]
fn what_is_pinned_on_a_joined_row_follows_it() {
    let config = both_formats();
    let files = files(&nordea_csv(&EXPORT));
    let join = |table: &mut Table| table.join(&id(1), &id(3)).expect("one transaction");
    let (groceries, coffee, joined) = (id(1).to_string(), id(2).to_string(), id(3).to_string());

    let mut findings = Findings::default();
    let loaded = load_settled(&config, &borrowed(&files), &mut findings, join);
    assert_eq!(found(&findings, Severity::Warning, "store"), [""; 0]);
    let a = &loaded.sources["bank:a"];
    assert_eq!(a.txs.len(), 2);
    // Joining gave the groceries the reference.
    assert_eq!(
        (a.txs[0].id.as_str(), a.txs[0].bank_ref.as_deref(), a.txs[0].origins.len()),
        (&*groceries, Some("r1"), 2)
    );
    let rules = format!("{}[later.{coffee:?}]\n", pin(&joined, "expenses:food"));
    let build = books(&config, &rules, loaded, findings);
    assert_eq!(build.generated.outcomes[&groceries].status, Status::Pin);
    assert!(build.rules.pins.contains_key(&groceries) && !build.rules.pins.contains_key(&joined));
    assert_eq!(build.rules.pins[&groceries].id, groceries);
    assert_eq!(found(&build.findings, Severity::Error, "pins"), [""; 0]);

    // A later mark follows the same way.
    let mut findings = Findings::default();
    let loaded = load_settled(&config, &borrowed(&files), &mut findings, join);
    let build = books(&config, &format!("[later.{joined:?}]\nnote = \"ask\"\n"), loaded, findings);
    assert!(matches!(build.generated.outcomes[&groceries].status, Status::Later(_)));
    assert_eq!(build.rules.later[&groceries].id, groceries);

    // Both rows pinned: the transaction keeps its own, and the other is an error.
    let mut findings = Findings::default();
    let loaded = load_settled(&config, &borrowed(&files), &mut findings, join);
    let rules = format!("{}{}", pin(&joined, "expenses:other"), pin(&groceries, "expenses:food"));
    let build = books(&config, &rules, loaded, findings);
    assert_eq!(
        found(&build.findings, Severity::Error, "pins"),
        [format!(
            "1 marks are for a transaction that has one already [\"pin \\\"{joined}\\\" (config/rules.toml) is for \
             the transaction {groceries}, which has one in config/rules.toml\"]"
        )]
    );
    let food = build.generated.txs.iter().any(|tx| tx.postings.iter().any(|p| p.account == "expenses:food"));
    let other = build.generated.txs.iter().any(|tx| tx.postings.iter().any(|p| p.account == "expenses:other"));
    assert!(food && !other);
}

/// The table says which transactions there are. A raw file it hasn't read is
/// an error that names the file, and the books are built without its rows; a
/// transaction whose raw file can't be read any more is in the books all the
/// same, without the original fields only the file had.
#[test]
fn the_table_and_not_the_raw_files_says_what_the_transactions_are() {
    let config = both_formats();
    let rel = nordea_rel("2026-01-07 10.00.00");
    let export = sources::parse_raw(&config, "bank:a", &rel, &nordea_csv(&EXPORT), None).expect("the export parses");
    let fetch = || sources::parse_raw(&config, "bank:a", FETCHED, FETCH, None).expect("the fetch parses");
    let mut table = Table::default();
    table.read("bank:a", &fetch().rows, &mut ids());

    let mut findings = Findings::default();
    let data = sources::stored_source(&config, "bank:a", &table, vec![export, fetch()], &mut findings);
    assert_eq!(
        found(&findings, Severity::Error, "store"),
        [format!(
            "bank:a: 1 raw files haven't been read into the table [\"data/{rel}\", \"→ the books are built without \
             their rows; `konter store` reads the ones data/MANIFEST lists\"]"
        )]
    );
    let texts: Vec<&str> = data.txs.iter().map(|t| t.description.as_str()).collect();
    assert_eq!(texts, ["GROCERIES APS", "Coffee"]);
    assert_eq!(data.txs[0].format, Some(Format::EnableBanking));
    assert!(data.txs[0].raw.contains_key("entry_reference"), "{:?}", data.txs[0].raw);
    assert_eq!(data.files.len(), 2);

    let mut findings = Findings::default();
    let data = sources::stored_source(&config, "bank:a", &table, Vec::new(), &mut findings);
    assert_eq!(found(&findings, Severity::Error, "store"), [""; 0]);
    assert_eq!(found(&findings, Severity::Warning, "data"), ["bank:a: no raw data yet []"]);
    let had: Vec<_> =
        data.txs.iter().map(|t| (t.description.as_str(), t.format, t.raw.len(), t.origin().line)).collect();
    assert_eq!(had, [("GROCERIES APS", None, 0, 1), ("Coffee", None, 0, 2)]);
    assert!(data.has_running_balance && data.chain_ok);

    // Another account's rows aren't this one's.
    let mut elsewhere = Table::default();
    elsewhere.read("bank:other", &fetch().rows, &mut ids());
    let data = sources::stored_source(&config, "bank:a", &elsewhere, Vec::new(), &mut Findings::default());
    assert!(data.txs.is_empty());
}

/// A transaction that a file covering its day doesn't have is reported,
/// whichever file it is: the bank has changed what it says happened.
#[test]
fn a_transaction_missing_from_a_file_that_covers_its_day_is_reported() {
    let config = config(NORDEA);
    let first = nordea_csv(&[
        ("2026/01/07", "-30,00", "Lunch", "50,00"),
        ("2026/01/06", "-20,00", "Coffee", "80,00"),
        ("2026/01/05", "-50,00", "Groceries", "100,00"),
    ]);
    // A later export without the coffee, and with a balance to match.
    let second = nordea_csv(&[
        ("2026/01/08", "-10,00", "Bus", "60,00"),
        ("2026/01/07", "-30,00", "Lunch", "70,00"),
        ("2026/01/05", "-50,00", "Groceries", "100,00"),
    ]);
    let (one, two) = (nordea_rel("2026-01-07 10.00.00"), nordea_rel("2026-01-08 10.00.00"));
    let mut findings = Findings::default();
    load(&config, &[("bank:a", &one, &first), ("bank:a", &two, &second)], &mut findings);
    let conflicts = found(&findings, Severity::Error, "overlap conflict").concat();
    assert!(conflicts.starts_with("bank:a: files disagree on 2 transactions"), "{conflicts}");
    assert!(conflicts.contains(&format!("\\\"Coffee\\\" (from {one}) is missing from {two}")), "{conflicts}");
    // The lunch with another balance is another transaction, to the table,
    // so the first one is missing too. The second isn't said to be missing
    // from the first export: that one ends on its day, maybe partway.
    assert!(conflicts.contains(&format!("\\\"Lunch\\\" (from {one}) is missing from {two}")), "{conflicts}");
    assert!(!conflicts.contains(&format!("is missing from {one}")), "{conflicts}");
}

/// Without a running balance on every transaction, a day's transactions are
/// in the order of the time of day, then of the file with the most of them.
#[test]
fn a_day_without_running_balances_is_in_time_and_file_order() {
    const LUNAR: &str = "[books]\nstart = 2026-01-01\n\n[sources.\"lunar:b\"]\nbank = \"lunar\"\nnumber = \"555\"\n\
         ledger = \"assets:bank:b\"\nformats = [\"lunar-csv\", \"enable-banking\"]\n\
         unmatched = \"allow\"\n";
    let config = config(LUNAR);
    let fetched = |rows: &[(&str, &str)]| {
        let rows: Vec<String> = rows
            .iter()
            .map(|(reference, text)| {
                format!(
                    r#"{{"entry_reference": "{reference}", "booking_date": "2026-01-05", "status": "BOOK", "credit_debit_indicator": "DBIT", "transaction_amount": {{"currency": "DKK", "amount": "10.00"}}, "remittance_information": ["{text}"]}}"#
                )
            })
            .collect();
        format!(
            r#"{{"acct_fetch": 1, "fetched_at": "2026-01-16T09:00:00Z", "pages": [{{"transactions": [{}]}}]}}"#,
            rows.join(",\n")
        )
    };
    let texts = |loaded: &Loaded| -> Vec<String> {
        loaded.sources["lunar:b"].txs.iter().map(|t| t.description.clone()).collect()
    };
    // One fetch with one row, then one with three: the day is in the order
    // of the longer file, wherever the first file's row is in it.
    let (short, long) = (fetched(&[("r2", "Second")]), fetched(&[("r1", "First"), ("r2", "Second"), ("r3", "Third")]));
    let files = [("lunar:b", "lunar/555/eb-1.json", short.as_str()), ("lunar:b", "lunar/555/eb-2.json", long.as_str())];
    let loaded = load(&config, &files, &mut Findings::default());
    assert_eq!(texts(&loaded), ["First", "Second", "Third"]);
    assert!(!loaded.sources["lunar:b"].has_running_balance);
    // An export gives the times of day, which come first.
    let export = "Date,Time,Title,Amount,Balance,Transaction ID\n\
                  05.01.2026,09.00,Third,\"-10,00\",\"70,00\",r3\n\
                  05.01.2026,12.00,First,\"-10,00\",\"60,00\",r1\n";
    let files = [files[0], files[1], ("lunar:b", "lunar/555/statement.csv", export)];
    let loaded = load(&config, &files, &mut Findings::default());
    assert_eq!(texts(&loaded), ["Second", "Third", "First"]);
}

/// The transactions of `bank:a` from exports (path relative to data/, text),
/// with what loading them found.
fn exported(config: &Config, exports: &[(&str, &str)]) -> (sources::SourceData, Findings) {
    let files: Vec<(&str, &str, &str)> = exports.iter().map(|(rel, text)| ("bank:a", *rel, *text)).collect();
    let mut findings = Findings::default();
    let mut loaded = load(config, &files, &mut findings);
    (loaded.sources.remove("bank:a").expect("the config has bank:a"), findings)
}

fn descriptions(data: &sources::SourceData) -> Vec<&str> {
    data.txs.iter().map(|t| t.description.as_str()).collect()
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
    let (data, findings) = exported(&config, &[(&morning_rel, &morning), (&evening_rel, &evening)]);
    assert!(!findings.has_errors(), "{}", findings.render(true));
    assert_eq!(descriptions(&data), ["Deposit", "Charge"]);
    assert_eq!(data.opening, Some(950.into()));
    // The first place a transaction was read from comes first.
    let files: Vec<&str> = data.txs[0].origins.iter().map(|o| o.file.as_str()).collect();
    assert_eq!(files, [morning_rel.as_str(), evening_rel.as_str()]);
    assert_eq!(data.txs[0].origin().file, morning_rel);
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
    let (data, findings) = exported(&config, &[(&early_rel, &early), (&late_rel, &late)]);
    assert!(!findings.has_errors(), "{}", findings.render(true));
    assert_eq!(descriptions(&data), ["First", "Second", "Third", "Fourth"]);
    let counts: Vec<(usize, Option<jiff::civil::Date>, Option<jiff::civil::Date>)> =
        data.files.iter().map(|f| (f.count, f.first, f.last)).collect();
    assert_eq!(
        counts,
        [(3, Some(date(2026, 1, 5)), Some(date(2026, 1, 10))), (3, Some(date(2026, 1, 5)), Some(date(2026, 1, 15)))]
    );

    // Lacks "Third", between its first day and its last.
    let holed =
        nordea_csv(&[("2026/01/15", "-50,00", "Fourth", "800,00"), ("2026/01/05", "-50,00", "First", "950,00")]);
    let (_, findings) = exported(&config, &[(&early_rel, &early), (&late_rel, &holed)]);
    let conflicts = found(&findings, Severity::Error, "overlap conflict");
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    assert!(conflicts[0].starts_with("bank:a: files disagree on 1 transactions"), "{conflicts:?}");
    let missing = format!("2026-01-10     -50.00 \\\"Third\\\" (from {early_rel}) is missing from {late_rel}");
    assert!(conflicts[0].contains(&missing), "{conflicts:?}");
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
    let (data, findings) = exported(&config, &[(&rel, &nordea_csv(&rows))]);
    assert_eq!(data.txs.len(), 17);
    assert!(!data.chain_ok);
    let breaks = found(&findings, Severity::Error, "balance chain").concat();
    assert!(breaks.contains("on 2026-01-05: the day's transactions don't form a chain"), "{breaks}");
}
