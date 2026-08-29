# Differential check: `cargo-depgate` closure vs guppy (AC 12)

`cargo-depgate` evaluates rules over the **v1-unified, all-platform** normal-dependency
closure that `cargo metadata --format-version 1` resolves for the whole workspace
(plan §1.4). This page records, per host, how that closure compares with an
independent resolver — [guppy](https://crates.io/crates/guppy) 0.18.0 — for every
workspace member of the reference fixture, so a user can predict where an exact-set
rule (`internal`, `direct`) may see `+extra`.

The example that produces the tables lives at `examples/guppy_diff.rs`:

```sh
RUSTFLAGS= cargo run --example guppy_diff -- /tmp/ganja-metadata.json
```

`ours` is the number of distinct package **names** reachable from the member through
normal edges (the member's own name excluded); `guppy` is the same count from guppy's
resolution; `extra = ours − guppy`; `missing = guppy − ours`. The example exits 1 if any
row has `missing > 0`.

## Host `aarch64-apple-darwin` (`Darwin arm64`, rustc 1.98.0, cargo 1.98.0)

Fixture: `cargo metadata` of `ganja-code@153bfb1` (585 packages / 14 members /
1,586 normal edges / 529 names; `superset_extra_edges` over the 14 roots = 207).

### Table A — package graph, normal links present and enabled on the host

guppy: `PackageGraph::from_json` → `query_forward([member])` →
`resolve_with_fn(|_, link| link.normal().is_present() && link.normal().status().enabled_on(host) != Disabled)`.
This keeps every feature-unified edge and drops only platform-conditional edges that are
disabled on the host, so `extra` here measures the **cfg-conditional** part of the gap
alone.

| member | ours | guppy | extra (ours−guppy) | missing (guppy−ours) |
|---|---:|---:|---:|---:|
| ganja-cli | 473 | 387 | +86 | 0 |
| ganja-client | 169 | 129 | +40 | 0 |
| ganja-protocol | 35 | 18 | +17 | 0 |
| ganja-core | 291 | 230 | +61 | 0 |
| ganja-permission | 24 | 21 | +3 | 0 |
| ganja-provider | 250 | 201 | +49 | 0 |
| ganja-tool | 234 | 183 | +51 | 0 |
| ganja-storage | 68 | 47 | +21 | 0 |
| ganja-team | 67 | 44 | +23 | 0 |
| ganja-teammate-local | 293 | 232 | +61 | 0 |
| ganja-testkit | 301 | 240 | +61 | 0 |
| ganja-serve | 298 | 237 | +61 | 0 |
| ganja-tui | 443 | 359 | +84 | 0 |
| tmux | 30 | 27 | +3 | 0 |

### Table B — feature graph, member default features only (package-rooted, host)

guppy: `resolve_ids([member]).to_feature_set(StandardFeatures::Default)` →
`to_feature_query(Forward)` → `resolve_with_fn(|_, link| link.normal().enabled_on(host) != Disabled)`
→ `to_package_set()`. This is the `cargo tree -p MEMBER -e normal` shape the plan's §1.4
gap table was derived from, and it reproduces that table's six rows exactly
(ganja-protocol 15/35/+20, ganja-team 34/67/+33, ganja-storage 40/68/+28,
ganja-client 108/169/+61, ganja-core 210/291/+81, tmux 25/30/+5).

| member | ours | guppy | extra (ours−guppy) | missing (guppy−ours) |
|---|---:|---:|---:|---:|
| ganja-cli | 473 | 325 | +148 | 0 |
| ganja-client | 169 | 108 | +61 | 0 |
| ganja-protocol | 35 | 15 | +20 | 0 |
| ganja-core | 291 | 210 | +81 | 0 |
| ganja-permission | 24 | 20 | +4 | 0 |
| ganja-provider | 250 | 182 | +68 | 0 |
| ganja-tool | 234 | 163 | +71 | 0 |
| ganja-storage | 68 | 40 | +28 | 0 |
| ganja-team | 67 | 34 | +33 | 0 |
| ganja-teammate-local | 293 | 212 | +81 | 0 |
| ganja-testkit | 301 | 220 | +81 | 0 |
| ganja-serve | 298 | 217 | +81 | 0 |
| ganja-tui | 443 | 295 | +148 | 0 |
| tmux | 30 | 25 | +5 | 0 |

## Why `extra` is a superset by design and `missing` must be 0

`cargo-depgate` never re-implements cargo's resolver: it walks exactly the edges that
`cargo metadata` emitted, for every platform and with the features that the *whole
workspace* unified. Two families of edges therefore appear in `ours` but not in a
host-rooted, package-rooted resolution. The first is **cfg-conditional edges** —
`dep_kinds[].target` such as `cfg(windows)` or `cfg(target_arch = "wasm32")` — which is
why Table A's `extra` for `tmux` is `wasi, windows-link, windows-sys` and every member
carries the `windows_*`/`wasm-bindgen*` families. The second is **optional dependencies
unified by siblings**: an optional dependency that *another* member's feature set enables
is present in the single resolve cargo produces (`uuid → atomic`, `rand`, `quinn`,
`ring`, `defmt` in Table B), so a package-rooted view of one member omits it while the
unified graph keeps it. Both families only ever *widen* the closure, which is safe for
containment rules (`deny`, `leaf`, `sealed`) and is the measured `+extra` risk for
equality rules; `counters.superset_extra_edges` reports how many such edges a run
actually traversed. `missing`, on the other hand, would mean an edge that guppy follows
and our CSR does not — a lost `dep_kinds` fold, a dropped node, or a broken id lookup —
and there is no legitimate source for one, so the differential example treats a single
missing name as a failure. On this host it is 0 on every row of both tables.
