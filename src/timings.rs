//! Phase timings and run counters, with the pinned `--timings` line format.
//!
//! `--timings` prints one `<phase>\t<ms>` line per [`Phase`] in declaration order,
//! followed by one `<counter>\t<n>` line per [`Counters`] field in the §1.5 order.
//! The format is a contract: scripts split on the tab, and every first token is
//! unique across the two blocks (the phase is `evaluate`, the counter is `rules`),
//! so `awk -F'\t' '$1=="…"'` yields exactly one line.

use std::{
    io::{self, Write},
    time::{Duration, Instant},
};

/// The pipeline phases, in report order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum Phase {
    /// Acquiring the metadata bytes (spawn + read, or file/stdin read).
    Read,
    /// `serde_json::from_slice` into the borrowing structs, plus rebasing.
    Parse,
    /// Interning, the `dep_kinds` fold and the CSR.
    Graph,
    /// Every forward and reverse BFS.
    Traversals,
    /// Rule evaluation over the reaches (labelled `evaluate`; `rules` is a counter).
    Evaluate,
    /// The manifest rule.
    Manifest,
    /// Rendering the report.
    Report,
    /// Wall time from [`Timings::start`] to [`Timings::finish`].
    Total,
}

impl Phase {
    /// Every phase, in report order.
    pub const ALL: [Self; 8] = [
        Self::Read,
        Self::Parse,
        Self::Graph,
        Self::Traversals,
        Self::Evaluate,
        Self::Manifest,
        Self::Report,
        Self::Total,
    ];

    /// The label printed by `--timings` and used as the JSON `timings` key.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Parse => "parse",
            Self::Graph => "graph",
            Self::Traversals => "traversals",
            Self::Evaluate => "evaluate",
            Self::Manifest => "manifest",
            Self::Report => "report",
            Self::Total => "total",
        }
    }
}

/// Monotonic per-phase durations in milliseconds.
#[derive(Clone, Debug)]
pub struct Timings {
    started: Instant,
    millis: [f64; Phase::ALL.len()],
}

impl Default for Timings {
    fn default() -> Self {
        Self::start()
    }
}

impl Timings {
    /// Starts the total clock.
    #[must_use]
    pub fn start() -> Self {
        Self { started: Instant::now(), millis: [0.0; Phase::ALL.len()] }
    }

    /// Records `elapsed` for `phase`, adding to any earlier measurement of it.
    pub fn add(&mut self, phase: Phase, elapsed: Duration) {
        self.millis[phase as usize] += elapsed.as_secs_f64() * 1e3;
    }

    /// Times `work` and records it under `phase`.
    pub fn measure<T>(&mut self, phase: Phase, work: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let result = work();
        self.add(phase, started.elapsed());
        result
    }

    /// Sets [`Phase::Total`] to the wall time since [`Timings::start`].
    pub fn finish(&mut self) {
        self.millis[Phase::Total as usize] = self.started.elapsed().as_secs_f64() * 1e3;
    }

    /// The recorded milliseconds for `phase`.
    #[must_use]
    pub fn millis(&self, phase: Phase) -> f64 {
        self.millis[phase as usize]
    }

    /// Writes the `--timings` block: phases, then counters.
    ///
    /// # Errors
    ///
    /// Propagates write errors from `out`.
    pub fn write_to(&self, counters: &Counters, out: &mut impl Write) -> io::Result<()> {
        for phase in Phase::ALL {
            writeln!(out, "{}\t{:.3}", phase.label(), self.millis(phase))?;
        }
        counters.write_to(out)
    }
}

/// The run counters of §1.5, in report order.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Counters {
    /// Nodes in the graph (= `packages[]` entries).
    pub packages: u32,
    /// Workspace members.
    pub members: u32,
    /// Normal edges in the CSR.
    pub normal_edges: u32,
    /// Distinct package names.
    pub names: u32,
    /// Distinct traversed cfg-only or member-optional edges (a union over roots).
    pub superset_extra_edges: u32,
    /// `direct` rules whose package declares an optional normal dependency.
    pub direct_optional_decls: u32,
    /// Non-member `path+` packages left unrebased under `--workspace-root`.
    pub unrebased_path_deps: u32,
    /// Rules evaluated (graph rules plus the manifest rule).
    pub rules: u32,
    /// Failed rules.
    pub violations: u32,
    /// Matched names across `deny`, `+extra` and `sealed` entries.
    pub matches: u32,
}

impl Counters {
    /// `(label, value)` pairs in report order.
    #[must_use]
    pub fn entries(&self) -> [(&'static str, u32); 10] {
        [
            ("packages", self.packages),
            ("members", self.members),
            ("normal_edges", self.normal_edges),
            ("names", self.names),
            ("superset_extra_edges", self.superset_extra_edges),
            ("direct_optional_decls", self.direct_optional_decls),
            ("unrebased_path_deps", self.unrebased_path_deps),
            ("rules", self.rules),
            ("violations", self.violations),
            ("matches", self.matches),
        ]
    }

    /// Writes one `<counter>\t<n>` line per field.
    ///
    /// # Errors
    ///
    /// Propagates write errors from `out`.
    pub fn write_to(&self, out: &mut impl Write) -> io::Result<()> {
        for (label, value) in self.entries() {
            writeln!(out, "{label}\t{value}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "timings_tests.rs"]
mod tests;
