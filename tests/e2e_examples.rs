//! Ignored end-to-end regeneration checks for the three committed example fixtures.
//!
//! Each test shells out to `scripts/fixture.sh <example> --check`, which is the only
//! place the pinned recipe lives: it re-runs `cargo metadata` on a `git archive` of the
//! pinned commit, normalises the paths, compares the decompressed digest against both
//! the recorded constant and the committed `.gz`, byte-compares every committed member
//! manifest, and then re-validates the regenerated document through a freshly built
//! binary, asserting the recorded exit code and counters.
//!
//! These tests need the upstream clones and are ignored by default. Point
//! `DEPGATE_FIXTURE_CLONES` at a directory holding (or able to hold) them and run
//! `cargo nextest run -E 'binary(e2e_examples)' --run-ignored all`.
#![expect(clippy::expect_used, reason = "test bodies assert directly")]
#![expect(clippy::ignore_without_reason, reason = "live e2e tests are ignored by default")]

use std::{path::PathBuf, process::Command};

/// The clone directory the fixture script uses, or `None` when the environment does not
/// name one. The script has its own `${TMPDIR}` default, but falling back to it here
/// would turn "no clones configured" into a multi-gigabyte clone of three repositories.
fn clones() -> Option<String> {
    match std::env::var("DEPGATE_FIXTURE_CLONES") {
        Ok(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Runs `scripts/fixture.sh <example> --check`.
///
/// A missing `DEPGATE_FIXTURE_CLONES` fails the test rather than skipping it. `#[ignore]` is
/// already the opt-out; reaching this function means someone asked for the regeneration suite
/// explicitly with `--run-ignored`, and a silent pass there is a green result that verified
/// nothing.
fn regenerate_and_verify(example: &str) {
    let Some(clones) = clones() else {
        panic!(
            "cannot check {example}: set DEPGATE_FIXTURE_CLONES to a directory for the upstream \
             clones to run the fixture regeneration suite"
        );
    };
    let script = repository_root().join("scripts/fixture.sh");
    assert!(script.is_file(), "the fixture script is missing: {}", script.display());
    let output = Command::new("bash")
        .arg(&script)
        .args([example, "--check"])
        .env("DEPGATE_FIXTURE_CLONES", clones)
        .env_remove("RUSTFLAGS")
        .env("CARGO_TERM_COLOR", "never")
        .current_dir(repository_root())
        .output()
        .expect("scripts/fixture.sh should execute");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "scripts/fixture.sh {example} --check failed ({}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    println!("{example}:\n{stdout}");
}

#[test]
#[ignore]
fn lemmy_fixture_regenerates_byte_stably() {
    regenerate_and_verify("lemmy");
}

#[test]
#[ignore]
fn ckb_fixture_regenerates_byte_stably() {
    regenerate_and_verify("ckb");
}

#[test]
#[ignore]
fn coreutils_fixture_regenerates_byte_stably() {
    regenerate_and_verify("coreutils");
}
