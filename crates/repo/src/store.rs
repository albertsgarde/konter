//! The store of transactions on disk: the engine's tables
//! (`konter_engine::store`), kept in `store/` and brought up to date with the
//! raw files in `data/` by the commands that read them, `fetch`,
//! `add-export` and `store`. The books are built from the store alone
//! (`load`): only what's here looks at the raw files.

use anyhow::{Context, Result};

use konter_engine::config::Config;
use konter_engine::findings::Findings;
use konter_engine::sources;
use konter_engine::store::{Files, Table, TransactionId};

use crate::load;
use crate::manifest::{self, Manifest};
use crate::repo::Repo;

/// The table of transactions, from the repository root.
pub const TABLE: &str = "store/bank-transactions.csv";
/// The table of the raw files that have been read, from the repository root.
pub const FILES: &str = "store/files.csv";

/// A new transaction ID, from the system's random bytes. A system that has
/// none to give is one nothing here can run on, as the standard library has it too.
fn new_id() -> TransactionId {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).expect("the system gives out random bytes");
    TransactionId::from_random(bytes)
}

/// The text of one of the store's files, if it's there.
fn text(repo: &Repo, rel: &str) -> Result<Option<String>> {
    let path = repo.root.join(rel);
    if !path.exists() {
        return Ok(None);
    }
    std::fs::read_to_string(&path).with_context(|| format!("reading {rel}")).map(Some)
}

/// The store as it is on disk: each table is empty while it has no file.
pub fn tables(repo: &Repo) -> Result<(Table, Files)> {
    let table = match text(repo, TABLE)? {
        Some(text) => Table::parse(&text).with_context(|| format!("reading {TABLE}"))?,
        None => Table::default(),
    };
    let files = match text(repo, FILES)? {
        Some(text) => Files::parse(&text).with_context(|| format!("reading {FILES}"))?,
        None => Files::default(),
    };
    Ok((table, files))
}

/// Writes one of the store's files: to a file beside it first, so that a
/// command that is killed leaves the old one whole.
fn save(repo: &Repo, rel: &str, text: &str) -> Result<()> {
    let path = repo.root.join(rel);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = repo.root.join(format!("{rel}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("writing {rel}"))
}

/// Brings the store up to date with `data/` and says in `findings` what came
/// of it. The raw files are in place whatever happens here, so a store that
/// can't be read or written is an error among the findings and not the end
/// of the command. Returns whether the store is up to date.
pub fn update(repo: &Repo, config: &Config, findings: &mut Findings) -> bool {
    match read_new_files(repo, config, findings) {
        Ok(()) => true,
        Err(e) => {
            findings.error("store", format!("the store wasn't brought up to date: {e:#}"));
            false
        }
    }
}

/// The raw files in the manifest that the store hasn't read, each with its
/// source: a file that isn't among the files read, or whose balance, recorded
/// beside it later, isn't.
fn unread<'a>(
    config: &'a Config,
    rels: &'a [String],
    recorded: &Manifest,
    files: &Files,
) -> Vec<(&'a str, &'a String)> {
    let mut unread = Vec::new();
    for (name, source) in &config.sources {
        if !source.imported() {
            continue;
        }
        for rel in sources::files_of(config, name, rels) {
            let beside = format!("{rel}{}", sources::SIDECAR);
            let new = !files.has(rel) || (rels.contains(&beside) && !files.has(&beside));
            if new && recorded.entries.contains_key(rel) {
                unread.push((name.as_str(), rel));
            }
        }
    }
    unread
}

/// Checks `data/` against its manifest, and that every raw file belongs to a
/// source: what the commands that read `data/` and `verify` say about it.
/// A build doesn't look.
pub fn check_data(repo: &Repo, config: &Config, findings: &mut Findings) -> Result<()> {
    manifest::verify(repo, findings)?;
    let rels = manifest::raw_files(repo)?;
    let orphans = sources::orphans(config, &rels);
    if !orphans.is_empty() {
        findings.error("data", "raw files in a directory that belongs to no imported source").details(orphans);
    }
    Ok(())
}

/// Says in `findings` which raw files in the manifest the store hasn't read,
/// if any: the books are built without their rows.
pub fn check_read(repo: &Repo, config: &Config, findings: &mut Findings) -> Result<()> {
    let (_, files) = tables(repo)?;
    let recorded = Manifest::load(&repo.manifest())?;
    let rels = manifest::raw_files(repo)?;
    let unread = unread(config, &rels, &recorded, &files);
    if !unread.is_empty() {
        findings
            .error("store", format!("{} raw files haven't been read into the store", unread.len()))
            .details(unread.iter().map(|(_, rel)| format!("data/{rel}")))
            .detail("→ the books are built without their rows; `konter store` reads them");
    }
    Ok(())
}

/// Reads every raw file in the manifest that the store hasn't read, source by
/// source and in path order, and writes the store back if that changed it.
/// Reading a file never fails: what the reader wasn't sure of is marked in
/// the table, and warned of here once, when it's read.
fn read_new_files(repo: &Repo, config: &Config, findings: &mut Findings) -> Result<()> {
    // The lock on data/ is the store's too: of two commands at once, the
    // second would otherwise write back the store without the first's rows.
    let _lock = manifest::lock(repo)?;
    check_data(repo, config, findings)?;
    let (mut table, mut files) = tables(repo)?;
    let before = (table.clone(), files.clone());
    let recorded = Manifest::load(&repo.manifest())?;
    let rels = manifest::raw_files(repo)?;
    let (mut read, mut added) = (0, 0);
    let (mut unsure, mut conflicts, mut unparsed) = (Vec::new(), Vec::new(), Vec::new());
    for (name, rel) in unread(config, &rels, &recorded, &files) {
        let file = match load::raw_file(repo, config, name, rel) {
            Ok(file) => file,
            Err(e) => {
                unparsed.push(format!("{e:#}"));
                continue;
            }
        };
        let reading = sources::read_into(&mut table, &mut files, name, &file, &mut new_id);
        read += 1;
        added += reading.added.len();
        for (new, other) in &reading.unsure {
            if let (Some(new), Some(other)) = (table.get(new), table.get(other)) {
                unsure.push(format!("{} {} may be {} {}", new.id, new.short(), other.id, other.short()));
            }
        }
        conflicts.extend(reading.conflicts);
    }
    if table != before.0 {
        save(repo, TABLE, &table.write()?)?;
    }
    if files != before.1 {
        save(repo, FILES, &files.write()?)?;
    }
    let news = if read == 0 { String::new() } else { format!(", {added} of them new, from {read} files") };
    findings.ok("store", format!("{TABLE}: {} transactions{news}", table.transactions().count()));
    if !unparsed.is_empty() {
        let n = unparsed.len();
        findings.warn("store", format!("raw files that couldn't be read into the store: {n}")).details(unparsed);
    }
    if !unsure.is_empty() {
        let n = unsure.len();
        let d = findings.warn("store", format!("rows added that may be transactions {TABLE} already had: {n}"));
        d.details(unsure);
        d.detail("→ each was added as a row of its own, with the other's ID in `maybe_same_as`");
    }
    if !conflicts.is_empty() {
        let n = conflicts.len();
        let d = findings.warn("store", format!("differences between a copy and what {TABLE} has: {n}"));
        d.details(conflicts);
        d.detail("→ the table kept what it had; the transactions have the copies in `conflicts`");
    }
    Ok(())
}
