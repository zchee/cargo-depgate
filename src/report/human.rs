//! Human-readable diagnostics with source annotations and dependency witnesses.
//!
//! Source diagnostics use [`annotate_snippets::Renderer::plain`] when color is disabled and
//! [`annotate_snippets::Renderer::styled`] when color is enabled.

use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use annotate_snippets::{AnnotationKind, Level, Renderer, Snippet};
use anstyle::{AnsiColor, Style};

use crate::{
    manifest,
    rules::{RuleStatus, Violation, WitnessHop},
};

use super::RenderContext;

/// Renders the complete human-readable policy report.
///
/// # Errors
///
/// Propagates write errors from `out`.
pub fn render(
    outcome: &crate::pipeline::Outcome,
    ctx: &RenderContext,
    out: &mut dyn Write,
) -> io::Result<()> {
    let violations = super::violation_lookup(outcome);

    for status in &outcome.statuses {
        if status.passed {
            render_pass(status, ctx.color, out)?;
            continue;
        }

        if status.kind == manifest::RULE_KIND {
            for entry in outcome.manifest.iter().flat_map(|report| &report.entries) {
                render_manifest_failure(status, entry, outcome, ctx, out)?;
            }
            continue;
        }

        let violation = violations.get(status.id.as_str()).copied();
        render_graph_failure(status, violation, outcome, ctx, out)?;
    }

    let verdict = if outcome.counters.violations == 0 { "ok" } else { "FAIL" };
    writeln!(
        out,
        "{verdict}: {} rules, {} violations",
        outcome.counters.rules, outcome.counters.violations
    )
}

fn render_pass(status: &RuleStatus, color: bool, out: &mut dyn Write) -> io::Result<()> {
    if color {
        let style = Style::new().fg_color(Some(AnsiColor::Green.into()));
        writeln!(out, "{}ok {}{}", style.render(), status.id, style.render_reset())
    } else {
        writeln!(out, "ok {}", status.id)
    }
}

fn render_manifest_failure(
    status: &RuleStatus,
    entry: &manifest::ManifestViolation,
    outcome: &crate::pipeline::Outcome,
    ctx: &RenderContext,
    out: &mut dyn Write,
) -> io::Result<()> {
    let label = format!("{} {} = {:?}", entry.table, entry.dependency, entry.version);
    let display_file = display_path(&entry.span.file, &outcome.workspace_root).to_string();
    let rendered = fs::read_to_string(&entry.span.file).ok().and_then(|source| {
        let start = line_col_to_offset(&source, entry.span.line, entry.span.col)?;
        let end = start.checked_add(entry.version.len().checked_add(2)?)?;
        render_snippet(&source, &display_file, status, &label, start..end, ctx.color)
    });

    if let Some(rendered) = rendered {
        writeln!(out, "{rendered}")
    } else {
        writeln!(
            out,
            "FAIL {}: {}:{}:{} {label}",
            status.id, display_file, entry.span.line, entry.span.col
        )
    }
}

fn render_graph_failure(
    status: &RuleStatus,
    violation: Option<&Violation>,
    outcome: &crate::pipeline::Outcome,
    ctx: &RenderContext,
    out: &mut dyn Write,
) -> io::Result<()> {
    let label = violation_label(status, violation);
    if let Some(violation) = violation {
        let display_file = display_path(&violation.span.file, &outcome.workspace_root).to_string();
        let rendered = fs::read_to_string(&violation.span.file).ok().and_then(|source| {
            let start = line_col_to_offset(&source, violation.span.line, violation.span.col)?;
            let end = source[start..].find('\n').map_or(source.len(), |length| start + length);
            render_snippet(&source, &display_file, status, &label, start..end, ctx.color)
        });

        if let Some(rendered) = rendered {
            writeln!(out, "{rendered}")?;
        } else {
            writeln!(
                out,
                "FAIL {}: {}:{}:{} {label}",
                status.id, display_file, violation.span.line, violation.span.col
            )?;
        }
        render_violation_witnesses(violation, outcome, out)
    } else {
        writeln!(out, "FAIL {}: {label}", status.id)
    }
}

fn render_snippet(
    source: &str,
    path: &str,
    status: &RuleStatus,
    label: &str,
    range: std::ops::Range<usize>,
    color: bool,
) -> Option<String> {
    if range.is_empty() || source.get(range.clone()).is_none() {
        return None;
    }
    let report = &[Level::ERROR
        .primary_title("dependency policy violation")
        .id(status.id.as_str())
        .element(
            Snippet::source(source)
                .path(path)
                .annotation(AnnotationKind::Primary.span(range).label(label)),
        )];
    let renderer = if color { Renderer::styled() } else { Renderer::plain() };
    Some(renderer.render(report))
}

/// Renders a configuration error with a source annotation when the source is readable.
pub(crate) fn render_config_snippet(
    message: &str,
    span: &crate::config::Span,
    color: bool,
) -> Option<String> {
    let source = fs::read_to_string(&span.file).ok()?;
    let start = line_col_to_offset(&source, span.line, span.col)?;
    let end = source[start..].find('\n').map_or(source.len(), |length| start + length);
    if start >= end || source.get(start..end).is_none() {
        return None;
    }

    let path = span.file.display().to_string();
    let report = &[Level::ERROR.primary_title(message).element(
        Snippet::source(&source)
            .path(path)
            .annotation(AnnotationKind::Primary.span(start..end).label("here")),
    )];
    let renderer = if color { Renderer::styled() } else { Renderer::plain() };
    Some(renderer.render(report))
}

fn render_violation_witnesses(
    violation: &Violation,
    outcome: &crate::pipeline::Outcome,
    out: &mut dyn Write,
) -> io::Result<()> {
    let root_version = outcome.member_versions.get(&violation.package).map(String::as_str);
    match violation.kind {
        "deny" => {
            for matched in &violation.matches {
                writeln!(
                    out,
                    "  {}",
                    render_witness(
                        &violation.package,
                        root_version,
                        &matched.witness,
                        &matched.other_versions,
                    )
                )?;
            }
        }
        "internal" | "leaf" | "direct" => {
            for extra in &violation.extra {
                let witness = render_witness(
                    &violation.package,
                    root_version,
                    &extra.witness,
                    &extra.other_versions,
                );
                writeln!(out, "  +{} (via {witness})", extra.name)?;
            }
            for missing in &violation.missing {
                writeln!(out, "  -{missing}")?;
            }
        }
        "sealed" => {
            for entry in &violation.sealed_by {
                let witness = render_witness_versionless(&entry.member, &entry.witness);
                writeln!(out, "  consumed by: {} ({witness})", entry.member)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Builds the concise summary attached to a failed rule's source span.
#[must_use]
pub(crate) fn violation_label(status: &RuleStatus, violation: Option<&Violation>) -> String {
    match (status.kind, violation) {
        ("internal" | "leaf" | "direct", Some(violation)) => {
            format!("{} extra, {} missing", violation.extra.len(), violation.missing.len())
        }
        ("sealed", Some(violation)) => {
            format!("consumed by {} member(s)", violation.sealed_by.len())
        }
        _ => format!("{} match(es)", status.matched),
    }
}

/// Renders a versioned root-to-match witness with edge annotations.
#[must_use]
pub(crate) fn render_witness(
    root: &str,
    root_version: Option<&str>,
    hops: &[WitnessHop],
    other_versions: &[String],
) -> String {
    let mut rendered =
        root_version.map_or_else(|| root.to_owned(), |version| format!("{root} v{version}"));
    for hop in hops {
        rendered.push_str(" → ");
        rendered.push_str(&hop.name);
        rendered.push_str(" v");
        rendered.push_str(&hop.version);
        push_hop_annotations(&mut rendered, hop);
    }
    if !other_versions.is_empty() {
        rendered.push_str(" (other reachable versions: ");
        for (index, version) in other_versions.iter().enumerate() {
            if index > 0 {
                rendered.push_str(", ");
            }
            rendered.push_str(version);
        }
        rendered.push(')');
    }
    rendered
}

/// Appends the `[cfg(...)]` and `(optional; ...)` annotations for one hop, shared
/// between the versioned witness renderer and the version-free `sealed` one.
fn push_hop_annotations(rendered: &mut String, hop: &WitnessHop) {
    if let Some(target) = &hop.target {
        rendered.push_str(" [");
        rendered.push_str(target);
        rendered.push(']');
    }
    if hop.optional {
        rendered.push_str(" (optional; present via workspace feature unification)");
    }
}

/// Renders a version-free witness path with edge annotations — the `sealed` form AC 4 pins
/// (`tool → core`, see `sealed_failure_omits_versions_from_consuming_path`). The
/// `[cfg(...)]`/optional hop annotations still carry real information and are kept, exactly as
/// [`render_witness`] renders them; the GitHub reporter shares this function so its annotation
/// line and the report body agree.
pub(crate) fn render_witness_versionless(root: &str, hops: &[WitnessHop]) -> String {
    let mut rendered = root.to_owned();
    for hop in hops {
        rendered.push_str(" → ");
        rendered.push_str(&hop.name);
        push_hop_annotations(&mut rendered, hop);
    }
    rendered
}

/// Reconstructs a zero-based byte offset from a one-based line and character column.
///
/// Columns beyond the end of an existing line clamp to that line's end. Zero-valued or
/// nonexistent line coordinates return `None`.
#[must_use]
pub(crate) fn line_col_to_offset(text: &str, line: u32, col: u32) -> Option<usize> {
    let target_line = usize::try_from(line.checked_sub(1)?).ok()?;
    let target_col = usize::try_from(col.checked_sub(1)?).ok()?;
    let mut line_start = 0;

    for (index, segment) in text.split_inclusive('\n').enumerate() {
        if index == target_line {
            let content = segment.strip_suffix('\n').unwrap_or(segment);
            let within_line =
                content.char_indices().nth(target_col).map_or(content.len(), |(offset, _)| offset);
            return Some(line_start + within_line);
        }
        line_start += segment.len();
    }

    if target_line == text.bytes().filter(|&byte| byte == b'\n').count()
        && (text.is_empty() || text.ends_with('\n'))
    {
        Some(text.len())
    } else {
        None
    }
}

fn display_path<'a>(path: &'a Path, root: &Path) -> std::path::Display<'a> {
    path.strip_prefix(root).unwrap_or(path).display()
}

#[cfg(test)]
#[path = "human_tests.rs"]
mod tests;
