//! End-to-end metadata, configuration, graph, and rule evaluation pipeline.

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use crate::{
    config,
    error::Error,
    graph::{Graph, Scratch},
    metadata,
    rules::{self, Evaluation},
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
    /// The pass/fail status of each configured graph rule.
    pub statuses: Vec<rules::RuleStatus>,
    /// The failed graph rules and their witnesses.
    pub violations: Vec<rules::Violation>,
    /// Non-fatal configuration diagnostics emitted during validation.
    pub warnings: Vec<String>,
    /// Graph, rule, and metadata counters for this run.
    pub counters: Counters,
    /// Per-phase elapsed time measurements for this run.
    pub timings: Timings,
    /// The policy result exit code (`0` for a pass, `1` for violations).
    pub exit: u8,
}

/// Loads configuration and metadata, builds the graph, and evaluates graph rules.
///
/// An explicit configuration is loaded and validated once without a graph before
/// metadata acquisition. This phase-A gate ensures malformed configuration cannot
/// spawn Cargo. The same parsed configuration is validated again against the graph
/// after graph construction. When no path is supplied, configuration discovery is
/// deferred until the metadata workspace root is known.
///
/// The `manifest.versions-in-root` rule remains a P3 stub. Enabling it returns
/// [`Error::ManifestRuleNotYetImplemented`] before graph-rule evaluation; disabling it is the
/// only way to reach the P2 evaluator.
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

    if validated.config.manifest_versions_in_root {
        return Err(Error::ManifestRuleNotYetImplemented);
    }

    let mut scratch = Scratch::new(&graph);
    let evaluation = timings
        .measure(Phase::Evaluate, || rules::evaluate(&graph, &validated.config, &mut scratch));
    let counters = counters(&graph, &validated, &evaluation);

    timings.measure(Phase::Report, || {});
    timings.finish();

    let exit = u8::from(!evaluation.violations.is_empty());
    Ok(Outcome {
        statuses: evaluation.statuses,
        violations: evaluation.violations,
        warnings: validated.warnings,
        counters,
        timings,
        exit,
    })
}

fn configuration_error(error: config::ConfigError) -> Error {
    Error::Configuration { message: error.message, span: error.span }
}

fn counters(graph: &Graph<'_>, validated: &config::Validated, evaluation: &Evaluation) -> Counters {
    let mut counters = graph.counters();
    counters.superset_extra_edges = evaluation.superset_extra_edges;
    counters.direct_optional_decls = validated.direct_optional_decls;
    // P3: +1 for the manifest rule
    counters.rules = count(evaluation.statuses.len());
    counters.violations = count(evaluation.violations.len());
    counters.matches = evaluation.matches;
    counters
}

fn count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod tests;
