//! Evaluation of dependency graph rules and their witnesses.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    time::{Duration, Instant},
};

use fixedbitset::FixedBitSet;

use crate::{
    config::{Config, RequirePattern, Rule, RuleKind, Span},
    features::{self, Selection},
    graph::{Graph, Reach, Scratch},
};

/// Evaluates all graph rules in declaration order.
///
/// Rules are grouped by package so that each package gets at most one forward
/// traversal and one reverse traversal. The graph is expected to have been built
/// successfully and the configuration to have passed graph-dependent validation.
/// If a caller violates that contract by naming a package that is not a member,
/// the affected rules fail closed with an empty internal-invariant violation.
///
/// A rule carrying a `features` selection is evaluated on its own package-rooted closure
/// instead, which costs one activation walk and one masked traversal per rule and one extra
/// unified traversal per group (the superset the pruned names are measured against). Nothing
/// on that path runs for a policy without such a rule, down to the walk's allocation.
#[must_use]
pub fn evaluate(graph: &Graph<'_>, config: &Config, scratch: &mut Scratch) -> Evaluation {
    scratch.reset_extra();
    let mut traversal_time = Duration::ZERO;

    let internal_mask = internal_mask(graph, config);
    let groups = group_rules(config);
    let mut results = Results::new(config);
    // Allocated on first use so a policy with no feature-aware rule pays nothing, and shared
    // by every rule that does: the walk's per-package decode caches are the expensive part.
    let mut walk = None;
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
                results.record(index, invariant_failure(&config.rules[index]));
            }
            continue;
        };

        for &index in &indices {
            if let RuleKind::Direct(expected) = &config.rules[index].kind {
                results.record(index, evaluate_direct(&config.rules[index], graph, root, expected));
            }
        }

        let group = Group { config, indices: &indices, root, internal_mask: &internal_mask };
        traversal_time += evaluate_closure_rules(graph, &group, scratch, &mut results);
        traversal_time += evaluate_feature_rules(graph, &group, scratch, &mut walk, &mut results);

        let needs_reverse =
            indices.iter().any(|&index| matches!(config.rules[index].kind, RuleKind::Sealed));
        if needs_reverse {
            let started = Instant::now();
            let reverse = graph.reverse_reach(root, scratch);
            traversal_time += started.elapsed();
            for &index in &indices {
                if matches!(config.rules[index].kind, RuleKind::Sealed) {
                    let result = evaluate_sealed(&config.rules[index], graph, &reverse);
                    results.record(index, result);
                }
            }
        }
    }

    Evaluation {
        statuses: results.statuses,
        violations: results.violations.into_iter().flatten().collect(),
        matches: results.matches,
        superset_extra_edges: scratch.superset_extra_edges(),
        traversal_time,
    }
}

/// Evaluates every rule of one group that reads the root's **unified** forward closure,
/// running the single BFS they share, and returns the time spent inside that traversal.
///
/// `deny`, `require`, `internal` and `leaf` all ask a question about the same reach, so
/// they are answered together; `direct` and `sealed` never enter here, and neither does a
/// rule that selected a feature-aware closure of its own.
fn evaluate_closure_rules(
    graph: &Graph<'_>,
    group: &Group<'_>,
    scratch: &mut Scratch,
    results: &mut Results,
) -> Duration {
    let unified = |&index: &usize| {
        let rule = group.rule(index);
        rule.features.is_none() && rule.kind.reads_closure()
    };
    if !group.indices.iter().any(unified) {
        return Duration::ZERO;
    }

    let started = Instant::now();
    let reach = graph.reach(group.root, scratch);
    let traversal_time = started.elapsed();
    for &index in group.indices.iter().filter(|index| unified(index)) {
        if let Some(result) = evaluate_closure_rule(group.rule(index), graph, &reach, group) {
            results.record(index, result);
        }
    }
    traversal_time
}

/// Evaluates every rule of one group that carries a `features` selection, and returns the
/// time spent inside the traversals that took.
///
/// Each such rule gets its own activation walk, because each may select different features,
/// and its own BFS over the edges that activation enables. The unified closure is traversed
/// once more for the whole group — it is the same for every rule rooted here — so that each
/// rule can report the names its selection removed from it.
///
/// A walk that cannot decode a package fails the rule closed rather than answering from a
/// closure that was never computed: the alternative is a rule that passes because its
/// evidence is missing, which is the failure this whole path exists to prevent.
fn evaluate_feature_rules<'g, 'm>(
    graph: &'g Graph<'m>,
    group: &Group<'_>,
    scratch: &mut Scratch,
    walk: &mut Option<features::Walk<'g, 'm>>,
    results: &mut Results,
) -> Duration {
    let mut traversal_time = Duration::ZERO;
    let mut superset: Option<FixedBitSet> = None;
    for &index in group.indices {
        let rule = group.rule(index);
        let Some(selection) = rule.features.as_ref().map(|features| &features.selection) else {
            continue;
        };
        if !rule.kind.reads_closure() {
            continue;
        }

        // The walk is traversal work: `--timings` folds it into `traversals` with the two
        // BFS runs it feeds, rather than growing the pinned phase list by a label.
        let started = Instant::now();
        let walk = walk.get_or_insert_with(|| features::Walk::new(graph));
        let activated = walk.activate(group.root, selection);
        let Ok(activation) = activated else {
            traversal_time += started.elapsed();
            results.record(index, invariant_failure(rule));
            continue;
        };

        let superset =
            superset.get_or_insert_with(|| graph.reach(group.root, scratch).names().clone());
        let reach = graph.reach_activated(group.root, activation.edges(), scratch);
        traversal_time += started.elapsed();

        let pruned = pruned_names(graph, superset, &reach);
        let Some(mut result) = evaluate_closure_rule(rule, graph, &reach, group) else {
            continue;
        };
        results.statuses[index].activation_pruned.clone_from(&pruned);
        if let Some(violation) = result.violation.as_mut() {
            violation.activation_pruned = pruned;
        }
        results.record(index, result);
    }
    traversal_time
}

/// Answers one closure rule against `reach`, whichever closure that reach is; `None` for a
/// kind that does not read one.
fn evaluate_closure_rule(
    rule: &Rule,
    graph: &Graph<'_>,
    reach: &Reach<'_, '_>,
    group: &Group<'_>,
) -> Option<RuleResult> {
    let root = group.root;
    Some(match &rule.kind {
        RuleKind::Deny { exact, globs, .. } => {
            evaluate_deny(rule, graph, reach, root, exact, globs)
        }
        RuleKind::Require(patterns) => evaluate_require(rule, graph, reach, root, patterns),
        RuleKind::Internal(expected) => {
            evaluate_internal(rule, graph, reach, root, group.internal_mask, expected)
        }
        RuleKind::Leaf => {
            evaluate_internal(rule, graph, reach, root, group.internal_mask, &BTreeSet::new())
        }
        RuleKind::Direct(_) | RuleKind::Sealed => return None,
    })
}

/// The names the unified closure `superset` carries that the activated `reach` does not,
/// alphabetically — the evidence that a feature-aware rule narrowed anything at all.
fn pruned_names(graph: &Graph<'_>, superset: &FixedBitSet, reach: &Reach<'_, '_>) -> Vec<String> {
    let mut pruned = superset
        .difference(reach.names())
        .filter_map(|name_index| u32::try_from(name_index).ok())
        .map(|name_id| graph.name_str(name_id).to_owned())
        .collect::<Vec<_>>();
    pruned.sort_unstable();
    pruned
}

/// The complete result of one rule evaluation pass.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Evaluation {
    /// One status for every configured rule, in configuration order.
    pub statuses: Vec<RuleStatus>,
    /// The failed rules, in their relative configuration order.
    pub violations: Vec<Violation>,
    /// The total number of deny, extra and sealed entries.
    ///
    /// `require` contributes nothing: its finding counts the patterns that matched
    /// *nothing*, which is not the same quantity every other contributor reports.
    pub matches: u32,
    /// The union of cfg-only and member-optional edges traversed by all BFS runs.
    pub superset_extra_edges: u32,
    /// Wall time spent inside every forward and reverse BFS this evaluation ran,
    /// summed across roots — lets the pipeline split `Phase::Traversals` from
    /// `Phase::Evaluate` without touching `graph.rs`.
    pub traversal_time: Duration,
}

/// The pass/fail status of one configured rule.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RuleStatus {
    /// The stable rule identifier.
    pub id: String,
    /// The workspace package targeted by the rule, or `None` for the manifest rule.
    pub package: Option<String>,
    /// The rule kind (`deny`, `require`, `internal`, `leaf`, `direct`, or `sealed`).
    pub kind: &'static str,
    /// Whether the rule passed.
    pub passed: bool,
    /// The number of deny, extra or sealed entries for this rule; always zero for a
    /// `require` rule, whose count of unmatched patterns is `Violation::missing` instead.
    pub matched: u32,
    /// The feature selection this rule's closure was narrowed to, or `None` when it read the
    /// workspace-unified closure.
    pub features: Option<Selection>,
    /// Names in this rule's unified closure that its activation removed, alphabetically.
    ///
    /// Always empty for a `features`-less rule, which narrows nothing. This is the only place
    /// the pruning is recorded: it is per-rule evidence, not a run-wide quantity, so no
    /// counter reports it.
    pub activation_pruned: Vec<String>,
}

/// One failed graph rule and its evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Violation {
    /// The stable identifier of the failed rule.
    pub rule_id: String,
    /// The workspace package targeted by the failed rule.
    pub package: String,
    /// The rule kind (`deny`, `require`, `internal`, `leaf`, `direct`, or `sealed`).
    pub kind: &'static str,
    /// Names matched by a deny rule.
    pub matches: Vec<Match>,
    /// Unexpected names found by an exact-set rule.
    pub extra: Vec<Match>,
    /// Expected names absent from an exact-set rule's actual set, or the `require`
    /// patterns that matched nothing in the rule's closure.
    pub missing: Vec<String>,
    /// Workspace members that consume a sealed package.
    pub sealed_by: Vec<SealedEntry>,
    /// The source span of the failed rule.
    pub span: Span,
    /// The feature selection the failed rule's closure was narrowed to, or `None` when it read
    /// the workspace-unified closure.
    pub features: Option<Selection>,
    /// Names in the rule's unified closure that its activation removed, alphabetically; empty
    /// for a rule that narrows nothing. See [`RuleStatus::activation_pruned`].
    pub activation_pruned: Vec<String>,
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

/// The rules of one package and everything their evaluation shares: they are answered
/// together because they are all rooted at the same node, whichever closure each reads.
struct Group<'c> {
    config: &'c Config,
    /// Configuration positions of this package's rules, in declaration order.
    indices: &'c [usize],
    /// The workspace member every rule in the group is rooted at.
    root: u32,
    internal_mask: &'c BTreeSet<u32>,
}

impl Group<'_> {
    fn rule(&self, index: usize) -> &Rule {
        &self.config.rules[index]
    }
}

/// The accumulating output of one evaluation pass: one status and one violation slot per
/// configured rule, indexed by configuration position so results can be recorded in any
/// order and still be reported in declaration order.
struct Results {
    statuses: Vec<RuleStatus>,
    violations: Vec<Option<Violation>>,
    matches: u32,
}

impl Results {
    fn new(config: &Config) -> Self {
        Self {
            statuses: config
                .rules
                .iter()
                .map(|rule| RuleStatus {
                    id: rule.id.clone(),
                    package: Some(rule.package.clone()),
                    kind: kind_name(&rule.kind),
                    passed: false,
                    matched: 0,
                    features: rule_selection(rule),
                    activation_pruned: Vec::new(),
                })
                .collect(),
            violations: (0..config.rules.len()).map(|_| None).collect(),
            matches: 0,
        }
    }

    fn record(&mut self, index: usize, result: RuleResult) {
        self.statuses[index].passed = result.passed;
        self.statuses[index].matched = result.matched;
        self.matches = self.matches.saturating_add(result.matched);
        self.violations[index] = result.violation;
    }
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
        RuleKind::Require(_) => "require",
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
        features: rule_selection(rule),
        activation_pruned: Vec::new(),
    }
}

/// The selection a rule's records report: the one its closure was narrowed to, and `None` for
/// the kinds that read no closure, whose records must not claim a narrowing that did not apply.
fn rule_selection(rule: &Rule) -> Option<Selection> {
    rule.kind
        .reads_closure()
        .then(|| rule.features.as_ref().map(|features| features.selection.clone()))
        .flatten()
}

fn invariant_failure(rule: &Rule) -> RuleResult {
    RuleResult { passed: false, matched: 0, violation: Some(empty_violation(rule)) }
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
    // rule's own package (`deny = ["acme-*"]` on `rules.acme-core`) is not a
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

/// Evaluates a `require` rule: every pattern must match some name in the closure.
///
/// The verdict is taken per pattern rather than per reached name, so the violation can
/// list exactly the patterns that matched nothing, in declaration order. An exact pattern
/// resolves through the name table in `O(1)`; a glob scans the reached names and stops at
/// its first match. Like `deny`, the root's own name is not a candidate.
///
/// The rule reports `matched: 0` however many patterns missed. Every other contributor to
/// that counter reports names it *found*, and summing those with names *not* found would
/// make the total mean nothing; the miss count reaches the reports through
/// `Violation::missing`, which is what the human and GitHub labels already read.
fn evaluate_require(
    rule: &Rule,
    graph: &Graph<'_>,
    reach: &Reach<'_, '_>,
    root: u32,
    patterns: &[RequirePattern],
) -> RuleResult {
    let root_name = graph.name_id(root);
    let missing = patterns
        .iter()
        .filter(|pattern| !is_required_name_reached(graph, reach, root_name, pattern))
        .map(|pattern| pattern.as_str().to_owned())
        .collect::<Vec<_>>();

    if missing.is_empty() {
        return passed_result();
    }
    let mut violation = empty_violation(rule);
    violation.missing = missing;
    RuleResult { passed: false, matched: 0, violation: Some(violation) }
}

fn is_required_name_reached(
    graph: &Graph<'_>,
    reach: &Reach<'_, '_>,
    root_name: u32,
    pattern: &RequirePattern,
) -> bool {
    match pattern {
        RequirePattern::Exact(name) => graph
            .lookup_name(name)
            .is_some_and(|name_id| name_id != root_name && reach.contains_name(name_id)),
        RequirePattern::Glob(_) => reach.names().ones().any(|name_index| {
            u32::try_from(name_index).is_ok_and(|name_id| {
                name_id != root_name && pattern.is_match(graph.name_str(name_id))
            })
        }),
    }
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

/// Converts a raw node path into witness hops with cfg/optional annotations.
pub(crate) fn witness_hops(graph: &Graph<'_>, path: &[u32]) -> Vec<WitnessHop> {
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

/// Returns joined `cfg(...)` targets for an edge, or `None` when unconditional.
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
