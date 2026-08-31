//! Loading, validating, and representing `depgate.toml` configuration.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

use globset::{Glob, GlobMatcher, GlobSet, GlobSetBuilder};
use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::Deserialize;
use toml::Spanned;

use crate::{
    error::Error,
    features::{self, Selection},
    graph::Graph,
    platform::{self, PlatformSelection},
};

const CURRENT_SCHEMA: u32 = 1;

/// A source location in a configuration file.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Span {
    /// The path of the configuration file.
    pub file: PathBuf,
    /// The one-based source line.
    pub line: u32,
    /// The one-based source column.
    pub col: u32,
}

/// The graph feature selection requested by a configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum FeatureSelection {
    /// Resolve the package's default features.
    Default,
    /// Resolve all available features.
    All,
    /// Resolve the listed package feature specifications.
    List(Vec<String>),
}

/// The validated definition of workspace-internal package matching.
#[derive(Clone, Debug)]
pub struct InternalDef {
    /// Whether workspace members are included automatically.
    pub members: bool,
    /// Additional package-name patterns treated as internal.
    pub patterns: GlobSet,
}

/// One flattened policy rule.
#[derive(Clone, Debug)]
pub struct Rule {
    /// The stable rule identifier, such as `rules.foo.deny`.
    pub id: String,
    /// The workspace package to which this rule applies.
    pub package: String,
    /// The rule operation and its operands.
    pub kind: RuleKind,
    /// The source location of the rule field.
    pub span: Span,
    /// The feature selection the rule's closure is narrowed to, or `None` for the
    /// workspace-unified closure every rule read before the key existed.
    ///
    /// Every rule flattened from one `[rules.<package>]` table carries that table's value:
    /// the key selects a closure, and the kinds in the table are questions about it.
    pub features: Option<RuleFeatures>,
}

/// A rule's feature-aware closure selection, with the source location of the key that asked
/// for it — the span a validation error anchors at, rather than the rule kind's own key.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RuleFeatures {
    /// The activation the rule's root is seeded with.
    pub selection: Selection,
    /// The source location of the `features` value.
    pub span: Span,
}

/// The operation represented by one flattened rule.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum RuleKind {
    /// Reject matching dependency names. Exact values stay outside the glob set.
    ///
    /// The closure a `deny` rule inspects includes the rule's own package, but a
    /// pattern matching that package's own name never matches: a self-match is
    /// not a dependency finding and would carry an empty witness.
    Deny {
        /// Literal names that must be rejected.
        exact: BTreeSet<String>,
        /// Glob patterns that must be rejected.
        globs: GlobSet,
        /// The original values in declaration order.
        raw: Vec<String>,
    },
    /// Require every pattern to match some name in the closure — the dual of `deny`,
    /// evaluated on exactly the same closure.
    ///
    /// Patterns keep declaration order because a failure reports the ones that matched
    /// nothing, and they are kept one per entry rather than split into an exact set and
    /// a glob set: the verdict is per pattern, not per reached name.
    ///
    /// The rule's own package name never satisfies a pattern, mirroring [`RuleKind::Deny`]:
    /// `require = ["acme-*"]` on `rules.acme-app` asks for a *dependency* of that family,
    /// not for the package the rule is rooted at.
    Require(Vec<RequirePattern>),
    /// Require the listed names to be internal packages.
    // P5 may move these sets to name-id space (plan §3.2 step 9); string sets are correct today.
    Internal(BTreeSet<String>),
    /// Require the package to have no normal dependencies.
    Leaf,
    /// Require the listed names to be direct normal dependencies.
    // P5 may move these sets to name-id space (plan §3.2 step 9); string sets are correct today.
    Direct(BTreeSet<String>),
    /// Require the package's normal dependency set to be sealed.
    Sealed,
}

impl RuleKind {
    /// Whether this kind is answered from the root's forward closure.
    ///
    /// The one definition of that set: rule evaluation runs the closure kinds together on one
    /// traversal, and a `features` selection narrows exactly them — `direct` reads depth-one
    /// edges and `sealed` reads the reverse graph, so neither has a closure to narrow.
    #[must_use]
    pub const fn reads_closure(&self) -> bool {
        matches!(self, Self::Deny { .. } | Self::Require(_) | Self::Internal(_) | Self::Leaf)
    }
}

/// One `require` entry: an exact package name or a compiled glob over names.
///
/// The split follows the same grammar `deny` uses — a value is a glob exactly when it
/// contains `*`, `?` or `[` — but each entry keeps its own matcher so that a failed rule
/// can name the patterns that matched nothing.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum RequirePattern {
    /// A literal package name.
    Exact(String),
    /// A glob over package names.
    Glob(Box<GlobMatcher>),
}

impl RequirePattern {
    /// The pattern exactly as it was written in the configuration.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Exact(name) => name,
            Self::Glob(matcher) => matcher.glob().glob(),
        }
    }

    /// Whether `name` satisfies this pattern.
    #[must_use]
    pub fn is_match(&self, name: &str) -> bool {
        match self {
            Self::Exact(exact) => exact == name,
            Self::Glob(matcher) => matcher.is_match(name),
        }
    }
}

/// A graph-independent configuration validation error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigError {
    /// A human-readable explanation of the invalid configuration.
    pub message: String,
    /// The source location associated with the error, when available.
    pub span: Option<Span>,
}

/// A validated configuration and diagnostics produced by graph validation.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Validated {
    /// The executable configuration representation.
    pub config: Config,
    /// Non-fatal diagnostics emitted during validation.
    pub warnings: Vec<String>,
    /// Number of direct rules whose package declares an optional normal dependency.
    pub direct_optional_decls: u32,
}

/// The validated configuration representation consumed by later rule passes.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
    /// The configuration schema version.
    pub schema: u32,
    /// The requested Cargo feature selection.
    pub features: FeatureSelection,
    /// The target platforms dependency edges are evaluated against.
    pub platform: PlatformSelection,
    /// The internal-package definition.
    pub internal: InternalDef,
    /// Whether the root manifest version rule is enabled.
    pub manifest_versions_in_root: bool,
    /// Flattened rules in TOML declaration order.
    pub rules: Vec<Rule>,
}

macro_rules! config_types {
    (
        raw $( $raw_default:ident )? {
            $(#[$raw_struct_meta:meta])*
            $raw_name:ident
        }
        schema $( $schema_default:ident )? {
            $(#[$schema_struct_meta:meta])*
            $schema_name:ident
        }
        keys [$keys_name:ident]
        fields {
            $(
                $field:ident {
                    raw {
                        $(#[$raw_field_meta:meta])*
                        type: $raw_type:ty
                    }
                    schema {
                        $(#[$schema_field_meta:meta])*
                        type: $schema_type:ty
                    }
                }
            ),* $(,)?
        }
        raw_extra {
            $(
                $(#[$raw_extra_meta:meta])*
                $raw_extra_name:ident: $raw_extra_type:ty
            ),* $(,)?
        }
    ) => {
        config_types!(@structs
            raw $( $raw_default )? {
                $(#[$raw_struct_meta])*
                $raw_name
            }
            schema $( $schema_default )? {
                $(#[$schema_struct_meta])*
                $schema_name
            }
            fields {
                $(
                    $field {
                        raw {
                            $(#[$raw_field_meta])*
                            type: $raw_type
                        }
                        schema {
                            $(#[$schema_field_meta])*
                            type: $schema_type
                        }
                    }
                ),*
            }
            raw_extra {
                $(
                    $(#[$raw_extra_meta])*
                    $raw_extra_name: $raw_extra_type
                ),*
            }
        );

        #[cfg(test)]
        const $keys_name: &[&str] = &[$(stringify!($field)),*];
    };

    (
        raw $( $raw_default:ident )? {
            $(#[$raw_struct_meta:meta])*
            $raw_name:ident
        }
        schema $( $schema_default:ident )? {
            $(#[$schema_struct_meta:meta])*
            $schema_name:ident
        }
        keys []
        fields {
            $(
                $field:ident {
                    raw {
                        $(#[$raw_field_meta:meta])*
                        type: $raw_type:ty
                    }
                    schema {
                        $(#[$schema_field_meta:meta])*
                        type: $schema_type:ty
                    }
                }
            ),* $(,)?
        }
        raw_extra {
            $(
                $(#[$raw_extra_meta:meta])*
                $raw_extra_name:ident: $raw_extra_type:ty
            ),* $(,)?
        }
    ) => {
        config_types!(@structs
            raw $( $raw_default )? {
                $(#[$raw_struct_meta])*
                $raw_name
            }
            schema $( $schema_default )? {
                $(#[$schema_struct_meta])*
                $schema_name
            }
            fields {
                $(
                    $field {
                        raw {
                            $(#[$raw_field_meta])*
                            type: $raw_type
                        }
                        schema {
                            $(#[$schema_field_meta])*
                            type: $schema_type
                        }
                    }
                ),*
            }
            raw_extra {
                $(
                    $(#[$raw_extra_meta])*
                    $raw_extra_name: $raw_extra_type
                ),*
            }
        );
    };

    (
        @structs
        raw $( $raw_default:ident )? {
            $(#[$raw_struct_meta:meta])*
            $raw_name:ident
        }
        schema $( $schema_default:ident )? {
            $(#[$schema_struct_meta:meta])*
            $schema_name:ident
        }
        fields {
            $(
                $field:ident {
                    raw {
                        $(#[$raw_field_meta:meta])*
                        type: $raw_type:ty
                    }
                    schema {
                        $(#[$schema_field_meta:meta])*
                        type: $schema_type:ty
                    }
                }
            ),* $(,)?
        }
        raw_extra {
            $(
                $(#[$raw_extra_meta:meta])*
                $raw_extra_name:ident: $raw_extra_type:ty
            ),* $(,)?
        }
    ) => {
        $(#[$raw_struct_meta])*
        #[derive(Clone, Debug, Deserialize $(, $raw_default)?)]
        #[serde(deny_unknown_fields)]
        pub struct $raw_name {
            $(
                $(#[$raw_field_meta])*
                pub $field: $raw_type,
            )*
            $(
                $(#[$raw_extra_meta])*
                $raw_extra_name: $raw_extra_type,
            )*
        }

        $(#[$schema_struct_meta])*
        #[derive(Clone, Debug, Deserialize, JsonSchema $(, $schema_default)?)]
        #[serde(deny_unknown_fields)]
        pub struct $schema_name {
            $(
                $(#[$schema_field_meta])*
                pub $field: $schema_type,
            )*
        }
    };
}

config_types! {
    raw {
        /// The source-spanned raw configuration accepted by the TOML loader.
        RawConfig
    }
    schema Default {
        /// The plain, schema-facing representation of a configuration.
        ConfigSchema
    }
    keys [RAW_CONFIG_FIELD_NAMES]
    fields {
        schema {
            raw {
                /// The required configuration schema version.
                type: Spanned<u32>
            }
            schema {
                /// The configuration schema version.
                type: u32
            }
        },
        graph {
            raw {
                /// Graph settings.
                #[serde(default)]
                type: RawGraph
            }
            schema {
                /// Graph feature settings.
                #[serde(default)]
                type: ConfigGraphSchema
            }
        },
        internal {
            raw {
                /// Internal package settings.
                #[serde(default)]
                type: RawInternal
            }
            schema {
                /// Internal package settings.
                #[serde(default)]
                type: ConfigInternalSchema
            }
        },
        manifest {
            raw {
                /// Manifest settings.
                #[serde(default)]
                type: RawManifest
            }
            schema {
                /// Manifest settings.
                #[serde(default)]
                type: ConfigManifestSchema
            }
        },
        rules {
            raw {
                /// Per-package rule tables in source declaration order.
                #[serde(default)]
                type: IndexMap<String, Spanned<RawRuleSpec>>
            }
            schema {
                /// Rules keyed by workspace package name.
                #[serde(default)]
                type: BTreeMap<String, ConfigRuleSchema>
            }
        }
    }
    raw_extra {
        #[serde(skip)]
        source: Option<SourceData>
    }
}

config_types! {
    raw {
        /// Source-spanned graph settings.
        RawGraph
    }
    schema {
        /// Plain graph settings used by [`ConfigSchema`].
        ConfigGraphSchema
    }
    keys []
    fields {
        features {
            raw {
                /// Cargo feature selection.
                #[serde(default = "default_spanned_feature_value")]
                type: Spanned<FeatureValue>
            }
            schema {
                /// Cargo feature selection: `default`, `all`, or a list of feature specs.
                #[serde(default = "default_feature_value")]
                type: FeatureValue
            }
        },
        platform {
            raw {
                /// Target-platform selection for dependency-edge evaluation.
                #[serde(default = "default_spanned_platform_value")]
                type: Spanned<PlatformValue>
            }
            schema {
                /// Target-platform selection: `all`, `host`, or a list of target triples.
                #[serde(default = "default_platform_value")]
                type: PlatformValue
            }
        }
    }
    raw_extra {}
}

config_types! {
    raw {
        /// Source-spanned internal package settings.
        RawInternal
    }
    schema {
        /// Plain internal settings used by [`ConfigSchema`].
        ConfigInternalSchema
    }
    keys []
    fields {
        members {
            raw {
                /// Whether workspace members are automatically internal.
                #[serde(default = "default_spanned_true")]
                type: Spanned<bool>
            }
            schema {
                /// Whether workspace members are automatically internal.
                #[serde(default = "default_true")]
                type: bool
            }
        },
        patterns {
            raw {
                /// Additional internal package-name glob patterns.
                #[serde(default = "default_spanned_strings")]
                type: Spanned<Vec<String>>
            }
            schema {
                /// Additional internal package-name glob patterns.
                #[serde(default)]
                type: Vec<String>
            }
        }
    }
    raw_extra {}
}

config_types! {
    raw {
        /// Source-spanned manifest settings.
        RawManifest
    }
    schema {
        /// Plain manifest settings used by [`ConfigSchema`].
        ConfigManifestSchema
    }
    keys []
    fields {
        versions_in_root {
            raw {
                /// Whether the root manifest version is checked.
                #[serde(default = "default_spanned_true")]
                #[serde(rename = "versions-in-root")]
                type: Spanned<bool>
            }
            schema {
                /// Whether the root manifest version is checked.
                #[serde(default = "default_true")]
                #[serde(rename = "versions-in-root")]
                type: bool
            }
        }
    }
    raw_extra {}
}

config_types! {
    raw Default {
        /// Source-spanned per-package rule settings.
        RawRuleSpec
    }
    schema Default {
        /// Plain per-package rule settings used by [`ConfigSchema`].
        ConfigRuleSchema
    }
    keys []
    fields {
        features {
            raw {
                /// The feature selection the rule's closure is evaluated on.
                #[serde(default)]
                type: Option<Spanned<FeatureValue>>
            }
            schema {
                /// Feature selection for this rule's closure: `unified` (the default), `none`,
                /// `default`, `all`, or a list of this package's own features.
                #[serde(default)]
                type: Option<FeatureValue>
            }
        },
        deny {
            raw {
                /// Dependency names or glob patterns denied by the rule.
                #[serde(default)]
                type: Option<Spanned<Vec<String>>>
            }
            schema {
                /// Dependency names or glob patterns denied by the rule.
                #[serde(default)]
                type: Option<Vec<String>>
            }
        },
        require {
            raw {
                /// Dependency names or glob patterns the rule's closure must contain.
                #[serde(default)]
                type: Option<Spanned<Vec<String>>>
            }
            schema {
                /// Dependency names or glob patterns the rule's closure must contain.
                #[serde(default)]
                type: Option<Vec<String>>
            }
        },
        internal {
            raw {
                /// Exact package names required to be internal.
                #[serde(default)]
                type: Option<Spanned<Vec<String>>>
            }
            schema {
                /// Exact package names required to be internal.
                #[serde(default)]
                type: Option<Vec<String>>
            }
        },
        leaf {
            raw {
                /// Whether the package must be a leaf.
                #[serde(default)]
                type: Option<Spanned<bool>>
            }
            schema {
                /// Whether the package must be a leaf.
                #[serde(default)]
                type: Option<bool>
            }
        },
        direct {
            raw {
                /// Exact package names required as direct dependencies.
                #[serde(default)]
                type: Option<Spanned<Vec<String>>>
            }
            schema {
                /// Exact package names required as direct dependencies.
                #[serde(default)]
                type: Option<Vec<String>>
            }
        },
        sealed {
            raw {
                /// Whether the package's normal dependency set must be sealed.
                #[serde(default)]
                type: Option<Spanned<bool>>
            }
            schema {
                /// Whether the package's normal dependency set must be sealed.
                #[serde(default)]
                type: Option<bool>
            }
        }
    }
    raw_extra {}
}

impl Default for ConfigGraphSchema {
    fn default() -> Self {
        Self { features: default_feature_value(), platform: default_platform_value() }
    }
}

impl Default for ConfigInternalSchema {
    fn default() -> Self {
        Self { members: true, patterns: Vec::new() }
    }
}

impl Default for RawGraph {
    fn default() -> Self {
        Self {
            features: default_spanned_feature_value(),
            platform: default_spanned_platform_value(),
        }
    }
}

impl Default for RawInternal {
    fn default() -> Self {
        Self { members: default_spanned_true(), patterns: default_spanned_strings() }
    }
}

impl Default for ConfigManifestSchema {
    fn default() -> Self {
        Self { versions_in_root: true }
    }
}

impl Default for RawManifest {
    fn default() -> Self {
        Self { versions_in_root: default_spanned_true() }
    }
}

/// The non-spanned feature input accepted by TOML and the schema generator.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum FeatureValue {
    /// A named selection (`default` or `all`).
    Named(String),
    /// Explicit Cargo feature specifications.
    List(Vec<String>),
}

/// The non-spanned platform input accepted by TOML and the schema generator.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum PlatformValue {
    /// A named selection (`all` or `host`).
    Named(String),
    /// Explicit target triples.
    List(Vec<String>),
}

#[derive(Clone, Debug)]
struct SourceData {
    path: PathBuf,
    text: String,
}

/// Loads and parses a configuration file.
///
/// # Errors
///
/// Returns [`Error::Configuration`] when the file cannot be read or TOML cannot be
/// deserialized into [`RawConfig`].
pub fn load(path: &Path) -> Result<RawConfig, Error> {
    let text = fs::read_to_string(path).map_err(|source| Error::Configuration {
        message: format!("failed to read configuration {}: {source}", path.display()),
        span: None,
    })?;
    let mut raw: RawConfig = toml::from_str(&text).map_err(|source| {
        let span = source.span().map(|range| source_span(path, &text, range.start));
        Error::Configuration { message: source.to_string(), span }
    })?;
    raw.source = Some(SourceData { path: path.to_path_buf(), text });
    Ok(raw)
}

/// Returns the conventional configuration path under `workspace_root`.
#[must_use]
pub fn discover(workspace_root: &Path) -> PathBuf {
    workspace_root.join("depgate.toml")
}

/// Builds the "unknown package" diagnostic shared by every place a configured or
/// requested package name fails to resolve against the graph: `phase_b`'s rule
/// validation and `pipeline::explain`'s package/dependency lookups use the same
/// wording with a different `context` prefix, so the message never drifts between
/// call sites.
#[must_use]
pub fn unknown_package_message(context: &str, name: &str) -> String {
    format!("{context} references unknown package `{name}`")
}

/// Validates a raw configuration, optionally against a dependency graph.
///
/// Graph-independent checks always run before any graph lookup. Passing `None` is
/// therefore equivalent to passing a graph for all Phase A errors.
///
/// # Errors
///
/// Returns [`ConfigError`] for invalid schema, rule declarations, glob patterns,
/// workspace membership, or unresolved package names.
pub fn validate(cfg: &RawConfig, graph: Option<&Graph<'_>>) -> Result<Validated, ConfigError> {
    let mut validated = phase_a(cfg)?;
    if let Some(graph) = graph {
        phase_b(&mut validated, graph)?;
    }
    Ok(validated)
}

/// Rejects a configuration written for a schema this build does not implement.
///
/// This is Phase A's first check, exposed on its own because the pipeline has to read
/// `[graph].platform` out of a *discovered* file before that file reaches validation: the graph
/// cannot be built without the selection, and validation needs the graph. Without this gate the
/// two configuration paths would disagree about what is wrong with the same file — `--config`
/// would report the schema, a discovered file would report whatever the platform value happens
/// to look like under a grammar it was never written for.
///
/// # Errors
///
/// Returns [`ConfigError`] anchored at the `schema` value when it is not the version this
/// build implements.
pub fn check_schema(cfg: &RawConfig) -> Result<(), ConfigError> {
    let schema = *cfg.schema.get_ref();
    if schema == CURRENT_SCHEMA {
        return Ok(());
    }
    Err(config_error(
        cfg,
        cfg.schema.span().start,
        format!("unsupported configuration schema {schema}; expected {CURRENT_SCHEMA}"),
    ))
}

fn phase_a(cfg: &RawConfig) -> Result<Validated, ConfigError> {
    check_schema(cfg)?;
    let schema = *cfg.schema.get_ref();

    let features = feature_selection(cfg, cfg.graph.features.get_ref(), cfg.graph.features.span())?;
    let platform = platform_selection(cfg)?;
    let patterns =
        compile_patterns(cfg, cfg.internal.patterns.get_ref(), cfg.internal.patterns.span())?;
    let mut rules = Vec::new();

    for (package, table) in &cfg.rules {
        rules.extend(flatten_package_rules(cfg, package, table)?);
    }

    if rules.is_empty() && !*cfg.manifest.versions_in_root.get_ref() {
        return Err(config_error(cfg, 0, "depgate.toml declares no rules"));
    }

    Ok(Validated {
        config: Config {
            schema,
            features,
            platform,
            internal: InternalDef { members: *cfg.internal.members.get_ref(), patterns },
            manifest_versions_in_root: *cfg.manifest.versions_in_root.get_ref(),
            rules,
        },
        warnings: Vec::new(),
        direct_optional_decls: 0,
    })
}

/// Flattens one `[rules.<package>]` table into rules, in TOML declaration order.
///
/// Every kind is collected with the byte offset of its key and sorted afterwards, so the
/// order the rules are reported in is the order they were written in, whatever order this
/// function happens to inspect the keys in.
fn flatten_package_rules(
    cfg: &RawConfig,
    package: &str,
    table: &Spanned<RawRuleSpec>,
) -> Result<Vec<Rule>, ConfigError> {
    let spec = table.get_ref();
    let mut package_rules = Vec::new();
    if spec.leaf.as_ref().is_some_and(|value| *value.get_ref()) && spec.internal.is_some() {
        let span = spec.leaf.as_ref().map_or_else(|| table.span(), Spanned::span);
        return Err(config_error(
            cfg,
            span.start,
            format!("rules.{package} declares both leaf and internal"),
        ));
    }
    let features = rule_features(cfg, package, spec.features.as_ref())?;
    let mut add_rule = |offset, name, kind| {
        package_rules.push((
            offset,
            Rule {
                id: format!("rules.{package}.{name}"),
                package: package.to_owned(),
                kind,
                span: config_span(cfg, offset),
                features: features.clone(),
            },
        ));
    };

    if let Some(internal) = &spec.internal {
        for (index, name) in internal.get_ref().iter().enumerate() {
            if name == package {
                let range = array_entry_range(cfg, internal.span(), index);
                return Err(config_error(
                    cfg,
                    range.start,
                    format!(
                        "rules.{package}.internal cannot contain the rule package itself ({name})"
                    ),
                ));
            }
        }
        let offset = internal.span().start;
        add_rule(
            offset,
            "internal",
            RuleKind::Internal(internal.get_ref().iter().cloned().collect()),
        );
    }

    if let Some(deny) = &spec.deny {
        let (exact, globs) = compile_deny(cfg, deny.get_ref(), deny.span())?;
        let offset = deny.span().start;
        add_rule(offset, "deny", RuleKind::Deny { exact, globs, raw: deny.get_ref().clone() });
    }

    if let Some(require) = &spec.require {
        let patterns = compile_require(cfg, require.get_ref(), require.span())?;
        let offset = require.span().start;
        add_rule(offset, "require", RuleKind::Require(patterns));
    }

    if let Some(leaf) = &spec.leaf
        && *leaf.get_ref()
    {
        let offset = leaf.span().start;
        add_rule(offset, "leaf", RuleKind::Leaf);
    }

    if let Some(direct) = &spec.direct {
        let offset = direct.span().start;
        add_rule(offset, "direct", RuleKind::Direct(direct.get_ref().iter().cloned().collect()));
    }

    if let Some(sealed) = &spec.sealed
        && *sealed.get_ref()
    {
        let offset = sealed.span().start;
        add_rule(offset, "sealed", RuleKind::Sealed);
    }

    if let Some(features) = &spec.features
        && !package_rules.iter().any(|(_, rule)| rule.kind.reads_closure())
    {
        return Err(config_error(
            cfg,
            features.span().start,
            format!(
                "rules.{package}.features narrows the closure deny, require, internal and leaf \
                 read; rules.{package} declares none of them"
            ),
        ));
    }

    package_rules.sort_by_key(|(offset, _)| *offset);
    Ok(package_rules.into_iter().map(|(_, rule)| rule).collect())
}

/// Validates one `[rules.<package>].features` value and resolves it to an activation.
///
/// `unified` is the absent key spelled out — the workspace-unified closure — so it resolves to
/// `None` and costs no walk. Every other value opts the rule into a package-rooted activation,
/// which is sound only on an all-features document; [`phase_b`] holds that guard, because it
/// is a property of the resolve rather than of the file.
fn rule_features(
    cfg: &RawConfig,
    package: &str,
    value: Option<&Spanned<FeatureValue>>,
) -> Result<Option<RuleFeatures>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let span = value.span();
    let selection = match value.get_ref() {
        FeatureValue::Named(name) => match name.as_str() {
            "unified" => return Ok(None),
            "none" => Selection::None,
            "default" => Selection::Default,
            "all" => Selection::All,
            other => {
                return Err(config_error(
                    cfg,
                    span.start,
                    format!(
                        "rules.{package}.features must be `unified`, `none`, `default`, `all`, \
                         or a list of feature names (got `{other}`)"
                    ),
                ));
            }
        },
        FeatureValue::List(features) => Selection::List(features.clone()),
    };
    Ok(Some(RuleFeatures { selection, span: config_span(cfg, span.start) }))
}

fn phase_b(validated: &mut Validated, graph: &Graph<'_>) -> Result<(), ConfigError> {
    let mut member_nodes = HashMap::with_capacity(graph.members().len());
    for &node in graph.members() {
        member_nodes.entry(graph.name(node)).or_insert(node);
    }

    for rule in &validated.config.rules {
        let Some(&package_node) = member_nodes.get(rule.package.as_str()) else {
            return Err(ConfigError {
                message: format!("rules.{} targets non-member workspace package", rule.package),
                span: Some(rule.span.clone()),
            });
        };

        let names = match &rule.kind {
            RuleKind::Internal(names) | RuleKind::Direct(names) => Some(names),
            // `require` takes patterns, not exact names, so a value naming no package in
            // the resolve is a rule that fails — never a configuration error (as for `deny`).
            RuleKind::Deny { .. } | RuleKind::Require(_) | RuleKind::Leaf | RuleKind::Sealed => {
                None
            }
        };
        if let Some(names) = names {
            for name in names {
                if graph.lookup_name(name).is_none() {
                    return Err(ConfigError {
                        message: unknown_package_message(&format!("rules.{}", rule.package), name),
                        span: Some(rule.span.clone()),
                    });
                }
            }
        }

        if let Some(features) = &rule.features
            && let Selection::List(requested) = &features.selection
        {
            let undeclared = features::first_undeclared_feature(graph, package_node, requested)
                .map_err(|error| ConfigError {
                    message: error.to_string(),
                    span: Some(features.span.clone()),
                })?;
            if let Some(feature) = undeclared {
                return Err(ConfigError {
                    message: format!(
                        "rules.{}.features references unknown feature `{feature}`",
                        rule.package
                    ),
                    span: Some(features.span.clone()),
                });
            }
        }

        if matches!(rule.kind, RuleKind::Direct(_)) {
            let deps = graph.declared_deps(package_node).map_err(|error| ConfigError {
                message: error.to_string(),
                span: Some(rule.span.clone()),
            })?;
            if let Some(dep) = deps.iter().find(|dep| dep.is_normal() && dep.optional) {
                validated.direct_optional_decls += 1;
                validated.warnings.push(format!(
                    "warning: {}: {} declares optional dependency {}; sibling feature unification may add it to the resolved edge set",
                    rule.id,
                    rule.package,
                    dep.name.as_ref(),
                ));
            }
        }
    }

    all_features_guard(validated, graph)
}

/// Rejects a document a feature-aware rule cannot be evaluated on soundly.
///
/// An activation walk is a *subset* of the document's unified closure only when every member
/// was resolved with all of its own features; otherwise an edge the walk would activate may
/// simply not be in the resolve, and a `deny` rule would pass because the edge was missing
/// rather than because the features it needs are off — a false pass, the worst failure this
/// tool can produce. The premise is verifiable from the document itself, so it is checked
/// rather than assumed, and only for a policy that opts in.
fn all_features_guard(validated: &Validated, graph: &Graph<'_>) -> Result<(), ConfigError> {
    let Some(rule) = validated.config.rules.iter().find(|rule| rule.features.is_some()) else {
        return Ok(());
    };
    let span = rule.features.as_ref().map(|features| features.span.clone());
    let unactivated = features::first_unactivated_member(graph)
        .map_err(|error| ConfigError { message: error.to_string(), span: span.clone() })?;
    let Some(member) = unactivated else {
        return Ok(());
    };
    Err(ConfigError {
        message: format!(
            "feature-aware rules need a graph resolved with all features; member {} has {} \
             unactivated feature(s) — re-run with --all-features",
            member.package, member.unactivated
        ),
        span,
    })
}

fn feature_selection(
    cfg: &RawConfig,
    value: &FeatureValue,
    span: Range<usize>,
) -> Result<FeatureSelection, ConfigError> {
    match value {
        FeatureValue::Named(name) if name == "default" => Ok(FeatureSelection::Default),
        FeatureValue::Named(name) if name == "all" => Ok(FeatureSelection::All),
        FeatureValue::Named(name) => Err(config_error(
            cfg,
            span.start,
            format!("graph.features must be `default`, `all`, or a feature list (got `{name}`)"),
        )),
        FeatureValue::List(features) => Ok(FeatureSelection::List(features.clone())),
    }
}

/// Resolves `[graph].platform` into the selection the graph is built against.
///
/// This is public because the pipeline needs the selection **before** the graph exists: the
/// filter is applied while the CSR is laid out, and a discovered `depgate.toml` is only found
/// once `cargo metadata` has reported the workspace root. Unlike `[graph].features`, that
/// ordering costs nothing — a platform narrows an already-resolved document rather than
/// shaping the `cargo metadata` invocation, so a discovered file can select one just as an
/// explicit `--config` can. Phase A re-derives the same value, so a configuration that fails
/// here fails validation too, with the identical message and span.
///
/// # Errors
///
/// Returns [`ConfigError`] anchored at the offending value — the scalar for `platform = "..."`,
/// the individual array entry for a list — when it is neither `all`, `host`, nor a target
/// triple rustc knows, or when the list is empty.
pub fn platform_selection(cfg: &RawConfig) -> Result<PlatformSelection, ConfigError> {
    let span = cfg.graph.platform.span();
    match cfg.graph.platform.get_ref() {
        PlatformValue::Named(name) => PlatformSelection::resolve(std::slice::from_ref(name))
            .map_err(|error| config_error(cfg, span.start, graph_platform_message(&error))),
        PlatformValue::List(triples) if triples.is_empty() => Err(config_error(
            cfg,
            span.start,
            "graph.platform must name at least one target triple; use `all` to keep every \
             platform",
        )),
        PlatformValue::List(triples) => PlatformSelection::resolve(triples).map_err(|error| {
            let range = array_entry_range(cfg, span.clone(), error.index);
            config_error(cfg, range.start, graph_platform_message(&error))
        }),
    }
}

/// The `graph.platform` wording of an unresolvable platform, so the key that carries the
/// value is named whichever form it was written in.
fn graph_platform_message(error: &platform::UnknownPlatform) -> String {
    format!("graph.platform: {error}")
}

fn compile_patterns(
    cfg: &RawConfig,
    patterns: &[String],
    span: Range<usize>,
) -> Result<GlobSet, ConfigError> {
    let mut builder = GlobSetBuilder::new();
    for (index, pattern) in patterns.iter().enumerate() {
        let glob = Glob::new(pattern).map_err(|error| {
            let range = array_entry_range(cfg, span.clone(), index);
            config_error(cfg, range.start, error.to_string())
        })?;
        builder.add(glob);
    }
    builder.build().map_err(|error| config_error(cfg, span.start, error.to_string()))
}

/// Whether a `deny` or `require` entry is a glob rather than a literal name.
///
/// The single definition of the grammar the two rule kinds share, so a value that is a
/// glob for one is never a literal name for the other.
fn is_glob(value: &str) -> bool {
    value.contains(['*', '?', '['])
}

/// Compiles one entry's glob, anchoring the error at that array entry.
fn compile_entry_glob(
    cfg: &RawConfig,
    value: &str,
    span: &Range<usize>,
    index: usize,
) -> Result<Glob, ConfigError> {
    Glob::new(value).map_err(|error| {
        let range = array_entry_range(cfg, span.clone(), index);
        config_error(cfg, range.start, error.to_string())
    })
}

fn compile_deny(
    cfg: &RawConfig,
    raw: &[String],
    span: Range<usize>,
) -> Result<(BTreeSet<String>, GlobSet), ConfigError> {
    let mut exact = BTreeSet::new();
    let mut builder = GlobSetBuilder::new();
    for (index, value) in raw.iter().enumerate() {
        if is_glob(value) {
            builder.add(compile_entry_glob(cfg, value, &span, index)?);
        } else {
            exact.insert(value.clone());
        }
    }
    let globs =
        builder.build().map_err(|error| config_error(cfg, span.start, error.to_string()))?;
    Ok((exact, globs))
}

/// Compiles `require` entries, keeping declaration order and one matcher per entry.
fn compile_require(
    cfg: &RawConfig,
    raw: &[String],
    span: Range<usize>,
) -> Result<Vec<RequirePattern>, ConfigError> {
    raw.iter()
        .enumerate()
        .map(|(index, value)| {
            if is_glob(value) {
                let glob = compile_entry_glob(cfg, value, &span, index)?;
                Ok(RequirePattern::Glob(Box::new(glob.compile_matcher())))
            } else {
                Ok(RequirePattern::Exact(value.clone()))
            }
        })
        .collect()
}

fn config_error(cfg: &RawConfig, offset: usize, message: impl Into<String>) -> ConfigError {
    ConfigError { message: message.into(), span: config_error_span(cfg, offset) }
}

fn config_error_span(cfg: &RawConfig, offset: usize) -> Option<Span> {
    cfg.source.as_ref().map(|source| source_span(&source.path, &source.text, offset))
}

fn config_span(cfg: &RawConfig, offset: usize) -> Span {
    match &cfg.source {
        Some(source) => source_span(&source.path, &source.text, offset),
        None => Span {
            file: PathBuf::new(),
            line: 1,
            col: u32::try_from(offset.saturating_add(1)).unwrap_or(u32::MAX),
        },
    }
}

/// Converts a byte `offset` into `text` to a 1-based line and character column.
///
/// Offsets past the end clamp to the end of `text`; the column counts `char`s from the
/// preceding newline, so multi-byte UTF-8 before the offset does not inflate it.
pub(crate) fn source_span(path: &Path, text: &str, offset: usize) -> Span {
    let offset = offset.min(text.len());
    let line = text[..offset].bytes().filter(|&byte| byte == b'\n').count() + 1;
    let line_start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
    let col = text[line_start..offset].chars().count() + 1;
    Span {
        file: path.to_path_buf(),
        line: u32::try_from(line).unwrap_or(u32::MAX),
        col: u32::try_from(col).unwrap_or(u32::MAX),
    }
}

fn array_entry_range(cfg: &RawConfig, span: Range<usize>, index: usize) -> Range<usize> {
    let Some(source) = &cfg.source else {
        return span;
    };
    let bytes = source.text.as_bytes();
    let mut cursor = span.start.min(bytes.len());
    let end = span.end.min(bytes.len());
    while cursor < end && bytes[cursor] != b'[' {
        cursor += 1;
    }
    if cursor == end {
        return span;
    }
    cursor += 1;
    for item in 0..=index {
        skip_array_trivia(bytes, &mut cursor, end);
        if cursor >= end || bytes[cursor] == b']' {
            return span;
        }
        let item_start = cursor;
        if bytes[cursor] == b'\'' || bytes[cursor] == b'"' {
            let quote = bytes[cursor];
            cursor += 1;
            while cursor < end {
                if bytes[cursor] == b'\\' && quote == b'"' {
                    cursor = cursor.saturating_add(2);
                    continue;
                }
                if bytes[cursor] == quote {
                    cursor += 1;
                    break;
                }
                cursor += 1;
            }
        } else {
            while cursor < end && bytes[cursor] != b',' && bytes[cursor] != b']' {
                cursor += 1;
            }
            while cursor > item_start && bytes[cursor - 1].is_ascii_whitespace() {
                cursor -= 1;
            }
        }
        if item == index {
            return item_start..cursor;
        }
        skip_array_trivia(bytes, &mut cursor, end);
        if cursor < end && bytes[cursor] == b',' {
            cursor += 1;
        }
    }
    span
}

fn skip_array_trivia(bytes: &[u8], cursor: &mut usize, end: usize) {
    loop {
        while *cursor < end && bytes[*cursor].is_ascii_whitespace() {
            *cursor += 1;
        }
        if *cursor < end && bytes[*cursor] == b'#' {
            while *cursor < end && bytes[*cursor] != b'\n' {
                *cursor += 1;
            }
            continue;
        }
        break;
    }
}

fn default_true() -> bool {
    true
}

fn default_feature_value() -> FeatureValue {
    FeatureValue::Named("default".to_owned())
}

fn default_spanned_true() -> Spanned<bool> {
    Spanned::new(0..0, true)
}

fn default_spanned_strings() -> Spanned<Vec<String>> {
    Spanned::new(0..0, Vec::new())
}

fn default_spanned_feature_value() -> Spanned<FeatureValue> {
    Spanned::new(0..0, default_feature_value())
}

fn default_platform_value() -> PlatformValue {
    PlatformValue::Named(platform::ALL.to_owned())
}

fn default_spanned_platform_value() -> Spanned<PlatformValue> {
    Spanned::new(0..0, default_platform_value())
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
