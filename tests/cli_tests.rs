//! Binary-level contract tests for the P0 command-line scaffold.
#![expect(clippy::expect_used, reason = "test bodies assert directly")]

use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;

fn output(arguments: &[&str]) -> Output {
    cargo_bin_cmd!().args(arguments).output().expect("cargo-depgate should execute")
}

#[test]
fn direct_and_cargo_plugin_help_are_identical() {
    let direct = output(&["--help"]);
    let cargo_plugin = output(&["depgate", "--help"]);

    assert!(direct.status.success(), "direct help failed: {direct:?}");
    assert!(cargo_plugin.status.success(), "Cargo-plugin help failed: {cargo_plugin:?}");
    assert_eq!(direct.stdout, cargo_plugin.stdout);

    let stdout = String::from_utf8_lossy(&direct.stdout);
    assert!(stdout.contains("cargo depgate"), "unexpected help output: {stdout}");
}

#[test]
fn help_subcommands_succeed_for_direct_and_cargo_plugin_invocations() {
    for arguments in [&["help"][..], &["depgate", "help"][..]] {
        let output = output(arguments);

        assert!(output.status.success(), "help failed: {output:?}");
        assert!(output.stderr.is_empty(), "help wrote to stderr: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("cargo depgate"),
            "unexpected help output: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn help_check_succeeds_with_usage_on_stdout() {
    let output = output(&["help", "check"]);

    assert!(output.status.success(), "help check failed: {output:?}");
    assert!(output.stderr.is_empty(), "help check wrote to stderr: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("cargo depgate"),
        "unexpected help output: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn unrecognized_flags_fail_on_stderr() {
    let output = output(&["--not-a-flag"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty(), "unexpected stdout: {output:?}");
    assert!(!output.stderr.is_empty(), "expected an error on stderr: {output:?}");
}

#[test]
fn version_uses_the_installed_binary_name() {
    let output = output(&["--version"]);

    assert!(output.status.success(), "version request failed: {output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("cargo-depgate {}\n", env!("CARGO_PKG_VERSION"))
    );
}

/// P0 deliberately keeps both `check` invocation forms as identical parse-only stubs.
///
/// The expected exit code and stderr legitimately change when P1, P2, and P4 implement `check`.
#[test]
fn direct_and_cargo_plugin_check_stubs_are_identical() {
    let direct = output(&["check"]);
    let cargo_plugin = output(&["depgate", "check"]);

    assert_eq!(direct.status.code(), Some(2));
    assert_eq!(cargo_plugin.status.code(), Some(2));
    assert_eq!(direct.stderr, cargo_plugin.stderr);
}
