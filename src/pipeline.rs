//! End-to-end metadata, configuration, graph, and rule evaluation pipeline.

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use crate::{
    config,
    error::Error,
    graph::{Graph, Scratch},
    manifest::{self, ManifestInput, ManifestReport},
    metadata,
    rules::{self, Evaluation, RuleStatus},
    timings::{Counters, Phase, Timings},
};

/// Everything the `check` pipeline needs beyond the command-line grammar.
#[derive(Clone, Debug)]
pub struct CheckArgs {
    /// Options controlling `cargo metadata` acquisition and rebasing.
    pub metadata: metadata::MetadataOptions,
    /// An explicit `depgate.toml` path, or `None` to discover it after parsing metadata.
    pub config_path: Option<PathBuf>,
}

/// The materialised result of one dependency-policy check.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Outcome {
    /// The pass/fail status of every rule: the graph rules in configuration order,
    /// then the manifest rule when it is enabled (`kind == "manifest"`, empty `package`).
    pub statuses: Vec<RuleStatus>,
    /// The failed graph rules and their witnesses.
    pub violations: Vec<rules::Violation>,
    /// The manifest rule's entries, or `None` when `versions-in-root = false`.
    ///
    /// Kept apart from [`Outcome::violations`] because a manifest entry renders as a
    /// `file:line:col table dependency = "version"` line, not as a witness path.
    pub manifest: Option<ManifestReport>,
    /// Non-fatal configuration diagnostics emitted during validation.
    pub warnings: Vec<String>,
    /// The workspace root reported by `cargo metadata`, for path relativisation.
    pub workspace_root: PathBuf,
    /// Graph, rule, and metadata counters for this run.
    pub counters: Counters,
    /// Per-phase elapsed time measurements for this run.
    pub timings: Timings,
    /// The policy result exit code (`0` for a pass, `1` for violations).
    pub exit: u8,
}

/// Loads configuration and metadata, builds the graph, and evaluates every rule.
///
/// An explicit configuration is loaded and validated once without a graph before
/// metadata acquisition. This phase-A gate ensures malformed configuration cannot
/// spawn Cargo. The same parsed configuration is validated again against the graph
/// after graph construction. When no path is supplied, configuration discovery is
/// deferred until the metadata workspace root is known.
///
/// After the graph rules, the `manifest.versions-in-root` rule runs over every
/// workspace member manifest when it is enabled (the default): it is one more rule
/// in [`Outcome::statuses`] and in `counters.rules`, it fails once however many
/// entries it finds (`counters.violations`), and its entries are returned in
/// [`Outcome::manifest`]. A member manifest that cannot be read or parsed aborts the
/// run with exit code 3 rather than being skipped.
///
/// Warnings are written verbatim to `stderr` as soon as graph validation produces
/// them. A write failure is intentionally best effort because the public error
/// contract has no diagnostic-stream I/O variant.
///
/// # Errors
///
/// Propagates configuration, metadata, and graph errors from the delegated layers.
pub fn check(args: &CheckArgs, stderr: &mut impl Write) -> Result<Outcome, Error> {
    let mut timings = Timings::start();

    let preloaded = if let Some(path) = &args.config_path {
        let raw = config::load(path)?;
        config::validate(&raw, None).map_err(configuration_error)?;
        Some(raw)
    } else {
        None
    };

    let buffer = timings.measure(Phase::Read, || metadata::acquire(&args.metadata))?;
    let meta = timings.measure(Phase::Parse, || metadata::parse(&buffer))?;
    let graph = timings.measure(Phase::Graph, || Graph::build(&meta))?;

    let raw = if let Some(raw) = preloaded {
        raw
    } else {
        let path = config::discover(Path::new(meta.workspace_root.as_ref()));
        config::load(&path)?
    };
    let validated = config::validate(&raw, Some(&graph)).map_err(configuration_error)?;

    for warning in &validated.warnings {
        drop(writeln!(stderr, "{warning}"));
    }

    let mut scratch = Scratch::new(&graph);
    let mut evaluation = timings
        .measure(Phase::Evaluate, || rules::evaluate(&graph, &validated.config, &mut scratch));

    let manifest = if validated.config.manifest_versions_in_root {
        let members = graph
            .members()
            .iter()
            .map(|&node| ManifestInput::new(graph.name(node), graph.manifest_path(node)));
        let report =
            timings.measure(Phase::Manifest, || manifest::check_versions_in_root(members))?;
        evaluation.statuses.push(manifest_status(&report));
        Some(report)
    } else {
        None
    };

    let counters = counters(&graph, &validated, &evaluation);

    timings.measure(Phase::Report, || {});
    timings.finish();

    let exit = u8::from(counters.violations > 0);
    Ok(Outcome {
        statuses: evaluation.statuses,
        violations: evaluation.violations,
        manifest,
        warnings: validated.warnings,
        workspace_root: PathBuf::from(meta.workspace_root.as_ref()),
        counters,
        timings,
        exit,
    })
}

/// The manifest rule as one more [`RuleStatus`]: it fails once, `matched` counts entries.
fn manifest_status(report: &ManifestReport) -> RuleStatus {
    RuleStatus {
        id: manifest::RULE_ID.to_owned(),
        package: String::new(),
        kind: manifest::RULE_KIND,
        passed: report.passed(),
        matched: count(report.entries.len()),
    }
}

fn configuration_error(error: config::ConfigError) -> Error {
    Error::Configuration { message: error.message, span: error.span }
}

fn counters(graph: &Graph<'_>, validated: &config::Validated, evaluation: &Evaluation) -> Counters {
    let mut counters = graph.counters();
    counters.superset_extra_edges = evaluation.superset_extra_edges;
    counters.direct_optional_decls = validated.direct_optional_decls;
    counters.rules = count(evaluation.statuses.len());
    counters.violations = count(evaluation.statuses.iter().filter(|status| !status.passed).count());
    counters.matches = evaluation.matches;
    counters
}

fn count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod tests;
