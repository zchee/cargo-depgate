//! The normal-dependency graph: interning, CSR adjacency, and BFS with witnesses.
//!
//! # Why a hand-rolled CSR and bitsets instead of petgraph or guppy
//!
//! Every rule the tool evaluates is a set test over the packages reachable from a
//! workspace member, and every failed rule must print a *witness* — the shortest
//! dependency path from the member to the offending package. That is one
//! breadth-first search per root over a static graph, `k · (V + E)` in total, and
//! nothing more. A compressed-sparse-row layout (`offsets`/`adj` as `Box<[u32]>`)
//! puts a node's out-edges in one contiguous slice, a `FixedBitSet` answers
//! "visited?" in one bit, and a `parent: Box<[u32]>` written on first visit *is*
//! the witness — no path search after the fact. petgraph would carry an edge
//! index and adjacency lists per node for a graph that is never mutated, and
//! guppy re-implements cargo's feature resolution, which this tool explicitly
//! leaves to `cargo metadata` (§1.4). Two tiny crates (`fixedbitset`,
//! `rustc-hash`) and roughly 300 lines cover the whole need, and the layout
//! scales linearly to the 20,000-package / 100,000-edge budget of §3.7.
//!
//! # Data layout
//!
//! - Node ids are `packages[]` positions (`u32`); name ids are dense `u32`
//!   ids assigned in first-seen order. Rules compare in *name-id space*
//!   (`node_to_name` projects the two), witnesses are *node* paths.
//! - Every string is borrowed from the JSON buffer through [`Meta`]; the graph
//!   itself owns only integer arrays and bitsets.
//! - The raw `dep_kinds` slice of an edge stays undecoded. A folding `Visitor`
//!   reduces it to two flags at build time without allocating; the `target`
//!   strings are decoded only when a witness is rendered.
//! - No hash lookups happen inside a traversal loop; the name projection is a
//!   slice index.
//!
//! # Traversal invariants
//!
//! [`Scratch`] holds `visited` (V bits), `reach` (name bits), `parent` and
//! `first_node` (per-name first reached node), each sized to exactly the graph
//! being traversed. `visited` and `reach` are cleared per root; `parent[v]` and
//! `first_node[n]` are written when their bit is set and read only while it
//! holds, so a stale value from an earlier root is never observed. A [`Reach`]
//! borrows the scratch shared, which makes it impossible to start the next BFS
//! while a witness is still pending: **materialise every witness before the next
//! call to [`Graph::reach`]**. The superset-edge union is the one piece of state
//! that survives across roots; [`Scratch::reset_extra`] fences it.

use std::{borrow::Cow, fmt, sync::OnceLock};

use fixedbitset::FixedBitSet;
use rustc_hash::{FxBuildHasher, FxHashMap};
use serde::{
    Deserialize,
    de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use serde_json::value::RawValue;

use crate::{
    error::Error,
    metadata::{Dep, Meta, Node, Pkg, Resolve, resolve_of},
    timings::Counters,
};

/// Compressed-sparse-row adjacency: the out-edges of node `u` are
/// `adj[offsets[u]..offsets[u + 1]]`.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Csr {
    offsets: Box<[u32]>,
    adj: Box<[u32]>,
}

impl Csr {
    fn range(&self, node: u32) -> std::ops::Range<usize> {
        let node = node as usize;
        self.offsets[node] as usize..self.offsets[node + 1] as usize
    }

    fn node_count(&self) -> usize {
        self.offsets.len() - 1
    }
}

/// The transposed graph plus, per transposed edge, the forward edge it mirrors.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Transposed {
    csr: Csr,
    forward_edge: Box<[u32]>,
}

/// The normal-dependency graph of one `cargo metadata` resolve.
///
/// Built once by [`Graph::build`]; every accessor is `O(1)` or `O(degree)`.
#[derive(Debug)]
pub struct Graph<'m> {
    meta: &'m Meta<'m>,
    forward: Csr,
    /// CSR edge → its still-borrowed `dep_kinds` slice (non-empty by construction).
    edge_kinds: Box<[&'m RawValue]>,
    /// CSR edge → the `deps[].name` cargo recorded for it: the dependency's library
    /// target name, renamed if the declaration renamed it. `None` when the document
    /// predates the field.
    edge_extern: Box<[Option<&'m str>]>,
    /// Edges whose every normal `dep_kinds` entry carries a `target`.
    edge_cfg_only: FixedBitSet,
    /// Edges out of a member whose every normal declaration of the target's name is
    /// `optional = true`.
    edge_member_optional: FixedBitSet,
    node_to_name: Box<[u32]>,
    names: Box<[&'m str]>,
    name_ids: FxHashMap<&'m str, u32>,
    /// Nodes grouped by name id: `nodes_by_name[name_offsets[n]..name_offsets[n + 1]]`.
    name_offsets: Box<[u32]>,
    nodes_by_name: Box<[u32]>,
    members: Box<[u32]>,
    is_member: FixedBitSet,
    /// The `resolve.nodes[]` entry of every package, indexed by node id: `resolve.nodes`
    /// is not in `packages[]` order, and the feature walk needs the two joined.
    resolve_nodes: Box<[&'m Node<'m>]>,
    transposed: OnceLock<Transposed>,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::MetadataInvalid { message: message.into() }
}

/// Narrows an index that an earlier `u32::try_from` check already bounded.
#[expect(clippy::cast_possible_truncation, reason = "callers bound the value by a prior check")]
const fn narrow(index: usize) -> u32 {
    index as u32
}

impl<'m> Graph<'m> {
    /// Interns `meta` into a CSR graph of normal edges, failing closed on any
    /// invariant violation (§4.9–4.12).
    ///
    /// # Errors
    ///
    /// Returns [`Error::MetadataInvalid`] when `resolve` is `null`, `workspace_members`
    /// is empty or names an unknown id, `resolve.nodes` is not 1:1 with `packages`,
    /// a package id is duplicated, an edge points to an unknown package, or an edge
    /// has an empty or absent `dep_kinds`. Returns [`Error::CargoMetadataUnparseable`]
    /// when a raw `dep_kinds` or `dependencies` slice is malformed.
    pub fn build(meta: &'m Meta<'m>) -> Result<Self, Error> {
        let resolve = resolve_of(meta)?;
        let packages = &meta.packages;
        let package_count = packages.len();
        if u32::try_from(package_count).is_err() {
            return Err(invalid(format!(
                "{package_count} packages exceed the supported node count"
            )));
        }
        if meta.workspace_members.is_empty() {
            return Err(invalid("`workspace_members` is empty"));
        }
        if resolve.nodes.len() != package_count {
            return Err(invalid(format!(
                "`resolve.nodes` has {} entries but `packages` has {package_count}",
                resolve.nodes.len()
            )));
        }
        let total_deps: usize = resolve.nodes.iter().map(|node| node.deps.len()).sum();
        if u32::try_from(total_deps).is_err() {
            return Err(invalid(format!(
                "{total_deps} resolve edges exceed the supported edge count"
            )));
        }

        let id_to_pkg = intern_ids(packages)?;
        let node_of_pkg = map_nodes(resolve, &id_to_pkg, package_count)?;
        let NameTables { names, name_ids, node_to_name } = intern_names(packages);
        let (members, is_member) = collect_members(meta, &id_to_pkg)?;
        let Csrs { forward, edge_kinds, edge_extern, edge_cfg_only } =
            build_csr(resolve, &node_of_pkg, &id_to_pkg, total_deps)?;
        let edge_member_optional =
            mark_member_optional(packages, &members, &forward, &names, &node_to_name)?;
        let (name_offsets, nodes_by_name) = group_by_name(&node_to_name, names.len());
        let resolve_nodes = node_of_pkg
            .iter()
            .map(|&node_index| &resolve.nodes[node_index as usize])
            .collect::<Vec<_>>()
            .into_boxed_slice();

        Ok(Self {
            meta,
            forward,
            edge_kinds,
            edge_extern,
            edge_cfg_only,
            edge_member_optional,
            node_to_name,
            names,
            name_ids,
            name_offsets,
            nodes_by_name,
            members,
            is_member,
            resolve_nodes,
            transposed: OnceLock::new(),
        })
    }

    /// The metadata this graph was built from.
    #[must_use]
    pub fn meta(&self) -> &'m Meta<'m> {
        self.meta
    }

    /// Number of nodes (= `packages[]` entries).
    #[must_use]
    pub fn node_count(&self) -> u32 {
        narrow(self.forward.node_count())
    }

    /// Number of normal edges in the CSR.
    #[must_use]
    pub fn edge_count(&self) -> u32 {
        narrow(self.forward.adj.len())
    }

    /// Number of distinct package names.
    #[must_use]
    pub fn name_count(&self) -> u32 {
        narrow(self.names.len())
    }

    /// The graph-size counters (§1.5) plus `unrebased_path_deps` from the metadata.
    ///
    /// `superset_extra_edges` is a traversal property: read it from
    /// [`Scratch::superset_extra_edges`] after the rule pass. The rule counters
    /// stay zero here.
    #[must_use]
    pub fn counters(&self) -> Counters {
        Counters {
            packages: self.node_count(),
            members: narrow(self.members.len()),
            normal_edges: self.edge_count(),
            names: self.name_count(),
            unrebased_path_deps: self.meta.unrebased_path_deps,
            ..Counters::default()
        }
    }

    /// The `packages[]` entry of `node`.
    #[must_use]
    pub fn package(&self, node: u32) -> &'m Pkg<'m> {
        &self.meta.packages[node as usize]
    }

    /// The `resolve.nodes[]` entry of `node`.
    #[must_use]
    pub fn resolve_node(&self, node: u32) -> &'m Node<'m> {
        self.resolve_nodes[node as usize]
    }

    /// The package name of `node`.
    #[must_use]
    pub fn name(&self, node: u32) -> &'m str {
        self.names[self.node_to_name[node as usize] as usize]
    }

    /// The package version of `node`.
    #[must_use]
    pub fn version(&self, node: u32) -> &str {
        &self.package(node).version
    }

    /// The (possibly rebased) manifest path of `node`.
    #[must_use]
    pub fn manifest_path(&self, node: u32) -> &str {
        &self.package(node).manifest_path
    }

    /// The name id of `node`.
    #[must_use]
    pub fn name_id(&self, node: u32) -> u32 {
        self.node_to_name[node as usize]
    }

    /// The `node → name id` projection, indexed by node.
    #[must_use]
    pub fn node_to_name(&self) -> &[u32] {
        &self.node_to_name
    }

    /// The name with id `name`.
    #[must_use]
    pub fn name_str(&self, name: u32) -> &'m str {
        self.names[name as usize]
    }

    /// All names, indexed by name id.
    #[must_use]
    pub fn names(&self) -> &[&'m str] {
        &self.names
    }

    /// Looks a package name up; `None` when no package in the resolve has it.
    #[must_use]
    pub fn lookup_name(&self, name: &str) -> Option<u32> {
        self.name_ids.get(name).copied()
    }

    /// The nodes (versions) carrying name id `name`, in node order.
    #[must_use]
    pub fn nodes_of_name(&self, name: u32) -> &[u32] {
        let name = name as usize;
        &self.nodes_by_name[self.name_offsets[name] as usize..self.name_offsets[name + 1] as usize]
    }

    /// Workspace member nodes in `workspace_members` order (duplicates dropped).
    #[must_use]
    pub fn members(&self) -> &[u32] {
        &self.members
    }

    /// Whether `node` is a workspace member.
    #[must_use]
    pub fn is_member(&self, node: u32) -> bool {
        self.is_member.contains(node as usize)
    }

    /// The direct normal dependencies of `node`, as nodes.
    #[must_use]
    pub fn direct_nodes(&self, node: u32) -> &[u32] {
        &self.forward.adj[self.forward.range(node)]
    }

    /// The direct normal dependencies of `node`, projected to name ids.
    ///
    /// Two versions of one name yield the same id twice; callers treat the result
    /// as a set (§4.5).
    pub fn direct(&self, node: u32) -> impl Iterator<Item = u32> + '_ {
        self.direct_nodes(node).iter().map(|&to| self.node_to_name[to as usize])
    }

    /// The CSR edge ids leaving `node`.
    #[must_use]
    pub fn edges_from(&self, node: u32) -> std::ops::Range<u32> {
        let range = self.forward.range(node);
        narrow(range.start)..narrow(range.end)
    }

    /// The first CSR edge `from → to`, if `to` is a direct normal dependency of `from`.
    #[must_use]
    pub fn edge_between(&self, from: u32, to: u32) -> Option<u32> {
        let range = self.forward.range(from);
        let position = self.forward.adj[range.clone()].iter().position(|&node| node == to)?;
        Some(narrow(range.start + position))
    }

    /// The node `edge` points to.
    #[must_use]
    pub fn edge_target(&self, edge: u32) -> u32 {
        self.forward.adj[edge as usize]
    }

    /// The node `edge` leaves; `O(log V)` via the offsets table.
    #[must_use]
    pub fn edge_source(&self, edge: u32) -> u32 {
        narrow(self.forward.offsets.partition_point(|&offset| offset <= edge) - 1)
    }

    /// Whether every normal `dep_kinds` entry of `edge` is platform-conditional (§4.7).
    #[must_use]
    pub fn edge_is_cfg_only(&self, edge: u32) -> bool {
        self.edge_cfg_only.contains(edge as usize)
    }

    /// Whether `edge` leaves a workspace member that declares the target's name
    /// `optional = true` — the precomputed answer of [`Graph::edge_declared_optional`]
    /// for member out-edges; always `false` for a non-member source.
    #[must_use]
    pub fn edge_is_member_optional(&self, edge: u32) -> bool {
        self.edge_member_optional.contains(edge as usize)
    }

    /// Whether the source package of `edge` declares the target's name optional
    /// under *every* normal declaration of it — the §1.5 "present via workspace
    /// feature unification" annotation, valid for any witness hop, member or not.
    ///
    /// A name declared required in one table and optional in another
    /// (`[dependencies]` versus `[target.'cfg(…)'.dependencies]`) is unconditionally
    /// present, so it is *not* optional. The source's `dependencies` slice is decoded
    /// on each call; [`Graph::edge_is_member_optional`] caches the same rule for the
    /// member edges the superset counter needs.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CargoMetadataUnparseable`] when the raw slice is malformed.
    pub fn edge_declared_optional(&self, edge: u32) -> Result<bool, Error> {
        let declared = self.declared_deps(self.edge_source(edge))?;
        Ok(OptionalDecls::fold(&declared).is_optional(self.name(self.edge_target(edge))))
    }

    /// The still-borrowed raw `dep_kinds` slice of `edge`.
    ///
    /// Every edge in the CSR passed the non-empty check at build time, so the slice
    /// is always present; `O(1)`.
    #[must_use]
    pub fn edge_dep_kinds(&self, edge: u32) -> &'m RawValue {
        self.edge_kinds[edge as usize]
    }

    /// The `deps[].name` of `edge`: the dependency's **library target** name, renamed if
    /// the declaration renamed it, or `None` on a document that predates the field.
    ///
    /// It is not the package name — `md-5` is reported as `md5` — so this only ever
    /// distinguishes two edges of one source, never identifies a package.
    #[must_use]
    pub fn edge_extern_name(&self, edge: u32) -> Option<&'m str> {
        self.edge_extern[edge as usize]
    }

    /// Decodes the `dep_kinds` entries of `edge` — the lazy path used only when a
    /// witness renders `[cfg(...)]` (§3.2 step 6).
    ///
    /// # Errors
    ///
    /// Returns [`Error::CargoMetadataUnparseable`] when the raw slice is malformed.
    pub fn edge_kinds(&self, edge: u32) -> Result<Vec<KindEntry>, Error> {
        serde_json::from_str(self.edge_dep_kinds(edge).get())
            .map_err(|source| Error::CargoMetadataUnparseable { source })
    }

    /// Decodes the declared `dependencies` of `node` (all kinds, optional flags, targets).
    ///
    /// Used for the `direct`-optional check, optional-edge annotation and manifest
    /// spans — never for the graph itself, which comes from the resolve (§4.5).
    ///
    /// # Errors
    ///
    /// Returns [`Error::CargoMetadataUnparseable`] when the raw slice is malformed.
    pub fn declared_deps(&self, node: u32) -> Result<Vec<DeclaredDep<'m>>, Error> {
        declared_deps(self.package(node))
    }

    fn transposed(&self) -> &Transposed {
        self.transposed.get_or_init(|| {
            let node_count = self.forward.node_count();
            let edge_count = self.forward.adj.len();
            let mut offsets = vec![0_u32; node_count + 1];
            for &to in &self.forward.adj {
                offsets[to as usize + 1] += 1;
            }
            for index in 0..node_count {
                offsets[index + 1] += offsets[index];
            }
            let mut fill = offsets.clone();
            let mut adj = vec![0_u32; edge_count];
            let mut forward_edge = vec![0_u32; edge_count];
            for from in 0..node_count {
                for edge in self.forward.range(narrow(from)) {
                    let to = self.forward.adj[edge] as usize;
                    let slot = &mut fill[to];
                    adj[*slot as usize] = narrow(from);
                    forward_edge[*slot as usize] = narrow(edge);
                    *slot += 1;
                }
            }
            Transposed {
                csr: Csr { offsets: offsets.into_boxed_slice(), adj: adj.into_boxed_slice() },
                forward_edge: forward_edge.into_boxed_slice(),
            }
        })
    }

    /// Whether the transposed CSR has been built (it is built lazily by
    /// [`Graph::reverse_reach`]).
    #[must_use]
    pub fn has_transposed(&self) -> bool {
        self.transposed.get().is_some()
    }

    /// The nodes that directly depend on `node` (in-neighbours), building the
    /// transposed CSR on first use.
    #[must_use]
    pub fn dependents(&self, node: u32) -> &[u32] {
        let transposed = self.transposed();
        &transposed.csr.adj[transposed.csr.range(node)]
    }

    /// Runs one forward BFS from `root` and returns its reach.
    ///
    /// `visited` and `reach` are cleared first; `parent` is written on first visit.
    /// Every cfg-only or member-optional edge whose source is reached is added to
    /// the scratch's union of traversed superset edges. Materialise all witnesses
    /// before the next call — the borrow checker enforces it.
    ///
    /// # Panics
    ///
    /// Panics if `root` is not a node of this graph.
    pub fn reach<'s>(&'s self, root: u32, scratch: &'s mut Scratch) -> Reach<'s, 'm> {
        assert!((root as usize) < self.forward.node_count(), "root {root} is not a node");
        scratch.prepare(self);
        self.bfs(&self.forward, None, root, scratch, None);
        Reach { graph: self, scratch, root, direction: Direction::Forward }
    }

    /// Runs one forward BFS from `root` over the edges `activated` selects.
    ///
    /// `activated` is a bitset over CSR edge ids — an [`crate::features::Activation`]'s edge
    /// set — so the reach is the closure a build of `root` under that feature selection would
    /// compile, with the witnesses [`Graph::reach`] produces on the unified graph. An edge the
    /// activation left out is not traversed and does not enter the superset-edge union either:
    /// it is not part of this rule's closure at all.
    ///
    /// # Panics
    ///
    /// Panics if `root` is not a node of this graph, or if `activated` is not sized to its
    /// edges — a mask from another graph would silently truncate.
    pub fn reach_activated<'s>(
        &'s self,
        root: u32,
        activated: &FixedBitSet,
        scratch: &'s mut Scratch,
    ) -> Reach<'s, 'm> {
        assert!((root as usize) < self.forward.node_count(), "root {root} is not a node");
        assert_eq!(
            activated.len(),
            self.forward.adj.len(),
            "the activation mask belongs to another graph"
        );
        scratch.prepare(self);
        self.bfs(&self.forward, None, root, scratch, Some(activated));
        Reach { graph: self, scratch, root, direction: Direction::Forward }
    }

    /// Runs one reverse BFS from `root` over the lazily built transposed CSR.
    ///
    /// The reach contains every node that transitively depends on `root`;
    /// [`Reach::witness_to_node`] then yields the forward path from that node down
    /// to `root`. Shares [`Scratch`] with [`Graph::reach`] under the same invariants.
    ///
    /// # Panics
    ///
    /// Panics if `root` is not a node of this graph.
    pub fn reverse_reach<'s>(&'s self, root: u32, scratch: &'s mut Scratch) -> Reach<'s, 'm> {
        assert!((root as usize) < self.forward.node_count(), "root {root} is not a node");
        scratch.prepare(self);
        let transposed = self.transposed();
        self.bfs(&transposed.csr, Some(&transposed.forward_edge), root, scratch, None);
        Reach { graph: self, scratch, root, direction: Direction::Reverse }
    }

    fn bfs(
        &self,
        csr: &Csr,
        forward_edge: Option<&[u32]>,
        root: u32,
        scratch: &mut Scratch,
        activated: Option<&FixedBitSet>,
    ) {
        scratch.visited.clear();
        scratch.reach.clear();
        scratch.queue.clear();
        scratch.traversals += 1;

        scratch.visited.insert(root as usize);
        scratch.parent[root as usize] = root;
        scratch.mark_name(self.node_to_name[root as usize], root);
        scratch.queue.push(root);

        let mut head = 0;
        while let Some(&from) = scratch.queue.get(head) {
            head += 1;
            for edge in csr.range(from) {
                let to = csr.adj[edge];
                let forward = forward_edge.map_or(edge, |map| map[edge] as usize);
                if activated.is_some_and(|activated| !activated.contains(forward)) {
                    continue;
                }
                if self.edge_cfg_only.contains(forward)
                    || self.edge_member_optional.contains(forward)
                {
                    scratch.extra.insert(forward);
                }
                if !scratch.visited.put(to as usize) {
                    scratch.parent[to as usize] = from;
                    scratch.mark_name(self.node_to_name[to as usize], to);
                    scratch.queue.push(to);
                }
            }
        }
    }
}

/// Interns `packages[].id` → node index, rejecting duplicates (§4.12).
fn intern_ids<'m>(packages: &'m [Pkg<'m>]) -> Result<FxHashMap<&'m str, u32>, Error> {
    let mut id_to_pkg = FxHashMap::with_capacity_and_hasher(packages.len(), FxBuildHasher);
    for (index, package) in packages.iter().enumerate() {
        if id_to_pkg.insert(package.id.as_ref(), narrow(index)).is_some() {
            return Err(invalid(format!("duplicate package id `{}`", package.id)));
        }
    }
    Ok(id_to_pkg)
}

/// Maps every package to its resolve node, requiring the two to be 1:1 (§4.12).
fn map_nodes(
    resolve: &Resolve<'_>,
    id_to_pkg: &FxHashMap<&str, u32>,
    package_count: usize,
) -> Result<Box<[u32]>, Error> {
    let mut node_of_pkg = vec![u32::MAX; package_count].into_boxed_slice();
    for (node_index, node) in resolve.nodes.iter().enumerate() {
        let Some(&pkg) = id_to_pkg.get(node.id.as_ref()) else {
            return Err(invalid(format!("resolve node `{}` has no `packages` entry", node.id)));
        };
        if node_of_pkg[pkg as usize] != u32::MAX {
            return Err(invalid(format!("`resolve.nodes` lists `{}` twice", node.id)));
        }
        node_of_pkg[pkg as usize] = narrow(node_index);
    }
    // Equal counts plus an injective node → package map leave no package without a node.
    debug_assert!(node_of_pkg.iter().all(|&node| node != u32::MAX));
    Ok(node_of_pkg)
}

/// The name interning tables: names by id, id by name, and the node projection.
struct NameTables<'m> {
    names: Box<[&'m str]>,
    name_ids: FxHashMap<&'m str, u32>,
    node_to_name: Box<[u32]>,
}

/// Assigns dense name ids in first-seen order and projects nodes onto them (§4.4).
fn intern_names<'m>(packages: &'m [Pkg<'m>]) -> NameTables<'m> {
    let mut name_ids: FxHashMap<&'m str, u32> = FxHashMap::default();
    let mut names: Vec<&'m str> = Vec::new();
    let mut node_to_name = Vec::with_capacity(packages.len());
    for package in packages {
        let name: &'m str = package.name.as_ref();
        let id = *name_ids.entry(name).or_insert_with(|| {
            names.push(name);
            narrow(names.len() - 1)
        });
        node_to_name.push(id);
    }
    NameTables {
        names: names.into_boxed_slice(),
        name_ids,
        node_to_name: node_to_name.into_boxed_slice(),
    }
}

/// Resolves `workspace_members` to nodes, dropping duplicates.
fn collect_members(
    meta: &Meta<'_>,
    id_to_pkg: &FxHashMap<&str, u32>,
) -> Result<(Box<[u32]>, FixedBitSet), Error> {
    let mut members = Vec::with_capacity(meta.workspace_members.len());
    let mut is_member = FixedBitSet::with_capacity(meta.packages.len());
    for member in &meta.workspace_members {
        let Some(&pkg) = id_to_pkg.get(member.as_ref()) else {
            return Err(invalid(format!("workspace member `{member}` is not in `packages`")));
        };
        if !is_member.put(pkg as usize) {
            members.push(pkg);
        }
    }
    Ok((members.into_boxed_slice(), is_member))
}

/// The per-edge tables [`build_csr`] produces alongside the adjacency itself.
struct Csrs<'m> {
    forward: Csr,
    edge_kinds: Box<[&'m RawValue]>,
    edge_extern: Box<[Option<&'m str>]>,
    edge_cfg_only: FixedBitSet,
}

/// Folds every edge's `dep_kinds` and lays the normal edges out as a CSR (§3.2 steps 5–6).
fn build_csr<'m>(
    resolve: &'m Resolve<'m>,
    node_of_pkg: &[u32],
    id_to_pkg: &FxHashMap<&str, u32>,
    total_deps: usize,
) -> Result<Csrs<'m>, Error> {
    let mut offsets = Vec::with_capacity(node_of_pkg.len() + 1);
    let mut adj: Vec<u32> = Vec::with_capacity(total_deps);
    let mut edge_kinds: Vec<&'m RawValue> = Vec::with_capacity(total_deps);
    let mut edge_extern: Vec<Option<&'m str>> = Vec::with_capacity(total_deps);
    let mut cfg_only_edges: Vec<u32> = Vec::new();
    for &node_index in node_of_pkg {
        offsets.push(narrow(adj.len()));
        let node = &resolve.nodes[node_index as usize];
        for dep in &node.deps {
            let Some(&to) = id_to_pkg.get(dep.pkg.as_ref()) else {
                return Err(invalid(format!(
                    "edge `{}` -> `{}` points to a package that is not in `packages`",
                    node.id, dep.pkg
                )));
            };
            let no_dep_kinds = || {
                invalid(format!(
                    "edge `{}` -> `{}` has no `dep_kinds`; cargo 1.41 or newer is required",
                    node.id, dep.pkg
                ))
            };
            let Some(raw) = dep.dep_kinds else {
                return Err(no_dep_kinds());
            };
            let fold = fold_raw_dep_kinds(raw)
                .map_err(|source| Error::CargoMetadataUnparseable { source })?;
            if fold.entries == 0 {
                return Err(no_dep_kinds());
            }
            if !fold.has_normal() {
                continue;
            }
            if fold.all_normal_targeted() {
                cfg_only_edges.push(narrow(adj.len()));
            }
            adj.push(to);
            edge_kinds.push(raw);
            edge_extern.push(dep.name.as_deref());
        }
    }
    offsets.push(narrow(adj.len()));
    let mut edge_cfg_only = FixedBitSet::with_capacity(adj.len());
    for edge in cfg_only_edges {
        edge_cfg_only.insert(edge as usize);
    }
    Ok(Csrs {
        forward: Csr { offsets: offsets.into_boxed_slice(), adj: adj.into_boxed_slice() },
        edge_kinds: edge_kinds.into_boxed_slice(),
        edge_extern: edge_extern.into_boxed_slice(),
        edge_cfg_only,
    })
}

/// One package's normal declarations folded by name (§1.5).
///
/// The flag means "this edge may exist only through feature unification", so a name
/// counts as optional only when *every* normal declaration of it says `optional =
/// true`: a required `[dependencies]` entry next to an optional
/// `[target.'cfg(…)'.dependencies]` entry is an unconditionally present edge. This is
/// the single place that decides the any/all question for both
/// [`Graph::edge_declared_optional`] and the precomputed member bitset.
struct OptionalDecls<'d> {
    all_optional_by_name: FxHashMap<&'d str, bool>,
}

impl<'d> OptionalDecls<'d> {
    fn fold(declared: &'d [DeclaredDep<'_>]) -> Self {
        let mut all_optional_by_name = FxHashMap::default();
        for dep in declared.iter().filter(|dep| dep.is_normal()) {
            all_optional_by_name
                .entry(dep.name.as_ref())
                .and_modify(|all_optional: &mut bool| *all_optional &= dep.optional)
                .or_insert(dep.optional);
        }
        Self { all_optional_by_name }
    }

    /// Whether `name` is declared, and never as a required dependency.
    fn is_optional(&self, name: &str) -> bool {
        self.all_optional_by_name.get(name).is_some_and(|&all_optional| all_optional)
    }

    /// Whether any name at all folds to optional.
    fn any(&self) -> bool {
        self.all_optional_by_name.values().any(|&all_optional| all_optional)
    }
}

/// Marks the edges out of each member whose target is declared optional (§1.5).
///
/// Matching is by package name against the member's normal declarations through
/// [`OptionalDecls`]; the member's `dependencies` slice is decoded once here and
/// never in a traversal.
fn mark_member_optional(
    packages: &[Pkg<'_>],
    members: &[u32],
    forward: &Csr,
    names: &[&str],
    node_to_name: &[u32],
) -> Result<FixedBitSet, Error> {
    let mut edge_member_optional = FixedBitSet::with_capacity(forward.adj.len());
    for &member in members {
        let declared = declared_deps(&packages[member as usize])?;
        let optional = OptionalDecls::fold(&declared);
        if !optional.any() {
            continue;
        }
        for edge in forward.range(member) {
            let to = forward.adj[edge];
            if optional.is_optional(names[node_to_name[to as usize] as usize]) {
                edge_member_optional.insert(edge);
            }
        }
    }
    Ok(edge_member_optional)
}

/// Groups nodes by name id with a counting sort: `nodes_by_name[offsets[n]..offsets[n + 1]]`.
fn group_by_name(node_to_name: &[u32], name_count: usize) -> (Box<[u32]>, Box<[u32]>) {
    let mut name_offsets = vec![0_u32; name_count + 1];
    for &name in node_to_name {
        name_offsets[name as usize + 1] += 1;
    }
    for index in 0..name_count {
        name_offsets[index + 1] += name_offsets[index];
    }
    let mut fill = name_offsets.clone();
    let mut nodes_by_name = vec![0_u32; node_to_name.len()];
    for (node, &name) in node_to_name.iter().enumerate() {
        let slot = &mut fill[name as usize];
        nodes_by_name[*slot as usize] = narrow(node);
        *slot += 1;
    }
    (name_offsets.into_boxed_slice(), nodes_by_name.into_boxed_slice())
}

/// Reusable traversal state; allocate once per run with [`Scratch::new`].
#[derive(Clone, Debug)]
pub struct Scratch {
    visited: FixedBitSet,
    reach: FixedBitSet,
    parent: Box<[u32]>,
    first_node: Box<[u32]>,
    queue: Vec<u32>,
    extra: FixedBitSet,
    traversals: u32,
}

impl Scratch {
    /// Allocates traversal state sized for `graph`.
    #[must_use]
    pub fn new(graph: &Graph<'_>) -> Self {
        let nodes = graph.forward.node_count();
        let names = graph.names.len();
        Self {
            visited: FixedBitSet::with_capacity(nodes),
            reach: FixedBitSet::with_capacity(names),
            parent: vec![u32::MAX; nodes].into_boxed_slice(),
            first_node: vec![u32::MAX; names].into_boxed_slice(),
            queue: Vec::with_capacity(nodes),
            extra: FixedBitSet::with_capacity(graph.forward.adj.len()),
            traversals: 0,
        }
    }

    /// Resizes the state to exactly `graph`'s dimensions when it was allocated for a
    /// different graph, growing or shrinking, so [`Reach::nodes`] and
    /// [`Reach::names`] never carry a stale length. `FixedBitSet` set operations on
    /// unequal lengths truncate silently, so an exact length is what keeps a rule
    /// mask intersection honest. The superset union is reallocated whenever any
    /// dimension changes: its edge ids belong to one graph (a different graph with
    /// identical node, name and edge counts is indistinguishable here — reuse a
    /// scratch across graphs only after [`Scratch::reset_extra`]).
    fn prepare(&mut self, graph: &Graph<'_>) {
        let nodes = graph.forward.node_count();
        let names = graph.names.len();
        let edges = graph.forward.adj.len();
        let dimensions_changed = self.parent.len() != nodes
            || self.first_node.len() != names
            || self.extra.len() != edges;
        if !dimensions_changed {
            return;
        }
        self.visited = FixedBitSet::with_capacity(nodes);
        self.parent = vec![u32::MAX; nodes].into_boxed_slice();
        self.reach = FixedBitSet::with_capacity(names);
        self.first_node = vec![u32::MAX; names].into_boxed_slice();
        self.extra = FixedBitSet::with_capacity(edges);
    }

    fn mark_name(&mut self, name: u32, node: u32) {
        if !self.reach.put(name as usize) {
            self.first_node[name as usize] = node;
        }
    }

    /// Distinct cfg-only or member-optional edges traversed by any BFS since the
    /// last [`Scratch::reset_extra`] — the `superset_extra_edges` counter, a union
    /// over roots, never a sum (§1.5).
    #[must_use]
    pub fn superset_extra_edges(&self) -> u32 {
        narrow(self.extra.count_ones(..))
    }

    /// Clears the superset-edge union so the next traversals start a fresh count.
    ///
    /// §1.5 scopes the counter to the rule pass, and the union otherwise survives for
    /// the scratch's whole life: P2 fences the rule pass with a `reset_extra` before
    /// it and reads [`Scratch::superset_extra_edges`] right after, so an `explain`
    /// traversal on the same scratch never leaks into the reported number.
    pub fn reset_extra(&mut self) {
        self.extra.clear();
    }

    /// Number of BFS runs performed so far.
    #[must_use]
    pub fn traversals(&self) -> u32 {
        self.traversals
    }
}

/// Which way the BFS that produced a [`Reach`] walked the edges.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    /// Dependencies of the root.
    Forward,
    /// Dependents of the root.
    Reverse,
}

/// The result of one BFS, valid until the next traversal on the same [`Scratch`].
#[derive(Debug)]
pub struct Reach<'s, 'm> {
    graph: &'s Graph<'m>,
    scratch: &'s Scratch,
    root: u32,
    direction: Direction,
}

impl Reach<'_, '_> {
    /// The BFS root.
    #[must_use]
    pub fn root(&self) -> u32 {
        self.root
    }

    /// Whether this reach was produced by [`Graph::reach`] or [`Graph::reverse_reach`].
    #[must_use]
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// The reached nodes (the root included), as a bitset over node ids.
    #[must_use]
    pub fn nodes(&self) -> &FixedBitSet {
        &self.scratch.visited
    }

    /// The reached names (the root's included), as a bitset over name ids.
    ///
    /// Rule masks intersect this directly: no allocation, no hashing.
    #[must_use]
    pub fn names(&self) -> &FixedBitSet {
        &self.scratch.reach
    }

    /// Whether `node` was reached.
    #[must_use]
    pub fn contains_node(&self, node: u32) -> bool {
        self.scratch.visited.contains(node as usize)
    }

    /// Whether any node named `name` was reached.
    #[must_use]
    pub fn contains_name(&self, name: u32) -> bool {
        self.scratch.reach.contains(name as usize)
    }

    /// The first node of `name` the BFS reached — the one a witness ends at (§4.4).
    #[must_use]
    pub fn first_node_of_name(&self, name: u32) -> Option<u32> {
        self.contains_name(name).then(|| self.scratch.first_node[name as usize])
    }

    /// Reached workspace members other than the root, in node order.
    pub fn reached_members(&self) -> impl Iterator<Item = u32> + '_ {
        self.scratch
            .visited
            .ones()
            .map(narrow)
            .filter(move |&node| node != self.root && self.graph.is_member(node))
    }

    /// The witness path to `node`: a forward dependency path read back from the
    /// BFS parent chain, shortest by construction.
    ///
    /// For a forward reach the path runs `root → … → node`; for a reverse reach it
    /// runs `node → … → root`. `None` when `node` was not reached.
    #[must_use]
    pub fn witness_to_node(&self, node: u32) -> Option<Vec<u32>> {
        if !self.contains_node(node) {
            return None;
        }
        let mut path = Vec::new();
        let mut current = node;
        loop {
            path.push(current);
            if current == self.root {
                break;
            }
            current = self.scratch.parent[current as usize];
        }
        if self.direction == Direction::Forward {
            path.reverse();
        }
        Some(path)
    }

    /// The witness path to the first reached node of `name`.
    #[must_use]
    pub fn witness_to_name(&self, name: u32) -> Option<Vec<u32>> {
        self.witness_to_node(self.first_node_of_name(name)?)
    }

    /// Materialises the witness to the first reached node of `name`, plus the
    /// other reached nodes of that name (§1.4: `(other reachable versions: …)`).
    ///
    /// The witness ends at that node for a forward reach and begins at it for a
    /// reverse one; the "other versions" never include that node — so for the root's
    /// own name (where the root is the first reached node) they never include the root.
    #[must_use]
    pub fn witness_with_versions(&self, name: u32) -> Option<(Vec<u32>, Vec<u32>)> {
        let first = self.first_node_of_name(name)?;
        let path = self.witness_to_node(first)?;
        let others = self
            .graph
            .nodes_of_name(name)
            .iter()
            .copied()
            .filter(|&node| node != first && self.contains_node(node))
            .collect();
        Some((path, others))
    }
}

/// The two flags §3.2 step 5 folds an edge's `dep_kinds` into, plus the entry count
/// that drives the §4.11 empty check.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KindFold {
    /// Number of `dep_kinds` entries.
    pub entries: u32,
    /// Number of entries with `kind == null`.
    pub normal: u32,
    /// Number of normal entries with a non-null `target`.
    pub normal_targeted: u32,
}

impl KindFold {
    /// Whether any entry is normal (`kind == null`) — the edge belongs to the graph.
    #[must_use]
    pub const fn has_normal(&self) -> bool {
        self.normal > 0
    }

    /// Whether every normal entry is platform-conditional (§4.2, §4.7).
    #[must_use]
    pub const fn all_normal_targeted(&self) -> bool {
        self.normal > 0 && self.normal == self.normal_targeted
    }
}

/// Folds `dep.dep_kinds` without allocating: the raw slice is re-scanned by a
/// `Visitor` that only counts. An absent array folds to zero entries.
///
/// # Errors
///
/// Returns the JSON error when the raw slice is not an array of objects.
pub fn fold_dep_kinds(dep: &Dep<'_>) -> Result<KindFold, serde_json::Error> {
    dep.dep_kinds.map_or_else(|| Ok(KindFold::default()), fold_raw_dep_kinds)
}

/// Folds one raw `dep_kinds` array; see [`fold_dep_kinds`].
fn fold_raw_dep_kinds(raw: &RawValue) -> Result<KindFold, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let fold = (&mut deserializer).deserialize_seq(FoldVisitor)?;
    deserializer.end()?;
    Ok(fold)
}

struct FoldVisitor;

impl<'de> Visitor<'de> for FoldVisitor {
    type Value = KindFold;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a `dep_kinds` array")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut fold = KindFold::default();
        while let Some(entry) = seq.next_element::<EntryFlags>()? {
            fold.entries += 1;
            if entry.normal {
                fold.normal += 1;
                if entry.targeted {
                    fold.normal_targeted += 1;
                }
            }
        }
        Ok(fold)
    }
}

/// One `dep_kinds` entry reduced to "is normal" and "has a target".
struct EntryFlags {
    normal: bool,
    targeted: bool,
}

impl<'de> Deserialize<'de> for EntryFlags {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(EntryVisitor)
    }
}

struct EntryVisitor;

impl<'de> Visitor<'de> for EntryVisitor {
    type Value = EntryFlags;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a `dep_kinds` entry object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        // `kind` defaults to normal when absent, matching cargo's own serialisation.
        let mut flags = EntryFlags { normal: true, targeted: false };
        while let Some(key) = map.next_key::<EntryField>()? {
            match key {
                EntryField::Kind => flags.normal = map.next_value::<IsNull>()?.0,
                EntryField::Target => flags.targeted = !map.next_value::<IsNull>()?.0,
                EntryField::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(flags)
    }
}

enum EntryField {
    Kind,
    Target,
    Other,
}

impl<'de> Deserialize<'de> for EntryField {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FieldVisitor;

        impl Visitor<'_> for FieldVisitor {
            type Value = EntryField;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a `dep_kinds` entry key")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(match value {
                    "kind" => EntryField::Kind,
                    "target" => EntryField::Target,
                    _ => EntryField::Other,
                })
            }
        }

        deserializer.deserialize_identifier(FieldVisitor)
    }
}

/// `true` when the value is JSON `null`; any string (or other scalar) is `false`.
struct IsNull(bool);

impl<'de> Deserialize<'de> for IsNull {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NullVisitor;

        impl Visitor<'_> for NullVisitor {
            type Value = IsNull;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("null or a string")
            }

            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(IsNull(true))
            }

            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(IsNull(true))
            }

            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(IsNull(false))
            }
        }

        deserializer.deserialize_any(NullVisitor)
    }
}

/// One decoded `dep_kinds` entry (the lazy path, §3.2 step 6).
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct KindEntry {
    /// `None` for a normal dependency; `"build"` or `"dev"` otherwise.
    #[serde(default)]
    pub kind: Option<String>,
    /// The `cfg(...)` or target triple the entry is conditional on.
    #[serde(default)]
    pub target: Option<String>,
}

impl KindEntry {
    /// Whether this entry is a normal (non-build, non-dev) dependency.
    #[must_use]
    pub fn is_normal(&self) -> bool {
        self.kind.is_none()
    }
}

/// One declared `packages[].dependencies[]` entry, borrowed from the JSON.
///
/// `#[non_exhaustive]` because the feature walk keeps pulling more of the declaration in
/// and a downstream struct literal would break on each addition; the crate's own
/// construction path is deserialization.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[non_exhaustive]
pub struct DeclaredDep<'a> {
    /// The dependency's package name (never the rename).
    #[serde(borrow)]
    pub name: Cow<'a, str>,
    /// `None` for a normal dependency; `"build"` or `"dev"` otherwise.
    #[serde(borrow, default)]
    pub kind: Option<Cow<'a, str>>,
    /// `optional = true`.
    #[serde(default)]
    pub optional: bool,
    /// The alias the package is used under, when renamed.
    #[serde(borrow, default)]
    pub rename: Option<Cow<'a, str>>,
    /// The `cfg(...)` or target triple the declaration is conditional on.
    #[serde(borrow, default)]
    pub target: Option<Cow<'a, str>>,
    /// The features this declaration requests on the dependency.
    #[serde(borrow, default)]
    pub features: Vec<Cow<'a, str>>,
    /// Whether the dependency's `default` feature is requested.
    ///
    /// Cargo's default is `true`, so an absent key means `true` here as well.
    #[serde(default = "uses_default_features_default")]
    pub uses_default_features: bool,
}

fn uses_default_features_default() -> bool {
    true
}

impl DeclaredDep<'_> {
    /// Whether this declaration is in a `[dependencies]` (or `target.*.dependencies`) table.
    #[must_use]
    pub fn is_normal(&self) -> bool {
        self.kind.is_none()
    }

    /// The name the dependency is used under: the rename when it has one, else the
    /// package name.
    ///
    /// Feature syntax addresses a dependency by this name (`serde1/derive` refers to the
    /// rename). The resolve edge is found by [`DeclaredDep::name`] and told apart from the
    /// other edges of that package by [`Graph::edge_extern_name`].
    #[must_use]
    pub fn extern_name(&self) -> &str {
        self.rename.as_deref().unwrap_or(&self.name)
    }
}

fn declared_deps<'m>(package: &Pkg<'m>) -> Result<Vec<DeclaredDep<'m>>, Error> {
    serde_json::from_str(package.dependencies.get())
        .map_err(|source| Error::CargoMetadataUnparseable { source })
}

#[cfg(test)]
#[path = "graph_tests.rs"]
mod tests;
