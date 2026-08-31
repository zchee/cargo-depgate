//! Package-rooted Cargo feature activation over one already-resolved metadata document.
//!
//! # Why a walk at all
//!
//! `cargo metadata` emits **one** activation for the whole workspace: the union over every
//! member, every dependency kind and every platform. `resolve.nodes[].deps` is filtered by
//! that union, not by any single package's feature selection, so the document answers
//! "which edges exist for *someone*" while a policy line such as
//! `cargo tree -p lemmy_api_common --no-default-features -i diesel` asks "which edges exist
//! for *this* package under *these* features". The second question is strictly narrower
//! than the first and is answerable from the same document, which is what this module does:
//! it re-runs cargo's feature resolution from one root over `packages[].features`,
//! `packages[].dependencies` and the CSR the resolve produced. No second `cargo metadata`,
//! no re-resolve, no compilation.
//!
//! # Soundness
//!
//! Activation is monotone in the root's feature set, so the walk's result is a subset of the
//! document's unified closure **provided every member was resolved with all of its own
//! features**. That premise is not assumed: [`first_unactivated_member`] verifies it against
//! the document itself. Without it, an edge the walk would have activated may simply not be
//! in the resolve, and a `deny` rule evaluated on the narrowed closure would pass for the
//! wrong reason — a false pass, the worst failure this tool can produce.
//!
//! # Joining declarations to edges
//!
//! A feature entry addresses a dependency by its **extern name** (`serde1/derive` names the
//! rename), while an edge of the resolve identifies a **package**. Joining the two by
//! package name alone loses whenever one package declares the same name twice under two
//! renames: enabling either would activate both edges and request both declarations'
//! features on both versions. So the join is on `(package name, extern name)`.
//!
//! The subtlety is that `resolve.nodes[].deps[].name` is not the extern name outright. It is
//! the **library target** name the dependency is compiled under: the rename when the
//! declaration renamed it, otherwise the dependency's own `[lib] name`, which need not
//! resemble the package name at all (`md-5` is reported as `md5`, `async-trait` as
//! `async_trait`, `redox_syscall` as `syscall`). Both halves were confirmed against cargo
//! directly on a throwaway workspace, not inferred. Comparing a declaration's extern name
//! with it would therefore drop 20 real edges across the three fixture documents — an
//! under-activation, the one direction that turns a `deny` rule into a false pass.
//!
//! So the extern name *disambiguates* and never excludes on its own. An edge belongs to a
//! declaration when their names agree once `-` is normalised to `_`, **or** when no
//! declaration of that package name claims the edge's name at all — exactly the `[lib] name`
//! override case, where no declaration could have spelled it.
//!
//! That leaves one question: can a declaration be robbed of its edge by a sibling that
//! claims the name? Only if two declarations of one package name resolved to the *same*
//! package, and cargo rejects that manifest outright (`depends on crate ... multiple times
//! with different names`). Two renames of one name therefore always resolve to two versions,
//! and each declaration keeps an edge of its own. Measured over all three documents, the
//! rule attaches every one of the 6 250 normally-declared edges to at least one declaration
//! and picks a single edge for each of the four ambiguous renames
//! (`activitypub_federation`'s two `http`s, `serde_with`'s two `indexmap`s and two
//! `schemars`, `ckb-vm`'s two `goblin`s).
//!
//! # Deliberate approximations
//!
//! * **All platforms are kept.** The CSR carries `cfg`-conditional edges on every host, and
//!   so does this walk. The result stays a superset of `cargo tree` in that one dimension.
//! * **Normal edges only**, matching `-e normal`. Build-dependency and proc-macro feature
//!   unification, which resolver v2 and v3 separate from the normal graph, is not modelled.
//! * **Every normal declaration of an enabled name contributes its requested features**,
//!   including a `cfg`-gated one the host would not compile — the same all-platform
//!   widening as the first point.
//! * **A document that predates `deps[].name`** falls back to the by-package-name join, and
//!   so to the over-activation the extern name exists to remove.
//!
//! Each of those widens the closure, and widening cannot hide a `deny` finding. The
//! differential test against guppy pins both directions on every fixture member.

use std::{borrow::Cow, collections::BTreeSet, fmt};

use fixedbitset::FixedBitSet;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    error::Error,
    graph::{DeclaredDep, Graph},
};

/// The feature selection an activation walk is seeded with.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Selection {
    /// `--no-default-features` with nothing else: only what the root pulls in
    /// unconditionally.
    None,
    /// The root's `default` feature, as a plain `cargo tree -p <root>` resolves it.
    Default,
    /// Every feature of the root, including the implicit feature of each optional
    /// dependency cargo did not suppress with `dep:` syntax.
    All,
    /// An explicit list, resolved as `--no-default-features --features …`: `default` is
    /// active only when the list names it.
    List(Vec<String>),
}

impl fmt::Display for Selection {
    /// Writes the selection the way a policy spells it, so a report can echo the key back.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("\"none\""),
            Self::Default => formatter.write_str("\"default\""),
            Self::All => formatter.write_str("\"all\""),
            Self::List(features) => {
                formatter.write_str("[")?;
                for (index, feature) in features.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(", ")?;
                    }
                    write!(formatter, "{feature:?}")?;
                }
                formatter.write_str("]")
            }
        }
    }
}

/// What one package-rooted walk turned on: the activated nodes and the normal CSR edges
/// between them.
///
/// Both sets are sized to the graph the walk ran on, and the root is always activated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Activation {
    root: u32,
    nodes: FixedBitSet,
    edges: FixedBitSet,
}

impl Activation {
    /// The node the walk started from.
    #[must_use]
    pub fn root(&self) -> u32 {
        self.root
    }

    /// The activated nodes, as a bitset over node ids.
    #[must_use]
    pub fn nodes(&self) -> &FixedBitSet {
        &self.nodes
    }

    /// The activated normal edges, as a bitset over CSR edge ids.
    #[must_use]
    pub fn edges(&self) -> &FixedBitSet {
        &self.edges
    }

    /// Whether `node` is activated.
    #[must_use]
    pub fn contains_node(&self, node: u32) -> bool {
        self.nodes.contains(node as usize)
    }

    /// Whether `edge` is activated.
    #[must_use]
    pub fn contains_edge(&self, edge: u32) -> bool {
        self.edges.contains(edge as usize)
    }

    /// The number of activated nodes, the root included.
    #[must_use]
    pub fn node_count(&self) -> u32 {
        count(self.nodes.count_ones(..))
    }
}

/// A workspace member the document was **not** resolved with all the features of.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct UnactivatedMember {
    /// The member's package name.
    pub package: String,
    /// How many of its declared features the resolve did not activate.
    pub unactivated: u32,
}

/// Runs one package-rooted activation walk from `root`.
///
/// The result is the node and edge subset a build of `root` under `selection` would use,
/// restricted to what the document's unified resolve contains — see the module
/// documentation for the four ways that is deliberately wider than `cargo tree`.
///
/// Every call allocates its own walk state. Evaluate several selections over one graph
/// through a single [`Walk`] instead, which keeps the per-package decode caches.
///
/// # Errors
///
/// Returns [`Error::CargoMetadataUnparseable`] when a package's raw `dependencies` or
/// `features` slice is malformed.
///
/// # Panics
///
/// Panics if `root` is not a node of `graph`.
pub fn activate(graph: &Graph<'_>, root: u32, selection: &Selection) -> Result<Activation, Error> {
    Walk::new(graph).activate(root, selection)
}

/// The first workspace member whose declared features the resolve did not fully activate.
///
/// This is the guard a feature-aware rule needs before it may narrow a closure: it holds
/// exactly when the document was produced with `--all-features` (or when every member
/// happens to carry all of its features anyway), which is the premise that makes an
/// activation walk a *subset* of the resolve rather than a different graph. Members are
/// checked in `workspace_members` order, so the answer is stable for a given document.
///
/// The check is complete only because cargo materialises each optional dependency's
/// implicit feature into `packages[].features` (see [`crate::metadata::Pkg::features`]): a
/// member resolved without one of its optional dependencies still declares that feature
/// here, so comparing the table against `resolve.nodes[].features` catches it. Were the
/// table the declared one alone, an unactivated implicit feature would leave no key to
/// miss and the guard would pass on a document it must reject.
///
/// # Errors
///
/// Returns [`Error::CargoMetadataUnparseable`] when a member's raw `features` slice, or its
/// resolve node's, is malformed.
pub fn first_unactivated_member(graph: &Graph<'_>) -> Result<Option<UnactivatedMember>, Error> {
    for &member in graph.members() {
        let declared = feature_table(graph, member)?;
        let active = activated_features(graph, member)?;
        let unactivated =
            declared.keys().filter(|feature| !active.contains(feature.as_ref())).count();
        if unactivated > 0 {
            return Ok(Some(UnactivatedMember {
                package: graph.name(member).to_owned(),
                unactivated: count(unactivated),
            }));
        }
    }
    Ok(None)
}

/// The first entry of `requested` that `node`'s own `[features]` table does not define.
///
/// A selection may name an optional dependency, because cargo materialises that dependency's
/// implicit feature into the table (see [`crate::metadata::Pkg::features`]); anything else names
/// a feature the root cannot enable, which would activate nothing and narrow the closure for a
/// reason the policy did not intend.
///
/// # Errors
///
/// Returns [`Error::CargoMetadataUnparseable`] when the package's raw `features` slice is
/// malformed.
pub fn first_undeclared_feature<'r>(
    graph: &Graph<'_>,
    node: u32,
    requested: &'r [String],
) -> Result<Option<&'r str>, Error> {
    let table = feature_table(graph, node)?;
    Ok(requested.iter().map(String::as_str).find(|feature| !table.contains_key(*feature)))
}

/// One package's declared `[features]` table, borrowed from the JSON where it can be.
type FeatureTable<'m> = FxHashMap<Cow<'m, str>, Vec<Cow<'m, str>>>;

fn count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn unparseable(source: serde_json::Error) -> Error {
    Error::CargoMetadataUnparseable { source }
}

/// Decodes `packages[node].features`, treating an absent table as empty.
fn feature_table<'m>(graph: &Graph<'m>, node: u32) -> Result<FeatureTable<'m>, Error> {
    match graph.package(node).features {
        Some(raw) => serde_json::from_str(raw.get()).map_err(unparseable),
        None => Ok(FeatureTable::default()),
    }
}

/// Decodes `resolve.nodes[node].features` — the workspace-unified activation cargo recorded.
fn activated_features<'m>(graph: &Graph<'m>, node: u32) -> Result<BTreeSet<Cow<'m, str>>, Error> {
    match graph.resolve_node(node).features {
        Some(raw) => serde_json::from_str(raw.get()).map_err(unparseable),
        None => Ok(BTreeSet::new()),
    }
}

/// One unit of pending work. Every task is enqueued at most once, guarded by the state it
/// would produce, so the walk is linear in what it activates.
enum Task {
    /// Pull a package's unconditional normal declarations.
    Node(u32),
    /// Turn one declared dependency of a package on, addressed by its extern name.
    Dependency(u32, String),
    /// Expand one feature of a package.
    Feature(u32, String),
}

/// A reusable activation walk over one graph.
///
/// Each [`Walk::activate`] resets the state one run owns and **keeps** the per-package decode
/// caches, so a policy with several feature-aware rules decodes each package's `dependencies`
/// and `[features]` slices once rather than once per rule, and pays the per-node allocation of
/// those caches once rather than per rule. Build one per graph; a walk borrows the graph and
/// the document it was built from.
pub struct Walk<'g, 'm> {
    graph: &'g Graph<'m>,
    /// Per node, the decoded `dependencies` array; empty until the node is touched, so a
    /// walk that reaches a tenth of the graph decodes a tenth of the declarations.
    declarations: Vec<Vec<DeclaredDep<'m>>>,
    /// Per node, the decoded `[features]` table, decoded on the same terms.
    tables: Vec<FeatureTable<'m>>,
    /// Per node, the dependency names its own table decoupled with `dep:` syntax.
    suppressed: Vec<FxHashSet<String>>,
    /// Which nodes' `declarations` and `tables` entries have been decoded.
    decoded: FixedBitSet,
    nodes: FixedBitSet,
    edges: FixedBitSet,
    /// Per node, the features already requested on it.
    active_features: Vec<BTreeSet<String>>,
    /// The `(package, extern name)` dependencies already turned on.
    active_dependencies: FxHashSet<(u32, String)>,
    /// `(package, extern name)` to the nodes that declaration resolved to, filled when the
    /// dependency is expanded.
    dependency_targets: FxHashMap<(u32, String), Vec<u32>>,
    /// Feature requests waiting for a dependency to be turned on: the weak `x?/feat` form,
    /// and any `x/feat` seen before `x` was expanded.
    pending: FxHashMap<(u32, String), Vec<String>>,
    queue: Vec<Task>,
}

impl<'g, 'm> Walk<'g, 'm> {
    /// Allocates walk state sized for `graph`.
    #[must_use]
    pub fn new(graph: &'g Graph<'m>) -> Self {
        let nodes = graph.node_count() as usize;
        let edges = graph.edge_count() as usize;
        Self {
            graph,
            declarations: vec![Vec::new(); nodes],
            tables: vec![FeatureTable::default(); nodes],
            suppressed: vec![FxHashSet::default(); nodes],
            decoded: FixedBitSet::with_capacity(nodes),
            nodes: FixedBitSet::with_capacity(nodes),
            edges: FixedBitSet::with_capacity(edges),
            active_features: vec![BTreeSet::new(); nodes],
            active_dependencies: FxHashSet::default(),
            dependency_targets: FxHashMap::default(),
            pending: FxHashMap::default(),
            queue: Vec::new(),
        }
    }

    /// Runs one walk from `root` and returns what it turned on.
    ///
    /// See [`activate`] for the semantics; this is the same walk with the decode caches of
    /// earlier runs still warm.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CargoMetadataUnparseable`] when a package's raw `dependencies` or
    /// `features` slice is malformed.
    ///
    /// # Panics
    ///
    /// Panics if `root` is not a node of the graph this walk was built for.
    pub fn activate(&mut self, root: u32, selection: &Selection) -> Result<Activation, Error> {
        assert!(root < self.graph.node_count(), "root {root} is not a node");
        self.reset();
        self.run(root, selection)?;
        Ok(Activation { root, nodes: self.nodes.clone(), edges: self.edges.clone() })
    }

    /// Clears everything one run owns, keeping the decode caches (`declarations`, `tables`,
    /// `suppressed` and the `decoded` bitset that guards them), which depend on the document
    /// alone and so stay valid for every root and every selection.
    fn reset(&mut self) {
        self.nodes.clear();
        self.edges.clear();
        for features in &mut self.active_features {
            features.clear();
        }
        self.active_dependencies.clear();
        self.dependency_targets.clear();
        self.pending.clear();
        self.queue.clear();
    }

    fn run(&mut self, root: u32, selection: &Selection) -> Result<(), Error> {
        self.enable_node(root);
        for feature in self.seed(root, selection)? {
            self.request_feature(root, &feature);
        }
        while let Some(task) = self.queue.pop() {
            match task {
                Task::Node(node) => self.expand_node(node)?,
                Task::Dependency(node, name) => self.expand_dependency(node, &name)?,
                Task::Feature(node, name) => self.expand_feature(node, &name)?,
            }
        }
        Ok(())
    }

    /// The features the root starts with.
    ///
    /// `All` is the whole feature table, which on a cargo-generated document already holds
    /// every optional dependency's implicit feature: cargo materialises it as
    /// `"<extern name>": ["dep:<extern name>"]` (see [`crate::metadata::Pkg::features`]).
    /// The `implicit` set below is a documented fallback for a synthetic or
    /// pre-materialisation document, reconstructing the keys cargo would have emitted; its
    /// own filter makes it empty against any document cargo produced, so nothing on that
    /// path double-counts.
    fn seed(&mut self, root: u32, selection: &Selection) -> Result<Vec<String>, Error> {
        match selection {
            Selection::None => Ok(Vec::new()),
            Selection::Default => Ok(vec!["default".to_owned()]),
            Selection::List(features) => Ok(features.clone()),
            Selection::All => {
                self.decode(root)?;
                let table = &self.tables[root as usize];
                let suppressed = &self.suppressed[root as usize];
                let declared = table
                    .keys()
                    .map(|feature| feature.as_ref().to_owned())
                    .collect::<BTreeSet<_>>();
                let implicit = self.declarations[root as usize]
                    .iter()
                    .filter(|declaration| declaration.is_normal() && declaration.optional)
                    .map(DeclaredDep::extern_name)
                    .filter(|name| !table.contains_key(*name) && !suppressed.contains(*name))
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>();
                Ok(declared.into_iter().chain(implicit).collect())
            }
        }
    }

    /// Decodes a node's declarations and feature table on first touch.
    fn decode(&mut self, node: u32) -> Result<(), Error> {
        if self.decoded.contains(node as usize) {
            return Ok(());
        }
        self.declarations[node as usize] = self.graph.declared_deps(node)?;
        self.tables[node as usize] = feature_table(self.graph, node)?;
        self.suppressed[node as usize] = suppressed_dependencies(&self.tables[node as usize]);
        self.decoded.insert(node as usize);
        Ok(())
    }

    fn enable_node(&mut self, node: u32) {
        if !self.nodes.put(node as usize) {
            self.queue.push(Task::Node(node));
        }
    }

    fn enable_dependency(&mut self, node: u32, name: &str) {
        if self.active_dependencies.insert((node, name.to_owned())) {
            self.queue.push(Task::Dependency(node, name.to_owned()));
        }
    }

    fn request_feature(&mut self, node: u32, feature: &str) {
        if self.active_features[node as usize].insert(feature.to_owned()) {
            self.queue.push(Task::Feature(node, feature.to_owned()));
        }
    }

    /// The library names every normal declaration of each package in `packages` claims on
    /// `node`, in one pass over that node's declarations.
    ///
    /// An edge whose own library name is absent from its package's set is one cargo renamed
    /// through `[lib] name`, which no declaration can spell; see [`edge_belongs`]. The answer is
    /// per package, not per declaration, so it is collected once for the whole expansion rather
    /// than re-scanned for each declaration that shares a package.
    fn claimed_library_names(
        &self,
        node: u32,
        packages: impl IntoIterator<Item = String>,
    ) -> FxHashMap<String, FxHashSet<String>> {
        let mut claimed: FxHashMap<String, FxHashSet<String>> =
            packages.into_iter().map(|package| (package, FxHashSet::default())).collect();
        for declaration in &self.declarations[node as usize] {
            if !declaration.is_normal() {
                continue;
            }
            if let Some(names) = claimed.get_mut(declaration.name.as_ref()) {
                names.insert(library_name(declaration.extern_name()));
            }
        }
        claimed
    }

    /// Requests `feature` on whatever `name` resolved to, deferring until it resolves.
    fn request_on_dependency(&mut self, node: u32, name: &str, feature: &str) {
        let key = (node, name.to_owned());
        if let Some(targets) = self.dependency_targets.get(&key) {
            for target in targets.clone() {
                self.request_feature(target, feature);
            }
        } else {
            self.pending.entry(key).or_default().push(feature.to_owned());
        }
    }

    /// Turning a package on pulls every normal declaration it does not gate behind a
    /// feature — exactly the edges cargo compiles whatever the selection is.
    fn expand_node(&mut self, node: u32) -> Result<(), Error> {
        self.decode(node)?;
        let unconditional = self.declarations[node as usize]
            .iter()
            .filter(|declaration| declaration.is_normal() && !declaration.optional)
            .map(|declaration| declaration.extern_name().to_owned())
            .collect::<Vec<_>>();
        for name in unconditional {
            self.enable_dependency(node, &name);
        }
        Ok(())
    }

    /// Links every normal declaration of `name` to its resolve edges and passes on the
    /// features those declarations request.
    ///
    /// A name can be declared more than once — `[dependencies]` beside
    /// `[target.'cfg(unix)'.dependencies]` — and each declaration contributes its own
    /// `features` and `default`.
    fn expand_dependency(&mut self, node: u32, name: &str) -> Result<(), Error> {
        self.decode(node)?;
        let wanted = library_name(name);
        let matching = self.declarations[node as usize]
            .iter()
            .filter(|declaration| declaration.is_normal() && declaration.extern_name() == name)
            .map(|declaration| {
                let features = declaration
                    .features
                    .iter()
                    .map(|feature| feature.as_ref().to_owned())
                    .collect::<Vec<_>>();
                (declaration.name.as_ref().to_owned(), declaration.uses_default_features, features)
            })
            .collect::<Vec<_>>();

        let claimed_by_package =
            self.claimed_library_names(node, matching.iter().map(|(package, ..)| package.clone()));

        let mut targets = Vec::new();
        for (package, uses_default, features) in matching {
            let claimed = &claimed_by_package[&package];
            for edge in self.graph.edges_from(node) {
                let target = self.graph.edge_target(edge);
                if self.graph.name(target) != package {
                    continue;
                }
                if !edge_belongs(self.graph.edge_extern_name(edge), &wanted, claimed) {
                    continue;
                }
                self.edges.insert(edge as usize);
                self.enable_node(target);
                if !targets.contains(&target) {
                    targets.push(target);
                }
                if uses_default {
                    self.request_feature(target, "default");
                }
                for feature in &features {
                    self.request_feature(target, feature);
                }
            }
        }

        self.dependency_targets.insert((node, name.to_owned()), targets.clone());
        if let Some(deferred) = self.pending.remove(&(node, name.to_owned())) {
            for feature in deferred {
                for &target in &targets {
                    self.request_feature(target, &feature);
                }
            }
        }
        Ok(())
    }

    /// Expands one feature of one package.
    ///
    /// A declared feature **shadows** the implicit feature an optional dependency of the
    /// same name would otherwise carry: cargo 1.98 resolves `[features] tdep = ["jiffish"]`
    /// beside `tdep = { optional = true }` by expanding `jiffish` and leaving the `tdep`
    /// edge off, and rejects the manifest outright unless some other entry reaches the
    /// dependency through `dep:tdep` or `tdep/feat`. So the table wins wherever it has a
    /// key, and the implicit feature applies only where it does not — and not even there
    /// when `dep:` syntax decoupled the two, which is the case where cargo says the feature
    /// does not exist at all.
    ///
    /// A name that is neither a feature nor a dependency is a no-op rather than an error:
    /// the document, not this walk, is the authority on which features exist.
    ///
    /// `default` is not special-cased, and must not be. A package with no `default` key but
    /// an optional dependency of that name has `default` as its default feature, because the
    /// implicit feature the dependency carries is named `default`: cargo 1.98 pulls that
    /// edge under a plain `cargo tree` and records `features: ["default"]` on the resolve
    /// node. So the fallback matches cargo here, and refusing it would *under*-activate —
    /// the one direction that turns a `deny` rule into a false pass. Where the package has
    /// neither the key nor such a dependency, the fallback finds no declaration and the
    /// default selection activates nothing, which is the rest of cargo's rule.
    fn expand_feature(&mut self, node: u32, feature: &str) -> Result<(), Error> {
        self.decode(node)?;
        let Some(entries) = self.tables[node as usize].get(feature).map(|entries| {
            entries.iter().map(|entry| entry.as_ref().to_owned()).collect::<Vec<_>>()
        }) else {
            if !self.suppressed[node as usize].contains(feature) {
                self.enable_dependency(node, feature);
            }
            return Ok(());
        };
        for entry in entries {
            self.expand_entry(node, &entry);
        }
        Ok(())
    }

    /// Applies one entry of a feature's value list.
    ///
    /// The four shapes, in the order cargo defines them: `dep:x` turns an optional
    /// dependency on without creating a feature; `x/feat` turns `x` on *and* requests
    /// `feat` on it; `x?/feat` requests `feat` only if something else turns `x` on; and a
    /// bare token is a feature of this package, falling back to the implicit
    /// optional-dependency feature in [`Walk::expand_feature`] when the table has no such
    /// key. The bare-token rule is load-bearing: coreutils' `uucore` has both an optional
    /// dependency named `time` and a feature named `time`, and
    /// `utmpx = ["time", "time/macros", …]` means the feature in the first entry and the
    /// dependency in the second.
    fn expand_entry(&mut self, node: u32, entry: &str) {
        if let Some(dependency) = entry.strip_prefix("dep:") {
            self.enable_dependency(node, dependency);
            return;
        }
        let Some((dependency, feature)) = entry.split_once('/') else {
            self.request_feature(node, entry);
            return;
        };
        if let Some(weak) = dependency.strip_suffix('?') {
            if self.active_dependencies.contains(&(node, weak.to_owned())) {
                self.request_on_dependency(node, weak, feature);
            } else {
                self.pending.entry((node, weak.to_owned())).or_default().push(feature.to_owned());
            }
            return;
        }
        self.enable_dependency(node, dependency);
        self.request_on_dependency(node, dependency, feature);
    }
}

/// Cargo's library-target spelling of a dependency name: `-` becomes `_`.
///
/// `resolve.nodes[].deps[].name` is a library target name, so a declaration's extern name
/// has to be spelled the same way before the two can be compared.
fn library_name(name: &str) -> String {
    name.replace('-', "_")
}

/// Whether an edge with library name `actual` belongs to a declaration whose own library
/// name is `wanted`, given every library name the node's declarations of the same package
/// `claimed`.
///
/// Equal names are a match. A name no declaration claimed is a `[lib] name` override — no
/// declaration could have spelled it, so the edge belongs to whichever declarations name
/// that package; over-attaching there is the safe direction, and dropping the edge would not
/// be. Excluding a claimed name cannot orphan the declaration that lost it, because cargo
/// refuses to resolve two differently-named declarations of one package to one version. A
/// document with no `deps[].name` at all falls back to the by-package-name join.
fn edge_belongs(actual: Option<&str>, wanted: &str, claimed: &FxHashSet<String>) -> bool {
    actual.is_none_or(|actual| actual == wanted || !claimed.contains(actual))
}

/// The extern names some feature value already references through `dep:` syntax.
///
/// Cargo suppresses an optional dependency's implicit feature exactly when one of these
/// mentions it — the one case where the feature table carries no key for the dependency,
/// and so the one case a reconstruction must not invent one. [`Walk::expand_feature`]
/// reads the same set to keep a bare token naming a suppressed dependency from turning it
/// on, since cargo holds that no such feature exists.
fn suppressed_dependencies(table: &FeatureTable<'_>) -> FxHashSet<String> {
    let mut suppressed = FxHashSet::default();
    for entries in table.values() {
        for entry in entries {
            if let Some(dependency) = entry.as_ref().strip_prefix("dep:") {
                suppressed.insert(dependency.trim_end_matches('?').to_owned());
            }
        }
    }
    suppressed
}

#[cfg(test)]
#[path = "features_tests.rs"]
mod tests;
