//! Binary-level contract tests for the P0 command-line scaffold.
#![expect(clippy::expect_used, reason = "test bodies assert directly")]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Duration, Instant},
};

use assert_cmd::cargo::cargo_bin_cmd;
use flate2::read::GzDecoder;

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

fn normalize_config_path(stderr: &[u8], path: &Path) -> String {
    let cleaned = strip_cargo_status_lines(stderr);
    String::from_utf8_lossy(&cleaned).replace(&path.display().to_string(), "<config>")
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn basic_fixture_root() -> PathBuf {
    repository_root().join("tests/fixtures/ws-basic")
}

fn violations_fixture_root() -> PathBuf {
    repository_root().join("tests/fixtures/ws-violations")
}

fn ganja_fixture_root() -> PathBuf {
    repository_root().join("tests/fixtures/ganja-code-153bfb1")
}

fn ganja_config_path() -> PathBuf {
    repository_root().join("tests/fixtures/ganja-code.depgate.toml")
}

fn config_error_fixture_root() -> PathBuf {
    repository_root().join("tests/fixtures/ws-config-errors")
}

/// Runs `check` on a path-only fixture with its own `depgate.toml`, offline.
fn fixture_check(fixture: &Path) -> Output {
    fixture_check_with_options(fixture, &[], false)
}

fn fixture_check_with_options(fixture: &Path, options: &[&str], github_actions: bool) -> Output {
    let manifest = fixture.join("Cargo.toml");
    let config = fixture.join("depgate.toml");
    check_with_manifest_and_config(Some(&manifest), &config, options, github_actions)
}

fn fixture_explain(fixture: &Path, package: &str, dependency: &str) -> Output {
    let manifest = fixture.join("Cargo.toml");
    let config = fixture.join("depgate.toml");
    depgate()
        .args(["explain", package, dependency, "--manifest-path"])
        .arg(manifest)
        .arg("--config")
        .arg(config)
        .arg("--offline")
        .output()
        .expect("cargo-depgate should execute the fixture explain")
}

fn check_with_manifest_and_config(
    manifest: Option<&Path>,
    config: &Path,
    options: &[&str],
    github_actions: bool,
) -> Output {
    let mut command = depgate();
    command.env_remove("GITHUB_ACTIONS").args(["check"]);
    if let Some(manifest) = manifest {
        command.args(["--manifest-path"]).arg(manifest);
    }
    command.arg("--config").arg(config).args(options);
    if manifest.is_some() {
        command.arg("--offline");
    }
    if github_actions {
        command.env("GITHUB_ACTIONS", "true");
    }
    command.output().expect("cargo-depgate should execute the fixture check")
}

fn config_error_check(fixture_name: &str) -> Output {
    let config = config_error_fixture_root().join(format!("{fixture_name}.toml"));
    let phase_a = matches!(
        fixture_name,
        "bad-glob" | "leaf-and-internal" | "self-reference" | "unknown-key" | "zero-rules"
    );
    if phase_a {
        let mut command = depgate();
        command.args(["check", "--config"]).arg(config).env("CARGO", fail_cargo_path());
        command.output().expect("cargo-depgate should execute the configuration check")
    } else {
        let manifest = basic_fixture_root().join("Cargo.toml");
        check_with_manifest_and_config(Some(&manifest), &config, &[], false)
    }
}

fn cleaned_stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn cleaned_stderr(output: &Output) -> String {
    String::from_utf8_lossy(&strip_cargo_status_lines(&output.stderr)).into_owned()
}

const SNAPSHOT_ROOT_FILTER: (&str, &str) = (r"/(?:[^/\s\n:]+/)*cargo-depgate", "<ROOT>");
const SNAPSHOT_TIMINGS_FILTER: (&str, &str) = (
    r#"("(?:read|parse|graph|traversals|evaluate|manifest|report|total)": )-?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?"#,
    r#"$1"<MS>""#,
);

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

/// Captures one fixture's live `cargo metadata` document for mutation/replay tests.
fn live_metadata(fixture: &Path, no_deps: bool) -> serde_json::Value {
    let mut command = Command::new("cargo");
    command.env_remove("RUSTFLAGS").env("CARGO_TERM_COLOR", "never");
    command.args(["metadata", "--format-version", "1"]);
    if no_deps {
        command.arg("--no-deps");
    }
    let output = command
        .args(["--offline", "--manifest-path"])
        .arg(fixture.join("Cargo.toml"))
        .output()
        .expect("cargo metadata should execute");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("cargo metadata should emit JSON")
}

fn write_metadata_json(directory: &Path, metadata: &serde_json::Value) -> PathBuf {
    let path = directory.join("metadata.json");
    let bytes = serde_json::to_vec(metadata).expect("metadata JSON should serialize");
    fs::write(&path, bytes).expect("metadata JSON should be writable");
    path
}

fn metadata_check(
    metadata: &Path,
    config: &Path,
    workspace_root: Option<&Path>,
    options: &[&str],
) -> Output {
    let mut command = depgate();
    command.args(["check", "--metadata-json"]).arg(metadata);
    if let Some(workspace_root) = workspace_root {
        command.args(["--workspace-root"]).arg(workspace_root);
    }
    command.arg("--config").arg(config).args(options);
    command.output().expect("cargo-depgate should execute metadata-backed check")
}

/// Decompresses the committed ganja-code metadata into a temporary file.
fn ganja_metadata_json() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let metadata_path = temp.path().join("metadata.json");
    let compressed = fs::File::open(ganja_fixture_root().join("metadata.json.gz"))
        .expect("ganja metadata fixture should be readable");
    let mut decoder = GzDecoder::new(compressed);
    let mut metadata = fs::File::create(&metadata_path).expect("metadata JSON should be writable");
    std::io::copy(&mut decoder, &mut metadata).expect("ganja metadata should decompress");
    (temp, metadata_path)
}

fn first_dependency_mut(metadata: &mut serde_json::Value) -> &mut serde_json::Value {
    let nodes = metadata["resolve"]["nodes"]
        .as_array_mut()
        .expect("live metadata should contain resolve nodes");
    let node = nodes
        .iter_mut()
        .find(|node| node["deps"].as_array().is_some_and(|deps| !deps.is_empty()))
        .expect("live metadata should contain a dependency");
    node["deps"]
        .as_array_mut()
        .expect("dependency list should be an array")
        .first_mut()
        .expect("dependency list should not be empty")
}

fn without_timings(mut report: serde_json::Value) -> serde_json::Value {
    report.as_object_mut().expect("JSON report should be an object").remove("timings");
    report
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

#[test]
fn direct_and_cargo_plugin_check_invocations_are_identical() {
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

    // The `--timings` stream is the authoritative source for AC-P2: `report` must be a real
    // measurement and `total` must include every phase (the JSON reporter keeps its own clock).
    let value = |label: &str| -> f64 {
        stderr
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{label}\t")))
            .and_then(|ms| ms.parse::<f64>().ok())
            .unwrap_or_else(|| panic!("missing or non-numeric `{label}` line in {stderr}"))
    };
    let report = value("report");
    let total = value("total");
    let phase_sum: f64 = phases.iter().filter(|p| **p != "total").map(|p| value(p)).sum();
    assert!(report > 0.0, "report phase must be measured: {stderr}");
    assert!(total + 1e-9 >= phase_sum, "total {total} < sum of phases {phase_sum}: {stderr}");
}

/// Spawns the binary with a piped stdout whose read end is closed before the child writes
/// anything, so every stdout write hits `EPIPE` deterministically. A closed reader must never
/// turn into exit 4: `check` keeps its policy result and `explain` keeps exit 0.
fn exit_code_with_closed_stdout(arguments: &[String]) -> Option<i32> {
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cargo-depgate"))
        .args(arguments)
        .env_remove("RUSTFLAGS")
        .env("CARGO_TERM_COLOR", "never")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("cargo-depgate should spawn");
    drop(child.stdout.take());
    child.wait().expect("cargo-depgate should exit").code()
}

fn violations_global_args() -> Vec<String> {
    let fixture = repository_root().join("tests/fixtures/ws-violations");
    vec![
        "--manifest-path".to_owned(),
        fixture.join("Cargo.toml").display().to_string(),
        "--config".to_owned(),
        fixture.join("depgate.toml").display().to_string(),
        "--offline".to_owned(),
    ]
}

#[test]
fn a_closed_stdout_keeps_the_policy_exit_code_in_every_format() {
    for format in ["human", "json", "github"] {
        let mut arguments = vec!["check".to_owned()];
        arguments.extend(violations_global_args());
        arguments.extend(["--format".to_owned(), format.to_owned()]);
        assert_eq!(
            exit_code_with_closed_stdout(&arguments),
            Some(1),
            "check --format {format} must keep the policy exit code on a closed reader"
        );
    }
    for format in ["human", "json"] {
        let mut arguments = vec!["explain".to_owned(), "core".to_owned(), "ui".to_owned()];
        arguments.extend(violations_global_args());
        arguments.extend(["--format".to_owned(), format.to_owned()]);
        assert_eq!(
            exit_code_with_closed_stdout(&arguments),
            Some(0),
            "explain --format {format} must keep exit 0 on a closed reader"
        );
    }
}

#[test]
fn json_timings_report_is_positive_and_total_is_self_consistent() {
    let output =
        fixture_check_with_options(&violations_fixture_root(), &["--format", "json"], false);

    assert_eq!(output.status.code(), Some(1), "JSON violation check failed: {output:?}");
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("JSON report should be valid JSON");
    let timings = document["timings"].as_object().expect("JSON report should contain timings");
    let phase = |name: &str| timings[name].as_f64().expect("timing should be numeric");
    let report = phase("report");
    let total = phase("total");
    let sum = ["read", "parse", "graph", "traversals", "evaluate", "manifest"]
        .into_iter()
        .map(phase)
        .sum::<f64>()
        + report;

    assert!(report > 0.0, "report timing should include typed report construction: {report}");
    assert!(total >= sum, "total={total} must cover phase sum={sum}");
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
    assert!(stdout.contains("rules.app.leaf"), "leaf violation id missing: {stdout}");
    assert!(stdout.contains("2 extra, 0 missing"), "leaf violation details missing: {stdout}");
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        let expected_message = match fixture_name {
            "unknown-key.toml" => "unknown field `mystery`",
            "zero-rules.toml" => "depgate.toml declares no rules",
            "leaf-and-internal.toml" => "rules.foo declares both leaf and internal",
            "self-reference.toml" => "rules.foo.internal cannot contain the rule package itself",
            "bad-glob.toml" => "error parsing glob 'a[b': unclosed character class",
            _ => unreachable!("fixture list is exhaustive"),
        };
        let expected_location = match fixture_name {
            "unknown-key.toml" => "unknown-key.toml:2:1",
            "zero-rules.toml" => "zero-rules.toml:1:1",
            "leaf-and-internal.toml" => "leaf-and-internal.toml:4:8",
            "self-reference.toml" => "self-reference.toml:4:13",
            "bad-glob.toml" => "bad-glob.toml:4:9",
            _ => unreachable!("fixture list is exhaustive"),
        };
        assert!(stderr.contains(expected_message), "phase-A message missing: {output:?}");
        assert!(stderr.contains(expected_location), "phase-A source location missing: {output:?}");
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

    // Source-annotated diagnostics include each config's absolute path. The fixture intentionally
    // uses different roots and basenames, so normalize only those known paths before comparing the
    // otherwise identical diagnostics. Cargo may prepend lock-contention diagnostics when the full
    // suite runs concurrently.
    assert_eq!(
        normalize_config_path(&explicit.stderr, &fixture.join("depgate-bad-rule.toml")),
        normalize_config_path(&discovered.stderr, &discovered_config)
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
    assert!(stdout.contains("manifest.versions-in-root"), "manifest rule id missing: {stdout}");
    assert!(stdout.contains("crates/app/Cargo.toml:7:36"), "foo source location missing: {stdout}");
    assert!(stdout.contains("dependencies foo = \"0.1.0\""), "foo entry missing: {stdout}");
    assert!(
        stdout.contains("crates/app/Cargo.toml:12:36"),
        "bar source location missing: {stdout}"
    );
    assert!(stdout.contains("dev-dependencies bar = \"0.1.0\""), "bar entry missing: {stdout}");
    assert!(
        stdout.contains("crates/app/Cargo.toml:19:36"),
        "baz source location missing: {stdout}"
    );
    assert!(
        stdout.contains("target.'cfg(unix)'.dependencies baz = \"0.1.0\""),
        "baz entry missing: {stdout}"
    );
    assert!(stdout.lines().any(|line| line == "ok rules.app.deny"), "{stdout}");
    assert_eq!(stdout.lines().last(), Some("FAIL: 2 rules, 1 violations"), "{stdout}");
}

#[test]
fn root_package_fixture_flags_the_root_dependencies_but_never_the_workspace_table() {
    let output = fixture_check(&repository_root().join("tests/fixtures/ws-rootpkg"));

    assert_eq!(output.status.code(), Some(1), "the root dependency must exit 1: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("manifest.versions-in-root"), "manifest rule id missing: {stdout}");
    assert!(stdout.contains("Cargo.toml:15:36"), "root source location missing: {stdout}");
    assert!(stdout.contains("dependencies y = \"0.1.0\""), "root entry missing: {stdout}");
    assert!(!stdout.contains(" x = "), "the workspace table must never be flagged: {stdout}");
    assert!(stdout.lines().any(|line| line == "ok rules.y.leaf"), "{stdout}");
    assert_eq!(stdout.lines().last(), Some("FAIL: 2 rules, 1 violations"), "{stdout}");
}

#[test]
fn ws_basic_human_report_snapshot() {
    let output = fixture_check_with_options(&basic_fixture_root(), &["--format", "human"], false);

    assert_eq!(output.status.code(), Some(0), "basic human check failed: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stdout(&output));
    });
}

#[test]
fn ws_basic_json_report_snapshot() {
    let output = fixture_check_with_options(&basic_fixture_root(), &["--format", "json"], false);

    assert_eq!(output.status.code(), Some(0), "basic JSON check failed: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stdout(&output));
    });
}

#[test]
fn ws_basic_github_report_snapshot() {
    let output = fixture_check_with_options(&basic_fixture_root(), &[], true);

    assert_eq!(output.status.code(), Some(0), "basic GitHub check failed: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stdout(&output));
    });
}

#[test]
fn ws_violations_human_report_snapshot() {
    let output =
        fixture_check_with_options(&violations_fixture_root(), &["--format", "human"], false);

    assert_eq!(output.status.code(), Some(1), "violation human check failed: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stdout(&output));
    });
}

#[test]
fn ws_violations_json_report_snapshot() {
    let output =
        fixture_check_with_options(&violations_fixture_root(), &["--format", "json"], false);

    assert_eq!(output.status.code(), Some(1), "violation JSON check failed: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stdout(&output));
    });
}

#[test]
fn ws_violations_github_report_snapshot() {
    let output = fixture_check_with_options(&violations_fixture_root(), &[], true);

    assert_eq!(output.status.code(), Some(1), "violation GitHub check failed: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stdout(&output));
    });
}

#[test]
fn config_error_bad_glob_snapshot() {
    let output = config_error_check("bad-glob");

    assert_eq!(output.status.code(), Some(2), "bad-glob check failed: {output:?}");
    assert!(output.stdout.is_empty(), "configuration errors belong on stderr: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stderr(&output));
    });
}

#[test]
fn config_error_leaf_and_internal_snapshot() {
    let output = config_error_check("leaf-and-internal");

    assert_eq!(output.status.code(), Some(2), "leaf-and-internal check failed: {output:?}");
    assert!(output.stdout.is_empty(), "configuration errors belong on stderr: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stderr(&output));
    });
}

#[test]
fn config_error_non_member_snapshot() {
    let output = config_error_check("non-member");

    assert_eq!(output.status.code(), Some(2), "non-member check failed: {output:?}");
    assert!(output.stdout.is_empty(), "configuration errors belong on stderr: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stderr(&output));
    });
}

#[test]
fn config_error_self_reference_snapshot() {
    let output = config_error_check("self-reference");

    assert_eq!(output.status.code(), Some(2), "self-reference check failed: {output:?}");
    assert!(output.stdout.is_empty(), "configuration errors belong on stderr: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stderr(&output));
    });
}

#[test]
fn config_error_unknown_direct_snapshot() {
    let output = config_error_check("unknown-direct");

    assert_eq!(output.status.code(), Some(2), "unknown-direct check failed: {output:?}");
    assert!(output.stdout.is_empty(), "configuration errors belong on stderr: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stderr(&output));
    });
}

#[test]
fn config_error_unknown_key_snapshot() {
    let output = config_error_check("unknown-key");

    assert_eq!(output.status.code(), Some(2), "unknown-key check failed: {output:?}");
    assert!(output.stdout.is_empty(), "configuration errors belong on stderr: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stderr(&output));
    });
}

#[test]
fn config_error_zero_rules_snapshot() {
    let output = config_error_check("zero-rules");

    assert_eq!(output.status.code(), Some(2), "zero-rules check failed: {output:?}");
    assert!(output.stdout.is_empty(), "configuration errors belong on stderr: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stderr(&output));
    });
}

#[test]
fn schema_output_snapshot() {
    let output = output(&["schema"]);

    assert!(output.status.success(), "schema command failed: {output:?}");
    assert!(output.stderr.is_empty(), "schema wrote to stderr: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stdout(&output));
    });
}

#[test]
fn explain_reachable_snapshot() {
    let output = fixture_explain(&violations_fixture_root(), "core", "ui");

    assert_eq!(output.status.code(), Some(0), "reachable explain failed: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stdout(&output));
    });
}

#[test]
fn explain_not_reachable_snapshot() {
    let output = fixture_explain(&violations_fixture_root(), "ui", "core");

    assert_eq!(output.status.code(), Some(0), "not-reachable explain failed: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stdout(&output));
    });
}

#[test]
fn explain_unknown_dependency_reports_a_usage_error() {
    let output = fixture_explain(&violations_fixture_root(), "core", "nonexistent");

    assert_eq!(output.status.code(), Some(2), "unknown explain dependency succeeded: {output:?}");
    assert!(output.stdout.is_empty(), "unknown explain dependency wrote to stdout: {output:?}");
    assert_eq!(
        cleaned_stderr(&output),
        "error: explain references unknown package `nonexistent`\n"
    );
}

#[test]
fn explain_accepts_metadata_json_and_workspace_root() {
    let fixture = basic_fixture_root();
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let metadata = live_metadata(&fixture, false);
    let metadata_path = write_metadata_json(temp.path(), &metadata);

    let output = depgate()
        .args(["explain", "core", "util", "--metadata-json"])
        .arg(&metadata_path)
        .args(["--workspace-root"])
        .arg(&fixture)
        .arg("--config")
        .arg(fixture.join("depgate.toml"))
        .output()
        .expect("cargo-depgate should execute metadata-backed explain");

    assert_eq!(output.status.code(), Some(0), "metadata-backed explain failed: {output:?}");
    assert_eq!(cleaned_stdout(&output), "core v0.1.0 → util v0.1.0\n");
}

#[test]
fn ganja_metadata_check_passes_with_pinned_graph_counters() {
    let (_temp, metadata) = ganja_metadata_json();
    let fixture = ganja_fixture_root();
    let output = depgate()
        .args(["check", "--metadata-json"])
        .arg(&metadata)
        .args(["--workspace-root"])
        .arg(&fixture)
        .args(["--config"])
        .arg(ganja_config_path())
        .args(["--format", "json", "--offline"])
        .output()
        .expect("cargo-depgate should execute the ganja metadata check");

    assert_eq!(output.status.code(), Some(0), "ganja metadata check failed: {output:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("ganja report should be valid JSON");
    let counters = &report["counters"];
    assert_eq!(counters["packages"].as_u64(), Some(585));
    assert_eq!(counters["members"].as_u64(), Some(14));
    assert_eq!(counters["normal_edges"].as_u64(), Some(1_586));
    assert_eq!(counters["names"].as_u64(), Some(529));
    assert_eq!(counters["rules"].as_u64(), Some(19));
    assert_eq!(counters["violations"].as_u64(), Some(0));
    assert_eq!(counters["unrebased_path_deps"].as_u64(), Some(0));
}

#[test]
fn ganja_metadata_check_counters_snapshot() {
    let (_temp, metadata) = ganja_metadata_json();
    let fixture = ganja_fixture_root();
    let output = depgate()
        .args(["check", "--metadata-json"])
        .arg(&metadata)
        .args(["--workspace-root"])
        .arg(&fixture)
        .args(["--config"])
        .arg(ganja_config_path())
        .args(["--format", "json", "--offline"])
        .output()
        .expect("cargo-depgate should execute the ganja metadata snapshot");

    assert_eq!(output.status.code(), Some(0), "ganja metadata snapshot failed: {output:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("ganja report should be valid JSON");
    let report = without_timings(report);
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        let counters = serde_json::to_string_pretty(&report["counters"])
            .expect("ganja counters should serialize");
        insta::assert_snapshot!(counters);
    });
}

#[test]
fn ganja_explain_ratatui_is_not_reachable() {
    let (_temp, metadata) = ganja_metadata_json();
    let fixture = ganja_fixture_root();
    let output = depgate()
        .args(["explain", "ganja-core", "ratatui", "--metadata-json"])
        .arg(&metadata)
        .args(["--workspace-root"])
        .arg(&fixture)
        .args(["--config"])
        .arg(ganja_config_path())
        .arg("--offline")
        .output()
        .expect("cargo-depgate should execute the ganja explain");

    assert_eq!(output.status.code(), Some(0), "ganja explain failed: {output:?}");
    assert_eq!(cleaned_stdout(&output), "not reachable\n");
}

#[test]
fn metadata_json_no_deps_is_rejected() {
    let fixture = basic_fixture_root();
    let metadata = live_metadata(&fixture, true);
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let metadata_path = write_metadata_json(temp.path(), &metadata);
    let output = metadata_check(&metadata_path, &fixture.join("depgate.toml"), None, &[]);

    assert_eq!(output.status.code(), Some(3), "--no-deps metadata must fail closed: {output:?}");
}

#[test]
fn nonexistent_manifest_path_maps_to_metadata_failure() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let manifest = temp.path().join("missing/Cargo.toml");
    let output = depgate()
        .args(["check", "--manifest-path"])
        .arg(&manifest)
        .arg("--offline")
        .output()
        .expect("cargo-depgate should execute the invalid manifest check");

    assert_eq!(output.status.code(), Some(3), "invalid manifest must map to exit 3: {output:?}");
    assert!(!output.stderr.is_empty(), "Cargo's diagnostic should be inherited: {output:?}");
}

#[test]
fn metadata_json_empty_dep_kinds_is_rejected() {
    let fixture = basic_fixture_root();
    let mut metadata = live_metadata(&fixture, false);
    first_dependency_mut(&mut metadata)["dep_kinds"] = serde_json::json!([]);
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let metadata_path = write_metadata_json(temp.path(), &metadata);
    let output = metadata_check(&metadata_path, &fixture.join("depgate.toml"), None, &[]);

    assert_eq!(output.status.code(), Some(3), "empty dep_kinds must fail closed: {output:?}");
}

#[test]
fn metadata_json_unknown_dependency_package_is_rejected() {
    let fixture = basic_fixture_root();
    let mut metadata = live_metadata(&fixture, false);
    first_dependency_mut(&mut metadata)["pkg"] =
        serde_json::Value::String("path+file:///nonexistent#ghost@0.1.0".to_owned());
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let metadata_path = write_metadata_json(temp.path(), &metadata);
    let output = metadata_check(&metadata_path, &fixture.join("depgate.toml"), None, &[]);

    assert_eq!(output.status.code(), Some(3), "unknown edge package must fail closed: {output:?}");
}

#[test]
fn metadata_json_empty_workspace_members_is_rejected() {
    let fixture = basic_fixture_root();
    let mut metadata = live_metadata(&fixture, false);
    metadata["workspace_members"] = serde_json::json!([]);
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let metadata_path = write_metadata_json(temp.path(), &metadata);
    let output = metadata_check(&metadata_path, &fixture.join("depgate.toml"), None, &[]);

    assert_eq!(
        output.status.code(),
        Some(3),
        "empty workspace_members must fail closed: {output:?}"
    );
}

#[test]
fn metadata_json_missing_resolve_node_is_rejected() {
    let fixture = basic_fixture_root();
    let mut metadata = live_metadata(&fixture, false);
    metadata["resolve"]["nodes"]
        .as_array_mut()
        .expect("live metadata should contain resolve nodes")
        .pop()
        .expect("live metadata should contain multiple resolve nodes");
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let metadata_path = write_metadata_json(temp.path(), &metadata);
    let output = metadata_check(&metadata_path, &fixture.join("depgate.toml"), None, &[]);

    assert_eq!(output.status.code(), Some(3), "missing resolve node must fail closed: {output:?}");
}

#[test]
fn metadata_json_unknown_extra_resolve_node_is_rejected() {
    let fixture = basic_fixture_root();
    let mut metadata = live_metadata(&fixture, false);
    metadata["resolve"]["nodes"]
        .as_array_mut()
        .expect("live metadata should contain resolve nodes")
        .push(serde_json::json!({
            "id": "path+file:///nonexistent#ghost@0.1.0",
            "deps": []
        }));
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let metadata_path = write_metadata_json(temp.path(), &metadata);
    let output = metadata_check(&metadata_path, &fixture.join("depgate.toml"), None, &[]);

    assert_eq!(output.status.code(), Some(3), "extra resolve node must fail closed: {output:?}");
}

#[test]
fn metadata_json_duplicate_package_id_is_rejected() {
    let fixture = basic_fixture_root();
    let mut metadata = live_metadata(&fixture, false);
    let packages =
        metadata["packages"].as_array_mut().expect("live metadata should contain packages");
    let duplicate = packages.first().expect("live metadata should contain a package").clone();
    packages.push(duplicate);
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let metadata_path = write_metadata_json(temp.path(), &metadata);
    let output = metadata_check(&metadata_path, &fixture.join("depgate.toml"), None, &[]);

    assert_eq!(output.status.code(), Some(3), "duplicate package id must fail closed: {output:?}");
}

#[test]
fn metadata_json_member_outside_workspace_root_is_rejected() {
    let fixture = basic_fixture_root();
    let mut metadata = live_metadata(&fixture, false);
    let member_id = metadata["workspace_members"]
        .as_array()
        .expect("live metadata should contain workspace members")
        .first()
        .and_then(|member| member.as_str())
        .expect("workspace member id should be a string")
        .to_owned();
    let package = metadata["packages"]
        .as_array_mut()
        .expect("live metadata should contain packages")
        .iter_mut()
        .find(|package| package["id"].as_str() == Some(member_id.as_str()))
        .expect("workspace member should have a package entry");
    package["manifest_path"] =
        serde_json::Value::String("/nonexistent/outside/Cargo.toml".to_owned());
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let metadata_path = write_metadata_json(temp.path(), &metadata);
    let output = metadata_check(&metadata_path, &fixture.join("depgate.toml"), Some(&fixture), &[]);

    assert_eq!(output.status.code(), Some(3), "outside member must fail closed: {output:?}");
}

#[test]
fn cargo_metadata_timeout_exits_three_within_bound() {
    let fixture = basic_fixture_root();
    let started = Instant::now();
    let output = depgate()
        .args(["check", "--manifest-path"])
        .arg(fixture.join("Cargo.toml"))
        .arg("--config")
        .arg(fixture.join("depgate.toml"))
        .args(["--offline", "--cargo-timeout", "1"])
        .env("CARGO", repository_root().join("tests/bin/slow-cargo"))
        .output()
        .expect("cargo-depgate should execute the timeout check");
    let elapsed = started.elapsed();

    assert_eq!(output.status.code(), Some(3), "timed-out metadata must exit 3: {output:?}");
    assert!(
        elapsed < Duration::from_secs(2),
        "metadata timeout exceeded the two-second bound: {elapsed:?}"
    );
    assert!(
        cleaned_stderr(&output).contains("cargo metadata exceeded --cargo-timeout=1s"),
        "timeout diagnostic missing: {output:?}"
    );
}

#[test]
fn metadata_json_file_and_stdin_reports_are_equivalent() {
    let fixture = basic_fixture_root();
    let config = fixture.join("depgate.toml");
    let metadata = live_metadata(&fixture, false);
    let metadata_bytes = serde_json::to_vec(&metadata).expect("metadata JSON should serialize");
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let metadata_path = write_metadata_json(temp.path(), &metadata);
    let options = ["--format", "json"];
    let from_file = metadata_check(&metadata_path, &config, Some(&fixture), &options);

    let from_stdin = depgate()
        .args(["check", "--metadata-json", "-"])
        .args(["--workspace-root"])
        .arg(&fixture)
        .arg("--config")
        .arg(&config)
        .args(options)
        .write_stdin(metadata_bytes)
        .output()
        .expect("cargo-depgate should execute the stdin metadata check");

    assert_eq!(from_file.status.code(), Some(0), "file metadata check failed: {from_file:?}");
    assert_eq!(from_stdin.status.code(), Some(0), "stdin metadata check failed: {from_stdin:?}");
    let file_report: serde_json::Value =
        serde_json::from_slice(&from_file.stdout).expect("file report should be valid JSON");
    let stdin_report: serde_json::Value =
        serde_json::from_slice(&from_stdin.stdout).expect("stdin report should be valid JSON");
    assert_eq!(without_timings(file_report), without_timings(stdin_report));
}

#[test]
fn workspace_root_without_metadata_json_is_a_usage_error() {
    let fixture = basic_fixture_root();
    let output = depgate()
        .args(["check", "--manifest-path"])
        .arg(fixture.join("Cargo.toml"))
        .arg("--config")
        .arg(fixture.join("depgate.toml"))
        .args(["--workspace-root"])
        .arg(&fixture)
        .output()
        .expect("cargo-depgate should execute the usage check");

    assert_eq!(
        output.status.code(),
        Some(2),
        "workspace root without JSON must be rejected: {output:?}"
    );
}

#[test]
fn renamed_dependency_matches_real_package_name_only() {
    let fixture = repository_root().join("tests/fixtures/ws-rename");
    let output = fixture_check(&fixture);

    assert_eq!(output.status.code(), Some(0), "renamed dependency check failed: {output:?}");
    assert!(cleaned_stdout(&output).contains("0 violations"), "unexpected report: {output:?}");
}

#[test]
fn renamed_dependency_deny_uses_real_package_name() {
    let fixture = repository_root().join("tests/fixtures/ws-rename");
    let output = check_with_manifest_and_config(
        Some(&fixture.join("Cargo.toml")),
        &fixture.join("depgate-deny-real-name.toml"),
        &[],
        false,
    );

    assert_eq!(output.status.code(), Some(1), "real-name deny must violate: {output:?}");
    let stdout = cleaned_stdout(&output);
    assert!(stdout.contains("rules.app.deny"), "deny rule id missing: {stdout}");
    assert!(stdout.contains("core"), "real package name missing from report: {stdout}");
}

#[test]
fn multi_version_dependency_witnesses_keep_reachable_versions_distinct() {
    let fixture = repository_root().join("tests/fixtures/ws-multiver");
    let output = fixture_check_with_options(&fixture, &["--format", "json"], false);

    assert_eq!(output.status.code(), Some(1), "multi-version deny must violate: {output:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("multi-version report should be JSON");
    assert_eq!(report["counters"]["violations"].as_u64(), Some(2));
    let violations = report["violations"].as_array().expect("report should contain violations");
    let app1 = violations
        .iter()
        .find(|violation| violation["rule_id"] == "rules.app1.deny")
        .expect("app1 deny violation should be present");
    let app2 = violations
        .iter()
        .find(|violation| violation["rule_id"] == "rules.app2.deny")
        .expect("app2 deny violation should be present");
    assert_eq!(app1["matches"][0]["name"], "foo");
    assert_eq!(app1["matches"][0]["version"], "1.0.0");
    assert_eq!(app2["matches"][0]["name"], "foo");
    assert_eq!(app2["matches"][0]["version"], "2.0.0");
}

#[test]
fn dev_dependency_cycle_is_excluded_from_normal_closure() {
    let fixture = repository_root().join("tests/fixtures/ws-devcycle");
    let output = fixture_check(&fixture);

    assert_eq!(output.status.code(), Some(0), "dev-dependency cycle check failed: {output:?}");
    assert!(cleaned_stdout(&output).contains("0 violations"), "unexpected report: {output:?}");
}

#[test]
fn cfg_only_dependency_is_reported_with_target_annotation() {
    let fixture = repository_root().join("tests/fixtures/ws-cfg");
    let output = fixture_check_with_options(&fixture, &["--format", "json"], false);

    assert_eq!(output.status.code(), Some(1), "cfg-only deny must violate: {output:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cfg report should be JSON");
    assert_eq!(report["counters"]["superset_extra_edges"].as_u64(), Some(1));
    let violation = report["violations"]
        .as_array()
        .expect("report should contain violations")
        .iter()
        .find(|violation| violation["rule_id"] == "rules.app.deny")
        .expect("app deny violation should be present");
    assert_eq!(violation["package"], "app");
    assert_eq!(violation["matches"][0]["name"], "winonly");
    assert_eq!(violation["matches"][0]["version"], "0.1.0");
    assert_eq!(violation["matches"][0]["witness"][0]["name"], "winonly");
    assert_eq!(violation["matches"][0]["witness"][0]["version"], "0.1.0");
    assert_eq!(violation["matches"][0]["witness"][0]["target"], "cfg(windows)");
}

#[test]
fn optional_dependency_is_absent_with_default_features() {
    let fixture = repository_root().join("tests/fixtures/ws-optfeature");
    let output = fixture_check(&fixture);

    assert_eq!(output.status.code(), Some(0), "default feature check failed: {output:?}");
    assert!(cleaned_stdout(&output).contains("0 violations"), "unexpected report: {output:?}");
}

#[test]
fn optional_dependency_is_forwarded_by_cli_feature_flag() {
    let fixture = repository_root().join("tests/fixtures/ws-optfeature");
    let output = check_with_manifest_and_config(
        Some(&fixture.join("Cargo.toml")),
        &fixture.join("depgate.toml"),
        &["--features", "app/net", "--format", "json"],
        false,
    );

    assert_eq!(
        output.status.code(),
        Some(1),
        "enabled optional dependency must violate: {output:?}"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("optional-feature report should be JSON");
    let violation = report["violations"]
        .as_array()
        .expect("report should contain violations")
        .iter()
        .find(|violation| violation["rule_id"] == "rules.app.deny")
        .expect("app deny violation should be present");
    assert_eq!(violation["package"], "app");
    assert_eq!(violation["matches"][0]["name"], "reqwest-like");
    assert_eq!(
        report["features"],
        serde_json::json!(["app/net"]),
        "the report records the selection that shaped the graph, not the file's default"
    );
}

#[test]
fn direct_rule_on_an_optional_declaration_warns_and_counts() {
    // AC 5 end to end: `app` declares `reqwest-like` as an optional normal dependency, so a
    // `direct` rule on it emits the §1.3 warning and bumps `direct_optional_decls`, even though
    // the rule itself passes once the feature that pulls the dependency in is enabled.
    let fixture = repository_root().join("tests/fixtures/ws-optfeature");
    let config_dir = tempfile::tempdir().expect("temporary direct-rule config should be creatable");
    let config = config_dir.path().join("depgate.toml");
    fs::write(
        &config,
        "schema = 1\n\n[manifest]\nversions-in-root = false\n\n[rules.app]\ndirect = [\"reqwest-like\"]\n",
    )
    .expect("direct-rule config should be writable");

    let output = check_with_manifest_and_config(
        Some(&fixture.join("Cargo.toml")),
        &config,
        &["--features", "app/net", "--format", "json"],
        false,
    );

    assert_eq!(output.status.code(), Some(0), "direct rule should pass: {output:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("direct-rule report should be JSON");
    assert_eq!(report["counters"]["direct_optional_decls"].as_u64(), Some(1));
    assert_eq!(
        cleaned_stderr(&output).trim(),
        "warning: rules.app.direct: app declares optional dependency reqwest-like; \
         sibling feature unification may add it to the resolved edge set"
    );
}

#[test]
fn optional_dependency_is_forwarded_by_config_graph_feature_selection() {
    let fixture = repository_root().join("tests/fixtures/ws-optfeature");
    let copied = tempfile::tempdir().expect("temporary feature workspace should be creatable");
    copy_tree(&fixture, copied.path());
    let config = copied.path().join("depgate.toml");
    let text = fs::read_to_string(&config).expect("feature fixture config should be readable");
    fs::write(
        &config,
        format!(
            "schema = 1\n\n[graph]\nfeatures = \"all\"\n\n{}",
            text.lines().skip(1).collect::<Vec<_>>().join("\n")
        ),
    )
    .expect("feature selection config should be writable");

    let output = check_with_manifest_and_config(
        Some(&copied.path().join("Cargo.toml")),
        &config,
        &["--format", "json"],
        false,
    );

    assert_eq!(
        output.status.code(),
        Some(1),
        "config-selected optional dependency must violate: {output:?}"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("config-feature report should be JSON");
    assert_eq!(report["features"], "all");
    assert_eq!(report["violations"][0]["matches"][0]["name"], "reqwest-like");
}

#[test]
fn a_discovered_config_at_the_workspace_root_gates_the_happy_path() {
    // The primary way the tool is run: a `depgate.toml` beside the workspace `Cargo.toml`
    // and no `--config`. Every other discovery test drives an error path, so a regression
    // that broke discovery on the passing path would have gone unnoticed.
    let fixture = basic_fixture_root();
    let output = depgate()
        .args(["check", "--manifest-path"])
        .arg(fixture.join("Cargo.toml"))
        .arg("--offline")
        .output()
        .expect("cargo-depgate should execute the discovered-config check");

    assert_eq!(output.status.code(), Some(0), "discovered-config check failed: {output:?}");
    let stdout = cleaned_stdout(&output);
    assert!(stdout.contains("ok: 8 rules, 0 violations"), "unexpected report: {stdout}");
    // The rule ids prove the report came from the discovered file, not from a default policy.
    for rule in ["rules.app.deny", "rules.util.leaf", "rules.tool.sealed"] {
        assert!(stdout.contains(&format!("ok {rule}")), "{rule} missing from report: {stdout}");
    }
}

#[test]
fn json_features_reports_the_all_features_override() {
    // The config selects nothing, so a `features` field taken from the file would say
    // "default" while `--all-features` is what actually shaped the graph.
    let fixture = repository_root().join("tests/fixtures/ws-optfeature");
    let output = check_with_manifest_and_config(
        Some(&fixture.join("Cargo.toml")),
        &fixture.join("depgate.toml"),
        &["--all-features", "--format", "json"],
        false,
    );

    assert_eq!(output.status.code(), Some(1), "--all-features must reach the graph: {output:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("all-features report should be JSON");
    assert_eq!(report["features"], "all");
    assert_eq!(report["violations"][0]["matches"][0]["name"], "reqwest-like");
}

#[test]
fn json_features_is_null_when_no_cargo_ran() {
    let (_temp, metadata) = ganja_metadata_json();
    let output = depgate()
        .args(["check", "--metadata-json"])
        .arg(&metadata)
        .args(["--workspace-root"])
        .arg(ganja_fixture_root())
        .args(["--config"])
        .arg(ganja_config_path())
        .args(["--format", "json"])
        .output()
        .expect("cargo-depgate should execute the metadata-backed check");

    assert_eq!(output.status.code(), Some(0), "metadata-backed check failed: {output:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("metadata report should be JSON");
    assert_eq!(
        report["features"],
        serde_json::Value::Null,
        "the document carries its own selection, which this process cannot observe"
    );
    assert!(
        report.get("features").is_some(),
        "the key stays present so a consumer can tell null from absent"
    );
}

#[test]
fn feature_flags_warn_when_metadata_json_makes_them_inert() {
    let (_temp, metadata) = ganja_metadata_json();
    let output = depgate()
        .args(["check", "--metadata-json"])
        .arg(&metadata)
        .args(["--workspace-root"])
        .arg(ganja_fixture_root())
        .args(["--config"])
        .arg(ganja_config_path())
        // Never resolved: no Cargo runs, so the spec is not even checked for existence.
        // That is precisely the silent failure the warning exists to break.
        .args(["--all-features", "--features", "ganja-cli/tui", "--no-default-features"])
        .output()
        .expect("cargo-depgate should execute the metadata-backed check");

    assert_eq!(
        output.status.code(),
        Some(0),
        "inert flags must not change the verdict: {output:?}"
    );
    assert_eq!(
        cleaned_stderr(&output).trim(),
        "warning: --features, --all-features, --no-default-features ignored under \
         --metadata-json; the JSON was produced with its own feature selection"
    );
}
