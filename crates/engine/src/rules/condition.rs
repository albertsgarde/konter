//! `when` conditions of pattern rules (docs/spec.md §5).

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use jiff::civil::Date;
use regex::Regex;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::dates::ConfigDate;
use crate::model::BankTransaction;
use crate::money::Amount;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn into_vec(self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    In,
    Out,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawCondition {
    source: Option<OneOrMany>,
    description: Option<OneOrMany>,
    description_re: Option<String>,
    counterparty: Option<OneOrMany>,
    counterparty_name: Option<OneOrMany>,
    counterparty_name_re: Option<String>,
    direction: Option<Direction>,
    amount: Option<Amount>,
    amount_range: Option<(Amount, Amount)>,
    date: Option<ConfigDate>,
    date_from: Option<ConfigDate>,
    date_to: Option<ConfigDate>,
    raw: Option<BTreeMap<String, String>>,
    any: Option<Vec<RawCondition>>,
    not: Option<Box<RawCondition>>,
}

/// A compiled condition. Every field that is set must hold.
#[derive(Debug, Default)]
pub struct Condition {
    pub sources: Option<Vec<String>>,
    description: Option<Vec<String>>,
    description_re: Option<Regex>,
    counterparty: Option<Vec<String>>,
    counterparty_name: Option<Vec<String>>,
    counterparty_name_re: Option<Regex>,
    direction: Option<Direction>,
    amount: Option<Decimal>,
    amount_range: Option<(Decimal, Decimal)>,
    date_from: Option<Date>,
    date_to: Option<Date>,
    raw: Vec<(String, Regex)>,
    any: Option<Vec<Condition>>,
    not: Option<Box<Condition>>,
}

fn regex(pattern: &str) -> Result<Regex> {
    Regex::new(pattern).with_context(|| format!("invalid regex {pattern:?}"))
}

impl Condition {
    pub fn compile(raw: RawCondition) -> Result<Condition> {
        if raw.date.is_some() && (raw.date_from.is_some() || raw.date_to.is_some()) {
            bail!("use either date or date_from/date_to");
        }
        let (date_from, date_to) = match raw.date {
            Some(d) => (Some(d.0), Some(d.0)),
            None => (raw.date_from.map(|d| d.0), raw.date_to.map(|d| d.0)),
        };
        let amount_range = raw.amount_range.map(|(a, b)| (a.0.min(b.0), a.0.max(b.0)));
        let c = Condition {
            sources: raw.source.map(OneOrMany::into_vec),
            description: raw.description.map(OneOrMany::into_vec),
            description_re: raw.description_re.as_deref().map(regex).transpose()?,
            counterparty: raw.counterparty.map(OneOrMany::into_vec),
            counterparty_name: raw.counterparty_name.map(OneOrMany::into_vec),
            counterparty_name_re: raw.counterparty_name_re.as_deref().map(regex).transpose()?,
            direction: raw.direction,
            amount: raw.amount.map(|a| a.0),
            amount_range,
            date_from,
            date_to,
            raw: raw
                .raw
                .unwrap_or_default()
                .into_iter()
                .map(|(field, pattern)| Ok((field, regex(&pattern)?)))
                .collect::<Result<_>>()?,
            any: raw.any.map(|v| v.into_iter().map(Condition::compile).collect::<Result<_>>()).transpose()?,
            not: raw.not.map(|c| Condition::compile(*c).map(Box::new)).transpose()?,
        };
        if c.is_empty() {
            bail!("`when` has no conditions, so it would match everything");
        }
        Ok(c)
    }

    fn is_empty(&self) -> bool {
        self.sources.is_none()
            && self.description.is_none()
            && self.description_re.is_none()
            && self.counterparty.is_none()
            && self.counterparty_name.is_none()
            && self.counterparty_name_re.is_none()
            && self.direction.is_none()
            && self.amount.is_none()
            && self.amount_range.is_none()
            && self.date_from.is_none()
            && self.date_to.is_none()
            && self.raw.is_empty()
            && self.any.is_none()
            && self.not.is_none()
    }

    /// All source names mentioned, including in nested conditions.
    pub fn mentioned_sources(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.sources.iter().flatten().map(String::as_str).collect();
        for c in self.any.iter().flatten() {
            out.extend(c.mentioned_sources());
        }
        if let Some(c) = &self.not {
            out.extend(c.mentioned_sources());
        }
        out
    }

    pub fn matches(&self, t: &BankTransaction) -> bool {
        let opt_in = |list: &Option<Vec<String>>, value: Option<&str>| match list {
            None => true,
            Some(list) => value.is_some_and(|v| list.iter().any(|x| x == v)),
        };
        let opt_re = |re: &Option<Regex>, value: Option<&str>| match re {
            None => true,
            Some(re) => value.is_some_and(|v| re.is_match(v)),
        };
        opt_in(&self.sources, Some(&t.source))
            && opt_in(&self.description, Some(&t.description))
            && opt_re(&self.description_re, Some(&t.description))
            && opt_in(&self.counterparty, t.counterparty.as_deref())
            && opt_in(&self.counterparty_name, t.counterparty_name.as_deref())
            && opt_re(&self.counterparty_name_re, t.counterparty_name.as_deref())
            && match self.direction {
                None => true,
                Some(Direction::In) => t.amount > Decimal::ZERO,
                Some(Direction::Out) => t.amount < Decimal::ZERO,
            }
            && self.amount.is_none_or(|a| a == t.amount)
            && self.amount_range.is_none_or(|(lo, hi)| lo <= t.amount && t.amount <= hi)
            && self.date_from.is_none_or(|d| d <= t.booked)
            && self.date_to.is_none_or(|d| t.booked <= d)
            && self.raw.iter().all(|(field, re)| t.raw.get(field).is_some_and(|v| re.is_match(v)))
            && self.any.as_ref().is_none_or(|cs| cs.iter().any(|c| c.matches(t)))
            && self.not.as_ref().is_none_or(|c| !c.matches(t))
    }
}
