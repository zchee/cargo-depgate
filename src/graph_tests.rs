#![expect(clippy::expect_used, reason = "test bodies assert directly")]

use std::collections::VecDeque;

use super::*;
use crate::metadata::{MetadataBuffer, parse, tests::fixture_json};

/// Parses `json` into a leaked, `'static` [`Meta`] so graphs can borrow it freely.
fn meta(json: &str) -> &'static Meta<'static> {
    let buffer: &'static MetadataBuffer =
        Box::leak(Box::new(MetadataBuffer::from_bytes(json.as_bytes().to_vec())));
    Box::leak(Box::new(parse(buffer).expect("fixture parses")))
}

fn fixture_graph() -> Graph<'static> {
    Graph::build(meta(&fixture_json())).expect("fixture graph builds")
}

/// A synthetic-graph description: `packages[i] = (name, version)`, `edges = (from, to,
/// dep_kinds JSON)`, `members` by index, `decls` the `dependencies[]` entries of the
/// packages (only the ones a test cares about; the resolve edges are independent).
#[derive(Default)]
struct Spec {
    packages: Vec<(&'static str, &'static str)>,
    edges: Vec<(usize, usize, &'static str)>,
    members: Vec<usize>,
    decls: Vec<Decl>,
}

/// One normal `dependencies[]` entry of `packages[package]`.
#[derive(Clone, Copy)]
struct Decl {
    package: usize,
    name: &'static str,
    optional: bool,
    target: Option<&'static str>,
    /// `None` is a normal declaration; `Some("dev")`/`Some("build")` the other kinds.
    kind: Option<&'static str>,
}

impl Decl {
    const fn optional(package: usize, name: &'static str) -> Self {
        Self { package, name, optional: true, target: None, kind: None }
    }

    const fn required(package: usize, name: &'static str) -> Self {
        Self { package, name, optional: false, target: None, kind: None }
    }

    const fn on(self, target: &'static str) -> Self {
        Self { target: Some(target), ..self }
    }

    const fn dev(self) -> Self {
        Self { kind: Some("dev"), ..self }
    }

    fn json(self) -> String {
        let target =
            self.target.map_or_else(String::new, |target| format!(r#","target":"{target}""#));
        let kind = self.kind.map_or_else(|| "null".to_owned(), |kind| format!(r#""{kind}""#));
        format!(r#"{{"name":"{}","kind":{kind},"optional":{}{target}}}"#, self.name, self.optional)
    }
}

const NORMAL: &str = r#"[{"kind":null,"target":null}]"#;
const CFG_ONLY: &str = r#"[{"kind":null,"target":"cfg(windows)"}]"#;

impl Spec {
    fn id(&self, index: usize) -> String {
        let (name, version) = self.packages[index];
        if self.members.contains(&index) {
            format!("path+file:///ws/{name}#{version}")
        } else {
            format!("registry+https://example.invalid/index#{name}@{version}")
        }
    }

    fn json(&self) -> String {
        let packages: Vec<String> = (0..self.packages.len())
            .map(|index| {
                let (name, version) = self.packages[index];
                let declared: Vec<String> = self
                    .decls
                    .iter()
                    .filter(|decl| decl.package == index)
                    .map(|decl| decl.json())
                    .collect();
                let source = if self.members.contains(&index) {
                    "null".to_owned()
                } else {
                    r#""registry+https://example.invalid/index""#.to_owned()
                };
                format!(
                    r#"{{"name":"{name}","version":"{version}","id":"{}","source":{source},"manifest_path":"/ws/{name}/Cargo.toml","dependencies":[{}]}}"#,
                    self.id(index),
                    declared.join(",")
                )
            })
            .collect();
        let nodes: Vec<String> = (0..self.packages.len())
            .map(|index| {
                let deps: Vec<String> = self
                    .edges
                    .iter()
                    .filter(|(from, _, _)| *from == index)
                    .map(|(_, to, kinds)| {
                        format!(
                            r#"{{"name":"{}","pkg":"{}","dep_kinds":{kinds}}}"#,
                            self.packages[*to].0,
                            self.id(*to)
                        )
                    })
                    .collect();
                format!(r#"{{"id":"{}","deps":[{}]}}"#, self.id(index), deps.join(","))
            })
            .collect();
        let members: Vec<String> =
            self.members.iter().map(|&member| format!("\"{}\"", self.id(member))).collect();
        format!(
            r#"{{"packages":[{}],"workspace_members":[{}],"workspace_root":"/ws","resolve":{{"nodes":[{}],"root":null}}}}"#,
            packages.join(","),
            members.join(","),
            nodes.join(",")
        )
    }

    fn graph(&self) -> Graph<'static> {
        Graph::build(meta(&self.json())).expect("synthetic graph builds")
    }
}

/// A 12-node graph with two roots, a diamond, two versions of one name at different
/// depths, a long detour that must never win over the short path, and a shared tail.
///
/// ```text
/// 0 a ─→ 1 b ─→ 3 d ─→ 5 f ─→ 7 h(1.0)
/// 0 a ─→ 2 c ─→ 3 d
/// 0 a ─→ 4 e ─→ 5 f
/// 4 e ─→ 6 g ─→ 8 h(2.0)
/// 9 r ─→ 6 g
/// 9 r ─→ 10 s ─→ 11 t ─→ 5 f
/// 10 s ─→ 7 h(1.0)
/// ```
fn twelve_node_spec() -> Spec {
    Spec {
        packages: vec![
            ("a", "1.0.0"),
            ("b", "1.0.0"),
            ("c", "1.0.0"),
            ("d", "1.0.0"),
            ("e", "1.0.0"),
            ("f", "1.0.0"),
            ("g", "1.0.0"),
            ("h", "1.0.0"),
            ("h", "2.0.0"),
            ("r", "1.0.0"),
            ("s", "1.0.0"),
            ("t", "1.0.0"),
        ],
        edges: vec![
            (0, 1, NORMAL),
            (1, 3, NORMAL),
            (3, 5, NORMAL),
            (5, 7, NORMAL),
            (0, 2, NORMAL),
            (2, 3, NORMAL),
            (0, 4, NORMAL),
            (4, 5, NORMAL),
            (4, 6, NORMAL),
            (6, 8, NORMAL),
            (9, 6, NORMAL),
            (9, 10, NORMAL),
            (10, 11, NORMAL),
            (11, 5, NORMAL),
            (10, 7, NORMAL),
        ],
        members: vec![0, 9],
        decls: Vec::new(),
    }
}

/// An independent BFS distance table over the spec's edge list.
fn distances(spec: &Spec, root: usize) -> Vec<Option<usize>> {
    let mut distance = vec![None; spec.packages.len()];
    distance[root] = Some(0);
    let mut queue = VecDeque::from([root]);
    while let Some(node) = queue.pop_front() {
        let next = distance[node].expect("queued nodes have a distance") + 1;
        for &(from, to, _) in &spec.edges {
            if from == node && distance[to].is_none() {
                distance[to] = Some(next);
                queue.push_back(to);
            }
        }
    }
    distance
}

fn names_of(graph: &Graph<'_>, nodes: &[u32]) -> Vec<String> {
    nodes.iter().map(|&node| format!("{} {}", graph.name(node), graph.version(node))).collect()
}

#[test]
fn csr_holds_exactly_the_normal_edges_in_package_order() {
    let graph = fixture_graph();

    assert_eq!(graph.node_count(), 7);
    assert_eq!(graph.edge_count(), 3);
    assert_eq!(graph.direct_nodes(0), [1, 2], "app → lib, serde 1.0.0");
    assert_eq!(graph.direct_nodes(1), [3], "lib → serde 2.0.0");
    for node in 2..7 {
        assert!(graph.direct_nodes(node).is_empty(), "node {node} has no normal edges");
    }
    assert_eq!(graph.edges_from(0), 0..2);
    assert_eq!(graph.edges_from(1), 2..3);
    assert_eq!(graph.edges_from(6), 3..3);
    assert_eq!(graph.edge_between(0, 2), Some(1));
    assert_eq!(graph.edge_between(0, 3), None, "dev/build-only edges are not in the CSR");
    assert_eq!(graph.edge_source(2), 1);
    assert_eq!(graph.edge_target(2), 3);
}

#[test]
fn names_are_interned_once_and_nodes_project_onto_them() {
    let graph = fixture_graph();

    assert_eq!(graph.name_count(), 6);
    assert_eq!(graph.names(), ["app", "lib", "serde", "dev-helper", "build-helper", "isolated"]);
    assert_eq!(graph.node_to_name(), [0, 1, 2, 2, 3, 4, 5]);
    let serde = graph.lookup_name("serde").expect("serde is a name");
    assert_eq!(graph.nodes_of_name(serde), [2, 3]);
    assert_eq!(graph.name_str(serde), "serde");
    assert_eq!(graph.lookup_name("sd"), None, "the rename is never a name (§4.3)");
    assert_eq!(graph.direct(0).collect::<Vec<_>>(), [1, serde]);
    assert_eq!(graph.name(3), "serde");
    assert_eq!(graph.version(3), "2.0.0");
    assert_eq!(graph.manifest_path(0), "/ws/proj/app/Cargo.toml");
    assert_eq!(graph.package(6).name, "isolated");
}

#[test]
fn members_and_counters_come_from_the_interning_pass() {
    let graph = fixture_graph();

    assert_eq!(graph.members(), [0, 1]);
    assert!(graph.is_member(0) && graph.is_member(1));
    assert!(!graph.is_member(2));
    let counters = graph.counters();
    assert_eq!(counters.packages, 7);
    assert_eq!(counters.members, 2);
    assert_eq!(counters.normal_edges, 3);
    assert_eq!(counters.names, 6);
    assert_eq!(counters.superset_extra_edges, 0, "a traversal property, not a build one");
    assert_eq!(counters.unrebased_path_deps, 0);
    assert_eq!(graph.meta().packages.len(), 7);
}

#[test]
fn duplicate_member_ids_are_dropped_not_rejected() {
    let json = fixture_json().replacen(
        r#""workspace_members": ["path+file:///ws/proj/app#0.1.0","#,
        r#""workspace_members": ["path+file:///ws/proj/app#0.1.0", "path+file:///ws/proj/app#0.1.0","#,
        1,
    );

    let graph = Graph::build(meta(&json)).expect("builds");

    assert_eq!(graph.members(), [0, 1]);
}

#[test]
fn cfg_only_edges_are_flagged_and_their_kinds_decode_lazily() {
    let graph = fixture_graph();

    assert!(!graph.edge_is_cfg_only(0));
    assert!(!graph.edge_is_cfg_only(1));
    assert!(graph.edge_is_cfg_only(2), "every normal entry of lib → serde 2.0.0 has a target");
    let raw = graph.edge_dep_kinds(2);
    assert!(raw.get().contains("cfg(windows)"));
    assert_eq!(graph.edge_dep_kinds(0).get(), r#"[{"kind":null,"target":null}]"#);
    let kinds = graph.edge_kinds(2).expect("decodes");
    assert_eq!(
        kinds,
        [
            KindEntry { kind: Some("build".to_owned()), target: None },
            KindEntry { kind: None, target: Some("cfg(unix)".to_owned()) },
            KindEntry { kind: None, target: Some("cfg(windows)".to_owned()) },
        ]
    );
    assert!(!kinds[0].is_normal() && kinds[1].is_normal());
}

#[test]
fn declared_dependencies_decode_with_kind_optional_and_rename() {
    let graph = fixture_graph();

    let declared = graph.declared_deps(0).expect("decodes");

    assert_eq!(declared.len(), 4);
    assert_eq!(declared[1].name, "serde");
    assert_eq!(declared[1].rename.as_deref(), Some("sd"));
    assert!(declared[1].is_normal() && !declared[1].optional);
    assert_eq!(declared[2].kind.as_deref(), Some("dev"));
    assert!(!declared[2].is_normal());
    let lib = graph.declared_deps(1).expect("decodes");
    assert_eq!(lib[0].target.as_deref(), Some("cfg(unix)"));
}

#[test]
fn forward_reach_collects_nodes_and_names_and_witnesses_from_the_parent_chain() {
    let graph = fixture_graph();
    let mut scratch = Scratch::new(&graph);
    let serde = graph.lookup_name("serde").expect("serde");

    let reach = graph.reach(0, &mut scratch);

    assert_eq!(reach.root(), 0);
    assert_eq!(reach.direction(), Direction::Forward);
    assert_eq!(reach.nodes().ones().collect::<Vec<_>>(), [0, 1, 2, 3]);
    assert_eq!(reach.names().ones().collect::<Vec<_>>(), [0, 1, 2]);
    assert!(reach.contains_name(serde) && !reach.contains_name(3));
    assert!(!reach.contains_node(4) && !reach.contains_node(6));
    assert_eq!(reach.witness_to_node(3), Some(vec![0, 1, 3]));
    assert_eq!(reach.witness_to_node(2), Some(vec![0, 2]));
    assert_eq!(reach.witness_to_node(6), None);
    assert_eq!(reach.first_node_of_name(serde), Some(2), "serde 1.0.0 is reached first");
    assert_eq!(reach.witness_to_name(serde), Some(vec![0, 2]));
    assert_eq!(reach.witness_with_versions(serde), Some((vec![0, 2], vec![3])));
    assert_eq!(reach.witness_with_versions(5), None);
    assert_eq!(reach.reached_members().collect::<Vec<_>>(), [1]);
    assert_eq!(scratch.traversals(), 1);
}

#[test]
fn visited_is_cleared_per_root_so_stale_parents_are_never_read() {
    let graph = twelve_node_spec().graph();
    let mut scratch = Scratch::new(&graph);

    // Root a reaches f through e (depth 2) and h 1.0 through f (depth 3).
    let from_a = graph.reach(0, &mut scratch);
    assert_eq!(from_a.witness_to_node(5), Some(vec![0, 4, 5]));
    assert_eq!(from_a.witness_to_node(7), Some(vec![0, 4, 5, 7]));
    assert!(!from_a.contains_node(9) && !from_a.contains_node(10));
    let a_nodes: Vec<u32> = from_a.nodes().ones().map(narrow).collect();

    // Root r reaches f through s → t and h 1.0 directly through s: parents differ.
    let from_r = graph.reach(9, &mut scratch);
    assert_eq!(from_r.witness_to_node(5), Some(vec![9, 10, 11, 5]));
    assert_eq!(from_r.witness_to_node(7), Some(vec![9, 10, 7]));
    assert_eq!(from_r.witness_to_node(0), None, "a's stale parent slot is unreachable");
    assert!(!from_r.contains_node(1) && !from_r.contains_node(3));
    let r_nodes: Vec<u32> = from_r.nodes().ones().map(narrow).collect();

    assert_eq!(a_nodes, [0, 1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(r_nodes, [5, 6, 7, 8, 9, 10, 11]);
    assert_eq!(scratch.traversals(), 2);
}

#[test]
fn witnesses_are_shortest_paths_for_every_reached_node() {
    let spec = twelve_node_spec();
    let graph = spec.graph();
    let mut scratch = Scratch::new(&graph);

    for &root in graph.members() {
        let expected = distances(&spec, root as usize);
        let reach = graph.reach(root, &mut scratch);
        for node in 0..graph.node_count() {
            let witness = reach.witness_to_node(node);
            match expected[node as usize] {
                None => assert_eq!(witness, None, "root {root} must not reach {node}"),
                Some(distance) => {
                    let path = witness.expect("reached nodes have a witness");
                    assert_eq!(path.len(), distance + 1, "root {root} → {node}: {path:?}");
                    assert_eq!((path[0], *path.last().expect("non-empty")), (root, node));
                    for pair in path.windows(2) {
                        assert!(
                            graph.direct_nodes(pair[0]).contains(&pair[1]),
                            "{pair:?} is not an edge"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn witness_for_a_name_ends_at_the_first_reached_version() {
    let graph = twelve_node_spec().graph();
    let mut scratch = Scratch::new(&graph);
    let h = graph.lookup_name("h").expect("h");
    assert_eq!(graph.nodes_of_name(h), [7, 8]);

    // From a: h 2.0 (node 8) at depth 3 via e → g; h 1.0 (node 7) at depth 3 via e → f.
    // BFS order visits e's children f then g, so f's child h 1.0 is dequeued first.
    let from_a = graph.reach(0, &mut scratch);
    assert_eq!(from_a.first_node_of_name(h), Some(7));
    assert_eq!(
        names_of(&graph, &from_a.witness_to_name(h).expect("h reached")),
        ["a 1.0.0", "e 1.0.0", "f 1.0.0", "h 1.0.0"]
    );
    assert_eq!(from_a.witness_with_versions(h).map(|(_, others)| others), Some(vec![8]));

    // From r: h 2.0 via g at depth 2 and h 1.0 via s at depth 2; g is enqueued first.
    let from_r = graph.reach(9, &mut scratch);
    assert_eq!(from_r.first_node_of_name(h), Some(8));
    assert_eq!(from_r.witness_to_name(h), Some(vec![9, 6, 8]));
}

#[test]
fn two_versions_of_one_name_reached_by_different_paths_compare_equal_in_name_space() {
    let graph = twelve_node_spec().graph();
    let mut scratch = Scratch::new(&graph);
    let h = graph.lookup_name("h").expect("h");

    let from_a = graph.reach(0, &mut scratch);
    assert!(from_a.contains_node(7) && from_a.contains_node(8));
    let mut a_names = from_a.names().clone();
    let from_r = graph.reach(9, &mut scratch);
    assert!(from_r.contains_node(7) && from_r.contains_node(8));

    a_names.intersect_with(from_r.names());
    assert!(a_names.contains(h as usize));
    assert!(from_r.contains_name(h));
}

#[test]
fn reverse_reach_uses_a_lazily_built_transposed_csr_and_yields_forward_witnesses() {
    let graph = twelve_node_spec().graph();
    let mut scratch = Scratch::new(&graph);
    assert!(!graph.has_transposed());

    let to_f = graph.reverse_reach(5, &mut scratch);

    assert!(graph.has_transposed());
    assert_eq!(to_f.direction(), Direction::Reverse);
    assert_eq!(to_f.nodes().ones().collect::<Vec<_>>(), [0, 1, 2, 3, 4, 5, 9, 10, 11]);
    assert_eq!(to_f.reached_members().collect::<Vec<_>>(), [0, 9]);
    let from_a = to_f.witness_to_node(0).expect("a depends on f");
    assert_eq!((from_a[0], *from_a.last().expect("non-empty")), (0, 5));
    assert_eq!(from_a.len(), 3, "a → e → f is the shortest: {from_a:?}");
    assert_eq!(to_f.witness_to_node(9), Some(vec![9, 10, 11, 5]));
    assert_eq!(to_f.witness_to_node(7), None, "h 1.0 does not depend on f");
    for pair in from_a.windows(2) {
        assert!(graph.direct_nodes(pair[0]).contains(&pair[1]), "{pair:?} is not an edge");
    }

    // The versions witness begins at the name's first reached node, never at the root.
    let a = graph.lookup_name("a").expect("a");
    let f = graph.lookup_name("f").expect("f");
    let h = graph.lookup_name("h").expect("h");
    assert_eq!(to_f.first_node_of_name(a), Some(0));
    assert_eq!(to_f.witness_with_versions(a), Some((vec![0, 4, 5], Vec::new())));
    assert_eq!(to_f.witness_with_versions(f), Some((vec![5], Vec::new())), "the root's own name");
    assert_eq!(to_f.witness_with_versions(h), None, "no version of h depends on f");

    assert_eq!(graph.dependents(5), [3, 4, 11]);
    assert!(graph.dependents(0).is_empty());
    assert_eq!(graph.dependents(7), [5, 10]);
}

#[test]
fn superset_extra_edges_is_a_union_over_roots() {
    let graph = fixture_graph();
    let mut scratch = Scratch::new(&graph);
    assert_eq!(scratch.superset_extra_edges(), 0);

    graph.reach(1, &mut scratch);
    assert_eq!(scratch.superset_extra_edges(), 1, "lib → serde 2.0.0 is cfg-only");
    graph.reach(0, &mut scratch);
    assert_eq!(scratch.superset_extra_edges(), 1, "the same edge from a second root counts once");
    graph.reach(6, &mut scratch);
    assert_eq!(scratch.superset_extra_edges(), 1, "an isolated root adds nothing");
    graph.reverse_reach(3, &mut scratch);
    assert_eq!(scratch.superset_extra_edges(), 1, "a reverse walk over the same edge counts once");
}

#[test]
fn member_optional_declarations_mark_their_edges_as_superset_edges() {
    let spec = Spec {
        packages: vec![("m", "0.1.0"), ("x", "1.0.0"), ("y", "1.0.0"), ("z", "1.0.0")],
        edges: vec![(0, 1, NORMAL), (0, 2, NORMAL), (1, 3, CFG_ONLY), (2, 3, NORMAL)],
        members: vec![0],
        decls: vec![Decl::required(0, "x"), Decl::optional(0, "y")],
    };
    let graph = spec.graph();
    let mut scratch = Scratch::new(&graph);

    assert!(!graph.edge_is_member_optional(0), "m → x is a required declaration");
    assert!(graph.edge_is_member_optional(1), "m → y is declared optional");
    assert!(!graph.edge_declared_optional(0).expect("decodes"));
    assert!(graph.edge_declared_optional(1).expect("decodes"));
    assert!(graph.edge_is_cfg_only(2));
    graph.reach(0, &mut scratch);
    assert_eq!(scratch.superset_extra_edges(), 2, "one cfg-only edge plus one optional edge");
    assert_eq!(graph.counters().members, 1);
}

#[test]
fn a_name_declared_required_in_one_table_and_optional_in_another_is_not_optional() {
    // `x` is required under `[dependencies]` and optional under
    // `[target.'cfg(windows)'.dependencies]`: the edge is unconditionally present.
    let mixed = Spec {
        packages: vec![("m", "0.1.0"), ("x", "1.0.0"), ("y", "1.0.0")],
        edges: vec![(0, 1, NORMAL), (0, 2, NORMAL)],
        members: vec![0],
        decls: vec![
            Decl::required(0, "x"),
            Decl::optional(0, "x").on("cfg(windows)"),
            Decl::optional(0, "y"),
            Decl::optional(0, "y").on("cfg(unix)"),
        ],
    };
    let graph = mixed.graph();
    let mut scratch = Scratch::new(&graph);

    assert!(!graph.edge_is_member_optional(0), "one required declaration wins");
    assert!(!graph.edge_declared_optional(0).expect("decodes"));
    assert!(graph.edge_is_member_optional(1), "every declaration of y is optional");
    assert!(graph.edge_declared_optional(1).expect("decodes"));
    graph.reach(0, &mut scratch);
    assert_eq!(scratch.superset_extra_edges(), 1, "only m → y is a superset edge");

    // Flip the required declaration to optional: now both tables agree and x is flagged.
    let all_optional = Spec {
        decls: vec![Decl::optional(0, "x"), Decl::optional(0, "x").on("cfg(windows)")],
        ..mixed
    };
    let graph = all_optional.graph();
    assert!(graph.edge_is_member_optional(0));
    assert!(graph.edge_declared_optional(0).expect("decodes"));
    assert!(!graph.edge_is_member_optional(1), "y is no longer declared at all");
    assert!(!graph.edge_declared_optional(1).expect("decodes"));
}

#[test]
fn an_optional_dev_declaration_never_marks_a_normal_edge_optional() {
    // Only normal declarations take part in the fold (§1.5): an optional
    // `[dev-dependencies]` entry says nothing about the normal edge `m → x`, and a
    // lone optional dev entry must not flag it.
    let spec = Spec {
        packages: vec![("m", "0.1.0"), ("x", "1.0.0")],
        edges: vec![(0, 1, NORMAL)],
        members: vec![0],
        decls: vec![Decl::optional(0, "x").dev()],
    };
    let graph = spec.graph();
    let mut scratch = Scratch::new(&graph);

    assert!(!graph.edge_is_member_optional(0), "a dev declaration is not a normal one");
    assert!(!graph.edge_declared_optional(0).expect("decodes"));
    graph.reach(0, &mut scratch);
    assert_eq!(scratch.superset_extra_edges(), 0);

    // The same name declared optional under `[dependencies]` too: now it counts.
    let with_normal =
        Spec { decls: vec![Decl::optional(0, "x").dev(), Decl::optional(0, "x")], ..spec };
    let graph = with_normal.graph();
    assert!(graph.edge_is_member_optional(0));
    assert!(graph.edge_declared_optional(0).expect("decodes"));
}

#[test]
fn edge_declared_optional_answers_for_non_member_sources_and_agrees_with_the_member_bitset() {
    // m → x → y with the non-member x declaring y optional; the member bitset cannot
    // see that edge, the lazy per-edge query can.
    let spec = Spec {
        packages: vec![("m", "0.1.0"), ("x", "1.0.0"), ("y", "1.0.0"), ("z", "1.0.0")],
        edges: vec![(0, 1, NORMAL), (0, 3, NORMAL), (1, 2, NORMAL), (1, 3, NORMAL)],
        members: vec![0],
        decls: vec![
            Decl::required(0, "x"),
            Decl::optional(0, "z"),
            Decl::optional(1, "y"),
            Decl::required(1, "z"),
        ],
    };
    let graph = spec.graph();
    let x_to_y = graph.edge_between(1, 2).expect("x → y");
    let x_to_z = graph.edge_between(1, 3).expect("x → z");
    let m_to_z = graph.edge_between(0, 3).expect("m → z");

    assert!(!graph.edge_is_member_optional(x_to_y), "x is not a member");
    assert!(graph.edge_declared_optional(x_to_y).expect("decodes"));
    assert!(!graph.edge_declared_optional(x_to_z).expect("decodes"));
    for edge in 0..graph.edge_count() {
        if graph.is_member(graph.edge_source(edge)) {
            assert_eq!(
                graph.edge_is_member_optional(edge),
                graph.edge_declared_optional(edge).expect("decodes"),
                "edge {edge}: the bitset and the lazy query must agree on member edges"
            );
        }
    }
    assert!(graph.edge_is_member_optional(m_to_z));
}

#[test]
fn reverse_witness_with_versions_starts_at_the_first_reached_version_and_excludes_it() {
    // m → x 1.0 → y and m → x 2.0 → y: a reverse reach from y meets both versions of x.
    let spec = Spec {
        packages: vec![("m", "0.1.0"), ("x", "1.0.0"), ("x", "2.0.0"), ("y", "1.0.0")],
        edges: vec![(0, 1, NORMAL), (0, 2, NORMAL), (1, 3, NORMAL), (2, 3, NORMAL)],
        members: vec![0],
        decls: Vec::new(),
    };
    let graph = spec.graph();
    let mut scratch = Scratch::new(&graph);
    let x = graph.lookup_name("x").expect("x");
    let y = graph.lookup_name("y").expect("y");
    let m = graph.lookup_name("m").expect("m");

    let to_y = graph.reverse_reach(3, &mut scratch);

    assert_eq!(to_y.first_node_of_name(x), Some(1), "x 1.0 is dequeued first");
    assert_eq!(to_y.witness_with_versions(x), Some((vec![1, 3], vec![2])));
    assert_eq!(to_y.witness_with_versions(y), Some((vec![3], Vec::new())), "the root");
    assert_eq!(to_y.witness_with_versions(m), Some((vec![0, 1, 3], Vec::new())));
    for (path, others) in [x, y, m].into_iter().filter_map(|name| to_y.witness_with_versions(name))
    {
        let first = path[0];
        assert!(!others.contains(&first), "{others:?} must not repeat the witness node {first}");
        assert!(
            !others.contains(&3) || graph.name(3) == graph.name(first),
            "the root only appears as another version of its own name"
        );
    }
}

#[test]
fn scratch_is_resized_to_the_exact_graph_when_reused_for_a_larger_then_a_smaller_graph() {
    let small = fixture_graph();
    let large = twelve_node_spec().graph();
    let mut scratch = Scratch::new(&small);

    let reach = large.reach(0, &mut scratch);
    assert_eq!(reach.nodes().len(), large.node_count() as usize);
    assert_eq!(reach.names().len(), large.name_count() as usize);
    assert_eq!(reach.nodes().ones().count(), 9);
    assert_eq!(reach.witness_to_node(8), Some(vec![0, 4, 6, 8]));

    let reach = small.reach(1, &mut scratch);
    assert_eq!(reach.nodes().len(), small.node_count() as usize, "shrunk back to 7 bits");
    assert_eq!(reach.names().len(), small.name_count() as usize, "shrunk back to 6 bits");
    assert_eq!(reach.nodes().ones().collect::<Vec<_>>(), [1, 3]);
    assert_eq!(scratch.superset_extra_edges(), 1, "lib → serde 2.0.0 on the small graph");
    assert_eq!(scratch.traversals(), 2);
}

#[test]
fn reset_extra_fences_the_superset_union_between_passes() {
    let graph = fixture_graph();
    let mut scratch = Scratch::new(&graph);

    graph.reach(1, &mut scratch);
    assert_eq!(scratch.superset_extra_edges(), 1);

    scratch.reset_extra();
    assert_eq!(scratch.superset_extra_edges(), 0, "the union is cleared");
    graph.reach(6, &mut scratch);
    assert_eq!(scratch.superset_extra_edges(), 0, "an isolated root after the fence adds nothing");
    graph.reach(0, &mut scratch);
    assert_eq!(scratch.superset_extra_edges(), 1, "the counter restarts from the fence");
    assert_eq!(scratch.traversals(), 3, "the fence does not touch the traversal count");
}

#[test]
#[should_panic(expected = "root 99 is not a node")]
fn reach_panics_on_an_unknown_root() {
    let graph = fixture_graph();
    let mut scratch = Scratch::new(&graph);

    graph.reach(99, &mut scratch);
}
