//! Command-line grammar and command dispatch.

use std::{
    collections::HashMap,
    ffi::OsString,
    io::{self, Write},
    path::PathBuf,
    time::Duration,
};

use clap::{ArgAction, Parser, Subcommand, ValueEnum, builder::TypedValueParser as _};
use clap_cargo::{Features, Manifest, style::CLAP_STYLING};
use schemars::schema_for;

use crate::{
    config::ConfigSchema,
    error::Error,
    metadata::{DEFAULT_TIMEOUT_SECS, MetadataOptions},
    pipeline,
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
    format: Option<Format>,

    /// Report command timings.
    #[arg(long)]
    timings: bool,

    /// Increase diagnostic verbosity; repeat for more detail.
    #[arg(short = 'v', action = ArgAction::Count)]
    verbose: u8,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Format {
    Human,
    Json,
    Github,
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
/// `check` uses the P2 pipeline and emits its plain placeholder report. The
/// `--format` value is accepted by the grammar but intentionally ignored until
/// the P4 human, JSON, and GitHub reporters land. `schema` prints the generated
/// configuration schema; `explain` remains a named P0 stub.
///
/// # Errors
///
/// Returns pipeline errors unchanged. A completed check with violations is
/// converted to [`Error::PolicyViolations`] so the process receives exit code 1.
pub fn run(args: &Args) -> Result<(), Error> {
    match &args.command {
        None => run_check(&args.check),
        Some(Command::Check(common)) => run_check(common),
        Some(Command::Explain(_)) => {
            Err(Error::NotYetImplemented { subcommand: "explain".to_owned() })
        }
        Some(Command::Schema) => run_schema(),
    }
}

fn run_check(common: &CommonArgs) -> Result<(), Error> {
    let check_args = pipeline::CheckArgs {
        metadata: common.metadata_options(),
        config_path: common.config.clone(),
    };
    let mut stderr = io::stderr();
    let outcome = pipeline::check(&check_args, &mut stderr)?;

    let mut stdout = io::stdout();
    render_plain_report(&outcome, &mut stdout);
    if common.timings {
        drop(outcome.timings.write_to(&outcome.counters, &mut stderr));
    }

    if outcome.exit == 0 {
        Ok(())
    } else {
        Err(Error::PolicyViolations { count: outcome.violations.len() })
    }
}

fn run_schema() -> Result<(), Error> {
    let schema = schema_for!(ConfigSchema).to_value();
    let rendered =
        serde_json::to_string_pretty(&schema).map_err(|source| Error::Configuration {
            message: format!("failed to serialize configuration schema: {source}"),
            span: None,
        })?;
    let mut stdout = io::stdout();
    drop(writeln!(stdout, "{rendered}"));
    Ok(())
}

fn render_plain_report(outcome: &pipeline::Outcome, out: &mut impl Write) {
    let violations = outcome
        .violations
        .iter()
        .map(|violation| (violation.rule_id.as_str(), violation))
        .collect::<HashMap<_, _>>();

    for status in &outcome.statuses {
        if status.passed {
            drop(writeln!(out, "ok {}", status.id));
            continue;
        }

        let violation = violations.get(status.id.as_str()).copied();
        match (status.kind, violation) {
            ("internal" | "leaf" | "direct", Some(violation)) => drop(writeln!(
                out,
                "FAIL {}: {} match(es), +{} extra, -{} missing",
                status.id,
                status.matched,
                violation.extra.len(),
                violation.missing.len()
            )),
            ("sealed", Some(violation)) => drop(writeln!(
                out,
                "FAIL {}: consumed by {} member(s)",
                status.id,
                violation.sealed_by.len()
            )),
            _ => drop(writeln!(out, "FAIL {}: {} match(es)", status.id, status.matched)),
        }
    }

    if outcome.violations.is_empty() {
        drop(writeln!(
            out,
            "ok: {} rules, {} violations",
            outcome.statuses.len(),
            outcome.violations.len()
        ));
    } else {
        drop(writeln!(
            out,
            "FAIL: {} rules, {} violations",
            outcome.statuses.len(),
            outcome.violations.len()
        ));
    }
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
