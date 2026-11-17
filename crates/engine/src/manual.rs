//! What's written by hand in config/ besides the sources and the rules: the
//! accounts (config/accounts.toml) and the transactions that have no bank row
//! (config/transactions.toml). konter writes both into the generated journal, so
//! everything in ledger/ is generated.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use jiff::civil::Date;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::dates::ConfigDate;
use crate::money::Amount;

pub const ACCOUNTS: &str = "config/accounts.toml";
pub const TRANSACTIONS: &str = "config/transactions.toml";

#[derive(Clone, Debug, Default)]
pub struct Manual {
    /// Every account the rules and transactions may use.
    pub accounts: BTreeSet<String>,
    /// In the order written.
    pub txs: Vec<ManualTransaction>,
}

/// A transaction written by hand. Amounts are in the books' currency.
#[derive(Clone, Debug, PartialEq)]
pub struct ManualTransaction {
    pub date: Date,
    pub payee: String,
    pub note: Option<String>,
    pub tags: Vec<(String, String)>,
    pub postings: Vec<ManualPosting>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ManualPosting {
    pub account: String,
    pub amount: ManualAmount,
    /// Balance assertion: the account's balance after this posting.
    pub balance: Option<Decimal>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ManualAmount {
    Fixed(Decimal),
    /// What balances the transaction (`rest = true`).
    Rest,
    /// What makes `balance` hold: a balance assignment.
    ToBalance,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAccounts {
    accounts: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTransactions {
    #[serde(default)]
    transaction: Vec<RawTx>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTx {
    date: ConfigDate,
    payee: String,
    note: Option<String>,
    #[serde(default)]
    tags: BTreeMap<String, String>,
    postings: Vec<RawPosting>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPosting {
    account: String,
    amount: Option<Amount>,
    balance: Option<Amount>,
    #[serde(default)]
    rest: bool,
}

impl Manual {
    /// Reads config/accounts.toml and config/transactions.toml from their text.
    pub fn parse(accounts: &str, transactions: &str) -> Result<Manual> {
        let mut manual = Manual::default();
        let raw: RawAccounts = toml::from_str(accounts).with_context(|| format!("reading {ACCOUNTS}"))?;
        for account in raw.accounts {
            if account.is_empty()
                || account.trim() != account
                || account.contains(['\t', ';'])
                || account.contains("  ")
            {
                bail!("{ACCOUNTS}: invalid account name {account:?}");
            }
            if !manual.accounts.insert(account.clone()) {
                bail!("{ACCOUNTS}: {account} is declared twice");
            }
        }
        let raw: RawTransactions = toml::from_str(transactions).with_context(|| format!("reading {TRANSACTIONS}"))?;
        for tx in raw.transaction {
            let what = format!("{TRANSACTIONS}: transaction {} {:?}", tx.date.0, tx.payee);
            manual.txs.push(convert(tx).context(what)?);
        }
        Ok(manual)
    }
}

fn convert(tx: RawTx) -> Result<ManualTransaction> {
    if tx.postings.is_empty() {
        bail!("has no postings");
    }
    for (k, v) in &tx.tags {
        if !crate::rules::valid_tag(k) || v.contains([',', ';', '\n']) {
            bail!("invalid tag {k:?} = {v:?} (no spaces, commas, colons or semicolons in names; no commas in values)");
        }
    }
    let mut postings = Vec::new();
    for p in tx.postings {
        let amount = match (p.amount, p.rest, p.balance) {
            (Some(a), false, _) => ManualAmount::Fixed(a.0),
            (None, true, None) => ManualAmount::Rest,
            (None, false, Some(_)) => ManualAmount::ToBalance,
            (Some(_), true, _) => bail!("posting to {:?} has both amount and rest", p.account),
            (None, true, Some(_)) => bail!("posting to {:?} has both rest and balance", p.account),
            (None, false, None) => bail!("posting to {:?} needs an amount, a balance or rest = true", p.account),
        };
        postings.push(ManualPosting { account: p.account, amount, balance: p.balance.map(|b| b.0) });
    }
    if postings.iter().filter(|p| p.amount == ManualAmount::Rest).count() > 1 {
        bail!("more than one posting has rest = true");
    }
    Ok(ManualTransaction {
        date: tx.date.0,
        payee: tx.payee,
        note: tx.note,
        tags: tx.tags.into_iter().collect(),
        postings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNTS_TOML: &str = r#"
        accounts = [
            # ── Assets ──
            "assets:investments:acme",  # money put into Acme
            "expenses:investment-losses:acme",
        ]
    "#;

    #[test]
    fn reads_accounts_and_transactions() {
        let manual = Manual::parse(
            ACCOUNTS_TOML,
            r#"
            [[transaction]]
            date = 2025-01-01
            payee = "Deposit: opening balance"
            tags = { kind = "opening" }
            postings = [
                { account = "assets:deposits:x", amount = 15000 },
                { account = "equity:opening-balances", rest = true },
            ]

            [[transaction]]
            date = 2026-03-25
            payee = "Acme dissolved"
            note = "what's left is lost"
            postings = [
                { account = "assets:investments:acme", balance = 0 },
                { account = "expenses:investment-losses:acme", rest = true },
                { account = "assets:x", amount = "1.50", balance = 3 },
            ]
            "#,
        )
        .expect("the transactions parse");
        assert_eq!(manual.accounts.len(), 2);
        let [opening, acme] = &manual.txs[..] else { panic!("{:?}", manual.txs) };
        assert_eq!(opening.tags, [("kind".into(), "opening".into())]);
        assert_eq!(opening.postings[0].amount, ManualAmount::Fixed(Decimal::from(15000)));
        assert_eq!(opening.postings[1].amount, ManualAmount::Rest);
        assert_eq!(acme.note.as_deref(), Some("what's left is lost"));
        assert_eq!(acme.postings[0].amount, ManualAmount::ToBalance);
        assert_eq!(acme.postings[0].balance, Some(Decimal::ZERO));
        assert_eq!(acme.postings[2].amount, ManualAmount::Fixed(Decimal::new(150, 2)));
        assert_eq!(acme.postings[2].balance, Some(Decimal::from(3)));
        assert!(Manual::parse(ACCOUNTS_TOML, "").expect("the accounts parse").txs.is_empty());
    }

    #[test]
    fn rejects_what_doesnt_make_sense() {
        let tx =
            |postings: &str| format!("[[transaction]]\ndate = 2026-01-01\npayee = \"X\"\npostings = [{postings}]\n");
        for (postings, expected) in [
            (r#"{ account = "a" }"#, "needs an amount, a balance or rest = true"),
            (r#"{ account = "a", amount = 1, rest = true }"#, "both amount and rest"),
            (r#"{ account = "a", balance = 1, rest = true }"#, "both rest and balance"),
            (r#"{ account = "a", rest = true }, { account = "b", rest = true }"#, "more than one"),
            ("", "has no postings"),
        ] {
            let error =
                format!("{:#}", Manual::parse(ACCOUNTS_TOML, &tx(postings)).expect_err("these postings are refused"));
            assert!(error.contains(expected), "{postings}: {error}");
            assert!(error.contains("config/transactions.toml: transaction 2026-01-01 \"X\""), "{error}");
        }
        // TOML errors point to the line instead.
        let error = format!(
            "{:#}",
            Manual::parse(ACCOUNTS_TOML, &tx(r#"{ account = "a", price = 2 }"#))
                .expect_err("an unknown field is refused")
        );
        assert!(error.contains("reading config/transactions.toml") && error.contains("unknown field"), "{error}");
        for (accounts, expected) in [
            (r#"accounts = ["a", "a"]"#, "a is declared twice"),
            (r#"accounts = ["a  b"]"#, "invalid account name"),
            (r#"accounts = ["a ; b"]"#, "invalid account name"),
        ] {
            let error = format!("{:#}", Manual::parse(accounts, "").expect_err("these accounts are refused"));
            assert!(error.contains(expected), "{accounts}: {error}");
        }
    }

    #[test]
    fn account_names_and_tags_hledger_would_read_differently_are_refused() {
        for name in ["", " expenses:x", "expenses:x ", "expenses;x", "expenses\tx", "expenses:two  words"] {
            let error = Manual::parse(&format!("accounts = [{name:?}]"), "").expect_err("the name is refused");
            assert!(format!("{error:#}").contains("invalid account name"), "{name:?}: {error:#}");
        }
        Manual::parse("accounts = [\"expenses:two words\"]", "").expect("one space is fine");

        let tagged = |tags: &str| {
            let text = format!(
                "[[transaction]]\ndate = 2026-01-05\npayee = \"Gift\"\ntags = {{ {tags} }}\n\
                 postings = [{{ account = \"expenses:x\", amount = 5 }}, {{ account = \"assets:y\", rest = true }}]\n"
            );
            Manual::parse("accounts = []", &text)
        };
        tagged("to = \"a friend\"").expect("a tag with a value is fine");
        for tags in ["\"two words\" = \"x\"", "to = \"here, there\"", "to = \"here; there\""] {
            let error = tagged(tags).expect_err("the tag is refused");
            assert!(format!("{error:#}").contains("invalid tag"), "{tags}: {error:#}");
        }
    }
}
