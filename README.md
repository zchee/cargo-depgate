# cargo-depgate

A high-performance dependency policy enforcer and CI gatekeeper for Cargo workspaces.

`cargo-depgate` acts as an automated quality gate in your CI/CD pipelines. It ensures that dependency graphs adhere to strict organizational policies before code reaches production.

### Key Capabilities

* **Workspace Boundary Enforcement**: Prevent target-specific or internal crates from leaking across crate boundaries.
* **Transitive Dependency Auditing**: Block banned crates or unvetted third-party additions anywhere in the closure.
* **Deterministic Fail-Fast CI**: Emits structured diagnostics with precise exit codes tailored for GitHub Actions and automated workflows.
* **Zero Compilation Overhead**: Evaluates `Cargo.lock` and metadata directly without compiling source code.

## Install and run

```sh
cargo install --git https://github.com/zchee/cargo-depgate --locked
```

`cargo depgate schema` prints the JSON Schema of `depgate.toml` and is the one subcommand that needs
no policy file. Write a `depgate.toml` at the workspace root, then run the gate:

```sh
cargo depgate check                        # `check` is the default subcommand
cargo depgate check --config depgate.toml  # explicit policy file
cargo depgate explain my-app openssl       # whether, and how, one package reaches another
```

`explain` loads and validates the policy file on exactly the same path `check` does, even though it
evaluates no rules. It therefore needs a `depgate.toml` at the workspace root, or an explicit
`--config PATH`. In a workspace that has neither, it exits 2 with the same missing-configuration
message `check` would print.

Both invocation forms work: `cargo depgate …`, where Cargo passes `depgate` as `argv[1]` and the
binary strips it, and `cargo-depgate …` run directly. `check` and `explain` share the same global
flags: `-m/--manifest-path`, `--config`, `--metadata-json`, `--workspace-root`, `-F/--features`,
`--all-features`, `--no-default-features`, `--offline`, `--locked`/`--no-locked`, `--cargo-timeout`,
`--format` and `--timings`. `--workspace-root` is the one exception to "shared": it is valid only
together with `--metadata-json`, and on its own it is a usage error. `--locked` is the default,
because a gate must never rewrite
`Cargo.lock`; `--no-locked` turns it off deliberately. `--format` defaults to `github` when
`GITHUB_ACTIONS=true` and to `human` otherwise.

<!-- depgate:semantics -->

## Policy semantics

### Where the policy file lives

`depgate.toml` sits at the workspace root and is the whole policy: one reviewable file, exact
sets, unknown keys rejected.

* With `--config PATH` the file is read and its graph-independent checks run **before**
  `cargo metadata` is spawned, so a typo never pays for a resolve. The same file is validated
  again against the resolved graph.
* Without `--config` the file is discovered at the workspace root that `cargo metadata` reports,
  and validated once. There is no walk-up from the current directory and no second Cargo spawn.
* When `--config` is given, a `depgate.toml` at the workspace root is ignored: the explicit file
  wins.
* A missing, unreadable or invalid file is exit code 2 on both paths, with the same message; only
  whether Cargo was spawned first differs.

### A complete `depgate.toml`

Every v1 key and every rule kind appears below.

```toml
schema = 1

[graph]
# "default" | "all" | ["pkg/feature", ...]. Reaches `cargo metadata` only through --config;
# --features / --all-features on the command line override it.
features = "default"

[internal]
# Which package names count as "internal" for the `internal` and `leaf` rules.
members = true
patterns = ["acme-*"]

[manifest]
# Every dependency version must live in the workspace-owning manifest.
versions-in-root = true

[rules.acme-app]
deny = ["openssl*", "ring"]
internal = ["acme-core", "acme-store"]
direct = ["acme-core", "serde", "tokio"]

[rules.acme-core]
internal = ["acme-store"]

[rules.acme-store]
leaf = true
sealed = true
```

### Configuration reference

| Key | Value | Default | Meaning |
|---|---|---|---|
| `schema` | integer | required | Policy schema version. v1 accepts only `1`; any other value is exit 2. |
| `[graph].features` | `"default"`, `"all"`, or a list of Cargo feature specs such as `["app/net"]` | `"default"` | Feature selection for the `cargo metadata` run. See *Feature selection* below: it takes effect only through `--config`. |
| `[internal].members` | boolean | `true` | Treat every workspace member as an internal package. |
| `[internal].patterns` | list of name globs | `[]` | Extra names counted as internal, e.g. `["acme-*"]`. Together with `members` this is the single definition of "internal", and it is used for membership matching only: the `internal` and `leaf` rules ask of each reached name whether it is in that set. Nothing else reads it — witness paths render identically whether or not a hop is internal. |
| `[manifest].versions-in-root` | boolean | `true` | Enable the manifest rule described below. |
| `[rules.<package>].deny` | list of names or globs | unset | Names that must not appear anywhere in the package's closure. The rule's own package name never matches, so a family glob such as `deny = ["acme-*"]` on `rules.acme-app` does not report `acme-app` itself: a self-match is not a dependency finding. |
| `[rules.<package>].internal` | list of exact names | unset | The exact set of internal names the closure may contain. The rule's own package name is skipped here too, so it is neither required in the set nor reported as `+extra`. |
| `[rules.<package>].leaf` | boolean | unset | The closure must contain no internal name at all. Sugar for `internal = []`, and mutually exclusive with it. |
| `[rules.<package>].direct` | list of exact names | unset | The exact set of resolved depth-one normal dependencies. |
| `[rules.<package>].sealed` | boolean | unset | No other workspace member may reach this package. |

Unknown keys are rejected rather than ignored. A file that enables nothing — no `[rules.*]` table
and `versions-in-root = false` — is rejected with `depgate.toml declares no rules`, because a green
gate that checks nothing is a failure mode, not a pass. `cargo depgate schema` prints the generated
JSON Schema for editor completion.

Validation happens in two groups. The graph-independent group covers TOML syntax, the `schema`
value, unknown keys, the empty-policy case, `leaf` together with `internal`, a package listing
itself in its own `internal` set, a `[graph].features` value that is neither `default`, `all` nor a
feature list, and glob compilation. The graph-dependent group additionally
requires that every `[rules.<package>]` key names a workspace member and that every `internal` and
`direct` entry names a package present in the resolved graph — these are exact sets, so globs are
not accepted there. Both groups exit 2 and point at the offending line and column in `depgate.toml`.

### The graph the rules see

Rules are evaluated over the **normal** edges of the single `cargo metadata --format-version 1`
resolve for the whole workspace:

* An edge is normal when any of its `dep_kinds` entries has `kind = null`. Dev-dependency and
  build-dependency edges are excluded; proc-macro crates are reached through normal edges and stay
  in.
* Every platform is traversed. A `cfg(windows)` or `cfg(target_arch = "wasm32")` edge is followed
  on every host, and the witness marks it, for example `app v0.1.0 → winonly v0.1.0 [cfg(windows)]`.
* Features are Cargo's, unified across the whole workspace. An optional dependency that another
  member enables is in the graph, and the witness annotates that hop with
  `(optional; present via workspace feature unification)`.
* Renamed dependencies match by resolved package name, never by the local alias.

This is a **superset** of what `cargo tree -p <member> -e normal` shows on one host, which is what
the gap table below measures. The direction matters per rule kind: `deny`, `leaf` and `sealed` ask
a containment question, so a wider graph can only add findings and never hide one. `internal` and
`direct` ask an equality question, so a wider graph can report a `+extra` name that a host-rooted,
package-rooted view would not have shown.

### Rule kinds

| Rule id | Question | Failure direction |
|---|---|---|
| `rules.<pkg>.deny` | Does the closure of `<pkg>` contain a name matching any pattern? | containment — widening only adds findings |
| `rules.<pkg>.internal` | Are the internal names in the closure exactly the declared set? | equality — widening can add `+extra` |
| `rules.<pkg>.leaf` | Does the closure contain no internal name? | containment |
| `rules.<pkg>.direct` | Are the resolved depth-one normal dependencies exactly the declared set? | equality on depth-one edges |
| `rules.<pkg>.sealed` | Is `<pkg>` absent from the closure of every other workspace member? | containment |
| `manifest.versions-in-root` | Does any member manifest name a dependency version? | not graph-based |

A `deny` entry is an exact name unless it contains `*`, `?` or `[`, in which case it is a glob.
Matching is case-sensitive and `-`/`_` are never normalised, so `deny = ["axum"]` does not match
`axum-core`; write `axum*` when the ban is meant to cover a family.

The manifest rule reads every workspace member's `Cargo.toml` and flags a version in
`[dependencies]`, `[dev-dependencies]`, `[build-dependencies]` and their `target.*` forms, in both
the string form `serde = "1"` and the table form `serde = { version = "1" }`. An entry passes
exactly when it names no `version`: `workspace = true`, a bare `path` and a bare `git` reference all
qualify, and adding a `version` to any of them does not, so
`foo = { path = "../foo", version = "0.1.0" }` is flagged. `[workspace.dependencies]` is
never flagged: only the workspace-owning manifest may declare it and it is the canonical version
table. On a root-package workspace the root's own dependency tables are still checked.

`direct` deserves one caveat, which the tool reports rather than hides. Because the resolve is
feature-unified, an optional dependency of `<pkg>` that a *sibling* member enables shows up among
`<pkg>`'s depth-one edges. A `direct` rule on a package that declares an optional normal dependency
therefore prints a warning and increments `counters.direct_optional_decls`.

### Feature selection

`[graph].features` can only change the graph if it is read before Cargo runs, which is exactly the
`--config` path. The resulting behaviour:

* With `--config PATH`, a non-default `[graph].features` is applied to the `cargo metadata` spawn.
* `--features` and `--all-features` on the command line override the file entirely. A bare
  `--no-default-features` is not a selection: it is combined with whatever the file or the other
  flags selected, exactly as Cargo combines it.
* A **discovered** `depgate.toml` — one found at the workspace root, without `--config` — with a
  non-default `[graph].features` is exit 2, not a silently different graph, because the file is
  found only after `cargo metadata` has already run. The message says to pass `--config` or the
  CLI feature flags.
* Under `--metadata-json` no Cargo runs at all, so a non-default selection is ignored with
  `warning: [graph].features is ignored under --metadata-json; the JSON was produced with its own
  feature selection`.

At a virtual workspace root, Cargo rejects bare feature names, so write `--features pkg/feature`.
Feature arguments are forwarded to Cargo verbatim; any Cargo error is exit 3 with Cargo's own
stderr.

### What a report looks like

Every configured rule is listed with its status, so a rule that matched nothing is still visible,
and every graph violation carries a shortest witness path:

```text
error[rules.core.deny]: dependency policy violation
 --> depgate.toml:7:8
  |
7 | deny = ["ui"]
  |        ^^^^^^ 1 match(es)
  core v0.1.0 → ui v0.1.0
error[rules.core.internal]: dependency policy violation
 --> depgate.toml:8:12
  |
8 | internal = []
  |            ^^ 1 extra, 0 missing
  +ui (via core v0.1.0 → ui v0.1.0)
error[rules.core.direct]: dependency policy violation
 --> depgate.toml:9:10
  |
9 | direct = ["tool"]
  |          ^^^^^^^^ 1 extra, 1 missing
  +ui (via core v0.1.0 → ui v0.1.0)
  -tool
error[rules.core.sealed]: dependency policy violation
  --> depgate.toml:10:10
   |
10 | sealed = true
   |          ^^^^ consumed by 1 member(s)
  consumed by: tool (tool → core)
error[rules.tool.leaf]: dependency policy violation
  --> depgate.toml:13:8
   |
13 | leaf = true
   |        ^^^^ 2 extra, 0 missing
  +core (via tool v0.1.0 → core v0.1.0)
  +ui (via tool v0.1.0 → core v0.1.0 → ui v0.1.0)
ok rules.ui.leaf
FAIL: 6 rules, 5 violations
```

There is one violation per failed **graph** rule, not one per matched name: a `deny` violation
carries every matching name it reached, an `internal` or `direct` violation carries its `+extra` and
`-missing` entries, and a `sealed` violation carries one entry per consuming member.

`--format json` emits the same information as
`{tool, version, features, timings, counters, violations[]}`, where `counters` reports `packages`,
`members`, `normal_edges`, `names`, `superset_extra_edges`, `direct_optional_decls`,
`unrebased_path_deps`, `rules`, `violations` and `matches`.

`manifest.versions-in-root` is the exception to the cardinality above: it contributes **one
`violations[]` element per offending dependency entry**, each anchored at the version span in that
member's `Cargo.toml` and carrying `table`, `dependency` and `version` in place of a witness.

```json
{
  "rule_id": "manifest.versions-in-root",
  "package": "app",
  "kind": "manifest",
  "matches": [],
  "extra": [],
  "missing": [],
  "sealed_by": [],
  "span": { "file": "crates/app/Cargo.toml", "line": 7, "col": 36 },
  "table": "dependencies",
  "dependency": "foo",
  "version": "0.1.0"
}
```

`counters.violations` always counts **failed rules**, so it can be smaller than the length of
`violations[]` — three flagged entries in one member manifest are three array elements but one
failed rule. Gate a pipeline on the exit code, or on `counters.violations`; treat `violations[]` as
the finding list to render, not as a count of rules.

`--timings` writes one `<phase>\t<ms>` line per phase to stderr, followed by one `<counter>\t<n>`
line per counter, in the report order listed above. Both blocks are tab-separated so scripts can
split on the tab.

<!-- depgate:gap-table -->

## Cargo feature gap table

`cargo-depgate` walks the workspace-unified, all-platform resolve, while `cargo tree -p M -e normal`
shows one member on one host with that member's own features. The difference is measured, not
assumed. The rows below are `ganja-code@153bfb1` on host **`aarch64-apple-darwin`** (rustc 1.98.0,
cargo 1.98.0); both columns count distinct reachable package names and exclude the member itself.

| member | `cargo tree` | depgate closure | `+extra` | extra internal |
|---|---:|---:|---:|---:|
| `ganja-protocol` | 15 | 35 | +20 | 0 |
| `ganja-team` | 34 | 67 | +33 | 0 |
| `ganja-storage` | 40 | 68 | +28 | 0 |
| `ganja-client` | 108 | 169 | +61 | 0 |
| `ganja-core` | 210 | 291 | +81 | 0 |
| `tmux` | 25 | 30 | +5 | 0 |

The extras come from two families only: platform-conditional edges such as the `windows-sys` and
`wasm-bindgen` families, and optional dependencies that another workspace member unified on, such as
`uuid → atomic`. No `cargo tree` name was ever missing from the closure, on any of the 14 members —
a missing name would mean a lost edge, and the differential gate fails on one.

"Extra internal" is the column that decides whether an exact-set rule is at risk, and it is 0 for
every member here, because no member of that workspace declares an optional normal dependency on
another member, and no member→member edge in this fixture is optional or target-gated. That is a
measured property of `ganja-code`, not a general rule: a member→member edge declared
`optional = true` would be traversed, and `superset_extra_edges` exists to count exactly that class
of edge. `counters.superset_extra_edges` reports how many
platform-conditional or member-optional edges a given run actually traversed, so the exposure is
visible per run rather than inferred. All 14 member rows, per host, and an independent cross-check
against [guppy](https://crates.io/crates/guppy) live in [`docs/differential.md`](docs/differential.md).

<!-- depgate:exit-codes -->

## Exit codes

Exit codes are 0 for success, 1 for policy violations, 2 for configuration or usage errors, 3 for `cargo metadata` failures, and 4 when the rendered report, `explain` output, or configuration schema could not be written (a closed reader, such as piping through `head`, is not treated as a failure).

| Code | Meaning |
|---:|---|
| `0` | The policy passed. `explain` and `schema` also exit 0 on success, and `explain` exits 0 whether or not the dependency is reachable. |
| `1` | At least one rule failed. `counters.violations` is the number of failed rules. |
| `2` | Configuration or usage error: an invalid command line, or a `depgate.toml` that is missing, unparsable, of the wrong schema, empty of rules, self-contradictory, or naming a package that is not a workspace member or not in the graph. `explain` validates the same file on the same path, so a missing or invalid `depgate.toml` exits 2 there too, and `explain` on an unknown name exits 2 with the same message. |
| `3` | `cargo metadata` failed — it could not be spawned, exceeded `--cargo-timeout`, exited non-zero, or produced JSON that fails a fail-closed input check — or a member manifest could not be read or parsed, or the `--metadata-json` document could not be read. Nothing is ever silently skipped. |
| `4` | The report, `explain` output, or schema could not be written. A broken pipe is excluded: piping into `head` keeps the policy exit code. |

New codes will be added rather than renumbered, because pipelines gate on them.

<!-- depgate:ci -->

## CI integration

Install the gate in the job first — `cargo install --git https://github.com/zchee/cargo-depgate
--locked`, or restore it from the job's tool cache — and then choose between the two forms below.

Run one `cargo metadata` for the whole job and let every tool read it. `--metadata-json` consumes an
existing `cargo metadata` document instead of spawning Cargo, and `--workspace-root` rebases the
paths inside it when the document was produced elsewhere.

v1 has **no staleness check** against `Cargo.lock`: a JSON file that predates the current lock will
be used as-is. So generate it in the same job that consumes it.

```yaml
      - name: dependency policy
        run: |
          cargo metadata --format-version 1 --locked > "$RUNNER_TEMP/metadata.json"
          cargo depgate check \
            --metadata-json "$RUNNER_TEMP/metadata.json" \
            --config depgate.toml
```

Pass the same feature flags to both commands whenever the build is not on default features; the
resolve in the document is what the gate sees. Do not pass `--locked` to the `cargo depgate` line
in this form — the generating command already enforced the lock, the gate cannot enforce it against
a stored document, and it warns that the flag is ignored.

`--offline` and `--cargo-timeout` are inert on this path too, since no Cargo runs, but unlike
`--locked`/`--no-locked` they are accepted **silently**: no warning is printed. Put them on the
`cargo metadata` line, where they still do something.

The simpler form lets the gate run Cargo itself, which is one `cargo metadata` and no temporary
file:

```yaml
      - name: dependency policy
        run: cargo depgate check --config depgate.toml
```

Under GitHub Actions both forms auto-select `--format github`, which emits error annotations
followed by the full human report. There are two annotation shapes:

* A failed **graph** rule emits one `::error file=depgate.toml,line=…,col=…::…` annotation,
  anchored at that rule's line in the policy file.
* `manifest.versions-in-root` emits one annotation **per offending dependency entry**, anchored at
  the version span in that member's `Cargo.toml` — for example
  `::error file=crates/app/Cargo.toml,line=7,col=36::manifest.versions-in-root: dependencies foo = "0.1.0"`.
  One failed manifest rule can therefore produce many annotations.

Graph annotations are emitted first, then manifest ones, and at most **ten** in total, which is
GitHub's per-step cap. A workspace with more than ten version-carrying member entries will lose the
overflow from the annotation list. The human report printed below the annotations is never
truncated: it always carries every violation, so the annotations are a navigation aid and the report
is the record.

[`docs/migration/ganja-code.md`](docs/migration/ganja-code.md) is a worked migration: 180 lines of
`cargo tree | grep` steps replaced by one policy file and one gate invocation, with the rule-to-line
mapping kept as the review baseline.

<!-- depgate:version-blind -->

## Version-blind policies

Rules operate on package **names**, not on `(name, version)` pairs. In the reference workspace, 585
resolved packages project onto 529 distinct names, and 42 of those names are resolved at two or
three versions at once. The consequences are worth stating plainly:

* `deny = ["syn"]` denies every resolved version of `syn`.
* `internal` and `direct` compare name sets, so two versions of one name are one member of the set.
* `[internal].patterns` inherits the same collapse: a pattern matching a multi-version name matches
  all of its versions.
* There is no version predicate. `deny` cannot express `ratatui < 0.30`, and no syntax such as
  `"openssl@<0.10"` is accepted.

Witnesses stay concrete, because they are node paths through the real graph: each hop renders with
its version, and when the matched name is resolved at several versions the report names the one
that was reached first and notes the others. The JSON report carries the same information as
`other_versions` on each match. For decisions that genuinely depend on semver — advisories,
licences, version bans — use a tool built for that, such as `cargo-deny`, alongside this one.

<!-- depgate:codeowners -->

## CODEOWNERS integration

`depgate.toml` is architecture policy in executable form, and a pull request that adds a dependency
can also weaken the rule that would have caught it. Assign the file to the people who own those
boundaries:

```text
/depgate.toml @your-org/architecture-owners
```

Anchor the entry at the repository root so it matches the workspace-root file and nothing else, and
name a team GitHub can actually request a review from. When a migration retires existing CI checks,
keep its rule-to-check mapping in the repository next to the policy: it is what a reviewer needs in
order to tell a deliberate policy change from an accidental one.
