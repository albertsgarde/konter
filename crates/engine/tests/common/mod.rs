//! Building the books from text, the way konter-repo does from the files on
//! disk: config, rules, raw bank files and accounts in, the whole build out.

#![allow(dead_code, reason = "each test binary uses its own part of this")]

use std::collections::BTreeMap;

use jiff::civil::Date;
use konter_engine::config::Config;
use konter_engine::findings::Findings;
use konter_engine::manual::Manual;
use konter_engine::pipeline::{self, Build};
use konter_engine::rules::Rules;
use konter_engine::sources::{self, Loaded};
use rust_decimal::Decimal;

/// One Nordea account (`bank:a`) imported from CSV, and books from 2026.
pub const NORDEA: &str = r#"
[books]
start = 2026-01-01

[sources."bank:a"]
bank = "nordea"
number = "1234567890"
ledger = "assets:bank:a"
id_prefix = "ba"
formats = ["nordea-csv"]
unmatched = "allow"
"#;

pub fn config(text: &str) -> Config {
    Config::parse(text, "config/sources.toml").expect("the test's config parses")
}

pub fn rules(config: &Config, files: &[(&str, &str)]) -> Rules {
    Rules::from_files(files.iter().copied(), config).expect("the test's rules load")
}

/// Builds the books. `files`: (source, path relative to data/, text).
pub fn build(config: &Config, rules: Rules, accounts: &[&str], files: &[(&str, &str, &str)], as_of: Date) -> Build {
    try_build(config, rules, accounts, files, as_of).expect("the books build")
}

/// Builds the books, or says why they can't be built.
pub fn try_build(
    config: &Config,
    rules: Rules,
    accounts: &[&str],
    files: &[(&str, &str, &str)],
    as_of: Date,
) -> anyhow::Result<Build> {
    let mut findings = Findings::default();
    let mut raw: BTreeMap<&str, Vec<sources::RawFile>> = BTreeMap::new();
    for (name, source) in &config.sources {
        if source.imported() {
            raw.insert(name, Vec::new());
        }
    }
    for (source, rel, text) in files {
        raw.get_mut(source)
            .expect("the file is for a source in the config")
            .push(sources::parse_raw(config, source, rel, text, None).expect("the test's bank file parses"));
    }
    let loaded = Loaded {
        sources: raw
            .into_iter()
            .map(|(name, files)| (name.to_string(), sources::merge_source(config, name, files, &mut findings)))
            .collect(),
    };
    let quoted: Vec<String> = accounts.iter().map(|a| format!("{a:?}")).collect();
    let manual = Manual::parse(&format!("accounts = [{}]", quoted.join(", ")), "").expect("a list of accounts parses");
    pipeline::assemble(config, rules, loaded, &manual, as_of, findings)
}

/// A Nordea CSV export of account 1234567890, as the bank writes it (newest
/// first, without the byte order mark, which reading the file strips).
/// Rows: (date as `YYYY/MM/DD`, amount, description, balance after), with
/// amounts in Danish notation (`-1.234,56`).
pub fn nordea_csv(rows: &[(&str, &str, &str, &str)]) -> String {
    let mut out = String::from("Bogføringsdato;Beløb;Afsender;Modtager;Navn;Beskrivelse;Saldo;Valuta;Afstemt;\n");
    for (date, amount, description, balance) in rows {
        out.push_str(&format!("{date};{amount};;;;{description};{balance};DKK;;\n"));
    }
    out
}

/// The path of a Nordea export taken at `when` (`2026-01-12 10.00.00`).
pub fn nordea_rel(when: &str) -> String {
    format!("nordea/1234567890/Konto 1234567890 - {when}.csv")
}

/// Danish notation for an amount: `-1069.5` → `-1.069,50`.
pub fn danish(d: Decimal) -> String {
    let text = format!("{:.2}", d.abs());
    let (int, frac) = text.split_once('.').expect("the amount is written with two decimals");
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push('.');
        }
        grouped.push(c);
    }
    format!("{}{grouped},{frac}", if d.is_sign_negative() && !d.is_zero() { "-" } else { "" })
}

/// Everything the books post to `account` up to and including `until`.
pub fn posted(build: &Build, account: &str, until: Date) -> Decimal {
    build
        .books
        .iter()
        .filter(|tx| tx.date <= until)
        .flat_map(|tx| &tx.postings)
        .filter(|p| p.account == account)
        .map(|p| p.amount)
        .sum()
}

/// A small deterministic random number generator (SplitMix64), so the
/// property tests need no dependencies and every failure can be replayed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number in `0..n`.
    pub fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).expect("below n, which is a usize")
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.below(i + 1));
        }
    }
}
