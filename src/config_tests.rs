#![expect(clippy::expect_used, reason = "test bodies assert directly")]

use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use schemars::schema_for;

use super::*;
use crate::{
    error::Error,
    graph::Graph,
    metadata::{Meta, MetadataBuffer, parse},
};

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ws-config-errors").join(name)
}

fn load_fixture(name: &str) -> RawConfig {
    let path = fixture_path(name);
    match load(&path) {
        Ok(loaded) => loaded,
        Err(error) => panic!("{name} should deserialize: {error:?}"),
    }
}

fn configuration_load_error(name: &str) -> (String, Option<Span>) {
    let path = fixture_path(name);
    match load(&path) {
        Err(Error::Configuration { message, span }) => (message, span),
        Ok(_) => panic!("{name} unexpectedly loaded successfully"),
        Err(error) => panic!("{name} returned the wrong error: {error:?}"),
    }
}

fn assert_span(span: Option<&Span>, file: &str, line: u32, col: u32) {
    let actual = span.expect("configuration error has a source span");
    assert_eq!(actual.file, fixture_path(file));
    assert_eq!((actual.line, actual.col), (line, col));
}

/// Parses `json` into a leaked, `'static` [`Meta`] so the graph can borrow it freely.
fn meta(json: &str) -> &'static Meta<'static> {
    let buffer: &'static MetadataBuffer =
        Box::leak(Box::new(MetadataBuffer::from_bytes(json.as_bytes().to_vec())));
    Box::leak(Box::new(parse(buffer).expect("synthetic metadata parses")))
}

fn synthetic_graph() -> Graph<'static> {
    let json = r#"{
      "packages": [
        {"name":"app","version":"1.0.0","id":"path+file:///ws/app#1.0.0","source":null,
         "manifest_path":"/ws/app/Cargo.toml",
         "dependencies":[{"name":"dep","kind":null,"optional":true}]},
        {"name":"dep","version":"1.0.0","id":"registry+https://example.invalid/index#dep@1.0.0",
         "source":"registry+https://example.invalid/index",
         "manifest_path":"/cargo/registry/dep-1.0.0/Cargo.toml","dependencies":[]}
      ],
      "workspace_members": ["path+file:///ws/app#1.0.0"],
      "workspace_root":"/ws",
      "resolve": {
        "nodes": [
          {"id":"path+file:///ws/app#1.0.0","deps":[
            {"name":"dep","pkg":"registry+https://example.invalid/index#dep@1.0.0",
             "dep_kinds":[{"kind":null,"target":null}]}
          ]},
          {"id":"registry+https://example.invalid/index#dep@1.0.0","deps":[]}
        ],
        "root":null
      }
    }"#;
    Graph::build(meta(json)).expect("synthetic graph builds")
}

fn raw_config(text: &str) -> RawConfig {
    toml::from_str(text).expect("configuration parses")
}

fn deny_matches(rule: &Rule, name: &str) -> bool {
    match &rule.kind {
        RuleKind::Deny { exact, globs, .. } => exact.contains(name) || globs.is_match(name),
        _ => false,
    }
}

fn collect_toml_files(path: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(path).expect("fixture directory is readable") {
        let entry = entry.expect("fixture directory entry is readable");
        let entry_path = entry.path();
        if entry_path.is_dir() {
            collect_toml_files(&entry_path, files);
        } else if entry_path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with("depgate") && name.strip_suffix(".toml").is_some())
        {
            files.push(entry_path);
        }
    }
}

#[test]
fn unknown_key_reports_toml_error_and_key_span() {
    let (message, span) = configuration_load_error("unknown-key.toml");

    assert_eq!(
        message,
        "TOML parse error at line 2, column 1\n  |\n2 | mystery = true\n  | ^^^^^^^\nunknown field `mystery`, expected one of `schema`, `graph`, `internal`, `manifest`, `rules`\n"
    );
    // The unknown key starts at line 2, column 1 in unknown-key.toml.
    assert_span(span.as_ref(), "unknown-key.toml", 2, 1);
}

#[test]
fn zero_rules_is_rejected_when_root_versions_are_disabled() {
    let raw = load_fixture("zero-rules.toml");
    let error = validate(&raw, None).expect_err("zero rules must be rejected");

    assert_eq!(error.message, "depgate.toml declares no rules");
    // The zero-offset configuration error is reported at line 1, column 1.
    assert_span(error.span.as_ref(), "zero-rules.toml", 1, 1);
}

#[test]
fn direct_raw_config_schema_error_has_no_source_span() {
    let raw = raw_config("schema = 2\n");
    let error = validate(&raw, None).expect_err("unsupported schema must be rejected");

    assert!(error.span.is_none());
}

#[test]
fn leaf_and_internal_is_rejected() {
    let raw = load_fixture("leaf-and-internal.toml");
    let error = validate(&raw, None).expect_err("conflicting rule kinds must fail");

    assert_eq!(error.message, "rules.foo declares both leaf and internal");
    // The `true` value in `leaf = true` starts at line 4, column 8.
    assert_span(error.span.as_ref(), "leaf-and-internal.toml", 4, 8);
}

#[test]
fn self_reference_is_rejected_at_the_array_entry() {
    let raw = load_fixture("self-reference.toml");
    let error = validate(&raw, None).expect_err("self-reference must fail");

    assert_eq!(error.message, "rules.foo.internal cannot contain the rule package itself (foo)");
    // The first array entry (`foo`) starts at line 4, column 13.
    assert_span(error.span.as_ref(), "self-reference.toml", 4, 13);
}

#[test]
fn unterminated_glob_reports_the_glob_parser_error_and_entry_span() {
    let raw = load_fixture("bad-glob.toml");
    let error = validate(&raw, None).expect_err("unterminated glob must fail");

    assert_eq!(error.message, "error parsing glob 'a[b': unclosed character class; missing ']'");
    // The first glob entry (`"a[b"`) starts at line 4, column 9.
    assert_span(error.span.as_ref(), "bad-glob.toml", 4, 9);
}

#[test]
fn phase_a_error_is_pure_with_or_without_a_graph() {
    let raw = load_fixture("leaf-and-internal.toml");
    let graph = synthetic_graph();
    let without_graph = validate(&raw, None).expect_err("fixture must fail in phase A");
    let with_graph =
        validate(&raw, Some(&graph)).expect_err("phase A must run before graph lookups");

    assert_eq!(without_graph, with_graph);
    assert_eq!(without_graph.message, "rules.foo declares both leaf and internal");
    // The phase-A conflict still points at `true`, line 4, column 8.
    assert_span(without_graph.span.as_ref(), "leaf-and-internal.toml", 4, 8);
}

#[test]
fn rules_follow_toml_declaration_order_within_a_package() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ws-basic/depgate.toml");
    let raw = load(&path).expect("workspace fixture configuration loads");
    let validated = validate(&raw, None).expect("workspace fixture validates in phase A");
    let app_rule_kinds = validated
        .config
        .rules
        .iter()
        .filter(|rule| rule.package == "app")
        .map(|rule| match &rule.kind {
            RuleKind::Deny { .. } => "deny",
            RuleKind::Internal(_) => "internal",
            RuleKind::Leaf => "leaf",
            RuleKind::Direct(_) => "direct",
            RuleKind::Sealed => "sealed",
        })
        .collect::<Vec<_>>();

    assert_eq!(app_rule_kinds, ["deny", "internal"]);
}

#[test]
fn non_member_rule_is_rejected_in_phase_b() {
    let raw = load_fixture("non-member.toml");
    let graph = synthetic_graph();
    let error = validate(&raw, Some(&graph)).expect_err("non-member rule must fail");

    assert_eq!(error.message, "rules.nonexistent-pkg targets non-member workspace package");
    // The deny array value starts at line 4, column 8.
    assert_span(error.span.as_ref(), "non-member.toml", 4, 8);
}

#[test]
fn unknown_direct_package_is_rejected_in_phase_b() {
    let raw = load_fixture("unknown-direct.toml");
    let graph = synthetic_graph();
    let error = validate(&raw, Some(&graph)).expect_err("unknown direct package must fail");

    assert_eq!(error.message, "rules.app references unknown package `totally-unknown-name`");
    // The direct array value starts at line 4, column 10.
    assert_span(error.span.as_ref(), "unknown-direct.toml", 4, 10);
}

#[test]
fn exact_and_glob_denies_have_distinct_matching_semantics() {
    let exact = raw_config(
        r#"schema = 1

[rules.app]
deny = ["ratatui"]
"#,
    );
    let exact_config = validate(&exact, None).expect("exact deny validates").config;
    let exact_rule = &exact_config.rules[0];
    assert!(deny_matches(exact_rule, "ratatui"));
    assert!(!deny_matches(exact_rule, "ratatui-widgets"));

    let glob = raw_config(
        r#"schema = 1

[rules.app]
deny = ["ratatui*"]
"#,
    );
    let glob_config = validate(&glob, None).expect("glob deny validates").config;
    let glob_rule = &glob_config.rules[0];
    assert!(deny_matches(glob_rule, "ratatui"));
    assert!(deny_matches(glob_rule, "ratatui-widgets"));
}

#[test]
fn direct_optional_dependency_emits_warning_and_counter() {
    let raw = raw_config(
        r#"schema = 1

[rules.app]
direct = ["dep"]
"#,
    );
    let graph = synthetic_graph();
    let validated = validate(&raw, Some(&graph)).expect("optional direct rule validates");

    assert_eq!(validated.direct_optional_decls, 1);
    assert_eq!(validated.warnings.len(), 1);
    assert_eq!(
        validated.warnings[0],
        "warning: rules.app.direct: app declares optional dependency dep; sibling feature unification may add it to the resolved edge set"
    );
}

#[test]
fn all_toml_fixtures_deserialize_into_schema_types() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut files = Vec::new();
    collect_toml_files(&root, &mut files);
    files.sort();
    assert!(!files.is_empty(), "fixture walk should discover TOML files");

    for path in files {
        let text = fs::read_to_string(&path).expect("fixture is readable");
        let _: RawConfig = toml::from_str(&text).expect("fixture should deserialize as RawConfig");
        let _: ConfigSchema = toml::from_str(&text).expect("raw-valid fixture has schema shape");
    }
}

#[test]
fn absent_sections_use_semantic_defaults_for_both_config_types() {
    let raw: RawConfig = toml::from_str("schema = 1").expect("minimal config parses as RawConfig");
    assert!(matches!(
        raw.graph.features.get_ref(),
        FeatureValue::Named(name) if name == "default"
    ));
    assert!(*raw.internal.members.get_ref());
    assert!(raw.internal.patterns.get_ref().is_empty());
    assert!(*raw.manifest.versions_in_root.get_ref());

    let schema: ConfigSchema =
        toml::from_str("schema = 1").expect("minimal config parses as ConfigSchema");
    assert!(matches!(
        schema.graph.features,
        FeatureValue::Named(name) if name == "default"
    ));
    assert!(schema.internal.members);
    assert!(schema.internal.patterns.is_empty());
    assert!(schema.manifest.versions_in_root);
}

#[test]
fn generated_schema_contains_rules_property() {
    let schema = schema_for!(ConfigSchema).to_value();
    assert!(
        schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|properties| properties.contains_key("rules"))
    );
}

#[test]
fn generated_schema_top_level_properties_match_raw_config_fields() {
    let schema = schema_for!(ConfigSchema).to_value();
    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .expect("generated schema has top-level properties");
    let actual: BTreeSet<_> = properties.keys().map(String::as_str).collect();
    let expected: BTreeSet<_> = RAW_CONFIG_FIELD_NAMES.iter().copied().collect();

    assert_eq!(actual, expected);
}

#[test]
fn rules_preserve_toml_table_order() {
    let raw = raw_config(
        r#"schema = 1

[rules.zeta]
deny = ["a"]

[rules.alpha]
leaf = true

[rules.middle]
sealed = true
"#,
    );
    let validated = validate(&raw, None).expect("ordered rules validate");
    let packages: Vec<_> =
        validated.config.rules.iter().map(|rule| rule.package.as_str()).collect();

    assert_eq!(packages, ["zeta", "alpha", "middle"]);
}
