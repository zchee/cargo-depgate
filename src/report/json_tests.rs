#![expect(clippy::expect_used, reason = "test bodies assert directly")]

use std::{collections::BTreeMap, path::PathBuf};

use crate::{
    config::{FeatureSelection, Span},
    manifest::{self, ManifestReport, ManifestViolation},
    pipeline::Outcome,
    report::RenderContext,
    rules::{Match, RuleStatus, SealedEntry, Violation, WitnessHop},
    timings::{Counters, Timings},
};

use super::render;

const WORKSPACE: &str = "/workspace";

fn span(file: &str, line: u32) -> Span {
    Span { file: PathBuf::from(WORKSPACE).join(file), line, col: 7 }
}

fn hop(name: &str) -> WitnessHop {
    WitnessHop { name: name.to_owned(), version: "1.2.3".to_owned(), target: None, optional: false }
}

fn found(name: &str) -> Match {
    Match {
        name: name.to_owned(),
        version: "1.2.3".to_owned(),
        witness: vec![hop(name)],
        other_versions: vec!["2.0.0".to_owned()],
    }
}

fn status(id: &str, package: Option<&str>, kind: &'static str, passed: bool) -> RuleStatus {
    RuleStatus {
        id: id.to_owned(),
        package: package.map(str::to_owned),
        kind,
        passed,
        matched: u32::from(!passed),
    }
}

fn violation(rule_id: &str, kind: &'static str, line: u32) -> Violation {
    Violation {
        rule_id: rule_id.to_owned(),
        package: "app".to_owned(),
        kind,
        matches: Vec::new(),
        extra: Vec::new(),
        missing: Vec::new(),
        sealed_by: Vec::new(),
        span: span("depgate.toml", line),
    }
}

fn outcome(features: Option<FeatureSelection>) -> Outcome {
    let deny_id = "rules.app.deny";
    let internal_id = "rules.app.internal";
    let direct_id = "rules.app.direct";
    let sealed_id = "rules.app.sealed";
    Outcome {
        statuses: vec![
            status("rules.app.leaf", Some("app"), "leaf", true),
            status(deny_id, Some("app"), "deny", false),
            status(internal_id, Some("app"), "internal", false),
            status(direct_id, Some("app"), "direct", false),
            status(sealed_id, Some("app"), "sealed", false),
            status(manifest::RULE_ID, None, manifest::RULE_KIND, false),
        ],
        violations: vec![
            Violation { matches: vec![found("blocked")], ..violation(deny_id, "deny", 2) },
            Violation {
                extra: vec![found("external")],
                missing: vec!["expected".to_owned()],
                ..violation(internal_id, "internal", 3)
            },
            Violation {
                extra: vec![found("indirect")],
                missing: vec!["direct-only".to_owned()],
                ..violation(direct_id, "direct", 4)
            },
            Violation {
                sealed_by: vec![SealedEntry {
                    member: "consumer".to_owned(),
                    witness: vec![hop("core")],
                }],
                ..violation(sealed_id, "sealed", 5)
            },
        ],
        manifest: Some(ManifestReport {
            entries: vec![
                ManifestViolation {
                    package: "app".to_owned(),
                    table: "dependencies".to_owned(),
                    dependency: "serde".to_owned(),
                    version: "1".to_owned(),
                    span: span("Cargo.toml", 12),
                    span_bytes: 3,
                },
                ManifestViolation {
                    package: "app".to_owned(),
                    table: "dev-dependencies".to_owned(),
                    dependency: "tempfile".to_owned(),
                    version: "3".to_owned(),
                    span: span("Cargo.toml", 16),
                    span_bytes: 3,
                },
            ],
            manifests_scanned: 1,
            bytes_scanned: 128,
            root_workspace_dependencies: None,
        }),
        warnings: Vec::new(),
        workspace_root: PathBuf::from(WORKSPACE),
        counters: Counters {
            packages: 8,
            members: 2,
            normal_edges: 7,
            names: 8,
            superset_extra_edges: 1,
            direct_optional_decls: 2,
            unrebased_path_deps: 3,
            rules: 6,
            violations: 5,
            matches: 4,
        },
        timings: Timings::start(),
        member_versions: BTreeMap::from([("app".to_owned(), "0.1.0".to_owned())]),
        features,
        exit: 1,
    }
}

fn render_outcome(features: Option<FeatureSelection>) -> String {
    let mut bytes = Vec::new();
    let context =
        RenderContext::new(PathBuf::from(WORKSPACE), "cargo-depgate", "0.1.0-test", false);
    render(&outcome(features), &context, &mut bytes).expect("JSON rendering should succeed");
    String::from_utf8(bytes).expect("JSON output should be UTF-8")
}

#[test]
fn report_preserves_schema_order_and_represents_every_violation_kind() {
    let rendered = render_outcome(Some(FeatureSelection::Default));
    let value: serde_json::Value =
        serde_json::from_str(&rendered).expect("the reporter should emit valid JSON");

    // serde_json::Value uses a BTreeMap without `preserve_order`, so verify the
    // serialized struct order from the bytes rather than its sorted parsed map.
    let positions = ["tool", "version", "features", "timings", "counters", "violations"]
        .map(|key| rendered.find(&format!("  \"{key}\":")).expect("top-level key should exist"));
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{positions:?}\n{rendered}");

    let counters = value["counters"].as_object().expect("counters should be an object");
    let expected_counters = [
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
    assert_eq!(counters.len(), expected_counters.len());
    assert!(expected_counters.iter().all(|key| counters.contains_key(*key)));

    let violations = value["violations"].as_array().expect("violations should be an array");
    assert_eq!(violations.len(), 6);
    let manifest_entries: Vec<_> =
        violations.iter().filter(|item| item["kind"] == "manifest").collect();
    assert_eq!(manifest_entries.len(), 2, "each manifest entry needs its own JSON violation");
    let manifest = manifest_entries[0];
    assert_eq!(manifest["table"], "dependencies");
    assert_eq!(manifest["dependency"], "serde");
    assert_eq!(manifest["version"], "1");
    assert!(manifest.get("table").is_some());
    assert!(manifest.get("dependency").is_some());
    assert!(manifest.get("version").is_some());

    let deny = violations
        .iter()
        .find(|item| item["kind"] == "deny")
        .expect("deny violation should be represented");
    assert_eq!(deny["sealed_by"], serde_json::json!([]));
    assert!(!deny["matches"].as_array().expect("matches should be an array").is_empty());
    assert_eq!(value["features"], "default");
    assert!(rendered.ends_with('\n'));
    assert!(!rendered.ends_with("\n\n"));
}

#[test]
fn feature_list_is_a_bare_json_array() {
    let rendered = render_outcome(Some(FeatureSelection::List(vec!["a/b".to_owned()])));
    let value: serde_json::Value =
        serde_json::from_str(&rendered).expect("the reporter should emit valid JSON");

    assert_eq!(value["features"], serde_json::json!(["a/b"]));
}

#[test]
fn an_unknown_feature_selection_is_json_null() {
    let rendered = render_outcome(None);
    let value: serde_json::Value =
        serde_json::from_str(&rendered).expect("the reporter should emit valid JSON");

    assert_eq!(value["features"], serde_json::Value::Null);
    assert!(
        rendered.contains("\"features\": null"),
        "the key stays present so consumers can tell null from absent: {rendered}"
    );
}
