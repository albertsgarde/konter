//! `konter init`, run as the binary: new books build as they are, and init
//! never touches books that are already there.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("init").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn konter(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_konter")).args(args).output().expect("konter runs")
}

fn text(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn new_books_build_cleanly() {
    let dir = scratch("new").join("books");
    let books = dir.to_str().expect("the path is UTF-8");
    let init = konter(&["init", books]);
    assert!(init.status.success(), "{}", text(&init));
    // A git repo, unless git isn't there to make one (as in the Nix build), which it says.
    assert!(dir.join(".git").is_dir() || text(&init).contains("isn't a git repo yet"), "{}", text(&init));

    let build = konter(&["--root", books, "build", "--strict"]);
    assert!(build.status.success(), "{}", text(&build));
    assert!(text(&build).contains("0 errors, 0 warnings"), "{}", text(&build));
    assert!(dir.join("ledger/main.journal").is_file());
}

#[test]
fn init_leaves_existing_books_alone() {
    let dir = scratch("existing");
    std::fs::create_dir_all(dir.join("config")).expect("the folder is made");
    std::fs::write(dir.join("config/sources.toml"), "mine").expect("the file is written");
    let init = konter(&["init", dir.to_str().expect("the path is UTF-8")]);
    assert!(!init.status.success(), "{}", text(&init));
    assert!(text(&init).contains("already exists"), "{}", text(&init));
    assert_eq!(std::fs::read_to_string(dir.join("config/sources.toml")).expect("the file is read"), "mine");
    assert!(!dir.join("data").exists() && !dir.join("README.md").exists());
}

/// A freshly cloned empty repo may have a README and a .gitignore already:
/// they're kept, and the books go in around them.
#[test]
fn init_keeps_a_cloned_readme() {
    let dir = scratch("cloned");
    std::fs::create_dir_all(&dir).expect("the folder is made");
    std::fs::write(dir.join("README.md"), "# My books\n").expect("the file is written");
    let init = konter(&["init", dir.to_str().expect("the path is UTF-8")]);
    assert!(init.status.success(), "{}", text(&init));
    assert_eq!(std::fs::read_to_string(dir.join("README.md")).expect("the file is read"), "# My books\n");
    assert!(dir.join("config/sources.toml").is_file());
}
