//! Evaluation of dependency graph rules and their witnesses.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::{
    config::{Config, Rule, RuleKind, Span},
    graph::{Graph, Reach, Scratch},
};

/// Evaluates all graph rules in declaration order.
///
/// Rules are grouped by package so that each package gets at most one forward
/// traversal and one reverse traversal. The graph is expected to have been built
/// successfully and the configuration to have passed graph-dependent validation.
/// If a caller violates that contract by naming a package that is not a member,
/// the affected rules fail closed with an empty internal-invariant violation.
#[must_use]
pub fn evaluate(graph: &Graph<'_>, config: &Config, scratch: &mut Scratch) -> Evaluation {
    scratch.reset_extra();

    let internal_mask = internal_mask(graph, config);
    let groups = group_rules(config);
    let mut statuses: Vec<RuleStatus> = config
        .rules
        .iter()
        .map(|rule| RuleStatus {
            id: rule.id.clone(),
            package: rule.package.clone(),
            kind: kind_name(&rule.kind),
            passed: false,
            matched: 0,
        })
        .collect();
    let mut violation_slots: Vec<Option<Violation>> =
        (0..config.rules.len()).map(|_| None).collect();
    let mut matches = 0_u32;
    let mut member_nodes = HashMap::with_capacity(graph.members().len());
    for &node in graph.members() {
        member_nodes.entry(graph.name(node)).or_insert(node);
    }

    for indices in groups {
        let Some(&first_index) = indices.first() else {
            continue;
        };
        let package = &config.rules[first_index].package;
        let root = member_nodes.get(package.as_str()).copied();
        let Some(root) = root else {
            // Phase B validation guarantees this lookup. Keep evaluation fail-closed
            // for direct callers that construct Config values by hand.
            for &index in &indices {
                let result = invariant_failure(&config.rules[index]);
                record_result(index, result, &mut statuses, &mut violation_slots, &mut matches);
            }
            continue;
        };

        for &index in &indices {
            if let RuleKind::Direct(expected) = &config.rules[index].kind {
                let result = evaluate_direct(&config.rules[index], graph, root, expected);
                record_result(index, result, &mut statuses, &mut violation_slots, &mut matches);
            }
        }

        let needs_forward = indices.iter().any(|&index| {
            matches!(
                config.rules[index].kind,
                RuleKind::Deny { .. } | RuleKind::Internal(_) | RuleKind::Leaf
            )
        });
        if needs_forward {
            let reach = graph.reach(root, scratch);
            for &index in &indices {
                let result = match &config.rules[index].kind {
                    RuleKind::Deny { exact, globs, .. } => {
                        evaluate_deny(&config.rules[index], graph, &reach, root, exact, globs)
                    }
                    RuleKind::Internal(expected) => evaluate_internal(
                        &config.rules[index],
                        graph,
                        &reach,
                        root,
                        &internal_mask,
                        expected,
                    ),
                    RuleKind::Leaf => evaluate_internal(
                        &config.rules[index],
                        graph,
                        &reach,
                        root,
                        &internal_mask,
                        &BTreeSet::new(),
                    ),
                    RuleKind::Direct(_) | RuleKind::Sealed => continue,
                };
                record_result(index, result, &mut statuses, &mut violation_slots, &mut matches);
            }
        }

        let needs_reverse =
            indices.iter().any(|&index| matches!(config.rules[index].kind, RuleKind::Sealed));
        if needs_reverse {
            let reverse = graph.reverse_reach(root, scratch);
            for &index in &indices {
                if matches!(config.rules[index].kind, RuleKind::Sealed) {
                    let result = evaluate_sealed(&config.rules[index], graph, &reverse);
                    record_result(index, result, &mut statuses, &mut violation_slots, &mut matches);
                }
            }
        }
    }

    let violations = violation_slots.into_iter().flatten().collect();
    Evaluation {
        statuses,
        violations,
        matches,
        superset_extra_edges: scratch.superset_extra_edges(),
    }
}

/// The complete result of one rule evaluation pass.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Evaluation {
    /// One status for every configured rule, in configuration order.
    pub statuses: Vec<RuleStatus>,
    /// The failed rules, in their relative configuration order.
    pub violations: Vec<Violation>,
    /// The total number of deny, extra, and sealed entries.
    pub matches: u32,
    /// The union of cfg-only and member-optional edges traversed by all BFS runs.
    pub superset_extra_edges: u32,
}

/// The pass/fail status of one configured rule.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RuleStatus {
    /// The stable rule identifier.
    pub id: String,
    /// The workspace package targeted by the rule.
    pub package: String,
    /// The rule kind (`deny`, `internal`, `leaf`, `direct`, or `sealed`).
    pub kind: &'static str,
    /// Whether the rule passed.
    pub passed: bool,
    /// The number of deny, extra, or sealed entries for this rule.
    pub matched: u32,
}

/// One failed graph rule and its evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Violation {
    /// The stable identifier of the failed rule.
    pub rule_id: String,
    /// The workspace package targeted by the failed rule.
    pub package: String,
    /// The rule kind (`deny`, `internal`, `leaf`, `direct`, or `sealed`).
    pub kind: &'static str,
    /// Names matched by a deny rule.
    pub matches: Vec<Match>,
    /// Unexpected names found by an exact-set rule.
    pub extra: Vec<Match>,
    /// Expected names absent from an exact-set rule's actual set.
    pub missing: Vec<String>,
    /// Workspace members that consume a sealed package.
    pub sealed_by: Vec<SealedEntry>,
    /// The source span of the failed rule.
    pub span: Span,
}

/// One matched or unexpected package name with its shortest witness.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Match {
    /// The package name.
    pub name: String,
    /// The version of the first reached node for this name.
    pub version: String,
    /// The forward-readable witness path to the first reached node.
    pub witness: Vec<WitnessHop>,
    /// Versions of the same name reached in addition to the witness endpoint.
    ///
    /// This preserves the second value returned by [`Reach::witness_with_versions`]
    /// so a later reporter can render its "other reachable versions" note.
    pub other_versions: Vec<String>,
}

/// One edge in a dependency witness.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct WitnessHop {
    /// The package name at the end of this edge.
    pub name: String,
    /// The package version at the end of this edge.
    pub version: String,
    /// Joined cfg targets when this edge is cfg-only.
    pub target: Option<String>,
    /// Whether the source package declares this dependency optional.
    pub optional: bool,
}

/// One workspace member that consumes a sealed package.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct SealedEntry {
    /// The consuming workspace member's package name.
    pub member: String,
    /// The forward-readable path from the consumer to the sealed package.
    pub witness: Vec<WitnessHop>,
}

struct RuleResult {
    passed: bool,
    matched: u32,
    violation: Option<Violation>,
}

fn group_rules(config: &Config) -> Vec<Vec<usize>> {
    let mut groups = Vec::new();
    let mut by_package = HashMap::<&str, usize>::new();
    for (index, rule) in config.rules.iter().enumerate() {
        let group = if let Some(&group) = by_package.get(rule.package.as_str()) {
            group
        } else {
            let group = groups.len();
            groups.push(Vec::new());
            by_package.insert(rule.package.as_str(), group);
            group
        };
        groups[group].push(index);
    }
    groups
}

fn internal_mask(graph: &Graph<'_>, config: &Config) -> BTreeSet<u32> {
    let mut mask = BTreeSet::new();
    if config.internal.members {
        mask.extend(graph.members().iter().map(|&node| graph.name_id(node)));
    }
    for (id, name) in graph.names().iter().enumerate() {
        if config.internal.patterns.is_match(name)
            && let Ok(id) = u32::try_from(id)
        {
            mask.insert(id);
        }
    }
    mask
}

fn kind_name(kind: &RuleKind) -> &'static str {
    match kind {
        RuleKind::Deny { .. } => "deny",
        RuleKind::Internal(_) => "internal",
        RuleKind::Leaf => "leaf",
        RuleKind::Direct(_) => "direct",
        RuleKind::Sealed => "sealed",
    }
}

fn empty_violation(rule: &Rule) -> Violation {
    Violation {
        rule_id: rule.id.clone(),
        package: rule.package.clone(),
        kind: kind_name(&rule.kind),
        matches: Vec::new(),
        extra: Vec::new(),
        missing: Vec::new(),
        sealed_by: Vec::new(),
        span: rule.span.clone(),
    }
}

fn invariant_failure(rule: &Rule) -> RuleResult {
    RuleResult { passed: false, matched: 0, violation: Some(empty_violation(rule)) }
}

fn record_result(
    index: usize,
    result: RuleResult,
    statuses: &mut [RuleStatus],
    violation_slots: &mut [Option<Violation>],
    matches: &mut u32,
) {
    statuses[index].passed = result.passed;
    statuses[index].matched = result.matched;
    *matches = matches.saturating_add(result.matched);
    violation_slots[index] = result.violation;
}

fn evaluate_deny(
    rule: &Rule,
    graph: &Graph<'_>,
    reach: &Reach<'_, '_>,
    root: u32,
    exact: &BTreeSet<String>,
    globs: &globset::GlobSet,
) -> RuleResult {
    // The closure includes the root itself; a pattern that happens to match the
    // rule's own package (`deny = ["ganja-*"]` on `rules.ganja-core`) is not a
    // dependency finding, so the root's name never matches (`RuleKind::Deny` docs).
    let root_name = graph.name_id(root);
    let mut matches = Vec::new();
    for name_index in reach.names().ones() {
        let Ok(name_id) = u32::try_from(name_index) else {
            continue;
        };
        if name_id == root_name {
            continue;
        }
        let name = graph.name_str(name_id);
        if exact.contains(name) || globs.is_match(name) {
            matches.push(reach_match(graph, reach, name_id));
        }
    }

    if matches.is_empty() {
        return passed_result();
    }
    let match_count = count_u32(matches.len());
    let mut violation = empty_violation(rule);
    violation.matches = matches;
    RuleResult { passed: false, matched: match_count, violation: Some(violation) }
}

fn evaluate_internal(
    rule: &Rule,
    graph: &Graph<'_>,
    reach: &Reach<'_, '_>,
    root: u32,
    internal_mask: &BTreeSet<u32>,
    expected: &BTreeSet<String>,
) -> RuleResult {
    let root_name = graph.name_id(root);
    let mut actual = BTreeSet::<String>::new();
    let mut extra_ids = Vec::new();
    for name_index in reach.names().ones() {
        let Ok(name_id) = u32::try_from(name_index) else {
            continue;
        };
        if name_id == root_name || !internal_mask.contains(&name_id) {
            continue;
        }
        let name = graph.name_str(name_id);
        actual.insert(name.to_owned());
        if !expected.contains(name) {
            extra_ids.push(name_id);
        }
    }

    let missing =
        expected.iter().filter(|name| !actual.contains(name.as_str())).cloned().collect::<Vec<_>>();
    if extra_ids.is_empty() && missing.is_empty() {
        return passed_result();
    }

    let extra =
        extra_ids.into_iter().map(|name_id| reach_match(graph, reach, name_id)).collect::<Vec<_>>();
    let matched = count_u32(extra.len());
    let mut violation = empty_violation(rule);
    violation.extra = extra;
    violation.missing = missing;
    RuleResult { passed: false, matched, violation: Some(violation) }
}

fn evaluate_direct(
    rule: &Rule,
    graph: &Graph<'_>,
    root: u32,
    expected: &BTreeSet<String>,
) -> RuleResult {
    let mut direct_by_name = BTreeMap::<String, Vec<u32>>::new();
    for &node in graph.direct_nodes(root) {
        direct_by_name.entry(graph.name(node).to_owned()).or_default().push(node);
    }

    let extra_names = direct_by_name
        .keys()
        .filter(|name| !expected.contains(name.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let missing = expected
        .iter()
        .filter(|name| !direct_by_name.contains_key(*name))
        .cloned()
        .collect::<Vec<_>>();
    if extra_names.is_empty() && missing.is_empty() {
        return passed_result();
    }

    let extra = extra_names
        .iter()
        .filter_map(|name| {
            direct_by_name.get(name).and_then(|nodes| nodes.first()).map(|&first| {
                let others = direct_by_name
                    .get(name)
                    .map(|nodes| nodes.iter().copied().skip(1).collect::<Vec<_>>())
                    .unwrap_or_default();
                path_match(graph, &[root, first], &others)
            })
        })
        .collect::<Vec<_>>();
    let matched = count_u32(extra.len());
    let mut violation = empty_violation(rule);
    violation.extra = extra;
    violation.missing = missing;
    RuleResult { passed: false, matched, violation: Some(violation) }
}

fn evaluate_sealed(rule: &Rule, graph: &Graph<'_>, reverse: &Reach<'_, '_>) -> RuleResult {
    let sealed_by = reverse
        .reached_members()
        .filter_map(|member| {
            reverse.witness_to_node(member).map(|path| SealedEntry {
                member: graph.name(member).to_owned(),
                witness: witness_hops(graph, &path),
            })
        })
        .collect::<Vec<_>>();
    if sealed_by.is_empty() {
        return passed_result();
    }

    let matched = count_u32(sealed_by.len());
    let mut violation = empty_violation(rule);
    violation.sealed_by = sealed_by;
    RuleResult { passed: false, matched, violation: Some(violation) }
}

fn passed_result() -> RuleResult {
    RuleResult { passed: true, matched: 0, violation: None }
}

fn reach_match(graph: &Graph<'_>, reach: &Reach<'_, '_>, name_id: u32) -> Match {
    let first = reach.first_node_of_name(name_id);
    let (path, others) = reach.witness_with_versions(name_id).unwrap_or_default();
    let version = first.map_or_else(String::new, |node| graph.version(node).to_owned());
    let other_versions = others.iter().map(|&node| graph.version(node).to_owned()).collect();
    Match {
        name: graph.name_str(name_id).to_owned(),
        version,
        witness: witness_hops(graph, &path),
        other_versions,
    }
}

fn path_match(graph: &Graph<'_>, path: &[u32], others: &[u32]) -> Match {
    let (name, version) = path.last().map_or_else(
        || (String::new(), String::new()),
        |&node| (graph.name(node).to_owned(), graph.version(node).to_owned()),
    );
    let other_versions = others.iter().map(|&node| graph.version(node).to_owned()).collect();
    Match { name, version, witness: witness_hops(graph, path), other_versions }
}

fn witness_hops(graph: &Graph<'_>, path: &[u32]) -> Vec<WitnessHop> {
    path.windows(2)
        .map(|pair| {
            let from = pair[0];
            let to = pair[1];
            let edge = graph.edge_between(from, to);
            let target = edge.and_then(|edge| cfg_target(graph, edge));
            let optional =
                edge.and_then(|edge| graph.edge_declared_optional(edge).ok()).unwrap_or(false);
            WitnessHop {
                name: graph.name(to).to_owned(),
                version: graph.version(to).to_owned(),
                target,
                optional,
            }
        })
        .collect()
}

fn cfg_target(graph: &Graph<'_>, edge: u32) -> Option<String> {
    if !graph.edge_is_cfg_only(edge) {
        return None;
    }
    let Ok(kinds) = graph.edge_kinds(edge) else {
        return None;
    };
    let targets = kinds.iter().filter_map(|kind| kind.target.as_deref()).collect::<Vec<_>>();
    (!targets.is_empty()).then(|| targets.join(", "))
}

fn count_u32(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
