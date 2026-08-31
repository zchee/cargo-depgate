#![expect(clippy::expect_used, reason = "test bodies assert directly")]

use std::{io::Cursor, time::Duration};

use super::*;
use crate::graph::{Graph, fold_dep_kinds};

/// The shared seven-node fixture (six connected-or-kinded nodes plus one isolated node).
///
/// Nodes (in `packages[]` order):
/// 0 `app` (member, path)          → `lib` (normal), `serde 1.0.0` (normal, renamed `sd`),
///                                   `dev-helper` (dev only), `build-helper` (build only)
/// 1 `lib` (member, path)          → `serde 2.0.0` (normal+build multi-kind, cfg-targeted normal)
/// 2 `serde 1.0.0`                 → (none)
/// 3 `serde 2.0.0`                 → (none)
/// 4 `dev-helper`                  → (none)
/// 5 `build-helper`                → (none)
/// 6 `isolated` (isolated node)    → (none)
pub(crate) const ROOT: &str = "/ws/proj";

pub(crate) fn fixture_json() -> String {
    format!(
        r#"{{
  "packages": [
    {{"name":"app","version":"0.1.0","id":"path+file://{ROOT}/app#0.1.0","source":null,
     "manifest_path":"{ROOT}/app/Cargo.toml",
     "dependencies":[
       {{"name":"lib","kind":null,"optional":false}},
       {{"name":"serde","kind":null,"optional":false,"rename":"sd"}},
       {{"name":"dev-helper","kind":"dev","optional":false}},
       {{"name":"build-helper","kind":"build","optional":false}}
     ]}},
    {{"name":"lib","version":"0.1.0","id":"path+file://{ROOT}/lib#0.1.0","source":null,
     "manifest_path":"{ROOT}/lib/Cargo.toml",
     "dependencies":[{{"name":"serde","kind":null,"optional":false,"target":"cfg(unix)"}}]}},
    {{"name":"serde","version":"1.0.0","id":"registry+https://example.invalid/index#serde@1.0.0",
     "source":"registry+https://example.invalid/index",
     "manifest_path":"/cargo/registry/serde-1.0.0/Cargo.toml","dependencies":[]}},
    {{"name":"serde","version":"2.0.0","id":"registry+https://example.invalid/index#serde@2.0.0",
     "source":"registry+https://example.invalid/index",
     "manifest_path":"/cargo/registry/serde-2.0.0/Cargo.toml","dependencies":[]}},
    {{"name":"dev-helper","version":"0.3.0","id":"registry+https://example.invalid/index#dev-helper@0.3.0",
     "source":"registry+https://example.invalid/index",
     "manifest_path":"/cargo/registry/dev-helper-0.3.0/Cargo.toml","dependencies":[]}},
    {{"name":"build-helper","version":"0.4.0","id":"registry+https://example.invalid/index#build-helper@0.4.0",
     "source":"registry+https://example.invalid/index",
     "manifest_path":"/cargo/registry/build-helper-0.4.0/Cargo.toml","dependencies":[]}},
    {{"name":"isolated","version":"9.9.9","id":"registry+https://example.invalid/index#isolated@9.9.9",
     "source":"registry+https://example.invalid/index",
     "manifest_path":"/cargo/registry/isolated-9.9.9/Cargo.toml","dependencies":[]}}
  ],
  "workspace_members": ["path+file://{ROOT}/app#0.1.0", "path+file://{ROOT}/lib#0.1.0"],
  "workspace_root": "{ROOT}",
  "resolve": {{
    "nodes": [
      {{"id":"registry+https://example.invalid/index#isolated@9.9.9","deps":[]}},
      {{"id":"path+file://{ROOT}/app#0.1.0","deps":[
        {{"name":"lib","pkg":"path+file://{ROOT}/lib#0.1.0","dep_kinds":[{{"kind":null,"target":null}}]}},
        {{"name":"sd","pkg":"registry+https://example.invalid/index#serde@1.0.0","dep_kinds":[{{"kind":null,"target":null}}]}},
        {{"name":"dev_helper","pkg":"registry+https://example.invalid/index#dev-helper@0.3.0","dep_kinds":[{{"kind":"dev","target":null}}]}},
        {{"name":"build_helper","pkg":"registry+https://example.invalid/index#build-helper@0.4.0","dep_kinds":[{{"kind":"build","target":null}}]}}
      ]}},
      {{"id":"path+file://{ROOT}/lib#0.1.0","deps":[
        {{"name":"serde","pkg":"registry+https://example.invalid/index#serde@2.0.0","dep_kinds":[
          {{"kind":"build","target":null}},
          {{"kind":null,"target":"cfg(unix)"}},
          {{"kind":null,"target":"cfg(windows)"}}
        ]}}
      ]}},
      {{"id":"registry+https://example.invalid/index#serde@1.0.0","deps":[]}},
      {{"id":"registry+https://example.invalid/index#serde@2.0.0","deps":[]}},
      {{"id":"registry+https://example.invalid/index#dev-helper@0.3.0","deps":[]}},
      {{"id":"registry+https://example.invalid/index#build-helper@0.4.0","deps":[]}}
    ],
    "root": null
  }}
}}"#
    )
}

pub(crate) fn buffer(json: &str) -> MetadataBuffer {
    MetadataBuffer::from_bytes(json.as_bytes().to_vec())
}

fn parse_str(json: &str) -> Result<Meta<'static>, Error> {
    let buffer: &'static MetadataBuffer = Box::leak(Box::new(buffer(json)));
    parse(buffer)
}

fn invalid_message<T: std::fmt::Debug>(result: Result<T, Error>) -> String {
    match result {
        Err(Error::MetadataInvalid { message }) => message,
        other => panic!("expected MetadataInvalid, got {other:?}"),
    }
}

/// Builds the graph from `json`, returning the fail-closed message on error.
fn build_message(json: &str) -> String {
    let meta = match parse_str(json) {
        Ok(meta) => meta,
        Err(error) => return invalid_message::<()>(Err(error)),
    };
    invalid_message(Graph::build(&meta))
}

#[test]
fn buffer_keeps_sixty_four_zero_bytes_of_padding_out_of_the_data() {
    let buffer = buffer("{}");

    assert_eq!(buffer.as_bytes(), b"{}");
    assert_eq!(buffer.padded_len(), 2 + BUFFER_PADDING);
    assert_eq!(BUFFER_PADDING, 64);
}

#[test]
fn fixture_parses_and_borrows_every_string() {
    let meta = parse_str(&fixture_json()).expect("fixture parses");

    assert_eq!(meta.packages.len(), 7);
    assert_eq!(meta.workspace_members.len(), 2);
    assert_eq!(meta.workspace_root, ROOT);
    assert_eq!(meta.unrebased_path_deps, 0);
    for package in &meta.packages {
        assert!(matches!(package.id, Cow::Borrowed(_)), "id copied: {}", package.id);
        assert!(matches!(package.name, Cow::Borrowed(_)), "name copied: {}", package.name);
        assert!(matches!(package.version, Cow::Borrowed(_)));
        assert!(matches!(package.manifest_path, Cow::Borrowed(_)));
    }
    let resolve = meta.resolve.as_ref().expect("resolve present");
    assert_eq!(resolve.nodes.len(), 7);
    let app = &resolve.nodes[1];
    assert_eq!(app.deps.len(), 4);
    assert!(app.deps.iter().all(|dep| matches!(dep.pkg, Cow::Borrowed(_))));
}

#[test]
fn path_packages_are_detected_by_null_source_and_id_prefix() {
    let meta = parse_str(&fixture_json()).expect("fixture parses");

    assert!(meta.packages[0].is_path());
    assert!(meta.packages[1].is_path());
    assert!(!meta.packages[2].is_path());
}

#[test]
fn dep_kinds_fold_distinguishes_null_build_dev_and_multi_kind_edges() {
    let meta = parse_str(&fixture_json()).expect("fixture parses");
    let resolve = meta.resolve.as_ref().expect("resolve present");
    let app = &resolve.nodes[1];
    let lib = &resolve.nodes[2];

    let normal = fold_dep_kinds(&app.deps[0]).expect("fold");
    assert!(normal.has_normal());
    assert!(!normal.all_normal_targeted());
    assert_eq!((normal.entries, normal.normal, normal.normal_targeted), (1, 1, 0));

    let dev = fold_dep_kinds(&app.deps[2]).expect("fold");
    assert!(!dev.has_normal());
    let build = fold_dep_kinds(&app.deps[3]).expect("fold");
    assert!(!build.has_normal());

    let multi = fold_dep_kinds(&lib.deps[0]).expect("fold");
    assert!(multi.has_normal(), "a build+normal edge is normal");
    assert!(multi.all_normal_targeted(), "every normal entry carries a target");
    assert_eq!((multi.entries, multi.normal, multi.normal_targeted), (3, 2, 2));
}

#[test]
fn dep_kinds_fold_treats_a_missing_kind_as_normal_and_ignores_unknown_keys() {
    let json = r#"[{"target":null,"extra":{"nested":[1,2]}},{"kind":"dev"}]"#;
    let raw: &RawValue = serde_json::from_str(json).expect("raw value");
    let dep = Dep { pkg: Cow::Borrowed("x"), dep_kinds: Some(raw) };

    let fold = fold_dep_kinds(&dep).expect("fold");

    assert_eq!((fold.entries, fold.normal, fold.normal_targeted), (2, 1, 0));
}

#[test]
fn dep_kinds_fold_reports_absent_and_malformed_arrays() {
    let absent = Dep { pkg: Cow::Borrowed("x"), dep_kinds: None };
    assert_eq!(fold_dep_kinds(&absent).expect("absent folds to zero").entries, 0);

    let raw: &RawValue = serde_json::from_str(r#"{"kind":null}"#).expect("raw value");
    let not_an_array = Dep { pkg: Cow::Borrowed("x"), dep_kinds: Some(raw) };
    assert!(fold_dep_kinds(&not_an_array).is_err(), "an object is not a dep_kinds array");
}

#[test]
fn resolve_null_fails_closed_in_parse() {
    let json = fixture_json();
    let start = json.find("\"resolve\": {").expect("resolve key");
    let end = json.rfind('}').expect("closing brace");
    let json = format!("{}\"resolve\": null\n{}", &json[..start], &json[end..]);

    let message = invalid_message(parse_str(&json));

    assert!(message.contains("`resolve` is null"), "{message}");
}

#[test]
fn empty_dep_kinds_on_one_edge_fails_closed() {
    let json = fixture_json().replacen(
        r#""dep_kinds":[{"kind":null,"target":null}]"#,
        r#""dep_kinds":[]"#,
        1,
    );

    let message = build_message(&json);

    assert!(message.contains("has no `dep_kinds`"), "{message}");
}

#[test]
fn absent_dep_kinds_on_one_edge_fails_closed() {
    let json = fixture_json().replacen(r#","dep_kinds":[{"kind":null,"target":null}]"#, "", 1);

    let message = build_message(&json);

    assert!(message.contains("has no `dep_kinds`"), "{message}");
}

#[test]
fn unknown_dep_pkg_id_fails_closed() {
    let json = fixture_json().replacen(
        r#""pkg":"path+file:///ws/proj/lib#0.1.0""#,
        r#""pkg":"path+file:///ws/proj/ghost#0.1.0""#,
        1,
    );

    let message = build_message(&json);

    assert!(message.contains("ghost") && message.contains("not in `packages`"), "{message}");
}

#[test]
fn empty_workspace_members_fails_closed() {
    let json = fixture_json().replacen(
        r#""workspace_members": ["path+file:///ws/proj/app#0.1.0", "path+file:///ws/proj/lib#0.1.0"]"#,
        r#""workspace_members": []"#,
        1,
    );

    let message = build_message(&json);

    assert!(message.contains("`workspace_members` is empty"), "{message}");
}

#[test]
fn unknown_workspace_member_fails_closed() {
    let json = fixture_json().replacen(
        r#""workspace_members": ["path+file:///ws/proj/app#0.1.0""#,
        r#""workspace_members": ["path+file:///ws/proj/nope#0.1.0""#,
        1,
    );

    let message = build_message(&json);

    assert!(message.contains("workspace member") && message.contains("nope"), "{message}");
}

#[test]
fn node_without_package_fails_closed() {
    let json = fixture_json().replacen(
        r#"{"id":"registry+https://example.invalid/index#isolated@9.9.9","deps":[]},"#,
        r#"{"id":"registry+https://example.invalid/index#phantom@0.0.1","deps":[]},"#,
        1,
    );

    let message = build_message(&json);

    assert!(message.contains("phantom") && message.contains("no `packages` entry"), "{message}");
}

#[test]
fn package_without_node_fails_closed() {
    let json = fixture_json().replacen(
        r#"{"id":"registry+https://example.invalid/index#isolated@9.9.9","deps":[]},"#,
        "",
        1,
    );

    let message = build_message(&json);

    assert!(message.contains("`resolve.nodes` has 6 entries but `packages` has 7"), "{message}");
}

#[test]
fn duplicate_node_for_one_package_fails_closed() {
    let json = fixture_json().replacen(
        r#"{"id":"registry+https://example.invalid/index#isolated@9.9.9","deps":[]},"#,
        r#"{"id":"registry+https://example.invalid/index#serde@1.0.0","deps":[]},"#,
        1,
    );

    let message = build_message(&json);

    assert!(
        message.contains("lists `registry+https://example.invalid/index#serde@1.0.0` twice"),
        "{message}"
    );
}

#[test]
fn duplicate_package_id_fails_closed() {
    // The package entry's id line ends the line; the resolve node's id is followed by `"deps"`.
    let json = fixture_json().replacen("#isolated@9.9.9\",\n", "#serde@1.0.0\",\n", 1);

    let message = build_message(&json);

    assert!(message.contains("duplicate package id"), "{message}");
}

#[test]
fn malformed_json_is_unparseable_not_invalid() {
    let result = parse_str("{\"packages\": [");

    assert!(matches!(result, Err(Error::CargoMetadataUnparseable { .. })), "{result:?}");
}

#[test]
fn stdin_reader_is_read_to_end_into_a_padded_buffer() {
    let json = fixture_json();
    let reader = Cursor::new(json.clone().into_bytes());

    let buffer = read_source(reader, Path::new("-"), 0).expect("stdin-style read");

    assert_eq!(buffer.as_bytes(), json.as_bytes());
    assert_eq!(buffer.padded_len(), json.len() + BUFFER_PADDING);
    let meta = parse(&buffer).expect("parses from the stdin buffer");
    assert_eq!(meta.packages.len(), 7);
}

#[test]
fn missing_metadata_file_reports_its_path() {
    let options = MetadataOptions {
        source: Some(MetadataSource::File(PathBuf::from("/nonexistent/depgate/metadata.json"))),
        ..MetadataOptions::default()
    };

    let error = acquire(&options).expect_err("a missing file must fail");

    assert!(
        matches!(error, Error::MetadataRead { ref path, .. } if path.ends_with("metadata.json"))
    );
    assert_eq!(error.exit_code(), 3);
}

#[test]
fn file_source_reads_and_applies_the_workspace_root_override() {
    let dir = tempdir("file-source");
    let path = dir.join("metadata.json");
    std::fs::write(&path, fixture_json()).expect("write fixture");
    let options = MetadataOptions {
        source: Some(MetadataSource::File(path)),
        workspace_root: Some(PathBuf::from("/elsewhere/checkout/")),
        ..MetadataOptions::default()
    };

    let buffer = acquire(&options).expect("file read");
    let meta = parse(&buffer).expect("parse");

    assert_eq!(meta.workspace_root, "/elsewhere/checkout");
    assert_eq!(meta.packages[0].manifest_path, "/elsewhere/checkout/app/Cargo.toml");
    assert!(matches!(meta.packages[0].manifest_path, Cow::Owned(_)));
    assert_eq!(meta.packages[1].manifest_path, "/elsewhere/checkout/lib/Cargo.toml");
    assert_eq!(meta.packages[2].manifest_path, "/cargo/registry/serde-1.0.0/Cargo.toml");
    assert_eq!(meta.unrebased_path_deps, 0);
    std::fs::remove_dir_all(dir).expect("cleanup");
}

#[test]
fn rebase_onto_the_filesystem_root_does_not_double_the_slash() {
    let mut meta = parse_str(&fixture_json()).expect("fixture parses");

    meta.rebase(Path::new("/")).expect("rebase onto /");

    assert_eq!(meta.workspace_root, "/");
    assert_eq!(meta.packages[0].manifest_path, "/app/Cargo.toml");
}

#[test]
fn rebase_is_slash_separated_not_a_string_prefix() {
    let json = fixture_json().replacen(
        r#""manifest_path":"/ws/proj/lib/Cargo.toml""#,
        r#""manifest_path":"/ws/proj-sibling/lib/Cargo.toml""#,
        1,
    );
    let mut meta = parse_str(&json).expect("fixture parses");

    let message = invalid_message(meta.rebase(Path::new("/new")));

    assert!(message.contains("workspace member `lib`"), "{message}");
    assert!(message.contains("/ws/proj-sibling/lib/Cargo.toml"), "{message}");
}

#[test]
fn non_member_path_package_outside_the_root_stays_unrebased_and_is_counted() {
    let json = fixture_json().replacen(
        r#""id":"registry+https://example.invalid/index#isolated@9.9.9",
     "source":"registry+https://example.invalid/index",
     "manifest_path":"/cargo/registry/isolated-9.9.9/Cargo.toml""#,
        r#""id":"path+file:///ws/sibling#9.9.9",
     "source":null,
     "manifest_path":"/ws/sibling/Cargo.toml""#,
        1,
    );
    let json = json.replacen(
        r#""id":"registry+https://example.invalid/index#isolated@9.9.9","deps":[]"#,
        r#""id":"path+file:///ws/sibling#9.9.9","deps":[]"#,
        1,
    );
    let buffer = buffer(&json).with_workspace_root(Some(PathBuf::from("/new/root")));

    let meta = parse(&buffer).expect("parse with rebase");
    let graph = Graph::build(&meta).expect("graph builds");

    assert_eq!(meta.unrebased_path_deps, 1);
    assert_eq!(meta.packages[6].manifest_path, "/ws/sibling/Cargo.toml");
    assert_eq!(meta.packages[0].manifest_path, "/new/root/app/Cargo.toml");
    assert_eq!(graph.counters().unrebased_path_deps, 1);
}

#[test]
fn member_outside_the_root_is_a_fail_closed_assertion() {
    let json = fixture_json().replacen(
        r#""manifest_path":"/ws/proj/app/Cargo.toml""#,
        r#""manifest_path":"/ws/outside/app/Cargo.toml""#,
        1,
    );
    let buffer = buffer(&json).with_workspace_root(Some(PathBuf::from("/new/root")));

    let error = parse(&buffer).expect_err("a member outside the root must fail");

    assert_eq!(error.exit_code(), 3);
    let message = invalid_message::<()>(Err(error));
    assert!(message.contains("workspace member `app`"), "{message}");
    assert!(message.contains("/ws/outside/app/Cargo.toml"), "{message}");
}

#[cfg(unix)]
#[test]
fn non_utf8_workspace_root_is_a_usage_error() {
    use std::os::unix::ffi::OsStrExt as _;
    let mut meta = parse_str(&fixture_json()).expect("fixture parses");
    let dir = Path::new(std::ffi::OsStr::from_bytes(b"/bad\xff"));

    let error = meta.rebase(dir).expect_err("non-UTF-8 root must be rejected");

    assert!(matches!(error, Error::Usage { .. }), "{error:?}");
    assert_eq!(error.exit_code(), 2);
}

#[test]
fn json_escaped_manifest_path_lands_in_cow_owned_while_plain_paths_borrow() {
    let json = fixture_json().replacen(
        r#""manifest_path":"/cargo/registry/isolated-9.9.9/Cargo.toml""#,
        r#""manifest_path":"C:\\cargo\\registry\\isolated-9.9.9\\Cargo.toml""#,
        1,
    );

    let meta = parse_str(&json).expect("escaped path parses");

    assert!(matches!(meta.packages[6].manifest_path, Cow::Owned(_)));
    assert_eq!(meta.packages[6].manifest_path, r"C:\cargo\registry\isolated-9.9.9\Cargo.toml");
    assert!(matches!(meta.packages[0].manifest_path, Cow::Borrowed(_)));
}

#[test]
fn cargo_command_forwards_every_flag_verbatim() {
    let options = MetadataOptions {
        cargo: Some(PathBuf::from("/opt/fake/cargo")),
        manifest_path: Some(PathBuf::from("/ws/Cargo.toml")),
        features: vec!["pkg/feat".to_owned(), "other".to_owned()],
        all_features: true,
        no_default_features: true,
        offline: true,
        locked: true,
        ..MetadataOptions::default()
    };

    let command = cargo_command(&options);
    let args: Vec<String> =
        command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();

    assert_eq!(command.get_program(), "/opt/fake/cargo");
    assert_eq!(
        args,
        [
            "metadata",
            "--format-version",
            "1",
            "--all-features",
            "--no-default-features",
            "--manifest-path",
            "/ws/Cargo.toml",
            "--locked",
            "--features",
            "pkg/feat",
            "--features",
            "other",
            "--offline",
        ]
    );
}

#[test]
fn no_locked_omits_the_lock_flag() {
    let options = MetadataOptions { locked: false, ..MetadataOptions::default() };

    let command = cargo_command(&options);
    let args: Vec<String> =
        command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();

    assert_eq!(args, ["metadata", "--format-version", "1"]);
}

#[test]
fn default_options_match_the_cli_defaults() {
    let options = MetadataOptions::default();

    assert!(options.locked);
    assert_eq!(options.timeout, Duration::from_secs(300));
    assert!(options.source.is_none());
}

#[test]
fn spawn_failure_is_reported_as_a_spawn_error() {
    let options = MetadataOptions {
        cargo: Some(PathBuf::from("/nonexistent/depgate/cargo")),
        ..MetadataOptions::default()
    };

    let error = acquire(&options).expect_err("a missing cargo must fail");

    assert!(matches!(error, Error::CargoMetadataSpawn { .. }), "{error:?}");
    assert_eq!(error.exit_code(), 3);
}

#[cfg(unix)]
#[test]
fn fake_cargo_output_is_captured_and_a_non_zero_exit_is_reported() {
    let dir = tempdir("fake-cargo");
    let ok = write_script(&dir, "ok-cargo", "#!/bin/sh\nprintf '%s' '{\"ok\":true}'\n");
    let failing = write_script(&dir, "fail-cargo", "#!/bin/sh\necho 'boom' >&2\nexit 101\n");

    let buffer = acquire(&MetadataOptions { cargo: Some(ok), ..MetadataOptions::default() })
        .expect("the fake cargo succeeds");
    assert_eq!(buffer.as_bytes(), br#"{"ok":true}"#);

    let error = acquire(&MetadataOptions { cargo: Some(failing), ..MetadataOptions::default() })
        .expect_err("a failing cargo must be reported");
    assert!(matches!(error, Error::CargoMetadataFailed { status: Some(101) }), "{error:?}");
    std::fs::remove_dir_all(dir).expect("cleanup");
}

#[cfg(unix)]
#[test]
fn timeout_kills_the_child_and_returns_without_joining_the_reader() {
    use std::time::Instant;

    let dir = tempdir("slow-cargo");
    // `sleep` is a grandchild holding the stdout pipe open: a join on the reader
    // thread would block until it exits, well past the 2 s bound. Its stderr is
    // detached so the lingering grandchild does not hold the test harness's pipe.
    let slow = write_script(&dir, "slow-cargo", "#!/bin/sh\nexec 2>/dev/null\nsleep 5\nexit 0\n");
    let options = MetadataOptions {
        cargo: Some(slow),
        timeout: Duration::from_secs(1),
        ..MetadataOptions::default()
    };

    let started = Instant::now();
    let error = acquire(&options).expect_err("the slow cargo must time out");
    let elapsed = started.elapsed();

    assert!(elapsed < Duration::from_secs(2), "returned after {elapsed:?}");
    assert!(elapsed >= Duration::from_secs(1), "returned before the timeout: {elapsed:?}");
    assert_eq!(error.to_string(), "cargo metadata exceeded --cargo-timeout=1s");
    assert_eq!(error.exit_code(), 3);
    std::fs::remove_dir_all(dir).expect("cleanup");
}

#[cfg(unix)]
#[test]
fn a_cargo_that_closes_stdout_and_keeps_running_is_killed_and_reported_as_a_timeout() {
    use std::time::Instant;

    let dir = tempdir("eof-then-hang-cargo");
    // The document is emitted, stdout is closed so the reader sees EOF, and the process then
    // keeps running with no writer left on the pipe — the shape a stuck credential helper
    // leaves behind. Before the bounded reap this returned only when the sleep ended, well
    // past --cargo-timeout. The emitted bytes are never parsed: the reap expires first.
    let hanging = write_script(
        &dir,
        "eof-then-hang-cargo",
        "#!/bin/sh\nexec 2>/dev/null\nprintf '%s' '{}'\nexec 1>&-\nsleep 30\nexit 0\n",
    );
    let options = MetadataOptions {
        cargo: Some(hanging),
        timeout: Duration::from_secs(1),
        ..MetadataOptions::default()
    };

    let started = Instant::now();
    let error = acquire(&options).expect_err("a cargo that never exits must time out");
    let elapsed = started.elapsed();

    assert_eq!(error.to_string(), "cargo metadata exceeded --cargo-timeout=1s");
    assert_eq!(error.exit_code(), 3);
    // The floor, not the remaining share of the one-second timeout, is what bounds this run.
    assert!(elapsed >= REAP_FLOOR, "returned before the reap floor: {elapsed:?}");
    assert!(elapsed < REAP_FLOOR * 3, "returned after {elapsed:?}");
    std::fs::remove_dir_all(dir).expect("cleanup");
}

#[cfg(unix)]
#[test]
fn the_bounded_reap_returns_the_status_of_a_child_that_has_already_exited() {
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 7"])
        .spawn()
        .expect("the shell should spawn");

    let status = reap_bounded(&mut child, Duration::from_secs(5))
        .expect("waiting on the child should succeed")
        .expect("a child that exits within the budget is reaped");

    assert_eq!(status.code(), Some(7));
}

#[cfg(unix)]
#[test]
fn the_bounded_reap_gives_up_on_a_child_that_outlives_its_budget() {
    use std::time::Instant;

    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "sleep 30"])
        .stderr(Stdio::null())
        .spawn()
        .expect("the shell should spawn");

    let started = Instant::now();
    let outcome = reap_bounded(&mut child, Duration::from_millis(50))
        .expect("polling the child should succeed");
    let elapsed = started.elapsed();

    assert!(outcome.is_none(), "the child was still running: {outcome:?}");
    assert!(elapsed < Duration::from_secs(1), "the budget was not honoured: {elapsed:?}");
    kill_and_reap(&mut child);
}

fn tempdir(label: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let unique = format!(
        "cargo-depgate-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let dir = std::env::temp_dir().join(unique);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[cfg(unix)]
fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join(name);
    std::fs::write(&path, body).expect("write script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

/// The builder is the only way to construct [`MetadataOptions`] outside this crate, so it has
/// to reach every field: a setter that silently dropped its argument would leave downstream
/// callers spawning cargo with different flags than they asked for.
#[test]
fn the_options_builder_reaches_every_field() {
    let built = MetadataOptions::default()
        .with_cargo("/opt/cargo")
        .with_manifest_path("/ws/Cargo.toml")
        .with_features(["app/other"])
        .with_all_features(true)
        .with_no_default_features(true)
        .with_offline(true)
        .with_locked(false)
        .with_timeout(Duration::from_secs(7))
        .with_source(MetadataSource::Stdin)
        .with_workspace_root("/ws");

    assert_eq!(
        built,
        MetadataOptions {
            cargo: Some(PathBuf::from("/opt/cargo")),
            manifest_path: Some(PathBuf::from("/ws/Cargo.toml")),
            features: vec!["app/other".to_owned()],
            all_features: true,
            no_default_features: true,
            offline: true,
            locked: false,
            timeout: Duration::from_secs(7),
            source: Some(MetadataSource::Stdin),
            workspace_root: Some(PathBuf::from("/ws")),
        }
    );
}
