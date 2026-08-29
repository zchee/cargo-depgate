//! Error types and process exit-code mappings.

use std::{io, path::PathBuf, time::Duration};

/// An error produced while evaluating or preparing a dependency policy command.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Dependency-graph policy violations were found.
    #[error("found {count} dependency policy violation(s)")]
    PolicyViolations {
        /// The number of policy violations found.
        count: usize,
    },

    /// The supplied configuration is invalid or could not be loaded.
    #[error("configuration error: {message}")]
    Configuration {
        /// A description of the configuration problem.
        message: String,
        /// The source location of the configuration problem, when available.
        span: Option<crate::config::Span>,
    },

    /// Writing the rendered report, `explain` output, or configuration schema failed.
    #[error("failed to write command output")]
    ReportWrite {
        /// The operating-system error returned while writing.
        #[source]
        source: io::Error,
    },

    /// The requested command or arguments are invalid.
    #[error("usage error: {message}")]
    Usage {
        /// A description of the usage problem.
        message: String,
    },

    /// Temporary P0 scaffolding that P1, P2, and P4 of the implementation plan delete once
    /// `check`, `explain`, and `schema` gain real behavior.
    #[error("the {subcommand} subcommand is not implemented yet")]
    NotYetImplemented {
        /// The subcommand whose implementation has not landed yet.
        subcommand: String,
    },

    /// Spawning the `cargo metadata` child process failed.
    #[error("failed to spawn cargo metadata")]
    CargoMetadataSpawn {
        /// The operating-system error returned while spawning the child process.
        #[source]
        source: io::Error,
    },

    /// The `cargo metadata` child process exceeded its allowed runtime.
    #[error("cargo metadata exceeded --cargo-timeout={}s", timeout.as_secs())]
    CargoMetadataTimeout {
        /// The maximum runtime allowed for the child process.
        timeout: Duration,
    },

    /// Reading the piped standard output of the `cargo metadata` child process failed.
    #[error("failed to read cargo metadata output")]
    CargoMetadataRead {
        /// The operating-system error returned while reading the pipe.
        #[source]
        source: io::Error,
    },

    /// The `cargo metadata` child process exited unsuccessfully.
    #[error(
        "cargo metadata {}",
        status.map_or_else(
            || "was terminated by a signal".to_owned(),
            |code| format!("exited with status {code}"),
        )
    )]
    CargoMetadataFailed {
        /// The process exit code, or `None` if the process ended without one.
        status: Option<i32>,
    },

    /// The `cargo metadata` output was not valid expected JSON.
    #[error("failed to parse cargo metadata output")]
    CargoMetadataUnparseable {
        /// The JSON parsing error returned for the metadata output.
        #[source]
        source: serde_json::Error,
    },

    /// Reading precomputed `cargo metadata` JSON (`--metadata-json`) failed.
    #[error("failed to read cargo metadata JSON from {}", path.display())]
    MetadataRead {
        /// The file that was being read; `-` names standard input.
        path: PathBuf,
        /// The operating-system error returned while reading.
        #[source]
        source: io::Error,
    },

    /// Reading a workspace member manifest for the `manifest.versions-in-root` rule failed.
    #[error("failed to read workspace member manifest {}", path.display())]
    ManifestRead {
        /// The member manifest that was being read.
        path: PathBuf,
        /// The operating-system error returned while reading.
        #[source]
        source: io::Error,
    },

    /// A workspace member manifest is not valid TOML, or a dependency entry has a shape
    /// Cargo would reject (neither a string nor a table).
    #[error("failed to parse workspace member manifest {}", path.display())]
    ManifestParse {
        /// The member manifest that was being parsed.
        path: PathBuf,
        /// The TOML error, which renders the offending line and column.
        #[source]
        source: toml::de::Error,
    },

    /// The metadata parsed, but violates an invariant the policy engine relies on.
    ///
    /// These are the fail-closed input checks: a missing `resolve`, an edge without
    /// `dep_kinds`, an unresolvable package id, an empty member list, a node/package
    /// mismatch, a duplicate id, or a workspace member manifest that cannot be rebased
    /// under `--workspace-root`. Silently skipping any of them could let a containment
    /// rule pass vacuously.
    #[error("invalid cargo metadata: {message}")]
    MetadataInvalid {
        /// A description of the violated invariant.
        message: String,
    },
}

impl Error {
    /// Returns the process exit code this error maps to.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::PolicyViolations { .. } => 1,
            Self::Configuration { .. } | Self::Usage { .. } | Self::NotYetImplemented { .. } => 2,
            Self::ReportWrite { .. } => 4,
            Self::CargoMetadataSpawn { .. }
            | Self::CargoMetadataTimeout { .. }
            | Self::CargoMetadataRead { .. }
            | Self::CargoMetadataFailed { .. }
            | Self::CargoMetadataUnparseable { .. }
            | Self::MetadataRead { .. }
            | Self::MetadataInvalid { .. }
            | Self::ManifestRead { .. }
            | Self::ManifestParse { .. } => 3,
        }
    }
}

/// Returns the process exit code for a command result.
///
/// Successful commands map to zero; errors use [`Error::exit_code`].
#[must_use]
pub fn exit_code_for(result: &Result<(), Error>) -> u8 {
    result.as_ref().map_or_else(Error::exit_code, |()| 0)
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
