#![expect(clippy::unwrap_used, reason = "test bodies assert directly")]
#![expect(clippy::expect_used, reason = "test bodies assert directly")]

use std::{collections::BTreeMap, fs};

use crate::{
    config::{FeatureSelection, Span},
    manifest::{ManifestReport, ManifestViolation},
    pipeline::Outcome,
    rules::{Match, SealedEntry},
    timings::{Counters, Timings},
};

use super::*;

fn context(workspace_root: &Path) -> RenderContext {
    RenderContext {
        workspace_root: workspace_root.to_path_buf(),
        tool: "cargo-depgate",
        version: "test",
        color: false,
    }
}

fn status(
    id: &str,
    package: Option<&str>,
    kind: &'static str,
    passed: bool,
    matched: u32,
) -> RuleStatus {
    RuleStatus { id: id.to_owned(), package: package.map(str::to_owned), kind, passed, matched }
}

fn span(root: &Path, file: &str, line: u32, col: u32) -> Span {
    Span { file: root.join(file), line, col }
}

fn outcome(
    root: &Path,
    statuses: Vec<RuleStatus>,
    violations: Vec<Violation>,
    manifest: Option<ManifestReport>,
    violation_count: u32,
) -> Outcome {
    let mut member_versions = BTreeMap::new();
    member_versions.insert("app".to_owned(), "1.2.3".to_owned());
    Outcome {
        statuses,
        violations,
        manifest,
        warnings: Vec::new(),
        workspace_root: root.to_path_buf(),
        member_versions,
        features: Some(FeatureSelection::Default),
        counters: Counters {
            rules: violation_count.max(1),
            violations: violation_count,
            ..Counters::default()
        },
        timings: Timings::start(),
        exit: u8::from(violation_count > 0),
    }
}

fn graph_violation(root: &Path, id: &str, kind: &'static str) -> Violation {
    Violation {
        rule_id: id.to_owned(),
        package: "app".to_owned(),
        kind,
        matches: Vec::new(),
        extra: Vec::new(),
        missing: Vec::new(),
        sealed_by: Vec::new(),
        span: span(root, "missing-depgate.toml", 3, 5),
    }
}

fn matched(name: &str, version: &str, witness: Vec<WitnessHop>) -> Match {
    Match {
        name: name.to_owned(),
        version: version.to_owned(),
        witness,
        other_versions: Vec::new(),
    }
}

fn render_text(outcome: &Outcome) -> String {
    let mut out = Vec::new();
    render(outcome, &context(&outcome.workspace_root), &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn passing_rule_and_all_pass_summary_are_exact() {
    let root = Path::new("/workspace");
    let outcome = outcome(
        root,
        vec![status("rules.app.leaf", Some("app"), "leaf", true, 0)],
        vec![],
        None,
        0,
    );

    let rendered = render_text(&outcome);

    assert_eq!(rendered, "ok rules.app.leaf\nok: 1 rules, 0 violations\n");
}

#[test]
fn deny_witnesses_include_versions_edge_annotations_and_other_versions() {
    let root = Path::new("/workspace");
    let id = "rules.app.deny";
    let mut violation = graph_violation(root, id, "deny");
    let mut first = matched(
        "platform",
        "2.0.0",
        vec![WitnessHop {
            name: "platform".to_owned(),
            version: "2.0.0".to_owned(),
            target: Some("cfg(windows)".to_owned()),
            optional: false,
        }],
    );
    first.other_versions = vec!["2.1.0".to_owned(), "3.0.0".to_owned()];
    let second = matched(
        "optional-dep",
        "4.0.0",
        vec![WitnessHop {
            name: "optional-dep".to_owned(),
            version: "4.0.0".to_owned(),
            target: None,
            optional: true,
        }],
    );
    violation.matches = vec![first, second];
    let outcome =
        outcome(root, vec![status(id, Some("app"), "deny", false, 2)], vec![violation], None, 1);

    let rendered = render_text(&outcome);

    assert!(rendered.contains("app v1.2.3 → platform v2.0.0"));
    assert!(rendered.contains("[cfg(windows)]"));
    assert!(rendered.contains("(optional; present via workspace feature unification)"));
    assert!(rendered.contains("(other reachable versions: 2.1.0, 3.0.0)"));
    assert!(rendered.ends_with("FAIL: 1 rules, 1 violations\n"));
}

#[test]
fn exact_set_failure_renders_extra_witness_and_missing_name() {
    let root = Path::new("/workspace");
    let id = "rules.app.internal";
    let mut violation = graph_violation(root, id, "internal");
    violation.extra = vec![matched(
        "unexpected",
        "0.4.0",
        vec![WitnessHop {
            name: "unexpected".to_owned(),
            version: "0.4.0".to_owned(),
            target: None,
            optional: false,
        }],
    )];
    violation.missing = vec!["required".to_owned()];
    let outcome = outcome(
        root,
        vec![status(id, Some("app"), "internal", false, 1)],
        vec![violation],
        None,
        1,
    );

    let rendered = render_text(&outcome);

    assert!(rendered.contains("+unexpected (via app v1.2.3 → unexpected v0.4.0)"));
    assert!(rendered.contains("  -required\n"));
}

#[test]
fn sealed_failure_omits_versions_from_consuming_path() {
    let root = Path::new("/workspace");
    let id = "rules.app.sealed";
    let mut violation = graph_violation(root, id, "sealed");
    violation.sealed_by = vec![SealedEntry {
        member: "tool".to_owned(),
        witness: vec![WitnessHop {
            name: "core".to_owned(),
            version: "9.8.7".to_owned(),
            target: None,
            optional: false,
        }],
    }];
    let outcome =
        outcome(root, vec![status(id, Some("app"), "sealed", false, 1)], vec![violation], None, 1);

    let rendered = render_text(&outcome);

    assert!(rendered.contains("consumed by: tool (tool → core)"));
    assert!(!rendered.contains("core v9.8.7"));
}

#[test]
fn sealed_failure_keeps_cfg_annotation_but_still_omits_versions() {
    let root = Path::new("/workspace");
    let id = "rules.app.sealed";
    let mut violation = graph_violation(root, id, "sealed");
    violation.sealed_by = vec![SealedEntry {
        member: "tool".to_owned(),
        witness: vec![WitnessHop {
            name: "core".to_owned(),
            version: "9.8.7".to_owned(),
            target: Some("cfg(windows)".to_owned()),
            optional: false,
        }],
    }];
    let outcome =
        outcome(root, vec![status(id, Some("app"), "sealed", false, 1)], vec![violation], None, 1);

    let rendered = render_text(&outcome);

    assert!(rendered.contains("consumed by: tool (tool → core [cfg(windows)])"));
    assert!(!rendered.contains("9.8.7"));
    let witness_line = rendered
        .lines()
        .find(|line| line.starts_with("  consumed by:"))
        .expect("sealed witness line exists");
    assert!(!witness_line.contains(" v"), "no version marker should appear in witness: {rendered}");
}

#[test]
fn manifest_failure_uses_source_annotation_when_file_is_readable() {
    let temp = tempfile::tempdir().unwrap();
    let manifest_path = temp.path().join("Cargo.toml");
    fs::write(&manifest_path, "[dependencies]\nserde = \"1.0\"\n").unwrap();
    let entry = ManifestViolation {
        package: "app".to_owned(),
        table: "dependencies".to_owned(),
        dependency: "serde".to_owned(),
        version: "1.0".to_owned(),
        span: Span { file: manifest_path, line: 2, col: 9 },
    };
    let report = ManifestReport { entries: vec![entry], manifests_scanned: 1, bytes_scanned: 31 };
    let outcome = outcome(
        temp.path(),
        vec![status(manifest::RULE_ID, None, manifest::RULE_KIND, false, 1)],
        vec![],
        Some(report),
        1,
    );

    let rendered = render_text(&outcome);

    assert!(rendered.contains("dependencies serde = \"1.0\""));
    assert!(rendered.contains("serde = \"1.0\""));
}

#[test]
fn manifest_failure_falls_back_when_file_cannot_be_read() {
    let root = Path::new("/workspace");
    let entry = ManifestViolation {
        package: "app".to_owned(),
        table: "dev-dependencies".to_owned(),
        dependency: "assert_cmd".to_owned(),
        version: "2".to_owned(),
        span: span(root, "missing-Cargo.toml", 7, 11),
    };
    let report = ManifestReport { entries: vec![entry], manifests_scanned: 1, bytes_scanned: 0 };
    let outcome = outcome(
        root,
        vec![status(manifest::RULE_ID, None, manifest::RULE_KIND, false, 1)],
        vec![],
        Some(report),
        1,
    );

    let rendered = render_text(&outcome);

    assert!(rendered.contains(
        "FAIL manifest.versions-in-root: missing-Cargo.toml:7:11 dev-dependencies assert_cmd = \"2\""
    ));
    assert!(rendered.ends_with("FAIL: 1 rules, 1 violations\n"));
}

#[test]
fn configuration_error_snippet_contains_message_and_source_path() {
    let temp = tempfile::tempdir().unwrap();
    let config_path = temp.path().join("depgate.toml");
    fs::write(&config_path, "schema = 1\n[rules.app]\nleaf = true\n").unwrap();
    let message = "rules.app declares an invalid setting";
    let span = Span { file: config_path.clone(), line: 2, col: 1 };

    let rendered = render_config_snippet(message, &span, false).expect("source is readable");

    assert!(rendered.contains(message));
    assert!(rendered.contains(&config_path.display().to_string()));
}

#[test]
fn configuration_error_snippet_returns_none_for_missing_source() {
    let temp = tempfile::tempdir().unwrap();
    let span = Span { file: temp.path().join("missing.toml"), line: 1, col: 1 };

    assert!(render_config_snippet("missing source", &span, false).is_none());
}

#[test]
fn line_and_character_columns_reconstruct_utf8_byte_offsets() {
    let text = "alpha\nβeta\nx\n";

    assert_eq!(line_col_to_offset(text, 1, 1), Some(0));
    assert_eq!(line_col_to_offset(text, 1, 4), Some(3));
    assert_eq!(line_col_to_offset(text, 2, 1), Some(6));
    assert_eq!(line_col_to_offset(text, 2, 2), Some(8));
    assert_eq!(line_col_to_offset(text, 3, 20), Some(13));
    assert_eq!(line_col_to_offset(text, 4, 1), Some(text.len()));
    assert_eq!(line_col_to_offset(text, 5, 1), None);
    assert_eq!(line_col_to_offset(text, 0, 1), None);
    assert_eq!(line_col_to_offset(text, 1, 0), None);
}
