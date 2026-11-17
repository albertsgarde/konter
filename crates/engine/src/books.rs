//! The books as a whole: the generated transactions and the ones written by
//! hand in config/transactions.toml, in the order they're written in
//! ledger/generated.journal, with every amount filled in and checked.
//!
//! The checks are the ones `hledger check --strict` makes on these books:
//! every transaction balances, every balance assertion holds, and every account
//! and commodity is declared. hledger itself is the independent second opinion
//! (`konter verify`); these run on every build without it.

use std::collections::{BTreeMap, BTreeSet};

use jiff::civil::Date;
use rust_decimal::Decimal;

use crate::findings::Findings;
use crate::manual::{ManualAmount, ManualTransaction};
use crate::model::{LedgerTransaction, Posting, Provenance};
use crate::money::fmt_amount;

/// The whole books, in journal order: the generated transactions, and the
/// hand-written ones with their amounts filled in. `declared`: every declared
/// account; `currency`: the books' only commodity. What doesn't add up goes
/// into `findings`; the transactions are returned anyway.
pub fn assemble(
    manual: &[ManualTransaction],
    generated: &[LedgerTransaction],
    declared: &BTreeSet<String>,
    currency: &str,
    findings: &mut Findings,
) -> Vec<LedgerTransaction> {
    enum Source<'a> {
        Manual(usize, &'a ManualTransaction),
        Generated(&'a LedgerTransaction),
    }
    let provenances: Vec<Provenance> = (0..manual.len()).map(|seq| Provenance::Manual { seq }).collect();
    let mut order: Vec<(Date, &Provenance, Source)> =
        manual.iter().enumerate().map(|(seq, t)| (t.date, &provenances[seq], Source::Manual(seq, t))).collect();
    order.extend(generated.iter().map(|t| (t.date, &t.provenance, Source::Generated(t))));
    // Stable, like the journal's own order.
    order.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));

    let mut balances: BTreeMap<(String, String), Decimal> = BTreeMap::new();
    let mut problems = Problems::default();
    let mut books = Vec::with_capacity(order.len());
    for (_, _, source) in order {
        let tx = match source {
            Source::Manual(seq, t) => fill_in(seq, t, currency, &balances),
            Source::Generated(t) => t.clone(),
        };
        let at = match source {
            Source::Manual(..) => format!("{} {} (config/transactions.toml)", tx.date, tx.description),
            Source::Generated(_) => format!("{} {}", tx.date, tx.description),
        };
        let mut sums: BTreeMap<&str, Decimal> = BTreeMap::new();
        for p in &tx.postings {
            match &p.total_cost {
                Some((cost, commodity)) => {
                    *sums.entry(commodity).or_default() += if p.amount.is_sign_negative() { -cost } else { *cost }
                }
                None => *sums.entry(&p.commodity).or_default() += p.amount,
            }
        }
        let off: Vec<String> = sums.iter().filter(|(_, v)| !v.is_zero()).map(|(c, v)| fmt_amount(*v, c)).collect();
        if !off.is_empty() {
            problems.unbalanced.push(format!("{at}: off by {}", off.join(", ")));
        }
        for p in &tx.postings {
            if !declared.contains(&p.account) {
                problems.accounts.entry(p.account.clone()).or_insert_with(|| at.clone());
            }
            if p.commodity != currency {
                problems.commodities.entry(p.commodity.clone()).or_insert_with(|| at.clone());
            }
            let balance = balances.entry((p.account.clone(), p.commodity.clone())).or_default();
            *balance += p.amount;
            if let Some(expected) = p.assert_balance {
                problems.assertions += 1;
                if *balance != expected {
                    problems.failed.push(format!(
                        "{at}: {} is {}, asserted {}",
                        p.account,
                        fmt_amount(*balance, &p.commodity),
                        fmt_amount(expected, &p.commodity)
                    ));
                }
            }
        }
        books.push(tx);
    }
    problems.report(books.len(), currency, findings);
    books
}

#[derive(Default)]
struct Problems {
    unbalanced: Vec<String>,
    /// Balance assertions checked.
    assertions: usize,
    failed: Vec<String>,
    /// Undeclared, with the first transaction that uses it.
    accounts: BTreeMap<String, String>,
    commodities: BTreeMap<String, String>,
}

impl Problems {
    fn report(self, count: usize, currency: &str, findings: &mut Findings) {
        let mut ok = true;
        let mut error = |message: String, details: Vec<String>| {
            ok = false;
            findings.error("books", message).details(details);
        };
        if !self.unbalanced.is_empty() {
            error(format!("{} transactions don't balance", self.unbalanced.len()), self.unbalanced);
        }
        if !self.failed.is_empty() {
            let message = format!("{} of {} balance assertions fail", self.failed.len(), self.assertions);
            error(message, self.failed);
        }
        let first = |m: BTreeMap<String, String>| m.into_iter().map(|(a, at)| format!("{a}  (first: {at})")).collect();
        if !self.accounts.is_empty() {
            error("accounts posted to aren't declared in config/accounts.toml".into(), first(self.accounts));
        }
        if !self.commodities.is_empty() {
            error(format!("amounts not in the books' currency, {currency}"), first(self.commodities));
        }
        if ok {
            findings.ok(
                "books",
                format!("{count} transactions balance, {} balance assertions hold, accounts declared", self.assertions),
            );
        }
    }
}

/// A hand-written transaction with its amounts filled in, given the balances
/// before it: a balance assignment gets the amount that makes its balance
/// hold, and then `rest` gets what balances the transaction.
fn fill_in(
    seq: usize,
    t: &ManualTransaction,
    currency: &str,
    balances: &BTreeMap<(String, String), Decimal>,
) -> LedgerTransaction {
    // Balances as of each posting, counting the transaction's earlier postings.
    let mut here: BTreeMap<&str, Decimal> = BTreeMap::new();
    let mut postings: Vec<Posting> = Vec::new();
    for p in &t.postings {
        let before = here
            .get(p.account.as_str())
            .or(balances.get(&(p.account.clone(), currency.to_string())))
            .copied()
            .unwrap_or_default();
        let amount = match (p.amount, p.balance) {
            (ManualAmount::Fixed(a), _) => a,
            (ManualAmount::ToBalance, Some(target)) => target - before,
            // Filled in below. A posting with `rest` has no balance to assert.
            (ManualAmount::Rest, _) | (ManualAmount::ToBalance, None) => Decimal::ZERO,
        };
        here.insert(&p.account, before + amount);
        let mut posting = Posting::new(&p.account, amount, currency);
        posting.assert_balance = p.balance;
        postings.push(posting);
    }
    if let Some(i) = t.postings.iter().position(|p| p.amount == ManualAmount::Rest) {
        postings[i].amount = -postings.iter().map(|p| p.amount).sum::<Decimal>();
    }
    LedgerTransaction {
        date: t.date,
        description: t.payee.clone(),
        tags: t.tags.clone(),
        comments: t.note.iter().cloned().collect(),
        postings,
        provenance: Provenance::Manual { seq },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manual::Manual;
    use jiff::civil::date;

    fn manual(transactions: &str) -> Manual {
        Manual::parse(r#"accounts = ["assets:bank", "expenses:x", "equity:opening"]"#, transactions)
            .expect("the test's transactions parse")
    }

    fn generated(day: i8, amount: i64, balance: i64) -> LedgerTransaction {
        let mut bank = Posting::new("assets:bank", Decimal::from(amount), "DKK");
        bank.assert_balance = Some(Decimal::from(balance));
        LedgerTransaction {
            date: date(2026, 1, day),
            description: format!("Bank {day}"),
            tags: vec![],
            comments: vec![],
            postings: vec![bank, Posting::new("expenses:x", Decimal::from(-amount), "DKK")],
            provenance: Provenance::Bank { source: "a".into(), seq: usize::try_from(day).expect("a day of the month") },
        }
    }

    fn check(manual: &Manual, generated: &[LedgerTransaction]) -> (Vec<LedgerTransaction>, String) {
        let mut findings = Findings::default();
        let books = assemble(&manual.txs, generated, &manual.accounts, "DKK", &mut findings);
        (books, findings.render(true))
    }

    #[test]
    fn fills_in_balance_assignments_and_the_rest() {
        let manual = manual(
            r#"
            [[transaction]]
            date = 2026-01-05
            payee = "Written off"
            postings = [
                { account = "assets:bank", balance = 0 },
                { account = "expenses:x", rest = true },
            ]
            "#,
        );
        let (books, report) = check(&manual, &[generated(1, 1000, 1000), generated(3, -300, 700)]);
        assert!(report.contains("3 transactions balance, 3 balance assertions hold"), "{report}");
        let written_off = &books[2];
        assert_eq!(written_off.postings[0].amount, Decimal::from(-700));
        assert_eq!(written_off.postings[0].assert_balance, Some(Decimal::ZERO));
        assert_eq!(written_off.postings[1].amount, Decimal::from(700));
        assert_eq!(written_off.provenance, Provenance::Manual { seq: 0 });
    }

    #[test]
    fn hand_written_transactions_come_after_the_banks_on_their_date() {
        // The bank's assertion on the 2nd doesn't see the hand-written posting
        // on the 2nd; the one on the 3rd does.
        let manual = manual(
            r#"
            [[transaction]]
            date = 2026-01-02
            payee = "By hand"
            postings = [{ account = "assets:bank", amount = 5 }, { account = "equity:opening", rest = true }]
            "#,
        );
        let (books, report) = check(&manual, &[generated(2, 100, 100), generated(3, -10, 90)]);
        let order: Vec<&str> = books.iter().map(|t| t.description.as_str()).collect();
        assert_eq!(order, ["Bank 2", "By hand", "Bank 3"]);
        assert!(report.contains("1 of 2 balance assertions fail"), "{report}");
        assert!(report.contains("2026-01-03 Bank 3: assets:bank is 95.00 DKK, asserted 90.00 DKK"), "{report}");
    }

    #[test]
    fn reports_unbalanced_and_undeclared() {
        let manual = manual(
            r#"
            [[transaction]]
            date = 2026-01-01
            payee = "Off"
            postings = [{ account = "assets:bank", amount = 1 }, { account = "expenses:x", amount = -2 }]

            [[transaction]]
            date = 2026-01-03
            payee = "Typo"
            postings = [{ account = "assets:bnak", amount = 1 }, { account = "expenses:x", rest = true }]
            "#,
        );
        let mut euro = generated(4, 1, 1);
        euro.postings[1].commodity = "EUR".into();
        let (_, report) = check(&manual, &[euro]);
        assert!(report.contains("2026-01-01 Off (config/transactions.toml): off by -1.00 DKK"), "{report}");
        assert!(report.contains("assets:bnak  (first: 2026-01-03 Typo (config/transactions.toml))"), "{report}");
        assert!(report.contains("not in the books' currency, DKK"), "{report}");
        assert!(report.contains("EUR  (first: 2026-01-04 Bank 4)"), "{report}");
        assert!(!report.contains("✓ books"), "{report}");
    }

    /// A balance assignment starts from where the account is after the
    /// transaction's own earlier postings to it.
    #[test]
    fn a_balance_assignment_counts_the_transactions_earlier_postings() {
        let manual = manual(
            r#"
            [[transaction]]
            date = 2026-01-05
            payee = "Moved and written off"
            postings = [
                { account = "expenses:x", amount = 10 },
                { account = "expenses:x", balance = 0 },
                { account = "equity:opening", rest = true },
            ]
            "#,
        );
        // The bank transaction left expenses:x at -1000.
        let (books, report) = check(&manual, &[generated(1, 1000, 1000)]);
        assert!(report.contains("2 transactions balance, 2 balance assertions hold"), "{report}");
        let amounts: Vec<Decimal> = books[1].postings.iter().map(|p| p.amount).collect();
        assert_eq!(amounts, [Decimal::from(10), Decimal::from(990), Decimal::from(-1000)]);
    }
}
