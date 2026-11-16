//! Subscriptions: what *should* happen on a schedule (docs/spec.md §8).
//!
//! Each due date books the price into a per-subscription balance account
//! (`liabilities:payable:<name>` or `assets:receivable:<name>`); bank payments
//! posting to that account settle it. A missing payment shows up as a deficit.
//!
//! A transfer subscription expects money to move between two of your own
//! accounts. Its due dates book nothing, since moving money changes neither
//! income nor spending; the transfers themselves pay it, whatever their size,
//! so paying ahead or catching up later both work.
//!
//! A variable expense or income (`variable = true`), such as electricity, has
//! an estimate for a price. Payments settle due dates oldest first, one each,
//! and a settled due date books what was paid; the others book the estimate.

use std::fmt::Write as _;

use anyhow::{Result, bail};
use jiff::civil::Date;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::config::Config;
use crate::dates::{ConfigDate, Period};
use crate::engine::{Generated, OPENING};
use crate::findings::Findings;
use crate::journal;
use crate::model::{LedgerTransaction, Posting, Provenance};
use crate::money::{Amount, fmt_amount, fmt_num};
use crate::tags;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Expense,
    Income,
    Transfer,
}

/// What a subscription expects to happen on each due date.
#[derive(Debug)]
pub enum Flow {
    /// You pay: the due date books an expense to this account and a payable.
    Expense(String),
    /// You get paid: the due date books a receivable and income to this account.
    Income(String),
    /// You move money from one of your sources to another. Due dates book
    /// nothing; transfers from `from` to `to` pay it.
    Transfer { from: String, to: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawSubscription {
    name: String,
    kind: Kind,
    account: Option<String>,
    from: Option<String>,
    to: Option<String>,
    every: Period,
    first: ConfigDate,
    end: Option<ConfigDate>,
    notice: Option<Period>,
    grace: Option<Period>,
    prices: Vec<RawPrice>,
    payee: Option<String>,
    #[serde(default)]
    opening: Option<Amount>,
    #[serde(default)]
    variable: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPrice {
    from: ConfigDate,
    amount: Amount,
}

#[derive(Debug)]
pub struct Subscription {
    pub name: String,
    pub flow: Flow,
    pub every: Period,
    pub first: Date,
    /// Last due date, inclusive.
    pub end: Option<Date>,
    pub notice: Option<Period>,
    pub grace_days: i64,
    /// (from, amount), sorted by date.
    pub prices: Vec<(Date, Decimal)>,
    pub payee: String,
    /// Paid in advance at the start of the books (positive) or owed (negative).
    pub opening: Decimal,
    /// The amount differs every time and `prices` are estimates: each payment
    /// settles one due date, which books what was paid.
    pub variable: bool,
    pub file: String,
}

const DEFAULT_GRACE_DAYS: i64 = 7;

impl RawSubscription {
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Subscription {
    pub fn from_raw(raw: RawSubscription, file: &str) -> Result<Subscription> {
        if raw.prices.is_empty() {
            bail!("needs at least one price");
        }
        let mut prices: Vec<(Date, Decimal)> = raw.prices.iter().map(|p| (p.from.0, p.amount.0)).collect();
        prices.sort();
        if prices.windows(2).any(|w| w[0].0 == w[1].0) {
            bail!("two prices start on the same date");
        }
        if prices[0].0 > raw.first.0 {
            bail!("no price for the first due date {}", raw.first.0);
        }
        if let Some(end) = raw.end
            && end.0 < raw.first.0
        {
            bail!("end is before first");
        }
        let grace_days = match raw.grace {
            None => DEFAULT_GRACE_DAYS,
            Some(Period::Days(d)) => d as i64,
            Some(Period::Months(_)) => bail!("grace must be in days or weeks"),
        };
        let flow = match (raw.kind, raw.account, raw.from, raw.to) {
            (Kind::Expense, Some(account), None, None) => Flow::Expense(account),
            (Kind::Income, Some(account), None, None) => Flow::Income(account),
            (Kind::Transfer, None, Some(from), Some(to)) => {
                if from == to {
                    bail!("from and to are the same source");
                }
                Flow::Transfer { from, to }
            }
            (Kind::Transfer, ..) => bail!("a transfer needs `from` and `to` (sources), and no `account`"),
            _ => bail!("an expense or income needs `account`, and no `from` or `to`"),
        };
        if raw.variable {
            if matches!(flow, Flow::Transfer { .. }) {
                bail!("a transfer is paid by transfers of any size already; `variable` is for expenses and income");
            }
            if raw.opening.is_some() {
                bail!("`opening` isn't supported with `variable`");
            }
        }
        Ok(Subscription {
            payee: raw.payee.unwrap_or_else(|| raw.name.clone()),
            name: raw.name,
            flow,
            every: raw.every,
            first: raw.first.0,
            end: raw.end.map(|d| d.0),
            notice: raw.notice,
            grace_days,
            prices,
            opening: raw.opening.map(|a| a.0).unwrap_or_default(),
            variable: raw.variable,
            file: file.to_string(),
        })
    }

    pub fn kind_name(&self) -> &'static str {
        match self.flow {
            Flow::Expense(_) => "expense",
            Flow::Income(_) => "income",
            Flow::Transfer { .. } => "transfer",
        }
    }

    /// The expense or income account; a transfer has none.
    pub fn account(&self) -> Option<&str> {
        match &self.flow {
            Flow::Expense(a) | Flow::Income(a) => Some(a),
            Flow::Transfer { .. } => None,
        }
    }

    /// Where due dates and payments meet; a transfer has none.
    pub fn balance_account(&self) -> Option<String> {
        match self.flow {
            Flow::Expense(_) => Some(format!("liabilities:payable:{}", self.name)),
            Flow::Income(_) => Some(format!("assets:receivable:{}", self.name)),
            Flow::Transfer { .. } => None,
        }
    }

    /// The (debit, credit) accounts of one due date: what the books get for an
    /// expense or income, and the bank accounts the money moves between for a
    /// transfer (which only the forecast books).
    fn legs(&self, config: &Config) -> (String, String) {
        match &self.flow {
            Flow::Expense(a) => (a.clone(), format!("liabilities:payable:{}", self.name)),
            Flow::Income(a) => (format!("assets:receivable:{}", self.name), a.clone()),
            Flow::Transfer { from, to } => (config.sources[to].ledger.clone(), config.sources[from].ledger.clone()),
        }
    }

    pub fn price_at(&self, date: Date) -> Decimal {
        self.prices.iter().rev().find(|(from, _)| *from <= date).map(|p| p.1).unwrap_or(self.prices[0].1)
    }

    /// Due dates from `first`, up to and including `until` (and `end`).
    pub fn due_dates(&self, until: Date) -> Vec<Date> {
        let last = self.end.map_or(until, |e| e.min(until));
        (0..).map(|k| self.every.nth(self.first, k)).take_while(|d| *d <= last).collect()
    }

    /// The first due date after `date`, if the subscription hasn't ended.
    pub fn next_due(&self, date: Date) -> Option<Date> {
        let next = (0..).map(|k| self.every.nth(self.first, k)).find(|d| *d > date)?;
        self.end.is_none_or(|e| next <= e).then_some(next)
    }

    /// Signs a "credit" (paid minus due, positive = paid ahead) as a posting
    /// amount on the balance account.
    fn signed(&self, credit: Decimal) -> Decimal {
        match self.flow {
            Flow::Income(_) => -credit,
            Flow::Expense(_) | Flow::Transfer { .. } => credit,
        }
    }

    /// What each bank transaction up to `as_of` paid to the balance account,
    /// in date order and signed so that paying is positive.
    fn payments(&self, balance_account: &str, txs: &[LedgerTransaction], as_of: Date) -> Vec<(Date, Decimal)> {
        let mut payments: Vec<(Date, Decimal)> = txs
            .iter()
            .filter(|tx| matches!(tx.provenance, Provenance::Bank { .. }) && tx.date <= as_of)
            .filter_map(|tx| {
                let mut postings = tx.postings.iter().filter(|p| p.account == balance_account).peekable();
                postings.peek()?;
                Some((tx.date, self.signed(postings.map(|p| p.amount).sum())))
            })
            .collect();
        payments.sort_by_key(|p| p.0);
        payments
    }

    /// The due dates from `start` that payments settle, with what each was
    /// paid: one payment per due date, oldest first, going past `--as-of` when
    /// paid ahead. Only a variable subscription settles due dates this way.
    fn settled(&self, start: Date, payments: &[(Date, Decimal)]) -> Vec<(Date, Decimal)> {
        if !self.variable {
            return Vec::new();
        }
        let dues = (0..)
            .map(|k| self.every.nth(self.first, k))
            .filter(|d| *d >= start)
            .take_while(|d| self.end.is_none_or(|e| *d <= e));
        dues.zip(payments.iter().map(|p| p.1)).collect()
    }

    /// The due dates from `start` up to `as_of`, with what each books: what was
    /// paid for a settled one, the price otherwise.
    fn dues(&self, start: Date, as_of: Date, settled: &[(Date, Decimal)]) -> Vec<(Date, Decimal)> {
        let dues = self.due_dates(as_of).into_iter().filter(|d| *d >= start);
        dues.enumerate().map(|(i, d)| (d, settled.get(i).map_or_else(|| self.price_at(d), |s| s.1))).collect()
    }

    /// The last due date `total` covers, paying due dates oldest first and
    /// going past `--as-of` if there's enough.
    fn paid_until(&self, start: Date, total: Decimal) -> Option<Date> {
        let mut remaining = total;
        let mut last = None;
        for d in (0..).map(|k| self.every.nth(self.first, k)) {
            if self.end.is_some_and(|e| d > e) {
                break;
            }
            if d < start {
                continue;
            }
            let price = self.price_at(d);
            if price <= Decimal::ZERO || remaining < price {
                break;
            }
            remaining -= price;
            last = Some(d);
        }
        last
    }
}

/// The bank transactions that pay a transfer subscription, with the amount
/// each moves: transfers from `from` to `to`, as booked on the `from` side.
/// Money moved back from `to` counts as negative.
fn transfer_payments<'a>(
    config: &'a Config,
    from: &'a str,
    to: &'a str,
    txs: &'a [LedgerTransaction],
) -> impl Iterator<Item = (usize, Decimal)> + 'a {
    let from_ledger = &config.sources[from].ledger;
    let to_ledger = &config.sources[to].ledger;
    txs.iter().enumerate().filter_map(move |(i, tx)| {
        let Provenance::Bank { source, .. } = &tx.provenance else { return None };
        // An imported target goes through assets:transfers with a tag; one
        // without data is posted to directly.
        let to_target = tx.tags.iter().any(|(k, v)| k == tags::TRANSFER && v == to)
            || tx.postings.iter().any(|p| p.account == *to_ledger);
        (source == from && to_target).then(|| {
            let moved: Decimal = tx.postings.iter().filter(|p| p.account == *from_ledger).map(|p| -p.amount).sum();
            (i, moved)
        })
    })
}

/// Where a subscription stands at `--as-of`.
#[derive(Debug)]
pub struct State {
    pub name: String,
    pub price: Decimal,
    /// Paid minus due; positive = paid ahead, negative = owed.
    pub credit: Decimal,
    pub oldest_unpaid: Option<Date>,
    pub next_due: Option<Date>,
    /// The last due date the payments cover, possibly after as-of: until
    /// then nothing more is owed.
    pub paid_until: Option<Date>,
    /// Committed until (as-of + notice), capped by `end`.
    pub committed_until: Option<Date>,
}

/// Adds the due-date and opening transactions, tags the transfers that pay
/// transfer subscriptions, then checks every subscription.
pub fn generate(
    config: &Config,
    subs: &[Subscription],
    as_of: Date,
    g: &mut Generated,
    findings: &mut Findings,
) -> Vec<State> {
    let start = config.start();
    let commodity = config.books.currency.clone();
    for s in subs {
        let Some(balance_account) = s.balance_account() else {
            let Flow::Transfer { from, to } = &s.flow else { unreachable!("only transfers have no balance account") };
            let paying: Vec<usize> = transfer_payments(config, from, to, &g.txs).map(|(i, _)| i).collect();
            for i in paying {
                g.txs[i].tags.push((tags::PAYS.into(), s.name.clone()));
            }
            continue;
        };
        g.tool_accounts.insert(balance_account.clone());
        if !s.opening.is_zero() {
            g.tool_accounts.insert(OPENING.into());
            g.txs.push(LedgerTransaction {
                date: start,
                description: format!("{}: opening balance", s.payee),
                tags: vec![(tags::SUBSCRIPTION.into(), s.name.clone())],
                comments: Vec::new(),
                postings: vec![
                    Posting::new(&balance_account, s.signed(s.opening), &commodity),
                    Posting::new(OPENING, -s.signed(s.opening), &commodity),
                ],
                provenance: Provenance::Opening,
            });
        }
        let (debit, credit) = s.legs(config);
        let settled = s.settled(start, &s.payments(&balance_account, &g.txs, as_of));
        for (due, price) in s.dues(start, as_of, &settled) {
            let postings = vec![Posting::new(&debit, price, &commodity), Posting::new(&credit, -price, &commodity)];
            g.txs.push(LedgerTransaction {
                date: due,
                description: s.payee.clone(),
                tags: vec![(tags::SUBSCRIPTION.into(), s.name.clone())],
                comments: Vec::new(),
                postings,
                provenance: Provenance::Subscription,
            });
        }
    }
    subs.iter().map(|s| check(config, s, as_of, g, findings)).collect()
}

fn check(config: &Config, s: &Subscription, as_of: Date, g: &Generated, findings: &mut Findings) -> State {
    let (paid, settled): (Decimal, _) = match (&s.flow, s.balance_account()) {
        (Flow::Transfer { from, to }, _) => {
            let paid = transfer_payments(config, from, to, &g.txs)
                .filter(|(i, _)| g.txs[*i].date <= as_of)
                .map(|(_, moved)| moved)
                .sum();
            (paid, Vec::new())
        }
        (_, Some(balance_account)) => {
            let payments = s.payments(&balance_account, &g.txs, as_of);
            if s.variable {
                // Each payment settles a due date, so a refund mapped here
                // would settle one as a negative bill and move every later
                // payment on a period (§8: refunds go to the account itself).
                for (date, refund) in payments.iter().filter(|p| p.1 < Decimal::ZERO) {
                    findings.warn(
                        "subscription",
                        format!(
                            "{}: {} on {date} is a refund, which settles a due date like a payment and moves the \
                             later ones; map it straight to {} ({})",
                            s.name,
                            fmt_num(-refund),
                            s.account().unwrap_or("its account"),
                            s.file
                        ),
                    );
                }
            }
            (payments.iter().map(|p| p.1).sum(), s.settled(config.start(), &payments))
        }
        (_, None) => unreachable!("only transfers have no balance account"),
    };
    let dues = s.dues(config.start(), as_of, &settled);
    let due_total: Decimal = dues.iter().map(|d| d.1).sum();
    let credit = s.opening + paid - due_total;

    // First in, first out: the oldest due date the payments don't cover.
    let mut remaining = s.opening + paid;
    let mut oldest_unpaid = None;
    for (date, price) in &dues {
        remaining -= price;
        if remaining < Decimal::ZERO {
            oldest_unpaid = Some(*date);
            break;
        }
    }
    let next_due = s.next_due(as_of);
    let price = s.price_at(next_due.unwrap_or(as_of));
    let paid_until =
        if s.variable { settled.last().map(|d| d.0) } else { s.paid_until(config.start(), s.opening + paid) };
    let committed_until = s
        .notice
        .map(|n| {
            let until = n.after(as_of);
            s.end.map_or(until, |e| e.min(until))
        })
        .filter(|d| *d > as_of);

    if let Some(d) = oldest_unpaid
        && crate::transfers::days_between(as_of, d) > s.grace_days
    {
        findings.warn(
            "deficit",
            format!("{}: {} owed, unpaid since {d} ({})", s.name, fmt_amount(-credit, &config.books.currency), s.file),
        );
    }
    // Paying a transfer ahead is fine: it's your own money either way. A
    // variable amount can't be compared to the price, so count the due dates.
    let ahead = if s.variable {
        settled.iter().filter(|d| d.0 > as_of).count() > 1
    } else {
        credit > price && credit > Decimal::ZERO && !matches!(s.flow, Flow::Transfer { .. })
    };
    if ahead {
        findings.warn(
            "subscription",
            format!("{}: paid {} ahead, more than one period; paid twice?", s.name, fmt_num(credit)),
        );
    }
    if s.end.is_some_and(|e| e < as_of) && credit > Decimal::ZERO {
        findings.warn(
            "subscription",
            format!("{}: ended {}, but {} was paid beyond that", s.name, s.end.unwrap(), fmt_num(credit)),
        );
    }
    State { name: s.name.clone(), price, credit, oldest_unpaid, next_due, paid_until, committed_until }
}

/// `ledger/forecast.journal`: hledger periodic transactions for every future
/// due date, so `hledger bal --forecast` shows what's coming. A transfer is
/// forecast as the bank transfer itself, from the first due date that isn't
/// paid yet (at its full price, even if part of it is).
pub fn forecast(config: &Config, subs: &[Subscription], states: &[State], as_of: Date) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", journal::header_line(as_of));
    let _ = writeln!(out, "; Periodic transactions for future subscription due dates. Used by --forecast.");
    let commodity = &config.books.currency;
    for (s, state) in subs.iter().zip(states) {
        let (after, tag) = match s.flow {
            Flow::Transfer { .. } => (state.paid_until.map_or(as_of, |p| p.max(as_of)), tags::PAYS),
            _ => (as_of, tags::SUBSCRIPTION),
        };
        let Some(mut from) = s.next_due(after) else { continue };
        let every = match s.every {
            Period::Days(n) => format!("every {n} days"),
            Period::Months(n) => format!("every {n} months"),
        };
        loop {
            let price = s.price_at(from);
            // Until the next price change or the end, whichever comes first.
            let change = s.prices.iter().map(|p| p.0).find(|d| *d > from);
            let end_excl = s.end.map(|e| e.tomorrow().expect("date out of range"));
            let to = match (change, end_excl) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let to_text = to.map(|t| format!(" to {t}")).unwrap_or_default();
            let _ = writeln!(out, "\n~ {every} from {from}{to_text}  {}  ; {tag}:{}", s.payee, s.name);
            let (debit, credit) = s.legs(config);
            let width = debit.chars().count().max(credit.chars().count()).max(36);
            for (account, amount) in [(&debit, price), (&credit, -price)] {
                let pad = " ".repeat(width - account.chars().count());
                let _ = writeln!(out, "    {account}{pad}  {:>16}", fmt_amount(amount, commodity));
            }
            // Continue from the first due date in the next price period.
            let Some(t) = to.filter(|t| Some(*t) == change) else { break };
            let Some(next) = (0..).map(|k| s.every.nth(s.first, k)).find(|d| *d >= t) else { break };
            if s.end.is_some_and(|e| next > e) {
                break;
            }
            from = next;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn sub(toml_text: &str) -> Subscription {
        let raw: RawSubscription = toml::from_str(toml_text).unwrap();
        Subscription::from_raw(raw, "test.toml").unwrap()
    }

    #[test]
    fn schedule_and_prices() {
        let s = sub(r#"
            name = "x"
            kind = "expense"
            account = "expenses:x"
            every = "1 month"
            first = 2025-01-31
            end = 2025-05-31
            prices = [{ from = 2025-01-01, amount = 100 }, { from = 2025-03-01, amount = 120 }]
        "#);
        assert_eq!(
            s.due_dates(date(2026, 1, 1)),
            [date(2025, 1, 31), date(2025, 2, 28), date(2025, 3, 31), date(2025, 4, 30), date(2025, 5, 31)]
        );
        assert_eq!(s.price_at(date(2025, 2, 28)), Decimal::from(100));
        assert_eq!(s.price_at(date(2025, 3, 31)), Decimal::from(120));
        assert_eq!(s.next_due(date(2025, 4, 30)), Some(date(2025, 5, 31)));
        assert_eq!(s.next_due(date(2025, 5, 31)), None);
    }

    #[test]
    fn rejects_bad_config() {
        let raw: RawSubscription = toml::from_str(
            r#"
            name = "x"
            kind = "expense"
            account = "expenses:x"
            every = "1 month"
            first = 2025-01-01
            prices = [{ from = 2025-02-01, amount = 100 }]
        "#,
        )
        .unwrap();
        assert!(Subscription::from_raw(raw, "t").unwrap_err().to_string().contains("no price"));
    }

    #[test]
    fn transfers_name_two_sources_and_no_account() {
        let error = |fields: &str| {
            let raw: RawSubscription = toml::from_str(&format!(
                "name = \"x\"\n{fields}\nevery = \"1 month\"\nfirst = 2025-01-31\nprices = [{{ from = 2025-01-01, amount = 100 }}]"
            ))
            .unwrap();
            Subscription::from_raw(raw, "t").err().map(|e| e.to_string())
        };
        assert_eq!(error("kind = \"transfer\"\nfrom = \"a\"\nto = \"b\""), None);
        assert!(error("kind = \"transfer\"\nfrom = \"a\"").unwrap().contains("needs `from` and `to`"));
        assert!(
            error("kind = \"transfer\"\nfrom = \"a\"\nto = \"b\"\naccount = \"x\"").unwrap().contains("no `account`")
        );
        assert!(error("kind = \"transfer\"\nfrom = \"a\"\nto = \"a\"").unwrap().contains("same source"));
        assert!(error("kind = \"expense\"\naccount = \"x\"\nto = \"b\"").unwrap().contains("needs `account`"));
        assert!(
            error("kind = \"transfer\"\nfrom = \"a\"\nto = \"b\"\nvariable = true").unwrap().contains("for expenses")
        );
        assert_eq!(error("kind = \"income\"\naccount = \"x\"\nvariable = true"), None);
    }

    #[test]
    fn paid_until_goes_past_as_of_and_follows_prices() {
        let s = sub(r#"
            name = "x"
            kind = "transfer"
            from = "a"
            to = "b"
            every = "1 month"
            first = 2025-01-31
            end = 2025-06-30
            prices = [{ from = 2025-01-01, amount = 100 }, { from = 2025-03-01, amount = 200 }]
        "#);
        let start = date(2025, 1, 1);
        assert_eq!(s.paid_until(start, Decimal::from(99)), None);
        assert_eq!(s.paid_until(start, Decimal::from(200)), Some(date(2025, 2, 28)));
        // 100 + 100 + 200 = 400 covers March; 150 left isn't enough for April.
        assert_eq!(s.paid_until(start, Decimal::from(550)), Some(date(2025, 3, 31)));
        // Never past the end, however much is paid.
        assert_eq!(s.paid_until(start, Decimal::from(100_000)), Some(date(2025, 6, 30)));
        // Due dates before the books start don't count.
        assert_eq!(s.paid_until(date(2025, 2, 1), Decimal::from(100)), Some(date(2025, 2, 28)));
    }
}
