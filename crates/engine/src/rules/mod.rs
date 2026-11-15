//! The rules: `config/rules/**/*.toml` and/or `config/rules.toml` (docs/spec.md §5).
//!
//! All amounts in rules are plain hledger posting amounts: positive debits the
//! account. For an outgoing payment of -1200 DKK, an expense posting is +1200.

pub mod condition;

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use rust_decimal::Decimal;
use serde::Deserialize;

pub use condition::Condition;
use condition::RawCondition;

use crate::config::Config;
use crate::dates::ConfigDate;
use crate::money::{Amount, parse_plain};
use crate::subscriptions::{Flow, RawSubscription, Subscription};

/// The special account name for the bank posting in the general form.
pub const BANK: &str = "@bank";

#[derive(Debug)]
pub struct Rules {
    pub patterns: Vec<PatternRule>,
    pub pins: BTreeMap<String, Pin>,
    /// Transactions left unmapped for now (`[later."<id>"]`).
    pub later: BTreeMap<String, Later>,
    /// Trips, by name (`[trip.<name>]`).
    pub trips: BTreeMap<String, Trip>,
    pub subscriptions: Vec<Subscription>,
}

/// A trip, declared with its dates so that purchases during it can be
/// recognised as it happens, before any of them are mapped. Not an output:
/// mapping the purchases to its account is up to rules and pins.
#[derive(Debug)]
pub struct Trip {
    pub name: String,
    pub account: String,
    /// The first and last day away, both included.
    pub from: jiff::civil::Date,
    pub to: jiff::civil::Date,
    pub note: Option<String>,
    pub file: String,
}

impl Trip {
    pub fn contains(&self, day: jiff::civil::Date) -> bool {
        self.from <= day && day <= self.to
    }
}

#[derive(Debug)]
pub struct PatternRule {
    pub name: String,
    pub when: Condition,
    pub priority: i64,
    pub output: Output,
    pub file: String,
}

#[derive(Debug)]
pub struct Pin {
    pub id: String,
    pub output: Output,
    pub file: String,
}

/// A transaction left unmapped for now: still booked to the fallback account,
/// but acknowledged, so it doesn't count against the source's `unmatched`
/// policy and isn't brought up as new. Not an output, so a pin or rule that
/// maps the transaction makes the mark stale.
#[derive(Debug)]
pub struct Later {
    pub id: String,
    pub note: Option<String>,
    pub file: String,
}

#[derive(Debug)]
pub struct Output {
    pub payee: Option<String>,
    pub note: Option<String>,
    pub tags: Vec<(String, String)>,
    pub action: Action,
}

#[derive(Debug)]
pub enum Action {
    Account(String),
    Split(Vec<SplitPart>),
    Transfer(String),
    Subscription(String),
    Unknown(String),
    Ignore(String),
    Transactions(Vec<TxSpec>),
}

#[derive(Debug)]
pub struct SplitPart {
    pub account: String,
    /// `None` means "the rest".
    pub amount: Option<Decimal>,
}

#[derive(Debug)]
pub struct TxSpec {
    /// `None` means the booking date.
    pub date: Option<jiff::civil::Date>,
    pub payee: Option<String>,
    pub note: Option<String>,
    pub postings: Vec<PostingSpec>,
}

#[derive(Debug)]
pub struct PostingSpec {
    pub account: String,
    pub amount: AmountSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmountSpec {
    Fixed(Decimal),
    /// `"@amount"`: the bank amount.
    Bank,
    /// `"-@amount"`: the bank amount negated.
    NegBank,
    /// Whatever balances the transaction.
    Rest,
}

// ── Raw TOML schema ────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    rule: Vec<toml::Table>,
    #[serde(default)]
    pin: BTreeMap<String, RawOutput>,
    #[serde(default)]
    later: BTreeMap<String, RawLater>,
    #[serde(default)]
    trip: BTreeMap<String, RawTrip>,
    #[serde(default)]
    subscription: Vec<RawSubscription>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLater {
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTrip {
    account: String,
    from: ConfigDate,
    to: ConfigDate,
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOutput {
    payee: Option<String>,
    note: Option<String>,
    #[serde(default)]
    tags: BTreeMap<String, String>,
    account: Option<String>,
    split: Option<Vec<RawSplit>>,
    transfer: Option<String>,
    subscription: Option<String>,
    unknown: Option<String>,
    ignore: Option<String>,
    transactions: Option<Vec<RawTx>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSplit {
    account: String,
    amount: Option<Amount>,
    #[serde(default)]
    rest: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTx {
    date: Option<ConfigDate>,
    payee: Option<String>,
    note: Option<String>,
    postings: Vec<RawPosting>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPosting {
    account: String,
    amount: Option<toml::Value>,
    #[serde(default)]
    rest: bool,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|c| c.is_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
}

pub(crate) fn valid_tag(s: &str) -> bool {
    !s.is_empty() && !s.contains([',', ';', '\n', ' ', ':'])
}

fn convert_output(raw: RawOutput) -> Result<Output> {
    let mut actions = Vec::new();
    if let Some(a) = raw.account {
        actions.push(Action::Account(a));
    }
    if let Some(parts) = raw.split {
        actions.push(Action::Split(convert_split(parts)?));
    }
    if let Some(t) = raw.transfer {
        actions.push(Action::Transfer(t));
    }
    if let Some(s) = raw.subscription {
        actions.push(Action::Subscription(s));
    }
    if let Some(r) = raw.unknown {
        actions.push(Action::Unknown(r));
    }
    if let Some(r) = raw.ignore {
        actions.push(Action::Ignore(r));
    }
    if let Some(txs) = raw.transactions {
        actions.push(Action::Transactions(convert_transactions(txs)?));
    }
    if actions.len() != 1 {
        bail!("needs exactly one of: account, split, transfer, subscription, unknown, ignore, transactions");
    }
    for (k, v) in &raw.tags {
        if !valid_tag(k) || v.contains([',', ';', '\n']) {
            bail!("invalid tag {k:?} = {v:?} (no spaces, commas, colons or semicolons in names; no commas in values)");
        }
    }
    Ok(Output {
        payee: raw.payee,
        note: raw.note,
        tags: raw.tags.into_iter().collect(),
        action: actions.pop().unwrap(),
    })
}

fn convert_split(parts: Vec<RawSplit>) -> Result<Vec<SplitPart>> {
    if parts.is_empty() {
        bail!("split is empty");
    }
    let mut out = Vec::new();
    for p in parts {
        let amount = match (p.amount, p.rest) {
            (Some(a), false) => Some(a.0),
            (None, true) => None,
            _ => bail!("split part for {:?} needs either amount or rest = true", p.account),
        };
        out.push(SplitPart { account: p.account, amount });
    }
    if out.iter().filter(|p| p.amount.is_none()).count() > 1 {
        bail!("split has more than one rest = true");
    }
    Ok(out)
}

fn convert_transactions(txs: Vec<RawTx>) -> Result<Vec<TxSpec>> {
    let mut out = Vec::new();
    let mut bank_postings = 0;
    for tx in txs {
        let mut postings = Vec::new();
        for p in tx.postings {
            let amount = match (&p.amount, p.rest) {
                (None, true) => AmountSpec::Rest,
                (None, false) if p.account == BANK => AmountSpec::Bank,
                (None, false) => bail!("posting to {:?} needs an amount or rest = true", p.account),
                (Some(_), true) => bail!("posting to {:?} has both amount and rest", p.account),
                (Some(toml::Value::String(s)), false) if s == "@amount" => AmountSpec::Bank,
                (Some(toml::Value::String(s)), false) if s == "-@amount" => AmountSpec::NegBank,
                (Some(toml::Value::String(s)), false) => AmountSpec::Fixed(parse_plain(s)?),
                (Some(v), false) => AmountSpec::Fixed(Amount::deserialize(v.clone())?.0),
            };
            if p.account == BANK {
                bank_postings += 1;
                if amount != AmountSpec::Bank {
                    bail!("the {BANK} posting always has the bank amount; leave out amount");
                }
                if tx.date.is_some() {
                    bail!("the transaction with the {BANK} posting is on the booking date; leave out date");
                }
            }
            postings.push(PostingSpec { account: p.account, amount });
        }
        if postings.len() < 2 {
            bail!("each transaction needs at least two postings");
        }
        if postings.iter().filter(|p| p.amount == AmountSpec::Rest).count() > 1 {
            bail!("a transaction can have at most one rest = true");
        }
        out.push(TxSpec { date: tx.date.map(|d| d.0), payee: tx.payee, note: tx.note, postings });
    }
    if bank_postings != 1 {
        bail!("transactions must contain exactly one {BANK} posting, found {bank_postings}");
    }
    Ok(out)
}

/// Rules can be spread over many files, so a duplicate names where both are.
fn defined_twice(what: &str, first: &str, second: &str) -> String {
    if first == second {
        format!("{what} is defined twice in {first}")
    } else {
        format!("{what} is defined in both {first} and {second}")
    }
}

fn is_pin_id(id: &str) -> bool {
    // <prefix>-YYYY-MM-DD-xxxxxx
    let parts: Vec<&str> = id.split('-').collect();
    parts.len() == 5
        && crate::config::valid_id_prefix(parts[0])
        && parts[4].len() == 6
        && parts[4].chars().all(|c| c.is_ascii_hexdigit())
        && format!("{}-{}-{}", parts[1], parts[2], parts[3]).parse::<jiff::civil::Date>().is_ok()
}

impl Rules {
    pub fn load_str(text: &str, file: &str, config: &Config) -> Result<Rules> {
        Rules::from_files([(file, text)], config)
    }

    /// Rules from `(file, text)` pairs, checked together as one set.
    pub fn from_files<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>, config: &Config) -> Result<Rules> {
        let mut rules = Rules {
            patterns: Vec::new(),
            pins: BTreeMap::new(),
            later: BTreeMap::new(),
            trips: BTreeMap::new(),
            subscriptions: Vec::new(),
        };
        for (file, text) in files {
            rules.parse(text, file).with_context(|| format!("in {file}"))?;
        }
        rules.validate(config)?;
        Ok(rules)
    }

    fn parse(&mut self, text: &str, file: &str) -> Result<()> {
        let raw: RawFile = toml::from_str(text)?;
        for mut table in raw.rule {
            let name = match table.remove("name") {
                Some(toml::Value::String(s)) => s,
                _ => bail!("every [[rule]] needs a name"),
            };
            (|| -> Result<()> {
                if !valid_name(&name) {
                    bail!("rule names use lowercase letters, digits and dashes");
                }
                let when: RawCondition =
                    table.remove("when").context("missing `when`")?.try_into().context("invalid `when`")?;
                let priority = match table.remove("priority") {
                    None => 0,
                    Some(toml::Value::Integer(p)) => p,
                    Some(_) => bail!("priority must be an integer"),
                };
                let output: RawOutput = toml::Value::Table(table).try_into()?;
                self.patterns.push(PatternRule {
                    name: name.clone(),
                    when: Condition::compile(when)?,
                    priority,
                    output: convert_output(output)?,
                    file: file.to_string(),
                });
                Ok(())
            })()
            .with_context(|| format!("rule {name:?}"))?;
        }
        for (id, output) in raw.pin {
            if !is_pin_id(&id) {
                bail!("pin {id:?}: not a transaction ID (expected e.g. nl-2025-06-11-7f3a9c)");
            }
            let output = convert_output(output).with_context(|| format!("pin {id:?}"))?;
            if let Some(old) = self.pins.insert(id.clone(), Pin { id: id.clone(), output, file: file.to_string() }) {
                bail!("{}", defined_twice(&format!("pin {id:?}"), &old.file, file));
            }
        }
        for (id, raw) in raw.later {
            if !is_pin_id(&id) {
                bail!("later {id:?}: not a transaction ID (expected e.g. nl-2025-06-11-7f3a9c)");
            }
            let later = Later { id: id.clone(), note: raw.note, file: file.to_string() };
            if let Some(old) = self.later.insert(id.clone(), later) {
                bail!("{}", defined_twice(&format!("later {id:?}"), &old.file, file));
            }
        }
        for (name, raw) in raw.trip {
            if !valid_name(&name) {
                bail!("trip {name:?}: names use lowercase letters, digits and dashes");
            }
            if raw.from.0 > raw.to.0 {
                bail!("trip {name:?}: `from` ({}) is after `to` ({})", raw.from.0, raw.to.0);
            }
            let trip = Trip {
                name: name.clone(),
                account: raw.account,
                from: raw.from.0,
                to: raw.to.0,
                note: raw.note,
                file: file.to_string(),
            };
            if let Some(old) = self.trips.insert(name.clone(), trip) {
                bail!("{}", defined_twice(&format!("trip {name:?}"), &old.file, file));
            }
        }
        for raw in raw.subscription {
            let name = raw.name().to_string();
            let sub = Subscription::from_raw(raw, file).with_context(|| format!("subscription {name:?}"))?;
            self.subscriptions.push(sub);
        }
        Ok(())
    }

    fn validate(&self, config: &Config) -> Result<()> {
        let mut names = BTreeMap::new();
        for rule in &self.patterns {
            if let Some(first) = names.insert(&rule.name, &rule.file) {
                bail!("{}", defined_twice(&format!("rule {:?}", rule.name), first, &rule.file));
            }
            for s in rule.when.mentioned_sources() {
                if !config.sources.get(s).is_some_and(|s| s.imported()) {
                    bail!("rule {:?}: {s:?} is not an imported source in config/sources.toml", rule.name);
                }
            }
        }
        let mut sub_files = BTreeMap::new();
        for s in &self.subscriptions {
            if !valid_name(&s.name) {
                bail!("subscription {:?}: names use lowercase letters, digits and dashes", s.name);
            }
            if let Some(first) = sub_files.insert(s.name.as_str(), &s.file) {
                bail!("{}", defined_twice(&format!("subscription {:?}", s.name), first, &s.file));
            }
            if let Flow::Transfer { from, to } = &s.flow {
                for source in [from, to] {
                    if !config.sources.contains_key(source) {
                        bail!("subscription {:?}: {source:?} is not a source in config/sources.toml", s.name);
                    }
                }
            }
        }
        for (label, output) in self.outputs() {
            if let Action::Transfer(target) = &output.action
                && !config.sources.contains_key(target)
            {
                bail!("{label}: transfer target {target:?} is not a source in config/sources.toml");
            }
            if let Action::Subscription(name) = &output.action {
                match self.subscription(name).map(|s| &s.flow) {
                    None => bail!("{label}: no subscription named {name:?}"),
                    Some(Flow::Transfer { from, to }) => bail!(
                        "{label}: {name:?} is a transfer subscription, paid by the transfers from {from} to {to}; \
                         map those with `transfer = \"{to}\"`"
                    ),
                    Some(_) => {}
                }
            }
        }
        Ok(())
    }

    /// Every output with a label for messages.
    pub fn outputs(&self) -> impl Iterator<Item = (String, &Output)> {
        self.patterns
            .iter()
            .map(|r| (format!("rule {:?}", r.name), &r.output))
            .chain(self.pins.values().map(|p| (format!("pin {:?}", p.id), &p.output)))
    }

    /// Every account the rules post to (excluding `@bank`).
    pub fn accounts(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for (label, output) in self.outputs() {
            let mut add = |a: &str| {
                if a != BANK {
                    out.entry(a.to_string()).or_insert_with(|| label.clone());
                }
            };
            match &output.action {
                Action::Account(a) => add(a),
                Action::Split(parts) => parts.iter().for_each(|p| add(&p.account)),
                Action::Transactions(txs) => txs.iter().flat_map(|t| &t.postings).for_each(|p| add(&p.account)),
                _ => {}
            }
        }
        for s in &self.subscriptions {
            if let Some(account) = s.account() {
                out.entry(account.to_string()).or_insert_with(|| format!("subscription {:?}", s.name));
            }
        }
        for t in self.trips.values() {
            out.entry(t.account.clone()).or_insert_with(|| format!("trip {:?}", t.name));
        }
        out
    }

    pub fn subscription(&self, name: &str) -> Option<&Subscription> {
        self.subscriptions.iter().find(|s| s.name == name)
    }
}
