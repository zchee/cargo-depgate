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
    /// The feature selection the graph was actually resolved with, or `None` when it is
    /// unknowable because no Cargo ran (`--metadata-json`; the document carries its own).
    ///
    /// This is the *effective* selection, not the configured one: a command-line
    /// `--features`/`--all-features` overrides `[graph].features`, so recording the file's
    /// value would misreport exactly the "released with `--features cloud`, gated on default"
    /// drift the field exists to expose.
    pub features: Option<config::FeatureSelection>,
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

    let (preloaded, metadata_options) =
        preload_config(args.config_path.as_deref(), &args.metadata)?;

    let buffer = timings.measure(Phase::Read, || metadata::acquire(&metadata_options))?;
    let meta = timings.measure(Phase::Parse, || metadata::parse(&buffer))?;
    let graph = timings.measure(Phase::Graph, || Graph::build(&meta))?;

    let explicit = preloaded.is_some();
    let raw = if let Some(raw) = preloaded {
        raw
    } else {
        let path = config::discover(Path::new(meta.workspace_root.as_ref()));
        config::load(&path)?
    };
    let validated = config::validate(&raw, Some(&graph)).map_err(configuration_error)?;
    let feature_warning =
        feature_selection_after_metadata(explicit, &args.metadata, &validated.config.features)?;

    for warning in feature_warning.iter().chain(&validated.warnings) {
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
        features: effective_features(&metadata_options, &validated.config.features),
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
pub fn explain(args: &ExplainArgs, stderr: &mut impl Write) -> Result<ExplainOutcome, Error> {
    let (preloaded, metadata_options) =
        preload_config(args.config_path.as_deref(), &args.metadata)?;

    let buffer = metadata::acquire(&metadata_options)?;
    let meta = metadata::parse(&buffer)?;
    let graph = Graph::build(&meta)?;

    let explicit = preloaded.is_some();
    let raw = if let Some(raw) = preloaded {
        raw
    } else {
        let path = config::discover(Path::new(meta.workspace_root.as_ref()));
        config::load(&path)?
    };
    // Validation is deliberately retained even though explain does not evaluate rules: it is the
    // same Phase-B gate as check and materialises the config's graph feature selection.
    let validated = config::validate(&raw, Some(&graph)).map_err(configuration_error)?;
    // Same diagnostics as `check` for identical flags: the discovered-config feature rule is an
    // error (exit 2) and the `--metadata-json` warning is written to the caller's stderr.
    if let Some(warning) =
        feature_selection_after_metadata(explicit, &args.metadata, &validated.config.features)?
    {
        drop(writeln!(stderr, "{warning}"));
    }

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

/// Loads and Phase-A-validates an explicit `--config` file before cargo is spawned and derives
/// the spawn options from it: `[graph].features` reaches the `cargo metadata` command **only**
/// through this pre-spawn path (§1.3). Without `--config` nothing is loaded here — the discovered
/// file is found at `workspace_root`, which exists only after metadata (no walk-up from cwd, no
/// second cargo spawn).
fn preload_config(
    config_path: Option<&Path>,
    metadata: &metadata::MetadataOptions,
) -> Result<(Option<config::RawConfig>, metadata::MetadataOptions), Error> {
    let Some(path) = config_path else {
        return Ok((None, metadata.clone()));
    };
    let raw = config::load(path)?;
    let validated = config::validate(&raw, None).map_err(configuration_error)?;
    let options = spawn_options(metadata, &validated.config.features);
    Ok((Some(raw), options))
}

/// Applies a config's `[graph].features` to the metadata command unless the CLI already selected
/// features (CLI flags override, §1.3) or no cargo runs (`--metadata-json`).
pub(crate) fn spawn_options(
    base: &metadata::MetadataOptions,
    features: &config::FeatureSelection,
) -> metadata::MetadataOptions {
    let mut options = base.clone();
    if options.source.is_some() || cli_selected_features(base) {
        return options;
    }
    match features {
        config::FeatureSelection::Default => {}
        config::FeatureSelection::All => options.all_features = true,
        config::FeatureSelection::List(list) => options.features.clone_from(list),
    }
    options
}

/// The feature selection the graph was actually resolved with, read back from the options that
/// reached `cargo metadata` after [`spawn_options`] merged `[graph].features` into them.
///
/// `--all-features` (or `features = "all"`) is [`config::FeatureSelection::All`], a non-empty
/// list is [`config::FeatureSelection::List`], and anything else leaves `configured` in force —
/// which at this point can only be the default, since a non-default *discovered* selection is
/// rejected before the graph is evaluated ([`feature_selection_after_metadata`]).
///
/// `None` means "no Cargo ran": under `--metadata-json` the document was resolved elsewhere with
/// a selection this process cannot observe, so reporting any value would be a guess.
///
/// A bare `--no-default-features` is not represented. Cargo combines it with the selection rather
/// than replacing it, and the enum has no variant for that combination; the reported value stays
/// the selection it was combined with.
pub(crate) fn effective_features(
    options: &metadata::MetadataOptions,
    configured: &config::FeatureSelection,
) -> Option<config::FeatureSelection> {
    if options.source.is_some() {
        return None;
    }
    if options.all_features {
        return Some(config::FeatureSelection::All);
    }
    if options.features.is_empty() {
        return Some(configured.clone());
    }
    Some(config::FeatureSelection::List(options.features.clone()))
}

/// Whether the CLI made a feature *selection* that supersedes `[graph].features`.
///
/// A bare `--no-default-features` does not: cargo combines it with `--features …` rather than
/// replacing the selection, so it passes through to the spawn and the config's list still applies.
fn cli_selected_features(options: &metadata::MetadataOptions) -> bool {
    !options.features.is_empty() || options.all_features
}

/// The fate of a non-default `[graph].features` once metadata already exists: under
/// `--metadata-json` it is ignored with a warning (the JSON was produced with its own features);
/// in a *discovered* `depgate.toml` it cannot have reached the spawn, so it is an error (exit 2)
/// rather than a silently different graph — pass `--config` or the CLI feature flags (D12).
pub(crate) fn feature_selection_after_metadata(
    explicit_config: bool,
    metadata: &metadata::MetadataOptions,
    features: &config::FeatureSelection,
) -> Result<Option<String>, Error> {
    if matches!(features, config::FeatureSelection::Default) || cli_selected_features(metadata) {
        return Ok(None);
    }
    if metadata.source.is_some() {
        return Ok(Some(
            "warning: [graph].features is ignored under --metadata-json; the JSON was produced \
             with its own feature selection"
                .to_owned(),
        ));
    }
    if explicit_config {
        return Ok(None);
    }
    Err(Error::Configuration {
        message: "[graph].features in a discovered depgate.toml cannot select features for this \
                  run: the file is found only after `cargo metadata`; pass --config <path> or \
                  --features/--all-features"
            .to_owned(),
        span: None,
    })
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
