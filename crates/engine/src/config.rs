//! `config/sources.toml`: global settings and every account you own.

use std::collections::BTreeMap;
use std::fmt;

use anyhow::{Context, Result, bail};
use jiff::civil::Date;
use serde::Deserialize;

use crate::dates::ConfigDate;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub books: Books,
    #[serde(default)]
    pub sources: BTreeMap<String, Source>,
    #[serde(default)]
    pub banks: BTreeMap<String, Bank>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Books {
    pub start: ConfigDate,
    #[serde(default = "default_currency")]
    pub currency: String,
    #[serde(default = "default_window")]
    pub transfer_window_days: i64,
}

fn default_currency() -> String {
    "DKK".into()
}

fn default_window() -> i64 {
    5
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum Format {
    NordeaCsv,
    LunarCsv,
    EnableBanking,
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Format::NordeaCsv => "nordea-csv",
            Format::LunarCsv => "lunar-csv",
            Format::EnableBanking => "enable-banking",
        })
    }
}

/// How loudly unmatched transactions are reported (docs/spec.md §5).
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Policy {
    #[default]
    Error,
    Warn,
    Allow,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub bank: String,
    /// Account number; raw data lives in `data/<bank>/<number>/`.
    pub number: Option<String>,
    pub iban: Option<String>,
    /// The hledger account this bank account is booked to.
    pub ledger: String,
    /// Prefix for transaction IDs, e.g. `nl` → `nl-2025-06-11-7f3a9c`.
    pub id_prefix: Option<String>,
    /// Formats raw data may come in. Empty means the account isn't imported.
    #[serde(default)]
    pub formats: Vec<Format>,
    #[serde(default)]
    pub unmatched: Policy,
    /// Overrides `books.start` for this account.
    pub start: Option<ConfigDate>,
    /// Where transfers to or from this account dated before its `start` are
    /// booked (for an account that wasn't part of your finances back then).
    pub before_start: Option<String>,
    /// Overrides `books.currency` for this account.
    pub currency: Option<String>,
    /// With both CSV and Enable Banking data: CSV before this date, API from it on.
    pub cutover: Option<ConfigDate>,
    /// Money in this account is set aside for something (donations, say): the
    /// spending report counts money moved in as gone, and leaves out the
    /// expenses paid from it.
    #[serde(default)]
    pub set_aside: bool,
}

/// Bank-level settings.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bank {
    pub enable_banking: Option<EnableBankingBank>,
    /// How many days back the bank lets you export or fetch. Data must be
    /// downloaded again before the window passes the last download.
    pub history_days: Option<i64>,
}

/// How Enable Banking knows the bank (`konter banks` lists the names).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnableBankingBank {
    pub aspsp: String,
    pub country: String,
}

impl Source {
    pub fn imported(&self) -> bool {
        !self.formats.is_empty()
    }
}

impl Config {
    /// Parses and checks `config/sources.toml`; `name` is for messages.
    pub fn parse(text: &str, name: &str) -> Result<Config> {
        let config: Config = toml::from_str(text).with_context(|| format!("parsing {name}"))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        let mut prefixes = BTreeMap::new();
        for (name, s) in &self.sources {
            if !name.contains(':') {
                bail!("source {name:?}: names look like \"<bank>:<account>\"");
            }
            if s.imported() {
                let Some(prefix) = &s.id_prefix else {
                    bail!("source {name:?} is imported, so it needs an id_prefix");
                };
                if !valid_id_prefix(prefix) {
                    bail!(
                        "source {name:?}: id_prefix {prefix:?} must be lowercase letters (a-z), or its \
                         transactions' IDs couldn't be pinned"
                    );
                }
                if s.number.is_none() {
                    bail!("source {name:?} is imported, so it needs a number (its data directory)");
                }
                if let Some(other) = prefixes.insert(prefix.clone(), name.clone()) {
                    bail!("sources {other:?} and {name:?} share id_prefix {prefix:?}");
                }
            }
            if s.before_start.is_some() && s.start.is_none() {
                bail!("source {name:?}: before_start only makes sense together with start");
            }
            if self.sources.values().filter(|o| o.ledger == s.ledger).count() > 1 {
                bail!("source {name:?}: ledger account {:?} is used by several sources", s.ledger);
            }
        }
        Ok(())
    }

    pub fn start(&self) -> Date {
        self.books.start.0
    }

    pub fn source_start(&self, source: &Source) -> Date {
        source.start.map(|d| d.0).unwrap_or(self.books.start.0)
    }

    pub fn source_currency<'a>(&'a self, source: &'a Source) -> &'a str {
        source.currency.as_deref().unwrap_or(&self.books.currency)
    }

    /// The data directory of an imported source, relative to `data/`.
    pub fn data_dir(source: &Source) -> Option<String> {
        source.number.as_ref().map(|n| format!("{}/{}", source.bank, n))
    }

    /// Finds the source whose data directory is `dir` (relative to `data/`).
    pub fn source_for_dir(&self, dir: &str) -> Option<&str> {
        self.sources
            .iter()
            .find(|(_, s)| s.imported() && Config::data_dir(s).as_deref() == Some(dir))
            .map(|(name, _)| name.as_str())
    }
}

/// Whether `prefix` can start a transaction ID: lowercase ASCII letters, as
/// pins and later marks expect (`<prefix>-<date>-<hash>`, split on `-`).
pub fn valid_id_prefix(prefix: &str) -> bool {
    !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_lowercase())
}
