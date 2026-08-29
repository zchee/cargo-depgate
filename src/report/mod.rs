//! Dispatches to the three §1.5 report renderers.

pub mod github;
pub mod human;
pub mod json;

use clap::ValueEnum;
use std::{io, path::PathBuf};

/// The selected diagnostic output format.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[non_exhaustive]
pub enum Format {
    /// Render the human-readable diagnostic report.
    Human,
    /// Render the structured JSON report.
    Json,
    /// Render GitHub Actions annotations followed by the human report.
    Github,
}

/// Shared rendering inputs independent of the outcome itself.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RenderContext {
    /// The workspace root used to relativize source paths.
    pub workspace_root: PathBuf,
    /// The tool name written by structured reporters.
    pub tool: &'static str,
    /// The tool version written by structured reporters.
    pub version: &'static str,
    /// Whether ANSI styling should be emitted by the human reporter.
    pub color: bool,
}

impl RenderContext {
    /// Builds a rendering context; the struct is `#[non_exhaustive]`, so this is the only way
    /// to construct one outside the crate.
    #[must_use]
    pub fn new(
        workspace_root: PathBuf,
        tool: &'static str,
        version: &'static str,
        color: bool,
    ) -> Self {
        Self { workspace_root, tool, version, color }
    }
}

/// Builds a `rule_id -> &Violation` lookup once, shared by every reporter that needs to pair a
/// failed [`crate::rules::RuleStatus`] with its [`crate::rules::Violation`].
#[must_use]
pub(crate) fn violation_lookup(
    outcome: &crate::pipeline::Outcome,
) -> std::collections::HashMap<&str, &crate::rules::Violation> {
    outcome.violations.iter().map(|violation| (violation.rule_id.as_str(), violation)).collect()
}

/// Renders `outcome` in the selected `format`.
///
/// # Errors
///
/// Propagates write errors from `out`.
pub fn render(
    format: Format,
    outcome: &crate::pipeline::Outcome,
    ctx: &RenderContext,
    out: &mut dyn io::Write,
) -> io::Result<()> {
    match format {
        Format::Human => human::render(outcome, ctx, out),
        Format::Json => json::render(outcome, ctx, out),
        Format::Github => github::render(outcome, ctx, out),
    }
}
