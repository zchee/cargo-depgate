//! Command-line grammar and P0 command dispatch.

use std::{ffi::OsString, path::PathBuf};

use clap::{ArgAction, Parser, Subcommand, ValueEnum, builder::TypedValueParser as _};
use clap_cargo::{Features, Manifest, style::CLAP_STYLING};

use crate::error::Error;

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
        match &self.command {
            Some(Command::Check(args)) => args.locked(),
            Some(Command::Explain(args)) => args.common.locked(),
            Some(Command::Schema) => true,
            None => self.check.locked(),
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
struct CommonArgs {
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

    /// Maximum number of seconds allowed for `cargo metadata`.
    #[arg(long, value_name = "SECS", default_value_t = 300)]
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
/// # Errors
///
/// P0 returns [`Error::NotYetImplemented`] for every subcommand. P1, P2, and
/// P4 replace these stubs with the corresponding behavior.
pub fn run(args: &Args) -> Result<(), Error> {
    let subcommand = match &args.command {
        None | Some(Command::Check(_)) => "check",
        Some(Command::Explain(_)) => "explain",
        Some(Command::Schema) => "schema",
    };

    Err(Error::NotYetImplemented { subcommand: subcommand.to_owned() })
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
