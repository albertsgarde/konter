//! `data/MANIFEST`: SHA-256 of every raw file, in `sha256sum` format.
//!
//! jj snapshots the working copy automatically, so an accidentally re-saved raw
//! file would be committed without anyone noticing. The manifest catches that.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};

use konter_engine::findings::Findings;
use konter_engine::hash::sha256_hex;

use crate::repo::{Repo, walk};

#[derive(Default)]
pub struct Manifest {
    /// Path relative to `data/` → hex SHA-256.
    pub entries: BTreeMap<String, String>,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Manifest> {
        if !path.exists() {
            return Ok(Manifest::default());
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut entries = BTreeMap::new();
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let Some((hash, file)) = line.split_once("  ") else {
                bail!("{}:{}: expected \"<sha256>  <path>\"", path.display(), i + 1);
            };
            entries.insert(file.to_string(), hash.to_string());
        }
        Ok(Manifest { entries })
    }

    /// Writes the manifest to a temporary file and renames it into place, so
    /// whoever reads it (a build, jj's snapshot) sees the old file or the new
    /// one, never a half-written one, and a failed write leaves the old one.
    /// Only call it while holding [`lock`].
    pub fn save(&self, path: &Path) -> Result<()> {
        let text: String = self.entries.iter().map(|(file, hash)| format!("{hash}  {file}\n")).collect();
        let tmp = path.with_file_name(format!("{TMP_PREFIX}{}", std::process::id()));
        let written = (|| {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&tmp, path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written.with_context(|| format!("writing {}", path.display()))
    }

    /// Checks that `rel` may be recorded with `bytes`: that neither data/ nor
    /// the manifest already holds something else under that name. Raw files are
    /// only ever added, so a file that has changed since it was added is
    /// refused, even when it's the one given. Returns whether the file is
    /// already in data/. `hint` ends the message when it's refused.
    pub fn check_add(&self, data: &Path, rel: &str, bytes: &[u8], hint: &str) -> Result<bool> {
        let path = data.join(rel);
        let recorded = self.entries.get(rel);
        let new = sha256_hex(bytes);
        if path.exists() {
            let current = sha256_hex(&std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?);
            if recorded.is_some_and(|h| *h != current) {
                bail!(
                    "data/{rel} has changed since it was added; restore it with `jj restore` instead of adding it again"
                );
            }
            if current != new {
                bail!("data/{rel} already exists with different content; {hint}");
            }
            Ok(true)
        } else {
            if recorded.is_some_and(|h| *h != new) {
                bail!(
                    "data/{rel} was added before with different content and is missing; restore it with `jj restore`"
                );
            }
            Ok(false)
        }
    }
}

/// The temporary file [`Manifest::save`] writes, followed by the process ID.
const TMP_PREFIX: &str = "MANIFEST.tmp-";

/// Holds the lock on data/ (and so on data/MANIFEST) until dropped.
pub struct Lock {
    #[cfg(unix)]
    _dir: std::fs::File,
}

/// Locks data/ for a command that adds to it, waiting for any other that
/// holds the lock. Take it before looking at what's there, and keep it until
/// the manifest is saved: two commands updating the manifest at once would
/// otherwise each write back what they read, and lose the other's lines. A
/// temporary file left by one that was killed is removed. (On Unix; elsewhere
/// there's no lock.)
pub fn lock(repo: &Repo) -> Result<Lock> {
    let data = repo.data();
    std::fs::create_dir_all(&data).with_context(|| format!("creating {}", data.display()))?;
    #[cfg(unix)]
    let lock = {
        let dir = std::fs::File::open(&data).with_context(|| format!("opening {}", data.display()))?;
        dir.lock().with_context(|| format!("locking {}", data.display()))?;
        Lock { _dir: dir }
    };
    #[cfg(not(unix))]
    let lock = Lock {};
    for entry in std::fs::read_dir(&data)? {
        let path = entry?.path();
        if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(TMP_PREFIX)) {
            let _ = std::fs::remove_file(path);
        }
    }
    Ok(lock)
}

/// Raw data files: everything under `data/` except the manifest (and its
/// temporary file while it's saved) and `data/reference/`.
pub fn raw_files(repo: &Repo) -> Result<Vec<String>> {
    let data = repo.data();
    let mut files = Vec::new();
    if data.is_dir() {
        walk(&data, &mut files)?;
    }
    Ok(files
        .iter()
        .map(|p| p.strip_prefix(&data).unwrap().to_string_lossy().replace('\\', "/"))
        .filter(|rel| rel != "MANIFEST" && !rel.starts_with(TMP_PREFIX) && !rel.starts_with("reference/"))
        .collect())
}

/// Checks every raw file against the manifest.
pub fn verify(repo: &Repo, findings: &mut Findings) -> Result<()> {
    let manifest = Manifest::load(&repo.manifest())?;
    let files = raw_files(repo)?;
    let mut bad = Vec::new();
    for rel in &files {
        let bytes = std::fs::read(repo.data().join(rel))?;
        match manifest.entries.get(rel) {
            None => bad.push(format!("{rel}: not in data/MANIFEST (add it with `konter add-export`)")),
            Some(hash) if *hash != sha256_hex(&bytes) => {
                bad.push(format!("{rel}: content changed since it was added (restore it with `jj restore`)"))
            }
            Some(_) => {}
        }
    }
    for rel in manifest.entries.keys() {
        if !files.contains(rel) {
            bad.push(format!("{rel}: listed in data/MANIFEST but missing"));
        }
    }
    if bad.is_empty() {
        findings.ok("manifest", format!("{} raw files match data/MANIFEST", files.len()));
    } else {
        findings.error("manifest", format!("{} raw files don't match data/MANIFEST", bad.len())).details(bad);
    }
    Ok(())
}
