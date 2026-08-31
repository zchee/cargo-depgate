#![expect(clippy::expect_used, reason = "test bodies assert directly")]

use super::*;

/// The three triples the assertions below name, chosen so that one is never the host: a
/// `cfg(windows)` case that passed only because the test ran on Windows would prove nothing.
const LINUX: &str = "x86_64-unknown-linux-gnu";
const WINDOWS: &str = "x86_64-pc-windows-msvc";
const MACOS: &str = "aarch64-apple-darwin";

fn tokens(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn selection(values: &[&str]) -> PlatformSelection {
    PlatformSelection::resolve(&tokens(values)).expect("the tokens resolve")
}

#[test]
fn the_default_selection_is_every_platform() {
    let default = PlatformSelection::default();

    assert!(default.is_all(), "an unset selection must not narrow anything");
    assert_eq!(default, PlatformSelection::all());
    assert_eq!(default.triples().count(), 0, "`all` names no triple to report");
}

#[test]
fn every_platform_activates_every_target_without_reading_it() {
    let all = PlatformSelection::all();

    // Including values no evaluator could decide: under `all` the string is never inspected,
    // which is what makes the default path free as well as unchanged.
    for target in ["cfg(windows)", "cfg(any())", LINUX, "not a cfg expression at all"] {
        assert!(all.activates(target), "`all` must keep the edge conditioned on {target}");
    }
}

#[test]
fn resolve_accepts_all_host_and_target_triples() {
    assert!(selection(&["all"]).is_all());
    assert_eq!(selection(&["host"]).triples().collect::<Vec<_>>(), vec![host_triple()]);
    assert_eq!(selection(&[LINUX, WINDOWS]).triples().collect::<Vec<_>>(), vec![LINUX, WINDOWS]);
}

#[test]
fn all_beside_a_triple_selects_every_platform() {
    // `all` is the superset of whatever stands next to it, so a neighbour cannot narrow it.
    // Order must not matter: both spellings mean the unfiltered graph.
    assert!(selection(&[LINUX, "all"]).is_all());
    assert!(selection(&["all", LINUX]).is_all());
}

#[test]
fn repeated_triples_collapse_and_keep_first_seen_order() {
    let selection = selection(&[WINDOWS, LINUX, WINDOWS]);

    assert_eq!(
        selection.triples().collect::<Vec<_>>(),
        vec![WINDOWS, LINUX],
        "a report echoes the selection back, so duplicates must not double it"
    );
}

#[test]
fn host_resolves_to_a_triple_rustc_knows() {
    let host = host_triple();

    assert!(
        cfg_expr::targets::get_builtin_target_by_triple(host).is_some(),
        "`host` resolved to {host}, which is not in rustc's built-in target table"
    );
    assert_eq!(
        selection(&["host"]).triples().collect::<Vec<_>>(),
        vec![host],
        "`host` must resolve to the same triple the free function reports"
    );
}

#[test]
fn an_unknown_platform_names_the_offending_value_and_its_position() {
    let error = PlatformSelection::resolve(&tokens(&[LINUX, "x86_64-unknown-linux-gnuu"]))
        .expect_err("a misspelt triple must not resolve");

    // The index is what lets a configuration error underline the offending array entry rather
    // than the whole `platform = [...]` value.
    assert_eq!(error.index, 1);
    assert_eq!(error.value, "x86_64-unknown-linux-gnuu");
    assert!(
        error.to_string().contains("x86_64-unknown-linux-gnuu"),
        "the message must quote the value: {error}"
    );
}

#[test]
fn a_cfg_expression_is_evaluated_against_each_selected_platform() {
    assert!(selection(&[WINDOWS]).activates("cfg(windows)"));
    assert!(!selection(&[LINUX]).activates("cfg(windows)"));
    assert!(!selection(&[MACOS]).activates("cfg(windows)"));

    assert!(selection(&[LINUX]).activates("cfg(unix)"));
    assert!(selection(&[MACOS]).activates("cfg(unix)"));
    assert!(!selection(&[WINDOWS]).activates("cfg(unix)"));
}

#[test]
fn one_activating_platform_is_enough() {
    let both = selection(&[LINUX, WINDOWS]);

    // The selection is a union: an edge belongs to the graph when *any* selected platform
    // compiles it, which is what makes a multi-platform gate a superset of each single one.
    assert!(both.activates("cfg(windows)"));
    assert!(both.activates("cfg(unix)"));
    assert!(!both.activates("cfg(target_os = \"macos\")"));
}

#[test]
fn a_compound_expression_is_evaluated_whole() {
    let linux = selection(&[LINUX]);

    assert!(linux.activates("cfg(all(unix, target_arch = \"x86_64\"))"));
    assert!(!linux.activates("cfg(all(unix, target_arch = \"aarch64\"))"));
    assert!(linux.activates("cfg(any(windows, target_env = \"gnu\"))"));
    assert!(!linux.activates("cfg(not(unix))"));
}

#[test]
fn a_bare_target_triple_is_compared_literally() {
    // `[target.x86_64-pc-windows-msvc.dependencies]` records the triple itself, not a `cfg`.
    assert!(selection(&[WINDOWS]).activates(WINDOWS));
    assert!(!selection(&[LINUX]).activates(WINDOWS));
    assert!(selection(&[LINUX, WINDOWS]).activates(WINDOWS));
}

#[test]
fn an_unknown_target_feature_keeps_the_edge() {
    let linux = selection(&[LINUX]);

    // Which target features a build enables is not a property of the target, so there is no
    // honest answer here — and dropping an edge on a guess is the one direction that can turn
    // a `deny` rule into a false pass. `not(...)` of an unknown stays unknown rather than
    // flipping to a confident drop.
    assert!(linux.activates("cfg(target_feature = \"avx2\")"));
    assert!(linux.activates("cfg(not(target_feature = \"avx2\"))"));
    assert!(linux.activates("cfg(all(unix, target_feature = \"avx2\"))"));
}

#[test]
fn a_predicate_cargo_calls_false_drops_the_edge() {
    let linux = selection(&[LINUX]);

    // The three cargo settles by documented rule rather than by asking rustc: while it
    // evaluates a dependency table's `cfg(...)`, none of them is ever set, on any target and
    // under any flags. Nothing a user can pass makes them true, so calling them false cannot
    // drop an edge a build compiles. (`feature` is false in cargo too — rust-lang/cargo#7442.)
    assert!(!linux.activates("cfg(test)"));
    assert!(!linux.activates("cfg(proc_macro)"));
    assert!(!linux.activates("cfg(feature = \"std\")"));

    // False, not unknown: `not(...)` of it is therefore a confident true.
    assert!(linux.activates("cfg(not(test))"));
    assert!(linux.activates("cfg(any(windows, not(feature = \"std\")))"));
    assert!(!linux.activates("cfg(all(unix, feature = \"std\"))"));
}

#[test]
fn a_predicate_rustc_can_print_keeps_the_edge() {
    let linux = selection(&[LINUX]);

    // Everything cargo settles by *asking rustc* is unknown here, because this process runs no
    // rustc and reads no RUSTFLAGS. These are not hypothetical: `rustc --print cfg --target
    // x86_64-unknown-linux-gnu` prints `debug_assertions` on stable 1.98 and adds
    // `overflow_checks` and `relocation_model="pic"` on a 1.100 nightly, and a probe workspace
    // confirms cargo compiles exactly those dependencies. Answering `false` for them — as this
    // evaluator did until the review — drops edges cargo compiles, which is how a `deny` rule
    // becomes a false pass.
    assert!(linux.activates("cfg(debug_assertions)"));
    assert!(linux.activates("cfg(overflow_checks)"));
    assert!(linux.activates("cfg(relocation_model = \"pic\")"));

    // A key rustc does not print today is kept on the same rule: an allowlist of the ones it
    // prints would under-report again the moment rustc grows a key.
    assert!(linux.activates("cfg(some_key = \"some value\")"));
    assert!(linux.activates("cfg(fuzzing)"));

    // Unknown, not false, so `not(...)` of it stays unknown and keeps the edge as well.
    assert!(linux.activates("cfg(not(debug_assertions))"));
    assert!(linux.activates("cfg(all(unix, relocation_model = \"pic\"))"));
}

#[test]
fn an_unparseable_target_key_keeps_the_edge_too() {
    let linux = selection(&[LINUX]);

    // `cfg-expr` demands a value after any `target_*` key and knows a fixed list of them, so
    // bare `cfg(target_thread_local)` — which a 1.100 nightly really does print, and which a
    // probe workspace confirms cargo compiles — never reaches the predicate closure: the whole
    // expression fails to parse, and an unparseable expression keeps its edge. Different route
    // from the unknown predicates above, same direction, and it is the route every future
    // `target_*` key rustc adds will take until this crate's parser learns it.
    assert!(linux.activates("cfg(target_thread_local)"));
    assert!(linux.activates("cfg(target_has_atomic_primitive_alignment = \"8\")"));
    assert!(Expression::parse("cfg(target_thread_local)").is_err(), "the premise of this test");
}

#[test]
fn a_bare_flag_only_rustflags_could_set_keeps_the_edge() {
    let linux = selection(&[LINUX]);

    // `tracing-core` gates `valuable` behind `cfg(tracing_unstable)`, which a default build
    // does not set — so guppy drops that edge and we keep it. That over-report is deliberate
    // and is pinned as a named exception in the guppy differential: cargo matches against
    // `rustc --print cfg`, where a `RUSTFLAGS=--cfg tracing_unstable` *does* appear, so a bare
    // flag is not provably absent from the build being gated. Keeping the edge can only widen
    // the closure; dropping it could hide a `deny` finding.
    assert!(linux.activates("cfg(tracing_unstable)"));
    assert!(linux.activates("cfg(not(tracing_unstable))"));
    assert!(linux.activates("cfg(any(unix, tracing_unstable))"));
    assert!(!linux.activates("cfg(all(windows, tracing_unstable))"));
}

#[test]
fn an_unresolvable_host_names_the_triple_it_resolved_to() {
    // The host is whatever `rustc -vV` reported, so telling this writer that `host` is one of
    // the expected values would be advising them to repeat what just failed. The resolver is
    // asked against an injected triple rather than the real host: a test that needed a machine
    // whose triple `cfg-expr` has never heard of could not run anywhere.
    let error =
        PlatformSelection::resolve_against_host(&tokens(&["host"]), "x86_64-unknown-nonesuch-elf")
            .expect_err("a host outside the built-in target table must not resolve");

    assert_eq!(error.index, 0);
    assert_eq!(error.value, "host");
    assert_eq!(
        error.to_string(),
        "`host` resolved to `x86_64-unknown-nonesuch-elf`, which is not in rustc's built-in \
         target table"
    );

    // A triple that is merely misspelt keeps the other wording, which does name `host`.
    let misspelt = PlatformSelection::resolve(&tokens(&["x86_64-unknown-nonesuch-elf"]))
        .expect_err("an unknown triple must not resolve");
    assert_eq!(misspelt.host_triple, None);
    assert!(misspelt.to_string().contains("expected `all`, `host`, or a target triple"));
}

#[test]
fn a_decided_half_still_settles_an_expression_with_an_unknown_half() {
    let linux = selection(&[LINUX]);

    // Three-valued logic, not "unknown anywhere means keep": `all(windows, ...)` is false on
    // Linux whatever the second operand turns out to be, so the edge is still dropped.
    assert!(!linux.activates("cfg(all(windows, target_feature = \"avx2\"))"));
    assert!(linux.activates("cfg(any(unix, target_feature = \"avx2\"))"));
}

#[test]
fn an_unparseable_expression_keeps_the_edge() {
    let linux = selection(&[LINUX]);

    assert!(linux.activates("cfg(unix"), "an unclosed expression must not drop an edge");
    assert!(linux.activates("cfg(!!!)"));
}
