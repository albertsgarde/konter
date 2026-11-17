//! Building the books from text, the way konter-repo does from the files on
//! disk: config, rules, raw bank files and accounts in, the whole build out.

#![allow(dead_code, reason = "each test binary uses its own part of this")]

use std::collections::BTreeMap;
use std::hash::{BuildHasher, DefaultHasher, Hash, Hasher, RandomState};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

use jiff::civil::Date;
use konter_engine::config::Config;
use konter_engine::findings::Findings;
use konter_engine::manual::Manual;
use konter_engine::pipeline::{self, Build};
use konter_engine::rules::Rules;
use konter_engine::sources::{self, Loaded};
use rust_decimal::Decimal;
use serde::Serialize;
use serde::de::DeserializeOwned;

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
    try_build_with(config, rules, accounts, "", files, as_of)
}

/// Builds the books with hand-written `transactions` (the text of
/// config/transactions.toml), or says why they can't be built.
pub fn try_build_with(
    config: &Config,
    rules: Rules,
    accounts: &[&str],
    transactions: &str,
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
    let manual = Manual::parse(&format!("accounts = [{}]", quoted.join(", ")), transactions)?;
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
    nordea_rel_of("1234567890", when)
}

/// The path of a Nordea export of account `number` taken at `when`.
pub fn nordea_rel_of(number: &str, when: &str) -> String {
    format!("nordea/{number}/Konto {number} - {when}.csv")
}

/// Danish notation for an amount, with two decimals or as many as it has:
/// `-1069.5` → `-1.069,50`.
pub fn danish(d: Decimal) -> String {
    let d = d.normalize();
    let text = if d.scale() <= 2 { format!("{:.2}", d.abs()) } else { d.abs().to_string() };
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

/// The variable that gives a run of `check_cases` its seed.
const SEED: &str = "KONTER_TEST_SEED";

/// Checks `property` on generated cases: first on every case saved in
/// `tests/cases/<name>/`, then on `fresh` new ones from `generate`. It gets a
/// size from 1 to 12 that grows along the way, so the first case to fail is
/// among the smallest that do.
///
/// The new cases differ from run to run, so a run can fail where the one
/// before it passed. It can't go the other way: a new case that fails is
/// saved before the test fails, and fails every run from then on until
/// what's wrong is fixed. What's saved is the case and not its seed, so it
/// stays the same case when the generator changes.
pub fn check_cases<C: Serialize + DeserializeOwned>(
    name: &str,
    fresh: u64,
    generate: impl Fn(&mut Rng, usize) -> C,
    property: impl Fn(&C) -> Result<(), String>,
) {
    // A property that panics has failed too.
    let check = |case: &C| {
        std::panic::catch_unwind(AssertUnwindSafe(|| property(case)))
            .unwrap_or_else(|_| Err("the property panicked: its message is above".to_string()))
    };
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases").join(name);
    for path in saved_cases(&dir) {
        let text = std::fs::read_to_string(&path).expect("a saved case is read");
        let case: C = toml::from_str(&text).unwrap_or_else(|e| {
            panic!(
                "{} isn't a case of {name} (any more?): write it again in today's form, or remove it\n{e}",
                path.display()
            )
        });
        if let Err(problem) = check(&case) {
            panic!("{}, saved when it failed, fails:\n{problem}", path.display());
        }
    }
    let seed = match std::env::var(SEED) {
        Ok(seed) => seed.parse().unwrap_or_else(|_| panic!("{SEED} is a whole number, not {seed:?}")),
        // Random, and std has nothing shorter.
        Err(_) => RandomState::new().build_hasher().finish(),
    };
    for n in 0..fresh {
        let size = 1 + usize::try_from(n * 12 / fresh).expect("at most 12");
        let case = generate(&mut Rng::new(seed.wrapping_add(n)), size);
        let Err(problem) = check(&case) else { continue };
        let text = toml::to_string_pretty(&case).expect("a case is written as TOML");
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        let path = dir.join(format!("{:016x}.toml", hasher.finish()));
        std::fs::create_dir_all(&dir).expect("the folder for saved cases is made");
        std::fs::write(&path, text).expect("the failing case is saved");
        panic!(
            "{problem}\n\nThe case is saved as {} and runs first from now on: commit it with the fix.\n\
             ({SEED}={seed} generates it again as case {n} of {name}, until the generator changes.)",
            path.display()
        );
    }
}

/// The cases saved in `dir`, in the order of their names.
fn saved_cases(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = entries.map(|entry| entry.expect("a saved case is listed").path()).collect();
    paths.retain(|path| path.extension().is_some_and(|extension| extension == "toml"));
    paths.sort();
    paths
}
