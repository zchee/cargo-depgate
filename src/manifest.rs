//! Rule 6, `manifest.versions-in-root`: no workspace member manifest may name a
//! dependency version in its own dependency tables.
//!
//! The rule reads every member `Cargo.toml` named by `cargo metadata` (rebased under
//! `--workspace-root` by the metadata layer), parses it once with byte spans, and
//! walks six table kinds: `dependencies`, `dev-dependencies`, `build-dependencies`
//! and the same three under every `target.<cfg>` entry. Any entry that names a
//! version is flagged, whether it uses the string form (`foo = "1"`) or the table
//! form (`foo = { version = "1", … }`, `foo.version = "1"`, `[dependencies.foo]`),
//! and regardless of `path`, `git` or `workspace` companions. Entries without a
//! version pass. The reported span is the **version value** itself: the quoted
//! string in the string form, the `version` value in the table form, as a 1-based
//! line and character column in the member manifest.
//!
//! `[workspace.dependencies]` is never inspected. Cargo rejects a second
//! `[workspace]` table in a member, so only the workspace-owning manifest can hold
//! it, and that table is the canonical version list the rule exists to protect.
//! When the owning manifest is itself a member (a root-package workspace), its own
//! `[dependencies]`/`[dev-dependencies]`/`[build-dependencies]`/`target.*` tables
//! are still checked like any other member's. There is therefore no member-level
//! branch here: the workspace table is simply absent from the deserialized shape.
//!
//! # Span capture (P3 spike decision)
//!
//! Spans come from typed deserialization with [`toml::Spanned`] placed at the
//! **map-value** level, `IndexMap<String, Spanned<DepSpec>>`, where `DepSpec` has a
//! hand-written [`serde::Deserialize`] that dispatches through `deserialize_any`:
//! a string visit records the string form (the outer `Spanned` already covers the
//! value), and a map visit reads the `version` key as `Spanned<String>` while
//! ignoring every other key. The `toml` deserializer honours `Spanned` at every
//! value position, so both spans are exact byte ranges of the value tokens
//! (quotes included), for every table shape, including dotted keys and
//! `[dependencies.foo]` sub-tables.
//!
//! Two alternatives were tried and rejected because they lose spans:
//!
//! - `#[serde(untagged)] enum { String, Table { version: Option<Spanned<String>> } }`
//!   fails with `data did not match any variant`: serde buffers untagged input
//!   into its private `Content` tree, which has no span support, so the inner
//!   `Spanned<String>` is asked to deserialize a plain map and refuses.
//! - `#[serde(flatten)]` for the shared dependency-table trio fails the same way
//!   (`invalid type: string, expected a spanned value`), because `flatten` uses the
//!   same buffering. The three tables are declared directly on the manifest struct
//!   instead, and the trio struct is reused only for `target.<cfg>` sub-tables.
//!
//! The document-level fallback, walking [`toml::de::DeTable`] by hand, produces
//! identical spans and remains available, but the typed route is shorter and
//! keeps the accepted shapes explicit. `toml_edit` was not needed.
//!
//! # The `[[test]]` hazard
//!
//! A line-oriented scanner that tracks "the current table" misattributes the
//! `name`/`harness` keys of `[[test]]`, `[[bin]]` or `[[bench]]` array-of-tables
//! that follow `[dev-dependencies]`. The layout is common: in the committed ckb
//! fixture alone, `freezer/Cargo.toml` opens a `[[test]]` immediately after its
//! `[dev-dependencies]`, and `network` and `benches` do the same with `[[bench]]`.
//! A real TOML parse attaches those entries to the top-level `test` array, so the
//! dependency tables only ever contain dependency entries.

use std::{borrow::Cow, fmt, fs, path::PathBuf};

use indexmap::IndexMap;
use serde::{
    Deserialize,
    de::{self, IgnoredAny, MapAccess, Visitor},
};
use toml::Spanned;

use crate::{config::Span, error::Error};

/// The stable identifier of the manifest rule.
pub const RULE_ID: &str = "manifest.versions-in-root";

/// The rule kind label of the manifest rule.
pub const RULE_KIND: &str = "manifest";

/// One workspace member manifest to check.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ManifestInput {
    /// The member package name, carried into every violation it produces.
    pub package: String,
    /// The member manifest path, already rebased under `--workspace-root`.
    pub path: PathBuf,
}

impl ManifestInput {
    /// Pairs a member package name with its manifest path.
    #[must_use]
    pub fn new(package: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self { package: package.into(), path: path.into() }
    }
}

/// One dependency declaration that names a version.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ManifestViolation {
    /// The member package whose manifest declares the version.
    pub package: String,
    /// The dependency table, rendered as it is written in the manifest
    /// (`dependencies`, `dev-dependencies`, `target.'cfg(unix)'.dependencies`, …).
    pub table: String,
    /// The dependency key in that table.
    pub dependency: String,
    /// The declared version requirement.
    pub version: String,
    /// The location of the version value in the member manifest.
    pub span: Span,
}

/// The result of the manifest rule over every workspace member.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct ManifestReport {
    /// Every offending entry, in member order and then in source order.
    pub entries: Vec<ManifestViolation>,
    /// The number of member manifests read and parsed.
    pub manifests_scanned: u32,
    /// The total size of the manifests read, in bytes.
    pub bytes_scanned: u64,
}

impl ManifestReport {
    /// Whether the rule passed, that is, no entry names a version.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Runs the manifest rule over the given member manifests.
///
/// Each manifest is read and parsed once. Violations are collected in the order
/// the members are given and, within one manifest, by source offset.
///
/// # Errors
///
/// Returns [`Error::ManifestRead`] when a manifest cannot be read and
/// [`Error::ManifestParse`] when it is not valid TOML or a dependency entry has a
/// shape Cargo would reject. Both map to exit code 3: a manifest that cannot be
/// checked is never silently skipped.
pub fn check_versions_in_root(
    members: impl IntoIterator<Item = ManifestInput>,
) -> Result<ManifestReport, Error> {
    let mut report = ManifestReport::default();
    for member in members {
        let text = fs::read_to_string(&member.path)
            .map_err(|source| Error::ManifestRead { path: member.path.clone(), source })?;
        report.entries.extend(scan_manifest(&member, &text)?);
        report.manifests_scanned = report.manifests_scanned.saturating_add(1);
        report.bytes_scanned =
            report.bytes_scanned.saturating_add(u64::try_from(text.len()).unwrap_or(u64::MAX));
    }
    Ok(report)
}

/// Scans one manifest's text for dependency entries that name a version.
///
/// The returned violations are sorted by source offset and carry `member.path`
/// in their spans.
///
/// # Errors
///
/// Returns [`Error::ManifestParse`] when `text` is not valid TOML or a dependency
/// entry is neither a string nor a table.
pub fn scan_manifest(member: &ManifestInput, text: &str) -> Result<Vec<ManifestViolation>, Error> {
    let manifest: RawManifest = toml::from_str(text)
        .map_err(|source| Error::ManifestParse { path: member.path.clone(), source })?;

    let mut found = Vec::new();
    collect_tables(manifest.kinds(), None, &mut found);
    for (target, tables) in &manifest.target {
        collect_tables(tables.kinds(), Some(target), &mut found);
    }
    found.sort_by_key(|entry| entry.offset);

    Ok(found
        .into_iter()
        .map(|entry| ManifestViolation {
            package: member.package.clone(),
            table: entry.table,
            dependency: entry.dependency,
            version: entry.version,
            span: crate::config::source_span(&member.path, text, entry.offset),
        })
        .collect())
}

/// Renders a dependency table path for one of the three tables under `target`.
///
/// Keys that are valid TOML bare keys are left as they are; anything else is
/// quoted the way Cargo documents `[target.'cfg(unix)'.dependencies]`.
fn table_label(target: Option<&str>, table: &'static str) -> String {
    match target {
        None => table.to_owned(),
        Some(target) => format!("target.{}.{table}", quote_key(target)),
    }
}

fn quote_key(key: &str) -> Cow<'_, str> {
    let bare = !key.is_empty()
        && key.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if bare {
        Cow::Borrowed(key)
    } else if !key.contains('\'') {
        Cow::Owned(format!("'{key}'"))
    } else {
        Cow::Owned(format!("\"{}\"", key.replace('\\', "\\\\").replace('"', "\\\"")))
    }
}

struct Found {
    offset: usize,
    table: String,
    dependency: String,
    version: String,
}

/// The three dependency tables of one manifest level, labelled as written.
type TableKinds<'a> = [(&'static str, &'a DepTable); 3];

fn collect_tables(kinds: TableKinds<'_>, target: Option<&str>, found: &mut Vec<Found>) {
    for (kind, table) in kinds {
        if table.is_empty() {
            continue;
        }
        let label = table_label(target, kind);
        for (dependency, spec) in table {
            let (offset, version) = match spec.get_ref() {
                DepSpec::Simple(version) => (spec.span().start, version.clone()),
                DepSpec::Detailed { version: Some(version) } => {
                    (version.span().start, version.get_ref().clone())
                }
                DepSpec::Detailed { version: None } => continue,
            };
            found.push(Found {
                offset,
                table: label.clone(),
                dependency: dependency.clone(),
                version,
            });
        }
    }
}

type DepTable = IndexMap<String, Spanned<DepSpec>>;

/// The subset of a manifest the rule reads. Every other table (`package`,
/// `workspace`, `features`, `[[test]]`, …) is ignored by the derived visitor.
///
/// The top-level trio is spelled out rather than shared with [`DepTables`] through
/// `#[serde(flatten)]`, because `flatten` buffers through serde's `Content` and
/// drops every span (module docs). Cargo accepts the deprecated underscore spellings
/// `dev_dependencies` / `build_dependencies` as separate fields — a manifest may carry
/// both spellings at once, and the hyphenated table wins — so this keeps two fields
/// per kind rather than a serde alias, which would reject that manifest as a
/// duplicate field that Cargo itself accepts.
#[derive(Deserialize)]
struct RawManifest {
    #[serde(default)]
    dependencies: DepTable,
    #[serde(default, rename = "dev-dependencies")]
    dev_dependencies: DepTable,
    #[serde(default, rename = "dev_dependencies")]
    dev_dependencies_underscore: DepTable,
    #[serde(default, rename = "build-dependencies")]
    build_dependencies: DepTable,
    #[serde(default, rename = "build_dependencies")]
    build_dependencies_underscore: DepTable,
    #[serde(default)]
    target: IndexMap<String, DepTables>,
}

/// Cargo's precedence when both spellings of a table are present: the hyphenated one.
fn prefer_hyphenated<'t>(hyphenated: &'t DepTable, underscore: &'t DepTable) -> &'t DepTable {
    if hyphenated.is_empty() { underscore } else { hyphenated }
}

impl RawManifest {
    fn kinds(&self) -> TableKinds<'_> {
        [
            ("dependencies", &self.dependencies),
            (
                "dev-dependencies",
                prefer_hyphenated(&self.dev_dependencies, &self.dev_dependencies_underscore),
            ),
            (
                "build-dependencies",
                prefer_hyphenated(&self.build_dependencies, &self.build_dependencies_underscore),
            ),
        ]
    }
}

/// The dependency-table trio under one `target.<cfg>` entry.
#[derive(Default, Deserialize)]
struct DepTables {
    #[serde(default)]
    dependencies: DepTable,
    #[serde(default, rename = "dev-dependencies")]
    dev_dependencies: DepTable,
    #[serde(default, rename = "dev_dependencies")]
    dev_dependencies_underscore: DepTable,
    #[serde(default, rename = "build-dependencies")]
    build_dependencies: DepTable,
    #[serde(default, rename = "build_dependencies")]
    build_dependencies_underscore: DepTable,
}

impl DepTables {
    fn kinds(&self) -> TableKinds<'_> {
        [
            ("dependencies", &self.dependencies),
            (
                "dev-dependencies",
                prefer_hyphenated(&self.dev_dependencies, &self.dev_dependencies_underscore),
            ),
            (
                "build-dependencies",
                prefer_hyphenated(&self.build_dependencies, &self.build_dependencies_underscore),
            ),
        ]
    }
}

/// One dependency entry, reduced to whether it names a version and where.
enum DepSpec {
    /// The string form `foo = "1"`; the version span is the whole value.
    Simple(String),
    /// The table form; only the `version` key is retained.
    Detailed { version: Option<Spanned<String>> },
}

impl<'de> Deserialize<'de> for DepSpec {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(DepSpecVisitor)
    }
}

struct DepSpecVisitor;

impl<'de> Visitor<'de> for DepSpecVisitor {
    type Value = DepSpec;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a version string or a dependency table")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(DepSpec::Simple(value.to_owned()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(DepSpec::Simple(value))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut version = None;
        while let Some(key) = map.next_key::<DepKey>()? {
            match key {
                DepKey::Version => version = Some(map.next_value::<Spanned<String>>()?),
                DepKey::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
                // `toml` encodes a datetime value as a map with one private key; it is
                // not a Cargo dependency shape and must fail closed like any other scalar.
                DepKey::Datetime => {
                    return Err(de::Error::invalid_type(
                        de::Unexpected::Other("a datetime"),
                        &self,
                    ));
                }
            }
        }
        Ok(DepSpec::Detailed { version })
    }
}

/// A dependency-table key, classified without allocating.
enum DepKey {
    Version,
    Other,
    /// `toml`'s private marker key for a datetime value.
    Datetime,
}

impl<'de> Deserialize<'de> for DepKey {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeyVisitor;

        impl Visitor<'_> for KeyVisitor {
            type Value = DepKey;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a dependency table key")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(match value {
                    "version" => DepKey::Version,
                    "$__toml_private_datetime" => DepKey::Datetime,
                    _ => DepKey::Other,
                })
            }
        }

        deserializer.deserialize_str(KeyVisitor)
    }
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
