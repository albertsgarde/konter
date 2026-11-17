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
