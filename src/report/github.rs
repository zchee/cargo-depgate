//! GitHub Actions error annotations followed by the complete human report.

use std::{fs, io, path::Path};

use crate::{manifest, rules::Violation};

use super::{RenderContext, human};

const ANNOTATION_LIMIT: usize = 10;

/// Renders at most ten GitHub error annotations followed by the human report.
///
/// Graph-rule failures retain declaration order and take priority over manifest
/// entries when the annotation cap is reached. Annotation paths are anchored at
/// [`RenderContext::github_workspace`] when it is set and contains the workspace root,
/// and at the workspace root otherwise.
///
/// # Errors
///
/// Propagates write errors from `out`.
pub fn render(
    outcome: &crate::pipeline::Outcome,
    ctx: &RenderContext,
    out: &mut dyn io::Write,
) -> io::Result<()> {
    let lookup = super::violation_lookup(outcome);
    let graph_candidates = outcome
        .statuses
        .iter()
        .filter(|status| !status.passed && status.kind != manifest::RULE_KIND)
        .filter_map(|status| {
            let violation = lookup.get(status.id.as_str()).copied()?;
            let label = human::violation_label(status, Some(violation));
            let mut message = format!("{}: {label}", status.id);
            if let Some(witness) = first_witness(violation, outcome) {
                message.push_str(" — ");
                message.push_str(&witness);
            }
            Some((&violation.span.file, violation.span.line, violation.span.col, message))
        });

    let manifest_candidates =
        outcome.manifest.iter().flat_map(|report| &report.entries).map(|entry| {
            let message = format!(
                "{}: {} {} = {:?}",
                manifest::RULE_ID,
                entry.table,
                entry.dependency,
                entry.version
            );
            (&entry.span.file, entry.span.line, entry.span.col, message)
        });

    for (file, line, col, message) in
        graph_candidates.chain(manifest_candidates).take(ANNOTATION_LIMIT)
    {
        let relative_file =
            display_path(file, &outcome.workspace_root, ctx.github_workspace.as_deref())
                .replace('\\', "/");
        writeln!(
            out,
            "::error file={},line={line},col={col}::{}",
            escape_property(&relative_file),
            escape(&message)
        )?;
    }

    human::render(outcome, ctx, out)
}

fn first_witness(violation: &Violation, outcome: &crate::pipeline::Outcome) -> Option<String> {
    if let Some(matched) = violation.matches.first().or_else(|| violation.extra.first()) {
        let root_version = outcome.member_versions.get(&violation.package).map(String::as_str);
        return Some(human::render_witness(
            &violation.package,
            root_version,
            &matched.witness,
            &matched.other_versions,
        ));
    }

    // A sealed witness is version-free everywhere (AC 4's literal form), so the annotation
    // line and the human report body below it render the same text.
    violation
        .sealed_by
        .first()
        .map(|entry| human::render_witness_versionless(&entry.member, &entry.witness))
}

/// Renders one annotation path the way GitHub Actions resolves it.
///
/// Actions anchors `file=` at the repository checkout, not at the Cargo workspace, and the two
/// differ whenever the workspace lives in a repository subdirectory. When `$GITHUB_WORKSPACE`
/// is known and contains the workspace root, the path is emitted relative to it; otherwise it
/// stays relative to the workspace root, which is the only anchor available off Actions.
///
/// `cargo metadata` reports a canonical `workspace_root`, so the containment test resolves
/// both sides first: a `$GITHUB_WORKSPACE` that traverses a symlink or carries a `..` fails a
/// purely lexical component comparison and would silently fall back to the workspace anchor.
/// When either side cannot be resolved — a path that does not exist, or one the process
/// cannot stat — the lexical comparison stands, which is the pre-canonicalisation behaviour.
fn display_path(path: &Path, workspace_root: &Path, github_workspace: Option<&Path>) -> String {
    if let Some(repository_root) = github_workspace
        && contains(repository_root, workspace_root)
        && let Some(relative) = strip_prefix_resolved(path, repository_root)
    {
        return relative;
    }
    path.strip_prefix(workspace_root).unwrap_or(path).display().to_string()
}

/// Whether `root` contains `descendant`, comparing the canonical form of both when both
/// resolve and the paths as written when either does not.
fn contains(root: &Path, descendant: &Path) -> bool {
    if let (Ok(root), Ok(descendant)) = (fs::canonicalize(root), fs::canonicalize(descendant)) {
        return descendant.starts_with(root);
    }
    descendant.starts_with(root)
}

/// Renders `path` relative to `root`, resolving both when possible so that a non-canonical
/// `root` still anchors the result, and `None` when `path` lies outside `root`.
fn strip_prefix_resolved(path: &Path, root: &Path) -> Option<String> {
    if let (Ok(root), Ok(canonical)) = (fs::canonicalize(root), fs::canonicalize(path))
        && let Ok(relative) = canonical.strip_prefix(root)
    {
        return Some(relative.display().to_string());
    }
    path.strip_prefix(root).ok().map(|relative| relative.display().to_string())
}

/// Escapes a GitHub Actions workflow-command message payload.
#[must_use]
pub(crate) fn escape(message: &str) -> String {
    message.replace('%', "%25").replace('\r', "%0D").replace('\n', "%0A")
}

/// Escapes a GitHub Actions workflow-command property value (e.g. the `file=`
/// property), which additionally escapes `:` and `,` beyond what the message
/// payload needs: those two characters delimit `key=value` pairs and separate
/// properties in the `::error key=value,key=value::message` syntax.
#[must_use]
pub(crate) fn escape_property(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
        .replace(':', "%3A")
        .replace(',', "%2C")
}

#[cfg(test)]
#[path = "github_tests.rs"]
mod tests;
