//! Applying the rules to bank transactions (docs/spec.md §5–§6).

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use rust_decimal::Decimal;

use crate::config::{Config, Policy};
use crate::findings::{Findings, Severity};
use crate::model::{BankTransaction, LedgerTransaction, Posting, Provenance, name_instead, quoted_description};
use crate::money::fmt_num;
use crate::rules::{Action, AmountSpec, BANK, Output, Rules};
use crate::sources::Loaded;
use crate::transfers::{self, Leg};

pub const TRANSFERS: &str = "assets:transfers";
pub const OPENING: &str = "equity:opening-balances";
pub const UNKNOWN_OUT: &str = "expenses:unknown";
pub const UNKNOWN_IN: &str = "income:unknown";

/// What happened to one bank transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Rule(String),
    Pin,
    Unmatched,
    /// Unmatched, but marked to be mapped later (`[later."<id>"]`), with the
    /// mark's note.
    Later(Option<String>),
    Ambiguous(Vec<String>),
    /// A rule matched but its output was invalid for this transaction.
    Invalid {
        rule: String,
        error: String,
    },
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub status: Status,
    /// Every pattern rule whose `when` matched, with its priority.
    pub candidates: Vec<(String, i64)>,
    /// Indices of the ledger transactions it produced.
    pub ledger: Vec<usize>,
    /// Set for `unknown`.
    pub unknown: Option<String>,
    /// Set for `ignore`.
    pub ignored: Option<String>,
}

#[derive(Default)]
pub struct Generated {
    pub txs: Vec<LedgerTransaction>,
    /// Bank transaction ID → outcome.
    pub outcomes: BTreeMap<String, Outcome>,
    /// Accounts the tool invents; declared in the generated journal.
    pub tool_accounts: BTreeSet<String>,
}

pub fn fallback_account(source: &str, amount: Decimal) -> String {
    let kind = if amount > Decimal::ZERO { "income" } else { "expenses" };
    format!("{kind}:unmatched:{source}")
}

fn bank_posting(config: &Config, t: &BankTransaction) -> Posting {
    let source = &config.sources[&t.source];
    let mut p = Posting::new(&source.ledger, t.amount, &t.currency);
    p.assert_balance = t.balance_after;
    p
}

fn header(t: &BankTransaction, rule: &str, seq: usize, output: Option<&Output>) -> LedgerTransaction {
    let mut tags = vec![("bank-id".to_string(), t.id.clone()), ("rule".to_string(), rule.to_string())];
    if let Some(d) = t.transaction_date.filter(|d| *d != t.booked) {
        tags.push(("purchased".into(), d.to_string()));
    }
    // Foreign-currency purchases stay in the account's currency; the original
    // amount is kept as a tag so reports don't mix currencies.
    if let (Some(a), Some(c)) = (t.original_amount, &t.original_currency) {
        tags.push(("original".into(), format!("{} {c}", fmt_num(a))));
    }
    let mut comments = Vec::new();
    // Without a payee from the rule, a description that's only a reference
    // number gives way to the counterparty's name.
    let payee = output.and_then(|o| o.payee.clone()).or_else(|| name_instead(t).map(str::to_string));
    if payee.as_ref().is_some_and(|p| *p != t.description) {
        comments.push(format!("bank: {:?}", t.description));
    }
    if let Some(o) = output {
        tags.extend(o.tags.iter().cloned());
        comments.extend(o.note.iter().cloned());
    }
    LedgerTransaction {
        date: t.booked,
        description: payee.unwrap_or_else(|| t.description.clone()),
        tags,
        comments,
        postings: Vec::new(),
        provenance: Provenance::Bank { source: t.source.clone(), seq },
    }
}

/// Result of applying one output to one bank transaction.
struct Applied {
    txs: Vec<LedgerTransaction>,
    transfer_to: Option<String>,
    unknown: Option<String>,
    ignored: Option<String>,
}

fn apply(
    config: &Config,
    rules: &Rules,
    t: &BankTransaction,
    seq: usize,
    rule: &str,
    output: &Output,
) -> Result<Applied> {
    let mut applied = Applied { txs: Vec::new(), transfer_to: None, unknown: None, ignored: None };
    let base = || header(t, rule, seq, Some(output));
    let counter = |account: &str| Posting::new(account, -t.amount, &t.currency);
    match &output.action {
        Action::Account(account) => {
            let mut tx = base();
            tx.postings = vec![bank_posting(config, t), counter(account)];
            applied.txs.push(tx);
        }
        Action::Split(parts) => {
            let too_large = || anyhow::anyhow!("split amounts too large to add up");
            let fixed = parts
                .iter()
                .filter_map(|p| p.amount)
                .try_fold(Decimal::ZERO, |sum, a| sum.checked_add(a))
                .ok_or_else(too_large)?;
            let rest = (-t.amount).checked_sub(fixed).ok_or_else(too_large)?;
            if parts.iter().all(|p| p.amount.is_some()) && rest != Decimal::ZERO {
                bail!(
                    "split amounts add up to {}, but must add up to {} (or add rest = true)",
                    fmt_num(fixed),
                    fmt_num(-t.amount)
                );
            }
            let mut tx = base();
            tx.postings.push(bank_posting(config, t));
            for p in parts {
                tx.postings.push(Posting::new(&p.account, p.amount.unwrap_or(rest), &t.currency));
            }
            applied.txs.push(tx);
        }
        Action::Transfer(target) => {
            let target_source = &config.sources[target];
            if *target == t.source {
                bail!("transfer to its own source");
            }
            let mut tx = base();
            let target_start = config.source_start(target_source);
            // An account that isn't imported only has a start that matters
            // when it says where transfers before then go.
            if t.booked < target_start && (target_source.imported() || target_source.before_start.is_some()) {
                // The other account wasn't part of the books yet.
                let Some(account) = &target_source.before_start else {
                    bail!(
                        "transfer to {target} on {}, before its start {target_start}; set `before_start` for \
                         {target} in config/sources.toml to say where such transfers go",
                        t.booked
                    );
                };
                tx.postings = vec![bank_posting(config, t), counter(account)];
                tx.comments.push(format!("{target} before its start ({target_start})"));
            } else if target_source.imported() {
                tx.postings = vec![bank_posting(config, t), counter(TRANSFERS)];
                tx.tags.push(("transfer".into(), target.clone()));
                applied.transfer_to = Some(target.clone());
            } else {
                tx.postings = vec![bank_posting(config, t), counter(&target_source.ledger)];
            }
            applied.txs.push(tx);
        }
        Action::Subscription(name) => {
            let sub = rules.subscription(name).expect("validated when loading");
            let mut tx = base();
            let balance_account = sub.balance_account().expect("validated when loading: not a transfer");
            tx.postings = vec![bank_posting(config, t), counter(&balance_account)];
            tx.tags.push(("subscription".into(), name.clone()));
            applied.txs.push(tx);
        }
        Action::Unknown(reason) => {
            let mut tx = base();
            let account = if t.amount > Decimal::ZERO { UNKNOWN_IN } else { UNKNOWN_OUT };
            tx.postings = vec![bank_posting(config, t), counter(account)];
            tx.comments.push(format!("won't map: {reason}"));
            applied.txs.push(tx);
            applied.unknown = Some(reason.clone());
        }
        Action::Ignore(reason) => applied.ignored = Some(reason.clone()),
        Action::Transactions(specs) => {
            for spec in specs {
                let mut tx = base();
                if let Some(d) = spec.date {
                    tx.date = d;
                }
                if let Some(p) = &spec.payee {
                    tx.description = p.clone();
                }
                tx.comments.extend(spec.note.iter().cloned());
                let mut rest_at = None;
                for p in &spec.postings {
                    let posting = match (p.account.as_str(), p.amount) {
                        (BANK, _) => bank_posting(config, t),
                        (_, AmountSpec::Rest) => {
                            rest_at = Some(tx.postings.len());
                            Posting::new(&p.account, Decimal::ZERO, &t.currency)
                        }
                        (_, AmountSpec::Fixed(a)) => Posting::new(&p.account, a, &t.currency),
                        (_, AmountSpec::Bank) => Posting::new(&p.account, t.amount, &t.currency),
                        (_, AmountSpec::NegBank) => Posting::new(&p.account, -t.amount, &t.currency),
                    };
                    tx.postings.push(posting);
                }
                let sum: Decimal = tx.postings.iter().map(|p| p.amount).sum();
                match rest_at {
                    Some(i) => tx.postings[i].amount = -sum,
                    None if sum != Decimal::ZERO => {
                        bail!("transaction {:?} doesn't balance (off by {})", tx.description, fmt_num(sum))
                    }
                    None => {}
                }
                applied.txs.push(tx);
            }
        }
    }
    Ok(applied)
}

fn fallback(config: &Config, t: &BankTransaction, seq: usize, label: &str, comment: &str) -> LedgerTransaction {
    let mut tx = header(t, label, seq, None);
    tx.postings =
        vec![bank_posting(config, t), Posting::new(fallback_account(&t.source, t.amount), -t.amount, &t.currency)];
    tx.comments.push(comment.to_string());
    tx
}

fn line(t: &BankTransaction) -> String {
    format!("{}  {}  {:>11}  {}", t.id, t.booked, fmt_num(t.amount), quoted_description(t))
}

pub fn generate(config: &Config, rules: &Rules, loaded: &Loaded, findings: &mut Findings) -> Generated {
    let mut g = Generated::default();
    for s in config.sources.values() {
        g.tool_accounts.insert(s.ledger.clone());
    }
    opening_balances(config, loaded, &mut g, findings);
    let mapped = apply_rules(config, rules, loaded, &mut g, findings);
    balance_snapshots(config, loaded, &mut g, findings);
    pair_transfers(config, loaded, &mapped.legs, &mut g, findings);
    report_outcomes(config, loaded, &g, mapped.ambiguous, mapped.invalid, findings);
    report_dead_rules(config, rules, &g, &mapped.matched_rules, findings);
    report_stale_later(rules, &g, findings);
    g
}

/// Books each source's opening balance at its start.
fn opening_balances(config: &Config, loaded: &Loaded, g: &mut Generated, findings: &mut Findings) {
    for (name, data) in &loaded.sources {
        let source = &config.sources[name];
        let (Some(opening), Some(first)) = (data.opening, data.first()) else { continue };
        if opening.is_zero() {
            continue;
        }
        let start = config.source_start(source);
        if transfers::days_between(first, start) > 7 {
            findings.warn(
                "opening balance",
                format!(
                    "{name}: data starts {first}, but the opening balance ({}) is booked at {start}; \
                     set `start` for this source if the account didn't exist yet",
                    fmt_num(opening)
                ),
            );
        }
        let mut bank = Posting::new(&source.ledger, opening, config.source_currency(source));
        bank.assert_balance = Some(opening);
        g.txs.push(LedgerTransaction {
            date: start,
            description: "Opening balance".into(),
            tags: vec![("source".into(), name.clone())],
            comments: Vec::new(),
            postings: vec![bank, Posting::new(OPENING, -opening, config.source_currency(source))],
            provenance: Provenance::Opening,
        });
        g.tool_accounts.insert(OPENING.into());
    }
}

/// What applying the rules leaves for the steps after it.
#[derive(Default)]
struct Mapped<'a> {
    /// The sides of transfers between imported sources, still to be paired.
    legs: Vec<Leg>,
    /// The pattern rules that matched a transaction, by name.
    matched_rules: BTreeSet<&'a str>,
    /// A line for each transaction whose rule didn't fit it.
    invalid: Vec<String>,
    /// A line for each transaction that several rules of equal priority match.
    ambiguous: Vec<String>,
}

/// Books every bank transaction, by its rule or pin or to the fallback
/// account, and records what became of it.
fn apply_rules<'a>(
    config: &Config,
    rules: &'a Rules,
    loaded: &Loaded,
    g: &mut Generated,
    findings: &mut Findings,
) -> Mapped<'a> {
    let mut mapped = Mapped::default();
    for (name, data) in &loaded.sources {
        let ledger = &config.sources[name].ledger;
        let mut books_balance = data.opening;
        for (seq, t) in data.txs.iter().enumerate() {
            let (mut outcome, produced) = map(config, rules, t, seq, g.txs.len(), &mut mapped);
            // The books' bank balance must follow the bank's running balance
            // wherever a bank posting (with its balance assertion) is written.
            let bank_postings: Vec<Decimal> = produced
                .iter()
                .flat_map(|tx| &tx.postings)
                .filter(|p| p.account == *ledger)
                .map(|p| p.amount)
                .collect();
            books_balance = books_balance.map(|b| b + bank_postings.iter().sum::<Decimal>());
            if (!bank_postings.is_empty() || seq + 1 == data.txs.len())
                && let (Some(books), Some(bank)) = (books_balance, t.balance_after)
                && books != bank
            {
                findings.error(
                    "ignore",
                    format!(
                        "{name}: at {} the books say {} but the bank says {}; ignored transactions must cancel out",
                        t.id,
                        fmt_num(books),
                        fmt_num(bank)
                    ),
                );
                books_balance = Some(bank);
            }
            for tx in produced {
                for p in &tx.postings {
                    if p.account == TRANSFERS
                        || p.account == UNKNOWN_IN
                        || p.account == UNKNOWN_OUT
                        || p.account.contains(":unmatched:")
                    {
                        g.tool_accounts.insert(p.account.clone());
                    }
                }
                outcome.ledger.push(g.txs.len());
                g.txs.push(tx);
            }
            g.outcomes.insert(t.id.clone(), outcome);
        }
    }
    mapped
}

/// Maps one bank transaction: finds its pin or rule and applies it, or books
/// it to the fallback account. Returns its outcome and the ledger transactions
/// it makes, which the caller adds to the books and to the outcome; `next` is
/// the index the first of them gets.
fn map<'a>(
    config: &Config,
    rules: &'a Rules,
    t: &BankTransaction,
    seq: usize,
    next: usize,
    mapped: &mut Mapped<'a>,
) -> (Outcome, Vec<LedgerTransaction>) {
    let mut candidates: Vec<(&str, i64, &Output)> =
        rules.patterns.iter().filter(|r| r.when.matches(t)).map(|r| (r.name.as_str(), r.priority, &r.output)).collect();
    candidates.sort_by_key(|c| std::cmp::Reverse(c.1));
    mapped.matched_rules.extend(candidates.iter().map(|c| c.0));
    let chosen: Result<(String, &Output), Status> = if let Some(pin) = rules.pins.get(&t.id) {
        Ok(("pin".into(), &pin.output))
    } else if candidates.is_empty() {
        Err(Status::Unmatched)
    } else if candidates.len() > 1 && candidates[0].1 == candidates[1].1 {
        let top = candidates[0].1;
        Err(Status::Ambiguous(candidates.iter().filter(|c| c.1 == top).map(|c| c.0.to_string()).collect()))
    } else {
        Ok((candidates[0].0.to_string(), candidates[0].2))
    };
    let mut outcome = Outcome {
        status: Status::Unmatched,
        candidates: candidates.iter().map(|c| (c.0.to_string(), c.1)).collect(),
        ledger: Vec::new(),
        unknown: None,
        ignored: None,
    };
    let produced = match chosen {
        Ok((label, output)) => match apply(config, rules, t, seq, &label, output) {
            Ok(applied) => {
                outcome.status = if label == "pin" { Status::Pin } else { Status::Rule(label.clone()) };
                if let Some(to) = applied.transfer_to {
                    mapped.legs.push(Leg {
                        ledger_idx: next,
                        from: t.source.clone(),
                        to,
                        date: t.booked,
                        amount: t.amount,
                        id: t.id.clone(),
                    });
                }
                outcome.unknown = applied.unknown;
                outcome.ignored = applied.ignored;
                applied.txs
            }
            Err(e) => {
                mapped.invalid.push(format!("{}: {label}: {e}", line(t)));
                outcome.status = Status::Invalid { rule: label.clone(), error: e.to_string() };
                vec![fallback(config, t, seq, "invalid", &format!("rule {label} failed: {e}"))]
            }
        },
        Err(status) => {
            if let Status::Ambiguous(names) = &status {
                mapped.ambiguous.push(format!("{}: {}", line(t), names.join(", ")));
            }
            // Only a plain unmatched transaction can wait: an
            // ambiguous one is an error in the rules either way.
            let later = rules.later.get(&t.id).filter(|_| status == Status::Unmatched);
            let comment = match later {
                None => "not mapped yet".to_string(),
                Some(l) => match &l.note {
                    None => "not mapped yet, marked later".to_string(),
                    Some(note) => format!("not mapped yet, marked later: {note}"),
                },
            };
            outcome.status = match later {
                Some(l) => Status::Later(l.note.clone()),
                None => status,
            };
            vec![fallback(config, t, seq, "unmatched", &comment)]
        }
    };
    (outcome, produced)
}

/// Balance snapshots stand in for running balances where there are none:
/// checks each against the books and writes it as a balance assertion.
fn balance_snapshots(config: &Config, loaded: &Loaded, g: &mut Generated, findings: &mut Findings) {
    for (name, data) in &loaded.sources {
        if data.has_running_balance {
            continue;
        }
        let source = &config.sources[name];
        for snap in &data.snapshots {
            let posted: Decimal = g
                .txs
                .iter()
                .filter(|tx| matches!(&tx.provenance, Provenance::Bank { source: s, .. } if s == name))
                .filter(|tx| tx.date <= snap.date)
                .flat_map(|tx| &tx.postings)
                .filter(|p| p.account == source.ledger)
                .map(|p| p.amount)
                .sum();
            let books = data.opening.unwrap_or_default() + posted;
            if books != snap.balance {
                findings.error(
                    "balance check",
                    format!(
                        "{name}: on {} the bank reported {} ({}), but the books say {}",
                        snap.date,
                        fmt_num(snap.balance),
                        snap.file,
                        fmt_num(books)
                    ),
                );
            }
            let currency = config.source_currency(source);
            let mut check = Posting::new(&source.ledger, Decimal::ZERO, currency);
            check.assert_balance = Some(snap.balance);
            g.txs.push(LedgerTransaction {
                date: snap.date,
                description: "Balance reported by the bank".into(),
                tags: vec![("source".into(), name.clone())],
                comments: vec![format!("from {}", snap.file)],
                postings: vec![check],
                provenance: Provenance::Assertion,
            });
        }
    }
}

/// Pairs the two sides of each transfer, tags them with each other, and
/// reports the sides left without one.
fn pair_transfers(config: &Config, loaded: &Loaded, legs: &[Leg], g: &mut Generated, findings: &mut Findings) {
    let window = config.books.transfer_window_days;
    let (pairs, unpaired) = transfers::pair(legs, window);
    for (i, j) in pairs {
        g.txs[legs[i].ledger_idx].tags.push(("transfer-pair".into(), legs[j].id.clone()));
        g.txs[legs[j].ledger_idx].tags.push(("transfer-pair".into(), legs[i].id.clone()));
    }
    let (mut inside, mut edge) = (Vec::new(), Vec::new());
    for i in unpaired {
        let l = &legs[i];
        let msg = format!("{}  {}  {} → {}  {:>11}", l.id, l.date, l.from, l.to, fmt_num(l.amount));
        let covered = loaded.sources.get(&l.to).is_some_and(|d| {
            d.covers(l.date.saturating_sub(jiff::Span::new().days(window)))
                && d.covers(l.date.saturating_add(jiff::Span::new().days(window)))
        });
        if covered { inside.push(msg) } else { edge.push(msg) }
    }
    if !inside.is_empty() {
        findings
            .error("unpaired transfer", format!("{} transfers have no matching other side", inside.len()))
            .details(inside);
    }
    if !edge.is_empty() {
        findings
            .warn(
                "unpaired transfer",
                format!("{} transfers have no other side yet (outside the other account's data)", edge.len()),
            )
            .details(edge);
    }
}

/// Reports what became of the transactions: the ambiguous and invalid ones
/// (a line each, from applying the rules), how much of each source is mapped,
/// and the ones that are ignored or won't be mapped.
fn report_outcomes(
    config: &Config,
    loaded: &Loaded,
    g: &Generated,
    ambiguous: Vec<String>,
    invalid: Vec<String>,
    findings: &mut Findings,
) {
    if !ambiguous.is_empty() {
        findings
            .error("ambiguous", format!("{} transactions match several rules of equal priority", ambiguous.len()))
            .details(ambiguous);
    }
    if !invalid.is_empty() {
        findings
            .error("invalid rule", format!("{} transactions matched a rule that doesn't fit them", invalid.len()))
            .details(invalid);
    }
    for (name, data) in &loaded.sources {
        let source = &config.sources[name];
        // Ambiguous and invalid ones are booked to the fallback account too.
        // Ones marked later are as well, but they're acknowledged, so the
        // source's policy is only about the rest.
        let status = |t: &BankTransaction| &g.outcomes[&t.id].status;
        let open: Vec<&BankTransaction> =
            data.txs.iter().filter(|t| !matches!(status(t), Status::Rule(_) | Status::Pin)).collect();
        let total: Decimal = data.txs.iter().map(|t| t.amount.abs()).sum();
        let open_amount: Decimal = open.iter().map(|t| t.amount.abs()).sum();
        let pct = if total.is_zero() {
            Decimal::ONE_HUNDRED
        } else {
            ((total - open_amount) / total * Decimal::ONE_HUNDRED).round()
        };
        let (later, unmatched): (Vec<&BankTransaction>, Vec<&BankTransaction>) =
            open.into_iter().partition(|t| matches!(status(t), Status::Later(_)));
        let later = later.len();
        let later_note = match later {
            0 => String::new(),
            1 => "; 1 more marked later".to_string(),
            n => format!("; {n} more marked later"),
        };
        if unmatched.is_empty() {
            if later > 0 {
                findings.ok(
                    "mapping",
                    format!("{name}: all {} transactions mapped or marked later ({later} later)", data.txs.len()),
                );
            } else if !data.txs.is_empty() {
                findings.ok("mapping", format!("{name}: all {} transactions mapped", data.txs.len()));
            }
            continue;
        }
        let summary = format!(
            "{name}: {} of {} not mapped ({}% of amount mapped{later_note})",
            unmatched.len(),
            data.txs.len(),
            pct
        );
        let severity = match source.unmatched {
            Policy::Error => Severity::Error,
            Policy::Warn => Severity::Warning,
            Policy::Allow => Severity::Info,
        };
        let d = findings.push(severity, "not mapped", summary);
        if source.unmatched != Policy::Allow {
            d.details(unmatched.iter().map(|t| line(t)));
        }
        d.detail(format!("→ `konter unmatched {name}` prints a rule stub for each"));
    }
    let unknown: Vec<String> = loaded
        .transactions()
        .filter_map(|t| g.outcomes[&t.id].unknown.as_ref().map(|r| format!("{}: {r}", line(t))))
        .collect();
    let ignored: Vec<String> = loaded
        .transactions()
        .filter_map(|t| g.outcomes[&t.id].ignored.as_ref().map(|r| format!("{}: {r}", line(t))))
        .collect();
    for (category, list) in [("won't map", unknown), ("ignored", ignored)] {
        if !list.is_empty() {
            findings.info(category, format!("{} transactions", list.len())).details(list);
        }
    }
}

/// Reports the rules that match nothing, and the pins and later marks for
/// transactions that don't exist. `matched_rules`: the names of the pattern
/// rules that matched something.
fn report_dead_rules(
    config: &Config,
    rules: &Rules,
    g: &Generated,
    matched_rules: &BTreeSet<&str>,
    findings: &mut Findings,
) {
    let mut dead: Vec<String> = rules
        .patterns
        .iter()
        .filter(|r| !matched_rules.contains(r.name.as_str()))
        .map(|r| format!("rule {:?} ({})", r.name, r.file))
        .collect();
    // An ID for no transaction, except one before its source's start.
    let missing = |id: &str| {
        if g.outcomes.contains_key(id) {
            return false;
        }
        let prefix = id.split('-').next().unwrap_or("");
        let date: Option<jiff::civil::Date> = id.get(prefix.len() + 1..prefix.len() + 11).and_then(|d| d.parse().ok());
        let source = config.sources.values().find(|s| s.id_prefix.as_deref() == Some(prefix));
        !matches!((source, date), (Some(s), Some(d)) if d < config.source_start(s))
    };
    for pin in rules.pins.values().filter(|p| missing(&p.id)) {
        dead.push(format!("pin {:?} ({}): no such transaction", pin.id, pin.file));
    }
    for later in rules.later.values().filter(|l| missing(&l.id)) {
        dead.push(format!("later {:?} ({}): no such transaction", later.id, later.file));
    }
    if !dead.is_empty() {
        findings.warn("dead rule", format!("{} rules match nothing", dead.len())).details(dead);
    }
}

/// Reports the later marks on transactions that are mapped after all.
fn report_stale_later(rules: &Rules, g: &Generated, findings: &mut Findings) {
    let stale: Vec<String> = rules
        .later
        .values()
        .filter_map(|l| {
            let by = match &g.outcomes.get(&l.id)?.status {
                Status::Pin => "a pin".to_string(),
                Status::Rule(name) => format!("rule {name:?}"),
                _ => return None,
            };
            Some(format!("later {:?} ({}): mapped by {by}; remove the mark", l.id, l.file))
        })
        .collect();
    if !stale.is_empty() {
        findings.warn("stale later", format!("{} transactions marked later are mapped", stale.len())).details(stale);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card_payment(description: &str, name: Option<&str>) -> BankTransaction {
        BankTransaction {
            id: "nb-2026-01-01-000000".into(),
            source: "test:a".into(),
            format: crate::config::Format::EnableBanking,
            booked: jiff::civil::date(2026, 1, 1),
            time: None,
            value_date: None,
            transaction_date: None,
            amount: Decimal::from(-50),
            currency: "DKK".into(),
            original_amount: None,
            original_currency: None,
            description: description.into(),
            counterparty: None,
            counterparty_name: name.map(str::to_string),
            balance_after: None,
            bank_ref: None,
            raw: BTreeMap::new(),
            origins: Vec::new(),
            order_hint: 0,
        }
    }

    #[test]
    fn a_reference_number_gives_way_to_the_counterparty_name() {
        let t = card_payment("74300000000000000000017", Some("NETTO SCANNGO"));
        let tx = header(&t, "unmatched", 0, None);
        assert_eq!(tx.description, "NETTO SCANNGO");
        assert_eq!(tx.comments, [r#"bank: "74300000000000000000017""#]);
        // A real description stays, with no comment.
        let tx = header(&card_payment("COOP APP", Some("COOP")), "unmatched", 0, None);
        assert_eq!(tx.description, "COOP APP");
        assert!(tx.comments.is_empty());
    }
}
