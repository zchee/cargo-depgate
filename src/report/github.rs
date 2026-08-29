//! GitHub Actions error annotations followed by the complete human report.

use std::{io, path::Path};

use crate::{manifest, rules::Violation};

use super::{RenderContext, human};

const ANNOTATION_LIMIT: usize = 10;

/// Renders at most ten GitHub error annotations followed by the human report.
///
/// Graph-rule failures retain declaration order and take priority over manifest
/// entries when the annotation cap is reached.
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
        let relative_file = display_path(file, &outcome.workspace_root).replace('\\', "/");
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

fn display_path(path: &Path, workspace_root: &Path) -> String {
    path.strip_prefix(workspace_root).unwrap_or(path).display().to_string()
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
