//! Raw Enable Banking responses, as stored by `konter fetch`:
//!
//! ```json
//! { "acct_fetch": 1, "fetched_at": "...", "aspsp": {...}, "account_uid": "...",
//!   "date_from": "...", "pages": [<GET /accounts/{uid}/transactions>, ...],
//!   "balances": <GET /accounts/{uid}/balances> }
//! ```
//!
//! The responses are stored unmodified inside the envelope; everything below
//! interprets them.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use jiff::civil::Date;
use rust_decimal::Decimal;
use serde_json::Value;

use super::FileContext;
use super::stored::Snapshot;
use crate::config::Format;
use crate::model::{BankTransaction, Origin};
use crate::money::parse_plain;

/// Named after the tool's first name, acct. Every fetched file carries it, and
/// raw files are never edited, so it stays.
pub const MARKER: &str = "acct_fetch";

pub fn looks_like(file_name: &str, text: &str) -> bool {
    file_name.ends_with(".json") && text.contains(&format!("\"{MARKER}\""))
}

/// Balance types that only include booked transactions, most preferred first.
const BOOKED_BALANCE_TYPES: [&str; 2] = ["CLBD", "ITBD"];

fn str_at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut v = v;
    for key in path {
        v = v.get(key)?;
    }
    v.as_str().filter(|s| !s.trim().is_empty())
}

fn date_at(v: &Value, path: &[&str]) -> Result<Option<Date>> {
    str_at(v, path)
        .map(|s| s.get(..10).unwrap_or(s).parse::<Date>().with_context(|| format!("invalid date {s:?}")))
        .transpose()
}

fn amount_at(v: &Value, path: &[&str]) -> Result<Option<Decimal>> {
    match path.iter().try_fold(v, |v, k| v.get(k)) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => parse_plain(s).map(Some),
        Some(Value::Number(n)) => parse_plain(&n.to_string()).map(Some),
        Some(other) => bail!("invalid amount {other}"),
    }
}

/// Flattens a JSON object into `a.b.c` → text, for `raw.<field>` rule conditions.
#[expect(clippy::wildcard_enum_match_arm, reason = "the kinds of JSON are fixed, and the rest are the scalars")]
fn flatten(prefix: &str, v: &Value, out: &mut BTreeMap<String, String>) {
    match v {
        Value::Object(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                flatten(&key, v, out);
            }
        }
        Value::Array(items) if items.iter().all(|i| !i.is_object() && !i.is_array()) => {
            let parts: Vec<String> = items.iter().map(scalar).collect();
            out.insert(prefix.to_string(), parts.join(" "));
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                flatten(&format!("{prefix}[{i}]"), item, out);
            }
        }
        Value::Null => {}
        other => {
            out.insert(prefix.to_string(), scalar(other));
        }
    }
}

#[expect(clippy::wildcard_enum_match_arm, reason = "the kinds of JSON are fixed, and the rest print as they are")]
fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn parse_transaction(t: &Value, index: usize, ctx: &FileContext) -> Result<Option<BankTransaction>> {
    if let Some(status) = str_at(t, &["status"])
        && status != "BOOK"
    {
        return Ok(None); // pending, scheduled, cancelled, ...
    }
    let booked = match date_at(t, &["booking_date"])? {
        Some(d) => d,
        // Some banks leave out booking_date on booked transactions.
        None => date_at(t, &["value_date"])?
            .or(date_at(t, &["transaction_date"])?)
            .context("transaction has no booking, value or transaction date")?,
    };
    let raw_amount = amount_at(t, &["transaction_amount", "amount"])?.context("missing transaction_amount")?;
    // The amount may be unsigned with the direction in credit_debit_indicator.
    let amount = match str_at(t, &["credit_debit_indicator"]) {
        Some("DBIT") => -raw_amount.abs(),
        Some("CRDT") => raw_amount.abs(),
        _ => raw_amount,
    };
    let outgoing = amount < Decimal::ZERO;
    let (party, party_account) = if outgoing { ("creditor", "creditor_account") } else { ("debtor", "debtor_account") };
    let counterparty_name = str_at(t, &[party, "name"]).map(str::to_string);
    let remittance: Vec<String> = t
        .get("remittance_information")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
        })
        .unwrap_or_default();
    // Nordea puts a reference code on a second line; the first line is the
    // readable text. The full text stays in raw.remittance_information.
    let description = if !remittance.is_empty() {
        remittance.join("\n").lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default().to_string()
    } else {
        counterparty_name
            .clone()
            .or_else(|| str_at(t, &["note"]).map(str::to_string))
            .or_else(|| str_at(t, &["bank_transaction_code", "description"]).map(str::to_string))
            .unwrap_or_default()
    };
    let currency =
        str_at(t, &["transaction_amount", "currency"]).map(str::to_string).unwrap_or_else(|| ctx.currency.clone());
    let instructed = t.get("exchange_rate").and_then(|e| e.get("instructed_amount"));
    let (original_amount, original_currency) = match instructed {
        Some(i) => match (amount_at(i, &["amount"])?, str_at(i, &["currency"])) {
            (Some(a), Some(c)) if c != currency => {
                (Some(if outgoing { -a.abs() } else { a.abs() }), Some(c.to_string()))
            }
            _ => (None, None),
        },
        None => (None, None),
    };
    let bank_ref = str_at(t, &["entry_reference"]).filter(|r| *r != "0").map(str::to_string);
    let mut raw = BTreeMap::new();
    flatten("", t, &mut raw);
    Ok(Some(BankTransaction {
        id: String::new(),
        source: ctx.source.clone(),
        format: Some(Format::EnableBanking),
        booked,
        time: None,
        value_date: date_at(t, &["value_date"])?,
        transaction_date: date_at(t, &["transaction_date"])?,
        amount,
        currency,
        original_amount,
        original_currency,
        description,
        counterparty: str_at(t, &[party_account, "iban"]).map(str::to_string),
        counterparty_name,
        balance_after: amount_at(t, &["balance_after_transaction", "amount"])?,
        bank_ref,
        raw,
        origins: vec![Origin { file: ctx.rel.clone(), line: index as u64 + 1 }],
        order_hint: i64::try_from(index).unwrap_or(i64::MAX),
    }))
}

/// The date a fetch was made.
pub fn fetched_date(text: &str) -> Option<Date> {
    let doc: Value = serde_json::from_str(text).ok()?;
    date_at(&doc, &["fetched_at"]).ok().flatten()
}

pub fn parse(text: &str, ctx: &FileContext) -> Result<(Vec<BankTransaction>, Vec<Snapshot>)> {
    let doc: Value = serde_json::from_str(text)?;
    if doc.get(MARKER).and_then(Value::as_i64) != Some(1) {
        bail!("unsupported {MARKER} version");
    }
    let mut rows = Vec::new();
    let mut index = 0;
    for page in doc.get("pages").and_then(Value::as_array).context("missing pages")? {
        for t in page.get("transactions").and_then(Value::as_array).into_iter().flatten() {
            if let Some(row) = parse_transaction(t, index, ctx).with_context(|| format!("transaction {}", index + 1))? {
                rows.push(row);
            }
            index += 1;
        }
    }
    let fetched = date_at(&doc, &["fetched_at"])?.context("missing fetched_at")?;
    let mut snapshots = Vec::new();
    let balances = doc.get("balances").and_then(|b| b.get("balances")).and_then(Value::as_array);
    for kind in BOOKED_BALANCE_TYPES {
        if let Some(b) = balances.into_iter().flatten().find(|b| str_at(b, &["balance_type"]) == Some(kind)) {
            let balance = amount_at(b, &["balance_amount", "amount"])?.context("balance without amount")?;
            let date = date_at(b, &["reference_date"])?.unwrap_or(fetched);
            snapshots.push(Snapshot { date, balance, file: ctx.rel.clone() });
            break;
        }
    }
    Ok((rows, snapshots))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn ctx() -> FileContext {
        FileContext {
            source: "nordea:x".into(),
            number: "1".into(),
            currency: "DKK".into(),
            rel: "nordea/1/eb.json".into(),
            file_name: "eb.json".into(),
        }
    }

    #[test]
    fn parses_documented_shape() {
        let text = r#"{
          "acct_fetch": 1,
          "fetched_at": "2026-11-20T09:14:03Z",
          "pages": [{"transactions": [
            {"entry_reference": "A1", "status": "BOOK", "booking_date": "2026-11-02",
             "value_date": "2026-11-02", "transaction_date": "2026-10-31",
             "transaction_amount": {"currency": "DKK", "amount": "300.00"},
             "credit_debit_indicator": "DBIT",
             "creditor": {"name": "Foreningen Eksempel"}, "creditor_account": {"iban": "DK11"},
             "remittance_information": ["FORENINGEN EKSEMPEL FR"],
             "balance_after_transaction": {"currency": "DKK", "amount": "-12.50"}},
            {"entry_reference": "0", "status": "PDNG", "booking_date": "2026-11-03",
             "transaction_amount": {"currency": "DKK", "amount": "1.00"}, "credit_debit_indicator": "DBIT"},
            {"entry_reference": "B2", "value_date": "2026-11-04",
             "transaction_amount": {"currency": "DKK", "amount": "149.20"}, "credit_debit_indicator": "DBIT",
             "exchange_rate": {"instructed_amount": {"currency": "EUR", "amount": "20.00"}},
             "creditor": {"name": "Shop"}}
          ]}],
          "balances": {"balances": [
            {"balance_type": "ITAV", "balance_amount": {"currency": "DKK", "amount": "999"}},
            {"balance_type": "ITBD", "balance_amount": {"currency": "DKK", "amount": "-161.70"}, "reference_date": "2026-11-20"}
          ]}
        }"#;
        let (rows, snaps) = parse(text, &ctx()).expect("the fetch parses");
        assert_eq!(rows.len(), 2, "pending transaction skipped");
        let a = &rows[0];
        assert_eq!(a.amount, Decimal::from_str("-300").expect("a decimal"));
        assert_eq!(a.description, "FORENINGEN EKSEMPEL FR");
        assert_eq!(a.counterparty.as_deref(), Some("DK11"));
        assert_eq!(a.balance_after, Some(Decimal::from_str("-12.50").expect("a decimal")));
        assert_eq!(a.transaction_date, Some(Date::constant(2026, 10, 31)));
        assert_eq!(a.raw.get("creditor.name").map(String::as_str), Some("Foreningen Eksempel"));
        let b = &rows[1];
        assert_eq!(b.booked, Date::constant(2026, 11, 4), "falls back to value_date");
        assert_eq!(b.description, "Shop");
        assert_eq!(b.original_amount, Some(Decimal::from_str("-20").expect("a decimal")));
        assert_eq!(b.original_currency.as_deref(), Some("EUR"));
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].balance, Decimal::from_str("-161.70").expect("a decimal"));
    }
}
