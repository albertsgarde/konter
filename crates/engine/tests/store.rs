//! The store of transactions: its table as text, and what reading a bank's
//! files into it does. The point of it is that a transaction keeps the ID it
//! was first given, whatever arrives later.

mod common;

use std::collections::BTreeMap;

use common::{NORDEA, config, nordea_csv, nordea_rel};
use konter_engine::config::Config;
use konter_engine::model::BankTransaction;
use konter_engine::sources;
use konter_engine::store::{Reading, Table, TransactionId};
use rust_decimal::Decimal;

const LUNAR: &str = "[books]\nstart = 2026-01-01\n\n[sources.\"lunar:b\"]\nbank = \"lunar\"\nnumber = \"555\"\n\
     ledger = \"assets:bank:b\"\nid_prefix = \"lb\"\nformats = [\"lunar-csv\", \"enable-banking\"]\nunmatched = \"allow\"\n";

const LUNAR_HEADER: &str = "Date,Time,Title,Amount,Balance,Transaction ID\n";

/// A fetch of one transaction without a running balance, as Lunar's are.
fn fetched(reference: &str, date: &str, amount: &str, currency: &str, text: &str) -> String {
    format!(
        r#"{{"acct_fetch": 1, "fetched_at": "2026-01-16T09:00:00Z", "pages": [{{"transactions": [{{"entry_reference": "{reference}", "booking_date": "{date}", "status": "BOOK", "credit_debit_indicator": "DBIT", "transaction_amount": {{"currency": "{currency}", "amount": "{amount}"}}, "remittance_information": ["{text}"]}}]}}]}}"#
    )
}

/// IDs that are the same from run to run: 0101…, 0202… and so on.
fn ids() -> impl FnMut() -> TransactionId {
    let mut n = 0u8;
    move || {
        n += 1;
        TransactionId::from_random([n; 16])
    }
}

fn rows(config: &Config, source: &str, rel: &str, text: &str) -> Vec<BankTransaction> {
    sources::parse_raw(config, source, rel, text, None).expect("the test's file parses").rows
}

/// Each transaction's ID, by what it reads.
fn by_text(table: &Table) -> BTreeMap<String, String> {
    table.transactions().map(|t| (t.text.clone().unwrap_or_default(), t.id.to_string())).collect()
}

/// Two payments of 100 on the 12th with a dinner paid between them: the
/// payments have the same day, amount and running balance.
const DAY: [(&str, &str, &str, &str); 5] = [
    ("2026/01/14", "-20,00", "Coffee", "180,00"),
    ("2026/01/12", "100,00", "MobilePay Alice", "200,00"),
    ("2026/01/12", "-100,00", "Club dinner", "100,00"),
    ("2026/01/12", "100,00", "MobilePay Carol", "200,00"),
    ("2026/01/05", "-50,00", "Groceries", "100,00"),
];

/// The problem the store is there for. An export has one payment on the
/// 12th. A later one has a payment before it that day which looks the same.
/// Counting from the start of the day, the second export's first payment is
/// the one we had; here the one we had keeps its ID, and the other gets its
/// own.
#[test]
fn a_transaction_that_turns_up_earlier_in_the_day_gets_an_id_of_its_own() {
    let config = config(NORDEA);
    let (mut table, mut mint) = (Table::default(), ids());
    let morning = nordea_csv(&[DAY[1], DAY[4]]);
    let first = table
        .read("bank:a", &rows(&config, "bank:a", &nordea_rel("2026-01-12 10.00.00"), &morning), &mut mint)
        .expect("the file is read");
    assert_eq!((first.matched, first.added.len()), (0, 2));
    let before = by_text(&table);

    let later = nordea_csv(&DAY);
    let second = table
        .read("bank:a", &rows(&config, "bank:a", &nordea_rel("2026-01-15 10.00.00"), &later), &mut mint)
        .expect("the file is read");
    assert_eq!((second.matched, second.added.len()), (2, 3), "{second:?}");
    assert!(second.unsure.is_empty(), "{second:?}");
    let after = by_text(&table);
    assert_eq!(after.len(), 5);
    for (text, id) in &before {
        assert_eq!(after.get(text), Some(id), "{text}");
    }
    let alice = table.transactions().find(|t| t.text.as_deref() == Some("MobilePay Alice")).expect("it's there");
    assert_eq!(alice.sources.len(), 2);
}

/// Lunar's fetched data has the bank's reference and no running balance, and
/// its exports have both. The export's row is the fetched transaction: it
/// keeps its ID, and gains the balance and the time of day.
#[test]
fn a_fetched_transaction_keeps_its_id_and_gains_what_an_export_adds() {
    let config = config(LUNAR);
    let (mut table, mut mint) = (Table::default(), ids());
    let fetch = fetched("r1", "2026-01-05", "50.00", "DKK", "Groceries");
    table.read("lunar:b", &rows(&config, "lunar:b", "lunar/555/eb.json", &fetch), &mut mint).expect("the file is read");
    let id = table.transactions().next().expect("one transaction").id.clone();
    assert_eq!(table.get(&id).and_then(|t| t.balance), None);

    let export = format!("{LUNAR_HEADER}05.01.2026,09.00,Groceries,\"-50,00\",\"100,00\",r1\n");
    let reading = table
        .read("lunar:b", &rows(&config, "lunar:b", "lunar/555/statement.csv", &export), &mut mint)
        .expect("the file is read");
    assert_eq!(reading, Reading { matched: 1, ..Reading::default() });
    assert_eq!(table.len(), 1);
    let stored = table.get(&id).expect("the same ID");
    assert_eq!(stored.balance, Some(Decimal::from(100)));
    assert_eq!(stored.text.as_deref(), Some("Groceries"));
    assert!(stored.time.is_some());
    assert_eq!(stored.sources.len(), 2);
    // The time of day and the two places it was read from are in the text too.
    let text = table.write().expect("it can be written");
    assert!(text.contains(",09:00:00,") && text.contains("lunar/555/eb.json:1|lunar/555/statement.csv:2"), "{text}");
    assert_eq!(Table::parse(&text).expect("it reads back"), table);
}

/// Two rows equal in everything are two transactions when a file has both,
/// and it makes no difference which of them is the one the table had.
#[test]
fn rows_equal_in_everything_are_matched_one_for_one() {
    let config = config(NORDEA);
    let (mut table, mut mint) = (Table::default(), ids());
    let coffee = ("2026/01/05", "-20,00", "Coffee", "100,00");
    let refund = ("2026/01/05", "20,00", "Refund", "120,00");
    let one = nordea_csv(&[coffee]);
    table
        .read("bank:a", &rows(&config, "bank:a", &nordea_rel("2026-01-05 10.00.00"), &one), &mut mint)
        .expect("the file is read");
    let two = nordea_csv(&[coffee, refund, coffee]);
    let reading = table
        .read("bank:a", &rows(&config, "bank:a", &nordea_rel("2026-01-06 10.00.00"), &two), &mut mint)
        .expect("the file is read");
    assert_eq!((reading.matched, reading.added.len()), (1, 2), "{reading:?}");
    assert!(reading.unsure.is_empty(), "{reading:?}");
    assert_eq!(table.transactions().filter(|t| t.text.as_deref() == Some("Coffee")).count(), 2);
}

/// Reading a file that was read before changes nothing.
#[test]
fn reading_a_file_again_changes_nothing() {
    let config = config(NORDEA);
    let (mut table, mut mint) = (Table::default(), ids());
    let rows = rows(&config, "bank:a", &nordea_rel("2026-01-15 10.00.00"), &nordea_csv(&DAY));
    table.read("bank:a", &rows, &mut mint).expect("the file is read");
    let once = table.clone();
    let again = table.read("bank:a", &rows, &mut mint).expect("the file is read");
    assert_eq!(again, Reading { matched: 5, ..Reading::default() });
    assert_eq!(table, once);
}

/// Nordea's fetched data sometimes words a transaction differently from its
/// export, and the export has no reference to go by. Then we aren't sure:
/// the row is added, marked as possibly the one we had, and nothing of the
/// two is mixed. Joining them keeps the lower ID, which takes over the
/// reference, and the other ID still means that transaction.
#[test]
fn a_copy_we_arent_sure_of_is_added_and_marked_and_can_be_joined() {
    let both = NORDEA.replace("formats = [\"nordea-csv\"]", "formats = [\"nordea-csv\", \"enable-banking\"]");
    let config = config(&both);
    let (mut table, mut mint) = (Table::default(), ids());
    let export = nordea_csv(&[("2026/01/05", "-50,00", "Groceries", "100,00")]);
    table
        .read("bank:a", &rows(&config, "bank:a", &nordea_rel("2026-01-06 10.00.00"), &export), &mut mint)
        .expect("the file is read");
    let had = table.transactions().next().expect("one transaction").id.clone();

    let fetch = r#"{"acct_fetch": 1, "fetched_at": "2026-01-16T09:00:00Z", "pages": [{"transactions": [
      {"entry_reference": "r1", "booking_date": "2026-01-05", "status": "BOOK", "credit_debit_indicator": "DBIT",
       "transaction_amount": {"currency": "DKK", "amount": "50.00"}, "remittance_information": ["GROCERIES APS"],
       "balance_after_transaction": {"currency": "DKK", "amount": "100.00"}}]}]}"#;
    let reading = table
        .read("bank:a", &rows(&config, "bank:a", "nordea/1234567890/eb.json", fetch), &mut mint)
        .expect("the file is read");
    assert_eq!(reading.matched, 0);
    let [(new, other)] = reading.unsure.as_slice() else { panic!("{reading:?}") };
    assert_eq!(other, &had);
    assert_eq!(table.get(new).and_then(|t| t.maybe_same_as.clone()), Some(had.clone()));
    assert_eq!(table.get(&had).and_then(|t| t.reference.clone()), None);
    assert_eq!(table.transactions().count(), 2);

    table.join(new, &had).expect("they can be one transaction");
    assert_eq!(table.transactions().count(), 1);
    let kept = table.resolve(new).expect("the ID still means a transaction");
    assert_eq!(kept.id, had);
    assert_eq!(kept.reference.as_deref(), Some("r1"));
    assert_eq!(kept.text.as_deref(), Some("Groceries"));
    assert_eq!(table.get(new).and_then(|t| t.same_as.clone()), Some(had.clone()));
    assert_eq!(table.get(new).and_then(|t| t.maybe_same_as.clone()), None);
    // The fetch read again is the transaction it was joined to.
    let again = table
        .read("bank:a", &rows(&config, "bank:a", "nordea/1234567890/eb.json", fetch), &mut mint)
        .expect("the file is read");
    assert_eq!(again, Reading { matched: 1, ..Reading::default() });

    // Two transactions of different amounts can't be joined.
    let coffee = nordea_csv(&[("2026/01/06", "-20,00", "Coffee", "80,00")]);
    let reading = table
        .read("bank:a", &rows(&config, "bank:a", &nordea_rel("2026-01-07 10.00.00"), &coffee), &mut mint)
        .expect("the file is read");
    let refused = table.join(&reading.added[0], &had).expect_err("two transactions");
    let both = "can't be one transaction: bank:a 2026-01-05 -50 \"Groceries\" / bank:a 2026-01-06 -20 \"Coffee\"";
    assert!(refused.to_string().contains(both), "{refused}");
}

/// What the table has is never overwritten. A copy that says something else
/// about a field both have fails the read, naming each such field, and the
/// table is left as it was: nothing of the file is taken in.
#[test]
fn a_copy_that_says_something_else_fails_the_read() {
    let config = config(LUNAR);
    let (mut table, mut mint) = (Table::default(), ids());
    let fetch = fetched("r1", "2026-01-05", "50.00", "EUR", "GROCERIES APS");
    table.read("lunar:b", &rows(&config, "lunar:b", "lunar/555/eb.json", &fetch), &mut mint).expect("the file is read");
    let before = table.clone();
    // The same reference, day and amount, in other words and another
    // currency, and a second row that would have been new.
    let export = format!(
        "{LUNAR_HEADER}06.01.2026,09.00,Coffee,\"-20,00\",\"80,00\",r2\n\
         05.01.2026,09.00,Groceries,\"-50,00\",\"100,00\",r1\n"
    );
    let failed = table
        .read("lunar:b", &rows(&config, "lunar:b", "lunar/555/statement.csv", &export), &mut mint)
        .expect_err("the copies disagree");
    assert_eq!(
        failed.to_string(),
        "2 copies say something else than the table, which was left as it was:\n  \
         lunar:b 2026-01-05 -50 \"GROCERIES APS\": the currency is \"EUR\" in the table and \"DKK\" in lunar/555/statement.csv\n  \
         lunar:b 2026-01-05 -50 \"GROCERIES APS\": the text is \"GROCERIES APS\" in the table and \"Groceries\" in lunar/555/statement.csv"
    );
    assert_eq!(table, before);
}

/// The table is text: a heading, then one line per transaction, sorted by
/// ID, with the columns a person reads first. It reads back as it was
/// written, text with commas and quotes in it included.
#[test]
fn the_table_reads_back_as_it_was_written() {
    let config = config(NORDEA);
    let (mut table, mut mint) = (Table::default(), ids());
    let export = nordea_csv(&[
        ("2026/01/06", "-20,00", "Café \"Den Gamle\", Aarhus", "80,00"),
        ("2026/01/05", "-50,00", "Groceries", "100,00"),
    ]);
    table
        .read("bank:a", &rows(&config, "bank:a", &nordea_rel("2026-01-07 10.00.00"), &export), &mut mint)
        .expect("the file is read");
    let text = table.write().expect("it can be written");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[0],
        "id,account,date,amount,currency,text,counterparty_name,counterparty,balance,reference,time,value_date,\
         transaction_date,original_amount,original_currency,same_as,maybe_same_as,sources"
    );
    assert!(
        lines[1].starts_with("01010101-0101-4101-8101-010101010101,bank:a,2026-01-05,-50,DKK,Groceries,,,100,"),
        "{text}"
    );
    assert!(lines[1].ends_with(",nordea/1234567890/Konto 1234567890 - 2026-01-07 10.00.00.csv:3"), "{text}");
    assert!(lines[2].starts_with("02020202-"), "{text}");
    let back = Table::parse(&text).expect("it reads back");
    assert_eq!(back, table);
    assert_eq!(back.write().expect("it can be written"), text);
}

/// A table that isn't one is refused, with the line that's wrong.
#[test]
fn a_table_that_isnt_one_is_refused() {
    let config = config(NORDEA);
    let (mut table, mut mint) = (Table::default(), ids());
    table
        .read("bank:a", &rows(&config, "bank:a", &nordea_rel("2026-01-15 10.00.00"), &nordea_csv(&DAY)), &mut mint)
        .expect("the file is read");
    let text = table.write().expect("it can be written");
    let refused = |text: &str| format!("{:#}", Table::parse(text).expect_err("it isn't a table"));

    assert!(refused(&text.replacen("id,account", "account,id", 1)).contains("the table's columns are"));
    let line = text.lines().nth(1).expect("a row");
    assert!(
        refused(&format!("{text}{line}\n"))
            .contains("line 7 of the table: 01010101-0101-4101-8101-010101010101 is there twice")
    );
    assert!(refused(&text.replacen("2026-01-05", "5 January", 1)).contains("\"5 January\" isn't a date"));
    assert!(refused(&text.replacen(",bank:a,", ",,", 1)).contains("a transaction has no account"));
    assert!(refused(&text.replacen("01010101-", "0101010-", 1)).contains("isn't a transaction ID"));
    // An ID is 36 characters, neither more nor fewer, and in lowercase.
    for not_an_id in [
        "01010101-0101-4101-8101-0101010101011",
        "01010101-0101-4101-8101-01010101010",
        "01010101-0101-4101-8101-01010101010A",
        "",
    ] {
        assert!(TransactionId::parse(not_an_id).is_err(), "{not_an_id}");
    }
    assert_eq!(TransactionId::parse("01010101-0101-4101-8101-010101010101").expect("an ID").as_str(), id(1).as_str());
    // Whatever the 16 bytes, the ID says it's a random UUID.
    assert_eq!(TransactionId::from_random([0xff; 16]).as_str(), "ffffffff-ffff-4fff-bfff-ffffffffffff");
    let pointing_nowhere = text.replacen(",,nordea/", ",09090909-0909-4909-8909-090909090909,nordea/", 1);
    assert!(refused(&pointing_nowhere).contains("which isn't another transaction"), "{}", refused(&pointing_nowhere));
}

const HEADING: &str = "id,account,date,amount,currency,text,counterparty_name,counterparty,balance,reference,time,\
                       value_date,transaction_date,original_amount,original_currency,same_as,maybe_same_as,sources";

fn id(n: u8) -> TransactionId {
    TransactionId::from_random([n; 16])
}

/// A line of the table, written by hand: groceries on the 5th, with the
/// columns in `changes` set to something else.
fn line(n: u8, changes: &[(&str, &str)]) -> String {
    let columns: Vec<&str> = HEADING.split(',').collect();
    let mut cells: Vec<String> = columns
        .iter()
        .map(|column| match *column {
            "id" => id(n).to_string(),
            "account" => "bank:a".to_owned(),
            "date" => "2026-01-05".to_owned(),
            "amount" => "-50".to_owned(),
            "currency" => "DKK".to_owned(),
            "text" => "Groceries".to_owned(),
            "balance" => "100".to_owned(),
            _ => String::new(),
        })
        .collect();
    for (column, value) in changes {
        let i = columns.iter().position(|c| c == column).expect("one of the table's columns");
        (*value).clone_into(&mut cells[i]);
    }
    format!("{}\n", cells.join(","))
}

fn table(lines: &[String]) -> Table {
    Table::parse(&format!("{HEADING}\n{}", lines.concat())).expect("the test's table")
}

/// Two rows are one transaction only if they're of one account, day, amount
/// and currency, and their balances and references don't say otherwise.
/// What they read may differ.
#[test]
fn only_rows_that_can_be_one_transaction_are_joined() {
    let two: [(&str, &str); 6] = [
        ("account", "bank:b"),
        ("date", "2026-01-06"),
        ("amount", "-51"),
        ("balance", "99"),
        ("currency", "EUR"),
        ("reference", "r2"),
    ];
    for change in two {
        let mut both = table(&[line(1, &[("reference", "r1")]), line(2, &[("reference", "r1"), change])]);
        let before = both.clone();
        let refused = both.join(&id(1), &id(2)).expect_err("two transactions");
        assert!(refused.to_string().contains("can't be one transaction"), "{change:?}: {refused}");
        assert_eq!(both, before, "{change:?}");
    }
    // Worded differently, and only one of them has the reference.
    for (a, b) in [(1, 2), (2, 1)] {
        let mut both = table(&[
            line(1, &[("maybe_same_as", id(2).as_str())]),
            line(2, &[("text", "GROCERIES APS"), ("reference", "r1")]),
        ]);
        both.join(&id(a), &id(b)).expect("one transaction");
        let kept = both.resolve(&id(2)).expect("the ID still means it");
        assert_eq!(
            (kept.id.clone(), kept.text.as_deref(), kept.reference.as_deref()),
            (id(1), Some("Groceries"), Some("r1"))
        );
        assert_eq!(kept.maybe_same_as, None);
        assert_eq!(both.transactions().count(), 1);
        assert_eq!((both.len(), both.is_empty()), (2, false));
        // Joining them again, or a row with itself, changes nothing.
        let joined = both.clone();
        both.join(&id(b), &id(a)).expect("already one");
        both.join(&id(1), &id(1)).expect("one row");
        assert_eq!(both, joined);
    }
    let mut one = table(&[line(1, &[])]);
    let unknown = one.join(&id(1), &id(9)).expect_err("no such row");
    assert!(unknown.to_string().contains("the table has no transaction 09090909-"), "{unknown}");
    assert!(Table::default().is_empty());
}

/// `same_as` has to lead to a transaction: a row can't be the same as
/// itself, directly or by way of others.
#[test]
fn rows_that_are_the_same_as_each_other_are_refused() {
    let refused = |lines: &[String]| {
        format!("{:#}", Table::parse(&format!("{HEADING}\n{}", lines.concat())).expect_err("a ring"))
    };
    let own = id(1).to_string();
    assert!(refused(&[line(1, &[("same_as", &own)])]).contains("which isn't another transaction"));
    let (one, two) = (id(1).to_string(), id(2).to_string());
    let ring = [line(1, &[("same_as", &two)]), line(2, &[("same_as", &one)])];
    assert!(refused(&ring).contains("is said to be the same as rows that lead back to it"), "{}", refused(&ring));
    // A chain that ends is fine, and every ID in it means the last row.
    let three = id(3).to_string();
    let chain = table(&[line(1, &[("same_as", &two)]), line(2, &[("same_as", &three)]), line(3, &[])]);
    assert_eq!(chain.resolve(&id(1)).map(|t| t.id.clone()), Some(id(3)));
    assert_eq!(chain.transactions().count(), 1);
}

/// Fetched transactions, each (reference, date, amount paid, text, running balance).
fn fetch_of(rows: &[(&str, &str, &str, &str, Option<&str>)]) -> String {
    let rows: Vec<String> = rows
        .iter()
        .map(|(reference, date, amount, text, balance)| {
            let balance = balance.map_or_else(String::new, |b| {
                format!(r#", "balance_after_transaction": {{"currency": "DKK", "amount": "{b}"}}"#)
            });
            format!(
                r#"{{"entry_reference": "{reference}", "booking_date": "{date}", "status": "BOOK", "credit_debit_indicator": "DBIT", "transaction_amount": {{"currency": "DKK", "amount": "{amount}"}}, "remittance_information": ["{text}"]{balance}}}"#
            )
        })
        .collect();
    format!(
        r#"{{"acct_fetch": 1, "fetched_at": "2026-01-16T09:00:00Z", "pages": [{{"transactions": [{}]}}]}}"#,
        rows.join(", ")
    )
}

/// A reference that two transactions of the table have doesn't say which of
/// them a row is, so the row is added and marked.
#[test]
fn a_reference_that_two_transactions_have_settles_nothing() {
    let config = config(LUNAR);
    let (mut table, mut mint) = (Table::default(), ids());
    let twice =
        fetch_of(&[("r1", "2026-01-05", "50.00", "Groceries", None), ("r1", "2026-01-05", "50.00", "Groceries", None)]);
    let first = table
        .read("lunar:b", &rows(&config, "lunar:b", "lunar/555/eb-1.json", &twice), &mut mint)
        .expect("the file is read");
    assert_eq!((first.matched, first.added.len(), first.unsure.len()), (0, 2, 0), "{first:?}");
    let once = fetch_of(&[("r1", "2026-01-05", "50.00", "Groceries", None)]);
    let second = table
        .read("lunar:b", &rows(&config, "lunar:b", "lunar/555/eb-2.json", &once), &mut mint)
        .expect("the file is read");
    assert_eq!((second.matched, second.added.len(), second.unsure.len()), (0, 1, 1), "{second:?}");
    assert_eq!(table.len(), 3);
}

/// A row that may be one of two transactions is marked as the one with its
/// reference, before one that only looks like it. And one transaction isn't
/// the possible twin of two rows of a file.
#[test]
fn a_row_we_arent_sure_of_points_at_the_likeliest_transaction() {
    let both = NORDEA.replace("formats = [\"nordea-csv\"]", "formats = [\"nordea-csv\", \"enable-banking\"]");
    let config = config(&both);
    let (mut table, mut mint) = (Table::default(), ids());
    // An export's row first, then a fetched one that the bank later moves a day.
    let export = nordea_csv(&[("2026/01/06", "-55,00", "Shop", "45,00")]);
    table
        .read("bank:a", &rows(&config, "bank:a", &nordea_rel("2026-01-07 10.00.00"), &export), &mut mint)
        .expect("the file is read");
    let fetch = fetch_of(&[("r1", "2026-01-05", "50.00", "Groceries", Some("100.00"))]);
    table
        .read("bank:a", &rows(&config, "bank:a", "nordea/1234567890/eb-1.json", &fetch), &mut mint)
        .expect("the file is read");
    let (shop, groceries) = (id(1), id(2));
    assert_eq!(table.get(&groceries).and_then(|t| t.reference.as_deref()), Some("r1"));

    // The same reference on the 6th, for the shop's amount and balance, and
    // a second row that looks the same.
    let moved = fetch_of(&[
        ("r1", "2026-01-06", "55.00", "Other", Some("45.00")),
        ("r9", "2026-01-06", "55.00", "Another", Some("45.00")),
    ]);
    let reading = table
        .read("bank:a", &rows(&config, "bank:a", "nordea/1234567890/eb-2.json", &moved), &mut mint)
        .expect("the file is read");
    assert_eq!(reading.matched, 0, "{reading:?}");
    assert_eq!(reading.unsure, [(id(3), groceries), (id(4), shop)], "{reading:?}");

    // Two rows of one file that both look like the shop: only one of them
    // can be it.
    let (mut table, mut mint) = (Table::default(), ids());
    table
        .read("bank:a", &rows(&config, "bank:a", &nordea_rel("2026-01-07 10.00.00"), &export), &mut mint)
        .expect("the file is read");
    let two = fetch_of(&[
        ("r9", "2026-01-06", "55.00", "Another", Some("45.00")),
        ("r8", "2026-01-06", "55.00", "A third", Some("45.00")),
    ]);
    let reading = table
        .read("bank:a", &rows(&config, "bank:a", "nordea/1234567890/eb-3.json", &two), &mut mint)
        .expect("the file is read");
    assert_eq!((reading.matched, reading.added.len()), (0, 2), "{reading:?}");
    assert_eq!(reading.unsure, [(id(2), id(1))], "{reading:?}");
}

/// An ID that a row already has isn't given to another.
#[test]
fn an_id_that_is_taken_isnt_given_again() {
    let config = config(NORDEA);
    let mut table = Table::default();
    let mut given = [1, 1, 1, 2].into_iter();
    let mut mint = || id(given.next().expect("enough IDs"));
    let reading = table
        .read("bank:a", &rows(&config, "bank:a", &nordea_rel("2026-01-15 10.00.00"), &nordea_csv(&DAY[3..])), &mut mint)
        .expect("the file is read");
    assert_eq!(reading.added, [id(1), id(2)]);
    assert_eq!(table.len(), 2);
}
