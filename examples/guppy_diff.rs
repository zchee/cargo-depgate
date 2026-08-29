//! Differential spike (plan AC 12): our normal-edge closure versus guppy's.
//!
//! For every workspace member the example prints one Markdown row comparing the
//! set of package *names* reachable through normal edges:
//!
//! - `ours` — `cargo_depgate::graph::Graph::reach` over the v1-unified resolve,
//!   every platform, every feature-unified edge (plan §1.4).
//! - `guppy` — `PackageGraph::query_forward([member])` resolved through normal
//!   links that are present and enabled on the *host* platform.
//!
//! `extra = ours − guppy` is a superset by design (cfg-conditional edges that the
//! host filter drops); `missing = guppy − ours` must be empty everywhere — a
//! non-empty `missing` means our CSR lost an edge and the example exits 1.
//!
//! A second table repeats the comparison against guppy's *feature-aware*
//! package-rooted resolution (the member's default features only, no sibling
//! unification), which is the `cargo tree -p MEMBER -e normal` shape the plan's
//! gap table was derived from. There `extra` also contains optional dependencies
//! unified by sibling members.
//!
//! ```sh
//! RUSTFLAGS= cargo run --example guppy_diff -- [/path/to/metadata.json] [--time] [--names]
//! ```

use std::{
    collections::BTreeSet,
    env, fs,
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use cargo_depgate::{
    graph::{Graph, Scratch},
    metadata::{MetadataBuffer, parse},
};
use guppy::{
    PackageId,
    graph::{DependencyDirection, PackageGraph, PackageSet, feature::StandardFeatures},
    platform::{EnabledTernary, PlatformSpec},
};

const DEFAULT_METADATA: &str = "/tmp/ganja-metadata.json";

struct Options {
    metadata: PathBuf,
    time: bool,
    names: bool,
}

fn options() -> Options {
    let mut options =
        Options { metadata: PathBuf::from(DEFAULT_METADATA), time: false, names: false };
    for argument in env::args_os().skip(1) {
        match argument.to_str() {
            Some("--time") => options.time = true,
            Some("--names") => options.names = true,
            _ => options.metadata = PathBuf::from(argument),
        }
    }
    options
}

struct Row {
    member: String,
    ours: usize,
    theirs: usize,
    extra: Vec<String>,
    missing: Vec<String>,
}

fn print_table(title: &str, rows: &[Row], names: bool) {
    println!("### {title}\n");
    println!("| member | ours | guppy | extra (ours−guppy) | missing (guppy−ours) |");
    println!("|---|---:|---:|---:|---:|");
    for row in rows {
        println!(
            "| {} | {} | {} | +{} | {} |",
            row.member,
            row.ours,
            row.theirs,
            row.extra.len(),
            row.missing.len()
        );
    }
    println!();
    if names {
        for row in rows.iter().filter(|row| !row.extra.is_empty() || !row.missing.is_empty()) {
            println!("- `{}` extra: {}", row.member, row.extra.join(", "));
            if !row.missing.is_empty() {
                println!("- `{}` MISSING: {}", row.member, row.missing.join(", "));
            }
        }
        println!();
    }
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

fn main() -> Result<ExitCode> {
    let options = options();
    let bytes = fs::read(&options.metadata)
        .with_context(|| format!("reading {}", options.metadata.display()))?;
    let json = String::from_utf8(bytes.clone()).context("metadata is not UTF-8")?;

    let started = Instant::now();
    let buffer = MetadataBuffer::from_bytes(bytes);
    let meta = parse(&buffer)?;
    let parse_elapsed = started.elapsed();
    let started = Instant::now();
    let graph = Graph::build(&meta)?;
    let build_elapsed = started.elapsed();
    let counters = graph.counters();
    if options.time {
        eprintln!("parse\t{:.3} ms", millis(parse_elapsed));
        eprintln!("graph\t{:.3} ms", millis(build_elapsed));
    }

    let started = Instant::now();
    let package_graph = PackageGraph::from_json(&json).context("guppy could not load")?;
    let guppy_elapsed = started.elapsed();
    if options.time {
        eprintln!("guppy PackageGraph::from_json\t{:.3} ms", millis(guppy_elapsed));
    }
    let host = PlatformSpec::build_target().context("host platform")?;

    let mut scratch = Scratch::new(&graph);
    let mut package_rows = Vec::new();
    let mut feature_rows = Vec::new();
    let traversal_started = Instant::now();
    let mut bfs_elapsed = Duration::ZERO;
    for &member in graph.members() {
        let member_name = graph.name(member);
        let bfs_started = Instant::now();
        let reach = graph.reach(member, &mut scratch);
        bfs_elapsed += bfs_started.elapsed();
        let ours: BTreeSet<String> = reach
            .names()
            .ones()
            .filter_map(|name| u32::try_from(name).ok())
            .map(|name| graph.name_str(name))
            .filter(|&name| name != member_name)
            .map(str::to_owned)
            .collect();

        let id = PackageId::new(graph.package(member).id.to_string());
        let (package_rooted, feature_rooted) = guppy_closures(&package_graph, &host, &id)?;
        package_rows.push(row(member_name, &ours, &package_rooted));
        feature_rows.push(row(member_name, &ours, &feature_rooted));
    }
    if options.time {
        eprintln!("{} forward BFS (ours)\t{:.3} ms", graph.members().len(), millis(bfs_elapsed));
        eprintln!(
            "{} forward BFS + guppy queries\t{:.3} ms",
            graph.members().len(),
            millis(traversal_started.elapsed())
        );
    }

    println!(
        "counters: packages {} / members {} / normal_edges {} / names {} / superset_extra_edges {}\n",
        counters.packages,
        counters.members,
        counters.normal_edges,
        counters.names,
        scratch.superset_extra_edges()
    );
    print_table(
        "Package graph, normal links present and enabled on the host",
        &package_rows,
        options.names,
    );
    print_table(
        "Feature graph, member default features only (package-rooted, host)",
        &feature_rows,
        options.names,
    );

    let missing: usize =
        package_rows.iter().chain(&feature_rows).map(|row| row.missing.len()).sum();
    if missing == 0 {
        println!("missing = 0 on every row: our closure is a superset of guppy's.");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("{missing} name(s) reachable through guppy but not through our CSR.");
        Ok(ExitCode::FAILURE)
    }
}

/// guppy's two closures for `id`: the package-graph one (Table A) and the
/// feature-graph, default-features-only one (Table B); the root's own name excluded.
fn guppy_closures(
    package_graph: &PackageGraph,
    host: &PlatformSpec,
    id: &PackageId,
) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let root_name = package_graph.metadata(id)?.name();
    let names = |set: PackageSet<'_>| -> BTreeSet<String> {
        set.packages(DependencyDirection::Forward)
            .map(|package| package.name())
            .filter(|&name| name != root_name)
            .map(str::to_owned)
            .collect()
    };

    let package_rooted = package_graph.query_forward([id])?.resolve_with_fn(|_, link| {
        let normal = link.normal();
        normal.is_present() && normal.status().enabled_on(host) != EnabledTernary::Disabled
    });
    let feature_rooted = package_graph
        .resolve_ids([id])?
        .to_feature_set(StandardFeatures::Default)
        .to_feature_query(DependencyDirection::Forward)
        .resolve_with_fn(|_, link| link.normal().enabled_on(host) != EnabledTernary::Disabled)
        .to_package_set();
    Ok((names(package_rooted), names(feature_rooted)))
}

fn row(member: &str, ours: &BTreeSet<String>, theirs: &BTreeSet<String>) -> Row {
    Row {
        member: member.to_owned(),
        ours: ours.len(),
        theirs: theirs.len(),
        extra: ours.difference(theirs).cloned().collect(),
        missing: theirs.difference(ours).cloned().collect(),
    }
}
