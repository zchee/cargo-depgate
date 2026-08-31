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

/// One committed example fixture: a real upstream workspace pinned to one commit, with
/// the upstream dependency policy distilled into `<name>.depgate.toml`.
///
/// `directory` is both the home of `metadata.json.gz` and the `--workspace-root` the
/// gate is given: the manifest rule re-reads member manifests relative to that root, so
/// it must be the committed directory, not the neutral `/fixture/<name>` prefix the
/// document's own paths carry.
struct Example {
    directory: &'static str,
    config: &'static str,
}

const LEMMY: Example = Example {
    directory: "tests/fixtures/lemmy-439734d",
    config: "tests/fixtures/lemmy.depgate.toml",
};
const CKB: Example =
    Example { directory: "tests/fixtures/ckb-17d7db5", config: "tests/fixtures/ckb.depgate.toml" };
const COREUTILS: Example = Example {
    directory: "tests/fixtures/coreutils-6341084",
    config: "tests/fixtures/coreutils.depgate.toml",
};

impl Example {
    fn fixture_root(&self) -> PathBuf {
        repository_root().join(self.directory)
    }

    fn config_path(&self) -> PathBuf {
        repository_root().join(self.config)
    }
}

fn require_fixture_root() -> PathBuf {
    repository_root().join("tests/fixtures/ws-require")
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

/// Decompresses one committed example's metadata into a temporary file.
fn example_metadata_json(example: &Example) -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let metadata_path = temp.path().join("metadata.json");
    let compressed = fs::File::open(example.fixture_root().join("metadata.json.gz"))
        .expect("example metadata fixture should be readable");
    let mut decoder = GzDecoder::new(compressed);
    let mut metadata = fs::File::create(&metadata_path).expect("metadata JSON should be writable");
    std::io::copy(&mut decoder, &mut metadata).expect("example metadata should decompress");
    (temp, metadata_path)
}

/// Runs `check` on one committed example, offline, with the given extra options.
fn example_check(example: &Example, options: &[&str]) -> (tempfile::TempDir, Output) {
    let (temp, metadata) = example_metadata_json(example);
    // `--format` defaults to `github` when GITHUB_ACTIONS is set, so the runner's own
    // environment must not decide which renderer these assertions see.
    let output = depgate()
        .env_remove("GITHUB_ACTIONS")
        .args(["check", "--metadata-json"])
        .arg(&metadata)
        .args(["--workspace-root"])
        .arg(example.fixture_root())
        .args(["--config"])
        .arg(example.config_path())
        .args(options)
        .output()
        .expect("cargo-depgate should execute the example check");
    (temp, output)
}

/// Runs `explain` on one committed example, offline, on the same path `check` takes.
fn example_explain(
    example: &Example,
    package: &str,
    dependency: &str,
) -> (tempfile::TempDir, Output) {
    let (temp, metadata) = example_metadata_json(example);
    let output = depgate()
        .env_remove("GITHUB_ACTIONS")
        .args(["explain", package, dependency, "--metadata-json"])
        .arg(&metadata)
        .args(["--workspace-root"])
        .arg(example.fixture_root())
        .args(["--config"])
        .arg(example.config_path())
        .arg("--offline")
        .output()
        .expect("cargo-depgate should execute the example explain");
    (temp, output)
}

/// The counters every example asserts, in the order the fixture report records them.
struct ExpectedCounters {
    packages: u64,
    members: u64,
    normal_edges: u64,
    names: u64,
    superset_extra_edges: u64,
    rules: u64,
    violations: u64,
}

fn assert_counters(report: &serde_json::Value, expected: &ExpectedCounters) {
    let counters = &report["counters"];
    assert_eq!(counters["packages"].as_u64(), Some(expected.packages));
    assert_eq!(counters["members"].as_u64(), Some(expected.members));
    assert_eq!(counters["normal_edges"].as_u64(), Some(expected.normal_edges));
    assert_eq!(counters["names"].as_u64(), Some(expected.names));
    assert_eq!(counters["superset_extra_edges"].as_u64(), Some(expected.superset_extra_edges));
    assert_eq!(counters["rules"].as_u64(), Some(expected.rules));
    assert_eq!(counters["violations"].as_u64(), Some(expected.violations));
    assert_eq!(counters["unrebased_path_deps"].as_u64(), Some(0));
}

/// Snapshots one example's counters block, timings removed.
///
/// The snapshot name is passed explicitly: insta derives it from the enclosing
/// function, which is this shared helper, so all three examples would otherwise
/// collide on one file.
fn assert_counters_snapshot(name: &str, report: serde_json::Value) {
    let report = without_timings(report);
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        let counters = serde_json::to_string_pretty(&report["counters"])
            .expect("example counters should serialize");
        insta::assert_snapshot!(name, counters);
    });
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
fn ws_require_human_report_snapshot() {
    let output = fixture_check_with_options(&require_fixture_root(), &["--format", "human"], false);

    assert_eq!(output.status.code(), Some(1), "require human check failed: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stdout(&output));
    });
}

#[test]
fn ws_require_json_report_snapshot() {
    let output = fixture_check_with_options(&require_fixture_root(), &["--format", "json"], false);

    assert_eq!(output.status.code(), Some(1), "require JSON check failed: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stdout(&output));
    });
}

#[test]
fn ws_require_github_report_snapshot() {
    let output = fixture_check_with_options(&require_fixture_root(), &[], true);

    assert_eq!(output.status.code(), Some(1), "require GitHub check failed: {output:?}");
    insta::with_settings!({
        filters => vec![SNAPSHOT_ROOT_FILTER, SNAPSHOT_TIMINGS_FILTER]
    }, {
        insta::assert_snapshot!(cleaned_stdout(&output));
    });
}

#[test]
fn require_reports_only_the_patterns_that_matched_nothing() {
    // The pass/fail split of the fixture policy: `rules.app.require` is satisfied by an
    // exact name and a glob, while `rules.core.require` matches `ui` and misses the other
    // two — the partial miss is what proves matched patterns are never listed.
    let output = fixture_check_with_options(&require_fixture_root(), &["--format", "json"], false);

    assert_eq!(output.status.code(), Some(1), "require check failed: {output:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("require report should be JSON");
    let violations = report["violations"].as_array().expect("report should contain violations");
    assert_eq!(violations.len(), 1, "the satisfied rule contributes no violation: {violations:?}");
    let violation = &violations[0];

    assert_eq!(violation["rule_id"], "rules.core.require");
    assert_eq!(violation["kind"], "require");
    assert_eq!(violation["package"], "core");
    assert_eq!(violation["missing"], serde_json::json!(["app", "no-such-*"]));
    assert_eq!(violation["matches"], serde_json::json!([]), "a matched pattern carries no witness");
    assert_eq!(violation["extra"], serde_json::json!([]));
    assert_eq!(
        report["counters"]["matches"].as_u64(),
        Some(0),
        "the counter sums names the rules found, and a require miss is a name not found"
    );
}

#[test]
fn require_is_satisfied_by_a_name_present_only_through_an_optional_edge() {
    // `require` reads exactly the closure `deny` reads, so an optional dependency that the
    // selected features activate satisfies it: the same unified closure, the same answer.
    let fixture = repository_root().join("tests/fixtures/ws-optfeature");
    let config_dir = tempfile::tempdir().expect("temporary require config should be creatable");
    let config = config_dir.path().join("depgate.toml");
    fs::write(
        &config,
        "schema = 1\n\n[manifest]\nversions-in-root = false\n\n[rules.app]\nrequire = [\"reqwest-like\"]\n",
    )
    .expect("require config should be writable");

    let enabled = check_with_manifest_and_config(
        Some(&fixture.join("Cargo.toml")),
        &config,
        &["--features", "app/net"],
        false,
    );
    assert_eq!(
        enabled.status.code(),
        Some(0),
        "an activated optional edge satisfies require: {enabled:?}"
    );

    let disabled =
        check_with_manifest_and_config(Some(&fixture.join("Cargo.toml")), &config, &[], false);
    assert_eq!(
        disabled.status.code(),
        Some(1),
        "with the feature off the name is absent from the closure: {disabled:?}"
    );
    let stdout = cleaned_stdout(&disabled);
    assert!(stdout.contains("  -reqwest-like"), "the unmatched pattern is listed: {stdout}");
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
fn lemmy_metadata_check_passes_with_pinned_graph_counters() {
    let (_temp, output) = example_check(&LEMMY, &["--format", "json", "--offline"]);

    assert_eq!(output.status.code(), Some(0), "lemmy metadata check failed: {output:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("lemmy report should be valid JSON");
    assert_counters(
        &report,
        &ExpectedCounters {
            packages: 833,
            members: 41,
            normal_edges: 2_950,
            names: 704,
            superset_extra_edges: 400,
            rules: 3,
            violations: 0,
        },
    );
    assert_eq!(
        report["violations"].as_array().map(Vec::len),
        Some(0),
        "the distilled lemmy policy holds at 439734d: {report}"
    );
}

#[test]
fn lemmy_metadata_check_counters_snapshot() {
    let (_temp, output) = example_check(&LEMMY, &["--format", "json", "--offline"]);

    assert_eq!(output.status.code(), Some(0), "lemmy metadata snapshot failed: {output:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("lemmy report should be valid JSON");
    assert_counters_snapshot("lemmy_metadata_check_counters_snapshot", report);
}

#[test]
fn ckb_metadata_check_reports_every_uninherited_version() {
    let (_temp, output) = example_check(&CKB, &["--format", "json", "--offline"]);

    assert_eq!(output.status.code(), Some(1), "ckb metadata check should violate: {output:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("ckb report should be valid JSON");
    // One failed rule, but 24 separate records: the manifest rule reports one finding per
    // dependency entry that names a version, so the counter and the array differ on purpose.
    assert_counters(
        &report,
        &ExpectedCounters {
            packages: 714,
            members: 75,
            normal_edges: 2_351,
            names: 641,
            superset_extra_edges: 0,
            rules: 1,
            violations: 1,
        },
    );
    let records = report["violations"].as_array().expect("violations should be an array");
    assert_eq!(records.len(), 24, "ckb records drifted at 17d7db5: {report}");
    assert!(
        records.iter().all(|record| record["rule_id"] == "manifest.versions-in-root"
            && record["kind"] == "manifest"),
        "every ckb record belongs to the manifest rule: {report}"
    );

    // `phf` is the plainest of the 24: an ordinary `[dependencies]` entry in a member,
    // not one of the 22 target-gated tables, so it pins the shape of a record end to end.
    let phf = records
        .iter()
        .find(|record| record["dependency"] == "phf")
        .unwrap_or_else(|| panic!("ckb report is missing the phf record: {report}"));
    assert_eq!(phf["package"], "ckb-resource");
    assert_eq!(phf["table"], "dependencies");
    assert_eq!(phf["version"], "= 0.8.0");
    assert_eq!(phf["span"]["file"], "resource/Cargo.toml");
    // The column is pinned as well as the line because `docs/examples.md` renders this record as
    // `--> resource/Cargo.toml:14:7` with a caret run under `"= 0.8.0"`; a column regression would
    // otherwise stale the document silently.
    assert_eq!(phf["span"]["line"].as_u64(), Some(14));
    assert_eq!(phf["span"]["col"].as_u64(), Some(7));
}

#[test]
fn ckb_metadata_check_counters_snapshot() {
    let (_temp, output) = example_check(&CKB, &["--format", "json", "--offline"]);

    assert_eq!(output.status.code(), Some(1), "ckb metadata snapshot should violate: {output:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("ckb report should be valid JSON");
    assert_counters_snapshot("ckb_metadata_check_counters_snapshot", report);
}

#[test]
fn coreutils_metadata_check_passes_on_the_feature_selection_its_ci_documents() {
    let (_temp, output) = example_check(&COREUTILS, &["--format", "json", "--offline"]);

    assert_eq!(output.status.code(), Some(0), "coreutils metadata check failed: {output:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("coreutils report should be valid JSON");
    assert_counters(
        &report,
        &ExpectedCounters {
            packages: 512,
            members: 114,
            normal_edges: 1_493,
            names: 482,
            superset_extra_edges: 358,
            rules: 1,
            violations: 0,
        },
    );
    assert_eq!(
        report["violations"].as_array().map(Vec::len),
        Some(0),
        "`features = [\"feat_os_unix\"]` asks the question CICD.yml asks: {report}"
    );
}

#[test]
fn coreutils_human_report_names_the_closure_that_compiled_ariadne_out() {
    let (_temp, output) = example_check(&COREUTILS, &["--offline"]);

    assert_eq!(output.status.code(), Some(0), "coreutils human check should pass: {output:?}");
    // `docs/examples.md` quotes this line verbatim: the pruned count is what distinguishes
    // "the selection compiled it out" from "the name was never in the graph".
    assert_eq!(
        cleaned_stdout(&output),
        "ok rules.coreutils.deny (features = [\"feat_os_unix\"], 43 pruned)\n\
         ok: 1 rules, 0 violations\n"
    );
}

#[test]
fn coreutils_without_the_feature_key_still_reports_the_optional_ariadne_edge() {
    // The other arm of the same fixture, and the reason the key exists: on the unified
    // closure the edge is there, because `uu_csplit` and `uu_numfmt` request
    // `uucore/diagnostics` from their dev-dependencies. Dropping one line from the policy
    // must bring the finding back, witness and optional annotation included.
    let (_temp, metadata) = example_metadata_json(&COREUTILS);
    let config_dir = tempfile::tempdir().expect("temporary unified config should be creatable");
    let config = config_dir.path().join("depgate.toml");
    fs::write(
        &config,
        "schema = 1\n\n[manifest]\nversions-in-root = false\n\n[rules.coreutils]\n\
         deny = [\"ariadne\"]\n",
    )
    .expect("unified config should be writable");

    let output =
        metadata_check(&metadata, &config, Some(&COREUTILS.fixture_root()), &["--format", "json"]);

    assert_eq!(output.status.code(), Some(1), "the unified closure carries it: {output:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("coreutils report should be valid JSON");
    let records = report["violations"].as_array().expect("violations should be an array");
    assert_eq!(records.len(), 1, "coreutils records drifted at 6341084: {report}");
    let ariadne = &records[0];
    assert_eq!(ariadne["rule_id"], "rules.coreutils.deny");
    assert_eq!(ariadne["kind"], "deny");
    assert!(ariadne.get("features").is_none(), "a unified rule adds no features key");
    let matched = ariadne["matches"].as_array().expect("matches should be an array");
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0]["name"], "ariadne");
    let witness = matched[0]["witness"].as_array().expect("witness should be an array");
    assert_eq!(witness.len(), 2);
    assert_eq!(witness[0]["name"], "uucore");
    assert_eq!(witness[0]["optional"], serde_json::Value::Bool(false));
    assert_eq!(witness[1]["name"], "ariadne");
    assert_eq!(witness[1]["optional"], serde_json::Value::Bool(true));

    let human =
        metadata_check(&metadata, &config, Some(&COREUTILS.fixture_root()), &["--format", "human"]);
    let rendered = cleaned_stdout(&human);
    let expected = "coreutils v0.10.0 \u{2192} uucore v0.10.0 \u{2192} ariadne v0.6.0 \
         (optional; present via workspace feature unification)";
    assert!(rendered.contains(expected), "optional witness {expected:?} missing from: {rendered}");
}

#[test]
fn coreutils_metadata_check_counters_snapshot() {
    let (_temp, output) = example_check(&COREUTILS, &["--format", "json", "--offline"]);

    assert_eq!(output.status.code(), Some(0), "coreutils metadata snapshot failed: {output:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("coreutils report should be valid JSON");
    assert_counters_snapshot("coreutils_metadata_check_counters_snapshot", report);
}

#[test]
fn lemmy_human_report_lists_every_rule_and_the_closure_each_answered_on() {
    let (_temp, output) = example_check(&LEMMY, &["--offline"]);

    assert_eq!(output.status.code(), Some(0), "lemmy human check should pass: {output:?}");
    // `docs/examples.md` quotes these lines verbatim. Two points live in them: a rule that
    // found nothing is still listed, because a green gate that quietly checked nothing is a
    // failure mode rather than a pass; and a rule that passed by narrowing says which closure
    // answered it and how much of the unified one that removed, so the pass cannot be mistaken
    // for the workspace-wide claim it is not.
    assert_eq!(
        cleaned_stdout(&output),
        "ok rules.lemmy_server.deny (features = \"default\", 115 pruned)\n\
         ok rules.lemmy_api_common.deny (features = \"none\", 404 pruned)\n\
         ok rules.lemmy_api_utils.require (features = \"all\", 31 pruned)\n\
         ok: 3 rules, 0 violations\n"
    );
}

#[test]
fn lemmy_json_report_records_every_rule_and_the_names_its_selection_removed() {
    let (_temp, output) = example_check(&LEMMY, &["--format", "json", "--offline"]);

    assert_eq!(output.status.code(), Some(0), "lemmy json check should pass: {output:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("lemmy report should be valid JSON");
    assert_eq!(
        report["violations"].as_array().map(Vec::len),
        Some(0),
        "all three rules pass, which is exactly why `violations[]` cannot carry their evidence"
    );

    let rules = report["rules"].as_array().expect("a feature-aware policy writes rules[]");
    assert_eq!(rules.len(), 3, "one record per configured rule, in evaluation order: {report}");
    for rule in rules {
        assert_eq!(rule["passed"], serde_json::Value::Bool(true));
    }

    // The counts are the ones the human report prints and `docs/examples.md` quotes; the names
    // are what the human line cannot carry and what this array exists to publish. Each named
    // check is the upstream assertion the rule translates: `extism` is in `lemmy_server`'s
    // unified closure and the default selection is what removes it (L202/L203), and `diesel`
    // is in `lemmy_api_common`'s and `--no-default-features` is what removes it (L201).
    let pruned = |rule: &serde_json::Value| -> Vec<String> {
        rule["activation_pruned"]
            .as_array()
            .expect("a feature-aware record lists the names it pruned")
            .iter()
            .map(|name| name.as_str().expect("a pruned name is a string").to_owned())
            .collect()
    };

    assert_eq!(rules[0]["id"], "rules.lemmy_server.deny");
    assert_eq!(rules[0]["kind"], "deny");
    assert_eq!(rules[0]["features"], serde_json::json!("default"));
    let server = pruned(&rules[0]);
    assert_eq!(server.len(), 115);
    assert!(server.contains(&"extism".to_owned()), "L203's name is what the selection removed");

    assert_eq!(rules[1]["id"], "rules.lemmy_api_common.deny");
    assert_eq!(rules[1]["kind"], "deny");
    assert_eq!(rules[1]["features"], serde_json::json!("none"));
    let api_common = pruned(&rules[1]);
    assert_eq!(api_common.len(), 404);
    assert!(api_common.contains(&"diesel".to_owned()), "L201's name is what the selection removed");

    assert_eq!(rules[2]["id"], "rules.lemmy_api_utils.require");
    assert_eq!(rules[2]["kind"], "require");
    assert_eq!(rules[2]["features"], serde_json::json!("all"));
    let api_utils = pruned(&rules[2]);
    assert_eq!(api_utils.len(), 31);
    assert!(
        !api_utils.contains(&"extism".to_owned()),
        "L204 is the positive assertion: `extism` has to survive this selection, not be pruned"
    );
}

#[test]
fn lemmy_explain_resolves_the_member_and_not_the_crates_io_copy() {
    let (_temp, output) = example_explain(&LEMMY, "lemmy_utils", "diesel");

    assert_eq!(output.status.code(), Some(0), "lemmy explain failed: {output:?}");
    // `lemmy_utils` resolves at two versions here -- the workspace member at 1.0.0-beta.1 and
    // the crates.io copy at 0.19.16 pulled in transitively -- and only the member declares
    // `diesel`. `explain` must bind the same node a `[rules.lemmy_utils]` root binds, so this is
    // the assertion that keeps `explain` and `check` from contradicting each other: the identical
    // deny rule reports exactly this witness. `.woodpecker.yml` L201 asks the narrower,
    // feature-resolved form of the same question, which schema 1 cannot ask.
    assert_eq!(
        cleaned_stdout(&output),
        "lemmy_utils v1.0.0-beta.1 \u{2192} diesel v2.3.7 \
         (optional; present via workspace feature unification)\n"
    );
}

#[test]
fn lemmy_explain_reports_a_genuinely_unreachable_pair() {
    let (_temp, output) = example_explain(&LEMMY, "lemmy_utils", "actix-cors");

    assert_eq!(output.status.code(), Some(0), "lemmy explain failed: {output:?}");
    // The inverse of the case above, on the same member: `actix-cors` is in the graph but only
    // `lemmy_server` reaches it, so "not reachable" here is a real negative rather than the
    // artefact of resolving the wrong node.
    assert_eq!(cleaned_stdout(&output), "not reachable\n");
}

#[test]
fn lemmy_explain_refuses_an_ambiguous_non_member_package() {
    let (_temp, output) = example_explain(&LEMMY, "bitflags", "diesel");

    // No workspace member is named `bitflags` and the graph carries it at two versions, so there
    // is no rule-root equivalent to resolve to. Picking one silently would make the verdict depend
    // on `cargo metadata`'s package order, so the query is refused as a configuration error.
    assert_eq!(output.status.code(), Some(2), "ambiguous explain succeeded: {output:?}");
    assert!(output.stdout.is_empty(), "ambiguous explain wrote to stdout: {output:?}");
    assert_eq!(
        cleaned_stderr(&output),
        "error: explain references `bitflags`, which is not a workspace member and resolves at \
         2 versions: 1.3.2, 2.11.0; explain resolves workspace members, so name one instead\n"
    );
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
    let (_temp, output) = example_check(&LEMMY, &["--format", "json"]);

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
    // The feature spec is never resolved: no Cargo runs, so it is not even checked for
    // existence. That is precisely the silent failure the warning exists to break.
    let (_temp, output) = example_check(
        &LEMMY,
        &["--all-features", "--features", "lemmy_server/embed-pictrs", "--no-default-features"],
    );

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

#[test]
fn a_feature_aware_rule_is_refused_on_a_default_features_document() {
    // ckb has no feature-aware rule, so its document is generated with the default selection
    // — which is exactly the premise an activation walk may not assume. Adding one to that
    // document is exit 2 naming the first member that proves it, rather than a narrowed
    // closure that could pass a `deny` rule for want of an edge that was never resolved.
    let (_temp, metadata) = example_metadata_json(&CKB);
    let config_dir = tempfile::tempdir().expect("temporary guard config should be creatable");
    let config = config_dir.path().join("depgate.toml");
    fs::write(
        &config,
        "schema = 1\n\n[manifest]\nversions-in-root = false\n\n[rules.ckb-util]\n\
         features = \"none\"\ndeny = [\"libc\"]\n",
    )
    .expect("guard config should be writable");

    let output = metadata_check(&metadata, &config, Some(&CKB.fixture_root()), &[]);

    assert_eq!(output.status.code(), Some(2), "the guard is a configuration error: {output:?}");
    let stderr = cleaned_stderr(&output);
    assert!(
        stderr.contains(
            "feature-aware rules need a graph resolved with all features; member ckb-util has 1 \
             unactivated feature(s) — re-run with --all-features"
        ),
        "the guard names the member it rejected on and how to fix it: {stderr}"
    );
}

#[test]
fn a_feature_aware_deny_narrows_the_closure_end_to_end() {
    // The whole chain on one workspace: `[graph].features = "all"` satisfies the guard, and
    // `features = "none"` then answers `deny` on the closure a default build compiles.
    let fixture = repository_root().join("tests/fixtures/ws-optfeature");
    let config_dir = tempfile::tempdir().expect("temporary feature config should be creatable");
    let policy = |selection: &str| {
        format!(
            "schema = 1\n\n[graph]\nfeatures = \"all\"\n\n[manifest]\nversions-in-root = false\n\n\
             [rules.app]\nfeatures = {selection}\ndeny = [\"reqwest-like\"]\n"
        )
    };

    let narrowed = config_dir.path().join("narrowed.toml");
    fs::write(&narrowed, policy("\"none\"")).expect("narrowed config should be writable");
    let output = check_with_manifest_and_config(
        Some(&fixture.join("Cargo.toml")),
        &narrowed,
        &["--format", "json"],
        false,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "--no-default-features never enables `net`, so the edge is not compiled: {output:?}"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("narrowed report should be JSON");
    assert_eq!(report["counters"]["violations"].as_u64(), Some(0));
    assert_eq!(
        report["features"],
        serde_json::json!("all"),
        "the document is still the all-features resolve the guard demands"
    );

    let human = check_with_manifest_and_config(
        Some(&fixture.join("Cargo.toml")),
        &narrowed,
        &["--format", "human"],
        false,
    );
    assert_eq!(
        cleaned_stdout(&human).lines().next(),
        Some("ok rules.app.deny (features = \"none\", 1 pruned)"),
        "a rule that passes by narrowing says so: {human:?}"
    );

    let selected = config_dir.path().join("selected.toml");
    fs::write(&selected, policy("[\"net\"]")).expect("selected config should be writable");
    let fired = check_with_manifest_and_config(
        Some(&fixture.join("Cargo.toml")),
        &selected,
        &["--format", "json"],
        false,
    );
    assert_eq!(
        fired.status.code(),
        Some(1),
        "the same rule still fires when its selection activates the edge: {fired:?}"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&fired.stdout).expect("selected report should be JSON");
    let violation = &report["violations"][0];
    assert_eq!(violation["rule_id"], "rules.app.deny");
    assert_eq!(violation["matches"][0]["name"], "reqwest-like");
    assert_eq!(violation["features"], serde_json::json!(["net"]));
    assert_eq!(violation["activation_pruned"], serde_json::json!([]));

    let fired_human = check_with_manifest_and_config(
        Some(&fixture.join("Cargo.toml")),
        &selected,
        &["--format", "human"],
        false,
    );
    let rendered = cleaned_stdout(&fired_human);
    assert!(
        rendered.contains("1 match(es) (features = [\"net\"], 0 pruned)"),
        "a rule that fails on a narrowed closure says which closure it was answered on, exactly \
         as the passing form does: {rendered}"
    );
}

#[test]
fn a_unified_rule_carries_no_feature_fields_in_its_json_record() {
    // AC 1 in the report shape: the keys a feature-aware rule adds stay absent everywhere else.
    let output =
        fixture_check_with_options(&violations_fixture_root(), &["--format", "json"], false);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("violations report should be JSON");

    for violation in report["violations"].as_array().expect("report should carry violations") {
        assert!(violation.get("features").is_none(), "unified rules add no features key");
        assert!(violation.get("activation_pruned").is_none(), "and no pruning key");
    }

    // And the array those keys otherwise live in is not written at all: a policy with no
    // feature-aware rule produces the report it produced before `rules[]` existed. The
    // committed `ws_violations_json_report_snapshot` is the byte-level form of this claim;
    // the assertion here names it so the reason the key is missing is not left to a diff.
    assert!(
        report.get("rules").is_none(),
        "a unified-only policy writes no rules[] array: {report}"
    );
}
