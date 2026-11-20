//! `konter suggest`: proposes pattern rules for groups of unmatched transactions
//! (docs/spec.md §10). Suggestions are printed for you to review and paste;
//! they never take part in `konter build`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use jiff::civil::Date;
use rust_decimal::Decimal;

use konter_engine::dates::days_between;
use konter_engine::dates::parse_dmy_dot;
use konter_engine::engine::Status;
use konter_engine::model::{BankTransaction, name_instead, normalize};
use konter_engine::money::{fmt_num, parse_plain};
use konter_engine::pipeline::Build;

/// A rule name from a description: lowercase letters, digits and dashes.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.trim_matches('-').chars().take(40).collect();
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "unnamed".into() } else { out }
}

fn common_prefix(items: &[&str]) -> String {
    let Some(first) = items.first() else { return String::new() };
    let mut len = first.chars().count();
    for s in &items[1..] {
        len = first.char_indices().zip(s.chars()).take_while(|((_, a), b)| a == b).count().min(len);
    }
    first.chars().take(len).collect()
}

fn toml_str(s: &str) -> String {
    // Literal strings ('…') need no escaping unless they contain a quote.
    if s.contains('\'') || s.contains('\n') { format!("{s:?}") } else { format!("'{s}'") }
}

/// What groups similar transactions within a source: the counterparty where
/// there is one, then the normalized description, then the normalized
/// counterparty name.
pub fn group_key(t: &BankTransaction) -> String {
    match (&t.counterparty, name_instead(t)) {
        (Some(cp), _) => format!("counterparty {cp}"),
        (None, Some(name)) => format!("counterparty name {}", normalize(name)),
        (None, None) => normalize(&t.description),
    }
}

/// (date, amount) → "Main category / Category" from the text of the
/// categorized reference export (data/reference/transactions.csv).
pub fn reference_categories(text: &str) -> BTreeMap<(Date, Decimal), String> {
    let mut out = BTreeMap::new();
    let mut reader = csv::ReaderBuilder::new().delimiter(b';').from_reader(text.as_bytes());
    let Ok(headers) = reader.headers().cloned() else { return out };
    let col = |name: &str| headers.iter().position(|h| h == name);
    let (Some(date), Some(amount), Some(main), Some(category)) =
        (col("Date"), col("Amount"), col("MainCategoryName"), col("CategoryName"))
    else {
        return out;
    };
    for record in reader.records().flatten() {
        let (Some(d), Some(a)) = (record.get(date), record.get(amount)) else { continue };
        let (Ok(d), Ok(a)) = (parse_dmy_dot(&d.replace('-', ".")), parse_plain(a)) else { continue };
        let label = format!("{} / {}", record.get(main).unwrap_or(""), record.get(category).unwrap_or(""));
        if label.trim() != "/" {
            out.insert((d, a), label);
        }
    }
    out
}

/// The reference export's most common category among `txs`, if it has any of them.
pub fn category_hint<'a>(
    categories: &'a BTreeMap<(Date, Decimal), String>,
    txs: &[&BankTransaction],
) -> Option<&'a str> {
    let mut hints: BTreeMap<&str, usize> = BTreeMap::new();
    for t in txs {
        if let Some(c) = categories.get(&(t.booked, t.amount)) {
            *hints.entry(c.as_str()).or_default() += 1;
        }
    }
    hints.into_iter().max_by_key(|(_, n)| *n).map(|(hint, _)| hint)
}

/// Whether amounts are identical and dates roughly a month apart.
fn looks_monthly(txs: &[&BankTransaction]) -> Option<Decimal> {
    if txs.len() < 3 || txs.iter().any(|t| t.amount != txs[0].amount) {
        return None;
    }
    // Allow one irregular gap (e.g. two payments in one month).
    let gaps = txs.len() - 1;
    let monthly = txs.windows(2).filter(|w| (25..=35).contains(&days_between(w[0].booked, w[1].booked))).count();
    (monthly + 1 >= gaps && monthly >= 2).then_some(txs[0].amount)
}

/// `categories`: from the reference export, see [`reference_categories`].
pub fn suggest(
    build: &Build,
    categories: &BTreeMap<(Date, Decimal), String>,
    only: Option<&str>,
    min: usize,
) -> (String, usize) {
    let mut groups: BTreeMap<(String, String), Vec<&BankTransaction>> = BTreeMap::new();
    for t in build.loaded.transactions() {
        if only.is_some_and(|s| s != t.source) || build.generated.outcomes[&t.id].status != Status::Unmatched {
            continue;
        }
        groups.entry((t.source.clone(), group_key(t))).or_default().push(t);
    }
    let mut groups: Vec<_> = groups.into_iter().filter(|(_, txs)| txs.len() >= min).collect();
    groups.sort_by_key(|(_, txs)| {
        (std::cmp::Reverse(txs.len()), std::cmp::Reverse(txs.iter().map(|t| t.amount.abs()).sum::<Decimal>()))
    });

    let mut names: Vec<String> = build.rules.patterns.iter().map(|r| r.name.clone()).collect();
    let mut out = String::new();
    for ((source, _), txs) in &groups {
        let descriptions: Vec<&str> = txs.iter().map(|t| t.description.as_str()).collect();
        let all_same = descriptions.iter().all(|d| *d == descriptions[0]);
        // Grouped by counterparty name (see `group_key`): every one has a name.
        let party_names: Vec<&str> = txs.iter().filter_map(|t| name_instead(t)).collect();
        let by_name = txs[0].counterparty.is_none() && party_names.len() == txs.len();
        let condition = if all_same {
            format!("description = {}", toml_str(descriptions[0]))
        } else if let Some(cp) =
            txs[0].counterparty.as_ref().filter(|_| txs.iter().all(|t| t.counterparty == txs[0].counterparty))
        {
            format!("counterparty = {}", toml_str(cp))
        } else if by_name && party_names.iter().all(|n| *n == party_names[0]) {
            format!("counterparty_name = {}", toml_str(party_names[0]))
        } else if by_name {
            let prefix = common_prefix(&party_names);
            format!("counterparty_name_re = {}", toml_str(&format!("^{}", regex::escape(prefix.trim_end()))))
        } else {
            let prefix = common_prefix(&descriptions);
            format!("description_re = {}", toml_str(&format!("^{}", regex::escape(prefix.trim_end()))))
        };
        let shown = if by_name { &party_names } else { &descriptions };
        let mut name = slug(&normalize(shown[0]));
        let base = name.clone();
        let mut n = 2;
        while names.contains(&name) {
            name = format!("{base}-{n}");
            n += 1;
        }
        names.push(name.clone());

        let total: Decimal = txs.iter().map(|t| t.amount).sum();
        let _ = writeln!(
            out,
            "# {} × {source}, {} … {}, total {}",
            txs.len(),
            txs[0].booked,
            txs[txs.len() - 1].booked,
            fmt_num(total)
        );
        if by_name {
            if shown.iter().any(|n| *n != shown[0]) {
                let _ = writeln!(out, "#   counterparty names, e.g. {:?}, {:?}", shown[0], shown[shown.len() - 1]);
            }
        } else if !all_same {
            let _ = writeln!(out, "#   e.g. {:?}, {:?}", descriptions[0], descriptions[descriptions.len() - 1]);
        }
        if let Some(hint) = category_hint(categories, txs) {
            let _ = writeln!(out, "#   the categorized reference export says: {hint}");
        }
        if let Some(amount) = looks_monthly(txs) {
            let _ = writeln!(out, "#   {} every month: maybe a [[subscription]]", fmt_num(amount.abs()));
        }
        let _ = writeln!(
            out,
            "[[rule]]\nname = \"{name}\"\nwhen = {{ source = \"{source}\", {condition} }}\naccount = \"\"\n"
        );
    }
    (out, groups.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use konter_engine::model::quoted_description;

    #[test]
    fn normalizing_and_naming() {
        assert_eq!(normalize("GOOGLE*CLOUD AB12CD"), "google*cloud");
        assert_eq!(normalize("Faktura 01-02-2026"), "faktura");
        assert_eq!(normalize("OVERFØRSEL 01.01 - 31.01.2026"), "overførsel");
        assert_eq!(slug("ejendoms adm. eksempel a/s,"), "ejendoms-adm-eksempel-a-s");
        assert_eq!(slug("Ærøtur Hotel+Færge"), "ærøtur-hotel-færge");
        assert_eq!(slug("***"), "unnamed");
        assert_eq!(common_prefix(&["GOOGLE*CLOUD A1", "GOOGLE*CLOUD B2"]), "GOOGLE*CLOUD ");
    }

    #[test]
    fn card_payments_group_by_merchant() {
        let tx = |description: &str, name: Option<&str>| BankTransaction {
            id: String::new(),
            source: "test:a".into(),
            format: Some(konter_engine::config::Format::EnableBanking),
            booked: jiff::civil::date(2026, 1, 1),
            time: None,
            value_date: None,
            transaction_date: None,
            amount: Decimal::from(-50),
            currency: "DKK".into(),
            original_amount: None,
            original_currency: None,
            description: description.into(),
            counterparty: None,
            counterparty_name: name.map(str::to_string),
            balance_after: None,
            bank_ref: None,
            raw: BTreeMap::new(),
            origins: Vec::new(),
            order_hint: 0,
        };
        // Only a reference number: grouped by the merchant instead.
        let a = tx("74300000000000000000017", Some("NETTO SCANNGO"));
        let b = tx("74300000000000000000025", Some("NETTO SCANNGO"));
        assert_eq!(group_key(&a), group_key(&b));
        assert_eq!(name_instead(&a), Some("NETTO SCANNGO"));
        // A real description wins over the name.
        assert_eq!(group_key(&tx("COOP APP", Some("COOP"))), "coop app");
        assert_eq!(name_instead(&tx("COOP APP", Some("COOP"))), None);
        // Listings show the name next to a reference, and leave real text alone.
        assert_eq!(quoted_description(&a), r#""74300000000000000000017" [NETTO SCANNGO]"#);
        assert_eq!(quoted_description(&tx("COOP APP", Some("COOP"))), r#""COOP APP""#);
        assert_eq!(group_key(&tx("74300000000000000000017", None)), "");
    }
}
