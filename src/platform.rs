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
//! `cfg(feature = "x")`, a bare `cfg(fuzzing)`. Cargo settles all of those the same way — it
//! matches the expression against `rustc --print cfg` for the target, where none of them
//! appear — so this evaluator follows cargo rather than inventing a rule:
//!
//! * `target_arch`, `target_os`, `target_env`, `target_family`, `target_vendor`,
//!   `target_endian`, `target_pointer_width`, `target_has_atomic`, `panic`, and the bare
//!   `unix` / `windows` families come straight from the built-in target table.
//! * `test`, `debug_assertions`, `proc_macro`, `feature = "..."`, a bare flag, and any other
//!   `key = "value"` are **false**, exactly as cargo evaluates them in a dependency table.
//!   (`feature` is false in cargo too — rust-lang/cargo#7442.)
//! * `target_feature = "..."` is **unknown**: which features a build enables depends on flags
//!   this process cannot see, and guessing either way would be a fabrication.
//!
//! # Widening, never narrowing, on what is genuinely unknown
//!
//! `cfg-expr`'s three-valued [`Option<bool>`] logic propagates *unknown* through `not`, `any`
//! and `all`, so an unknown operand only spreads where it actually decides the outcome:
//! `all(windows, target_feature = "avx2")` is still false on Linux. An edge whose condition
//! comes out unknown is **kept**, and so is one whose expression will not even parse. Keeping
//! an edge widens the closure, and a wider closure cannot hide a `deny` finding; dropping one
//! could, which is the single worst failure this tool can produce.

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
}

impl fmt::Display for UnknownPlatform {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
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
    /// diagnostic at the value that caused it.
    pub fn resolve(tokens: &[String]) -> Result<Self, UnknownPlatform> {
        let mut targets: Vec<&'static TargetInfo> = Vec::with_capacity(tokens.len());
        for (index, token) in tokens.iter().enumerate() {
            if token == ALL {
                return Ok(Self::all());
            }
            let triple = if token == HOST { host_triple() } else { token.as_str() };
            let target = get_builtin_target_by_triple(triple)
                .ok_or_else(|| UnknownPlatform { index, value: token.clone() })?;
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
                // The one genuinely undecidable predicate: enabled target features depend on
                // build flags, not on the target.
                Predicate::TargetFeature(_) => None,
                // Everything else is false in a dependency table, the way cargo evaluates it.
                Predicate::Test
                | Predicate::DebugAssertions
                | Predicate::ProcMacro
                | Predicate::Feature(_)
                | Predicate::Flag(_)
                | Predicate::KeyValue { .. } => Some(false),
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
