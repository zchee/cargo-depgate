# Three worked examples

Each example is a real workspace pinned to one commit, with a dependency policy taken from that
project's own CI and distilled into a `depgate.toml`. The `cargo metadata` document is frozen under
`tests/fixtures/`, so every number and quoted line below is reproduced offline on each build.

| example | commit | packages / members | `superset_extra_edges` | exit |
|---|---|---:|---:|---:|
| lemmy | `439734d` | 707 / 41 | 311 | `0` |
| ckb | `17d7db5` | 714 / 75 | 0 | `1` |
| coreutils | `6341084` | 498 / 114 | 329 | `1` |

Each policy runs against its committed fixture with the same two commands, from the repository root:

```sh
example=lemmy dir=tests/fixtures/lemmy-439734d  # ckb-17d7db5, coreutils-6341084
gzip -dc "$dir/metadata.json.gz" > "/tmp/$example-metadata.json"
cargo depgate check --metadata-json "/tmp/$example-metadata.json" \
  --workspace-root "$dir" --config "tests/fixtures/$example.depgate.toml"
```

## lemmy — a workspace-wide ban that holds

`LemmyNet/lemmy@439734d`, `.woodpecker.yml` L200-204:

```yaml
    commands:
      - "! cargo tree -p lemmy_api_common --no-default-features -i diesel"
      - "! cargo tree -i aws-lc-sys"
      - "! cargo tree -i extism"
      - "cargo tree --all-features -i extism"
```

L202 and L203 ask a workspace-wide reachability question, and `lemmy_server` is the binary that
closes over every other member, so one `deny` rule rooted there reproduces both:

```toml
[rules.lemmy_server]
deny = ["aws-lc-sys", "extism"]
```

```text
ok rules.lemmy_server.deny
ok: 1 rules, 0 violations
```

Exit 0 — and the rule that matched nothing is still *listed*, because a green gate that quietly
checks nothing is a failure mode rather than a pass.

The other two lines are outside schema 1, and that is the honest limit. L201 asks whether
`--no-default-features` switches an optional edge off, but the resolve `cargo metadata` emits is
workspace-unified: an optional edge that any member activates stays in it, so one member's feature
selection does not remove it. A `deny` rule therefore fires on
`lemmy_api_common → lemmy_db_schema → diesel (optional)` however the document was generated. L204 is
a *positive* assertion, and there is no `require` rule kind. Both are `cargo-depgate-xqh`.

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

## coreutils — where resolve-level checking ends

`uutils/coreutils@6341084`, `.github/workflows/CICD.yml` L987-994, two lines elided:

```yaml
    # (`cargo tree -i` exits non-zero when the crate is absent from the graph.)
    - name: Verify ariadne is compiled out
      run: |
        if cargo tree -p coreutils --no-default-features --features feat_os_unix -e normal -i ariadne 2>/dev/null; then
          exit 1
        fi
```

`deny = ["ariadne"]` on `rules.coreutils` exits 1:

```text
30 | deny = ["ariadne"]
   |        ^^^^^^^^^^^ 1 match(es)
  coreutils v0.10.0 → uucore v0.10.0 → ariadne v0.6.0 (optional; present via workspace feature unification)
```

coreutils' own CI is green here, and neither result is wrong. `uucore` declares `ariadne` with
`optional = true`, and the resolve `cargo metadata` emits is workspace-unified: an optional edge
that any member activates survives another member's `--no-default-features`. This fixture is
generated with exactly `--no-default-features --features feat_os_unix`, and the edge is there
anyway. The gate walks that workspace-unified superset by design, which makes a
build-level "compiled out" claim the documented boundary of resolve-level checking
(`cargo-depgate-xqh`) — and `superset_extra_edges = 329` measures exactly how far it widens.

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
