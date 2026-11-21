//! Lunar statement CSV export.
//!
//! `Date,Time,Title,Amount,Balance,Transaction ID`, UTF-8 with BOM, newest first.
//! Rows with the same timestamp aren't reliably in order; a build orders them
//! by running balance (`chain`).

use anyhow::{Context, Result, bail};
use jiff::civil::Time;

use super::FileContext;
use crate::dates::parse_dmy_dot;
use crate::model::{BankTransaction, Origin};
use crate::money::parse_danish;

pub const HEADER: [&str; 6] = ["Date", "Time", "Title", "Amount", "Balance", "Transaction ID"];

pub fn looks_like(text: &str) -> bool {
    text.starts_with("Date,Time,Title,Amount,Balance,Transaction ID")
}

fn parse_time(s: &str) -> Result<Time> {
    let (h, m) = s.split_once('.').with_context(|| format!("invalid time {s:?}, expected HH.MM"))?;
    Ok(Time::new(h.parse()?, m.parse()?, 0, 0)?)
}

pub fn parse(text: &str, ctx: &FileContext) -> Result<Vec<BankTransaction>> {
    let mut reader = csv::ReaderBuilder::new().from_reader(text.as_bytes());
    let header: Vec<String> = reader.headers()?.iter().map(str::to_string).collect();
    if header != HEADER {
        bail!("unexpected header {header:?}");
    }
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record?;
        let line = record.position().map(|p| p.line()).unwrap_or(0);
        let get = |i: usize| record.get(i).unwrap_or("").trim().to_string();
        let row = (|| -> Result<BankTransaction> {
            Ok(BankTransaction {
                id: String::new(),
                source: ctx.source.clone(),
                booked: parse_dmy_dot(&get(0))?,
                time: Some(parse_time(&get(1))?),
                value_date: None,
                transaction_date: None,
                amount: parse_danish(&get(3))?,
                currency: ctx.currency.clone(),
                original_amount: None,
                original_currency: None,
                description: get(2),
                counterparty: None,
                counterparty_name: None,
                balance_after: Some(parse_danish(&get(4))?),
                bank_ref: Some(get(5)).filter(|r| !r.is_empty()),
                origins: vec![Origin { file: ctx.rel.clone(), line }],
                order_hint: -i64::try_from(line).unwrap_or(i64::MAX), // newest first
                unsettled: None,
            })
        })()
        .with_context(|| format!("line {line}"))?;
        rows.push(row);
    }
    Ok(rows)
}
