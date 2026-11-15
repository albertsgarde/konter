//! The pipeline against the repository on disk: `konter build` and `konter verify`.
//! Reads the inputs (see `load`), builds and checks the books in memory
//! (`pipeline::assemble`), and writes the journals. hledger, run by `verify`,
//! checks the written journals independently.

use std::process::Command;

use anyhow::{Context, Result};
use jiff::civil::Date;

use crate::load;
use crate::repo::Repo;
use konter_engine::config::Config;
use konter_engine::findings::Findings;
use konter_engine::journal;
use konter_engine::manual;
use konter_engine::pipeline::{self, Build};

pub const MAIN: &str = "main.journal";
pub const GENERATED: &str = "generated.journal";
pub const FORECAST: &str = "forecast.journal";

/// Every file konter writes in ledger/, with its text.
fn files(build: &Build) -> [(&'static str, &str); 3] {
    [(MAIN, journal::MAIN), (GENERATED, &build.journal), (FORECAST, &build.forecast)]
}

/// Reads everything and builds the books in memory. Fails only if inputs can't
/// be read at all.
pub fn build(repo: &Repo, config: &Config, as_of: Date) -> Result<Build> {
    let mut findings = Findings::default();
    let rules = load::rules(repo, config)?;
    let loaded = load::all_sources(repo, config, &mut findings)?;
    let manual = load::manual(repo)?;
    stray_files(repo, &mut findings);
    Ok(pipeline::assemble(config, rules, loaded, &manual, as_of, findings))
}

/// Everything in ledger/ is written by konter, so anything else there is left
/// over (such as the old hand-written accounts.journal) and nothing reads it.
fn stray_files(repo: &Repo, findings: &mut Findings) {
    let Ok(entries) = std::fs::read_dir(repo.ledger("")) else { return };
    let mut stray: Vec<String> = entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|name| ![MAIN, GENERATED, FORECAST].contains(&name.as_str()))
        .collect();
    if stray.is_empty() {
        return;
    }
    stray.sort();
    let d = findings.warn("ledger", "files in ledger/ that konter doesn't write; nothing reads them");
    for name in stray {
        d.detail(format!("ledger/{name}"));
    }
    d.detail(format!("→ accounts go in {}, hand-written transactions in {}", manual::ACCOUNTS, manual::TRANSACTIONS));
}

/// Writes the files in ledger/.
pub fn write(repo: &Repo, build: &Build) -> Result<()> {
    std::fs::create_dir_all(repo.ledger("")).context("creating ledger/")?;
    for (file, text) in files(build) {
        let path = repo.ledger(file);
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// Runs `hledger check --strict` on the books as written and adds the result to
/// the findings: an independent check of what the build checked itself. Only a
/// warning if hledger isn't installed.
pub fn hledger_check(repo: &Repo, findings: &mut Findings) {
    let main = repo.ledger(MAIN);
    match Command::new("hledger").arg("-f").arg(&main).args(["check", "--strict"]).output() {
        Err(_) => {
            findings.warn("hledger", "hledger not found; skipped `hledger check --strict`");
        }
        Ok(out) if out.status.success() => {
            findings.ok("hledger", "hledger check --strict");
        }
        Ok(out) => {
            let d = findings.error("hledger", "hledger check --strict failed");
            for line in String::from_utf8_lossy(&out.stderr).lines().filter(|l| !l.trim().is_empty()).take(30) {
                d.detail(line.to_string());
            }
        }
    }
}

/// Rebuilds with the `--as-of` recorded in the committed journal and checks the
/// output is byte-identical.
pub fn verify(repo: &Repo, config: &Config) -> Result<Findings> {
    let path = repo.ledger(GENERATED);
    let committed = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let as_of = journal::as_of_from_header(&committed)
        .context("ledger/generated.journal has no `--as-of` header; run `konter build` first")?;
    let mut build = build(repo, config, as_of)?;
    let mut findings = std::mem::take(&mut build.findings);
    for (file, fresh) in files(&build) {
        let committed = std::fs::read_to_string(repo.ledger(file)).unwrap_or_default();
        if committed == fresh {
            findings.ok("verify", format!("ledger/{file} is up to date (--as-of {as_of})"));
            continue;
        }
        let d = findings.error("verify", format!("ledger/{file} differs from a fresh build with --as-of {as_of}"));
        match committed.lines().zip(fresh.lines()).enumerate().find(|(_, (a, b))| a != b) {
            Some((i, (a, b))) => {
                d.detail(format!("first difference at line {}:", i + 1));
                d.detail(format!("  committed: {a}"));
                d.detail(format!("  fresh:     {b}"));
            }
            None => {
                d.detail(format!(
                    "committed has {} lines, a fresh build {}",
                    committed.lines().count(),
                    fresh.lines().count()
                ));
            }
        }
        d.detail("run `konter build --as-of <date>` (or plain `konter build`) and commit the result");
    }
    Ok(findings)
}
