//! `konter fetch`, as far as it goes without a bank to talk to.

use std::path::Path;
use std::process::Command;

/// `fetch --source` with a source that can't be fetched says so. It used to
/// skip it like every other source and then report that no source at all has
/// "enable-banking" in its formats.
#[test]
fn a_source_that_cant_be_fetched_is_named() {
    let fetch = |source: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_konter"))
            .arg("--root")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic"))
            .args(["fetch", "--source", source])
            // No secrets: the source is checked before any are read.
            .env("KONTER_CONFIG_DIR", Path::new(env!("CARGO_TARGET_TMPDIR")).join("no-config"))
            .output()
            .expect("konter runs");
        assert_eq!(output.status.code(), Some(2));
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    assert_eq!(fetch("bank:typo"), "error: unknown source bank:typo\n");
    let not_fetched = fetch("bank:a");
    assert!(not_fetched.contains("bank:a isn't fetched"), "{not_fetched}");
}
