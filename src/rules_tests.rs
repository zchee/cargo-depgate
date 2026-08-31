#![expect(clippy::expect_used, reason = "test fixtures and assertions use expect")]

use std::{collections::BTreeSet, path::PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};

use super::*;
use crate::{
    config::{FeatureSelection, InternalDef},
    graph::Graph,
    metadata::{Meta, MetadataBuffer, parse},
};

const NORMAL: &str = r#"[{"kind":null,"target":null}]"#;
const CFG: &str = r#"[{"kind":null,"target":"cfg(unix)"},{"kind":null,"target":"cfg(windows)"}]"#;

#[derive(Default)]
struct Spec {
    packages: Vec<(&'static str, &'static str)>,
    edges: Vec<(usize, usize, &'static str)>,
    members: Vec<usize>,
    decls: Vec<Decl>,
}

#[derive(Clone, Copy)]
struct Decl {
    package: usize,
    name: &'static str,
    optional: bool,
}

impl Decl {
    const fn required(package: usize, name: &'static str) -> Self {
        Self { package, name, optional: false }
    }

    const fn optional(package: usize, name: &'static str) -> Self {
        Self { package, name, optional: true }
    }

    fn json(self) -> String {
        format!(r#"{{"name":"{}","kind":null,"optional":{}}}"#, self.name, self.optional)
    }
}

impl Spec {
    fn id(&self, index: usize) -> String {
        let (name, version) = self.packages[index];
        if self.members.contains(&index) {
            format!("path+file:///ws/{name}#{version}")
        } else {
            format!("registry+https://example.invalid/index#{name}@{version}")
        }
    }

    fn json(&self) -> String {
        let packages = (0..self.packages.len())
            .map(|index| {
                let (name, version) = self.packages[index];
                let dependencies = self
                    .decls
                    .iter()
                    .filter(|decl| decl.package == index)
                    .map(|decl| decl.json())
                    .collect::<Vec<_>>();
                let source = if self.members.contains(&index) {
                    "null".to_owned()
                } else {
                    r#""registry+https://example.invalid/index""#.to_owned()
                };
                format!(
                    r#"{{"name":"{name}","version":"{version}","id":"{}","source":{source},"manifest_path":"/ws/{name}/Cargo.toml","dependencies":[{}]}}"#,
                    self.id(index),
                    dependencies.join(",")
                )
            })
            .collect::<Vec<_>>();
        let nodes = (0..self.packages.len())
            .map(|index| {
                let deps = self
                    .edges
                    .iter()
                    .filter(|(from, _, _)| *from == index)
                    .map(|(_, to, kinds)| {
                        format!(
                            r#"{{"name":"{}","pkg":"{}","dep_kinds":{kinds}}}"#,
                            self.packages[*to].0,
                            self.id(*to)
                        )
                    })
                    .collect::<Vec<_>>();
                format!(r#"{{"id":"{}","deps":[{}]}}"#, self.id(index), deps.join(","))
            })
            .collect::<Vec<_>>();
        let members = self
            .members
            .iter()
            .map(|&member| format!(r#""{}""#, self.id(member)))
            .collect::<Vec<_>>();
        format!(
            r#"{{"packages":[{}],"workspace_members":[{}],"workspace_root":"/ws","resolve":{{"nodes":[{}],"root":null}}}}"#,
            packages.join(","),
            members.join(","),
            nodes.join(",")
        )
    }

    fn graph(&self) -> Graph<'static> {
        let buffer: &'static MetadataBuffer =
            Box::leak(Box::new(MetadataBuffer::from_bytes(self.json().into_bytes())));
        let metadata: &'static Meta<'static> =
            Box::leak(Box::new(parse(buffer).expect("fixture parses")));
        Graph::build(metadata).expect("fixture graph builds")
    }
}

fn fixture_spec() -> Spec {
    Spec {
        packages: vec![
            ("a", "1.0.0"),
            ("b", "1.0.0"),
            ("mid", "1.0.0"),
            ("leaf", "1.0.0"),
            ("dual", "1.0.0"),
            ("dual", "2.0.0"),
            ("cfgdep", "1.0.0"),
            ("opt", "1.0.0"),
        ],
        edges: vec![
            (0, 2, NORMAL),
            (0, 6, CFG),
            (0, 7, NORMAL),
            (0, 4, NORMAL),
            (0, 5, NORMAL),
            (2, 3, NORMAL),
            (2, 4, NORMAL),
            (2, 5, NORMAL),
            (1, 0, NORMAL),
        ],
        members: vec![0, 1],
        decls: vec![
            Decl::required(0, "mid"),
            Decl::required(0, "cfgdep"),
            Decl::optional(0, "opt"),
            Decl::required(0, "dual"),
            Decl::required(1, "a"),
        ],
    }
}

fn meta_span() -> Span {
    Span { file: PathBuf::from("depgate.toml"), line: 1, col: 1 }
}

fn names(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn globs(values: &[&str]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for value in values {
        builder.add(Glob::new(value).expect("glob compiles"));
    }
    builder.build().expect("glob set builds")
}

fn rule(id: &str, package: &str, kind: RuleKind) -> Rule {
    Rule { id: id.to_owned(), package: package.to_owned(), kind, span: meta_span() }
}

fn deny(id: &str, package: &str, values: &[&str]) -> Rule {
    rule(
        id,
        package,
        RuleKind::Deny {
            exact: names(values),
            globs: globs(&[]),
            raw: values.iter().map(|v| (*v).to_owned()).collect(),
        },
    )
}

fn config(rules: Vec<Rule>, patterns: &[&str]) -> Config {
    Config {
        schema: 1,
        features: FeatureSelection::Default,
        internal: InternalDef { members: true, patterns: globs(patterns) },
        manifest_versions_in_root: true,
        rules,
    }
}

#[test]
fn deny_never_matches_the_rules_own_package() {
    let graph = fixture_spec().graph();
    let config = config(vec![deny("rules.a.deny", "a", &["a"])], &[]);
    let mut scratch = Scratch::new(&graph);

    let evaluation = evaluate(&graph, &config, &mut scratch);

    assert!(evaluation.violations.is_empty(), "a self-match is not a dependency finding");
    assert!(evaluation.statuses[0].passed);
    assert_eq!(evaluation.matches, 0);
}

#[test]
fn deny_failure_has_one_violation_with_bfs_versions_cfg_and_optional_witnesses() {
    let graph = fixture_spec().graph();
    let config = config(
        vec![
            deny("rules.a.deny", "a", &["leaf", "cfgdep", "opt", "dual"]),
            deny("pass", "a", &["absent"]),
        ],
        &[],
    );
    let mut scratch = Scratch::new(&graph);

    let evaluation = evaluate(&graph, &config, &mut scratch);

    assert_eq!(evaluation.violations.len(), 1);
    assert_eq!(evaluation.matches, 4);
    assert_eq!(evaluation.statuses[0].matched, 4);
    assert!(!evaluation.statuses[0].passed);
    assert!(evaluation.statuses[1].passed);
    let violation = &evaluation.violations[0];
    assert_eq!(violation.matches.len(), 4);

    let leaf = violation.matches.iter().find(|item| item.name == "leaf").expect("leaf match");
    assert_eq!(leaf.version, "1.0.0");
    assert_eq!(
        leaf.witness.iter().map(|hop| hop.name.as_str()).collect::<Vec<_>>(),
        ["mid", "leaf"]
    );
    assert!(leaf.witness.iter().all(|hop| !hop.optional && hop.target.is_none()));

    let cfg = violation.matches.iter().find(|item| item.name == "cfgdep").expect("cfg match");
    assert_eq!(cfg.witness[0].target.as_deref(), Some("cfg(unix), cfg(windows)"));
    assert!(!cfg.witness[0].optional);

    let optional =
        violation.matches.iter().find(|item| item.name == "opt").expect("optional match");
    assert!(optional.witness[0].optional);

    let dual = violation.matches.iter().find(|item| item.name == "dual").expect("dual match");
    assert_eq!(dual.other_versions, ["2.0.0"]);
}

#[test]
fn internal_reports_extra_and_missing_names_and_can_pass_exactly() {
    let graph = fixture_spec().graph();
    let failing = config(
        vec![rule("internal-fail", "a", RuleKind::Internal(names(&["mid", "missing"])))],
        &["mid", "leaf", "cfgdep"],
    );
    let mut scratch = Scratch::new(&graph);
    let evaluation = evaluate(&graph, &failing, &mut scratch);
    let violation = evaluation.violations.first().expect("internal violation");
    assert_eq!(
        violation.extra.iter().map(|item| item.name.as_str()).collect::<Vec<_>>(),
        ["leaf", "cfgdep"]
    );
    assert_eq!(violation.missing, ["missing"]);
    assert_eq!(evaluation.matches, 2);

    let graph = fixture_spec().graph();
    let passing = config(
        vec![rule("internal-pass", "a", RuleKind::Internal(names(&["mid", "leaf", "cfgdep"])))],
        &["mid", "leaf", "cfgdep"],
    );
    let mut scratch = Scratch::new(&graph);
    let evaluation = evaluate(&graph, &passing, &mut scratch);
    assert!(evaluation.violations.is_empty());
    assert!(evaluation.statuses[0].passed);
    assert_eq!(evaluation.statuses[0].matched, 0);
}

#[test]
fn leaf_is_internal_empty_and_excludes_the_rule_root_from_its_mask() {
    let graph = fixture_spec().graph();
    let failing = config(vec![rule("leaf-fail", "a", RuleKind::Leaf)], &["mid", "leaf"]);
    let mut scratch = Scratch::new(&graph);
    let evaluation = evaluate(&graph, &failing, &mut scratch);
    assert_eq!(
        evaluation.violations[0].extra.iter().map(|item| item.name.as_str()).collect::<Vec<_>>(),
        ["mid", "leaf"]
    );

    let graph = fixture_spec().graph();
    let passing = config(
        vec![
            rule("internal-root", "a", RuleKind::Internal(BTreeSet::new())),
            rule("leaf-root", "a", RuleKind::Leaf),
        ],
        &["a"],
    );
    let mut scratch = Scratch::new(&graph);
    let evaluation = evaluate(&graph, &passing, &mut scratch);
    assert!(evaluation.violations.is_empty());
    assert!(evaluation.statuses.iter().all(|status| status.passed));
}

#[test]
fn direct_compares_depth_one_names_without_running_a_traversal() {
    let graph = fixture_spec().graph();
    let passing = config(
        vec![rule("direct-pass", "a", RuleKind::Direct(names(&["mid", "cfgdep", "opt", "dual"])))],
        &[],
    );
    let mut scratch = Scratch::new(&graph);
    let evaluation = evaluate(&graph, &passing, &mut scratch);
    assert!(evaluation.violations.is_empty());
    assert_eq!(scratch.traversals(), 0, "direct reads CSR depth-one nodes only");
    assert_eq!(evaluation.superset_extra_edges, 0);

    let failing =
        config(vec![rule("direct-fail", "a", RuleKind::Direct(names(&["mid", "missing"])))], &[]);
    let evaluation = evaluate(&graph, &failing, &mut scratch);
    let violation = evaluation.violations.first().expect("direct violation");
    assert_eq!(
        violation.extra.iter().map(|item| item.name.as_str()).collect::<Vec<_>>(),
        ["cfgdep", "dual", "opt"]
    );
    assert_eq!(violation.missing, ["missing"]);
    assert!(violation.extra.iter().all(|item| item.witness.len() == 1));
    assert_eq!(scratch.traversals(), 0, "a direct rule never starts BFS");
}

#[test]
fn sealed_reports_each_consuming_member_and_passes_when_none_consume() {
    let graph = fixture_spec().graph();
    let failing = config(vec![rule("sealed-a", "a", RuleKind::Sealed)], &[]);
    let mut scratch = Scratch::new(&graph);
    let evaluation = evaluate(&graph, &failing, &mut scratch);
    let violation = evaluation.violations.first().expect("sealed violation");
    assert_eq!(violation.sealed_by.len(), 1);
    assert_eq!(violation.sealed_by[0].member, "b");
    assert_eq!(violation.sealed_by[0].witness[0].name, "a");
    assert_eq!(evaluation.matches, 1);

    let graph = fixture_spec().graph();
    let passing = config(vec![rule("sealed-b", "b", RuleKind::Sealed)], &[]);
    let mut scratch = Scratch::new(&graph);
    let evaluation = evaluate(&graph, &passing, &mut scratch);
    assert!(evaluation.violations.is_empty());
    assert!(evaluation.statuses[0].passed);
}

#[test]
fn superset_extra_edges_are_a_union_across_forward_roots() {
    let graph = fixture_spec().graph();
    let config =
        config(vec![deny("a-noop", "a", &["absent"]), deny("b-noop", "b", &["absent"])], &[]);
    let mut scratch = Scratch::new(&graph);

    let evaluation = evaluate(&graph, &config, &mut scratch);

    assert!(evaluation.violations.is_empty());
    assert_eq!(scratch.traversals(), 2);
    assert_eq!(
        evaluation.superset_extra_edges, 2,
        "cfg-only and optional edges are each counted once, not once per root"
    );
}

#[test]
fn matches_counter_sums_deny_extra_and_sealed_entries_and_status_order_is_stable() {
    let graph = fixture_spec().graph();
    let config = config(
        vec![
            deny("a-deny", "a", &["cfgdep", "dual"]),
            rule("a-internal", "a", RuleKind::Internal(names(&["mid"]))),
            deny("b-pass", "b", &["absent"]),
            rule("a-sealed", "a", RuleKind::Sealed),
        ],
        &["mid", "leaf", "cfgdep"],
    );
    let mut scratch = Scratch::new(&graph);

    let evaluation = evaluate(&graph, &config, &mut scratch);

    assert_eq!(evaluation.matches, 5, "2 deny + 2 extra + 1 sealed");
    assert_eq!(
        evaluation
            .violations
            .iter()
            .map(|violation| violation.rule_id.as_str())
            .collect::<Vec<_>>(),
        ["a-deny", "a-internal", "a-sealed"]
    );
    assert_eq!(
        evaluation.statuses.iter().map(|status| status.id.as_str()).collect::<Vec<_>>(),
        ["a-deny", "a-internal", "b-pass", "a-sealed"]
    );
    assert!(evaluation.statuses[2].passed);
    assert_eq!(evaluation.statuses[0].matched, 2);
    assert_eq!(evaluation.statuses[1].matched, 2);
    assert_eq!(evaluation.statuses[2].matched, 0);
    assert_eq!(evaluation.statuses[3].matched, 1);
}

/// Builds a `require` rule with the same exact/glob split the configuration loader applies.
fn require(id: &str, package: &str, values: &[&str]) -> Rule {
    let patterns = values
        .iter()
        .map(|value| {
            if value.contains(['*', '?', '[']) {
                let glob = Glob::new(value).expect("glob compiles");
                RequirePattern::Glob(Box::new(glob.compile_matcher()))
            } else {
                RequirePattern::Exact((*value).to_owned())
            }
        })
        .collect();
    rule(id, package, RuleKind::Require(patterns))
}

#[test]
fn require_passes_when_every_exact_and_glob_pattern_matches_the_closure() {
    let graph = fixture_spec().graph();
    let config = config(vec![require("rules.a.require", "a", &["mid", "le*", "dual"])], &[]);
    let mut scratch = Scratch::new(&graph);

    let evaluation = evaluate(&graph, &config, &mut scratch);

    assert!(evaluation.violations.is_empty(), "every pattern matches a reached name");
    assert!(evaluation.statuses[0].passed);
    assert_eq!(evaluation.statuses[0].kind, "require");
    assert_eq!(evaluation.statuses[0].matched, 0);
    assert_eq!(evaluation.matches, 0);
}

#[test]
fn an_empty_require_list_passes_vacuously_like_an_empty_deny_list() {
    // Neither kind treats "no entries" as a configuration error, so both have to mean the
    // same thing at evaluation time: nothing is asked, nothing can fail.
    let graph = fixture_spec().graph();
    let config =
        config(vec![require("rules.a.require", "a", &[]), deny("rules.a.deny", "a", &[])], &[]);
    let mut scratch = Scratch::new(&graph);

    let evaluation = evaluate(&graph, &config, &mut scratch);

    assert!(evaluation.violations.is_empty(), "an empty list asks nothing of the closure");
    assert!(evaluation.statuses.iter().all(|status| status.passed));
    assert!(evaluation.statuses.iter().all(|status| status.matched == 0));
    assert_eq!(evaluation.matches, 0);
}

#[test]
fn require_reports_only_the_unmatched_patterns_in_declaration_order() {
    let graph = fixture_spec().graph();
    let config =
        config(vec![require("rules.a.require", "a", &["absent", "mid", "no-such-*", "le*"])], &[]);
    let mut scratch = Scratch::new(&graph);

    let evaluation = evaluate(&graph, &config, &mut scratch);

    let violation = evaluation.violations.first().expect("require violation");
    assert_eq!(violation.kind, "require");
    assert_eq!(
        violation.missing,
        ["absent", "no-such-*"],
        "a partial miss keeps configuration order and never lists the patterns that matched"
    );
    assert!(violation.matches.is_empty(), "a matched pattern carries no witness");
    assert!(violation.extra.is_empty() && violation.sealed_by.is_empty());
    assert_eq!(
        evaluation.statuses[0].matched, 0,
        "a require miss is a count of names not found, so it is not a match"
    );
    assert_eq!(
        evaluation.matches, 0,
        "the counter sums names the rules found; the miss count lives in `missing`"
    );
}

#[test]
fn require_is_scoped_to_the_closure_not_to_the_whole_graph() {
    // `b` is a workspace member of the same graph, but nothing under `a` reaches it: the
    // question `require` asks is about the rule's closure, exactly as `deny` asks it.
    let graph = fixture_spec().graph();
    let config = config(vec![require("rules.a.require", "a", &["b"])], &[]);
    let mut scratch = Scratch::new(&graph);

    let evaluation = evaluate(&graph, &config, &mut scratch);

    assert_eq!(evaluation.violations.first().expect("require violation").missing, ["b"]);
    assert!(graph.lookup_name("b").is_some(), "the name exists, it is just not reachable");
}

#[test]
fn require_is_never_satisfied_by_the_rules_own_package() {
    // The dual of `deny_never_matches_the_rules_own_package`: `require` asks for a
    // dependency, so the root's own name is not a candidate for either kind.
    let graph = fixture_spec().graph();
    let config = config(vec![require("rules.a.require", "a", &["a", "a*"])], &[]);
    let mut scratch = Scratch::new(&graph);

    let evaluation = evaluate(&graph, &config, &mut scratch);

    assert_eq!(evaluation.violations.first().expect("require violation").missing, ["a", "a*"]);
}

#[test]
fn require_shares_one_forward_traversal_with_the_other_closure_rules() {
    let graph = fixture_spec().graph();
    let config = config(
        vec![
            deny("rules.a.deny", "a", &["absent"]),
            require("rules.a.require", "a", &["mid"]),
            rule("rules.a.leaf", "a", RuleKind::Leaf),
        ],
        &[],
    );
    let mut scratch = Scratch::new(&graph);

    let evaluation = evaluate(&graph, &config, &mut scratch);

    assert!(evaluation.violations.is_empty());
    assert_eq!(scratch.traversals(), 1, "require reuses the group's single forward BFS");
}
