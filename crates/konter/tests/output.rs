//! What the `konter` binary does when nobody reads what it prints.

use std::path::Path;
use std::process::{Command, Stdio};

/// `konter list | head` closes the pipe while konter still has lines to
/// write. That ends the command quietly, with the status a shell reports for
/// one killed by SIGPIPE, and not with the panic `println!` makes of a failed
/// write.
#[test]
fn a_closed_pipe_ends_the_command_quietly() {
    let (reader, writer) = std::io::pipe().expect("a pipe is made");
    drop(reader);
    let output = Command::new(env!("CARGO_BIN_EXE_konter"))
        .arg("--root")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic"))
        .arg("list")
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .expect("konter runs");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    assert_eq!(output.status.code(), Some(141));
}

/// `konter explain <id>` shows a transaction as the store has it, with the
/// original fields of the raw file it was read from and where that is.
#[test]
fn explain_shows_a_transaction_and_where_it_was_read_from() {
    let explain = |id: &str| {
        Command::new(env!("CARGO_BIN_EXE_konter"))
            .arg("--root")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic"))
            .args(["explain", id])
            .output()
            .expect("konter runs")
    };
    let shown = explain("00000000-0000-4000-8000-000000000008");
    let text = String::from_utf8_lossy(&shown.stdout);
    assert!(shown.status.success(), "{text}");
    for line in [
        "00000000-0000-4000-8000-000000000008  (bank:a)",
        "description   \"Top up\"",
        "raw (nordea-csv):",
        "data/nordea/1234567890/Konto 1234567890 - 2026-01-31 10.00.00.csv:3",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
    let unknown = explain("00000000-0000-4000-8000-ffffffffffff");
    assert!(!unknown.status.success());
}
