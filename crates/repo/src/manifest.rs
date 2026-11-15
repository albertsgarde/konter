//! `data/MANIFEST`: SHA-256 of every raw file, in `sha256sum` format.
//!
//! jj snapshots the working copy automatically, so an accidentally re-saved raw
//! file would be committed without anyone noticing. The manifest catches that.

use std::collections::BTreeMap;
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

    pub fn save(&self, path: &Path) -> Result<()> {
        let text: String = self.entries.iter().map(|(file, hash)| format!("{hash}  {file}\n")).collect();
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }
}

/// Raw data files: everything under `data/` except the manifest and `data/reference/`.
pub fn raw_files(repo: &Repo) -> Result<Vec<String>> {
    let data = repo.data();
    let mut files = Vec::new();
    if data.is_dir() {
        walk(&data, &mut files)?;
    }
    Ok(files
        .iter()
        .map(|p| p.strip_prefix(&data).unwrap().to_string_lossy().replace('\\', "/"))
        .filter(|rel| rel != "MANIFEST" && !rel.starts_with("reference/"))
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
        let d = findings.error("manifest", format!("{} raw files don't match data/MANIFEST", bad.len()));
        for line in bad {
            d.detail(line);
        }
    }
    Ok(())
}
