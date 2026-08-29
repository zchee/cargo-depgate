#![expect(clippy::expect_used, reason = "test bodies assert directly")]

use std::{path::PathBuf, time::Duration};

use clap::error::ErrorKind;

use super::*;
use crate::{
    config::Span,
    manifest::{ManifestReport, ManifestViolation},
    rules::RuleStatus,
    timings::{Counters, Timings},
};

fn parse(arguments: &[&str]) -> Args {
    parse_from(arguments.iter().copied()).expect("arguments should parse")
}

fn common_args(args: &Args) -> &CommonArgs {
    match &args.command {
        Some(Command::Check(common)) => common,
        Some(Command::Explain(explain)) => &explain.common,
        Some(Command::Schema) => panic!("schema has no common arguments"),
        None => &args.check,
    }
}

#[test]
fn direct_and_cargo_plugin_invocations_parse_identically() {
    let direct = parse(&["cargo-depgate", "check"]);
    let cargo_plugin = parse(&["cargo-depgate", "depgate", "check"]);

    assert_eq!(direct, cargo_plugin);
}

#[test]
fn omitted_subcommand_defaults_to_check() {
    let implicit = parse(&["cargo-depgate"]);
    let explicit = parse(&["cargo-depgate", "check"]);

    assert_eq!(common_args(&implicit), common_args(&explicit));
}

#[test]
fn empty_argv_uses_the_direct_program_name_and_default_command() {
    let empty = parse_from(std::iter::empty::<OsString>()).expect("empty argv should parse");
    let explicit = parse(&["cargo-depgate", "check"]);

    assert_eq!(common_args(&empty), common_args(&explicit));
}

#[test]
fn implicit_check_accepts_common_options() {
    let args = parse(&["cargo-depgate", "--config", "x.toml"]);

    assert!(args.command.is_none());
    assert_eq!(common_args(&args).config, Some(PathBuf::from("x.toml")));
}

#[test]
fn cargo_plugin_token_is_removed_before_explain_parsing() {
    let direct = parse(&["cargo-depgate", "explain", "a", "b"]);
    let cargo_plugin = parse(&["cargo-depgate", "depgate", "explain", "a", "b"]);

    assert_eq!(cargo_plugin, direct);
}

#[test]
fn check_defaults_match_the_p0_contract() {
    let args = parse(&["cargo-depgate", "check"]);
    let common = common_args(&args);

    assert_eq!(common.cargo_timeout, DEFAULT_TIMEOUT_SECS);
    assert_eq!(common.cargo_timeout, 300, "the P0 contract pins the default at 300 s");
    assert!(args.locked());
    assert_eq!(common.format, None);
}

#[test]
fn cargo_timeout_zero_is_rejected_and_one_is_the_minimum() {
    let error = parse_from(["cargo-depgate", "check", "--cargo-timeout", "0"])
        .expect_err("a zero timeout would fire before cargo can answer");

    assert_eq!(error.kind(), ErrorKind::ValueValidation, "{error}");
    assert!(error.to_string().contains("--cargo-timeout"), "{error}");

    let one = parse(&["cargo-depgate", "check", "--cargo-timeout", "1"]);
    assert_eq!(common_args(&one).cargo_timeout, 1);
}

#[test]
fn no_locked_disables_the_effective_locked_setting() {
    let explicit = parse(&["cargo-depgate", "check", "--no-locked"]);
    let implicit = parse(&["cargo-depgate", "--no-locked"]);

    assert!(!explicit.locked());
    assert!(!implicit.locked());
}

#[test]
fn metadata_json_dash_means_stdin_and_any_other_value_is_a_file() {
    let stdin = parse(&["cargo-depgate", "--metadata-json", "-"]);
    let file = parse(&["cargo-depgate", "--metadata-json", "./-"]);

    assert_eq!(common_args(&stdin).metadata_json, Some(MetadataSource::Stdin));
    assert_eq!(common_args(&file).metadata_json, Some(MetadataSource::File(PathBuf::from("./-"))));
}

#[cfg(unix)]
#[test]
fn non_utf8_metadata_json_path_is_a_file() {
    let non_utf8 = parse_from([
        OsString::from("cargo-depgate"),
        OsString::from("--metadata-json"),
        std::os::unix::ffi::OsStringExt::from_vec(vec![b'm', 0xff, b'.', b'j', b's', b'o', b'n']),
    ])
    .expect("non-UTF-8 metadata paths must parse");

    assert!(matches!(common_args(&non_utf8).metadata_json, Some(MetadataSource::File(_))));
}

#[test]
fn locked_and_no_locked_are_mutually_exclusive() {
    let error = parse_from(["cargo-depgate", "check", "--locked", "--no-locked"])
        .expect_err("conflicting lockfile flags must be rejected");

    assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
}

#[test]
fn workspace_root_requires_metadata_json() {
    let error = parse_from(["cargo-depgate", "check", "--workspace-root", "/workspace"])
        .expect_err("workspace root without metadata JSON must be rejected");

    assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
}

#[test]
fn explain_requires_package_and_dependency() {
    for arguments in
        [&["cargo-depgate", "explain"][..], &["cargo-depgate", "explain", "package"][..]]
    {
        let error = parse_from(arguments.iter().copied())
            .expect_err("explain must require exactly two positional arguments");

        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }
}

#[test]
fn help_uses_the_cargo_subcommand_name() {
    let error = parse_from(["cargo-depgate", "--help"])
        .expect_err("help is represented by clap as a successful early exit");
    let help = error.to_string();

    assert_eq!(error.kind(), ErrorKind::DisplayHelp);
    assert!(help.contains("cargo depgate"), "unexpected help output: {help}");
}

#[test]
fn explain_remains_its_named_stub_error() {
    let args = parse(&["cargo-depgate", "explain", "package", "dependency"]);
    let error = run(&args).expect_err("explain remains a P0 stub");

    assert!(matches!(
        error,
        Error::NotYetImplemented { ref subcommand } if subcommand == "explain"
    ));
}

#[test]
fn schema_is_implemented() {
    let args = parse(&["cargo-depgate", "schema"]);

    run(&args).expect("schema should render successfully");
}

#[test]
fn metadata_options_project_every_cargo_facing_flag() {
    let args = parse(&[
        "cargo-depgate",
        "check",
        "--manifest-path",
        "/ws/Cargo.toml",
        "--features",
        "pkg/feat",
        "--features",
        "other",
        "--all-features",
        "--no-default-features",
        "--offline",
        "--no-locked",
        "--cargo-timeout",
        "7",
        "--metadata-json",
        "meta.json",
        "--workspace-root",
        "/checkout",
    ]);

    let options = args.metadata_options().expect("check has metadata options");

    assert_eq!(
        options,
        MetadataOptions {
            cargo: None,
            manifest_path: Some(PathBuf::from("/ws/Cargo.toml")),
            features: vec!["pkg/feat".to_owned(), "other".to_owned()],
            all_features: true,
            no_default_features: true,
            offline: true,
            locked: false,
            timeout: Duration::from_secs(7),
            source: Some(MetadataSource::File(PathBuf::from("meta.json"))),
            workspace_root: Some(PathBuf::from("/checkout")),
        }
    );
}

#[test]
fn metadata_options_defaults_match_the_library_defaults_and_schema_has_none() {
    let implicit = parse(&["cargo-depgate"]);
    let explain = parse(&["cargo-depgate", "explain", "a", "b", "--metadata-json", "-"]);
    let schema = parse(&["cargo-depgate", "schema"]);

    assert_eq!(implicit.metadata_options(), Some(MetadataOptions::default()));
    let explain = explain.metadata_options().expect("explain shares the common flags");
    assert_eq!(explain.source, Some(MetadataSource::Stdin));
    assert!(explain.locked);
    assert_eq!(schema.metadata_options(), None);
    assert!(schema.locked());
}

#[test]
fn plain_report_prints_one_line_per_manifest_entry_relative_to_the_workspace_root() {
    let workspace_root = PathBuf::from("/ws");
    let entry = |dependency: &str, table: &str, line: u32, col: u32| ManifestViolation {
        package: "app".to_owned(),
        table: table.to_owned(),
        dependency: dependency.to_owned(),
        version: "0.1.0".to_owned(),
        span: Span { file: workspace_root.join("crates/app/Cargo.toml"), line, col },
    };
    let outcome = pipeline::Outcome {
        statuses: vec![
            RuleStatus {
                id: "rules.app.deny".to_owned(),
                package: "app".to_owned(),
                kind: "deny",
                passed: true,
                matched: 0,
            },
            RuleStatus {
                id: manifest::RULE_ID.to_owned(),
                package: String::new(),
                kind: manifest::RULE_KIND,
                passed: false,
                matched: 2,
            },
        ],
        violations: Vec::new(),
        manifest: Some(ManifestReport {
            entries: vec![
                entry("foo", "dependencies", 7, 36),
                entry("baz", "target.'cfg(unix)'.dependencies", 19, 36),
            ],
            manifests_scanned: 1,
            bytes_scanned: 400,
        }),
        warnings: Vec::new(),
        workspace_root,
        counters: Counters { rules: 2, violations: 1, ..Counters::default() },
        timings: Timings::start(),
        exit: 1,
    };

    let mut out = Vec::new();
    render_plain_report(&outcome, &mut out);

    let rendered = String::from_utf8(out).expect("the report is UTF-8");
    assert_eq!(
        rendered,
        "ok rules.app.deny\n\
         FAIL manifest.versions-in-root: crates/app/Cargo.toml:7:36 dependencies foo = \"0.1.0\"\n\
         FAIL manifest.versions-in-root: crates/app/Cargo.toml:19:36 target.'cfg(unix)'.dependencies baz = \"0.1.0\"\n\
         FAIL: 2 rules, 1 violations\n"
    );
}

#[test]
fn plain_report_marks_a_clean_manifest_rule_ok() {
    let outcome = pipeline::Outcome {
        statuses: vec![RuleStatus {
            id: manifest::RULE_ID.to_owned(),
            package: String::new(),
            kind: manifest::RULE_KIND,
            passed: true,
            matched: 0,
        }],
        violations: Vec::new(),
        manifest: Some(ManifestReport::default()),
        warnings: Vec::new(),
        workspace_root: PathBuf::from("/ws"),
        counters: Counters { rules: 1, ..Counters::default() },
        timings: Timings::start(),
        exit: 0,
    };

    let mut out = Vec::new();
    render_plain_report(&outcome, &mut out);

    assert_eq!(
        String::from_utf8(out).expect("the report is UTF-8"),
        "ok manifest.versions-in-root\nok: 1 rules, 0 violations\n"
    );
}
