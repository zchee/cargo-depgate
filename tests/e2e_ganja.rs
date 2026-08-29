//! Ignored end-to-end parity checks against a live `ganja-code` checkout.
#![expect(clippy::expect_used, reason = "test bodies assert directly")]
#![expect(clippy::ignore_without_reason, reason = "live e2e tests are ignored by default")]

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use assert_cmd::cargo::cargo_bin_cmd;
use cargo_depgate::{
    graph::{Graph, Scratch},
    metadata::{self, MetadataOptions},
};

const EXPECTED_PACKAGES: u64 = 585;
const EXPECTED_MEMBERS: u64 = 14;
const EXPECTED_NORMAL_EDGES: u64 = 1_586;
const EXPECTED_NAMES: u64 = 529;
const EXPECTED_SUPERSET_EXTRA_EDGES: u64 = 202;

const MEMBERS: [&str; 14] = [
    "ganja-cli",
    "ganja-client",
    "ganja-core",
    "ganja-permission",
    "ganja-protocol",
    "ganja-provider",
    "ganja-serve",
    "ganja-storage",
    "ganja-team",
    "ganja-teammate-local",
    "ganja-testkit",
    "ganja-tool",
    "ganja-tui",
    "tmux",
];

fn workspace() -> PathBuf {
    match std::env::var("DEPGATE_E2E_WORKSPACE") {
        Ok(value) if !value.is_empty() => PathBuf::from(value),
        _ => panic!(
            "DEPGATE_E2E_WORKSPACE must be set to a ganja-code checkout \
             (a descendant of 153bfb1 whose dependency lines are unchanged) \
             to run the ignored e2e suite"
        ),
    }
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn migration_config() -> PathBuf {
    repository_root().join("tests/fixtures/ganja-code.depgate.toml")
}

fn depgate() -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!();
    command.env_remove("RUSTFLAGS").env("CARGO_TERM_COLOR", "never");
    command
}

fn run_check(
    manifest: &Path,
    config: &Path,
    locked: bool,
    all_features: bool,
    json: bool,
) -> Output {
    let mut command = depgate();
    command
        .arg("check")
        .args(["--manifest-path"])
        .arg(manifest)
        .args(["--config"])
        .arg(config)
        .arg(if locked { "--locked" } else { "--no-locked" })
        .arg("--offline");
    if all_features {
        command.arg("--all-features");
    }
    if json {
        command.args(["--format", "json"]);
    }
    command.output().expect("cargo-depgate check should execute")
}

fn report(output: &Output, context: &str) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{context} should emit JSON: {error}; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn counter(report: &serde_json::Value, field: &str) -> u64 {
    report["counters"][field].as_u64().expect("JSON report counter should be an unsigned integer")
}

fn violation<'a>(report: &'a serde_json::Value, rule_id: &str) -> &'a serde_json::Value {
    report["violations"]
        .as_array()
        .expect("JSON report violations should be an array")
        .iter()
        .find(|entry| entry["rule_id"] == rule_id)
        .unwrap_or_else(|| panic!("JSON report is missing violation {rule_id}: {report}"))
}

fn evidence_names(violation: &serde_json::Value, field: &str) -> BTreeSet<String> {
    violation[field]
        .as_array()
        .expect("JSON violation evidence should be an array")
        .iter()
        .map(|entry| {
            entry["name"]
                .as_str()
                .expect("JSON violation evidence name should be a string")
                .to_owned()
        })
        .collect()
}

fn copy_on_write_workspace(source: &Path, parent: &Path) -> PathBuf {
    let source = fs::canonicalize(source).unwrap_or_else(|error| {
        panic!("failed to canonicalize live workspace {}: {error}", source.display())
    });
    assert!(source.is_dir(), "live workspace is not a directory: {}", source.display());
    let parent = fs::canonicalize(parent).unwrap_or_else(|error| {
        panic!("failed to canonicalize temporary parent {}: {error}", parent.display())
    });
    assert!(
        !parent.starts_with(&source),
        "temporary copy parent must not be inside the live workspace: {}",
        parent.display()
    );
    let destination = parent.join("workspace");
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("/bin/cp");
        command.arg("-cR");
        command
    } else {
        let mut command = Command::new("cp");
        command.arg("-R");
        command
    };
    let status =
        command.arg(source).arg(&destination).status().expect("workspace copy should execute");
    assert!(
        status.success(),
        "copying live workspace failed with {status}: {}",
        destination.display()
    );
    let copied_metadata = fs::symlink_metadata(&destination).unwrap_or_else(|error| {
        panic!("failed to inspect copied workspace {}: {error}", destination.display())
    });
    assert!(
        copied_metadata.file_type().is_dir(),
        "copied workspace must be a real directory, not a symlink: {}",
        destination.display()
    );
    destination
}

fn insert_after_header(path: &Path, header: &str, line: &str) {
    let metadata = fs::symlink_metadata(path)
        .unwrap_or_else(|error| panic!("failed to inspect manifest {}: {error}", path.display()));
    assert!(
        metadata.file_type().is_file(),
        "injected manifest must be a regular copied file: {}",
        path.display()
    );
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    assert!(
        !text.lines().any(|existing| existing.trim() == line),
        "duplicate injected line: {line}"
    );
    let needle = format!("{header}\n");
    let replacement = format!("{needle}{line}\n");
    let updated = text.replacen(&needle, &replacement, 1);
    assert_ne!(updated, text, "manifest header {header:?} was not found in {}", path.display());
    fs::write(path, updated)
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", path.display()));
}

fn inject_core_line(copy: &Path, line: &str) {
    insert_after_header(&copy.join("crates/ganja-core/Cargo.toml"), "[dependencies]", line);
}

fn scratch_config(parent: &Path, pattern: &str) -> PathBuf {
    let path = parent.join("scratch.depgate.toml");
    let text = format!(
        "schema = 1\n\n[manifest]\nversions-in-root = false\n\n[rules.\"ganja-core\"]\ndeny = [\"{pattern}\"]\n"
    );
    fs::write(&path, text).expect("scratch depgate.toml should be writable");
    path
}

fn cargo_tree_names(manifest: &Path, member: &str) -> BTreeSet<String> {
    let output = Command::new("cargo")
        .env_remove("RUSTFLAGS")
        .env("CARGO_TERM_COLOR", "never")
        .args([
            "tree",
            "-p",
            member,
            "-e",
            "normal",
            "--prefix",
            "none",
            "--locked",
            "--offline",
            "--manifest-path",
        ])
        .arg(manifest)
        .output()
        .expect("cargo tree should execute");
    assert!(
        output.status.success(),
        "cargo tree for {member} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| *name != member)
        .map(str::to_owned)
        .collect()
}

fn normal_closure_names(
    graph: &Graph<'_>,
    member: &str,
    scratch: &mut Scratch,
) -> BTreeSet<String> {
    let name_id = graph.lookup_name(member).expect("cargo metadata should contain the member");
    let root = *graph
        .nodes_of_name(name_id)
        .first()
        .expect("cargo metadata should contain a node for the member");
    let reach = graph.reach(root, scratch);
    let names: BTreeSet<String> = reach
        .names()
        .ones()
        .map(|name_id| {
            let name_id = u32::try_from(name_id).expect("name id should fit in u32");
            graph.name_str(name_id).to_owned()
        })
        .filter(|name| name.as_str() != member)
        .collect();
    names
}

fn expected_mac_row(member: &str) -> Option<(usize, usize, usize)> {
    // Reach names are intentionally masked to remove the root, as required by AC 12.
    // These are the pinned package-rooted rows for the current aarch64 macOS checkout.
    match member {
        "ganja-protocol" => Some((15, 35, 20)),
        "ganja-team" => Some((34, 67, 33)),
        "ganja-storage" => Some((40, 68, 28)),
        "ganja-client" => Some((108, 169, 61)),
        "ganja-core" => Some((210, 291, 81)),
        "tmux" => Some((25, 30, 5)),
        _ => None,
    }
}

#[test]
#[ignore]
fn graph_identity_guard() {
    let ws = workspace();
    let output = run_check(&ws.join("Cargo.toml"), &migration_config(), true, false, true);
    let report = report(&output, "graph identity check");
    let packages = counter(&report, "packages");
    let members = counter(&report, "members");
    let normal_edges = counter(&report, "normal_edges");
    let names = counter(&report, "names");
    let extra_edges = counter(&report, "superset_extra_edges");
    eprintln!(
        "graph identity observed: {packages} packages / {members} members / {normal_edges} normal edges / {names} names; superset_extra_edges={extra_edges}"
    );
    assert!(
        (packages, members, normal_edges, names)
            == (EXPECTED_PACKAGES, EXPECTED_MEMBERS, EXPECTED_NORMAL_EDGES, EXPECTED_NAMES)
            && extra_edges == EXPECTED_SUPERSET_EXTRA_EDGES,
        "e2e expects the 153bfb1 dependency graph (585 packages / 14 members / 1586 normal edges / 529 names), got {packages} packages / {members} members / {normal_edges} normal edges / {names} names"
    );
    assert_eq!(output.status.code(), Some(0), "graph identity check should pass: {output:?}");
    assert_eq!(counter(&report, "rules"), 19);
    assert_eq!(counter(&report, "violations"), 0);
}

#[test]
#[ignore]
fn all_features_live_confirmation() {
    let ws = workspace();
    let output = run_check(&ws.join("Cargo.toml"), &migration_config(), true, true, true);
    let report = report(&output, "all-features check");
    assert_eq!(output.status.code(), Some(0), "all-features check should pass: {output:?}");
    assert_eq!(counter(&report, "rules"), 19);
    assert_eq!(counter(&report, "violations"), 0);
}

#[test]
#[ignore]
fn ratatui_injection() {
    let ws = workspace();
    let temp = tempfile::tempdir().expect("ratatui injection tempdir should be created");
    let copy = copy_on_write_workspace(&ws, temp.path());
    inject_core_line(&copy, "ratatui.workspace = true");
    let output = run_check(&copy.join("Cargo.toml"), &migration_config(), false, false, true);
    let report = report(&output, "ratatui injection");
    let expected: BTreeSet<String> = [
        "ratatui",
        "ratatui-core",
        "ratatui-crossterm",
        "ratatui-macros",
        "ratatui-termina",
        "ratatui-termwiz",
        "ratatui-widgets",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let core = violation(&report, "rules.ganja-core.deny");
    let serve = violation(&report, "rules.ganja-serve.deny");
    let actual = evidence_names(core, "matches");
    assert_eq!(core["kind"], "deny");
    assert_eq!(serve["kind"], "deny");
    assert_eq!(actual, expected, "ratatui deny names changed: {actual:?}");
    assert_eq!(evidence_names(serve, "matches"), expected);
    assert_eq!(output.status.code(), Some(1), "ratatui injection should violate: {output:?}");
    assert_eq!(counter(&report, "rules"), 19);
    assert_eq!(counter(&report, "violations"), 2);

    // AC 11(i) pins the rendered witness, not only the matched name set. The arrow is the
    // human reporter's U+2192, and the versions come from the pinned 153bfb1 graph.
    let human = run_check(&copy.join("Cargo.toml"), &migration_config(), false, false, false);
    let rendered = String::from_utf8_lossy(&human.stdout);
    let witness = "ganja-core v0.1.0 \u{2192} ratatui v0.30.2";
    assert!(rendered.contains(witness), "AC 11(i) witness {witness:?} missing from: {rendered}");
    println!(
        "ratatui injection: exit=1, violations=2, matched names={actual:?}, serve deny present, witness={witness:?}"
    );
}

#[test]
#[ignore]
fn ganja_client_injection() {
    let ws = workspace();
    let temp = tempfile::tempdir().expect("ganja-client injection tempdir should be created");
    let copy = copy_on_write_workspace(&ws, temp.path());
    inject_core_line(&copy, "ganja-client.workspace = true");
    let output = run_check(&copy.join("Cargo.toml"), &migration_config(), false, false, true);
    let report = report(&output, "ganja-client injection");
    for rule_id in ["rules.ganja-core.internal", "rules.ganja-teammate-local.internal"] {
        let internal = violation(&report, rule_id);
        assert_eq!(internal["kind"], "internal");
        assert!(
            evidence_names(internal, "extra").contains("ganja-client"),
            "{rule_id} should report ganja-client as an extra dependency: {internal}"
        );
    }
    assert_eq!(output.status.code(), Some(1), "ganja-client injection should violate: {output:?}");
    assert_eq!(counter(&report, "rules"), 19);
    assert_eq!(counter(&report, "violations"), 2);

    // AC 11(ii): the `internal` extra renders as `+<name> (via <witness>)`, with the same arrow.
    let human = run_check(&copy.join("Cargo.toml"), &migration_config(), false, false, false);
    let rendered = String::from_utf8_lossy(&human.stdout);
    let witness = "+ganja-client (via ganja-core v0.1.0 \u{2192} ganja-client v0.1.0)";
    assert!(rendered.contains(witness), "AC 11(ii) witness {witness:?} missing from: {rendered}");
    println!(
        "ganja-client injection: exit=1, violations=2, core and teammate-local internal extras include ganja-client, witness={witness:?}"
    );
}

#[test]
#[ignore]
fn axum_core_exact_vs_glob() {
    let ws = workspace();
    let temp = tempfile::tempdir().expect("axum-core injection tempdir should be created");
    let copy = copy_on_write_workspace(&ws, temp.path());
    insert_after_header(
        &copy.join("Cargo.toml"),
        "[workspace.dependencies]",
        "axum-core = \"0.5.6\"",
    );
    inject_core_line(&copy, "axum-core.workspace = true");

    let exact_config = scratch_config(temp.path(), "axum");
    let exact = run_check(&copy.join("Cargo.toml"), &exact_config, false, false, false);
    assert_eq!(exact.status.code(), Some(0), "exact axum scratch rule should pass: {exact:?}");

    let glob = run_check(&copy.join("Cargo.toml"), &migration_config(), false, false, true);
    let report = report(&glob, "axum-core migration check");
    let core = violation(&report, "rules.ganja-core.deny");
    let tui = violation(&report, "rules.ganja-tui.deny");
    assert!(evidence_names(core, "matches").contains("axum-core"));
    assert!(evidence_names(tui, "matches").contains("axum-core"));
    assert_eq!(glob.status.code(), Some(1), "axum-core migration check should violate: {glob:?}");
    assert_eq!(counter(&report, "rules"), 19);
    assert_eq!(counter(&report, "violations"), 2);
    println!(
        "axum-core injection: exact axum scratch exit=0; migration glob exit=1 with core and tui deny violations"
    );
}

#[test]
#[ignore]
fn ratatui_widgets_exact_vs_glob() {
    let ws = workspace();
    let temp = tempfile::tempdir().expect("ratatui-widgets injection tempdir should be created");
    let copy = copy_on_write_workspace(&ws, temp.path());
    // A literal member version would also trigger manifest.versions-in-root, making the
    // requested real-config 2/19 result impossible. Keep the version in the workspace table,
    // where the shipped manifest rule intentionally does not inspect it, and inherit it here.
    insert_after_header(
        &copy.join("Cargo.toml"),
        "[workspace.dependencies]",
        "ratatui-widgets = \"0.3.2\"",
    );
    inject_core_line(&copy, "ratatui-widgets.workspace = true");

    let glob_config = scratch_config(temp.path(), "ratatui*");
    let glob = run_check(&copy.join("Cargo.toml"), &glob_config, false, false, true);
    let glob_report = report(&glob, "ratatui-widgets glob scratch check");
    let glob_names = evidence_names(violation(&glob_report, "rules.ganja-core.deny"), "matches");
    assert_eq!(glob.status.code(), Some(1), "ratatui glob scratch rule should violate: {glob:?}");
    assert!(glob_names.contains("ratatui-widgets"));
    assert!(glob_names.contains("ratatui-core"));

    let exact_config = scratch_config(temp.path(), "ratatui");
    let exact = run_check(&copy.join("Cargo.toml"), &exact_config, false, false, false);
    assert_eq!(exact.status.code(), Some(0), "exact ratatui scratch rule should pass: {exact:?}");

    let migration = run_check(&copy.join("Cargo.toml"), &migration_config(), false, false, true);
    let migration_report = report(&migration, "ratatui-widgets migration check");
    assert!(
        evidence_names(violation(&migration_report, "rules.ganja-core.deny"), "matches")
            .contains("ratatui-widgets")
    );
    assert!(
        evidence_names(violation(&migration_report, "rules.ganja-serve.deny"), "matches")
            .contains("ratatui-widgets")
    );
    assert_eq!(
        migration.status.code(),
        Some(1),
        "ratatui-widgets migration should violate: {migration:?}"
    );
    assert_eq!(counter(&migration_report, "rules"), 19);
    assert_eq!(counter(&migration_report, "violations"), 2);
    println!(
        "ratatui-widgets injection: glob scratch matches {glob_names:?}; exact ratatui scratch exit=0; migration exit=1 with 2 violations"
    );
}

#[test]
#[ignore]
fn cargo_tree_differential() {
    let ws = workspace();
    let manifest = ws.join("Cargo.toml");
    let options = MetadataOptions {
        manifest_path: Some(manifest.clone()),
        locked: true,
        offline: true,
        ..MetadataOptions::default()
    };
    let buffer = metadata::acquire(&options).expect("live cargo metadata should be acquired");
    let meta = metadata::parse(&buffer).expect("live cargo metadata should parse");
    let graph = Graph::build(&meta).expect("live cargo metadata graph should build");
    let mut scratch = Scratch::new(&graph);
    let mut rows = Vec::with_capacity(MEMBERS.len());

    for member in MEMBERS {
        let tree = cargo_tree_names(&manifest, member);
        let closure = normal_closure_names(&graph, member, &mut scratch);
        let missing: BTreeSet<&String> = tree.difference(&closure).collect();
        assert!(
            missing.is_empty(),
            "cargo tree names missing from cargo-depgate closure for {member}: {missing:?}"
        );
        let extra = closure
            .len()
            .checked_sub(tree.len())
            .expect("closure should be at least as large as cargo tree");
        rows.push((member, tree.len(), closure.len(), extra));
    }

    println!("AC 12 differential (root member excluded from both distinct-name sets):");
    println!("member | cargo tree | cargo-depgate closure | extra");
    println!("---|---:|---:|---:");
    for &(member, tree, closure, extra) in &rows {
        println!("{member} | {tree} | {closure} | +{extra}");
    }
    if std::env::consts::ARCH == "aarch64" && std::env::consts::OS == "macos" {
        for (member, tree, closure, extra) in rows {
            if let Some(expected) = expected_mac_row(member) {
                assert_eq!(
                    (tree, closure, extra),
                    expected,
                    "AC 12 row drifted for {member} (root excluded from both sets)"
                );
            }
        }
    }
}

#[test]
#[ignore]
fn live_explain() {
    let ws = workspace();
    let output = depgate()
        .args(["explain", "ganja-core", "ratatui", "--manifest-path"])
        .arg(ws.join("Cargo.toml"))
        .args(["--config"])
        .arg(migration_config())
        .args(["--locked", "--offline"])
        .output()
        .expect("cargo-depgate explain should execute");
    assert_eq!(output.status.code(), Some(0), "live explain should succeed: {output:?}");
    assert_eq!(output.stdout, b"not reachable\n");
}

#[test]
#[ignore]
fn migration_diff_applicability() {
    let ws = workspace();
    let diff = repository_root().join("docs/migration/ganja-code.diff");
    // P6 turns this early return into a hard failure: once the diff is committed, a renamed or
    // mistyped path must fail the test rather than skip it.
    if !diff.exists() {
        eprintln!(
            "migration diff applicability check deferred: {} does not exist yet",
            diff.display()
        );
        return;
    }
    let output = Command::new("git")
        .args(["-C"])
        .arg(&ws)
        .args(["apply", "--check"])
        .arg(&diff)
        .output()
        .expect("git apply --check should execute");
    assert!(
        output.status.success(),
        "ganja-code migration diff is not applicable: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
