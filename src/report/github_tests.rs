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

use super::{escape, escape_property, render};

const WORKSPACE: &str = "/workspace";

fn span(file: impl Into<PathBuf>, line: u32) -> Span {
    Span { file: file.into(), line, col: 7 }
}

fn status(id: &str, kind: &'static str, passed: bool) -> RuleStatus {
    RuleStatus {
        id: id.to_owned(),
        package: (kind != manifest::RULE_KIND).then(|| "app".to_owned()),
        kind,
        passed,
        matched: u32::from(!passed),
        features: None,
        activation_pruned: Vec::new(),
    }
}

fn violation(id: &str, file: impl Into<PathBuf>, line: u32) -> Violation {
    Violation {
        rule_id: id.to_owned(),
        package: "app".to_owned(),
        kind: "deny",
        matches: vec![Match {
            name: "blocked".to_owned(),
            version: "2.0.0".to_owned(),
            witness: vec![WitnessHop {
                name: "blocked".to_owned(),
                version: "2.0.0".to_owned(),
                target: None,
                optional: false,
            }],
            other_versions: Vec::new(),
        }],
        extra: Vec::new(),
        missing: Vec::new(),
        sealed_by: Vec::new(),
        span: span(file, line),
        features: None,
        activation_pruned: Vec::new(),
    }
}

fn manifest_entry(dependency: &str, line: u32) -> ManifestViolation {
    ManifestViolation {
        package: "app".to_owned(),
        table: "dependencies".to_owned(),
        dependency: dependency.to_owned(),
        version: "1".to_owned(),
        span: span(PathBuf::from(WORKSPACE).join("Cargo.toml"), line),
        span_bytes: 3,
    }
}

fn outcome(graph_count: usize, manifest_entries: Vec<ManifestViolation>) -> Outcome {
    let statuses = (0..graph_count)
        .map(|index| status(&format!("rules.app.deny-{index}"), "deny", false))
        .chain(
            (!manifest_entries.is_empty())
                .then(|| status(manifest::RULE_ID, manifest::RULE_KIND, false)),
        )
        .collect::<Vec<_>>();
    let violations = (0..graph_count)
        .map(|index| {
            violation(
                &format!("rules.app.deny-{index}"),
                PathBuf::from(WORKSPACE).join("depgate.toml"),
                u32::try_from(index).expect("test index fits u32") + 2,
            )
        })
        .collect();
    let rules = u32::try_from(statuses.len()).expect("test status count fits u32");
    Outcome {
        statuses,
        violations,
        manifest: (!manifest_entries.is_empty()).then_some(ManifestReport {
            entries: manifest_entries,
            manifests_scanned: 1,
            bytes_scanned: 64,
            root_workspace_dependencies: None,
        }),
        warnings: Vec::new(),
        workspace_root: PathBuf::from(WORKSPACE),
        counters: Counters { rules, violations: rules, matches: rules, ..Counters::default() },
        timings: Timings::default(),
        member_versions: BTreeMap::from([("app".to_owned(), "1.0.0".to_owned())]),
        features: Some(FeatureSelection::Default),
        exit: 1,
    }
}

fn context() -> RenderContext {
    RenderContext::new(PathBuf::from(WORKSPACE), "cargo-depgate", "0.1.0", false)
}

fn rendered(outcome: &Outcome) -> String {
    rendered_with(outcome, &context())
}

fn rendered_with(outcome: &Outcome, ctx: &RenderContext) -> String {
    let mut output = Vec::new();
    render(outcome, ctx, &mut output).expect("render succeeds");
    String::from_utf8(output).expect("report is UTF-8")
}

/// One annotation over a workspace rooted at `workspace_root`, rendered with the given
/// `$GITHUB_WORKSPACE` value.
fn annotation_for(workspace_root: &str, github_workspace: Option<&str>) -> String {
    let mut nested = outcome(1, Vec::new());
    nested.workspace_root = PathBuf::from(workspace_root);
    nested.violations[0].span.file = PathBuf::from(workspace_root).join("depgate.toml");
    let ctx = RenderContext::new(PathBuf::from(workspace_root), "cargo-depgate", "0.1.0", false)
        .with_github_workspace(github_workspace.map(PathBuf::from));
    rendered_with(&nested, &ctx).lines().next().expect("annotation exists").to_owned()
}

#[test]
fn escape_percent_before_carriage_returns_and_newlines() {
    assert_eq!(escape("100%\rnext\nuser typed %0A"), "100%25%0Dnext%0Auser typed %250A");
}

#[test]
fn escape_property_also_escapes_colon_and_comma() {
    assert_eq!(escape_property("crates/a,b/Cargo.toml"), "crates/a%2Cb/Cargo.toml");
    assert_eq!(escape_property("C:\\path"), "C%3A\\path");
    assert_eq!(escape_property("100%\r\n"), "100%25%0D%0A");
}

#[test]
fn caps_annotations_at_ten_then_prints_the_full_human_report() {
    let output = rendered(&outcome(12, Vec::new()));
    let annotations = output.match_indices("::error ").collect::<Vec<_>>();
    assert_eq!(annotations.len(), 10);

    let tenth_annotation = annotations[9].0;
    let human_marker = output.find("FAIL: 12 rules, 12 violations").expect("human summary exists");
    assert!(human_marker > tenth_annotation);
    assert!(output.contains("FAIL rules.app.deny-11:"));
}

#[test]
fn graph_annotations_precede_manifest_annotations() {
    let output = rendered(&outcome(
        3,
        vec![manifest_entry("serde", 20), manifest_entry("toml", 21), manifest_entry("clap", 22)],
    ));
    let annotation_lines =
        output.lines().filter(|line| line.starts_with("::error ")).collect::<Vec<_>>();
    assert_eq!(annotation_lines.len(), 6);
    assert!(annotation_lines[..3].iter().all(|line| line.contains("rules.app.deny-")));
    assert!(annotation_lines[3..].iter().all(|line| line.contains("manifest.versions-in-root")));
}

#[test]
fn annotation_file_is_relative_to_workspace_root() {
    let output = rendered(&outcome(1, Vec::new()));
    let annotation = output.lines().next().expect("annotation exists");
    assert!(annotation.starts_with("::error file=depgate.toml,line=2,col=7::"));
    assert!(annotation.contains("rules.app.deny-0: 1 match(es) — app v1.0.0 → blocked v2.0.0"));
    assert!(!annotation.contains(WORKSPACE));
}

#[test]
fn sealed_annotation_matches_the_version_free_human_body_convention() {
    let mut sealed_outcome = outcome(0, Vec::new());
    sealed_outcome.statuses = vec![status("rules.app.sealed", "sealed", false)];
    sealed_outcome.violations = vec![Violation {
        rule_id: "rules.app.sealed".to_owned(),
        package: "app".to_owned(),
        kind: "sealed",
        matches: Vec::new(),
        extra: Vec::new(),
        missing: Vec::new(),
        sealed_by: vec![SealedEntry {
            member: "tool".to_owned(),
            witness: vec![WitnessHop {
                name: "core".to_owned(),
                version: "9.8.7".to_owned(),
                target: Some("cfg(windows)".to_owned()),
                optional: false,
            }],
        }],
        span: span(PathBuf::from(WORKSPACE).join("depgate.toml"), 2),
        features: None,
        activation_pruned: Vec::new(),
    }];
    sealed_outcome.counters =
        Counters { rules: 1, violations: 1, matches: 1, ..Counters::default() };

    let output = rendered(&sealed_outcome);
    let annotation_line = output
        .lines()
        .find(|line| line.contains("rules.app.sealed"))
        .expect("sealed annotation line exists");
    assert!(annotation_line.contains("— tool → core [cfg(windows)]"), "{annotation_line}");
    assert!(!annotation_line.contains(" v"), "no version marker expected: {annotation_line}");
}

#[test]
fn annotation_file_is_relative_to_github_workspace_when_the_workspace_is_nested() {
    let annotation = annotation_for("/repo/rust", Some("/repo"));
    assert!(
        annotation.starts_with("::error file=rust/depgate.toml,line=2,col=7::"),
        "{annotation}"
    );
}

#[test]
fn annotation_file_keeps_the_workspace_anchor_when_the_workspace_is_outside_github_workspace() {
    let annotation = annotation_for(WORKSPACE, Some("/repo"));
    assert!(annotation.starts_with("::error file=depgate.toml,line=2,col=7::"), "{annotation}");
}

#[test]
fn an_empty_github_workspace_is_treated_as_unset() {
    let annotation = annotation_for("/repo/rust", Some(""));
    assert!(annotation.starts_with("::error file=depgate.toml,line=2,col=7::"), "{annotation}");
}

/// `cargo metadata` reports a canonical `workspace_root`, so a `$GITHUB_WORKSPACE` that
/// reaches the same directory through a symlink or a `..` segment fails a lexical
/// containment test. Both forms have to anchor the annotation at the repository anyway,
/// or the `file=` quietly reverts to the workspace anchor and points at the wrong place.
#[cfg(unix)]
#[test]
fn a_non_canonical_github_workspace_still_anchors_the_annotation() {
    let temp = tempfile::tempdir().expect("temporary directory should be creatable");
    let repository = temp.path().join("repo");
    let workspace = repository.join("rust");
    std::fs::create_dir_all(&workspace).expect("the nested workspace should be creatable");
    std::fs::write(workspace.join("depgate.toml"), "").expect("the manifest should be writable");
    let symlinked = temp.path().join("link");
    std::os::unix::fs::symlink(&repository, &symlinked).expect("symlink should be creatable");

    // What cargo hands the pipeline, which is what the lexical comparison was measured against.
    let canonical = std::fs::canonicalize(&workspace).expect("the workspace resolves");
    let workspace_root = canonical.to_str().expect("temporary paths are UTF-8");

    for repository_root in [symlinked, repository.join("rust/..")] {
        let root = repository_root.to_str().expect("temporary paths are UTF-8");
        let annotation = annotation_for(workspace_root, Some(root));
        assert!(
            annotation.starts_with("::error file=rust/depgate.toml,line=2,col=7::"),
            "{root}: {annotation}"
        );
    }
}
