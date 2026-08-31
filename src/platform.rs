//! The platform selection a run evaluates `dep_kinds[].target` against.
//!
//! # Why this exists
//!
//! `cargo metadata` emits **one** resolve for every platform at once: `resolve.nodes[].deps`
//! is the union over every target a member could be built for, so a `cfg(windows)`-only
//! dependency is in the document on a Mac. That union is the right default for a policy gate —
//! a `deny` rule that missed an edge because the auditor happened to run on Linux would be a
//! false pass — but it is not what `cargo tree` shows, and a policy that only ever ships to one
//! platform has no reason to gate on the others. Selecting a platform narrows the edge set to
//! the one that platform activates, which is `cargo tree` parity in that dimension.
//!
//! # How an edge is judged
//!
//! Cargo records a conditional dependency's condition verbatim in `dep_kinds[].target`, in
//! either of the two forms a manifest may write: a `cfg(...)` expression
//! (`[target.'cfg(windows)'.dependencies]`) or a bare target triple
//! (`[target.x86_64-pc-windows-msvc.dependencies]`). A triple is compared literally; an
//! expression is parsed and evaluated by `cfg-expr` against the built-in target table rustc
//! ships, so no `rustc --print cfg` subprocess runs and no target has to be installed. An
//! entry with no `target` is unconditional and always survives.
//!
//! # What each predicate evaluates to
//!
//! A `cfg(...)` in a dependency table can name more than the target: `cfg(test)`,
//! `cfg(feature = "x")`, a bare `cfg(fuzzing)`. Cargo settles all of those by matching the
//! expression against the `rustc --print cfg` output for the target — which carries whatever
//! *that* rustc emits, plus whatever `--cfg` the build's `RUSTFLAGS` add. This process runs no
//! rustc and reads no `RUSTFLAGS`, so it has exactly two honest answers, and "the target table
//! cannot answer it" is one of them:
//!
//! * `target_arch`, `target_os`, `target_env`, `target_family`, `target_vendor`,
//!   `target_endian`, `target_pointer_width`, `target_has_atomic`, `panic`, and the bare
//!   `unix` / `windows` families come straight from the built-in target table.
//! * `test`, `proc_macro` and `feature = "..."` are **false**: cargo documents these as never
//!   set when it evaluates a dependency table's `cfg`, whatever the target or the flags.
//!   (`feature` is false in cargo too — rust-lang/cargo#7442.)
//! * Everything else is **unknown**, so its edge is kept: `debug_assertions`, a bare flag such
//!   as `cfg(overflow_checks)` or `cfg(tracing_unstable)`, a `key = "value"` outside the target
//!   table such as `cfg(relocation_model = "pic")`, and `target_feature = "..."`, whose enabled
//!   set depends on build flags rather than on the target. These are not hypothetical:
//!   `rustc --print cfg --target x86_64-unknown-linux-gnu` prints `debug_assertions` on every
//!   stable release and adds `overflow_checks` and `relocation_model="pic"` on a current
//!   nightly, and a `RUSTFLAGS=--cfg tracing_unstable` can add any bare flag at all.
//! * A `target_*` key this crate's parser does not know — including bare `cfg(target_thread_local)`,
//!   which another rustc release may well print — is not a predicate at all: `cfg-expr` rejects
//!   the whole expression, and an unparseable expression keeps its edge. Different route, same
//!   direction.
//!
//! Answering *false* for any of those would drop an edge cargo compiles. Enumerating the ones
//! rustc emits today would only move the failure to the next release that adds a key, so the
//! rule is the other way round: only what is provably false is false, and under-reporting is
//! structurally impossible rather than merely unobserved.
//!
//! # Widening, never narrowing, on what is genuinely unknown
//!
//! `cfg-expr`'s three-valued [`Option<bool>`] logic propagates *unknown* through `not`, `any`
//! and `all`, so an unknown operand only spreads where it actually decides the outcome:
//! `all(windows, target_feature = "avx2")` is still false on Linux. An edge whose condition
//! comes out unknown is **kept**, and so is one whose expression will not even parse. Keeping
//! an edge widens the closure, and a wider closure cannot hide a `deny` finding; dropping one
//! could, which is the single worst failure this tool can produce.
//!
//! The cost of that rule is over-reporting, and it is real: `tracing-core` gates `valuable`
//! behind a bare `cfg(tracing_unstable)`, so a `--platform host` run keeps `valuable` while a
//! default `cargo build` does not compile it. That is the direction a gate is allowed to be
//! wrong in, and `graph_tests` pins it as a named exception against guppy rather than letting
//! it drift.

use std::{fmt, process::Command, sync::OnceLock};

use cfg_expr::{
    Expression, Predicate,
    targets::{TargetInfo, get_builtin_target_by_triple},
};

/// The token that selects every platform: the default, and what `platform` is when unset.
pub const ALL: &str = "all";
/// The token that selects the host this process runs on.
pub const HOST: &str = "host";

/// The platforms whose dependency edges a run keeps.
///
/// Valid by construction: [`PlatformSelection::resolve`] is the only way to name a platform,
/// and it accepts a triple only when rustc's built-in target table has it, so nothing
/// downstream has to re-check a triple or decide what an unresolvable one means.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlatformSelection {
    /// Empty selects every platform — the unfiltered workspace-unified resolve, and the default.
    targets: Vec<&'static TargetInfo>,
}

/// A `platform` value naming something that is neither `all`, `host`, nor a target triple.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnknownPlatform {
    /// The position of the offending value in the list it was written in.
    pub index: usize,
    /// The value as it was written.
    pub value: String,
    /// The triple `host` resolved to, when `value` is `host` and rustc's built-in target table
    /// does not carry that triple; `None` when the value named a triple itself.
    ///
    /// The two failures need different words. A misspelt triple is the writer's to fix; a
    /// `host` this table cannot answer is not — the machine reported a triple `cfg-expr`'s
    /// snapshot of rustc's target list has never heard of — and telling that writer to use
    /// `host` instead would be advising them to repeat what just failed.
    pub host_triple: Option<String>,
}

impl fmt::Display for UnknownPlatform {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(triple) = &self.host_triple {
            return write!(
                formatter,
                "`host` resolved to `{triple}`, which is not in rustc's built-in target table"
            );
        }
        write!(
            formatter,
            "unknown target platform `{}`; expected `all`, `host`, or a target triple rustc \
             knows (see `rustc --print target-list`)",
            self.value
        )
    }
}

impl PlatformSelection {
    /// Every platform: the unfiltered resolve, and what a run does when nothing selects.
    #[must_use]
    pub const fn all() -> Self {
        Self { targets: Vec::new() }
    }

    /// Resolves `all`, `host` and target-triple tokens into a selection.
    ///
    /// `all` anywhere selects every platform: it is the superset of whatever stands beside it,
    /// so an explicit `all` cannot be narrowed by a neighbour. `host` resolves through
    /// [`host_triple`]. Repeated triples collapse and first-seen order is kept, so a report can
    /// echo the selection back in the order it was written.
    ///
    /// An empty `tokens` selects every platform. Callers that must reject "no platform at all"
    /// — a written `platform = []`, which would silently drop every conditional edge — check
    /// for it before calling; there is no platform set this function could return for it.
    ///
    /// # Errors
    ///
    /// Returns [`UnknownPlatform`] for the first token that is neither `all`, `host`, nor a
    /// triple in rustc's built-in target table, carrying its index so the caller can anchor a
    /// diagnostic at the value that caused it. A `host` that resolves to a triple the table
    /// does not carry fails here too, naming the resolved triple rather than the word.
    pub fn resolve(tokens: &[String]) -> Result<Self, UnknownPlatform> {
        Self::resolve_lazy(tokens, host_triple)
    }

    /// [`PlatformSelection::resolve`] with the host lookup handed in unevaluated.
    ///
    /// The host triple comes from spawning `rustc -vV`, and every selection used to pay for
    /// that spawn through argument evaluation even when no token was `host` -- on a CI runner
    /// that is ~30 ms of wall clock and a child whose peak RSS the measurement harness
    /// attributes to this process. The lookup now runs only when some token actually says
    /// `host`, and the test suite pins that with a closure that panics if called.
    fn resolve_lazy(
        tokens: &[String],
        host: impl FnOnce() -> &'static str,
    ) -> Result<Self, UnknownPlatform> {
        let host = if tokens.iter().any(|token| token == HOST) { host() } else { "" };
        Self::resolve_against_host(tokens, host)
    }

    /// [`PlatformSelection::resolve`] with the triple `host` stands for passed in, so a `host`
    /// the built-in target table cannot answer is testable without a rustc that reports one.
    fn resolve_against_host(tokens: &[String], host: &str) -> Result<Self, UnknownPlatform> {
        let mut targets: Vec<&'static TargetInfo> = Vec::with_capacity(tokens.len());
        for (index, token) in tokens.iter().enumerate() {
            if token == ALL {
                return Ok(Self::all());
            }
            let is_host = token == HOST;
            let triple = if is_host { host } else { token.as_str() };
            let target = get_builtin_target_by_triple(triple).ok_or_else(|| UnknownPlatform {
                index,
                value: token.clone(),
                host_triple: is_host.then(|| triple.to_owned()),
            })?;
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
        Ok(Self { targets })
    }

    /// Whether this selection keeps every edge — `all`, and the default.
    #[must_use]
    pub fn is_all(&self) -> bool {
        self.targets.is_empty()
    }

    /// The selected target triples in first-seen order; empty for `all`.
    ///
    /// These are the *resolved* triples, so `host` reads back as the triple it resolved to and
    /// a report stays reproducible off the machine that produced it.
    pub fn triples(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.targets.iter().map(|target| target.triple.as_str())
    }

    /// Whether a `dep_kinds[].target` value activates on at least one selected platform.
    ///
    /// Always `true` under [`PlatformSelection::all`]. A value that is not a `cfg(...)`
    /// expression is a literal target triple and is compared as one. See the module
    /// documentation for why an undecidable or unparseable expression keeps its edge.
    #[must_use]
    pub fn activates(&self, target: &str) -> bool {
        if self.is_all() {
            return true;
        }
        if !target.starts_with("cfg(") {
            return self.targets.iter().any(|selected| selected.triple.as_str() == target);
        }
        let Ok(expression) = Expression::parse(target) else {
            return true;
        };
        self.targets.iter().any(|selected| {
            let verdict: Option<bool> = expression.eval(|predicate| match predicate {
                Predicate::Target(target) => Some(target.matches(*selected)),
                // The three cargo settles for us: never set while it evaluates a dependency
                // table's cfg, on any target and under any flags.
                Predicate::Test | Predicate::ProcMacro | Predicate::Feature(_) => Some(false),
                // Everything else is unknown, so the edge is kept. `cfg-expr` routes every key
                // the built-in target table answers into `Predicate::Target`, so a `KeyValue`
                // reaching this arm is by construction one the table cannot answer — today
                // `relocation_model = "pic"`, tomorrow whatever rustc adds next. A bare flag
                // and `debug_assertions` are the same story with a shorter spelling, and any
                // of them can also arrive from a `RUSTFLAGS=--cfg ...` this process never sees.
                Predicate::DebugAssertions
                | Predicate::TargetFeature(_)
                | Predicate::Flag(_)
                | Predicate::KeyValue { .. } => None,
            });
            verdict != Some(false)
        })
    }
}

/// The target triple `host` resolves to.
///
/// `rustc -vV` is the authority: it is the triple cargo resolves a bare build to, and asking
/// for it compiles nothing. `RUSTC` is honoured so a run inside a toolchain wrapper asks the
/// same compiler cargo would. When rustc cannot be reached at all, the triple this binary was
/// compiled for stands in — right on every machine that runs the binary its build produced.
#[must_use]
pub fn host_triple() -> &'static str {
    static HOST_TRIPLE: OnceLock<String> = OnceLock::new();
    HOST_TRIPLE
        .get_or_init(|| rustc_host().unwrap_or_else(|| env!("DEPGATE_HOST_TARGET").to_owned()))
        .as_str()
}

/// The `host:` line of `rustc -vV`, or `None` when rustc cannot be run or does not report one.
fn rustc_host() -> Option<String> {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc).arg("-vV").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
#[path = "platform_tests.rs"]
mod tests;
