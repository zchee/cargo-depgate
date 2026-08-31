# cargo-depgate

A high-performance dependency policy enforcer and CI gatekeeper for Cargo workspaces.

`cargo-depgate` acts as an automated quality gate in your CI/CD pipelines. It ensures that dependency graphs adhere to strict organizational policies before code reaches production.

### Key Capabilities

* **Workspace Boundary Enforcement**: Prevent target-specific or internal crates from leaking across crate boundaries.
* **Transitive Dependency Auditing**: Block banned crates or unvetted third-party additions anywhere in the closure.
* **Deterministic Fail-Fast CI**: Emits structured diagnostics with precise exit codes tailored for GitHub Actions and automated workflows.
* **Zero Compilation Overhead**: Evaluates the resolved `cargo metadata` graph directly without compiling source code.

Concretely: the gate's own work after `cargo metadata` returns is ~4 ms on a 700-package workspace,
so its cost is dominated by the single resolve it already needs. Replacing the four `cargo tree`
invocations of lemmy's dependency-policy step — all four of which the gate expresses — with one
gate run measures 3.1x end to end (298.7 ms against 931.2 ms on `aarch64-apple-darwin`; the absolute
numbers move with the host, the ratio holds), and the ceiling scales with how many invocations a
policy replaces.

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

It also resolves the package it starts from the way a rule root resolves: **workspace members
first**, so `explain foo bar` and `[rules.foo] deny = ["bar"]` always ask about the same node. A
name that is not a member but is carried by exactly one package resolves to that package; a name
that is not a member and is resolved at several versions is refused with exit 2 naming those
versions, rather than silently answering for one of them.

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
# Answer this table's rules on the closure `acme-core` compiles with no features, instead of
# on the workspace-unified resolve. Only on an all-features document; see below.
features = "none"
internal = ["acme-store"]
require = ["serde"]

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
| `[rules.<package>].features` | `"unified"`, `"none"`, `"default"`, `"all"`, or a list of the package's own features | `"unified"` | Which closure this package's `deny`, `require`, `internal` and `leaf` rules read. `"unified"` is the workspace-wide resolve every rule reads by default. Any other value evaluates them on the closure a build of that package under that selection compiles, derived from the same resolve — see *Package-rooted feature selection* below. `direct` and `sealed` read no closure and are unaffected, so a table that declares only those is rejected rather than silently ignoring the key. |
| `[rules.<package>].deny` | list of names or globs | unset | Names that must not appear anywhere in the package's closure. The rule's own package name never matches, so a family glob such as `deny = ["acme-*"]` on `rules.acme-app` does not report `acme-app` itself: a self-match is not a dependency finding. |
| `[rules.<package>].require` | list of names or globs | unset | The dual of `deny`, read on the same closure: every pattern must match at least one name in it, and a failure lists only the patterns that matched nothing. The rule's own package name never satisfies a pattern, so `require` always asks for a dependency. |
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
feature list, a `[rules.<package>].features` value outside the five it accepts, and glob
compilation. The graph-dependent group additionally
requires that every `[rules.<package>]` key names a workspace member and that every `internal` and
`direct` entry names a package present in the resolved graph — these are exact sets, so globs are
not accepted there. It also rejects a `features` list naming a feature the package does not
declare, and any feature-aware rule at all on a document that was not resolved with every
member's features (see below). Both groups exit 2 and point at the offending line and column in
`depgate.toml`.

### Package-rooted feature selection

`cargo metadata` resolves features **once for the whole workspace**: the union over every member,
every dependency kind and every platform. A `[rules.<package>].features` value other than
`"unified"` re-runs Cargo's feature resolution from that one package over the same document, and
the rule is then answered on the edges that activation enables — the question
`cargo tree -p <package> --no-default-features -i <name>` asks, without a second resolve and
without compiling anything.

That narrowing is sound only when every edge the activation could enable is in the document, which
holds exactly when every workspace member was resolved with all of its own features. That is
checked against the document itself: a feature-aware rule on any other document is exit 2 naming
the first member that proves it, because a `deny` rule passing for want of an edge is a false pass.
Resolve with `--all-features` (or `[graph].features = "all"`) to satisfy it.

The two divergences from `cargo tree` that remain are the ones the gap table already records: the
closure keeps every platform's edges, and it is rooted at the package rather than at a build, so
the root's own dev-dependencies — which a bare `cargo tree -p P` includes — stay out. A rule that
narrows reports the selection it used and how many names it removed; the names themselves are in
the JSON report, as `features` and `activation_pruned` on the rule's record.

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
package-rooted view would not have shown. `require` asks a presence question, which points the
other way: a wider graph can only satisfy more patterns, never fewer.

### Rule kinds

| Rule id | Question | Failure direction |
|---|---|---|
| `rules.<pkg>.deny` | Does the closure of `<pkg>` contain a name matching any pattern? | containment — widening only adds findings |
| `rules.<pkg>.require` | Does the closure of `<pkg>` contain a name matching every pattern? | presence — widening can only satisfy more patterns |
| `rules.<pkg>.internal` | Are the internal names in the closure exactly the declared set? | equality — widening can add `+extra` |
| `rules.<pkg>.leaf` | Does the closure contain no internal name? | containment |
| `rules.<pkg>.direct` | Are the resolved depth-one normal dependencies exactly the declared set? | equality on depth-one edges |
| `rules.<pkg>.sealed` | Is `<pkg>` absent from the closure of every other workspace member? | containment |
| `manifest.versions-in-root` | Does any member manifest name a dependency version? | not graph-based |

A `deny` or `require` entry is an exact name unless it contains `*`, `?` or `[`, in which case it is a glob.
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
  feature selection`. The CLI feature flags are inert on that path for the same reason and warn
  the same way, and the JSON report's `features` is `null` rather than a value it cannot know.

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

`features` is the selection the graph was **actually** resolved with, not the file's
`[graph].features`: `"all"` for `--all-features` (or `features = "all"`), the array of specs for
`--features`, `"default"` otherwise. Under `--metadata-json` it is `null` — no Cargo ran, so the
selection that shaped the document is not observable here. The key is always present, so a CI
job can compare it against the feature flags its build used and catch a gate that ran on a
different graph than the release.

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
assumed: `counters.superset_extra_edges` reports how many of the edges a run actually traversed are
platform-conditional (every normal `dep_kinds` entry carries a non-null `target`) or leave a
workspace member through a declaration marked `optional = true`.

| example | packages / members | `superset_extra_edges` |
|---|---:|---:|
| [LemmyNet/lemmy@439734d](https://github.com/LemmyNet/lemmy/tree/439734d) | 833 / 41 | 400 |
| [nervosnetwork/ckb@17d7db5](https://github.com/nervosnetwork/ckb/tree/17d7db5) | 714 / 75 | 0 |
| [uutils/coreutils@6341084](https://github.com/uutils/coreutils/tree/6341084) | 512 / 114 | 358 |

Measured on host **`aarch64-apple-darwin`** (rustc 1.98.0, cargo 1.98.0) against the frozen fixtures
in `tests/fixtures/`. The lemmy and coreutils documents are resolved with `--all-features`, which
their feature-aware rules require, so they carry more packages than a default resolve would. ckb's 0
is a property of its policy rather than of its graph: the counter counts edges a run walked, and a
manifest-only policy declares no graph rule, so it walks none.

The extras come from two families and no others: platform-conditional edges, such as the
`windows-sys` and `wasm-bindgen` families, and optional dependencies that a sibling member unified
on. Both only ever *widen* the closure. Widening is safe for the containment rules — `deny`, `leaf`
and `sealed` cannot lose a finding to it — and it is the measured risk for the equality rules
`internal` and `direct`, which can report an `+extra` name that a host-rooted, package-rooted view
would not have shown, and for `require`, which a widened closure can satisfy on an edge the build
never compiles.

A rule can also decline the widening. `[rules.<package>].features` re-runs Cargo's feature
resolution from that package over the same document and answers the rule on the edges that
activation enables, which is how the two upstream lines that ask about a named feature set — lemmy's
`cargo tree -p lemmy_api_common --no-default-features -i diesel` and coreutils' `--features
feat_os_unix` step — are expressed at all. Two divergences from `cargo tree` survive it, and they
are why the result is still a superset: every platform's edges are kept, and the closure is rooted
at the package rather than at a build, so the root's own dev-dependencies, which a bare
`cargo tree -p P` includes, stay out. What the narrowing removed is reported per rule rather than
counted per run — the human report gives the number, the JSON record lists the names as
`activation_pruned` — so a pass by narrowing never reads as a workspace-wide claim.
[`docs/examples.md`](docs/examples.md) works three real policies through end to end, including the
coreutils case where the same rule fires on the unified closure and passes on the package-rooted
one.

<!-- depgate:exit-codes -->

## Exit codes

Exit codes are 0 for success, 1 for policy violations, 2 for configuration or usage errors, 3 for `cargo metadata` failures, and 4 when the rendered report, `explain` output, or configuration schema could not be written (a closed reader, such as piping through `head`, is not treated as a failure).

| Code | Meaning |
|---:|---|
| `0` | The policy passed. `explain` and `schema` also exit 0 on success, and `explain` exits 0 whether or not the dependency is reachable. |
| `1` | At least one rule failed. `counters.violations` is the number of failed rules. |
| `2` | Configuration or usage error: an invalid command line, or a `depgate.toml` that is missing, unparsable, of the wrong schema, empty of rules, self-contradictory, or naming a package that is not a workspace member or not in the graph. `explain` validates the same file on the same path, so a missing or invalid `depgate.toml` exits 2 there too, and `explain` on an unknown name exits 2 with the same message, as does `explain` on a name that is not a workspace member and is resolved at several versions. |
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

[`docs/examples.md`](docs/examples.md) migrates three real projects' CI policies this way — all
four of lemmy's `cargo tree` assertions among them — each with the upstream lines it replaces quoted
next to the rule, so a reviewer can tell a deliberate policy change from an accidental one.

<!-- depgate:version-blind -->

## Version-blind policies

Rules operate on package **names**, not on `(name, version)` pairs. In the lemmy fixture, 833
resolved packages project onto 704 distinct names, and 83 of those names are resolved at two or more
versions at once. The consequences are worth stating plainly:

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
