//! Core data types: bank transactions in, ledger transactions out.

use jiff::civil::{Date, Time};
use rust_decimal::Decimal;

/// Where a bank transaction was read from.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Origin {
    /// Path relative to `data/`.
    pub file: String,
    /// 1-based line (CSV) or index (JSON) in the file.
    pub line: u64,
}

/// One booked bank transaction in the common format (docs/spec.md §4).
#[derive(Clone, Debug)]
pub struct BankTransaction {
    /// Its ID in the store of transactions (docs/spec.md §4). Empty in a row
    /// of a raw file, which isn't a transaction of the table yet.
    pub id: String,
    pub source: String,
    pub booked: Date,
    pub time: Option<Time>,
    pub value_date: Option<Date>,
    pub transaction_date: Option<Date>,
    pub amount: Decimal,
    pub currency: String,
    pub original_amount: Option<Decimal>,
    pub original_currency: Option<String>,
    pub description: String,
    pub counterparty: Option<String>,
    pub counterparty_name: Option<String>,
    pub balance_after: Option<Decimal>,
    pub bank_ref: Option<String>,
    /// All files this transaction appears in; the first one is its representative.
    pub origins: Vec<Origin>,
    /// Position within its file, lower = earlier. Breaks ties when ordering a day.
    pub order_hint: i64,
    /// Set on a transaction of the books that the store has something
    /// unsettled about, for whatever shows it to say so: `maybe-same-as` when
    /// another row of the table may be the same transaction and counts as it,
    /// `conflicts` when a copy says something else than the table, or both
    /// with a `+` between.
    pub unsettled: Option<String>,
}

impl BankTransaction {
    pub fn balance_before(&self) -> Option<Decimal> {
        self.balance_after.map(|b| b - self.amount)
    }

    pub fn origin(&self) -> &Origin {
        &self.origins[0]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Posting {
    pub account: String,
    pub amount: Decimal,
    pub commodity: String,
    /// Balance assertion: the account's balance after this posting.
    pub assert_balance: Option<Decimal>,
    pub comment: Option<String>,
}

impl Posting {
    pub fn new(account: impl Into<String>, amount: Decimal, commodity: impl Into<String>) -> Posting {
        Posting { account: account.into(), amount, commodity: commodity.into(), assert_balance: None, comment: None }
    }
}

/// What a ledger transaction was generated from; decides output order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Provenance {
    Opening,
    Subscription,
    /// Generated from a bank transaction; the number is its position in the
    /// source's chain, which keeps balance assertions in booking order.
    Bank {
        source: String,
        seq: usize,
    },
    /// Written by hand in config/transactions.toml; the number is its
    /// position there. After the bank's transactions on its date, so a
    /// balance it asserts or assigns is the balance at the end of the day.
    Manual {
        seq: usize,
    },
    /// A balance check from a snapshot; after everything else on its date.
    Assertion,
}

#[derive(Clone, Debug)]
pub struct LedgerTransaction {
    pub date: Date,
    pub description: String,
    /// Tags as (name, value), written in the transaction comment.
    pub tags: Vec<(String, String)>,
    /// Extra comment lines below the header.
    pub comments: Vec<String>,
    pub postings: Vec<Posting>,
    pub provenance: Provenance,
}

/// Lowercased description without words that contain digits (dates, card
/// numbers, references), so "GOOGLE*CLOUD AB12CD" and "GOOGLE*CLOUD EF34GH" group.
pub fn normalize(description: &str) -> String {
    description
        .split_whitespace()
        .filter(|w| !w.chars().any(|c| c.is_ascii_digit()))
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The counterparty name, for transactions whose description is only a
/// reference (card payments through Enable Banking: "74300000000000000000017",
/// with the merchant as the creditor's name).
pub fn name_instead(t: &BankTransaction) -> Option<&str> {
    t.counterparty_name.as_deref().filter(|_| normalize(&t.description).is_empty())
}

/// The description, quoted, followed by the counterparty name in brackets when
/// the description is only a reference: `"74300000000000000000017" [NETTO]`.
pub fn quoted_description(t: &BankTransaction) -> String {
    match name_instead(t) {
        Some(name) => format!("{:?} [{name}]", t.description),
        None => format!("{:?}", t.description),
    }
}
