#![expect(clippy::expect_used, reason = "test bodies assert directly")]

use std::{collections::BTreeSet, fs, io::Read as _, path::PathBuf};

use flate2::read::GzDecoder;
use guppy::{
    PackageId,
    graph::{DependencyDirection, PackageGraph, feature::StandardFeatures},
    platform::{EnabledTernary, PlatformSpec},
};

use super::*;
use crate::metadata::{Meta, MetadataBuffer, parse};

/// One declared dependency of a synthetic package.
#[derive(Clone)]
struct Decl {
    package: &'static str,
    rename: Option<&'static str>,
    optional: bool,
    uses_default_features: bool,
    features: Vec<&'static str>,
    target: Option<&'static str>,
    kind: Option<&'static str>,
    /// The version the declaration resolves to, when the name carries several.
    version: &'static str,
    /// The dependency's `[lib] name`, when it does not follow from the package name.
    ///
    /// Only the resolve edge carries it; a declaration cannot spell it, which is the case
    /// [`edge_belongs`] has to let through.
    lib_name: Option<&'static str>,
}

impl Decl {
    fn new(package: &'static str) -> Self {
        Self {
            package,
            rename: None,
            optional: false,
            uses_default_features: true,
            features: Vec::new(),
            target: None,
            kind: None,
            version: "1.0.0",
            lib_name: None,
        }
    }

    fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    fn renamed(mut self, rename: &'static str) -> Self {
        self.rename = Some(rename);
        self
    }

    fn no_default(mut self) -> Self {
        self.uses_default_features = false;
        self
    }

    fn features(mut self, features: &[&'static str]) -> Self {
        self.features = features.to_vec();
        self
    }

    fn target(mut self, target: &'static str) -> Self {
        self.target = Some(target);
        self
    }

    fn dev(mut self) -> Self {
        self.kind = Some("dev");
        self
    }

    fn version(mut self, version: &'static str) -> Self {
        self.version = version;
        self
    }

    fn lib_name(mut self, lib_name: &'static str) -> Self {
        self.lib_name = Some(lib_name);
        self
    }

    /// The name cargo records for this declaration's resolve edge: the rename when there is
    /// one, else the dependency's `[lib] name`, else the package name spelled as a library
    /// target. Confirmed against `cargo metadata` directly.
    fn resolved_name(&self) -> String {
        self.rename.or(self.lib_name).map_or_else(|| self.package.replace('-', "_"), str::to_owned)
    }

    fn json(&self) -> String {
        let rename =
            self.rename.map_or_else(|| "null".to_owned(), |rename| format!("\"{rename}\""));
        let kind = self.kind.map_or_else(|| "null".to_owned(), |kind| format!("\"{kind}\""));
        let target =
            self.target.map_or_else(|| "null".to_owned(), |target| format!("\"{target}\""));
        let features = self
            .features
            .iter()
            .map(|feature| format!("\"{feature}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"name":"{}","rename":{rename},"kind":{kind},"target":{target},"optional":{},"uses_default_features":{},"features":[{features}]}}"#,
            self.package, self.optional, self.uses_default_features
        )
    }
}

/// One synthetic package: its features, its declarations, and the activation the resolve
/// recorded for it.
struct Pkg {
    name: &'static str,
    version: &'static str,
    features: Vec<(&'static str, Vec<&'static str>)>,
    decls: Vec<Decl>,
    /// The `resolve.nodes[].features` entry; `None` records every declared feature, which
    /// is the `--all-features` document the guard demands.
    activated: Option<Vec<&'static str>>,
}

impl Pkg {
    fn new(name: &'static str) -> Self {
        Self { name, version: "1.0.0", features: Vec::new(), decls: Vec::new(), activated: None }
    }

    fn version(mut self, version: &'static str) -> Self {
        self.version = version;
        self
    }

    fn feature(mut self, name: &'static str, entries: &[&'static str]) -> Self {
        self.features.push((name, entries.to_vec()));
        self
    }

    fn decl(mut self, declaration: Decl) -> Self {
        self.decls.push(declaration);
        self
    }

    fn activated(mut self, features: &[&'static str]) -> Self {
        self.activated = Some(features.to_vec());
        self
    }

    fn id(&self) -> String {
        format!("path+file:///ws/{}#{}", self.name, self.version)
    }
}

/// A synthetic workspace: every package is a member, and every normal declaration becomes a
/// resolve edge, which is exactly the shape an `--all-features` document has.
struct Workspace {
    packages: Vec<Pkg>,
}

impl Workspace {
    fn new(packages: Vec<Pkg>) -> Self {
        Self { packages }
    }

    fn find(&self, package: &str, version: &str) -> &Pkg {
        self.packages
            .iter()
            .find(|candidate| candidate.name == package && candidate.version == version)
            .unwrap_or_else(|| panic!("the workspace declares {package} v{version}"))
    }

    fn json(&self) -> String {
        let packages = self
            .packages
            .iter()
            .map(|package| {
                let features = package
                    .features
                    .iter()
                    .map(|(name, entries)| {
                        let entries = entries
                            .iter()
                            .map(|entry| format!("\"{entry}\""))
                            .collect::<Vec<_>>()
                            .join(",");
                        format!("\"{name}\":[{entries}]")
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                let decls =
                    package.decls.iter().map(Decl::json).collect::<Vec<_>>().join(",");
                format!(
                    r#"{{"name":"{}","version":"{}","id":"{}","source":null,"manifest_path":"/ws/{}/Cargo.toml","dependencies":[{decls}],"features":{{{features}}}}}"#,
                    package.name,
                    package.version,
                    package.id(),
                    package.name
                )
            })
            .collect::<Vec<_>>()
            .join(",");

        let nodes = self
            .packages
            .iter()
            .map(|package| {
                let mut seen = BTreeSet::new();
                let deps = package
                    .decls
                    .iter()
                    .filter(|declaration| declaration.kind.is_none())
                    .filter(|declaration| seen.insert((declaration.package, declaration.version)))
                    .map(|declaration| {
                        let target = self.find(declaration.package, declaration.version);
                        format!(
                            r#"{{"name":"{}","pkg":"{}","dep_kinds":[{{"kind":null,"target":null}}]}}"#,
                            declaration.resolved_name(),
                            target.id()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                let activated = package.activated.clone().unwrap_or_else(|| {
                    package.features.iter().map(|(name, _)| *name).collect()
                });
                let activated = activated
                    .iter()
                    .map(|feature| format!("\"{feature}\""))
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    r#"{{"id":"{}","deps":[{deps}],"features":[{activated}]}}"#,
                    package.id()
                )
            })
            .collect::<Vec<_>>()
            .join(",");

        let members = self
            .packages
            .iter()
            .map(|package| format!("\"{}\"", package.id()))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"packages":[{packages}],"workspace_members":[{members}],"workspace_root":"/ws","resolve":{{"nodes":[{nodes}],"root":null}}}}"#
        )
    }

    fn graph(&self) -> Graph<'static> {
        let buffer: &'static MetadataBuffer =
            Box::leak(Box::new(MetadataBuffer::from_bytes(self.json().into_bytes())));
        let meta: &'static Meta<'static> =
            Box::leak(Box::new(parse(buffer).expect("synthetic metadata parses")));
        Graph::build(meta).expect("synthetic graph builds")
    }
}

/// The names the activation from `root` reaches, the root's own name excluded.
fn reached(graph: &Graph<'_>, root: u32, selection: &Selection) -> BTreeSet<String> {
    let activation = activate(graph, root, selection).expect("the walk runs");
    let root_name = graph.name(root);
    activation
        .nodes()
        .ones()
        .filter_map(|node| u32::try_from(node).ok())
        .map(|node| graph.name(node))
        .filter(|&name| name != root_name)
        .map(str::to_owned)
        .collect()
}

fn member(graph: &Graph<'_>, name: &str) -> u32 {
    graph
        .members()
        .iter()
        .copied()
        .find(|&node| graph.name(node) == name)
        .unwrap_or_else(|| panic!("{name} is a workspace member"))
}

fn names(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

/// `app` depends on the optional `opt`, which is reached only through `dep:opt`.
fn dep_syntax_workspace() -> Workspace {
    Workspace::new(vec![
        Pkg::new("app")
            .feature("net", &["dep:opt"])
            .feature("default", &["plain"])
            .feature("plain", &[])
            .decl(Decl::new("opt").optional())
            .decl(Decl::new("always")),
        Pkg::new("opt").decl(Decl::new("opt-child")),
        Pkg::new("opt-child"),
        Pkg::new("always"),
    ])
}

#[test]
fn dep_syntax_turns_an_optional_dependency_on_only_when_its_feature_is_selected() {
    let workspace = dep_syntax_workspace();
    let graph = workspace.graph();
    let app = member(&graph, "app");

    assert_eq!(
        reached(&graph, app, &Selection::Default),
        names(&["always"]),
        "the default feature does not name `net`, so `opt` stays out"
    );
    assert_eq!(
        reached(&graph, app, &Selection::List(vec!["net".to_owned()])),
        names(&["always", "opt", "opt-child"]),
        "`dep:opt` pulls the optional dependency and everything under it"
    );
    assert_eq!(
        reached(&graph, app, &Selection::All),
        names(&["always", "opt", "opt-child"]),
        "--all-features selects `net` as well"
    );
    assert_eq!(
        reached(&graph, app, &Selection::None),
        names(&["always"]),
        "--no-default-features still compiles the unconditional declarations"
    );
}

#[test]
fn a_slash_entry_enables_an_optional_dependency_and_requests_the_feature_on_it() {
    let workspace = Workspace::new(vec![
        Pkg::new("app").feature("net", &["opt/inner"]).decl(Decl::new("opt").optional()),
        Pkg::new("opt")
            .feature("inner", &["dep:inner-child"])
            .decl(Decl::new("inner-child").optional()),
        Pkg::new("inner-child"),
    ]);
    let graph = workspace.graph();
    let app = member(&graph, "app");

    assert_eq!(reached(&graph, app, &Selection::None), names(&[]));
    assert_eq!(
        reached(&graph, app, &Selection::List(vec!["net".to_owned()])),
        names(&["opt", "inner-child"]),
        "a non-weak `x/feat` turns `x` on even though it is optional"
    );
}

#[test]
fn a_weak_slash_entry_waits_for_the_dependency_and_fires_in_either_order() {
    let workspace = Workspace::new(vec![
        Pkg::new("app")
            .feature("weak", &["opt?/inner"])
            .feature("turn-on", &["dep:opt"])
            .decl(Decl::new("opt").optional()),
        Pkg::new("opt")
            .feature("inner", &["dep:inner-child"])
            .decl(Decl::new("inner-child").optional()),
        Pkg::new("inner-child"),
    ]);
    let graph = workspace.graph();
    let app = member(&graph, "app");

    assert_eq!(
        reached(&graph, app, &Selection::List(vec!["weak".to_owned()])),
        names(&[]),
        "a weak entry alone never turns the dependency on"
    );
    // Both orders reach the same state: the deferred request is flushed when the
    // dependency is expanded, and a request made afterwards is applied immediately.
    let deferred_first =
        reached(&graph, app, &Selection::List(vec!["weak".to_owned(), "turn-on".to_owned()]));
    let enabled_first =
        reached(&graph, app, &Selection::List(vec!["turn-on".to_owned(), "weak".to_owned()]));
    assert_eq!(deferred_first, names(&["opt", "inner-child"]));
    assert_eq!(enabled_first, deferred_first, "activation does not depend on selection order");
}

#[test]
fn a_bare_token_is_a_feature_when_the_table_defines_one_of_that_name() {
    // coreutils' real `uucore` shape: an optional dependency named `time` beside a feature
    // named `time` whose value is something else entirely. `utmpx` names both, and the
    // bare token has to mean the feature while `time/macros` means the dependency.
    let workspace = Workspace::new(vec![
        Pkg::new("uucore")
            .feature("time", &["jiff"])
            .feature("jiff", &["dep:jiff"])
            .feature("utmpx", &["time", "time/macros", "dep:libc"])
            .decl(Decl::new("time").optional())
            .decl(Decl::new("jiff").optional())
            .decl(Decl::new("libc").optional()),
        Pkg::new("time")
            .feature("macros", &["dep:time-macros"])
            .decl(Decl::new("time-macros").optional()),
        Pkg::new("time-macros"),
        Pkg::new("jiff"),
        Pkg::new("libc"),
    ]);
    let graph = workspace.graph();
    let uucore = member(&graph, "uucore");

    assert_eq!(
        reached(&graph, uucore, &Selection::List(vec!["time".to_owned()])),
        names(&["jiff"]),
        "the bare token resolves to the feature, which pulls jiff and not the crate `time`"
    );
    assert_eq!(
        reached(&graph, uucore, &Selection::List(vec!["utmpx".to_owned()])),
        names(&["jiff", "libc", "time", "time-macros"]),
        "`time/macros` addresses the dependency, so both meanings coexist in one value"
    );
}

#[test]
fn an_optional_dependency_without_a_feature_key_carries_its_implicit_feature() {
    let workspace =
        Workspace::new(vec![Pkg::new("app").decl(Decl::new("opt").optional()), Pkg::new("opt")]);
    let graph = workspace.graph();
    let app = member(&graph, "app");

    assert_eq!(reached(&graph, app, &Selection::Default), names(&[]));
    assert_eq!(
        reached(&graph, app, &Selection::List(vec!["opt".to_owned()])),
        names(&["opt"]),
        "cargo materialises `opt = [\"dep:opt\"]`, which the document never spells out"
    );
    assert_eq!(
        reached(&graph, app, &Selection::All),
        names(&["opt"]),
        "--all-features selects the implicit feature too"
    );
}

#[test]
fn a_dependency_named_default_is_the_packages_default_feature() {
    // The name `default` gets no special case in `expand_feature`, because cargo gives it
    // none: the implicit feature of an optional dependency named `default` *is* the
    // package's default feature. Verified on cargo 1.98 — a plain `cargo tree` pulls the
    // edge and the resolve node records `features: ["default"]`.
    let workspace = Workspace::new(vec![
        Pkg::new("app").decl(Decl::new("default").optional()).decl(Decl::new("always")),
        Pkg::new("default"),
        Pkg::new("always"),
    ]);
    let graph = workspace.graph();
    let app = member(&graph, "app");

    assert_eq!(
        reached(&graph, app, &Selection::Default),
        names(&["always", "default"]),
        "the default selection activates the implicit feature, as cargo does"
    );
    assert_eq!(
        reached(&graph, app, &Selection::None),
        names(&["always"]),
        "--no-default-features withholds it again"
    );
}

#[test]
fn a_package_with_no_default_feature_activates_nothing_for_the_default_selection() {
    // The other half of the rule: with no `default` key and no dependency of that name,
    // the seeded `default` finds nothing to expand and is a no-op, never an error.
    let workspace = Workspace::new(vec![
        Pkg::new("app").feature("extra", &["dep:opt"]).decl(Decl::new("opt").optional()),
        Pkg::new("opt"),
    ]);
    let graph = workspace.graph();
    let app = member(&graph, "app");

    assert_eq!(reached(&graph, app, &Selection::Default), names(&[]));
    assert_eq!(
        reached(&graph, app, &Selection::List(vec!["extra".to_owned()])),
        names(&["opt"]),
        "the package's real features still work"
    );
}

#[test]
fn all_features_skips_an_implicit_feature_that_dep_syntax_suppressed() {
    // `dep:hidden` inside a feature value means cargo creates no `hidden` feature, so
    // `--all-features` reaches `hidden` only through `gate`, never as a feature of its own.
    let workspace = Workspace::new(vec![
        Pkg::new("app")
            .feature("gate", &["dep:hidden"])
            .decl(Decl::new("hidden").optional())
            .decl(Decl::new("plain").optional()),
        Pkg::new("hidden"),
        Pkg::new("plain"),
    ]);
    let graph = workspace.graph();
    let app = member(&graph, "app");

    assert_eq!(
        reached(&graph, app, &Selection::All),
        names(&["hidden", "plain"]),
        "both arrive, one through its gate feature and one through its implicit feature"
    );
    assert_eq!(
        reached(&graph, app, &Selection::List(vec!["hidden".to_owned()])),
        names(&[]),
        "the suppressed name is not a feature, so selecting it activates nothing"
    );
}

#[test]
fn uses_default_features_false_withholds_the_dependencys_default_feature() {
    let workspace = Workspace::new(vec![
        Pkg::new("app").decl(Decl::new("lib").no_default()),
        Pkg::new("bare").decl(Decl::new("lib")),
        Pkg::new("lib").feature("default", &["dep:heavy"]).decl(Decl::new("heavy").optional()),
        Pkg::new("heavy"),
    ]);
    let graph = workspace.graph();

    assert_eq!(
        reached(&graph, member(&graph, "app"), &Selection::Default),
        names(&["lib"]),
        "the dependency's own default feature is withheld"
    );
    assert_eq!(
        reached(&graph, member(&graph, "bare"), &Selection::Default),
        names(&["lib", "heavy"]),
        "and requested by the declaration that does not opt out"
    );
}

#[test]
fn a_renamed_dependency_is_addressed_by_its_extern_name() {
    let workspace = Workspace::new(vec![
        Pkg::new("app")
            .feature("net", &["alias/inner"])
            .decl(Decl::new("real").renamed("alias").optional()),
        Pkg::new("real").feature("inner", &["dep:child"]).decl(Decl::new("child").optional()),
        Pkg::new("child"),
    ]);
    let graph = workspace.graph();
    let app = member(&graph, "app");

    assert_eq!(
        reached(&graph, app, &Selection::List(vec!["net".to_owned()])),
        names(&["real", "child"]),
        "feature syntax names the rename while the edge is found by the package name"
    );
    assert_eq!(
        reached(&graph, app, &Selection::List(vec!["real".to_owned()])),
        names(&[]),
        "the implicit feature of a renamed optional dependency carries the rename"
    );
}

#[test]
fn a_name_declared_in_two_target_tables_contributes_both_declarations() {
    let workspace = Workspace::new(vec![
        Pkg::new("app")
            .decl(Decl::new("lib").features(&["unix-side"]))
            .decl(Decl::new("lib").target("cfg(windows)").features(&["windows-side"])),
        Pkg::new("lib")
            .feature("unix-side", &["dep:posix"])
            .feature("windows-side", &["dep:win"])
            .decl(Decl::new("posix").optional())
            .decl(Decl::new("win").optional()),
        Pkg::new("posix"),
        Pkg::new("win"),
    ]);
    let graph = workspace.graph();

    assert_eq!(
        reached(&graph, member(&graph, "app"), &Selection::Default),
        names(&["lib", "posix", "win"]),
        "every platform is kept, so both tables contribute their requested features"
    );
}

/// `app` declares one name at two versions, each declaration requesting the feature that
/// belongs to *its* version. Both versions define **both** feature names, and each of the
/// four leads to a package of its own, so a join that could not tell the two edges apart
/// would request `one` and `two` on both versions and reach the two `wrong-*` packages.
fn two_versions_workspace(first: Decl, second: Decl) -> Workspace {
    Workspace::new(vec![
        Pkg::new("app").decl(first).decl(second),
        Pkg::new("dual")
            .feature("one", &["dep:only-one"])
            .feature("two", &["dep:wrong-two"])
            .decl(Decl::new("only-one").optional())
            .decl(Decl::new("wrong-two").optional()),
        Pkg::new("dual")
            .version("2.0.0")
            .feature("one", &["dep:wrong-one"])
            .feature("two", &["dep:only-two"])
            .decl(Decl::new("wrong-one").optional())
            .decl(Decl::new("only-two").optional()),
        Pkg::new("only-one"),
        Pkg::new("only-two"),
        Pkg::new("wrong-one"),
        Pkg::new("wrong-two"),
    ])
}

/// Asserts that each declaration reached its own version and only its own version, and that
/// neither edge was dropped on the way.
fn assert_versions_stay_apart(workspace: &Workspace) {
    let graph = workspace.graph();
    let app = member(&graph, "app");

    assert_eq!(
        reached(&graph, app, &Selection::Default),
        names(&["dual", "only-one", "only-two"]),
        "each declaration's features land on the version it resolved to, and on no other"
    );

    let activation = activate(&graph, app, &Selection::Default).expect("the walk runs");
    let used = graph.edges_from(app).filter(|&edge| activation.contains_edge(edge)).count();
    assert_eq!(used, 2, "both versions are still reached — one edge per declaration");
}

#[test]
fn one_name_at_two_versions_keeps_each_declarations_features_on_its_own_version() {
    assert_versions_stay_apart(&two_versions_workspace(
        Decl::new("dual").version("1.0.0").features(&["one"]),
        Decl::new("dual").renamed("dual2").version("2.0.0").features(&["two"]),
    ));
}

#[test]
fn two_renames_of_one_name_at_two_versions_keep_their_declarations_apart() {
    // Neither declaration spells the package name, so the extern name is the only thing that
    // tells the two edges apart. Cargo refuses to resolve two differently-named declarations
    // of one package to a single version, so this shape always carries two edges and every
    // declaration keeps one.
    assert_versions_stay_apart(&two_versions_workspace(
        Decl::new("dual").renamed("dual_v1").version("1.0.0").features(&["one"]),
        Decl::new("dual").renamed("dual_v2").version("2.0.0").features(&["two"]),
    ));
}

#[test]
fn an_edge_named_after_a_lib_target_still_belongs_to_its_declaration() {
    // `deps[].name` is the dependency's library target name, so a package whose `[lib] name`
    // does not follow from its package name is reported under a name no declaration can
    // spell. Dropping such an edge would under-activate — the one direction that turns a
    // `deny` rule into a false pass — so an unclaimed name attaches rather than excludes.
    let workspace = Workspace::new(vec![
        Pkg::new("app")
            .decl(Decl::new("redox-syscall").lib_name("syscall").features(&["inner"]))
            .decl(Decl::new("md-5").features(&["inner"])),
        Pkg::new("redox-syscall")
            .feature("inner", &["dep:child"])
            .decl(Decl::new("child").optional()),
        Pkg::new("md-5").feature("inner", &["dep:digest"]).decl(Decl::new("digest").optional()),
        Pkg::new("child"),
        Pkg::new("digest"),
    ]);
    let graph = workspace.graph();

    assert_eq!(
        reached(&graph, member(&graph, "app"), &Selection::Default),
        names(&["redox-syscall", "child", "md-5", "digest"]),
        "an unspellable `[lib] name` attaches, and a dash-for-underscore spelling matches"
    );
}

#[test]
fn dev_declarations_never_enter_the_activation() {
    let workspace =
        Workspace::new(vec![Pkg::new("app").decl(Decl::new("harness").dev()), Pkg::new("harness")]);
    let graph = workspace.graph();

    assert_eq!(
        reached(&graph, member(&graph, "app"), &Selection::All),
        names(&[]),
        "the walk answers the `-e normal` question"
    );
}

#[test]
fn the_activation_reports_the_edges_it_used_and_leaves_the_others_out() {
    let workspace = dep_syntax_workspace();
    let graph = workspace.graph();
    let app = member(&graph, "app");

    let default = activate(&graph, app, &Selection::Default).expect("the walk runs");
    let all = activate(&graph, app, &Selection::All).expect("the walk runs");

    assert_eq!(default.root(), app);
    assert_eq!(default.node_count(), 2, "app and always");
    assert_eq!(default.edges().count_ones(..), 1);
    assert!(all.edges().is_superset(default.edges()), "a wider selection keeps every edge");
    assert_eq!(all.edges().count_ones(..), 3, "app→always, app→opt and opt→opt-child");
    for edge in default.edges().ones() {
        let edge = u32::try_from(edge).expect("edge ids fit in u32");
        assert_eq!(graph.name(graph.edge_target(edge)), "always");
    }
}

#[test]
fn a_document_resolved_with_every_feature_passes_the_guard() {
    let workspace = dep_syntax_workspace();
    let graph = workspace.graph();

    assert_eq!(first_unactivated_member(&graph).expect("the guard runs"), None);
}

#[test]
fn the_guard_names_the_first_member_the_resolve_left_features_off() {
    let workspace = Workspace::new(vec![
        Pkg::new("app")
            .feature("net", &["dep:opt"])
            .feature("plain", &[])
            .decl(Decl::new("opt").optional()),
        Pkg::new("opt"),
    ]);
    let graph = workspace.graph();
    assert_eq!(first_unactivated_member(&graph).expect("the guard runs"), None);

    let workspace = Workspace::new(vec![
        Pkg::new("app")
            .feature("net", &["dep:opt"])
            .feature("plain", &[])
            .decl(Decl::new("opt").optional())
            .activated(&["plain"]),
        Pkg::new("opt"),
    ]);
    let graph = workspace.graph();

    assert_eq!(
        first_unactivated_member(&graph).expect("the guard runs"),
        Some(UnactivatedMember { package: "app".to_owned(), unactivated: 1 })
    );
}

/// One committed example document, what the guard has to say about it, and the exact set of
/// names on which this walk and guppy disagree.
struct Fixture {
    name: &'static str,
    directory: &'static str,
    /// The first member whose features the resolve left off, and how many it left off;
    /// `None` for a document generated with `--all-features`, which the guard must accept.
    offender: Option<(&'static str, u32)>,
    /// Names guppy reaches on the host that the walk does not, over all members.
    guppy_only: &'static [&'static str],
    /// Names the walk reaches that guppy does not reach on any platform, over all members.
    ours_only: &'static [&'static str],
}

/// The one place the two disagree, verified against cargo 1.98 itself rather than argued
/// from the documentation.
///
/// `uucore` declares an optional dependency `time` **and** a feature `time = ["jiff"]`, and
/// reaches the dependency only through `utmpx`'s `time/macros` entry. Cargo resolves that
/// collision by letting the declared feature shadow the implicit one: a throwaway workspace
/// with `[features] tdep = ["jiffish"]` beside `tdep = { optional = true }` resolves
/// `--features tdep` to `jiffish` with **no** `tdep` edge, and cargo refuses to build the
/// manifest at all unless some entry reaches the dependency through `dep:tdep` or
/// `tdep/feat`. guppy models the collision the other way round — it treats `time` as the
/// optional-dependency feature and drops the declared `["jiff"]` — so the disagreement
/// appears once in each direction on the same feature: guppy alone reaches the `time` crate
/// and its subtree, this walk alone reaches the `jiff` crate and its subtree.
///
/// The walk follows cargo. These two lists are the whole of the difference across all 230
/// members of the three documents; lemmy and ckb match guppy exactly.
const UUCORE_TIME_COLLISION_GUPPY_ONLY: &[&str] =
    &["deranged", "num-conv", "num_threads", "powerfmt", "time", "time-core", "time-macros"];
const UUCORE_TIME_COLLISION_OURS_ONLY: &[&str] = &[
    "jiff",
    "jiff-core",
    "jiff-static",
    "jiff-tzdb",
    "jiff-tzdb-platform",
    "portable-atomic",
    "portable-atomic-util",
];

const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "lemmy",
        directory: "tests/fixtures/lemmy-439734d",
        offender: None,
        guppy_only: &[],
        ours_only: &[],
    },
    Fixture {
        name: "ckb",
        directory: "tests/fixtures/ckb-17d7db5",
        offender: Some(("ckb-util", 1)),
        guppy_only: &[],
        ours_only: &[],
    },
    Fixture {
        name: "coreutils",
        directory: "tests/fixtures/coreutils-6341084",
        offender: None,
        guppy_only: UUCORE_TIME_COLLISION_GUPPY_ONLY,
        ours_only: UUCORE_TIME_COLLISION_OURS_ONLY,
    },
];

fn fixture_json(fixture: &Fixture) -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(fixture.directory).join("metadata.json.gz");
    let compressed = fs::File::open(&path).expect("the example fixture is readable");
    let mut json = String::new();
    GzDecoder::new(compressed).read_to_string(&mut json).expect("the example fixture decompresses");
    json
}

#[test]
fn the_guard_reads_each_committed_document_the_way_it_was_generated() {
    // lemmy and coreutils are generated with `--all-features` because their policies carry
    // feature-aware rules, so the guard must accept them; ckb has no such rule and takes the
    // default selection, which is exactly the situation the guard exists to catch — a narrowed
    // closure over such a document could pass a `deny` rule because the edge it would have
    // matched was never resolved into the graph.
    for fixture in FIXTURES {
        let json = fixture_json(fixture);
        let buffer = MetadataBuffer::from_bytes(json.into_bytes());
        let meta = parse(&buffer).expect("the example document parses");
        let graph = Graph::build(&meta).expect("the example graph builds");

        let offender = first_unactivated_member(&graph).expect("the guard runs");
        let named = offender.as_ref().map(|member| (member.package.as_str(), member.unactivated));
        assert_eq!(named, fixture.offender, "{} got the wrong guard verdict", fixture.name);
    }
}

/// The package names guppy's feature-rooted, default-features closure reaches from `id`
/// through normal links, under one platform spec; the root's own name excluded.
fn guppy_closure(
    package_graph: &PackageGraph,
    id: &PackageId,
    platform: &PlatformSpec,
) -> BTreeSet<String> {
    let root_name = package_graph.metadata(id).expect("the member is in guppy's graph").name();
    package_graph
        .resolve_ids([id])
        .expect("the member resolves")
        .to_feature_set(StandardFeatures::Default)
        .to_feature_query(DependencyDirection::Forward)
        .resolve_with_fn(|_, link| link.normal().enabled_on(platform) != EnabledTernary::Disabled)
        .to_package_set()
        .packages(DependencyDirection::Forward)
        .map(|package| package.name())
        .filter(|&name| name != root_name)
        .map(str::to_owned)
        .collect()
}

#[test]
fn default_activation_matches_guppys_feature_closure_on_every_fixture_member() {
    // guppy resolves cargo's features for real, so it is the oracle for this walk. The
    // bound is two-sided: the host-filtered closure is what a build on this machine
    // compiles and must be contained in ours, while the any-platform closure is everything
    // cargo could compile anywhere and must contain ours. The gap between those two is
    // exactly the platform-conditional widening this walk keeps by design, so nothing has
    // to be excused for it — the only declared exception is the `uucore` feature-name
    // collision documented above, and the assertion is an equality on it rather than a
    // subset check so that a walk which drifts either way fails here.
    let host = PlatformSpec::build_target().expect("the host platform is known");
    for fixture in FIXTURES {
        let json = fixture_json(fixture);
        let package_graph = PackageGraph::from_json(&json).expect("guppy loads the document");
        let buffer = MetadataBuffer::from_bytes(json.into_bytes());
        let meta = parse(&buffer).expect("the example document parses");
        let graph = Graph::build(&meta).expect("the example graph builds");

        let mut guppy_only = BTreeSet::new();
        let mut ours_only = BTreeSet::new();
        for &node in graph.members() {
            let ours = reached(&graph, node, &Selection::Default);
            let id = PackageId::new(graph.package(node).id.to_string());
            guppy_only.extend(guppy_closure(&package_graph, &id, &host).difference(&ours).cloned());
            ours_only.extend(
                ours.difference(&guppy_closure(&package_graph, &id, &PlatformSpec::Any)).cloned(),
            );
        }

        assert_eq!(
            guppy_only,
            names(fixture.guppy_only),
            "{}: names guppy compiles on this host that the walk does not reach",
            fixture.name
        );
        assert_eq!(
            ours_only,
            names(fixture.ours_only),
            "{}: names the walk reaches that no platform activates",
            fixture.name
        );
    }
}

#[test]
fn a_declared_feature_shadows_the_implicit_feature_of_the_dependency_it_is_named_after() {
    // Ground truth, taken from cargo 1.98 rather than from the reference: a workspace with
    // `[features] tdep = ["jiffish"]` beside an optional dependency `tdep` resolves
    // `--features tdep` to `["jiffish", "tdep"]` with `jiffdep` as the only new edge, and
    // reaches `tdep` only through an entry that names it as a dependency. Cargo rejects the
    // manifest outright when no entry does, which is why the shadowed dependency is never
    // simply unreachable in a document that exists.
    let workspace = Workspace::new(vec![
        Pkg::new("mid")
            .feature("tdep", &["jiffish"])
            .feature("jiffish", &["dep:jiffdep"])
            .feature("utmpxish", &["tdep", "tdep/macros"])
            .decl(Decl::new("tdep").optional())
            .decl(Decl::new("jiffdep").optional()),
        Pkg::new("tdep").feature("macros", &[]),
        Pkg::new("jiffdep"),
    ]);
    let graph = workspace.graph();
    let mid = member(&graph, "mid");

    assert_eq!(
        reached(&graph, mid, &Selection::List(vec!["tdep".to_owned()])),
        names(&["jiffdep"]),
        "the declared value wins and the same-named dependency stays off"
    );
    assert_eq!(
        reached(&graph, mid, &Selection::List(vec!["utmpxish".to_owned()])),
        names(&["jiffdep", "tdep"]),
        "`tdep/macros` is what reaches the dependency, exactly as cargo resolves it"
    );
}

#[test]
fn one_walk_reused_across_selections_answers_as_a_fresh_walk_would() {
    // The walk keeps its decode caches between runs, so every other piece of state has to be
    // reset: a leaked feature, dependency or edge would narrow the next rule's closure wrongly.
    let workspace = dep_syntax_workspace();
    let graph = workspace.graph();
    let app = member(&graph, "app");
    let selections = [
        Selection::None,
        Selection::List(vec!["net".to_owned()]),
        Selection::Default,
        Selection::All,
        Selection::None,
        Selection::List(vec!["net".to_owned()]),
    ];

    let mut walk = Walk::new(&graph);
    for selection in &selections {
        let reused = walk.activate(app, selection).expect("the reused walk runs");
        let fresh = activate(&graph, app, selection).expect("a fresh walk runs");
        assert_eq!(reused, fresh, "reuse must not change the answer for {selection}");
    }
}
