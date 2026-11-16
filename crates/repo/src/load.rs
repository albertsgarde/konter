//! Reading the books' inputs from the repository: the config, the rules, the
//! raw bank data and the accounts and transactions written by hand. What's done with them is the
//! engine's business; this module only reads.

use std::path::Path;

use anyhow::{Context, Result};

use crate::manifest;
use crate::repo::Repo;
use konter_engine::config::Config;
use konter_engine::findings::Findings;
use konter_engine::manual::{self, Manual};
use konter_engine::rules::Rules;
use konter_engine::sources::{self, Loaded, RawFile, SourceData};

/// `config/sources.toml`.
pub fn config(repo: &Repo) -> Result<Config> {
    let path = repo.sources_toml();
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Config::parse(&text, &path.display().to_string())
}

/// Every rules file under `config/`.
pub fn rules(repo: &Repo, config: &Config) -> Result<Rules> {
    let mut files = Vec::new();
    for path in repo.rules_files()? {
        let rel = repo.rel(&path);
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {rel}"))?;
        files.push((rel, text));
    }
    Rules::from_files(files.iter().map(|(rel, text)| (rel.as_str(), text.as_str())), config)
}

/// A raw data file's text (see [`sources::decode_text`]).
pub fn read_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    sources::decode_text(bytes, &path.display().to_string())
}

/// One raw file of source `name`, with its balance sidecar if it has one.
pub fn raw_file(repo: &Repo, config: &Config, name: &str, rel: &str) -> Result<RawFile> {
    let text = read_text(&repo.data().join(rel))?;
    let sidecar_path = repo.data().join(format!("{rel}{}", sources::SIDECAR));
    let sidecar = if sidecar_path.exists() {
        Some(std::fs::read_to_string(&sidecar_path).with_context(|| format!("reading {}", sidecar_path.display()))?)
    } else {
        None
    };
    sources::parse_raw(config, name, rel, &text, sidecar.as_deref())
}

/// Parses and merges the raw files of one imported source.
pub fn source(repo: &Repo, config: &Config, name: &str, findings: &mut Findings) -> Result<SourceData> {
    let rels = manifest::raw_files(repo)?;
    let mut raw = Vec::new();
    for rel in sources::files_of(config, name, &rels) {
        match raw_file(repo, config, name, rel) {
            Ok(file) => raw.push(file),
            Err(e) => {
                findings.error("parse", format!("{e:#}"));
            }
        }
    }
    Ok(sources::merge_source(config, name, raw, findings))
}

/// Verifies the manifest, parses every raw file and merges each source.
pub fn all_sources(repo: &Repo, config: &Config, findings: &mut Findings) -> Result<Loaded> {
    manifest::verify(repo, findings)?;
    let rels = manifest::raw_files(repo)?;
    let orphans = sources::orphans(config, &rels);
    if !orphans.is_empty() {
        findings.error("data", "raw files in a directory that belongs to no imported source").details(orphans);
    }
    let mut loaded = Loaded { sources: Default::default() };
    for (name, source_config) in &config.sources {
        if source_config.imported() {
            loaded.sources.insert(name.clone(), source(repo, config, name, findings)?);
        }
    }
    Ok(loaded)
}

/// What's written by hand in config/ besides the sources and rules:
/// config/accounts.toml and config/transactions.toml (which may be missing).
pub fn manual(repo: &Repo) -> Result<Manual> {
    let read = |rel: &str| {
        let path = repo.root.join(rel);
        if !path.exists() {
            return Ok(String::new());
        }
        std::fs::read_to_string(&path).with_context(|| format!("reading {rel}"))
    };
    let accounts = read(manual::ACCOUNTS)?;
    if accounts.is_empty() {
        anyhow::bail!("{} is missing: it declares every account the books use", manual::ACCOUNTS);
    }
    Manual::parse(&accounts, &read(manual::TRANSACTIONS)?)
}

/// The text of the categorized reference export (data/reference/transactions.csv),
/// if there is one.
pub fn reference_export(repo: &Repo) -> Option<String> {
    read_text(&repo.data().join("reference/transactions.csv")).ok()
}
