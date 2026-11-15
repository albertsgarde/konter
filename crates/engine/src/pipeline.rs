//! The whole pipeline (docs/spec.md §2), from inputs already read: config, rules,
//! bank data and what's written by hand in config/ (accounts, transactions), to
//! the whole books in memory and the generated journals. Reading them from the repository is the IO side's
//! job (`books`).

use jiff::civil::Date;

use crate::config::Config;
use crate::engine::{self, Generated};
use crate::findings::Findings;
use crate::journal;
use crate::manual::Manual;
use crate::model::LedgerTransaction;
use crate::rules::Rules;
use crate::sources::Loaded;

/// Sources fetched through Enable Banking are stale after this many days.
const STALE_FETCH_DAYS: i64 = 35;
/// Sources only exported by hand: suggest a new export after this many days.
const STALE_EXPORT_DAYS: i64 = 90;
/// Warn this long before the bank's history window passes the last download:
/// a year, since the books can easily go a year without being looked at.
const HISTORY_MARGIN_DAYS: i64 = 365;

pub struct Build {
    pub as_of: Date,
    pub loaded: Loaded,
    pub rules: Rules,
    pub generated: Generated,
    /// The text of ledger/generated.journal.
    pub journal: String,
    /// The text of ledger/forecast.journal.
    pub forecast: String,
    /// The whole books (generated and hand-written, without the forecast) in
    /// journal order, every amount filled in: what generated.journal holds.
    pub books: Vec<LedgerTransaction>,
    pub subscriptions: Vec<crate::subscriptions::State>,
    pub findings: Findings,
}

/// Builds the books as of `as_of`. `manual`: the accounts and transactions
/// written by hand in config/; `findings` already holds what loading the inputs
/// found.
pub fn assemble(
    config: &Config,
    rules: Rules,
    loaded: Loaded,
    manual: &Manual,
    as_of: Date,
    mut findings: Findings,
) -> Build {
    for (name, data) in &loaded.sources {
        let source = &config.sources[name];
        if let Some(last) = data.last()
            && last > as_of
        {
            findings.warn("as-of", format!("{name} has data after --as-of {as_of} (until {last})"));
        }
        let Some(covered) = data.covered_until() else { continue };
        let age = crate::transfers::days_between(as_of, covered);
        let fetched = source.formats.contains(&crate::config::Format::EnableBanking)
            && config.banks.get(&source.bank).is_some_and(|b| b.enable_banking.is_some());
        let how = if fetched {
            "run `konter fetch`".to_string()
        } else {
            format!("export {name} again and `konter add-export` it")
        };
        let history = config.banks.get(&source.bank).and_then(|b| b.history_days);
        if let Some(days) = history
            && age + HISTORY_MARGIN_DAYS > days
        {
            let lost = covered.saturating_add(jiff::Span::new().days(days));
            findings.warn(
                "download soon",
                format!("{name}: data covers until {covered}; {how} before {lost}, when the bank's {days}-day history passes it"),
            );
        } else if covered < as_of && age > if fetched { STALE_FETCH_DAYS } else { STALE_EXPORT_DAYS } {
            findings.warn("stale data", format!("{name}: data covers until {covered} ({age} days ago); {how}"));
        }
    }

    // Every account the rules use must be declared by hand.
    let mut declared = manual.accounts.clone();
    declared.extend(rules.subscriptions.iter().filter_map(|s| s.balance_account()));
    let mut used = rules.accounts();
    for (name, source) in &config.sources {
        if let Some(account) = &source.before_start {
            used.entry(account.clone()).or_insert_with(|| format!("before_start of {name}"));
        }
    }
    let undeclared: Vec<String> = used
        .into_iter()
        .filter(|(a, _)| !declared.contains(a))
        // The fallback accounts are declared by konter itself.
        .filter(|(a, _)| !config.sources.keys().any(|n| a.ends_with(&format!(":unmatched:{n}"))))
        .map(|(a, label)| format!("{a}  (used by {label})"))
        .collect();
    if !undeclared.is_empty() {
        let d = findings.error("accounts", "accounts used by rules aren't declared in config/accounts.toml");
        for u in undeclared {
            d.detail(u);
        }
    }

    let mut generated = engine::generate(config, &rules, &loaded, &mut findings);
    let subscriptions =
        crate::subscriptions::generate(config, &rules.subscriptions, as_of, &mut generated, &mut findings);
    let accounts = manual.accounts.union(&generated.tool_accounts).cloned().collect();
    let currency = &config.books.currency;
    let books = crate::books::assemble(&manual.txs, &generated.txs, &accounts, currency, &mut findings);
    let journal = journal::render(as_of, currency, &accounts, &books);
    let forecast = crate::subscriptions::forecast(config, &rules.subscriptions, &subscriptions, as_of);
    Build { as_of, loaded, rules, generated, journal, forecast, books, subscriptions, findings }
}
