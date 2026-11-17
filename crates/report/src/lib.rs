//! The site `konter build` and `konter report` write to `build/` ([`site`]), and
//! its spending and income pages: amounts per account and month, for reading
//! in a browser. They're one page with different rows ([`Side`]), built from
//! the finished books, hand-written entries included.
//! The page is self-contained: the postings are embedded as JSON and the
//! page's script does the rest.
//!
//! Besides expenses, the spending page shows money moved into accounts set aside for
//! something (such as donations), so the page can show where money went, not
//! only what was spent. Expenses paid from such an account are marked, since
//! the money already counted when it was set aside.
//!
//! It reads only the books, as [`Transaction`]s, and the [`Options`] it's
//! given: nothing here knows how the books are made.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use jiff::civil::Date;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::Serialize;

pub mod site;
use site::Page;

const TEMPLATE: &str = include_str!("page.html");
const DATA_PLACEHOLDER: &str = "__REPORT_DATA__";

/// The page's account for money set aside in an account: `set-aside:<name>`.
const SET_ASIDE: &str = "set-aside";

/// Which side of the books a page shows, per account and month. Both pages
/// are the same page with different rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Expense accounts, and money moved into accounts set aside.
    Spending,
    /// Income accounts, shown as positive amounts.
    Income,
}

impl Side {
    /// The top-level account the page reads.
    fn root(self) -> &'static str {
        match self {
            Side::Spending => "expenses",
            Side::Income => "income",
        }
    }

    fn page(self) -> Page {
        match self {
            Side::Spending => Page::Spending,
            Side::Income => Page::Income,
        }
    }

    /// What the page's script calls it.
    fn name(self) -> &'static str {
        match self {
            Side::Spending => "spending",
            Side::Income => "income",
        }
    }
}

/// One posting to an expense (or income) account, or money moved into an
/// account set aside for something.
#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Row {
    /// Date: the transaction's.
    pub d: String,
    /// Account: an expense or income account, or `set-aside:<name>`.
    pub a: String,
    /// Amount in hundredths (øre), so sums in the page are exact. Income is
    /// positive: what was earned.
    pub v: i64,
    /// Payee: the transaction's description.
    pub p: String,
    /// The bank's own text, when the payee replaced it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b: Option<String>,
    /// The bank transaction's ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The subscription whose due date this is.
    #[serde(skip)]
    pub sub: Option<String>,
    /// For the due date of a subscription billed less often than monthly: how
    /// many months it covers. The page can spread it over those months.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub m: Option<i32>,
    /// An expense paid from an account set aside for it: already counted when
    /// the money was set aside.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub x: bool,
}

/// A transaction in the books: what the pages read. Every amount is filled in.
#[derive(Clone, Debug)]
pub struct Transaction {
    pub date: Date,
    pub description: String,
    /// Tags as (name, value).
    pub tags: Vec<(String, String)>,
    /// Comment lines below the header.
    pub comments: Vec<String>,
    pub postings: Vec<Posting>,
}

impl Transaction {
    fn tag(&self, name: &str) -> Option<String> {
        self.tags.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())
    }
}

#[derive(Clone, Debug)]
pub struct Posting {
    pub account: String,
    pub amount: Decimal,
    pub commodity: String,
}

/// An account marked `set_aside`: its source's name and its ledger account.
#[derive(Clone, Debug)]
pub struct SetAside {
    pub source: String,
    pub ledger: String,
}

/// What the page needs besides the books. It knows nothing of how the books
/// are made: whoever writes the page works these out.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The books' currency; every amount must be in it.
    pub currency: String,
    /// The date the books are built for, e.g. "2026-11-13".
    pub as_of: String,
    /// Accounts set aside for something (such as donations).
    pub set_aside: Vec<SetAside>,
    /// Subscriptions billed less often than monthly, with how many months each
    /// due date covers: the page can spread those over the months.
    pub spans: BTreeMap<String, i32>,
}

#[derive(Serialize)]
struct Data<'a> {
    kind: &'a str,
    #[serde(rename = "asOf")]
    as_of: &'a str,
    currency: &'a str,
    rows: &'a [Row],
}

/// Writes the spending and income pages into `site_dir` for `books`.
pub fn write(books: &[Transaction], site_dir: &Path, options: &Options) -> Result<()> {
    for side in [Side::Spending, Side::Income] {
        let rows = rows(books, side, &options.currency, &options.set_aside)?;
        let rows = mark_spans(rows, &options.spans);
        site::write(site_dir, side.page(), &render(side, &rows, &options.as_of, &options.currency))?;
    }
    Ok(())
}

/// The rows for the side's page: postings to the side's accounts, and for
/// spending, postings that move money into a `set_aside` account from one of
/// your other accounts.
pub fn rows(books: &[Transaction], side: Side, currency: &str, set_aside: &[SetAside]) -> Result<Vec<Row>> {
    // Money set aside is part of spending only.
    let set_aside = if side == Side::Spending { set_aside } else { &[] };
    // Income is booked as negative amounts; the page shows what was earned.
    let sign = if side == Side::Income { -1 } else { 1 };
    let under = |a: &str, root: &str| a == root || a.starts_with(&format!("{root}:"));
    let pot = |a: &str| set_aside.iter().find(|s| s.ledger == a);
    let mut rows = Vec::new();
    for tx in books {
        let (date, payee) = (tx.date.to_string(), &tx.description);
        // The engine writes the bank's text as `bank: "…"` (a quoted string).
        let bank = tx.comments.iter().find_map(|c| {
            let text = c.trim().strip_prefix("bank: ")?;
            Some(serde_json::from_str::<String>(text).unwrap_or_else(|_| text.to_string()))
        });
        let id = tx.tag("bank-id");
        // A due date is generated, so it has no bank ID; payments do.
        let sub = if id.is_none() { tx.tag("subscription") } else { None };
        let pots: Vec<&SetAside> = tx.postings.iter().filter_map(|p| pot(&p.account)).collect();
        // Money only moves into a pot when nothing is earned or spent on the
        // way, it isn't an opening balance, and it doesn't come from another pot.
        let moves_money = !tx
            .postings
            .iter()
            .any(|p| under(&p.account, "expenses") || under(&p.account, "income") || under(&p.account, "equity"))
            && pots.len() == 1
            && tx.tag("transfer").is_none_or(|from| !set_aside.iter().any(|s| s.source == from));
        for posting in &tx.postings {
            let account = &posting.account;
            let (a, x) = if under(account, side.root()) {
                (account.clone(), !pots.is_empty())
            } else if let Some(p) = pot(account).filter(|_| moves_money) {
                (format!("{SET_ASIDE}:{}", p.ledger.rsplit(':').next().unwrap_or(&p.ledger)), false)
            } else {
                continue;
            };
            if posting.commodity != currency {
                bail!("{date} {payee:?}: {account} is in {:?}, not {currency}", posting.commodity);
            }
            let v = sign * hundredths(posting.amount).with_context(|| format!("{date} {payee:?}: amount"))?;
            if v == 0 {
                continue;
            }
            rows.push(Row {
                d: date.clone(),
                a,
                v,
                p: payee.clone(),
                b: bank.clone(),
                id: id.clone(),
                sub: sub.clone(),
                x,
                ..Row::default()
            });
        }
    }
    Ok(rows)
}

/// Marks each due date of a subscription in `spans` with how many months it
/// covers. The page spreads it over the months shown (see the page's script),
/// which it can only do knowing which months those are.
pub fn mark_spans(mut rows: Vec<Row>, spans: &BTreeMap<String, i32>) -> Vec<Row> {
    for row in &mut rows {
        row.m = row.sub.as_ref().and_then(|s| spans.get(s)).copied();
    }
    rows
}

/// An amount in hundredths.
fn hundredths(value: Decimal) -> Result<i64> {
    (value * Decimal::ONE_HUNDRED).round().to_i64().context("amount out of range")
}

/// The side's page, with `rows` embedded.
pub fn render(side: Side, rows: &[Row], as_of: &str, currency: &str) -> String {
    let json = serde_json::to_string(&Data { kind: side.name(), as_of, currency, rows }).expect("rows serialize");
    // Inside <script>, "</" could end the element early; "<\/" is the same JSON.
    // The data goes in last, so nothing in it is taken for a placeholder.
    TEMPLATE
        .replace("__TITLE__", side.page().title())
        .replace("__SITE_CSS__", site::CSS)
        .replace("__NAV__", &site::nav(side.page()))
        .replace(DATA_PLACEHOLDER, &json.replace("</", "<\\/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A transaction with tags written `name:value` and postings in whole
    /// hundredths.
    fn tx(date: &str, payee: &str, tags: &[&str], comments: &[&str], postings: &[(&str, i64)]) -> Transaction {
        Transaction {
            date: date.parse().expect("the test's date parses"),
            description: payee.into(),
            tags: tags
                .iter()
                .map(|t| t.split_once(':').expect("the test's tags are written name:value"))
                .map(|(n, v)| (n.into(), v.into()))
                .collect(),
            comments: comments.iter().map(|c| c.to_string()).collect(),
            postings: postings
                .iter()
                .map(|(a, v)| Posting { account: a.to_string(), amount: Decimal::new(*v, 2), commodity: "DKK".into() })
                .collect(),
        }
    }

    fn books() -> Vec<Transaction> {
        vec![
            tx(
                "2026-10-12",
                "Rejsekort",
                &["bank-id:ns-2026-10-12-4f2a9c", "rule:rejsekort-spending"],
                &[r#"bank: "40001234567890000000001""#],
                &[("assets:bank:nordea:spending", -3600), ("expenses:transport:rejsekort", 3600)],
            ),
            tx("2026-10-01", "Sam: their part", &[], &[], &[("expenses:eating-out", -15000)]),
        ]
    }

    #[test]
    fn reads_expense_postings() {
        let found = rows(&books(), Side::Spending, "DKK", &[]).expect("the rows are made");
        assert_eq!(
            found,
            [
                Row {
                    d: "2026-10-12".into(),
                    a: "expenses:transport:rejsekort".into(),
                    v: 3600,
                    p: "Rejsekort".into(),
                    b: Some("40001234567890000000001".into()),
                    id: Some("ns-2026-10-12-4f2a9c".into()),
                    ..Row::default()
                },
                Row {
                    d: "2026-10-01".into(),
                    a: "expenses:eating-out".into(),
                    v: -15000,
                    p: "Sam: their part".into(),
                    ..Row::default()
                },
            ]
        );
        let error = rows(&books(), Side::Spending, "EUR", &[]).expect_err("another currency is refused").to_string();
        assert!(error.contains("not EUR"), "{error}");
    }

    #[test]
    fn shows_income_as_positive_and_nothing_else() {
        let salary = tx(
            "2026-10-31",
            "Salary",
            &[],
            &[],
            &[
                ("assets:bank:nordea:main", 3000000),
                ("income:salary", -3000000),
                ("expenses:tax", 10000),
                ("assets:bank:nordea:main", -10000),
            ],
        );
        let pots = [SetAside { source: "nordea:main".into(), ledger: "assets:bank:nordea:main".into() }];
        let found = rows(&[salary], Side::Income, "DKK", &pots).expect("the rows are made");
        assert_eq!(
            found,
            [Row {
                d: "2026-10-31".into(),
                a: "income:salary".into(),
                v: 3000000,
                p: "Salary".into(),
                ..Row::default()
            }]
        );
        assert!(rows(&books(), Side::Income, "DKK", &[]).expect("the rows are made").is_empty());
    }

    #[test]
    fn embeds_the_rows_safely() {
        let rows = vec![Row {
            d: "2026-01-01".into(),
            a: "expenses:x".into(),
            v: 100,
            p: "</script>".into(),
            ..Row::default()
        }];
        let page = render(Side::Spending, &rows, "2026-11-13", "DKK");
        assert!(!page.contains(DATA_PLACEHOLDER));
        assert!(page.contains(r#""asOf":"2026-11-13""#));
        assert!(page.contains(r#""p":"<\/script>""#));
        assert_eq!(page.matches("</script>").count(), TEMPLATE.matches("</script>").count());
    }

    #[test]
    fn marks_the_months_a_due_date_covers() {
        let due = |sub: Option<&str>| Row {
            d: "2025-11-10".into(),
            v: 25900,
            sub: sub.map(str::to_string),
            ..Row::default()
        };
        let spans = BTreeMap::from([("yearly".to_string(), 12), ("quarterly".to_string(), 3)]);
        let rows =
            mark_spans(vec![due(Some("yearly")), due(Some("quarterly")), due(Some("monthly")), due(None)], &spans);
        assert_eq!(rows.iter().map(|r| r.m).collect::<Vec<_>>(), [Some(12), Some(3), None, None]);
        assert!(serde_json::to_string(&rows[0]).expect("a row is JSON").contains(r#""m":12"#));
        assert!(!serde_json::to_string(&rows[2]).expect("a row is JSON").contains(r#""m""#));
    }

    #[test]
    fn money_set_aside_counts_once() {
        let pot = "assets:bank:lunar:charity";
        let books = [
            // Moved in from the main account (both sides imported: through assets:transfers).
            tx(
                "2026-10-30",
                "MONTHLY TRANSFER",
                &["transfer:nordea:main"],
                &[],
                &[(pot, 200000), ("assets:transfers", -200000)],
            ),
            // The other side of that move doesn't touch the pot.
            tx(
                "2026-10-30",
                "Lunar - Charity",
                &["transfer:lunar:charity"],
                &[],
                &[("assets:bank:nordea:main", -200000), ("assets:transfers", 200000)],
            ),
            // Given away from the pot: an expense, marked as paid from it.
            tx(
                "2026-11-05",
                "Example Charity",
                &[],
                &[],
                &[(pot, -150000), ("expenses:donations:example-charity", 150000)],
            ),
            // Interest earned in the pot isn't money set aside.
            tx("2026-12-31", "Rente", &[], &[], &[(pot, 100), ("income:interest", -100)]),
            // Nor is the opening balance.
            tx("2025-01-01", "Opening balance", &[], &[], &[(pot, 1000), ("equity:opening-balances", -1000)]),
            // Nor money moved between two pots.
            tx("2026-12-01", "Between pots", &[], &[], &[(pot, 1000), ("assets:bank:savings", -1000)]),
        ];
        let pots = [
            SetAside { source: "lunar:charity".into(), ledger: pot.into() },
            SetAside { source: "bank:savings".into(), ledger: "assets:bank:savings".into() },
        ];
        let found = rows(&books, Side::Spending, "DKK", &pots).expect("the rows are made");
        let summary: Vec<(&str, i64, bool)> = found.iter().map(|r| (r.a.as_str(), r.v, r.x)).collect();
        assert_eq!(
            summary,
            [("set-aside:charity", 200000, false), ("expenses:donations:example-charity", 150000, true)]
        );
    }
}
