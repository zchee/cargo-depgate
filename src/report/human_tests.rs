#![expect(clippy::unwrap_used, reason = "test bodies assert directly")]
#![expect(clippy::expect_used, reason = "test bodies assert directly")]

use std::{collections::BTreeMap, fs};

use crate::{
    config::{FeatureSelection, Span},
    features::Selection,
    manifest::{self, ManifestReport, ManifestViolation},
    pipeline::Outcome,
    platform::PlatformSelection,
    rules::{Match, SealedEntry},
    timings::{Counters, Timings},
};

use super::*;

fn context(workspace_root: &Path) -> RenderContext {
    RenderContext::new(workspace_root.to_path_buf(), "cargo-depgate", "test", false)
}

fn status(
    id: &str,
    package: Option<&str>,
    kind: &'static str,
    passed: bool,
    matched: u32,
) -> RuleStatus {
    RuleStatus {
        id: id.to_owned(),
        package: package.map(str::to_owned),
        kind,
        passed,
        matched,
        features: None,
        activation_pruned: Vec::new(),
    }
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
        platform: PlatformSelection::all(),
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
        features: None,
        activation_pruned: Vec::new(),
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
        span_bytes: 5,
    };
    let report = ManifestReport {
        entries: vec![entry],
        manifests_scanned: 1,
        bytes_scanned: 31,
        root_workspace_dependencies: None,
    };
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
        span_bytes: 3,
    };
    let report = ManifestReport {
        entries: vec![entry],
        manifests_scanned: 1,
        bytes_scanned: 0,
        root_workspace_dependencies: None,
    };
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

#[test]
fn require_lists_its_unmatched_patterns_and_labels_them_as_missing() {
    let root = Path::new("/workspace");
    let id = "rules.app.require";
    let mut violation = graph_violation(root, id, "require");
    violation.missing = vec!["serde".to_owned(), "tokio-*".to_owned()];
    let outcome =
        outcome(root, vec![status(id, Some("app"), "require", false, 2)], vec![violation], None, 1);

    let rendered = render_text(&outcome);

    assert!(rendered.contains("  -serde\n  -tokio-*\n"), "unmatched patterns listed: {rendered}");
    assert!(!rendered.contains(" → "), "a require finding has no witness path: {rendered}");
    assert!(rendered.ends_with("FAIL: 1 rules, 1 violations\n"), "{rendered}");
}

#[test]
fn require_violation_label_counts_only_the_missing_patterns() {
    let root = Path::new("/workspace");
    let mut violation = graph_violation(root, "rules.app.require", "require");
    violation.missing = vec!["serde".to_owned()];
    let status = status("rules.app.require", Some("app"), "require", false, 1);

    assert_eq!(violation_label(&status, Some(&violation)), "1 missing");
}

#[test]
fn a_failing_feature_aware_rule_names_the_closure_it_was_answered_on() {
    // The mirror image of the passing line's note. A finding made against a narrowed closure
    // is a claim about one build, not about the workspace, so the selection has to travel with
    // the finding as well as with the pass -- otherwise the only rules that say which question
    // they answered are the ones that found nothing. The span file does not exist here, so the
    // fallback line carries the label verbatim and the note is readable in place.
    let root = Path::new("/workspace");
    let id = "rules.app.deny";
    let mut violation = graph_violation(root, id, "deny");
    violation.matches = vec![matched("reqwest-like", "1.0.0", Vec::new())];
    violation.features = Some(Selection::List(vec!["net".to_owned()]));
    violation.activation_pruned = vec!["tls-only".to_owned()];
    let mut status = status(id, Some("app"), "deny", false, 1);
    status.features = Some(Selection::List(vec!["net".to_owned()]));
    status.activation_pruned.clone_from(&violation.activation_pruned);
    let outcome = outcome(root, vec![status], vec![violation], None, 1);

    let rendered = render_text(&outcome);

    assert!(
        rendered.contains(
            "FAIL rules.app.deny: missing-depgate.toml:3:5 1 match(es) \
             (features = [\"net\"], 1 pruned)"
        ),
        "a failing feature-aware rule carries the same closure note a passing one does: \
         {rendered}"
    );
}

#[test]
fn a_unified_rule_adds_no_closure_note_to_its_label() {
    // The other half of AC 1 in the human report: the note appears only where a rule narrowed.
    let root = Path::new("/workspace");
    let mut violation = graph_violation(root, "rules.app.deny", "deny");
    violation.matches = vec![matched("ui", "0.1.0", Vec::new())];
    let status = status("rules.app.deny", Some("app"), "deny", false, 1);

    assert_eq!(violation_label(&status, Some(&violation)), "1 match(es)");
}

/// The first-run hint is the only line in the report that talks about configuration rather
/// than about the workspace, so it has to appear exactly when the reader can act on it: the
/// manifest rule failed and the workspace has no `[workspace.dependencies]` table.
#[test]
fn the_manifest_hint_follows_a_failure_only_when_the_workspace_centralises_nothing() {
    let root = Path::new("/workspace");
    let hinted = |root_workspace_dependencies| {
        let entry = ManifestViolation {
            package: "app".to_owned(),
            table: "dependencies".to_owned(),
            dependency: "tempfile".to_owned(),
            version: "3".to_owned(),
            span: span(root, "missing-Cargo.toml", 7, 11),
            span_bytes: 3,
        };
        let report = ManifestReport {
            entries: vec![entry],
            manifests_scanned: 1,
            bytes_scanned: 0,
            root_workspace_dependencies,
        };
        let outcome = outcome(
            root,
            vec![status(manifest::RULE_ID, None, manifest::RULE_KIND, false, 1)],
            vec![],
            Some(report),
            1,
        );
        render_text(&outcome).contains("hint: set [manifest] versions-in-root = false")
    };

    assert!(
        hinted(Some(false)),
        "a workspace without the table should be told the rule is opt-out"
    );
    assert!(!hinted(Some(true)), "a workspace that centralises versions asked for this rule");
    assert!(!hinted(None), "an unreadable root manifest is not evidence of anything");
}

#[test]
fn a_passing_manifest_rule_prints_no_hint() {
    let root = Path::new("/workspace");
    let report = ManifestReport {
        entries: Vec::new(),
        manifests_scanned: 1,
        bytes_scanned: 0,
        root_workspace_dependencies: Some(false),
    };
    let outcome = outcome(
        root,
        vec![status(manifest::RULE_ID, None, manifest::RULE_KIND, true, 0)],
        vec![],
        Some(report),
        0,
    );

    let rendered = render_text(&outcome);

    assert!(rendered.ends_with("ok: 1 rules, 0 violations\n"), "{rendered}");
}
