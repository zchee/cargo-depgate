//! End-to-end metadata, configuration, graph, and rule evaluation pipeline.

use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
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

/// Inputs for `explain <package> <dependency>`.
#[derive(Clone, Debug)]
pub struct ExplainArgs {
    /// Options controlling `cargo metadata` acquisition and rebasing.
    pub metadata: metadata::MetadataOptions,
    /// An explicit `depgate.toml` path, or `None` to discover it after parsing metadata.
    pub config_path: Option<PathBuf>,
    /// The package whose dependency path should be explained.
    pub package: String,
    /// The dependency to locate beneath `package`.
    pub dependency: String,
}

/// The result of `explain`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ExplainOutcome {
    /// Whether the dependency is reachable from the requested package.
    pub reachable: bool,
    /// The requested root package name.
    pub root: String,
    /// The version selected for the requested root package.
    pub root_version: String,
    /// The requested dependency name.
    pub dependency: String,
    /// The root-to-dependency witness hops, empty when `reachable` is false.
    pub path: Vec<rules::WitnessHop>,
}

/// The materialised result of one dependency-policy check.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Outcome {
    /// The pass/fail status of every rule: the graph rules in configuration order,
    /// then the manifest rule when it is enabled (`kind == "manifest"`, `package == None`).
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
    /// Every rule root is a workspace member (Phase B validation guarantees this); the
    /// report layer uses this to prefix a witness path with the rule's own package and
    /// version without holding onto the borrowed Graph after `check()` returns.
    pub member_versions: BTreeMap<String, String>,
    /// The effective feature selection for the JSON reporter and future `explain`/CLI use.
    pub features: config::FeatureSelection,
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
    let evaluation_started = Instant::now();
    let mut evaluation = rules::evaluate(&graph, &validated.config, &mut scratch);
    let evaluation_elapsed = evaluation_started.elapsed();
    timings.add(Phase::Traversals, evaluation.traversal_time);
    timings.add(Phase::Evaluate, evaluation_elapsed.saturating_sub(evaluation.traversal_time));

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
    // This populates library outcomes; cli::run_check safely recomputes it after rendering so
    // its --timings total also includes the report phase.
    timings.finish();

    let exit = u8::from(counters.violations > 0);
    let member_versions = graph
        .members()
        .iter()
        .map(|&node| (graph.name(node).to_owned(), graph.version(node).to_owned()))
        .collect();
    Ok(Outcome {
        statuses: evaluation.statuses,
        violations: evaluation.violations,
        manifest,
        warnings: validated.warnings,
        workspace_root: PathBuf::from(meta.workspace_root.as_ref()),
        counters,
        timings,
        member_versions,
        features: validated.config.features.clone(),
        exit,
    })
}

/// Loads the validated graph and explains one package-to-dependency query.
///
/// Configuration validation intentionally mirrors [`check`]: an explicit file is loaded and
/// validated before metadata acquisition (Phase A), metadata is then acquired, parsed, and
/// graphed, and the discovered or preloaded file is validated again against that graph (Phase B).
/// Although `explain` does not evaluate rules or scan manifests, this keeps configuration errors
/// and the validated `[graph].features` selection consistent with `check` for identical flags.
/// The first node returned for a package name is selected when multiple versions exist, matching
/// the deterministic package order emitted by `cargo metadata`; version disambiguation is out of
/// scope for this query.
///
/// # Errors
///
/// Propagates configuration, metadata, and graph errors. Unknown package or dependency names are
/// reported as [`Error::Configuration`] and therefore map to exit code 2.
pub fn explain(args: &ExplainArgs) -> Result<ExplainOutcome, Error> {
    let preloaded = if let Some(path) = &args.config_path {
        let raw = config::load(path)?;
        config::validate(&raw, None).map_err(configuration_error)?;
        Some(raw)
    } else {
        None
    };

    let buffer = metadata::acquire(&args.metadata)?;
    let meta = metadata::parse(&buffer)?;
    let graph = Graph::build(&meta)?;

    let raw = if let Some(raw) = preloaded {
        raw
    } else {
        let path = config::discover(Path::new(meta.workspace_root.as_ref()));
        config::load(&path)?
    };
    // Validation is deliberately retained even though explain does not evaluate rules: it is the
    // same Phase-B gate as check and materialises the config's graph feature selection.
    let _validated = config::validate(&raw, Some(&graph)).map_err(configuration_error)?;

    let package_name = graph.lookup_name(&args.package).ok_or_else(|| Error::Configuration {
        message: config::unknown_package_message("explain", &args.package),
        span: None,
    })?;
    let root =
        graph.nodes_of_name(package_name).first().copied().ok_or_else(|| Error::Configuration {
            message: config::unknown_package_message("explain", &args.package),
            span: None,
        })?;
    let dependency_name =
        graph.lookup_name(&args.dependency).ok_or_else(|| Error::Configuration {
            message: config::unknown_package_message("explain", &args.dependency),
            span: None,
        })?;
    let root_version = graph.version(root).to_owned();

    let node_path = {
        let mut scratch = Scratch::new(&graph);
        let reach = graph.reach(root, &mut scratch);
        if reach.contains_name(dependency_name) {
            reach.witness_to_name(dependency_name)
        } else {
            None
        }
    };

    let Some(node_path) = node_path else {
        return Ok(ExplainOutcome {
            reachable: false,
            root: args.package.clone(),
            root_version,
            dependency: args.dependency.clone(),
            path: Vec::new(),
        });
    };

    Ok(ExplainOutcome {
        reachable: true,
        root: args.package.clone(),
        root_version,
        dependency: args.dependency.clone(),
        path: rules::witness_hops(&graph, &node_path),
    })
}

/// The manifest rule as one more [`RuleStatus`]: it fails once, `matched` counts entries.
fn manifest_status(report: &ManifestReport) -> RuleStatus {
    RuleStatus {
        id: manifest::RULE_ID.to_owned(),
        package: None,
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
