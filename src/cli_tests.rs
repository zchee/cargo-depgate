#![expect(clippy::expect_used, reason = "test bodies assert directly")]

use clap::error::ErrorKind;

use super::*;

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

    assert_eq!(common.cargo_timeout, 300);
    assert!(args.locked());
    assert_eq!(common.format, None);
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
    let non_utf8 = parse_from([
        OsString::from("cargo-depgate"),
        OsString::from("--metadata-json"),
        std::os::unix::ffi::OsStringExt::from_vec(vec![b'm', 0xff, b'.', b'j', b's', b'o', b'n']),
    ])
    .expect("non-UTF-8 metadata paths must parse");

    assert_eq!(common_args(&stdin).metadata_json, Some(MetadataSource::Stdin));
    assert_eq!(common_args(&file).metadata_json, Some(MetadataSource::File(PathBuf::from("./-"))));
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
fn every_p0_subcommand_returns_its_named_stub_error() {
    for (arguments, expected_name) in [
        (&["cargo-depgate"][..], "check"),
        (&["cargo-depgate", "--offline"][..], "check"),
        (&["cargo-depgate", "check"][..], "check"),
        (&["cargo-depgate", "explain", "package", "dependency"][..], "explain"),
        (&["cargo-depgate", "schema"][..], "schema"),
    ] {
        let args = parse(arguments);
        let error = run(&args).expect_err("P0 commands must remain parse-only stubs");

        assert!(matches!(
            error,
            Error::NotYetImplemented { ref subcommand } if subcommand == expected_name
        ));
    }
}
