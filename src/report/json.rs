//! Renders the complete policy outcome as ordered, pretty-printed JSON.

use std::{io, path::Path};

use serde::Serialize;

use crate::{
    config::{FeatureSelection, Span},
    features::Selection,
    manifest,
    rules::{Match, SealedEntry, Violation, WitnessHop},
    timings::{Counters, Phase},
};

use super::RenderContext;

#[derive(Serialize)]
struct Report<'a> {
    tool: &'static str,
    version: &'static str,
    features: serde_json::Value,
    timings: TimingsJson,
    counters: CountersJson,
    #[serde(skip_serializing_if = "Option::is_none")]
    rules: Option<Vec<RuleJson<'a>>>,
    violations: Vec<ViolationJson<'a>>,
}

/// Timing fields are measured immediately before serialization begins: the document cannot time
/// its own write, so `report` and `total` are lower bounds. They remain self-consistent within the
/// document (`total` is always at least the sum of the other six phase fields plus `report`), while
/// the `--timings` stderr stream from the CLI is the authoritative wall-clock source when they
/// disagree.
#[derive(Serialize)]
struct TimingsJson {
    read: f64,
    parse: f64,
    graph: f64,
    traversals: f64,
    evaluate: f64,
    manifest: f64,
    report: f64,
    total: f64,
}

impl TimingsJson {
    fn from_outcome(outcome: &crate::pipeline::Outcome) -> Self {
        Self {
            read: outcome.timings.millis(Phase::Read),
            parse: outcome.timings.millis(Phase::Parse),
            graph: outcome.timings.millis(Phase::Graph),
            traversals: outcome.timings.millis(Phase::Traversals),
            evaluate: outcome.timings.millis(Phase::Evaluate),
            manifest: outcome.timings.millis(Phase::Manifest),
            report: outcome.timings.millis(Phase::Report),
            total: outcome.timings.millis(Phase::Total),
        }
    }
}

#[derive(Serialize)]
struct CountersJson {
    packages: u32,
    members: u32,
    normal_edges: u32,
    names: u32,
    superset_extra_edges: u32,
    direct_optional_decls: u32,
    unrebased_path_deps: u32,
    rules: u32,
    violations: u32,
    matches: u32,
}

impl From<Counters> for CountersJson {
    fn from(counters: Counters) -> Self {
        Self {
            packages: counters.packages,
            members: counters.members,
            normal_edges: counters.normal_edges,
            names: counters.names,
            superset_extra_edges: counters.superset_extra_edges,
            direct_optional_decls: counters.direct_optional_decls,
            unrebased_path_deps: counters.unrebased_path_deps,
            rules: counters.rules,
            violations: counters.violations,
            matches: counters.matches,
        }
    }
}

/// One record per configured rule, in the order the rules were evaluated — the surface
/// `violations[]` cannot provide, because a rule that passes emits no violation at all.
///
/// The array is written only when the policy carries at least one feature-aware rule. That is
/// the only case in which a rule's outcome is not already recoverable from `violations[]` and
/// `counters`: a rule that passes *because its selection compiled the offending name out* is
/// indistinguishable, in a report without this array, from one that passes because the name was
/// never in the graph, and `activation_pruned` is the evidence that tells them apart. A policy
/// with no feature-aware rule therefore produces the report it produced before this key existed,
/// byte for byte.
///
/// `features` and `activation_pruned` are per-record and follow the same rule the violation
/// records follow: both are absent for a rule evaluated on the workspace-unified closure, so a
/// unified rule sitting beside a feature-aware one adds no empty keys.
#[derive(Serialize)]
struct RuleJson<'a> {
    id: &'a str,
    kind: &'static str,
    passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    features: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    activation_pruned: Option<&'a [String]>,
}

#[derive(Serialize)]
struct SpanJson {
    file: String,
    line: u32,
    col: u32,
}

#[derive(Serialize)]
struct WitnessJson<'a> {
    name: &'a str,
    version: &'a str,
    target: Option<&'a str>,
    optional: bool,
}

impl<'a> From<&'a WitnessHop> for WitnessJson<'a> {
    fn from(hop: &'a WitnessHop) -> Self {
        Self {
            name: &hop.name,
            version: &hop.version,
            target: hop.target.as_deref(),
            optional: hop.optional,
        }
    }
}

#[derive(Serialize)]
struct MatchJson<'a> {
    name: &'a str,
    version: &'a str,
    witness: Vec<WitnessJson<'a>>,
    other_versions: &'a [String],
}

impl<'a> From<&'a Match> for MatchJson<'a> {
    fn from(found: &'a Match) -> Self {
        Self {
            name: &found.name,
            version: &found.version,
            witness: found.witness.iter().map(WitnessJson::from).collect(),
            other_versions: &found.other_versions,
        }
    }
}

#[derive(Serialize)]
struct SealedJson<'a> {
    member: &'a str,
    witness: Vec<WitnessJson<'a>>,
}

impl<'a> From<&'a SealedEntry> for SealedJson<'a> {
    fn from(entry: &'a SealedEntry) -> Self {
        Self {
            member: &entry.member,
            witness: entry.witness.iter().map(WitnessJson::from).collect(),
        }
    }
}

/// The `sealed_by` array is always present so sealed violations remain representable,
/// extending the compressed report shape that listed only deny and exact-set evidence.
///
/// `features` and `activation_pruned` are the exception: they are absent for a rule evaluated
/// on the workspace-unified closure, which keeps every report a policy without feature-aware
/// rules produces byte-identical to the one it produced before the key existed.
#[derive(Serialize)]
struct ViolationJson<'a> {
    rule_id: &'a str,
    package: &'a str,
    kind: &'static str,
    matches: Vec<MatchJson<'a>>,
    extra: Vec<MatchJson<'a>>,
    missing: &'a [String],
    sealed_by: Vec<SealedJson<'a>>,
    span: SpanJson,
    #[serde(skip_serializing_if = "Option::is_none")]
    features: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    activation_pruned: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    table: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dependency: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<&'a str>,
}

impl<'a> ViolationJson<'a> {
    fn graph(violation: &'a Violation, workspace_root: &Path) -> Self {
        Self {
            rule_id: &violation.rule_id,
            package: &violation.package,
            kind: violation.kind,
            matches: violation.matches.iter().map(MatchJson::from).collect(),
            extra: violation.extra.iter().map(MatchJson::from).collect(),
            missing: &violation.missing,
            sealed_by: violation.sealed_by.iter().map(SealedJson::from).collect(),
            span: span_json(&violation.span, workspace_root),
            features: violation.features.as_ref().map(selection_json),
            activation_pruned: violation
                .features
                .as_ref()
                .map(|_| violation.activation_pruned.as_slice()),
            table: None,
            dependency: None,
            version: None,
        }
    }

    fn manifest(entry: &'a manifest::ManifestViolation, workspace_root: &Path) -> Self {
        Self {
            rule_id: manifest::RULE_ID,
            package: &entry.package,
            kind: manifest::RULE_KIND,
            matches: Vec::new(),
            extra: Vec::new(),
            missing: &[],
            sealed_by: Vec::new(),
            span: span_json(&entry.span, workspace_root),
            features: None,
            activation_pruned: None,
            table: Some(&entry.table),
            dependency: Some(&entry.dependency),
            version: Some(&entry.version),
        }
    }
}

/// Renders `outcome` as pretty-printed JSON with one trailing newline.
///
/// # Errors
///
/// Propagates serialization and write errors from `out`.
pub fn render(
    outcome: &crate::pipeline::Outcome,
    ctx: &RenderContext,
    out: &mut dyn io::Write,
) -> io::Result<()> {
    let build_started = std::time::Instant::now();
    let mut report = Report {
        tool: ctx.tool,
        version: ctx.version,
        features: features_json(outcome.features.as_ref()),
        timings: TimingsJson::from_outcome(outcome),
        counters: outcome.counters.into(),
        rules: rules(outcome),
        violations: violations(outcome),
    };
    let build_millis = build_started.elapsed().as_secs_f64() * 1e3;
    report.timings.report = build_millis;
    report.timings.total += build_millis;

    // `io::Error::from` keeps the underlying I/O error (and its `BrokenPipe` kind) intact;
    // `io::Error::other` would wrap it and defeat the broken-pipe handling in the CLI.
    serde_json::to_writer_pretty(&mut *out, &report).map_err(io::Error::from)?;
    writeln!(out)
}

/// `null` records that no Cargo ran (`--metadata-json`), so the selection that shaped the
/// document is unknown to this process rather than being the default.
fn features_json(features: Option<&FeatureSelection>) -> serde_json::Value {
    match features {
        None => serde_json::Value::Null,
        Some(FeatureSelection::Default) => serde_json::Value::String("default".to_owned()),
        Some(FeatureSelection::All) => serde_json::Value::String("all".to_owned()),
        Some(FeatureSelection::List(features)) => serde_json::Value::Array(
            features.iter().cloned().map(serde_json::Value::String).collect(),
        ),
    }
}

/// One rule's effective feature selection, spelled the way the policy key spells it.
fn selection_json(selection: &Selection) -> serde_json::Value {
    match selection {
        Selection::None => serde_json::Value::String("none".to_owned()),
        Selection::Default => serde_json::Value::String("default".to_owned()),
        Selection::All => serde_json::Value::String("all".to_owned()),
        Selection::List(features) => serde_json::Value::Array(
            features.iter().cloned().map(serde_json::Value::String).collect(),
        ),
    }
}

/// Every rule's record, or `None` when no rule narrowed its closure — see [`RuleJson`] for why
/// the absent case is the one that has to stay absent.
fn rules(outcome: &crate::pipeline::Outcome) -> Option<Vec<RuleJson<'_>>> {
    if outcome.statuses.iter().all(|status| status.features.is_none()) {
        return None;
    }
    Some(
        outcome
            .statuses
            .iter()
            .map(|status| RuleJson {
                id: &status.id,
                kind: status.kind,
                passed: status.passed,
                features: status.features.as_ref().map(selection_json),
                activation_pruned: status
                    .features
                    .as_ref()
                    .map(|_| status.activation_pruned.as_slice()),
            })
            .collect(),
    )
}

fn violations(outcome: &crate::pipeline::Outcome) -> Vec<ViolationJson<'_>> {
    let mut rendered = Vec::new();
    let lookup = super::violation_lookup(outcome);
    for status in &outcome.statuses {
        if status.passed {
            continue;
        }
        if status.kind == manifest::RULE_KIND {
            if let Some(report) = &outcome.manifest {
                rendered.extend(
                    report
                        .entries
                        .iter()
                        .map(|entry| ViolationJson::manifest(entry, &outcome.workspace_root)),
                );
            }
        } else if let Some(violation) = lookup.get(status.id.as_str()).copied() {
            rendered.push(ViolationJson::graph(violation, &outcome.workspace_root));
        }
    }
    rendered
}

fn span_json(span: &Span, workspace_root: &Path) -> SpanJson {
    let file = span.file.strip_prefix(workspace_root).unwrap_or(&span.file);
    SpanJson { file: file.display().to_string(), line: span.line, col: span.col }
}

#[cfg(test)]
#[path = "json_tests.rs"]
mod tests;
