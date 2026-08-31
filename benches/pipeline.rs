//! Divan benchmarks for the real Cargo metadata pipeline and a calibrated synthetic graph.

#![expect(
    clippy::cast_possible_truncation,
    reason = "benchmark sizes fit in the measured integer types"
)]
#![expect(clippy::cast_precision_loss, reason = "throughput calculations intentionally use f64")]
#![expect(clippy::expect_used, reason = "benchmark setup failures are unrecoverable")]

use std::{
    collections::BTreeSet,
    fmt::Write as _,
    fs::{self, File},
    io::{self, Read},
    path::Path,
    sync::{LazyLock, Mutex, OnceLock},
    time::{Duration, Instant},
};

use cargo_depgate::{
    cli::MetadataSource,
    config::{self, Config},
    graph::{Graph, Scratch},
    metadata::{self, Meta, MetadataBuffer, MetadataOptions},
    pipeline::{self, CheckArgs},
    report::{self, Format, RenderContext},
    rules,
};
use divan::{Bencher, black_box, counter::BytesCount};
use flate2::read::GzDecoder;

// The real-fixture benchmarks run on lemmy, the largest of the three committed examples
// (3,526,964 decompressed bytes against ckb's 3,367,042 and coreutils' 2,026,143), so the
// parse-rate and pipeline numbers describe the worst case the gate actually ships with.
const REAL_GZIP_PATH: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/lemmy-439734d/metadata.json.gz");
const REAL_FIXTURE_ROOT: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/lemmy-439734d");
const REAL_CONFIG_PATH: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/lemmy.depgate.toml");

/// The pinned shape of the lemmy fixture at `439734d`, asserted once so a fixture swap
/// cannot silently move the real-fixture numbers.
const REAL_JSON_BYTES: usize = 3_526_964;
const REAL_PACKAGES: u32 = 707;
const REAL_MEMBERS: usize = 41;

// The SYNTHETIC_* ratios below are deliberately fixed constants of the generator, not a
// description of any committed fixture. They were calibrated once against a 585-package /
// 1,586-edge / 529-name real workspace that this repository no longer ships, and they are kept
// verbatim so the scaling curve -- and the AC-P* bounds derived from it -- stay comparable
// across a fixture swap. They are not lemmy's statistics: lemmy resolves 707 packages onto 603
// names, 70 of them at two or more versions. Nothing reads them except the generator; the
// real-fixture benchmarks above measure the fixture itself.
const SYNTHETIC_PACKAGE_BYTES: usize = 4_146;
const SYNTHETIC_NORMAL_EDGES_PER_PACKAGE: usize = 5;
/// `targets[]` entries per synthetic package: 5.08 in the calibration workspace, rounded down.
const SYNTHETIC_TARGETS_PER_PACKAGE: usize = 5;
const SYNTHETIC_ROOTS_AT_MAX: usize = 24;
const SYNTHETIC_MAX_PACKAGES: usize = 20_000;
const SYNTHETIC_TARGET_NAMES_NUMERATOR: usize = 529;
const SYNTHETIC_TARGET_NAMES_DENOMINATOR: usize = 585;
const SYNTHETIC_MULTIVERSION_NUMERATOR: usize = 42;
const SYNTHETIC_MULTIVERSION_DENOMINATOR: usize = 529;
const SYNTHETIC_CFG_EDGES_NUMERATOR: usize = 229;
const SYNTHETIC_CFG_EDGES_DENOMINATOR: usize = 1_586;
const SYNTHETIC_NONNORMAL_EDGES_NUMERATOR: usize = 58;
const SYNTHETIC_NONNORMAL_EDGES_DENOMINATOR: usize = 1_586;
const SYNTHETIC_EDGE_BYTES_NUMERATOR: usize = 1_399;
const SYNTHETIC_EDGE_BYTES_DENOMINATOR: usize = 5;
const SYNTHETIC_WORKSPACE_ROOT: &str = "/fixture/cargo-depgate";

#[derive(Clone, Copy, Debug)]
enum BenchProfile {
    Dev,
    Ci,
}

impl BenchProfile {
    fn current() -> Self {
        matches!(std::env::var("DEPGATE_BENCH_PROFILE").as_deref(), Ok("ci"))
            .then_some(Self::Ci)
            .unwrap_or(Self::Dev)
    }

    const fn parse_gbps(self) -> f64 {
        match self {
            Self::Dev => 0.8,
            Self::Ci => 0.4,
        }
    }

    /// The floor the *real* fixture's parse must clear. Real `cargo metadata` documents
    /// are denser than the synthetic generator's output -- far more short strings per
    /// byte -- so the synthetic `parse_gbps` bound does not transfer unexamined.
    /// Measured at 1.188 GB/s on the lemmy fixture (aarch64-apple-darwin, release);
    /// the floor is set at half that, and the ci floor halves it again.
    const fn real_parse_gbps(self) -> f64 {
        match self {
            Self::Dev => 0.6,
            Self::Ci => 0.3,
        }
    }

    const fn own_work_ms(self) -> f64 {
        match self {
            Self::Dev => 20.0,
            Self::Ci => 60.0,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Ci => "ci",
        }
    }
}

static PROFILE: LazyLock<BenchProfile> = LazyLock::new(BenchProfile::current);

static REAL_BUFFER: LazyLock<MetadataBuffer> = LazyLock::new(|| {
    let file = File::open(REAL_GZIP_PATH).expect("open hermetic metadata fixture");
    let mut decoder = GzDecoder::new(file);
    let mut bytes = Vec::new();
    decoder.read_to_end(&mut bytes).expect("decompress hermetic metadata fixture");
    MetadataBuffer::from_bytes(bytes)
});

static REAL_META: LazyLock<Meta<'static>> =
    LazyLock::new(|| metadata::parse(&REAL_BUFFER).expect("parse hermetic metadata fixture"));

static REAL_GRAPH: LazyLock<Graph<'static>> =
    LazyLock::new(|| Graph::build(&REAL_META).expect("build hermetic metadata graph"));

static REAL_CONFIG: LazyLock<Config> = LazyLock::new(|| {
    let raw = config::load(Path::new(REAL_CONFIG_PATH)).expect("load hermetic depgate config");
    config::validate(&raw, Some(&REAL_GRAPH))
        .expect("validate hermetic depgate config against graph")
        .config
});

static REAL_METADATA_TEMP: LazyLock<tempfile::TempDir> = LazyLock::new(|| {
    let directory = tempfile::tempdir().expect("create temporary metadata directory");
    fs::write(directory.path().join("metadata.json"), REAL_BUFFER.as_bytes())
        .expect("write decompressed metadata for pipeline benchmark");
    directory
});

static SYNTHETIC_1K_BYTES: LazyLock<Vec<u8>> = LazyLock::new(|| generate_synthetic_json(1_000));
static SYNTHETIC_5K_BYTES: LazyLock<Vec<u8>> = LazyLock::new(|| generate_synthetic_json(5_000));
static SYNTHETIC_20K_BYTES: LazyLock<Vec<u8>> =
    LazyLock::new(|| generate_synthetic_json(SYNTHETIC_MAX_PACKAGES));

static SYNTHETIC_1K_META: LazyLock<Meta<'static>> =
    LazyLock::new(|| parse_synthetic(&SYNTHETIC_1K_BYTES).expect("parse 1k synthetic metadata"));
static SYNTHETIC_5K_META: LazyLock<Meta<'static>> =
    LazyLock::new(|| parse_synthetic(&SYNTHETIC_5K_BYTES).expect("parse 5k synthetic metadata"));
static SYNTHETIC_20K_META: LazyLock<Meta<'static>> =
    LazyLock::new(|| parse_synthetic(&SYNTHETIC_20K_BYTES).expect("parse 20k synthetic metadata"));

static SYNTHETIC_1K_GRAPH: LazyLock<Graph<'static>> =
    LazyLock::new(|| Graph::build(&SYNTHETIC_1K_META).expect("build 1k synthetic graph"));
static SYNTHETIC_5K_GRAPH: LazyLock<Graph<'static>> =
    LazyLock::new(|| Graph::build(&SYNTHETIC_5K_META).expect("build 5k synthetic graph"));
static SYNTHETIC_20K_GRAPH: LazyLock<Graph<'static>> =
    LazyLock::new(|| Graph::build(&SYNTHETIC_20K_META).expect("build 20k synthetic graph"));

static SYNTHETIC_1K_CONFIG: LazyLock<Config> =
    LazyLock::new(|| synthetic_config(&SYNTHETIC_1K_GRAPH, synthetic_root_count(1_000)));
static SYNTHETIC_5K_CONFIG: LazyLock<Config> =
    LazyLock::new(|| synthetic_config(&SYNTHETIC_5K_GRAPH, synthetic_root_count(5_000)));
static SYNTHETIC_20K_CONFIG: LazyLock<Config> =
    LazyLock::new(|| synthetic_config(&SYNTHETIC_20K_GRAPH, SYNTHETIC_ROOTS_AT_MAX));

static SYNTHETIC_1K_PIPELINE_TEMP: LazyLock<tempfile::TempDir> = LazyLock::new(|| {
    synthetic_pipeline_temp(&SYNTHETIC_1K_BYTES, &SYNTHETIC_1K_GRAPH, synthetic_root_count(1_000))
});
static SYNTHETIC_5K_PIPELINE_TEMP: LazyLock<tempfile::TempDir> = LazyLock::new(|| {
    synthetic_pipeline_temp(&SYNTHETIC_5K_BYTES, &SYNTHETIC_5K_GRAPH, synthetic_root_count(5_000))
});
static SYNTHETIC_20K_PIPELINE_TEMP: LazyLock<tempfile::TempDir> = LazyLock::new(|| {
    synthetic_pipeline_temp(&SYNTHETIC_20K_BYTES, &SYNTHETIC_20K_GRAPH, SYNTHETIC_ROOTS_AT_MAX)
});

static SYNTHETIC_1K_REPORT_OUTCOME: LazyLock<pipeline::Outcome> = LazyLock::new(|| {
    synthetic_report_outcome(&SYNTHETIC_1K_PIPELINE_TEMP, synthetic_root_count(1_000))
});
static SYNTHETIC_5K_REPORT_OUTCOME: LazyLock<pipeline::Outcome> = LazyLock::new(|| {
    synthetic_report_outcome(&SYNTHETIC_5K_PIPELINE_TEMP, synthetic_root_count(5_000))
});
static SYNTHETIC_20K_REPORT_OUTCOME: LazyLock<pipeline::Outcome> = LazyLock::new(|| {
    synthetic_report_outcome(&SYNTHETIC_20K_PIPELINE_TEMP, SYNTHETIC_ROOTS_AT_MAX)
});

static SYNTHETIC_1K_RENDER_CONTEXT: LazyLock<RenderContext> =
    LazyLock::new(|| synthetic_render_context(&SYNTHETIC_1K_PIPELINE_TEMP));
static SYNTHETIC_5K_RENDER_CONTEXT: LazyLock<RenderContext> =
    LazyLock::new(|| synthetic_render_context(&SYNTHETIC_5K_PIPELINE_TEMP));
static SYNTHETIC_20K_RENDER_CONTEXT: LazyLock<RenderContext> =
    LazyLock::new(|| synthetic_render_context(&SYNTHETIC_20K_PIPELINE_TEMP));

static REAL_VALIDATED: OnceLock<()> = OnceLock::new();
static REAL_PARSE_RATE_PRINTED: OnceLock<()> = OnceLock::new();
static SYNTHETIC_20K_VALIDATED: OnceLock<()> = OnceLock::new();
static PARSE_RATES: LazyLock<Mutex<[Option<f64>; 3]>> =
    LazyLock::new(|| Mutex::new([None, None, None]));
static PARSE_RATES_PRINTED: OnceLock<()> = OnceLock::new();
static OWN_WORK_PRINTED: OnceLock<()> = OnceLock::new();

#[divan::bench(sample_count = 5, sample_size = 1)]
fn real_parse(bencher: Bencher) {
    ensure_real_fixture_validated();
    let bytes = REAL_BUFFER.as_bytes();
    let mut samples = Vec::with_capacity(5);
    bencher.counter(BytesCount::new(bytes.len())).bench_local(|| {
        let started = Instant::now();
        black_box(serde_json::from_slice::<Meta<'static>>(bytes).expect("parse hermetic metadata"));
        samples.push(started.elapsed());
    });
    let elapsed = median_duration(&mut samples);
    let rate = bytes.len() as f64 / elapsed.as_secs_f64().max(f64::MIN_POSITIVE) / 1e9;
    let floor = PROFILE.real_parse_gbps();
    if REAL_PARSE_RATE_PRINTED.set(()).is_ok() {
        eprintln!(
            "achieved real-fixture parse GB/s ({} profile): {rate:.3} (floor {floor:.3})",
            PROFILE.label()
        );
    }
    assert!(
        rate >= floor,
        "real-fixture parse rate {rate:.3} GB/s below the {} floor {floor:.3} GB/s",
        PROFILE.label()
    );
}

#[divan::bench(sample_count = 5, sample_size = 1)]
fn real_graph(bencher: Bencher) {
    let meta = &*REAL_META;
    bencher.bench(|| Graph::build(meta).expect("build hermetic graph"));
}

#[divan::bench(sample_count = 5, sample_size = 1)]
fn real_rules(bencher: Bencher) {
    let graph = &*REAL_GRAPH;
    let config = &*REAL_CONFIG;
    let mut scratch = Scratch::new(graph);
    bencher.bench_local(|| {
        black_box(rules::evaluate(graph, config, &mut scratch));
    });
}

#[divan::bench(sample_count = 5, sample_size = 1)]
fn real_pipeline(bencher: Bencher) {
    let args = real_check_args();
    bencher.bench_local(|| {
        let mut stderr = io::sink();
        black_box(pipeline::check(&args, &mut stderr).expect("run hermetic pipeline"));
    });
}

fn real_check_args() -> CheckArgs {
    let metadata = MetadataOptions::default()
        .with_source(MetadataSource::File(REAL_METADATA_TEMP.path().join("metadata.json")))
        .with_workspace_root(REAL_FIXTURE_ROOT);
    CheckArgs::new(metadata).with_config_path(REAL_CONFIG_PATH)
}

fn synthetic_check_args(temp: &tempfile::TempDir) -> CheckArgs {
    let metadata = MetadataOptions::default()
        .with_source(MetadataSource::File(temp.path().join("metadata.json")))
        .with_workspace_root(temp.path());
    CheckArgs::new(metadata).with_config_path(temp.path().join("depgate.toml"))
}

#[divan::bench(sample_count = 5, sample_size = 1)]
fn synthetic_parse_1k(bencher: Bencher) {
    synthetic_parse_bench(bencher, &SYNTHETIC_1K_BYTES, 0);
}

#[divan::bench(sample_count = 5, sample_size = 1)]
fn synthetic_parse_5k(bencher: Bencher) {
    synthetic_parse_bench(bencher, &SYNTHETIC_5K_BYTES, 1);
}

#[divan::bench(sample_count = 5, sample_size = 1)]
fn synthetic_parse_20k(bencher: Bencher) {
    ensure_synthetic_20k_validated();
    let elapsed = synthetic_parse_bench(bencher, &SYNTHETIC_20K_BYTES, 2);
    let profile = *PROFILE;
    let ceiling_ms = parse_ceiling_ms(SYNTHETIC_20K_BYTES.len(), profile);
    let measured_ms = elapsed.as_secs_f64() * 1_000.0;
    assert!(
        measured_ms <= ceiling_ms,
        "synthetic parse measured {measured_ms:.3} ms, threshold {ceiling_ms:.3} ms (profile={})",
        profile.label()
    );
}

#[divan::bench(sample_count = 5, sample_size = 1)]
fn synthetic_graph_rules_1k(bencher: Bencher) {
    synthetic_graph_rules_bench(
        bencher,
        &SYNTHETIC_1K_META,
        &SYNTHETIC_1K_CONFIG,
        &SYNTHETIC_1K_REPORT_OUTCOME,
        &SYNTHETIC_1K_RENDER_CONTEXT,
        synthetic_root_count(1_000),
        false,
    );
}

#[divan::bench(sample_count = 5, sample_size = 1)]
fn synthetic_graph_rules_5k(bencher: Bencher) {
    synthetic_graph_rules_bench(
        bencher,
        &SYNTHETIC_5K_META,
        &SYNTHETIC_5K_CONFIG,
        &SYNTHETIC_5K_REPORT_OUTCOME,
        &SYNTHETIC_5K_RENDER_CONTEXT,
        synthetic_root_count(5_000),
        false,
    );
}

#[divan::bench(sample_count = 5, sample_size = 1)]
fn synthetic_graph_rules_20k(bencher: Bencher) {
    ensure_synthetic_20k_validated();
    synthetic_graph_rules_bench(
        bencher,
        &SYNTHETIC_20K_META,
        &SYNTHETIC_20K_CONFIG,
        &SYNTHETIC_20K_REPORT_OUTCOME,
        &SYNTHETIC_20K_RENDER_CONTEXT,
        SYNTHETIC_ROOTS_AT_MAX,
        true,
    );
}

fn synthetic_parse_bench(bencher: Bencher, bytes: &'static [u8], index: usize) -> Duration {
    let mut samples = Vec::with_capacity(5);
    bencher.counter(BytesCount::new(bytes.len())).bench_local(|| {
        let started = Instant::now();
        black_box(
            serde_json::from_slice::<Meta<'static>>(bytes).expect("parse synthetic metadata"),
        );
        samples.push(started.elapsed());
    });
    let elapsed = median_duration(&mut samples);
    let rate = bytes.len() as f64 / elapsed.as_secs_f64().max(f64::MIN_POSITIVE) / 1e9;
    record_parse_rate(index, rate);
    elapsed
}

fn synthetic_graph_rules_bench(
    bencher: Bencher,
    meta: &'static Meta<'static>,
    config: &'static Config,
    outcome: &'static pipeline::Outcome,
    context: &'static RenderContext,
    roots: usize,
    enforce: bool,
) {
    let mut samples = Vec::with_capacity(5);
    bencher.bench_local(|| {
        let started = Instant::now();
        let graph = Graph::build(meta).expect("build synthetic graph");
        let mut scratch = Scratch::new(&graph);
        let evaluation = rules::evaluate(&graph, config, &mut scratch);
        let mut report_output = io::sink();
        report::render(Format::Human, outcome, context, &mut report_output)
            .expect("render synthetic report");
        black_box(evaluation);
        samples.push(started.elapsed());
    });
    let elapsed = median_duration(&mut samples);
    if enforce {
        let profile = *PROFILE;
        let measured_ms = elapsed.as_secs_f64() * 1_000.0;
        let threshold_ms = profile.own_work_ms();
        assert!(
            measured_ms <= threshold_ms,
            "synthetic non-parse own-work measured {measured_ms:.3} ms, threshold {threshold_ms:.3} ms (profile={})",
            profile.label()
        );
        if OWN_WORK_PRINTED.set(()).is_ok() {
            eprintln!(
                "synthetic non-parse own-work at 20k: {measured_ms:.3} ms (profile={}, threshold={threshold_ms:.3} ms, roots={roots})",
                profile.label()
            );
        }
    }
}

/// Median of `samples`, or [`Duration::MAX`] when there are none.
///
/// The enforced AC-P6a/AC-P6b numbers come from these hand-rolled
/// [`Instant::now`] samples taken inside the divan closure rather than from
/// divan's own statistics: divan reports its timings to the terminal but
/// exposes no programmatic result API, so a gate cannot read them. The empty
/// case returns [`Duration::MAX`] so a benchmark that never ran fails the
/// threshold assertion instead of passing on a zero.
fn median_duration(samples: &mut [Duration]) -> Duration {
    samples.sort_unstable();
    samples.get(samples.len() / 2).copied().unwrap_or(Duration::MAX)
}

fn synthetic_pipeline_temp(bytes: &[u8], graph: &Graph<'_>, roots: usize) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("create synthetic pipeline directory");
    fs::write(directory.path().join("metadata.json"), bytes)
        .expect("write synthetic metadata for report benchmark");
    fs::write(directory.path().join("depgate.toml"), synthetic_config_text(graph, roots))
        .expect("write synthetic config for report benchmark");
    directory
}

fn synthetic_report_outcome(temp: &tempfile::TempDir, roots: usize) -> pipeline::Outcome {
    let mut stderr = io::sink();
    let outcome = pipeline::check(&synthetic_check_args(temp), &mut stderr)
        .expect("run synthetic pipeline for report benchmark");
    assert_eq!(outcome.statuses.len(), roots);
    assert!(outcome.manifest.is_none());
    outcome
}

fn synthetic_render_context(temp: &tempfile::TempDir) -> RenderContext {
    RenderContext::new(temp.path().to_path_buf(), "cargo-depgate", env!("CARGO_PKG_VERSION"), false)
}

fn record_parse_rate(index: usize, rate: f64) {
    let mut rates = PARSE_RATES.lock().expect("lock parse-rate report");
    rates[index] = Some(rate);
    let [Some(one_k), Some(five_k), Some(twenty_k)] = *rates else {
        return;
    };
    drop(rates);
    if PARSE_RATES_PRINTED.set(()).is_ok() {
        eprintln!("achieved parse GB/s at 1k, 5k, 20k: {one_k:.3}, {five_k:.3}, {twenty_k:.3}");
    }
}

fn ensure_real_fixture_validated() {
    REAL_VALIDATED.get_or_init(|| {
        let bytes = REAL_BUFFER.as_bytes();
        let graph = &*REAL_GRAPH;
        assert_eq!(bytes.len(), REAL_JSON_BYTES, "lemmy fixture JSON size drifted");
        assert_eq!(graph.node_count(), REAL_PACKAGES, "lemmy fixture package count drifted");
        assert_eq!(graph.members().len(), REAL_MEMBERS, "lemmy fixture member count drifted");
        eprintln!(
            "real fixture shape: bytes={}, packages={}, members={}, names={}",
            bytes.len(),
            graph.node_count(),
            graph.members().len(),
            graph.name_count()
        );
    });
}

fn ensure_synthetic_20k_validated() {
    SYNTHETIC_20K_VALIDATED.get_or_init(|| {
        let bytes = &*SYNTHETIC_20K_BYTES;
        let graph = &*SYNTHETIC_20K_GRAPH;
        let config = &*SYNTHETIC_20K_CONFIG;
        assert_eq!(graph.node_count(), SYNTHETIC_MAX_PACKAGES as u32);
        assert_eq!(graph.edge_count(), 100_000);
        assert!(
            (100_700_000..=123_100_000).contains(&bytes.len()),
            "synthetic JSON bytes {} outside 100.7–123.1 MB",
            bytes.len()
        );
        let roots = config.rules.iter().map(|rule| rule.package.as_str()).collect::<BTreeSet<_>>();
        assert_eq!(roots.len(), SYNTHETIC_ROOTS_AT_MAX);
        assert_eq!(graph.members().len(), SYNTHETIC_ROOTS_AT_MAX);
        let target_names = SYNTHETIC_MAX_PACKAGES as f64 * SYNTHETIC_TARGET_NAMES_NUMERATOR as f64
            / SYNTHETIC_TARGET_NAMES_DENOMINATOR as f64;
        let observed_names = f64::from(graph.name_count());
        assert!(
            (observed_names - target_names).abs() <= target_names * 0.01,
            "synthetic distinct names {observed_names:.0} outside ±1% of {target_names:.0}"
        );
        let mut scratch = Scratch::new(graph);
        let evaluation = rules::evaluate(graph, config, &mut scratch);
        assert_eq!(evaluation.statuses.len(), SYNTHETIC_ROOTS_AT_MAX);
        assert_eq!(scratch.traversals(), SYNTHETIC_ROOTS_AT_MAX as u32);
        eprintln!(
            "synthetic 20k shape: bytes={}, packages={}, normal_edges={}, roots={}, names={}",
            bytes.len(),
            graph.node_count(),
            graph.edge_count(),
            roots.len(),
            graph.name_count()
        );
    });
}

fn parse_ceiling_ms(bytes: usize, profile: BenchProfile) -> f64 {
    bytes as f64 / (profile.parse_gbps() * 1_000_000.0)
}

fn parse_synthetic(bytes: &'static [u8]) -> Result<Meta<'static>, serde_json::Error> {
    serde_json::from_slice(bytes)
}

fn synthetic_root_count(package_count: usize) -> usize {
    (package_count * SYNTHETIC_ROOTS_AT_MAX / SYNTHETIC_MAX_PACKAGES).max(1)
}

fn synthetic_name_count(package_count: usize) -> usize {
    (package_count * SYNTHETIC_TARGET_NAMES_NUMERATOR + SYNTHETIC_TARGET_NAMES_DENOMINATOR / 2)
        / SYNTHETIC_TARGET_NAMES_DENOMINATOR
}

#[derive(Clone, Copy)]
enum EdgeKind {
    Normal,
    Dev,
    Build,
}

#[derive(Clone, Copy)]
struct EdgeSpec {
    target: usize,
    kind: EdgeKind,
    cfg: bool,
}

struct PackageIdentity {
    name: String,
    version: String,
    id: String,
    directory: String,
    manifest_path: String,
}

fn generate_synthetic_json(package_count: usize) -> Vec<u8> {
    assert!(package_count > 0);
    let distinct_names = synthetic_name_count(package_count);
    let multi_version_names = (distinct_names * SYNTHETIC_MULTIVERSION_NUMERATOR
        + SYNTHETIC_MULTIVERSION_DENOMINATOR / 2)
        / SYNTHETIC_MULTIVERSION_DENOMINATOR;
    let identities = package_identities(package_count, distinct_names, multi_version_names);
    let normal_edges = package_count * SYNTHETIC_NORMAL_EDGES_PER_PACKAGE;
    let cfg_edges = (normal_edges * SYNTHETIC_CFG_EDGES_NUMERATOR
        + SYNTHETIC_CFG_EDGES_DENOMINATOR / 2)
        / SYNTHETIC_CFG_EDGES_DENOMINATOR;
    let extra_edges = (normal_edges * SYNTHETIC_NONNORMAL_EDGES_NUMERATOR
        + SYNTHETIC_NONNORMAL_EDGES_DENOMINATOR / 2)
        / SYNTHETIC_NONNORMAL_EDGES_DENOMINATOR;
    assert!(extra_edges <= package_count);
    let edge_count = normal_edges + extra_edges;
    let node_prefixes = identities
        .iter()
        .enumerate()
        .map(|(source, identity)| {
            node_prefix(identity, source, &identities, cfg_edges, extra_edges)
        })
        .collect::<Vec<_>>();
    let node_fixed_bytes = node_prefixes.iter().map(|prefix| prefix.len() + 2).sum::<usize>()
        + package_count.saturating_sub(1);
    let edge_separators = edge_count - package_count;
    let resolve_fixed_bytes = "{\"nodes\":[".len() + 2 + node_fixed_bytes + edge_separators;
    let target_resolve_bytes = (edge_count * SYNTHETIC_EDGE_BYTES_NUMERATOR
        + SYNTHETIC_EDGE_BYTES_DENOMINATOR / 2)
        / SYNTHETIC_EDGE_BYTES_DENOMINATOR;
    assert!(
        target_resolve_bytes >= resolve_fixed_bytes,
        "synthetic resolve template exceeds target: {resolve_fixed_bytes}"
    );
    let edge_bytes_total = target_resolve_bytes - resolve_fixed_bytes;
    let edge_bytes_floor = edge_bytes_total / edge_count;
    let edge_bytes_remainder = edge_bytes_total % edge_count;
    let mut output = String::with_capacity(
        package_count * SYNTHETIC_PACKAGE_BYTES + target_resolve_bytes + package_count * 150,
    );
    output.push_str("{\"packages\":[");
    let mut package_bytes_total = 0usize;
    for (source, identity) in identities.iter().enumerate() {
        if source != 0 {
            output.push(',');
        }
        let package_target = SYNTHETIC_PACKAGE_BYTES - usize::from(source != 0);
        let package =
            package_json(identity, source, &identities, cfg_edges, extra_edges, package_target);
        package_bytes_total += package.len();
        output.push_str(&package);
    }
    assert_eq!(
        package_bytes_total + package_count.saturating_sub(1),
        package_count * SYNTHETIC_PACKAGE_BYTES,
        "synthetic packages span must average {SYNTHETIC_PACKAGE_BYTES} bytes per package"
    );
    output.push_str("],\"workspace_members\":[");
    let root_count = synthetic_root_count(package_count);
    for (index, identity) in identities.iter().take(root_count).enumerate() {
        if index != 0 {
            output.push(',');
        }
        write_json_string(&mut output, &identity.id);
    }
    output.push_str("],\"workspace_root\":\"");
    output.push_str(SYNTHETIC_WORKSPACE_ROOT);
    output.push_str("\",\"resolve\":");
    let resolve_start = output.len();
    output.push_str("{\"nodes\":[");
    let mut emitted_edges = 0usize;
    for (source, node_prefix) in node_prefixes.iter().enumerate() {
        if source != 0 {
            output.push(',');
        }
        output.push_str(node_prefix);
        let source_edge_count =
            SYNTHETIC_NORMAL_EDGES_PER_PACKAGE + usize::from(source < extra_edges);
        for ordinal in 0..source_edge_count {
            if ordinal != 0 {
                output.push(',');
            }
            let spec = edge_spec(source, ordinal, package_count, cfg_edges, extra_edges);
            let target_len = edge_bytes_floor + usize::from(emitted_edges < edge_bytes_remainder);
            output.push_str(&edge_json(&identities[spec.target], spec, target_len));
            emitted_edges += 1;
        }
        output.push_str("]}");
    }
    assert_eq!(emitted_edges, edge_count);
    output.push_str("]}");
    let resolve_len = output.len() - resolve_start;
    assert_eq!(resolve_len, target_resolve_bytes);
    output.push_str(",\"version\":1}");
    output.into_bytes()
}

fn node_prefix(
    identity: &PackageIdentity,
    source: usize,
    identities: &[PackageIdentity],
    cfg_edges: usize,
    extra_edges: usize,
) -> String {
    let edge_count = SYNTHETIC_NORMAL_EDGES_PER_PACKAGE + usize::from(source < extra_edges);
    let mut node = String::new();
    write!(&mut node, "{{\"id\":").expect("write synthetic node id prefix");
    write_json_string(&mut node, &identity.id);
    node.push_str(",\"dependencies\":[");
    for ordinal in 0..edge_count {
        if ordinal != 0 {
            node.push(',');
        }
        let spec = edge_spec(source, ordinal, identities.len(), cfg_edges, extra_edges);
        write_json_string(&mut node, &identities[spec.target].id);
    }
    node.push_str("],\"features\":[],\"deps\":[");
    node
}

fn package_identities(
    package_count: usize,
    distinct_names: usize,
    multi_version_names: usize,
) -> Vec<PackageIdentity> {
    let mut seen_versions = vec![0usize; distinct_names];
    (0..package_count)
        .map(|index| {
            let name_index = if index < distinct_names {
                index
            } else {
                let extra_index = index - distinct_names;
                if extra_index < multi_version_names {
                    extra_index
                } else {
                    extra_index - multi_version_names
                }
            };
            let version_number = seen_versions[name_index] + 1;
            seen_versions[name_index] = version_number;
            let name = format!("pkg-{name_index:05}");
            let version = format!("{version_number}.0.0");
            let directory = format!("{SYNTHETIC_WORKSPACE_ROOT}/{name}");
            let manifest_path = format!("{directory}/Cargo.toml");
            let id = format!("path+file://{directory}#{version}");
            PackageIdentity { name, version, id, directory, manifest_path }
        })
        .collect()
}

fn package_json(
    identity: &PackageIdentity,
    source: usize,
    identities: &[PackageIdentity],
    cfg_edges: usize,
    extra_edges: usize,
    target_bytes: usize,
) -> String {
    let mut dependencies = String::from("[");
    let edge_count = SYNTHETIC_NORMAL_EDGES_PER_PACKAGE + usize::from(source < extra_edges);
    for ordinal in 0..edge_count {
        if ordinal != 0 {
            dependencies.push(',');
        }
        let spec = edge_spec(source, ordinal, identities.len(), cfg_edges, extra_edges);
        dependencies.push_str(&dependency_json(&identities[spec.target], spec));
    }
    dependencies.push(']');
    // Spend the byte budget on realistic repeated structure rather than one long
    // unescaped run. serde_json skips a 2 kB run inside `description` with a
    // memchr-style scan, roughly an order of magnitude cheaper per byte than the
    // small-token soup real metadata is made of, which would make AC-P6a easier
    // than the bound derived from the real fixture. Measured on the committed
    // fixture: 231.5 string/key tokens per package, mean token 13.0 B, longest
    // single run 109.6 B (4.5 % of the package).
    let mut targets = String::new();
    for ordinal in 0..SYNTHETIC_TARGETS_PER_PACKAGE {
        if ordinal != 0 {
            targets.push(',');
        }
        targets.push_str(&target_json(identity, ordinal));
    }
    let prefix = format!(
        "{{\"name\":\"{}\",\"version\":\"{}\",\"id\":\"{}\",\"license\":\"Apache-2.0\",\"license_file\":null,\"description\":\"{} is a synthetic workspace crate. ",
        identity.name, identity.version, identity.id, identity.name
    );
    let body = format!(
        "\",\"source\":null,\"dependencies\":{dependencies},\"targets\":[{targets}],\"features\":{{"
    );
    let suffix = format!(
        "}},\"manifest_path\":\"{}\",\"metadata\":{{}},\"publish\":null,\"authors\":[\"Synthetic Author <author@example.invalid>\"],\"categories\":[\"development-tools\",\"development-tools::cargo-plugins\"],\"keywords\":[\"cargo\",\"dependency\",\"graph\"],\"readme\":\"README.md\",\"repository\":\"https://example.invalid/{}\",\"homepage\":null,\"documentation\":null,\"edition\":\"2024\",\"links\":null,\"default_run\":null,\"rust_version\":\"1.85\"}}",
        identity.manifest_path, identity.name
    );
    let base_len = prefix.len() + body.len() + suffix.len();
    assert!(base_len <= target_bytes, "synthetic package template exceeds target: {base_len}");
    // Fill the remaining budget with `features` entries, then absorb the sub-entry
    // remainder in the description so the exact per-package byte count still holds.
    let mut features = String::new();
    let mut ordinal = 0usize;
    loop {
        let entry = feature_json(ordinal);
        let separator = usize::from(ordinal != 0);
        if base_len + features.len() + separator + entry.len() > target_bytes {
            break;
        }
        if separator != 0 {
            features.push(',');
        }
        features.push_str(&entry);
        ordinal += 1;
    }
    let filler = target_bytes - base_len - features.len();
    let mut package = String::with_capacity(target_bytes);
    package.push_str(&prefix);
    package.extend(std::iter::repeat_n('a', filler));
    package.push_str(&body);
    package.push_str(&features);
    package.push_str(&suffix);
    assert_eq!(package.len(), target_bytes);
    package
}

/// One `targets[]` entry; ordinal 0 is the crate's own `lib` target.
fn target_json(identity: &PackageIdentity, ordinal: usize) -> String {
    const SHAPES: [(&str, &str, &str); SYNTHETIC_TARGETS_PER_PACKAGE] = [
        ("lib", "lib", "src/lib.rs"),
        ("bin", "bin", "src/main.rs"),
        ("test", "test", "tests/integration.rs"),
        ("bench", "bench", "benches/throughput.rs"),
        ("example", "example", "examples/basic.rs"),
    ];
    let (kind, crate_type, source_path) = SHAPES[ordinal % SHAPES.len()];
    let name =
        if ordinal == 0 { identity.name.clone() } else { format!("{}-{kind}", identity.name) };
    format!(
        "{{\"kind\":[\"{kind}\"],\"crate_types\":[\"{crate_type}\"],\"name\":\"{name}\",\"src_path\":\"{}/{source_path}\",\"edition\":\"2024\",\"doc\":true,\"doctest\":false,\"test\":true,\"required-features\":[]}}",
        identity.directory
    )
}

/// One `features` map entry: a short key and a two-element activation list.
fn feature_json(ordinal: usize) -> String {
    format!("\"feature-{ordinal:03}\":[\"std\",\"pkg-{:05}/alloc\"]", ordinal * 7 % 100_000)
}

fn dependency_json(identity: &PackageIdentity, spec: EdgeSpec) -> String {
    let kind = match spec.kind {
        EdgeKind::Normal => "null",
        EdgeKind::Dev => "\"dev\"",
        EdgeKind::Build => "\"build\"",
    };
    let target = if spec.cfg { "\"cfg(unix)\"" } else { "null" };
    format!(
        "{{\"name\":\"{}\",\"source\":null,\"req\":\"*\",\"kind\":{kind},\"rename\":null,\"optional\":false,\"uses_default_features\":true,\"features\":[],\"target\":{target},\"registry\":null,\"path\":\"{}\"}}",
        identity.name, identity.directory
    )
}

/// One `resolve.nodes[].deps[]` entry, padded to `target_len`.
///
/// Unlike a package, an edge's padding stays one string run on purpose: real
/// resolve edges are string-dominated. Measured on the committed fixture, the
/// average edge is 152.6 B of which 89.6 B (58.7 %) sits inside string literals
/// and the longest single run averages 72.1 B (47 %) — mostly the registry
/// `pkg` id. The synthetic edge's run is a comparable share of its bytes, so
/// spreading it would make the edge *less* like the fixture, not more.
fn edge_json(identity: &PackageIdentity, spec: EdgeSpec, target_len: usize) -> String {
    let kind = match spec.kind {
        EdgeKind::Normal => "null",
        EdgeKind::Dev => "\"dev\"",
        EdgeKind::Build => "\"build\"",
    };
    let target = if spec.cfg { "\"cfg(unix)\"" } else { "null" };
    let mut edge = String::with_capacity(target_len);
    write!(
        &mut edge,
        "{{\"name\":\"{}\",\"pkg\":\"{}\",\"dep_kinds\":[{{\"kind\":{kind},\"target\":{target}}}],\"metadata\":\"",
        identity.name, identity.id
    )
    .expect("write synthetic edge prefix");
    assert!(
        edge.len() + 2 <= target_len,
        "synthetic edge template exceeds target: {}",
        edge.len() + 2
    );
    edge.extend(std::iter::repeat_n('b', target_len - edge.len() - 2));
    edge.push_str("\"}");
    assert_eq!(edge.len(), target_len);
    edge
}

fn edge_spec(
    source: usize,
    ordinal: usize,
    package_count: usize,
    cfg_edges: usize,
    extra_edges: usize,
) -> EdgeSpec {
    if ordinal < SYNTHETIC_NORMAL_EDGES_PER_PACKAGE {
        let global_index = source * SYNTHETIC_NORMAL_EDGES_PER_PACKAGE + ordinal;
        EdgeSpec {
            target: (source + ordinal + 1) % package_count,
            kind: EdgeKind::Normal,
            cfg: global_index < cfg_edges,
        }
    } else {
        assert!(source < extra_edges);
        EdgeSpec {
            target: (source + SYNTHETIC_NORMAL_EDGES_PER_PACKAGE + 2) % package_count,
            kind: if source.is_multiple_of(2) { EdgeKind::Dev } else { EdgeKind::Build },
            cfg: false,
        }
    }
}

fn write_json_string(output: &mut String, value: &str) {
    output.push('"');
    output.push_str(value);
    output.push('"');
}

fn synthetic_config(graph: &Graph<'_>, roots: usize) -> Config {
    let text = synthetic_config_text(graph, roots);
    let raw: config::RawConfig = toml::from_str(&text).expect("parse synthetic depgate config");
    config::validate(&raw, Some(graph)).expect("validate synthetic depgate config").config
}

fn synthetic_config_text(graph: &Graph<'_>, roots: usize) -> String {
    let mut text = String::from("schema = 1\n\n[manifest]\nversions-in-root = false\n\n");
    // Every synthetic root reaches the other workspace members through the
    // generated cycle, so an exact internal set exercises the full forward
    // traversal and rule/report path without a no-op deny rule.
    for &node in graph.members().iter().take(roots) {
        let root_name = graph.name(node);
        let expected = graph
            .members()
            .iter()
            .take(roots)
            .map(|&member| graph.name(member))
            .filter(|&name| name != root_name)
            .collect::<Vec<_>>();
        write!(&mut text, "[rules.\"{}\"]\ninternal = [", graph.name(node))
            .expect("write synthetic depgate config");
        for (index, name) in expected.iter().enumerate() {
            if index != 0 {
                text.push(',');
            }
            write_json_string(&mut text, name);
        }
        text.push_str("]\n\n");
    }
    text
}

fn main() {
    divan::main();
}
