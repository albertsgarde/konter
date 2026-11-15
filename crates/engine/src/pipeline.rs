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
use crate::sources::{Loaded, SourceData};

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
    for (name, source) in &config.sources {
        let data = loaded.sources.get(name);
        if let Some(last) = data.and_then(|d| d.last())
            && last > as_of
        {
            findings.warn("as-of", format!("{name} has data after --as-of {as_of} (until {last})"));
        }
        let history = config.banks.get(&source.bank).and_then(|b| b.history_days);
        let fetched = source.formats.contains(&crate::config::Format::EnableBanking)
            && config.banks.get(&source.bank).is_some_and(|b| b.enable_banking.is_some());
        if let Some(covered) = data.and_then(|d| d.covered_until()) {
            let age = crate::transfers::days_between(as_of, covered);
            let how = if fetched {
                "run `konter fetch`".to_string()
            } else {
                format!("export {name} again and `konter add-export` it")
            };
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
        if let Some(days) = history {
            history_before_the_data(config, name, data, fetched, days, as_of, &mut findings);
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

/// Warns, a year ahead like the warning about where the data ends, before the
/// bank's history window passes history that was never downloaded: a source's
/// history from its start up to where its data begins, or all of it when there's
/// no data yet (not imported, or nothing added). A beginning that's already
/// gone is left to the opening balance remark, which says to set `start`.
fn history_before_the_data(
    config: &Config,
    name: &str,
    data: Option<&SourceData>,
    fetched: bool,
    days: i64,
    as_of: Date,
    findings: &mut Findings,
) {
    let source = &config.sources[name];
    let start = config.source_start(source);
    let span = |n: i64| jiff::Span::new().days(n);
    // The bank still has everything from this day on.
    let kept_from = as_of.saturating_sub(span(days));
    // History from `start` starts to go on this day.
    let deadline = start.saturating_add(span(days));
    if as_of.saturating_add(span(HISTORY_MARGIN_DAYS)) <= deadline {
        return;
    }
    let from = start.max(kept_from);
    let download = if fetched {
        format!("run `konter fetch --source {name} --from {from}`")
    } else {
        format!("export {name} from {from} and `konter add-export` it")
    };
    let (missing, how, otherwise) = match data.and_then(|d| d.first()) {
        Some(first) => {
            // A week's gap is a quiet start, and an opening balance of zero a
            // new account: the same test as the opening balance remark.
            let new_account = data.and_then(|d| d.opening).is_some_and(|o| o.is_zero());
            if first <= start || crate::transfers::days_between(first, start) <= 7 || new_account || first <= kept_from
            {
                return;
            }
            (
                format!("{name}: nothing before {first} is downloaded, though its start is {start}"),
                download,
                format!("set `start` to {first} if there's nothing before it"),
            )
        }
        None if source.imported() => {
            (format!("{name}: nothing is downloaded yet"), download, "set a later `start`".to_string())
        }
        None => (
            format!("{name} isn't imported yet"),
            "import it (give it `formats` in config/sources.toml)".to_string(),
            "set a later `start` if its history isn't wanted".to_string(),
        ),
    };
    let message = if deadline > as_of {
        format!("{missing}; {how} before {deadline}, when the bank's {days}-day history passes {start}, or {otherwise}")
    } else {
        format!(
            "{missing}; the bank's {days}-day history passed {start} on {deadline} and loses a day more every day: \
             {how} now, or {otherwise}"
        )
    };
    findings.warn("download soon", message);
}
