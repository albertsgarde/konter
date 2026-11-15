//! `konter init`: new, empty books that build as they are.

use std::io::Write;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The skeleton: path, contents, and whether it's left alone if it's there
/// already (as a README or .gitignore may be in a freshly cloned empty repo).
const FILES: &[(&str, &str, bool)] = &[
    ("config/sources.toml", include_str!("skeleton/sources.toml"), false),
    ("config/accounts.toml", include_str!("skeleton/accounts.toml"), false),
    ("config/rules/README.md", include_str!("skeleton/rules-README.md"), false),
    ("data/MANIFEST", "", false),
    ("README.md", include_str!("skeleton/README.md"), true),
    (".gitignore", include_str!("skeleton/gitignore"), true),
];

/// What `init` did about version control.
#[derive(Debug, PartialEq, Eq)]
pub enum Git {
    /// Made `dir` a new git repo.
    Initialized,
    /// `dir` was a git repo already.
    Existing,
    /// Couldn't run `git init`; the reason.
    Failed(String),
}

/// Writes new books in `dir` (made if it's missing), starting on `start`, and
/// makes `dir` a git repo first, so that no repo around it ever sees the files
/// without one. Refuses if `dir` already has books (`config/` or `data/`):
/// it only ever adds files, never replaces one.
pub fn init(dir: &Path, start: jiff::civil::Date) -> Result<(Git, Vec<&'static str>)> {
    for existing in ["config", "data"] {
        if dir.join(existing).exists() {
            bail!("{} already exists: init only makes new books", dir.join(existing).display());
        }
    }
    std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    let git = if dir.join(".git").exists() {
        Git::Existing
    } else {
        match Command::new("git").arg("init").arg("--quiet").arg(dir).status() {
            Ok(status) if status.success() => Git::Initialized,
            Ok(status) => Git::Failed(format!("git init exited with {status}")),
            Err(e) => Git::Failed(format!("couldn't run git: {e}")),
        }
    };
    let mut written = Vec::new();
    for &(rel, text, keep_existing) in FILES {
        let path = dir.join(rel);
        if keep_existing && path.exists() {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("making {}", parent.display()))?;
        }
        let text = text.replace("@START@", &start.to_string());
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("creating {}", path.display()))?;
        file.write_all(text.as_bytes()).with_context(|| format!("writing {}", path.display()))?;
        written.push(rel);
    }
    Ok((git, written))
}
