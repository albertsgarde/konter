//! What the commands that write data/ do to what's already there, run as the
//! `konter` binary on a copy of tests/fixtures/basic. data/ is never edited
//! and data/MANIFEST catches it if it is (docs/spec.md §1, §3); these are ways
//! the commands themselves once got around that.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A fresh copy of the basic fixture.
fn scratch(name: &str) -> PathBuf {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).expect("the folder is made");
        for entry in std::fs::read_dir(from).expect("the fixture's folder is read") {
            let path = entry.expect("the folder's entry is read").path();
            let target = to.join(path.file_name().expect("the entry has a name"));
            if path.is_dir() {
                copy(&path, &target)
            } else {
                std::fs::copy(&path, &target).map(|_| ()).expect("the file is copied")
            }
        }
    }
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("data-safety").join(name);
    let _ = std::fs::remove_dir_all(&root);
    copy(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic"), &root);
    root
}

fn konter(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_konter"));
    command.arg("--root").arg(root);
    command
}

fn text(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

fn manifest(root: &Path) -> String {
    std::fs::read_to_string(root.join("data/MANIFEST")).expect("data/MANIFEST is read")
}

const LUNAR: &str = "\u{feff}Date,Time,Title,Amount,Balance,Transaction ID\n\
    29.01.2026,09.00,Overførsel,\"500,00\",\"520,00\",00000000-0000-0000-0000-000000000002\n\
    10.01.2026,12.00,Coffee Shop,\"-30,00\",\"20,00\",00000000-0000-0000-0000-000000000001\n";

/// data/MANIFEST is updated by reading it, adding a line and writing it back.
/// Without a lock, two `konter fetch` or `add-export` runs at once, as with
/// several agents in one working copy, lose each other's lines; and one that
/// reads the file while another has emptied it to write it writes back only
/// its own line, wiping the record of every other raw file. The build then
/// says to `konter add-export` each of them, which records whatever they hold
/// by then. Without the lock, this lost lines on nearly every run.
#[test]
fn concurrent_add_exports_keep_every_manifest_line() {
    let root = scratch("manifest-race");
    let before = manifest(&root);
    let incoming = root.join("incoming");
    std::fs::create_dir_all(&incoming).expect("the folder is made");
    let names: Vec<String> = (0..16).map(|i| format!("export-{i}.csv")).collect();
    for name in &names {
        std::fs::write(incoming.join(name), LUNAR).expect("the export is written");
    }
    let runs: Vec<_> = names
        .iter()
        .map(|name| {
            konter(&root)
                .arg("add-export")
                .arg(incoming.join(name))
                .args(["--source", "lunar:b"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("konter starts")
        })
        .collect();
    for mut run in runs {
        run.wait().expect("konter finishes");
    }
    let after = manifest(&root);
    let lost: Vec<&str> = before.lines().filter(|line| !after.contains(line)).collect();
    let missing: Vec<&String> = names.iter().filter(|name| !after.contains(&format!("lunar/555/{name}"))).collect();
    assert!(lost.is_empty() && missing.is_empty(), "lost {lost:?} and never recorded {missing:?}; now:\n{after}");
}

/// `add-export` refuses a file whose name is taken by different content. It
/// once didn't when given the raw file where it lies: it said "already in
/// place" and recorded the file's hash as it was then. A raw file changed by
/// accident (here an editor dropped the newest row, which leaves the running
/// balance unbroken) was accepted, and the transaction was gone from the books
/// without an error.
#[test]
fn add_export_doesnt_accept_a_changed_raw_file() {
    let root = scratch("in-place");
    let path = root.join("data/nordea/1234567890/Konto 1234567890 - 2026-01-31 10.00.00.csv");
    let before = manifest(&root);
    let original = std::fs::read_to_string(&path).expect("the export is read");
    let changed: String =
        original.lines().filter(|l| !l.contains("Concert tickets")).map(|l| format!("{l}\n")).collect();
    std::fs::write(&path, changed).expect("the export is written");
    let build = konter(&root).args(["build", "--as-of", "2026-02-15"]).output().expect("konter runs");
    assert!(text(&build).contains("content changed since it was added"), "{}", text(&build));

    let run = konter(&root).arg("add-export").arg(&path).output().expect("konter runs");
    // Refusing is fine; recording the changed file isn't.
    assert_eq!(manifest(&root), before, "add-export said:\n{}", text(&run));
}

/// The balance given to `add-export --balance` is what the bank showed when
/// the export was taken: it can't be looked up again later, and for an
/// account without running balances the opening balance is worked back from
/// it. Running the command again with another balance once overwrote the
/// recorded one, and recorded the new one in data/MANIFEST, so nothing
/// noticed.
#[test]
fn add_export_doesnt_overwrite_a_recorded_balance() {
    let root = scratch("sidecar");
    let path = root.join("data/lunar/555/statement.csv");
    let sidecar = root.join("data/lunar/555/statement.csv.balance.toml");
    let add = |balance: &str| {
        konter(&root)
            .arg("add-export")
            .arg(&path)
            .args(["--source", "lunar:b", "--balance", balance, "--balance-date", "2026-01-29"])
            .output()
            .expect("konter runs")
    };
    let first = add("520");
    assert!(first.status.success(), "{}", text(&first));
    let recorded = std::fs::read_to_string(&sidecar).expect("the sidecar is read");
    let again = add("5200"); // a slip of the finger
    // Refusing is fine; replacing the record isn't.
    assert_eq!(
        std::fs::read_to_string(&sidecar).expect("the sidecar is read"),
        recorded,
        "add-export said:\n{}",
        text(&again)
    );
}
