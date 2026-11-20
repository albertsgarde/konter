//! The store of transactions on disk: the engine's table
//! (`konter_engine::store`), kept in `store/bank-transactions.csv` and brought
//! up to date with the raw files in `data/` by the commands that add one,
//! `fetch` and `add-export`. Nothing builds from it yet.

use std::collections::BTreeSet;

use anyhow::{Context, Result};

use konter_engine::config::Config;
use konter_engine::findings::Findings;
use konter_engine::model::BankTransaction;
use konter_engine::sources;
use konter_engine::store::{Table, TransactionId};

use crate::load;
use crate::manifest::{self, Manifest};
use crate::repo::Repo;

/// The table's file, from the repository root.
pub const TABLE: &str = "store/bank-transactions.csv";

/// A new transaction ID, from the system's random bytes. A system that has
/// none to give is one nothing here can run on, as the standard library has it too.
fn new_id() -> TransactionId {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).expect("the system gives out random bytes");
    TransactionId::from_random(bytes)
}

/// The table as it is on disk: empty while there's no file.
pub fn table(repo: &Repo) -> Result<Table> {
    let path = repo.root.join(TABLE);
    if !path.exists() {
        return Ok(Table::default());
    }
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {TABLE}"))?;
    Table::parse(&text).with_context(|| format!("reading {TABLE}"))
}

/// The raw rows at the places the table's conflicts name: the copies that
/// say something else than the table, as their files have them. A copy whose
/// file can't be read any more is left out.
pub fn conflicting_copies(repo: &Repo, config: &Config, table: &Table) -> Vec<BankTransaction> {
    let mut copies = Vec::new();
    for (row, conflict) in table.conflicts() {
        // An account that has since left the config has no format to read its files by.
        if !config.sources.contains_key(&row.account) {
            continue;
        }
        let Ok(file) = load::raw_file(repo, config, &row.account, &conflict.place.file) else { continue };
        copies.extend(file.rows.into_iter().filter(|copy| copy.origins.contains(&conflict.place)));
    }
    copies
}

/// Writes the table: to a file beside it first, so that a command that is
/// killed leaves the old table whole.
fn save(repo: &Repo, table: &Table) -> Result<()> {
    let path = repo.root.join(TABLE);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = repo.root.join(format!("{TABLE}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, table.write()?).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("writing {TABLE}"))
}

/// Brings the table up to date with `data/` and says in `findings` what came
/// of it. The raw files are in place whatever happens here, so a table that
/// can't be read or written is an error among the findings and not the end
/// of the command.
pub fn update(repo: &Repo, config: &Config, findings: &mut Findings) {
    if let Err(e) = read_new_files(repo, config, findings) {
        findings.error("store", format!("{TABLE} wasn't brought up to date: {e:#}"));
    }
}

/// Reads every raw file in the manifest that the table hasn't read, source by
/// source and in path order, and writes the table back if that changed it.
/// Reading a file never fails: what the reader wasn't sure of is marked in
/// the table, and warned of here once, when it's read.
fn read_new_files(repo: &Repo, config: &Config, findings: &mut Findings) -> Result<()> {
    // The lock on data/ is the table's too: of two commands at once, the
    // second would otherwise write back the table without the first's rows.
    let _lock = manifest::lock(repo)?;
    let mut table = table(repo)?;
    let before = table.clone();
    let read: BTreeSet<String> = table.files().into_iter().map(str::to_owned).collect();
    let recorded = Manifest::load(&repo.manifest())?;
    let rels = manifest::raw_files(repo)?;
    let (mut files, mut added) = (0, 0);
    let (mut unsure, mut conflicts, mut unread) = (Vec::new(), Vec::new(), Vec::new());
    for (name, source) in &config.sources {
        if !source.imported() {
            continue;
        }
        for rel in sources::files_of(config, name, &rels) {
            if read.contains(rel) || !recorded.entries.contains_key(rel) {
                continue;
            }
            let file = match load::raw_file(repo, config, name, rel) {
                Ok(file) => file,
                Err(e) => {
                    unread.push(format!("{e:#}"));
                    continue;
                }
            };
            let reading = table.read(name, &file.rows, &mut new_id);
            files += 1;
            added += reading.added.len();
            for (new, other) in &reading.unsure {
                if let (Some(new), Some(other)) = (table.get(new), table.get(other)) {
                    unsure.push(format!("{} {} may be {} {}", new.id, new.short(), other.id, other.short()));
                }
            }
            conflicts.extend(reading.conflicts);
        }
    }
    if table != before {
        save(repo, &table)?;
    }
    let news = if files == 0 { String::new() } else { format!(", {added} of them new, from {files} files") };
    findings.ok("store", format!("{TABLE}: {} transactions{news}", table.transactions().count()));
    if !unread.is_empty() {
        findings
            .warn("store", format!("raw files that couldn't be read into {TABLE}: {}", unread.len()))
            .details(unread);
    }
    if !unsure.is_empty() {
        let n = unsure.len();
        let d = findings.warn("store", format!("rows added that may be transactions {TABLE} already had: {n}"));
        d.details(unsure);
        d.detail("→ each was added as a transaction of its own, with the other's ID in `maybe_same_as`");
    }
    if !conflicts.is_empty() {
        let n = conflicts.len();
        let d = findings.warn("store", format!("differences between a copy and what {TABLE} has: {n}"));
        d.details(conflicts);
        d.detail("→ the table kept what it had; the transactions have the copies in `conflicts`");
    }
    Ok(())
}
