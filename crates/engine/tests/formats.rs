//! Reading the banks' files: what each field of a row becomes, which files
//! are refused, and which format a file is taken for (docs/spec.md §3, §4).

mod common;

use common::{NORDEA, build, config, nordea_csv, nordea_rel};
use jiff::civil::{Time, date};
use konter_engine::config::Format;
use konter_engine::rules::Rules;
use konter_engine::sources::{self, FileContext, enable_banking, lunar_csv, nordea_csv as nordea};
use rust_decimal::Decimal;

/// A file of account 1234567890, in kroner.
fn ctx(file_name: &str) -> FileContext {
    FileContext {
        source: "bank:a".into(),
        number: "1234567890".into(),
        currency: "DKK".into(),
        rel: format!("nordea/1234567890/{file_name}"),
        file_name: file_name.into(),
    }
}

const NORDEA_HEADER: &str = "Bogføringsdato;Beløb;Afsender;Modtager;Navn;Beskrivelse;Saldo;Valuta;Afstemt;";

#[test]
fn a_nordea_filename_gives_its_account_number_only_when_it_is_one() {
    assert_eq!(nordea::number_from_filename("Konto abc - 2026-01-31 10.00.00.csv"), None);
    assert_eq!(nordea::number_from_filename(" - 2026-01-31 10.00.00.csv"), None);
}

#[test]
fn a_nordea_row_gives_the_other_party_and_the_currency() {
    let text = format!(
        "{NORDEA_HEADER}\n\
         2026/01/08;-10,00;;DK99;;Kiosk;1040,00;;;\n\
         2026/01/07;40,00;DK77;;;Gift;1050,00;;;\n\
         2026/01/06;60,00;DK77;1234567890;;Refund;1010,00;;;\n\
         2026/01/05;-50,00;1234567890;DK99;Shop A/S;Shop;950,00;EUR;;\n"
    );
    let rows = nordea::parse(&text, &ctx("Konto 1234567890 - 2026-01-31 10.00.00.csv")).expect("the export parses");
    let parties: Vec<Option<&str>> = rows.iter().map(|t| t.counterparty.as_deref()).collect();
    // The account that isn't this one, whichever side it's on, and also when
    // the bank leaves this one out.
    assert_eq!(parties, [Some("DK99"), Some("DK77"), Some("DK77"), Some("DK99")]);
    let shop = &rows[3];
    assert_eq!(shop.counterparty_name.as_deref(), Some("Shop A/S"));
    assert_eq!(shop.currency, "EUR");
    // Without a name or a currency: no name, and the account's currency.
    assert_eq!((rows[2].counterparty_name.as_deref(), rows[2].currency.as_str()), (None, "DKK"));
}

#[test]
fn a_nordea_file_that_isnt_this_accounts_export_is_refused() {
    let export = ctx("Konto 1234567890 - 2026-01-31 10.00.00.csv");
    let refused = |text: &str, expected: &str| {
        let error = format!("{:#}", nordea::parse(text, &export).expect_err("the file is refused"));
        assert!(error.contains(expected), "{error}");
    };
    // Between two other accounts.
    refused(&format!("{NORDEA_HEADER}\n2026/01/05;-50,00;DK77;DK99;;Shop;950,00;DKK;;\n"), "neither sender");
    // Other columns, or too few.
    refused(&format!("{}\n", NORDEA_HEADER.replace("Saldo", "Balance")), "unexpected header");
    refused("Bogføringsdato;Beløb\n", "unexpected header");
    // The columns are enough, with or without the empty one the bank ends the line with.
    let bare = NORDEA_HEADER.trim_end_matches(';');
    let rows = nordea::parse(&format!("{bare}\n2026/01/05;-50,00;;;;Shop;950,00;DKK;\n"), &export);
    assert_eq!(rows.expect("the export parses").len(), 1);
}

#[test]
fn a_lunar_row_gives_its_time_its_reference_and_its_place_in_the_file() {
    let text = "Date,Time,Title,Amount,Balance,Transaction ID\n\
                29.01.2026,09.05,Top up,\"500,00\",\"520,00\",abc-1\n\
                10.01.2026,12.34,Coffee Shop,\"-30,00\",\"20,00\",\n";
    let rows = lunar_csv::parse(text, &ctx("statement.csv")).expect("the statement parses");
    assert_eq!(rows[0].time, Some(Time::constant(9, 5, 0, 0)));
    assert_eq!(rows[1].time, Some(Time::constant(12, 34, 0, 0)));
    assert_eq!((rows[0].bank_ref.as_deref(), rows[1].bank_ref.as_deref()), (Some("abc-1"), None));
    // The file is newest first, and a lower hint is earlier.
    assert!(rows[1].order_hint < rows[0].order_hint, "{} and {}", rows[1].order_hint, rows[0].order_hint);
}

/// A fetch with four booked transactions.
const FETCH: &str = r#"{
  "acct_fetch": 1,
  "fetched_at": "2026-01-31T09:14:03Z",
  "pages": [{"transactions": [
    {"entry_reference": "A1", "booking_date": "2026-01-05",
     "transaction_amount": {"currency": "DKK", "amount": "-5.00"}, "credit_debit_indicator": "CRDT",
     "debtor": {"name": "Payer"}, "creditor": {"name": "Payee"},
     "remittance_information": ["Refund", "REF 123"],
     "charges": [{"amount": "1"}, {"amount": "2"}],
     "mixed": ["x", {"k": "v"}]},
    {"entry_reference": "0", "booking_date": "2026-01-06",
     "transaction_amount": {"currency": "DKK", "amount": "0.00"},
     "debtor": {"name": "Payer"}, "creditor": {"name": "Payee"}},
    {"entry_reference": "A3", "booking_date": "2026-01-07",
     "transaction_amount": {"currency": "DKK", "amount": "20.00"}, "credit_debit_indicator": "DBIT",
     "exchange_rate": {"instructed_amount": {"currency": "DKK", "amount": "20.00"}}},
    {"entry_reference": "A4", "booking_date": "2026-01-08",
     "transaction_amount": {"currency": "DKK", "amount": "149.20"}, "credit_debit_indicator": "DBIT",
     "exchange_rate": {"instructed_amount": {"currency": "EUR", "amount": "20.00"}}}
  ]}]
}"#;

#[test]
fn an_enable_banking_transaction_gives_its_sign_its_party_and_its_original_amount() {
    let (rows, _) = enable_banking::parse(FETCH, &ctx("fetch.json")).expect("the fetch parses");
    // The indicator decides the sign, whatever sign the amount is written with.
    let amounts: Vec<String> = rows.iter().map(|t| t.amount.to_string()).collect();
    assert_eq!(amounts, ["5.00", "0.00", "-20.00", "-149.20"]);
    // Money that comes in comes from the debtor, and so does none at all.
    assert_eq!(rows[0].counterparty_name.as_deref(), Some("Payer"));
    assert_eq!(rows[1].counterparty_name.as_deref(), Some("Payer"));
    // A reference of "0" is none.
    let references: Vec<Option<&str>> = rows.iter().map(|t| t.bank_ref.as_deref()).collect();
    assert_eq!(references, [Some("A1"), None, Some("A3"), Some("A4")]);
    // An original amount is one in another currency.
    assert_eq!((rows[2].original_amount, rows[2].original_currency.as_deref()), (None, None));
    assert_eq!(
        (rows[3].original_amount, rows[3].original_currency.as_deref()),
        (Some(Decimal::from(-20)), Some("EUR"))
    );
    // Each is later in the file than the one before.
    let hints: Vec<i64> = rows.iter().map(|t| t.order_hint).collect();
    assert_eq!(hints, [0, 1, 2, 3]);
    assert_eq!(enable_banking::fetched_date(FETCH), Some(date(2026, 1, 31)));
    assert_eq!(enable_banking::fetched_date("not a fetch"), None);
}

#[test]
fn a_file_is_taken_for_the_format_it_is_in() {
    let lunar = "Date,Time,Title,Amount,Balance,Transaction ID\n";
    assert_eq!(sources::detect_format("statement.csv", lunar).expect("a Lunar statement"), Format::LunarCsv);
    assert_eq!(sources::detect_format("export.csv", NORDEA_HEADER).expect("a Nordea export"), Format::NordeaCsv);
    assert_eq!(sources::detect_format("fetch.json", FETCH).expect("a fetch"), Format::EnableBanking);
    // A fetch is a JSON file that says so: neither is enough alone.
    let mentions = format!("{NORDEA_HEADER}\n2026/01/05;-50,00;;;;\"acct_fetch\";950,00;DKK;;\n");
    assert_eq!(sources::detect_format("export.csv", &mentions).expect("a Nordea export"), Format::NordeaCsv);
    sources::detect_format("other.json", "{}").expect_err("JSON that isn't a fetch");
    sources::detect_format("notes.csv", "Dear diary").expect_err("no format at all");
}

#[test]
fn files_and_transactions_are_found_by_where_they_belong() {
    let config =
        config(&format!("{NORDEA}\n[sources.\"bank:savings\"]\nbank = \"nordea\"\nledger = \"assets:bank:savings\"\n"));
    assert_eq!(config.source_for_dir("nordea/1234567890"), Some("bank:a"));
    assert_eq!(config.source_for_dir("nordea/999"), None);
    assert_eq!(config.source_for_dir(""), None);

    // A file in no imported account's folder belongs to nobody. A balance
    // sidecar isn't a raw file.
    let rels: Vec<String> =
        ["nordea/1234567890/a.csv", "nordea/999/b.csv", "nordea/999/b.csv.balance.toml", "stray.csv"]
            .map(str::to_string)
            .to_vec();
    assert_eq!(sources::orphans(&config, &rels), ["nordea/999/b.csv", "stray.csv"]);

    let rules = Rules::load_str("", "config/rules.toml", &config).expect("no rules load");
    let csv = nordea_csv(&[("2026/01/06", "-25,00", "Kiosk", "925,00"), ("2026/01/05", "-50,00", "Shop", "950,00")]);
    let rel = nordea_rel("2026-01-31 10.00.00");
    let build = build(&config, rules, &[], &[("bank:a", &rel, &csv)], date(2026, 1, 31));
    let kiosk = &build.loaded.sources["bank:a"].txs[1];
    assert_eq!(build.loaded.find(&kiosk.id).map(|t| t.description.as_str()), Some("Kiosk"));
    assert!(build.loaded.find("00000000-0000-4000-8000-ffffffffffff").is_none());
}
