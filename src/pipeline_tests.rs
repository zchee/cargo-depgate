#![expect(clippy::expect_used, reason = "test fixtures and assertions use expect")]

use std::{
    fs,
    path::{Path, PathBuf},
};

use super::*;
use crate::{
    cli::MetadataSource, config::FeatureSelection, error::Error, metadata::MetadataOptions,
};
use tempfile::tempdir;

fn metadata_json(root: &Path) -> String {
    let root = root.to_string_lossy();
    let app_id = format!("path+file://{root}/app#0.1.0");
    let dep_id = "registry+https://example.invalid/index#dep@1.0.0";
    serde_json::json!({
        "packages": [
            {
                "name": "app",
                "version": "0.1.0",
                "id": app_id,
                "source": null,
                "manifest_path": format!("{root}/app/Cargo.toml"),
                "dependencies": [
                    {"name": "dep", "kind": null, "optional": true}
                ]
            },
            {
                "name": "dep",
                "version": "1.0.0",
                "id": dep_id,
                "source": "registry+https://example.invalid/index",
                "manifest_path": "/cargo/registry/dep-1.0.0/Cargo.toml",
                "dependencies": []
            }
        ],
        "workspace_members": [app_id],
        "workspace_root": root,
        "resolve": {
            "nodes": [
                {
                    "id": app_id,
                    "deps": [
                        {
                            "name": "dep",
                            "pkg": dep_id,
                            "dep_kinds": [{"kind": null, "target": null}]
                        }
                    ]
                },
                {"id": dep_id, "deps": []}
            ],
            "root": null
        }
    })
    .to_string()
}

fn write(path: &Path, text: &str) {
    fs::write(path, text).expect("pipeline fixture should be writable");
}

fn metadata_file(root: &Path) -> PathBuf {
    let path = root.join("metadata.json");
    write(&path, &metadata_json(root));
    path
}

fn args(root: &Path, config_path: Option<PathBuf>) -> CheckArgs {
    CheckArgs {
        metadata: MetadataOptions {
            source: Some(MetadataSource::File(metadata_file(root))),
            ..MetadataOptions::default()
        },
        config_path,
    }
}

fn explain_args(root: &Path, config_path: PathBuf, package: &str, dependency: &str) -> ExplainArgs {
    ExplainArgs {
        metadata: MetadataOptions {
            source: Some(MetadataSource::File(metadata_file(root))),
            ..MetadataOptions::default()
        },
        config_path: Some(config_path),
        package: package.to_owned(),
        dependency: dependency.to_owned(),
    }
}

fn explain_config(root: &Path) -> PathBuf {
    let path = root.join("depgate.toml");
    write(
        &path,
        "schema = 1\n\n[manifest]\nversions-in-root = false\n\n[rules.app]\ndirect = [\"dep\"]\n",
    );
    path
}

fn explain_for_test(args: &ExplainArgs) -> Result<ExplainOutcome, Error> {
    explain(args, &mut Vec::new())
}

fn run_explain(args: &ExplainArgs) -> (Result<ExplainOutcome, Error>, String) {
    let mut stderr = Vec::new();
    let result = explain(args, &mut stderr);
    (result, String::from_utf8(stderr).expect("diagnostics should be UTF-8"))
}

fn run_check(args: &CheckArgs) -> (Result<Outcome, Error>, String) {
    let mut stderr = Vec::new();
    let result = check(args, &mut stderr);
    (result, String::from_utf8(stderr).expect("diagnostics should be UTF-8"))
}

#[cfg(unix)]
#[test]
fn explicit_phase_a_error_does_not_spawn_cargo() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("temporary pipeline directory should be creatable");
    let config_path = temp.path().join("invalid.toml");
    write(&config_path, "schema = 2\n");

    let cargo = temp.path().join("fake-cargo.sh");
    write(&cargo, "#!/bin/sh\nprintf MARKER > \"$0.marker\"\nexit 7\n");
    let mut permissions = fs::metadata(&cargo).expect("fake cargo should exist").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&cargo, permissions).expect("fake cargo should be executable");

    let check_args = CheckArgs {
        metadata: MetadataOptions { cargo: Some(cargo.clone()), ..MetadataOptions::default() },
        config_path: Some(config_path),
    };
    let (result, stderr) = run_check(&check_args);

    let error = result.expect_err("phase-A configuration errors must stop the pipeline");
    assert!(matches!(error, Error::Configuration { .. }));
    assert_eq!(error.exit_code(), 2);
    assert!(error.to_string().contains("unsupported configuration schema 2"));
    assert!(!cargo.with_file_name("fake-cargo.sh.marker").exists());
    assert!(stderr.is_empty(), "phase-A failures should not emit warnings: {stderr:?}");
}

#[test]
fn discovered_phase_a_error_happens_after_metadata() {
    let temp = tempdir().expect("temporary pipeline directory should be creatable");
    let config_path = temp.path().join("depgate.toml");
    write(&config_path, "schema = 2\n");

    let check_args = args(temp.path(), None);
    let (result, stderr) = run_check(&check_args);

    let error = result.expect_err("discovered phase-A configuration should fail");
    match error {
        Error::Configuration { ref message, ref span } => {
            assert!(message.contains("unsupported configuration schema 2"));
            assert_eq!(
                span.as_ref().expect("schema errors have a span").file.as_path(),
                config_path.as_path()
            );
        }
        other => panic!("expected a configuration error, got {other:?}"),
    }
    assert_eq!(error.exit_code(), 2);
    assert!(stderr.is_empty(), "phase-A failures should not emit warnings: {stderr:?}");
}

#[cfg(unix)]
#[test]
fn phase_a_error_matches_explicit_and_discovered_config() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("temporary pipeline directory should be creatable");
    let config_path = temp.path().join("depgate.toml");
    write(&config_path, "schema = 2\n");

    let cargo = temp.path().join("fake-cargo.sh");
    write(&cargo, "#!/bin/sh\nprintf MARKER > \"$0.marker\"\nexit 7\n");
    let mut permissions = fs::metadata(&cargo).expect("fake cargo should exist").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&cargo, permissions).expect("fake cargo should be executable");

    let explicit_args = CheckArgs {
        metadata: MetadataOptions { cargo: Some(cargo.clone()), ..MetadataOptions::default() },
        config_path: Some(config_path),
    };
    let (explicit_result, explicit_stderr) = run_check(&explicit_args);
    let explicit_error = explicit_result.expect_err("explicit phase-A configuration should fail");

    let (discovered_result, discovered_stderr) = run_check(&args(temp.path(), None));
    let discovered_error =
        discovered_result.expect_err("discovered phase-A configuration should fail");

    assert_eq!(explicit_error.to_string(), discovered_error.to_string());
    assert!(!cargo.with_file_name("fake-cargo.sh.marker").exists());
    assert!(explicit_stderr.is_empty());
    assert!(discovered_stderr.is_empty());
}

#[test]
fn missing_discovered_config_names_absolute_path() {
    let temp = tempdir().expect("temporary pipeline directory should be creatable");
    let expected_path = temp.path().join("depgate.toml");

    let (result, stderr) = run_check(&args(temp.path(), None));

    let error = result.expect_err("a missing discovered configuration should fail");
    assert!(matches!(error, Error::Configuration { .. }));
    assert_eq!(error.exit_code(), 2);
    assert!(error.to_string().contains(&expected_path.display().to_string()));
    assert!(stderr.is_empty());
}

fn write_app_manifest(root: &Path, dependencies: &str) {
    fs::create_dir_all(root.join("app")).expect("app directory should be creatable");
    write(
        &root.join("app/Cargo.toml"),
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n{dependencies}"
        ),
    );
}

#[test]
fn manifest_rule_alone_is_one_rule_and_passes_on_a_clean_member() {
    let temp = tempdir().expect("temporary pipeline directory should be creatable");
    let config_path = temp.path().join("depgate.toml");
    write(&config_path, "schema = 1\n");
    write_app_manifest(temp.path(), "dep = { workspace = true, optional = true }\n");

    let (result, stderr) = run_check(&args(temp.path(), Some(config_path)));
    let outcome = result.expect("a clean member manifest passes the manifest rule");

    assert_eq!(outcome.exit, 0);
    assert_eq!(outcome.statuses.len(), 1);
    assert_eq!(outcome.statuses[0].id, "manifest.versions-in-root");
    assert_eq!(outcome.statuses[0].kind, "manifest");
    assert_eq!(outcome.statuses[0].package, None);
    assert!(outcome.statuses[0].passed);
    assert_eq!(outcome.statuses[0].matched, 0);
    assert!(outcome.violations.is_empty());
    let report = outcome.manifest.as_ref().expect("the enabled rule returns its report");
    assert!(report.passed());
    assert_eq!(report.manifests_scanned, 1);
    assert_eq!(outcome.workspace_root, temp.path());
    assert_eq!(outcome.counters.rules, 1);
    assert_eq!(outcome.counters.violations, 0);
    assert!(outcome.timings.millis(Phase::Manifest) > 0.0, "the manifest phase must be timed");
    assert!(stderr.is_empty());
}

#[test]
fn manifest_entries_fail_the_rule_once_after_the_graph_rules() {
    let temp = tempdir().expect("temporary pipeline directory should be creatable");
    let config_path = temp.path().join("depgate.toml");
    write(&config_path, "schema = 1\n\n[rules.app]\nleaf = true\n");
    write_app_manifest(
        temp.path(),
        "dep = { version = \"1.0\", optional = true }\nother = { path = \"../other\" }\n",
    );

    let (result, stderr) = run_check(&args(temp.path(), Some(config_path)));
    let outcome = result.expect("manifest entries are returned as an outcome");

    assert_eq!(outcome.exit, 1);
    let ids: Vec<&str> = outcome.statuses.iter().map(|status| status.id.as_str()).collect();
    assert_eq!(ids, vec!["rules.app.leaf", "manifest.versions-in-root"]);
    assert!(outcome.statuses[0].passed);
    assert!(!outcome.statuses[1].passed);
    assert_eq!(outcome.statuses[1].matched, 1);
    assert!(outcome.violations.is_empty(), "graph violations stay graph-only");
    let report = outcome.manifest.as_ref().expect("the enabled rule returns its report");
    assert_eq!(report.entries.len(), 1);
    let entry = &report.entries[0];
    assert_eq!(entry.package, "app");
    assert_eq!(entry.table, "dependencies");
    assert_eq!(entry.dependency, "dep");
    assert_eq!(entry.version, "1.0");
    assert_eq!(entry.span.file, temp.path().join("app/Cargo.toml"));
    assert_eq!((entry.span.line, entry.span.col), (6, 19));
    assert_eq!(outcome.counters.rules, 2);
    assert_eq!(outcome.counters.violations, 1);
    assert_eq!(outcome.counters.matches, 0, "manifest entries are not graph matches");
    assert!(stderr.is_empty());
}

/// The human report's first-run hint depends on this answer, so the pipeline has to reach the
/// workspace-owning manifest and distinguish "no `[workspace.dependencies]` table" from "no
/// readable manifest at all".
#[test]
fn a_failing_manifest_rule_records_whether_the_workspace_centralises_versions() {
    let centralising = |root_manifest: Option<&str>| {
        let temp = tempdir().expect("temporary pipeline directory should be creatable");
        let config_path = temp.path().join("depgate.toml");
        write(&config_path, "schema = 1\n");
        write_app_manifest(temp.path(), "dep = { version = \"1.0\", optional = true }\n");
        if let Some(text) = root_manifest {
            write(&temp.path().join("Cargo.toml"), text);
        }

        let (result, _) = run_check(&args(temp.path(), Some(config_path)));
        let outcome = result.expect("manifest entries are returned as an outcome");
        let report = outcome.manifest.expect("the enabled rule returns its report");
        assert!(!report.passed(), "the fixture member names a version");
        report.root_workspace_dependencies
    };

    assert_eq!(
        centralising(Some(
            "[workspace]\nmembers = [\"app\"]\n\n[workspace.dependencies]\ndep = \"1.0\"\n"
        )),
        Some(true)
    );
    assert_eq!(centralising(Some("[workspace]\nmembers = [\"app\"]\n")), Some(false));
    assert_eq!(centralising(None), None, "an unreadable root manifest stays unknown");
}

#[test]
fn a_missing_member_manifest_aborts_with_exit_3() {
    let temp = tempdir().expect("temporary pipeline directory should be creatable");
    let config_path = temp.path().join("depgate.toml");
    write(&config_path, "schema = 1\n");

    let (result, stderr) = run_check(&args(temp.path(), Some(config_path)));

    let error = result.expect_err("an unreadable member manifest must not be skipped");
    assert!(
        matches!(&error, Error::ManifestRead { path, .. } if path == &temp.path().join("app/Cargo.toml")),
        "{error:?}"
    );
    assert_eq!(error.exit_code(), 3);
    assert!(stderr.is_empty());
}

#[test]
fn passing_direct_rule_populates_counters_and_warning() {
    let temp = tempdir().expect("temporary pipeline directory should be creatable");
    let config_path = temp.path().join("depgate.toml");
    write(
        &config_path,
        "schema = 1\n\n[manifest]\nversions-in-root = false\n\n[rules.app]\ndirect = [\"dep\"]\n",
    );

    let (result, stderr) = run_check(&args(temp.path(), Some(config_path)));
    let outcome = result.expect("the direct rule should pass");

    assert_eq!(outcome.exit, 0);
    assert_eq!(outcome.statuses.len(), 1);
    assert!(outcome.statuses[0].passed);
    assert_eq!(outcome.statuses[0].kind, "direct");
    assert!(outcome.violations.is_empty());
    assert_eq!(
        outcome.counters,
        Counters {
            packages: 2,
            members: 1,
            normal_edges: 1,
            names: 2,
            superset_extra_edges: 0,
            direct_optional_decls: 1,
            unrebased_path_deps: 0,
            rules: 1,
            violations: 0,
            matches: 0,
        }
    );
    assert!(outcome.manifest.is_none(), "versions-in-root = false must skip the manifest rule");
    assert_eq!(outcome.member_versions.get("app").map(String::as_str), Some("0.1.0"));
    assert_eq!(
        outcome.features, None,
        "no cargo ran, so the selection that shaped the document is unknowable"
    );
    let warning = "warning: rules.app.direct: app declares optional dependency dep; sibling feature unification may add it to the resolved edge set\n";
    assert_eq!(outcome.warnings, vec![warning.trim_end().to_owned()]);
    assert_eq!(stderr, warning);
}

#[test]
fn failing_deny_rule_returns_policy_outcome() {
    let temp = tempdir().expect("temporary pipeline directory should be creatable");
    let config_path = temp.path().join("depgate.toml");
    write(
        &config_path,
        "schema = 1\n\n[manifest]\nversions-in-root = false\n\n[rules.app]\ndeny = [\"dep\"]\n",
    );

    let (result, stderr) = run_check(&args(temp.path(), Some(config_path)));
    let outcome = result.expect("policy violations are returned as an outcome");

    assert_eq!(outcome.exit, 1);
    assert_eq!(outcome.statuses.len(), 1);
    assert!(!outcome.statuses[0].passed);
    assert_eq!(outcome.statuses[0].matched, 1);
    assert_eq!(outcome.violations.len(), 1);
    assert_eq!(outcome.violations[0].rule_id, "rules.app.deny");
    assert_eq!(outcome.violations[0].matches[0].name, "dep");
    assert_eq!(outcome.counters.rules, 1);
    assert_eq!(outcome.counters.violations, 1);
    assert_eq!(outcome.counters.matches, 1);
    assert_eq!(outcome.counters.superset_extra_edges, 1);
    assert_eq!(outcome.counters.direct_optional_decls, 0);
    // A deny rule performs a forward BFS, so traversal timing must be a finite
    // measured value rather than the pipeline's former always-zero placeholder.
    let traversals = outcome.timings.millis(Phase::Traversals);
    assert!(
        traversals.is_finite() && traversals > 0.0,
        "unexpected traversal timing: {traversals}"
    );
    assert!(stderr.is_empty());
}

#[test]
fn explain_unknown_package_returns_configuration_error() {
    let temp = tempdir().expect("temporary pipeline directory should be creatable");
    let config_path = explain_config(temp.path());
    let result = explain_for_test(&explain_args(temp.path(), config_path, "missing", "dep"));

    let error = result.expect_err("an unknown explain root must fail");
    assert!(matches!(
        &error,
        Error::Configuration { message, span }
            if message == "explain references unknown package `missing`" && span.is_none()
    ));
    assert_eq!(error.exit_code(), 2);
}

#[test]
fn explain_unknown_dependency_returns_configuration_error() {
    let temp = tempdir().expect("temporary pipeline directory should be creatable");
    let config_path = explain_config(temp.path());
    let result = explain_for_test(&explain_args(temp.path(), config_path, "app", "missing"));

    let error = result.expect_err("an unknown explain dependency must fail");
    assert!(matches!(
        &error,
        Error::Configuration { message, span }
            if message == "explain references unknown package `missing`" && span.is_none()
    ));
    assert_eq!(error.exit_code(), 2);
}

#[test]
fn explain_returns_a_root_to_dependency_witness_when_reachable() {
    let temp = tempdir().expect("temporary pipeline directory should be creatable");
    let config_path = explain_config(temp.path());
    let outcome = explain_for_test(&explain_args(temp.path(), config_path, "app", "dep"))
        .expect("the dependency should be reachable");

    assert!(outcome.reachable);
    assert_eq!(outcome.root, "app");
    assert_eq!(outcome.root_version, "0.1.0");
    assert_eq!(outcome.dependency, "dep");
    assert_eq!(outcome.path.last().map(|hop| hop.name.as_str()), Some("dep"));
}

#[test]
fn explain_returns_an_empty_path_when_dependency_is_not_reachable() {
    let temp = tempdir().expect("temporary pipeline directory should be creatable");
    let config_path = explain_config(temp.path());
    let outcome = explain_for_test(&explain_args(temp.path(), config_path, "dep", "app"))
        .expect("an unreachable dependency is a successful query");

    assert!(!outcome.reachable);
    assert!(outcome.path.is_empty());
}

fn spawn_base() -> metadata::MetadataOptions {
    metadata::MetadataOptions::default()
}

#[test]
fn explicit_config_features_reach_the_spawn_unless_the_cli_selected_some() {
    let all = spawn_options(&spawn_base(), &config::FeatureSelection::All);
    assert!(all.all_features);

    let list =
        spawn_options(&spawn_base(), &config::FeatureSelection::List(vec!["app/net".to_owned()]));
    assert_eq!(list.features, ["app/net"]);

    let mut cli = spawn_base();
    cli.features.push("app/other".to_owned());
    let kept = spawn_options(&cli, &config::FeatureSelection::All);
    assert!(!kept.all_features, "CLI feature flags override the config");
    assert_eq!(kept.features, ["app/other"]);

    let mut json = spawn_base();
    json.source = Some(crate::cli::MetadataSource::Stdin);
    let untouched = spawn_options(&json, &config::FeatureSelection::All);
    assert!(!untouched.all_features, "nothing is spawned under --metadata-json");
}

#[test]
fn discovered_non_default_features_are_an_error_and_json_input_only_warns() {
    let all = config::FeatureSelection::All;
    assert!(feature_selection_after_metadata(true, &spawn_base(), &all).is_ok_and(|w| w.is_none()));
    assert!(
        feature_selection_after_metadata(false, &spawn_base(), &config::FeatureSelection::Default)
            .is_ok_and(|w| w.is_none())
    );

    let error = feature_selection_after_metadata(false, &spawn_base(), &all)
        .expect_err("a discovered config cannot select features after the spawn");
    assert_eq!(error.exit_code(), 2);
    assert!(error.to_string().contains("--config"), "{error}");

    let mut json = spawn_base();
    json.source = Some(crate::cli::MetadataSource::Stdin);
    let warning = feature_selection_after_metadata(false, &json, &all).expect("warning only");
    assert!(warning.is_some_and(|w| w.contains("--metadata-json")));

    let mut cli = spawn_base();
    cli.all_features = true;
    assert!(feature_selection_after_metadata(false, &cli, &all).is_ok_and(|w| w.is_none()));
}

#[test]
fn the_reported_selection_is_the_one_that_reached_the_spawn() {
    // The options passed here are the post-`spawn_options` ones, exactly as `check` reads them.
    let base = spawn_base();
    assert_eq!(
        effective_features(&base, &FeatureSelection::Default),
        Some(FeatureSelection::Default)
    );

    let mut all = spawn_base();
    all.all_features = true;
    assert_eq!(
        effective_features(&all, &FeatureSelection::Default),
        Some(FeatureSelection::All),
        "--all-features wins over the file's default"
    );

    let mut list = spawn_base();
    list.features.push("app/net".to_owned());
    assert_eq!(
        effective_features(&list, &FeatureSelection::All),
        Some(FeatureSelection::List(vec!["app/net".to_owned()])),
        "a CLI --features list wins over features = \"all\""
    );

    let mut json = spawn_base();
    json.source = Some(MetadataSource::Stdin);
    json.all_features = true;
    assert_eq!(
        effective_features(&json, &FeatureSelection::All),
        None,
        "no cargo ran under --metadata-json, so the flags shaped nothing"
    );
}

#[test]
fn bare_no_default_features_is_not_a_cli_feature_selection() {
    // Cargo combines `--no-default-features` with `--features …` rather than replacing the
    // selection, so on its own it must not discard the config's list.
    let mut no_default = spawn_base();
    no_default.no_default_features = true;
    let list = config::FeatureSelection::List(vec!["app/net".to_owned()]);

    let combined = spawn_options(&no_default, &list);
    assert!(combined.no_default_features, "the CLI flag still reaches the spawn");
    assert_eq!(combined.features, ["app/net"], "the config's list still applies");

    let all = spawn_options(&no_default, &config::FeatureSelection::All);
    assert!(all.all_features);
    assert!(all.no_default_features);

    // With no CLI selection, a discovered non-default selection stays a hard error (D12).
    let error =
        feature_selection_after_metadata(false, &no_default, &config::FeatureSelection::All)
            .expect_err("a discovered config still cannot select features after the spawn");
    assert_eq!(error.exit_code(), 2);

    // `--features` and `--all-features` remain selections that win over the config.
    let mut features = spawn_base();
    features.no_default_features = true;
    features.features.push("app/other".to_owned());
    let kept = spawn_options(&features, &config::FeatureSelection::All);
    assert!(!kept.all_features, "an explicit --features list overrides the config");
    assert_eq!(kept.features, ["app/other"]);
}

#[test]
fn explain_warns_that_metadata_json_ignores_config_features() {
    let temp = tempdir().expect("temporary pipeline directory should be creatable");
    let path = temp.path().join("depgate.toml");
    write(
        &path,
        "schema = 1\n\n[graph]\nfeatures = \"all\"\n\n[manifest]\nversions-in-root = false\n\n\
         [rules.app]\ndirect = [\"dep\"]\n",
    );
    let (outcome, stderr) = run_explain(&explain_args(temp.path(), path, "app", "dep"));

    assert!(outcome.is_ok_and(|outcome| outcome.reachable));
    assert!(
        stderr.contains("[graph].features is ignored under --metadata-json"),
        "explain must emit check's feature warning: {stderr:?}"
    );
}

/// Both argument structs are `#[non_exhaustive]`, so downstream callers reach the pipeline only
/// through these constructors: `new` has to leave the configuration discovered, and
/// `with_config_path` has to be the one thing that makes it explicit.
#[test]
fn the_argument_constructors_default_to_a_discovered_configuration() {
    let options = MetadataOptions::default().with_offline(true);

    let discovered = CheckArgs::new(options.clone());
    assert_eq!(discovered.metadata, options);
    assert_eq!(discovered.config_path, None);

    let explicit = CheckArgs::new(options.clone()).with_config_path("/ws/depgate.toml");
    assert_eq!(explicit.config_path, Some(PathBuf::from("/ws/depgate.toml")));

    let explain = ExplainArgs::new(options.clone(), "app", "dep");
    assert_eq!(explain.metadata, options);
    assert_eq!(explain.config_path, None);
    assert_eq!(explain.package, "app");
    assert_eq!(explain.dependency, "dep");

    let explain = explain.with_config_path(PathBuf::from("/ws/depgate.toml"));
    assert_eq!(explain.config_path, Some(PathBuf::from("/ws/depgate.toml")));
}
