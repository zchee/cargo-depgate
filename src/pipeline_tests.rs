#![expect(clippy::expect_used, reason = "test fixtures and assertions use expect")]

use std::{
    fs,
    path::{Path, PathBuf},
};

use super::*;
use crate::{cli::MetadataSource, error::Error, metadata::MetadataOptions};
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

#[test]
fn manifest_versions_in_root_returns_p3_stub() {
    let temp = tempdir().expect("temporary pipeline directory should be creatable");
    let config_path = temp.path().join("depgate.toml");
    write(&config_path, "schema = 1\n");

    let (result, stderr) = run_check(&args(temp.path(), Some(config_path)));

    let error = result.expect_err("the P3 manifest rule must fail loudly");
    assert!(matches!(error, Error::ManifestRuleNotYetImplemented));
    assert_eq!(error.exit_code(), 2);
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
    assert!(stderr.is_empty());
}
