#![expect(clippy::expect_used, reason = "test bodies assert directly")]

use std::{fs, path::Path};

use tempfile::tempdir;

use super::*;

const MEMBER_PATH: &str = "/ws/crates/app/Cargo.toml";

fn member() -> ManifestInput {
    ManifestInput::new("app", MEMBER_PATH)
}

fn scan(text: &str) -> Vec<ManifestViolation> {
    scan_manifest(&member(), text).expect("the manifest should parse")
}

/// `(table, dependency, version, line, col)` for every entry, in report order.
fn located(entries: &[ManifestViolation]) -> Vec<(&str, &str, &str, u32, u32)> {
    entries
        .iter()
        .map(|entry| {
            assert_eq!(entry.package, "app");
            assert_eq!(entry.span.file, Path::new(MEMBER_PATH));
            (
                entry.table.as_str(),
                entry.dependency.as_str(),
                entry.version.as_str(),
                entry.span.line,
                entry.span.col,
            )
        })
        .collect()
}

#[test]
fn ac6_string_table_and_target_forms_report_the_version_value_position() {
    let text = "[package]\n\
                name = \"app\"\n\
                version = \"0.1.0\"\n\
                \n\
                [dependencies]\n\
                foo = \"1\"\n\
                \n\
                [dev-dependencies]\n\
                bar = { version = \"1\" }\n\
                \n\
                [target.'cfg(unix)'.dependencies]\n\
                baz = { version = \"2\", optional = true }\n";

    let entries = scan(text);

    assert_eq!(
        located(&entries),
        vec![
            ("dependencies", "foo", "1", 6, 7),
            ("dev-dependencies", "bar", "1", 9, 19),
            ("target.'cfg(unix)'.dependencies", "baz", "2", 12, 19),
        ]
    );
}

#[test]
fn package_version_is_not_a_dependency_version() {
    let entries = scan("[package]\nname = \"app\"\nversion = \"0.1.0\"\n");

    assert!(entries.is_empty(), "{entries:?}");
}

#[test]
fn workspace_path_and_git_entries_without_a_version_pass() {
    let text = "[dependencies]\n\
                ws = { workspace = true }\n\
                ws2.workspace = true\n\
                p = { path = \"../p\" }\n\
                g = { git = \"https://example.invalid/g\", branch = \"main\" }\n\
                renamed = { package = \"other\", path = \"../other\", optional = true }\n\
                \n\
                [dev-dependencies]\n\
                dp = { path = \"../dp\", features = [\"x\"] }\n\
                \n\
                [build-dependencies]\n\
                bg = { git = \"https://example.invalid/bg\" }\n\
                \n\
                [target.'cfg(windows)'.dependencies]\n\
                wp = { path = \"../wp\" }\n";

    let entries = scan(text);

    assert!(entries.is_empty(), "{entries:?}");
}

#[test]
fn path_git_and_workspace_companions_do_not_hide_a_version() {
    let text = "[dependencies]\n\
                pv = { path = \"../pv\", version = \"0.1.0\" }\n\
                gv = { git = \"https://example.invalid/gv\", version = \"2\" }\n\
                wv = { workspace = true, version = \"3\" }\n";

    let entries = scan(text);

    assert_eq!(
        located(&entries),
        vec![
            ("dependencies", "pv", "0.1.0", 2, 34),
            ("dependencies", "gv", "2", 3, 54),
            ("dependencies", "wv", "3", 4, 36),
        ]
    );
}

#[test]
fn array_of_tables_after_dev_dependencies_is_not_misattributed() {
    let text = "[dev-dependencies]\n\
                bar = { version = \"1\" }\n\
                \n\
                [[test]]\n\
                name = \"pane_lifecycle\"\n\
                harness = false\n\
                \n\
                [[test]]\n\
                name = \"pane_env\"\n\
                harness = false\n\
                \n\
                [[bin]]\n\
                name = \"tool\"\n\
                path = \"src/bin/tool.rs\"\n\
                \n\
                [[bench]]\n\
                name = \"speed\"\n\
                harness = false\n\
                \n\
                [build-dependencies]\n\
                bb = \"2\"\n";

    let entries = scan(text);

    assert_eq!(
        located(&entries),
        vec![("dev-dependencies", "bar", "1", 2, 19), ("build-dependencies", "bb", "2", 21, 6)]
    );
}

#[test]
fn workspace_dependencies_in_the_owning_manifest_are_never_flagged() {
    let text = "[workspace]\n\
                members = [\"crates/*\"]\n\
                \n\
                [workspace.dependencies]\n\
                x = \"1\"\n\
                y = { version = \"2\", path = \"crates/y\" }\n\
                z.version = \"3\"\n";

    let entries = scan(text);

    assert!(entries.is_empty(), "{entries:?}");
}

#[test]
fn root_package_dependencies_are_checked_but_its_workspace_table_is_not() {
    let text = "[package]\n\
                name = \"root\"\n\
                version = \"0.1.0\"\n\
                \n\
                [workspace]\n\
                members = [\"crates/*\"]\n\
                \n\
                [workspace.dependencies]\n\
                x = { path = \"crates/x\", version = \"0.1.0\" }\n\
                \n\
                [dependencies]\n\
                x = { workspace = true }\n\
                y = { path = \"crates/y\", version = \"0.1.0\" }\n\
                \n\
                [dev-dependencies]\n\
                z = \"1\"\n";

    let entries = scan(text);

    assert_eq!(
        located(&entries),
        vec![("dependencies", "y", "0.1.0", 13, 36), ("dev-dependencies", "z", "1", 16, 5)]
    );
}

#[test]
fn dotted_keys_and_sub_tables_are_the_table_form() {
    let text = "[dependencies]\n\
                dotted.version = \"3\"\n\
                dotted.path = \"../dotted\"\n\
                \n\
                [dependencies.sub]\n\
                path = \"../sub\"\n\
                version = \"6\"\n\
                \n\
                [target.'cfg(unix)'.dependencies.deep]\n\
                version = \"7\"\n";

    let entries = scan(text);

    assert_eq!(
        located(&entries),
        vec![
            ("dependencies", "dotted", "3", 2, 18),
            ("dependencies", "sub", "6", 7, 11),
            ("target.'cfg(unix)'.dependencies", "deep", "7", 10, 11),
        ]
    );
}

#[test]
fn underscore_table_aliases_are_accepted() {
    let text = "[dev_dependencies]\n\
                a = \"1\"\n\
                \n\
                [build_dependencies]\n\
                b = { version = \"2\" }\n\
                \n\
                [target.'cfg(unix)'.dev_dependencies]\n\
                c = \"3\"\n";

    let entries = scan(text);

    assert_eq!(
        located(&entries),
        vec![
            ("dev-dependencies", "a", "1", 2, 5),
            ("build-dependencies", "b", "2", 5, 17),
            ("target.'cfg(unix)'.dev-dependencies", "c", "3", 8, 5),
        ]
    );
}

#[test]
fn target_tables_cover_every_variant_and_label_the_target_as_written() {
    let text = "[target.'cfg(windows)'.dev-dependencies]\n\
                w = \"4\"\n\
                \n\
                [target.x86_64-unknown-linux-gnu.build-dependencies]\n\
                lb = { version = \"5\" }\n\
                \n\
                [target.'cfg(target_os = \"macos\")'.dependencies]\n\
                m = \"6\"\n";

    let entries = scan(text);

    assert_eq!(
        located(&entries),
        vec![
            ("target.'cfg(windows)'.dev-dependencies", "w", "4", 2, 5),
            ("target.x86_64-unknown-linux-gnu.build-dependencies", "lb", "5", 5, 18),
            ("target.'cfg(target_os = \"macos\")'.dependencies", "m", "6", 8, 5),
        ]
    );
}

#[test]
fn quoted_keys_escape_only_when_a_single_quote_forces_a_basic_string() {
    assert_eq!(quote_key("cfg_unix-1"), "cfg_unix-1");
    assert_eq!(quote_key("cfg(unix)"), "'cfg(unix)'");
    assert_eq!(quote_key("cfg(target_os = \"macos\")"), "'cfg(target_os = \"macos\")'");
    assert_eq!(quote_key("it's"), "\"it's\"");
    assert_eq!(quote_key("a'\"\\b"), "\"a'\\\"\\\\b\"");
    assert_eq!(quote_key(""), "''");
}

#[test]
fn columns_count_characters_not_bytes() {
    let text = "[dependencies]\n\
                \"ünï\" = \"1\"\n\
                # 日本語のコメント\n\
                \"日本\" = { version = \"2\" }\n";

    let entries = scan(text);

    assert_eq!(
        located(&entries),
        vec![("dependencies", "ünï", "1", 2, 9), ("dependencies", "日本", "2", 4, 20)]
    );
}

#[test]
fn crlf_line_endings_keep_line_and_column_exact() {
    let entries = scan("[dependencies]\r\nfoo = \"1\"\r\nbar = { version = \"2\" }\r\n");

    assert_eq!(
        located(&entries),
        vec![("dependencies", "foo", "1", 2, 7), ("dependencies", "bar", "2", 3, 19)]
    );
}

#[test]
fn entries_follow_source_order_when_tables_are_declared_out_of_order() {
    let text = "[target.'cfg(unix)'.dependencies]\n\
                t = \"1\"\n\
                \n\
                [dev-dependencies]\n\
                d = \"2\"\n\
                \n\
                [dependencies]\n\
                n = \"3\"\n";

    let entries = scan(text);

    let lines: Vec<u32> = entries.iter().map(|entry| entry.span.line).collect();
    assert_eq!(lines, vec![2, 5, 8]);
}

#[test]
fn invalid_toml_is_a_parse_error_naming_the_manifest() {
    let error = scan_manifest(&member(), "[dependencies\nfoo = \"1\"\n")
        .expect_err("an unterminated table header must not parse");

    assert_eq!(error.exit_code(), 3);
    match &error {
        Error::ManifestParse { path, source } => {
            assert_eq!(path, Path::new(MEMBER_PATH));
            assert!(source.to_string().contains("line 1"), "{source}");
        }
        other => panic!("expected a manifest parse error, got {other:?}"),
    }
    assert!(error.to_string().contains(MEMBER_PATH), "{error}");
}

#[test]
fn a_dependency_that_is_neither_string_nor_table_is_a_parse_error() {
    let error = scan_manifest(&member(), "[dependencies]\nfoo = 1\n")
        .expect_err("an integer dependency spec is not a Cargo shape");

    assert!(matches!(error, Error::ManifestParse { .. }), "{error:?}");
    assert_eq!(error.exit_code(), 3);
    let rendered = format!("{error}: {}", source_of(&error));
    assert!(rendered.contains("expected a version string or a dependency table"), "{rendered}");
    assert!(rendered.contains("line 2"), "{rendered}");
}

#[test]
fn both_spellings_of_a_table_are_accepted_and_the_hyphenated_one_wins() {
    // Cargo keeps `dev-dependencies` and `dev_dependencies` as two fields and prefers
    // the hyphenated table when both are present; a serde alias would reject the
    // manifest as a duplicate field that Cargo itself accepts.
    let text = "[dev-dependencies]\nserde = \"1\"\n\n[dev_dependencies]\nrand = \"0.8\"\n\n\
                [target.'cfg(unix)'.build-dependencies]\ncc = \"1\"\n\n\
                [target.'cfg(unix)'.build_dependencies]\npkg = \"2\"\n";

    let entries = scan(text);

    assert_eq!(
        located(&entries),
        [
            ("dev-dependencies", "serde", "1", 2, 9),
            ("target.'cfg(unix)'.build-dependencies", "cc", "1", 8, 6),
        ]
    );
}

#[test]
fn a_datetime_dependency_value_is_a_parse_error() {
    let error = scan_manifest(&member(), "[dependencies]\nfoo = 1979-05-27\n")
        .expect_err("a datetime is not a Cargo dependency shape");

    assert!(matches!(error, Error::ManifestParse { .. }), "{error:?}");
    assert_eq!(error.exit_code(), 3);
    let rendered = format!("{error}: {}", source_of(&error));
    assert!(rendered.contains("a datetime"), "{rendered}");
}

#[test]
fn a_non_string_version_is_a_parse_error() {
    let error = scan_manifest(&member(), "[dependencies]\nfoo = { version = 1 }\n")
        .expect_err("an integer version is not a Cargo shape");

    assert!(matches!(error, Error::ManifestParse { .. }), "{error:?}");
    assert_eq!(error.exit_code(), 3);
}

#[test]
fn check_versions_in_root_reads_every_member_in_order_and_counts_bytes() {
    let temp = tempdir().expect("temporary workspace should be creatable");
    let clean = temp.path().join("clean/Cargo.toml");
    let dirty = temp.path().join("dirty/Cargo.toml");
    let clean_text = "[dependencies]\nutil = { workspace = true }\n";
    let dirty_text = "[dev-dependencies]\nd = \"2\"\n\n[dependencies]\nn = \"3\"\n";
    write(&clean, clean_text);
    write(&dirty, dirty_text);

    let report = check_versions_in_root([
        ManifestInput::new("dirty", &dirty),
        ManifestInput::new("clean", &clean),
    ])
    .expect("both manifests should scan");

    assert!(!report.passed());
    assert_eq!(report.manifests_scanned, 2);
    assert_eq!(
        report.bytes_scanned,
        u64::try_from(clean_text.len() + dirty_text.len()).expect("fits")
    );
    let summary: Vec<(&str, &str, u32)> = report
        .entries
        .iter()
        .map(|entry| (entry.package.as_str(), entry.dependency.as_str(), entry.span.line))
        .collect();
    assert_eq!(summary, vec![("dirty", "d", 2), ("dirty", "n", 5)]);
    assert!(report.entries.iter().all(|entry| entry.span.file == dirty));
}

#[test]
fn a_clean_workspace_passes_with_an_empty_report() {
    let temp = tempdir().expect("temporary workspace should be creatable");
    let manifest = temp.path().join("Cargo.toml");
    let text = "[package]\nname = \"only\"\nversion = \"0.1.0\"\n";
    write(&manifest, text);

    let report = check_versions_in_root([ManifestInput::new("only", &manifest)])
        .expect("the manifest should scan");

    assert!(report.passed());
    let bytes_scanned = u64::try_from(text.len()).expect("fits");
    assert_eq!(report, ManifestReport { entries: Vec::new(), manifests_scanned: 1, bytes_scanned });
}

#[test]
fn a_missing_member_manifest_is_a_read_error_not_a_skip() {
    let temp = tempdir().expect("temporary workspace should be creatable");
    let missing = temp.path().join("absent/Cargo.toml");

    let error = check_versions_in_root([ManifestInput::new("absent", &missing)])
        .expect_err("a missing manifest must abort the rule");

    assert_eq!(error.exit_code(), 3);
    match &error {
        Error::ManifestRead { path, source } => {
            assert_eq!(path, &missing);
            assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
        }
        other => panic!("expected a manifest read error, got {other:?}"),
    }
}

#[test]
fn a_parse_error_in_the_second_member_aborts_after_the_first_scanned() {
    let temp = tempdir().expect("temporary workspace should be creatable");
    let good = temp.path().join("good/Cargo.toml");
    let bad = temp.path().join("bad/Cargo.toml");
    write(&good, "[dependencies]\nfoo = \"1\"\n");
    write(&bad, "not toml at all = = =\n");

    let error = check_versions_in_root([
        ManifestInput::new("good", &good),
        ManifestInput::new("bad", &bad),
    ])
    .expect_err("the broken manifest must abort the rule");

    assert!(matches!(&error, Error::ManifestParse { path, .. } if path == &bad), "{error:?}");
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().expect("manifest paths have a parent"))
        .expect("manifest directory should be creatable");
    fs::write(path, text).expect("manifest should be writable");
}

fn source_of(error: &Error) -> String {
    std::error::Error::source(error).map(ToString::to_string).unwrap_or_default()
}
