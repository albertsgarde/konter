//! Nordea netbank CSV export.
//!
//! `Bogføringsdato;Beløb;Afsender;Modtager;Navn;Beskrivelse;Saldo;Valuta;Afstemt`,
//! UTF-8 with BOM, newest transaction first.

use anyhow::{Context, Result, bail};

use super::FileContext;
use crate::dates::parse_ymd_slash;
use crate::model::{BankTransaction, Origin};
use crate::money::parse_danish;

pub const HEADER: [&str; 9] =
    ["Bogføringsdato", "Beløb", "Afsender", "Modtager", "Navn", "Beskrivelse", "Saldo", "Valuta", "Afstemt"];

pub fn looks_like(text: &str) -> bool {
    text.starts_with("Bogføringsdato;")
}

/// The account number in Nordea's export filename (`Konto 1234567890 - …csv`).
/// Nordea names exports `<account name> <number> - <YYYY-MM-DD HH.MM.SS>.csv`,
/// e.g. `Konto 1234567890 - …` or `Opsparing 9876543210 - …`.
pub fn number_from_filename(name: &str) -> Option<String> {
    let (head, _) = name.split_once(" - ")?;
    let number = head.rsplit(' ').next()?;
    (!number.is_empty() && number.chars().all(|c| c.is_ascii_digit())).then(|| number.to_string())
}

/// The export date in Nordea's filename.
pub fn export_date_from_filename(name: &str) -> Option<jiff::civil::Date> {
    let (_, tail) = name.split_once(" - ")?;
    tail.get(..10)?.parse().ok()
}

pub fn parse(text: &str, ctx: &FileContext) -> Result<Vec<BankTransaction>> {
    if let Some(number) = number_from_filename(&ctx.file_name)
        && number.trim_start_matches('0') != ctx.number.trim_start_matches('0')
    {
        bail!("filename says account {number}, but this is the directory of account {}", ctx.number);
    }
    let mut reader = csv::ReaderBuilder::new().delimiter(b';').flexible(true).from_reader(text.as_bytes());
    let header: Vec<String> = reader.headers()?.iter().map(str::to_string).collect();
    if header.len() < HEADER.len() || header[..HEADER.len()] != HEADER {
        bail!("unexpected header {header:?}");
    }
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record?;
        let line = record.position().map(|p| p.line()).unwrap_or(0);
        let get = |i: usize| record.get(i).unwrap_or("").trim().to_string();
        let row = (|| -> Result<Option<BankTransaction>> {
            let date = get(0);
            if date.eq_ignore_ascii_case("reserveret") {
                return Ok(None); // pending
            }
            let booked = parse_ymd_slash(&date)?;
            let (sender, recipient) = (get(2), get(3));
            let is_own = |s: &str| !s.is_empty() && s.ends_with(&ctx.number);
            if !sender.is_empty() && !recipient.is_empty() && !is_own(&sender) && !is_own(&recipient) {
                bail!("neither sender {sender:?} nor recipient {recipient:?} is account {}", ctx.number);
            }
            let counterparty = [&sender, &recipient].into_iter().find(|s| !s.is_empty() && !is_own(s)).cloned();
            Ok(Some(BankTransaction {
                id: String::new(),
                source: ctx.source.clone(),
                booked,
                time: None,
                value_date: None,
                transaction_date: None,
                amount: parse_danish(&get(1))?,
                currency: Some(get(7)).filter(|c| !c.is_empty()).unwrap_or_else(|| ctx.currency.clone()),
                original_amount: None,
                original_currency: None,
                description: get(5),
                counterparty,
                counterparty_name: Some(get(4)).filter(|n| !n.is_empty()),
                balance_after: Some(parse_danish(&get(6))?),
                bank_ref: None,
                origins: vec![Origin { file: ctx.rel.clone(), line }],
                order_hint: -i64::try_from(line).unwrap_or(i64::MAX), // newest first
                unsettled: None,
            }))
        })()
        .with_context(|| format!("line {line}"))?;
        rows.extend(row);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames() {
        assert_eq!(number_from_filename("Konto 1234567890 - 2026-11-08 12.30.58.csv").as_deref(), Some("1234567890"));
        assert_eq!(
            number_from_filename("Opsparing 9876543210 - 2026-11-08 21.49.07.csv").as_deref(),
            Some("9876543210")
        );
        assert_eq!(number_from_filename("lunar-expenses.csv"), None);
        assert_eq!(
            export_date_from_filename("Opsparing 9876543210 - 2026-11-08 21.49.07.csv"),
            Some(jiff::civil::date(2026, 11, 8))
        );
    }
}
