//! Binary-level contract tests for the P0 command-line scaffold.
#![expect(clippy::expect_used, reason = "test bodies assert directly")]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Output,
};

use assert_cmd::cargo::cargo_bin_cmd;

/// The binary under test with Cargo's colour output disabled so the child's inherited
/// stderr carries no ANSI escapes (CI sets `CARGO_TERM_COLOR=always`).
fn depgate() -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!();
    command.env_remove("RUSTFLAGS").env("CARGO_TERM_COLOR", "never");
    command
}

fn output(arguments: &[&str]) -> Output {
    depgate().args(arguments).output().expect("cargo-depgate should execute")
}

/// Cargo status verbs that `cargo metadata` may print on its inherited stderr before the
/// tool's own lines: lock contention, index updates and downloads of platform-specific
/// crates the host never compiled. They vary run to run and machine to machine.
const CARGO_STATUS_VERBS: &[&str] = &[
    "Blocking",
    "Downloading",
    "Downloaded",
    "Updating",
    "Locking",
    "Adding",
    "Removing",
    "Fetch",
    "Waiting",
    "Checking",
    "Compiling",
    "Finished",
];

/// Strips ANSI escape sequences from one line.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find('\x1b') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        // CSI sequences: ESC [ ... final byte in 0x40..=0x7e
        if let Some(body) = after.strip_prefix('[') {
            let end =
                body.find(|c: char| ('\x40'..='\x7e').contains(&c)).map_or(body.len(), |i| i + 1);
            rest = &body[end..];
        } else {
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Keeps only the tool's own stderr lines: ANSI escapes removed and every line that
/// starts with a Cargo status verb dropped, wherever it appears.
fn strip_cargo_status_lines(stderr: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(stderr);
    let mut kept = String::with_capacity(text.len());
    for line in text.lines() {
        let clean = strip_ansi(line);
        let first_word = clean.split_whitespace().next().unwrap_or("");
        if CARGO_STATUS_VERBS.contains(&first_word) {
            continue;
        }
        kept.push_str(&clean);
        kept.push('\n');
    }
    kept.into_bytes()
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn basic_fixture_root() -> PathBuf {
    repository_root().join("tests/fixtures/ws-basic")
}

fn config_error_fixture_root() -> PathBuf {
    repository_root().join("tests/fixtures/ws-config-errors")
}

/// Runs `check` on a path-only fixture with its own `depgate.toml`, offline.
fn fixture_check(fixture: &Path) -> Output {
    depgate()
        .args(["check", "--manifest-path"])
        .arg(fixture.join("Cargo.toml"))
        .arg("--config")
        .arg(fixture.join("depgate.toml"))
        .arg("--offline")
        .output()
        .expect("cargo-depgate should execute the fixture check")
}

fn fail_cargo_path() -> PathBuf {
    repository_root().join("tests/bin/fail-cargo")
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination)
        .unwrap_or_else(|error| panic!("failed to create {}: {error}", destination.display()));

    for entry in fs::read_dir(source)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", source.display()))
    {
        let entry = entry.expect("fixture directory entry should be readable");
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry
            .file_type()
            .unwrap_or_else(|error| panic!("failed to inspect {}: {error}", source_path.display()));

        if file_type.is_dir() {
            copy_tree(&source_path, &destination_path);
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path).unwrap_or_else(|error| {
                panic!(
                    "failed to copy {} to {}: {error}",
                    source_path.display(),
                    destination_path.display()
                )
            });
        } else {
            panic!("fixture entry {} is not a regular file or directory", source_path.display());
        }
    }
}

#[test]
fn direct_and_cargo_plugin_help_are_identical() {
    let direct = output(&["--help"]);
    let cargo_plugin = output(&["depgate", "--help"]);

    assert!(direct.status.success(), "direct help failed: {direct:?}");
    assert!(cargo_plugin.status.success(), "Cargo-plugin help failed: {cargo_plugin:?}");
    assert_eq!(direct.stdout, cargo_plugin.stdout);

    let stdout = String::from_utf8_lossy(&direct.stdout);
    assert!(stdout.contains("cargo depgate"), "unexpected help output: {stdout}");
}

#[test]
fn help_subcommands_succeed_for_direct_and_cargo_plugin_invocations() {
    for arguments in [&["help"][..], &["depgate", "help"][..]] {
        let output = output(arguments);

        assert!(output.status.success(), "help failed: {output:?}");
        assert!(output.stderr.is_empty(), "help wrote to stderr: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("cargo depgate"),
            "unexpected help output: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn help_check_succeeds_with_usage_on_stdout() {
    let output = output(&["help", "check"]);

    assert!(output.status.success(), "help check failed: {output:?}");
    assert!(output.stderr.is_empty(), "help check wrote to stderr: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("cargo depgate"),
        "unexpected help output: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn unrecognized_flags_fail_on_stderr() {
    let output = output(&["--not-a-flag"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty(), "unexpected stdout: {output:?}");
    assert!(!output.stderr.is_empty(), "expected an error on stderr: {output:?}");
}

#[test]
fn version_uses_the_installed_binary_name() {
    let output = output(&["--version"]);

    assert!(output.status.success(), "version request failed: {output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("cargo-depgate {}\n", env!("CARGO_PKG_VERSION"))
    );
}

/// P0 deliberately keeps both `check` invocation forms as identical parse-only stubs.
///
/// The expected exit code and stderr legitimately change when P1, P2, and P4 implement `check`.
#[test]
fn direct_and_cargo_plugin_check_stubs_are_identical() {
    let direct = output(&["check"]);
    let cargo_plugin = output(&["depgate", "check"]);

    assert_eq!(direct.status.code(), Some(2));
    assert_eq!(cargo_plugin.status.code(), Some(2));
    assert_eq!(
        strip_cargo_status_lines(&direct.stderr),
        strip_cargo_status_lines(&cargo_plugin.stderr)
    );
}

#[test]
fn basic_workspace_check_passes_end_to_end() {
    let fixture = basic_fixture_root();
    let output = depgate()
        .args(["check", "--manifest-path"])
        .arg(fixture.join("Cargo.toml"))
        .arg("--config")
        .arg(fixture.join("depgate.toml"))
        .arg("--offline")
        .output()
        .expect("cargo-depgate should execute the basic workspace check");

    assert_eq!(output.status.code(), Some(0), "basic workspace check failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().last(),
        Some("ok: 8 rules, 0 violations"),
        "unexpected basic workspace report: {stdout}"
    );
}

#[test]
fn timings_go_to_stderr_and_the_report_stays_on_stdout() {
    let fixture = basic_fixture_root();
    let output = depgate()
        .args(["check", "--manifest-path"])
        .arg(fixture.join("Cargo.toml"))
        .arg("--config")
        .arg(fixture.join("depgate.toml"))
        .args(["--offline", "--timings"])
        .output()
        .expect("cargo-depgate should execute the timed check");

    assert_eq!(output.status.code(), Some(0), "timed check failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.lines().last(), Some("ok: 8 rules, 0 violations"));
    assert!(
        !stdout.lines().any(|line| line.contains('\t')),
        "the report stream must not carry timings lines: {stdout}"
    );

    let stderr = String::from_utf8_lossy(&strip_cargo_status_lines(&output.stderr)).into_owned();
    let labels: Vec<&str> = stderr.lines().filter_map(|line| line.split('\t').next()).collect();
    let phases =
        ["read", "parse", "graph", "traversals", "evaluate", "manifest", "report", "total"];
    let counters = [
        "packages",
        "members",
        "normal_edges",
        "names",
        "superset_extra_edges",
        "direct_optional_decls",
        "unrebased_path_deps",
        "rules",
        "violations",
        "matches",
    ];
    let expected: Vec<&str> = phases.iter().chain(counters.iter()).copied().collect();
    assert_eq!(labels, expected, "unexpected --timings stream: {stderr}");
    assert!(stderr.lines().any(|line| line == "rules\t8"), "rules counter missing: {stderr}");
}

#[test]
fn violations_are_reported_with_a_fail_prefix_and_exit_one() {
    let fixture = basic_fixture_root();
    let scratch = tempfile::tempdir().expect("temporary directory");
    let config = scratch.path().join("depgate.toml");
    std::fs::write(
        &config,
        "schema = 1\n[manifest]\nversions-in-root = false\n[rules.app]\nleaf = true\n[rules.tool]\ndirect = [\"core\"]\n",
    )
    .expect("write the violating config");

    let output = depgate()
        .args(["check", "--manifest-path"])
        .arg(fixture.join("Cargo.toml"))
        .arg("--config")
        .arg(&config)
        .arg("--offline")
        .output()
        .expect("cargo-depgate should execute the violating check");

    assert_eq!(output.status.code(), Some(1), "a violation must exit 1: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().any(|line| line.starts_with("FAIL rules.app.leaf:")),
        "leaf violation line missing: {stdout}"
    );
    assert!(stdout.lines().any(|line| line == "ok rules.tool.direct"), "{stdout}");
    assert_eq!(stdout.lines().last(), Some("FAIL: 2 rules, 1 violations"), "{stdout}");
}

#[test]
fn phase_a_configuration_errors_never_spawn_cargo() {
    for fixture_name in [
        "unknown-key.toml",
        "zero-rules.toml",
        "leaf-and-internal.toml",
        "self-reference.toml",
        "bad-glob.toml",
    ] {
        let temp_dir = tempfile::tempdir().expect("temporary directory should be created");
        let marker = temp_dir.path().join("cargo-invoked");
        let output = depgate()
            .args(["check", "--config"])
            .arg(config_error_fixture_root().join(fixture_name))
            .env("CARGO", fail_cargo_path())
            .env("FAIL_CARGO_MARKER", &marker)
            .output()
            .expect("cargo-depgate should execute the configuration-error check");

        assert_eq!(
            output.status.code(),
            Some(2),
            "phase-A fixture {fixture_name} returned unexpected output: {output:?}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).starts_with("configuration error:"),
            "phase-A fixture {fixture_name} did not report a configuration error: {output:?}"
        );
        assert!(!marker.exists(), "phase-A fixture {fixture_name} spawned cargo");
    }
}

#[test]
fn phase_b_non_member_error_matches_explicit_and_discovered_config() {
    let fixture = basic_fixture_root();
    let explicit = depgate()
        .args(["check", "--manifest-path"])
        .arg(fixture.join("Cargo.toml"))
        .arg("--config")
        .arg(fixture.join("depgate-bad-rule.toml"))
        .arg("--offline")
        .output()
        .expect("cargo-depgate should execute the explicit bad-rule check");
    assert_eq!(
        explicit.status.code(),
        Some(2),
        "explicit bad-rule check did not fail: {explicit:?}"
    );

    let copied = tempfile::tempdir().expect("temporary directory should be created");
    copy_tree(&fixture, copied.path());
    let discovered_config = copied.path().join("depgate.toml");
    fs::remove_file(&discovered_config)
        .expect("copied passing configuration should be present before replacement");
    fs::rename(copied.path().join("depgate-bad-rule.toml"), &discovered_config)
        .expect("copied bad-rule configuration should become the discovered configuration");

    let discovered = depgate()
        .args(["check", "--manifest-path"])
        .arg(copied.path().join("Cargo.toml"))
        .arg("--offline")
        .output()
        .expect("cargo-depgate should execute the discovered bad-rule check");
    assert_eq!(
        discovered.status.code(),
        Some(2),
        "discovered bad-rule check did not fail: {discovered:?}"
    );

    // Error::Configuration displays ConfigError.message only; its source Span path is not rendered,
    // so the two locations produce byte-identical stderr without path normalization. Cargo may
    // prepend lock-contention diagnostics when the full suite runs concurrently.
    assert_eq!(
        strip_cargo_status_lines(&explicit.stderr),
        strip_cargo_status_lines(&discovered.stderr)
    );
}

#[test]
fn explicit_config_wins_over_discovered_config() {
    let copied = tempfile::tempdir().expect("temporary directory should be created");
    copy_tree(&basic_fixture_root(), copied.path());
    let discovered_config = copied.path().join("depgate.toml");
    let explicit_config = copied.path().join("depgate-good.toml");
    fs::rename(&discovered_config, &explicit_config)
        .expect("copied passing configuration should be preserved for explicit use");
    fs::write(&discovered_config, "schema = 2\n")
        .expect("failing discovered configuration should be written");

    let output = depgate()
        .args(["check", "--manifest-path"])
        .arg(copied.path().join("Cargo.toml"))
        .arg("--config")
        .arg(&explicit_config)
        .arg("--offline")
        .output()
        .expect("cargo-depgate should execute the explicit configuration check");

    assert_eq!(
        output.status.code(),
        Some(0),
        "explicit configuration did not override discovered configuration: {output:?}"
    );
}

#[test]
fn schema_outputs_valid_json() {
    let output = output(&["schema"]);

    assert!(output.status.success(), "schema command failed: {output:?}");
    let schema: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("schema output should be valid JSON");
    assert!(
        schema.get("$defs").is_some() || schema.get("properties").is_some(),
        "schema output has no top-level definitions or properties: {schema}"
    );
}

#[test]
fn manifest_fixture_reports_each_version_with_its_position_and_exits_one() {
    let output = fixture_check(&repository_root().join("tests/fixtures/ws-manifest"));

    assert_eq!(output.status.code(), Some(1), "manifest violations must exit 1: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let manifest_lines: Vec<&str> =
        stdout.lines().filter(|line| line.starts_with("FAIL manifest.versions-in-root:")).collect();
    assert_eq!(
        manifest_lines,
        vec![
            "FAIL manifest.versions-in-root: crates/app/Cargo.toml:7:36 dependencies foo = \"0.1.0\"",
            "FAIL manifest.versions-in-root: crates/app/Cargo.toml:12:36 dev-dependencies bar = \"0.1.0\"",
            "FAIL manifest.versions-in-root: crates/app/Cargo.toml:19:36 target.'cfg(unix)'.dependencies baz = \"0.1.0\"",
        ],
        "{stdout}"
    );
    assert!(stdout.lines().any(|line| line == "ok rules.app.deny"), "{stdout}");
    assert_eq!(stdout.lines().last(), Some("FAIL: 2 rules, 1 violations"), "{stdout}");
}

#[test]
fn root_package_fixture_flags_the_root_dependencies_but_never_the_workspace_table() {
    let output = fixture_check(&repository_root().join("tests/fixtures/ws-rootpkg"));

    assert_eq!(output.status.code(), Some(1), "the root dependency must exit 1: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let manifest_lines: Vec<&str> =
        stdout.lines().filter(|line| line.starts_with("FAIL manifest.versions-in-root:")).collect();
    assert_eq!(
        manifest_lines,
        vec!["FAIL manifest.versions-in-root: Cargo.toml:15:36 dependencies y = \"0.1.0\""],
        "{stdout}"
    );
    assert!(!stdout.contains(" x = "), "the workspace table must never be flagged: {stdout}");
    assert!(stdout.lines().any(|line| line == "ok rules.y.leaf"), "{stdout}");
    assert_eq!(stdout.lines().last(), Some("FAIL: 2 rules, 1 violations"), "{stdout}");
}
