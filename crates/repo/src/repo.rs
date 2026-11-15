//! Locations of everything in the repository.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

pub struct Repo {
    pub root: PathBuf,
}

impl Repo {
    /// Finds the repository root: the nearest ancestor with `config/sources.toml`.
    pub fn discover(start: &Path) -> Result<Repo> {
        let start = start.canonicalize()?;
        for dir in start.ancestors() {
            if dir.join("config/sources.toml").is_file() {
                return Ok(Repo { root: dir.to_path_buf() });
            }
        }
        bail!("no config/sources.toml found in {} or any parent directory", start.display())
    }

    pub fn sources_toml(&self) -> PathBuf {
        self.root.join("config/sources.toml")
    }

    /// `config/rules.toml` and every `*.toml` under `config/rules/`, sorted.
    pub fn rules_files(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        let single = self.root.join("config/rules.toml");
        if single.is_file() {
            files.push(single);
        }
        let dir = self.root.join("config/rules");
        if dir.is_dir() {
            let mut found = Vec::new();
            walk(&dir, &mut found)?;
            found.retain(|p| p.extension().is_some_and(|e| e == "toml"));
            files.extend(found);
        }
        Ok(files)
    }

    pub fn data(&self) -> PathBuf {
        self.root.join("data")
    }

    pub fn manifest(&self) -> PathBuf {
        self.root.join("data/MANIFEST")
    }

    pub fn ledger(&self, file: &str) -> PathBuf {
        self.root.join("ledger").join(file)
    }

    /// Path relative to the repository root, for messages.
    pub fn rel(&self, path: &Path) -> String {
        path.strip_prefix(&self.root).unwrap_or(path).display().to_string()
    }
}

/// Recursively lists files under `dir`, sorted.
pub fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?.map(|e| e.map(|e| e.path())).collect::<Result<_, _>>()?;
    entries.sort();
    for path in entries {
        if path.is_dir() {
            walk(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}
