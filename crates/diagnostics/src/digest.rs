//! `konter digest`: what needs attention, as a short message to read on a phone.
//! The new transactions to map (unmapped and not marked later), numbered and
//! with what it takes to recognise them, then the build's errors and warnings.
//! Nothing at all when there's nothing to say, so a daily job can stay quiet.
//!
//! The numbers only mean something within one digest; the JSON form maps them
//! to transaction IDs for whoever acts on the answer.

use std::fmt::Write as _;

use jiff::civil::Date;
use serde_json::{Value, json};

use crate::gaps::tidy;
use konter_engine::engine::Status;
use konter_engine::findings::{Diag, Severity};
use konter_engine::model::{BankTransaction, name_instead};
use konter_engine::money::fmt_num;
use konter_engine::pipeline::Build;
use konter_engine::rules::Trip;

pub struct Item<'a> {
    /// From 1, oldest first.
    pub number: usize,
    pub tx: &'a BankTransaction,
    /// The declared trip it was bought during, if any.
    pub trip: Option<&'a Trip>,
}

pub struct Digest<'a> {
    pub as_of: Date,
    pub items: Vec<Item<'a>>,
    /// Errors, then warnings.
    pub findings: Vec<&'a Diag>,
}

/// When the money was spent: the purchase date if the bank gives one.
fn day(t: &BankTransaction) -> Date {
    t.transaction_date.unwrap_or(t.booked)
}

pub fn digest(build: &Build) -> Digest<'_> {
    let mut txs: Vec<&BankTransaction> =
        build.loaded.transactions().filter(|t| build.generated.outcomes[&t.id].status == Status::Unmatched).collect();
    txs.sort_by(|a, b| (a.booked, &a.source, a.order_hint, &a.id).cmp(&(b.booked, &b.source, b.order_hint, &b.id)));
    let items = txs
        .into_iter()
        .enumerate()
        .map(|(i, t)| Item { number: i + 1, tx: t, trip: build.rules.trips.values().find(|tr| tr.contains(day(t))) })
        .collect();
    // The unmapped ones are listed already.
    let mut findings: Vec<&Diag> = build
        .findings
        .diags
        .iter()
        .filter(|d| matches!(d.severity, Severity::Error | Severity::Warning) && d.category != "not mapped")
        .collect();
    findings.sort_by_key(|d| d.severity);
    Digest { as_of: build.as_of, items, findings }
}

impl Digest<'_> {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.findings.is_empty()
    }

    /// The message; empty when there's nothing to say.
    pub fn text(&self) -> String {
        let mut out = String::new();
        if self.is_empty() {
            return out;
        }
        if !self.items.is_empty() {
            let n = self.items.len();
            let _ = writeln!(out, "{n} new {} to map:", if n == 1 { "transaction" } else { "transactions" });
            for item in &self.items {
                let t = item.tx;
                let mut first = format!("{}. {}  {} {}", item.number, t.booked, fmt_num(t.amount), t.currency);
                if let (Some(amount), Some(currency)) = (t.original_amount, &t.original_currency) {
                    let _ = write!(first, " ({} {currency})", fmt_num(amount));
                }
                let _ = writeln!(out, "{first}  {}", tidy(name_instead(t).unwrap_or(&t.description)));
                let mut more = vec![t.source.clone()];
                if day(t) != t.booked {
                    more.push(format!("bought {}", day(t)));
                }
                if let Some(trip) = item.trip {
                    more.push(format!("during the {} trip", trip.name));
                }
                if t.unsettled.is_some() {
                    more.push("not settled in the store".into());
                }
                let _ = writeln!(out, "   {}", more.join(" · "));
            }
        }
        if !self.findings.is_empty() {
            if !self.items.is_empty() {
                out.push('\n');
            }
            let _ = writeln!(out, "Needs attention:");
            for d in &self.findings {
                let symbol = if d.severity == Severity::Error { "✗" } else { "⚠" };
                let _ = writeln!(out, "{symbol} {}: {}", d.category, d.message);
            }
        }
        out
    }

    /// Everything in the message, with transaction IDs and finding details.
    /// Amounts are strings, so they stay exact.
    pub fn json(&self) -> Value {
        let items: Vec<Value> = self
            .items
            .iter()
            .map(|item| {
                let t = item.tx;
                json!({
                    "number": item.number,
                    "id": t.id,
                    "source": t.source,
                    "booked": t.booked.to_string(),
                    "purchased": t.transaction_date.map(|d| d.to_string()),
                    "amount": fmt_num(t.amount),
                    "currency": t.currency,
                    "original_amount": t.original_amount.map(fmt_num),
                    "original_currency": t.original_currency,
                    "description": t.description,
                    "counterparty_name": t.counterparty_name,
                    "unsettled": t.unsettled,
                    "trip": item.trip.map(|tr| json!({
                        "name": tr.name,
                        "account": tr.account,
                        "from": tr.from.to_string(),
                        "to": tr.to.to_string(),
                    })),
                })
            })
            .collect();
        let findings: Vec<Value> = self
            .findings
            .iter()
            .map(|d| {
                json!({
                    "severity": if d.severity == Severity::Error { "error" } else { "warning" },
                    "category": d.category,
                    "message": d.message,
                    "details": d.details,
                })
            })
            .collect();
        json!({
            "as_of": self.as_of.to_string(),
            "text": self.text(),
            "transactions": items,
            "findings": findings,
        })
    }
}
