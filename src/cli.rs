//! Command-line grammar and command dispatch.

use std::{
    env,
    ffi::OsString,
    io::{self, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

use clap::{Parser, Subcommand, builder::TypedValueParser as _};
use clap_cargo::{Features, Manifest, style::CLAP_STYLING};
use schemars::schema_for;

use crate::{
    config::ConfigSchema,
    error::Error,
    metadata::{DEFAULT_TIMEOUT_SECS, MetadataOptions},
    pipeline,
    report::{self, Format as ReportFormat, RenderContext},
};

/// Parsed command-line arguments for `cargo depgate`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
#[command(
    name = "cargo-depgate",
    bin_name = "cargo depgate",
    display_name = "cargo-depgate",
    version,
    about = env!("CARGO_PKG_DESCRIPTION"),
    long_about = None,
    styles = CLAP_STYLING,
    args_conflicts_with_subcommands = true
)]
pub struct Args {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    check: CommonArgs,
}

impl Args {
    /// Returns whether Cargo's lockfile must remain unchanged for this command.
    ///
    /// This defaults to `true`; `--no-locked` changes it to `false`.
    #[must_use]
    pub fn locked(&self) -> bool {
        self.common_args().is_none_or(CommonArgs::locked)
    }

    /// The metadata acquisition options for this command; `None` for `schema`,
    /// which never touches cargo.
    #[must_use]
    pub fn metadata_options(&self) -> Option<MetadataOptions> {
        self.common_args().map(CommonArgs::metadata_options)
    }

    /// The shared flags of `check`/`explain`; `None` for `schema`.
    pub(crate) fn common_args(&self) -> Option<&CommonArgs> {
        match &self.command {
            Some(Command::Check(args)) => Some(args),
            Some(Command::Explain(args)) => Some(&args.common),
            Some(Command::Schema) => None,
            None => Some(&self.check),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
enum Command {
    /// Check the dependency graph against the configured policy.
    Check(CommonArgs),

    /// Explain why one package depends on another.
    Explain(ExplainArgs),

    /// Print the configuration schema.
    Schema,
}

#[derive(Clone, Debug, Eq, PartialEq, clap::Args)]
struct ExplainArgs {
    /// Package whose dependency path should be explained.
    package: String,

    /// Dependency to locate beneath the package.
    dependency: String,

    #[command(flatten)]
    common: CommonArgs,
}

#[derive(Clone, Debug, Eq, PartialEq, clap::Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the CLI grammar intentionally represents independent boolean flags"
)]
pub(crate) struct CommonArgs {
    #[command(flatten)]
    manifest: Manifest,

    #[command(flatten)]
    features: Features,

    /// Path to the dependency-policy configuration file.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// The source of precomputed `cargo metadata` JSON (`-` reads standard input).
    #[arg(
        long,
        value_name = "FILE",
        value_parser = clap::builder::OsStringValueParser::new().map(MetadataSource::from)
    )]
    metadata_json: Option<MetadataSource>,

    /// Override the workspace root for precomputed metadata.
    #[arg(long, value_name = "DIR", requires = "metadata_json")]
    workspace_root: Option<PathBuf>,

    /// Request offline Cargo operation.
    #[arg(long)]
    offline: bool,

    /// Require Cargo.lock to remain unchanged.
    #[arg(long = "locked", conflicts_with = "no_locked")]
    locked_flag: bool,

    /// Permit Cargo.lock to change.
    #[arg(long, conflicts_with = "locked_flag")]
    no_locked: bool,

    /// Maximum number of seconds allowed for `cargo metadata` (at least 1).
    #[arg(
        long,
        value_name = "SECS",
        default_value_t = DEFAULT_TIMEOUT_SECS,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    cargo_timeout: u64,

    /// Select the diagnostic output format.
    #[arg(long, value_enum)]
    format: Option<ReportFormat>,

    /// Report command timings.
    #[arg(long)]
    timings: bool,
}

impl CommonArgs {
    /// `--locked` names the default; only `--no-locked` changes it.
    const fn locked(&self) -> bool {
        !self.no_locked
    }

    /// Projects the cargo-facing flags onto [`MetadataOptions`].
    ///
    /// `--features` entries are forwarded verbatim; `--offline` and `--cargo-timeout`
    /// are carried even under `--metadata-json`, where [`crate::metadata::acquire`]
    /// leaves them inert.
    pub(crate) fn metadata_options(&self) -> MetadataOptions {
        MetadataOptions {
            cargo: None,
            manifest_path: self.manifest.manifest_path.clone(),
            features: self.features.features.clone(),
            all_features: self.features.all_features,
            no_default_features: self.features.no_default_features,
            offline: self.offline,
            locked: self.locked(),
            timeout: Duration::from_secs(self.cargo_timeout),
            source: self.metadata_json.clone(),
            workspace_root: self.workspace_root.clone(),
        }
    }
}

/// The source of precomputed `cargo metadata` JSON.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MetadataSource {
    /// Read metadata JSON from standard input.
    Stdin,
    /// Read metadata JSON from the given file.
    File(PathBuf),
}

impl From<OsString> for MetadataSource {
    /// `-` selects standard input; anything else (including non-UTF-8 paths) is a file path.
    fn from(value: OsString) -> Self {
        if value == "-" { Self::Stdin } else { Self::File(PathBuf::from(value)) }
    }
}

/// Parses direct `cargo-depgate` and Cargo-plugin `cargo depgate` arguments.
///
/// A `depgate` token immediately following the executable name is removed
/// before clap parses the remaining arguments.
///
/// # Errors
///
/// Returns clap's structured error for invalid arguments, help, or version
/// requests.
pub fn parse_from<I, T>(argv: I) -> Result<Args, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let mut argv: Vec<OsString> = argv.into_iter().map(Into::into).collect();
    if argv.is_empty() {
        argv.push(OsString::from("cargo-depgate"));
    }

    if argv.get(1).is_some_and(|argument| argument == "depgate") {
        argv.remove(1);
    }

    Args::try_parse_from(argv)
}

/// Runs a parsed command.
///
/// `check` evaluates the configured policy and renders the selected human, JSON, or GitHub
/// report. `explain` resolves one dependency witness without applying policy rules, while
/// `schema` prints the generated configuration schema.
///
/// # Errors
///
/// Returns pipeline errors unchanged. A completed check with violations is
/// converted to [`Error::PolicyViolations`] so the process receives exit code 1.
pub fn run(args: &Args) -> Result<(), Error> {
    match &args.command {
        None => run_check(&args.check),
        Some(Command::Check(common)) => run_check(common),
        Some(Command::Explain(explain)) => run_explain(explain),
        Some(Command::Schema) => run_schema(),
    }
}

fn run_check(common: &CommonArgs) -> Result<(), Error> {
    let mut stderr = io::stderr();
    warn_if_flags_ignored(common, &mut stderr);

    let check_args = pipeline::CheckArgs {
        metadata: common.metadata_options(),
        config_path: common.config.clone(),
    };
    let mut outcome = pipeline::check(&check_args, &mut stderr)?;

    let mut stdout = anstream::stdout();
    // The environment is read here rather than in the reporter so that the GitHub
    // annotation anchor is an explicit input a test can supply.
    let context = RenderContext::new(
        outcome.workspace_root.clone(),
        "cargo-depgate",
        env!("CARGO_PKG_VERSION"),
        stdout.current_choice() != anstream::ColorChoice::Never,
    )
    .with_github_workspace(env::var_os("GITHUB_WORKSPACE").map(PathBuf::from));
    let started = Instant::now();
    let render_result =
        report::render(resolve_format(common.format), &outcome, &context, &mut stdout);
    outcome.timings.add(crate::timings::Phase::Report, started.elapsed());
    outcome.timings.finish();
    if common.timings {
        // The diagnostic stream is deliberately best effort: the public error contract has no
        // diagnostic-stream I/O variant, unlike the primary report output below.
        drop(outcome.timings.write_to(&outcome.counters, &mut stderr));
    }
    write_report_result(render_result)?;

    if outcome.exit == 0 {
        Ok(())
    } else {
        Err(Error::PolicyViolations { count: outcome.counters.violations as usize })
    }
}

fn run_explain(explain: &ExplainArgs) -> Result<(), Error> {
    let mut stderr = io::stderr();
    warn_if_flags_ignored(&explain.common, &mut stderr);

    let args = pipeline::ExplainArgs {
        metadata: explain.common.metadata_options(),
        config_path: explain.common.config.clone(),
        package: explain.package.clone(),
        dependency: explain.dependency.clone(),
    };
    let outcome = pipeline::explain(&args, &mut stderr)?;
    let format = resolve_format(explain.common.format);
    let mut stdout = anstream::stdout();

    let write_result = match format {
        report::Format::Json => {
            #[derive(serde::Serialize)]
            struct ExplainHop {
                name: String,
                version: String,
                target: Option<String>,
                optional: bool,
            }

            #[derive(serde::Serialize)]
            struct ExplainReport {
                reachable: bool,
                path: Vec<ExplainHop>,
            }

            let mut path = Vec::with_capacity(outcome.path.len() + usize::from(outcome.reachable));
            if outcome.reachable {
                path.push(ExplainHop {
                    name: outcome.root.clone(),
                    version: outcome.root_version.clone(),
                    target: None,
                    optional: false,
                });
                path.extend(outcome.path.iter().map(|hop| ExplainHop {
                    name: hop.name.clone(),
                    version: hop.version.clone(),
                    target: hop.target.clone(),
                    optional: hop.optional,
                }));
            }
            let report = ExplainReport { reachable: outcome.reachable, path };
            // `io::Error::from` preserves a `BrokenPipe` kind so `| head` stays a policy exit.
            serde_json::to_writer_pretty(&mut stdout, &report)
                .map_err(io::Error::from)
                .and_then(|()| writeln!(stdout))
        }
        report::Format::Human | report::Format::Github => {
            // GitHub annotations have no natural shape for reachability queries, so both formats
            // intentionally use the same human-readable witness output.
            if outcome.reachable {
                let witness = report::human::render_witness(
                    &outcome.root,
                    Some(&outcome.root_version),
                    &outcome.path,
                    &[],
                );
                writeln!(stdout, "{witness}")
            } else {
                writeln!(stdout, "not reachable")
            }
        }
    };

    write_report_result(write_result)
}

fn run_schema() -> Result<(), Error> {
    let schema = schema_for!(ConfigSchema).to_value();
    let rendered =
        serde_json::to_string_pretty(&schema).map_err(|source| Error::Configuration {
            message: format!("failed to serialize configuration schema: {source}"),
            span: None,
        })?;
    let mut stdout = anstream::stdout();
    write_report_result(writeln!(stdout, "{rendered}"))
}

fn resolve_format(explicit: Option<ReportFormat>) -> ReportFormat {
    explicit.unwrap_or_else(|| {
        if std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true") {
            ReportFormat::Github
        } else {
            ReportFormat::Human
        }
    })
}

/// Converts a write result into the CLI error contract.
///
/// A broken pipe (e.g. the reader end of a `| head` pipeline closing early) is treated as
/// intentional and swallowed: the caller falls through to its ordinary success/failure outcome
/// instead of reporting a spurious write failure. Any other I/O error becomes
/// [`Error::ReportWrite`] (exit code 4), so a truncated report from a genuine write failure is
/// never mistaken for a passing or failing policy result.
fn write_report_result(result: io::Result<()>) -> Result<(), Error> {
    match result {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(source) => Err(Error::ReportWrite { source }),
    }
}

/// Warns about flags that `--metadata-json` renders inert.
///
/// Both are silent failure modes otherwise: the lock is enforced by whoever produced the
/// document, and the feature flags shaped nothing, so a user who passes them only to the gate
/// (and not to the `cargo metadata` that generated the JSON) would gate a default-features
/// resolve without any diagnostic. `--offline` and `--cargo-timeout` are deliberately not
/// warned about: they are inert but harmless, and CI templates pass them uniformly.
fn warn_if_flags_ignored(common: &CommonArgs, stderr: &mut impl Write) {
    if common.metadata_json.is_none() {
        return;
    }
    if common.locked_flag || common.no_locked {
        let _ = writeln!(
            stderr,
            "warning: --locked is ignored under --metadata-json; the JSON may predate Cargo.lock"
        );
    }
    let mut flags = Vec::new();
    if !common.features.features.is_empty() {
        flags.push("--features");
    }
    if common.features.all_features {
        flags.push("--all-features");
    }
    if common.features.no_default_features {
        flags.push("--no-default-features");
    }
    if !flags.is_empty() {
        let _ = writeln!(
            stderr,
            "warning: {} ignored under --metadata-json; the JSON was produced with its own \
             feature selection",
            flags.join(", ")
        );
    }
}

/// Renders a configuration error for the binary entry point.
///
/// Readable source spans use the human reporter's annotate-snippets diagnostic. If the span is
/// absent or its source cannot be reconstructed, the output falls back to a single `error:` line.
///
/// # Errors
///
/// Returns an I/O error when writing the diagnostic to `out` fails.
pub fn render_configuration_error(
    message: &str,
    span: Option<&crate::config::Span>,
    color: bool,
    out: &mut dyn Write,
) -> io::Result<()> {
    if let Some(span) = span
        && let Some(rendered) = report::human::render_config_snippet(message, span, color)
    {
        return writeln!(out, "{rendered}");
    }
    writeln!(out, "error: {message}")
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
