# Three worked examples

Each example is a real workspace pinned to one commit, with a dependency policy taken from that
project's own CI and distilled into a `depgate.toml`. The `cargo metadata` document is frozen under
`tests/fixtures/`, so every number and quoted line below is reproduced offline on each build.

| example | commit | packages / members | `superset_extra_edges` | exit |
|---|---|---:|---:|---:|
| lemmy | `439734d` | 833 / 41 | 400 | `0` |
| ckb | `17d7db5` | 714 / 75 | 0 | `1` |
| coreutils | `6341084` | 512 / 114 | 358 | `0` |

lemmy's and coreutils' documents are resolved with `--all-features`, which is what their per-rule
`features` keys require; ckb's takes the default selection because its policy has no such rule.

Each policy runs against its committed fixture with the same two commands, from the repository root:

```sh
example=lemmy dir=tests/fixtures/lemmy-439734d  # ckb-17d7db5, coreutils-6341084
gzip -dc "$dir/metadata.json.gz" > "/tmp/$example-metadata.json"
cargo depgate check --metadata-json "/tmp/$example-metadata.json" \
  --workspace-root "$dir" --config "tests/fixtures/$example.depgate.toml"
```

## lemmy — four `cargo tree` runs become one resolve

`LemmyNet/lemmy@439734d`, `.woodpecker.yml` L200-204:

```yaml
    commands:
      - "! cargo tree -p lemmy_api_common --no-default-features -i diesel"
      - "! cargo tree -i aws-lc-sys"
      - "! cargo tree -i extism"
      - "cargo tree --all-features -i extism"
```

Four resolves, four verdicts, and three different feature selections between them. The policy is
three rules over one document:

```toml
[rules.lemmy_server]
features = "default"
deny = ["aws-lc-sys", "extism"]

[rules.lemmy_api_common]
features = "none"
deny = ["diesel"]

[rules.lemmy_api_utils]
features = "all"
require = ["extism"]
```

```text
ok rules.lemmy_server.deny (features = "default", 115 pruned)
ok rules.lemmy_api_common.deny (features = "none", 404 pruned)
ok rules.lemmy_api_utils.require (features = "all", 31 pruned)
ok: 3 rules, 0 violations
```

Exit 0 — and every rule that matched nothing is still *listed*, because a green gate that quietly
checks nothing is a failure mode rather than a pass.

The first rule is L202 and L203. `cargo tree -i <name>` with no `-p` and no feature flags asks a
workspace-wide question about the default build, and `lemmy_server` is the binary that closes over
the workspace — 39 of the 40 other members are in the closure the default selection activates, and
the fortieth, `lemmy_api_common`, is depended on by nothing here at all, so it adds no package name
that closure does not already carry — so one rule rooted there with `features = "default"` answers
both. The key is
not decoration here: this document is resolved with `--all-features`, so its unified closure *does*
contain `extism`, and the same rule without the key fires with
`lemmy_server → lemmy_api_utils → extism v1.20.0 (optional; present via workspace feature
unification)` — a finding about an edge no default build compiles.

The second rule is L201, the line that had no expression before. `diesel` reaches
`lemmy_api_common` through `lemmy_db_schema`, which declares it `optional = true`; the edge is in
the resolve because a *different* member activates `lemmy_db_schema/full`. `features = "none"` is
`--no-default-features`, and under it `lemmy_api_common` compiles none of that: 404 of the names in
its unified closure are gone, `diesel` among them.

The third is L204, the one *positive* assertion — `extism` must still be reachable with features
on, so the ban above cannot be satisfied by deleting the dependency. `require` is the dual of
`deny` and reads exactly the closure its own `features` key selects; here that is what
`lemmy_api_utils`, the member that declares `extism`, compiles with all of its features enabled.

Each of the three narrowings is derived from the one document already in memory, not from another
resolve, and what each removed is reported per rule: the count above, and the names themselves
under `--format json`, in the `rules[]` array a feature-aware policy brings into the report. All
three rules pass here, so `violations[]` is empty and that array is the only place their evidence
lives — one `{id, kind, passed, features, activation_pruned}` record each.

## ckb — a check that was switched off

`nervosnetwork/ckb@17d7db5` enforces workspace version inheritance with
`devtools/ci/check-cargotoml.sh`, a 158-line shell program run from `Makefile` L226-227 and three
`.github/workflows/ci_quick_checks_*.yaml` jobs. Its version check does not run (L145-149):

```sh
function main() {
  echo "[BEGIN] Checking Cargo.toml ..."
  check_package_name
  # check_version
  check_license
```

The equivalent policy is one key, `[manifest] versions-in-root = true`. It exits 1 with **24
findings** — 24 `violations[]` entries under one failed rule, each anchored at the version span that
caused it:

```text
error[manifest.versions-in-root]: dependency policy violation
  --> resource/Cargo.toml:14:7
   |
14 | phf = "= 0.8.0" # ckb-resource's build script need this, and cargo shear think ckb don't need this
   |       ^^^^^^^^^ dependencies phf = "= 0.8.0"
```

The findings are genuine drift: 19 sit in member `target.'cfg(…)'.dependencies` tables never
migrated to `workspace = true`, 3 in the root package's own target-gated tables, and 2 in plain
`[dependencies]` — the `phf` entry above and `axum-streams = "0.21"` in `rpc/Cargo.toml`. Two
caveats belong with them. The mapping is wider than the original question: `check_version` compared
each member's own `package.version` and its intra-workspace dependency versions against the
workspace version, while `versions-in-root` asks whether any entry names a version at all. And since
the check is commented out, nothing has been enforced here — the 24 entries are the residue a
disabled check leaves behind.

## coreutils — the same rule, two closures

`uutils/coreutils@6341084`, `.github/workflows/CICD.yml` L987-994, two lines elided:

```yaml
    # (`cargo tree -i` exits non-zero when the crate is absent from the graph.)
    - name: Verify ariadne is compiled out
      run: |
        if cargo tree -p coreutils --no-default-features --features feat_os_unix -e normal -i ariadne 2>/dev/null; then
          exit 1
        fi
```

The flag pair is the whole assertion, and a list-valued `features` key is resolved the same way —
`--no-default-features --features …`:

```toml
[rules.coreutils]
features = ["feat_os_unix"]
deny = ["ariadne"]
```

```text
ok rules.coreutils.deny (features = ["feat_os_unix"], 43 pruned)
ok: 1 rules, 0 violations
```

Drop the one `features` line and the same rule exits 1, on the same document, with the witness the
gate has always reported for it:

```text
  coreutils v0.10.0 → uucore v0.10.0 → ariadne v0.6.0 (optional; present via workspace feature unification)
```

Neither result is wrong; they answer different questions. `uucore` declares `ariadne` optional
behind its `diagnostics` feature, and the resolve `cargo metadata` emits is unified over every
member, every dependency kind and every platform, so the edge is in it twice over: this document
was generated with `--all-features`, and even the flag pair upstream documents would leave it
there, because `uu_csplit` and `uu_numfmt` request `uucore/diagnostics` from their
`[dev-dependencies]` while the upstream command carries `-e normal`. The unified rule reports that
edge, correctly. The feature-aware rule starts from `coreutils`, follows normal edges only, and
never reaches it — which is the build-level claim the CI step is making.

`superset_extra_edges = 358` counts the platform-conditional and member-optional edges the run
traversed, and it is non-zero even though this policy's one rule narrows: measuring what a
selection pruned means walking the unified closure too, and that walk is where the 358 are counted.
The 43 names are the other half of the same measurement — what the narrowing actually removed.

## Why the shell form is fragile

Each policy above is a `cargo tree` invocation whose verdict is an exit code, and the upstream
comments already record the trap: `cargo tree -i` "exits non-zero when the crate is absent from the
graph" (CICD.yml L987) — the same code it returns when cargo itself failed — and the `2>/dev/null`
on L991 hides the difference, so an unresolvable manifest reads as a pass. `-e normal` there filters
what is *displayed*, not what is reachable; ckb's `cargo tree --depth 1 --prefix none`
(check-cargotoml.sh L78) takes the default edge set instead, so dev- and build-dependencies land in
a list meant for normal ones. That output is then split on whitespace by `awk` (L84-86), so anything
that changes how cargo renders a line — colour under `CARGO_TERM_COLOR=always`, the `(*)`
de-duplication marker, a ` (proc-macro)` suffix — moves the fields; the `$?` test guarding the call
(L79) is unreachable under the `set -euo pipefail` on L3; and the script needs GNU `sed` and `grep`
installed before it runs on macOS at all (L8-24). Every assertion also pays for its own resolve.

Against that: one `cargo metadata` for the whole workspace, typed counters, a shortest witness path
per finding, a span pointing at the line that caused it, and exit codes that separate a passing
policy from a failing one from a graph that could not be produced.

## Reproducing

`cargo nextest run --locked --all-features -E 'binary(cli_tests)'` asserts every number above from
the committed fixtures, offline. To rebuild one from upstream instead — clone the pinned commit,
re-run `cargo metadata`, and compare the digest, member manifests, counters and exit code against
what is committed:

```sh
DEPGATE_FIXTURE_CLONES=~/.cache/depgate-fixture-clones scripts/fixture.sh ckb --check
```

## Re-pinning a fixture

Every number above is a claim about one upstream commit, so a legitimate upstream change — a
dependency added, a version finally inherited, a member renamed — moves the pin and moves the
assertions with it; none of them is relaxed in place. The procedure is the numbered comment block
at the top of `scripts/fixture.sh`, and that is its only copy: bump the recipe's `commit` and
`short`, rename the fixture directory, regenerate, copy the printed digest, shape, counters and
exit code back into the recipe, update the callers the block lists, refresh the `insta` snapshots,
and finish with `scripts/fixture.sh <example> --check`. Record the old and the new commit in the
commit message together with what changed upstream to require the move: a message that says only
"update fixture" leaves the next reader unable to tell an upstream change from a policy regression.
