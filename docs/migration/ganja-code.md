# Migrating `ganja-code` from shell dependency gates to `cargo-depgate`

`zchee/ganja-code` enforced its crate-layering rules with 180 lines of shell in
`.github/workflows/ci.yaml` — nineteen assertions built out of `cargo tree`, `grep`, `awk`, `jq`
and `test`, each of which paid for its own dependency resolve. This document replaces lines
176–355 of that file, at commit
`153bfb155e59ea27af310d8262933f65cd024daa`, with one policy file and one gate invocation.

The patch is the sibling file [`ganja-code.diff`](ganja-code.diff). It is a proposal, not an
applied change: verify it now, apply it to `ganja-code` only on the maintainer's explicit go.

## What the patch does

1. Deletes the nineteen shell steps at `ci.yaml` L176–355 from the `lint` job.
2. Adds three steps in their place: an `actions/cache` restore of `~/.cargo/bin/cargo-depgate` keyed
   on the pinned `cargo-depgate` commit, a `cargo install --git … --rev … --locked` that runs only
   on a cache miss, and the `dependency policy` step itself, which is one
   `cargo depgate check --config depgate.toml`.
3. Adds `depgate.toml` at the workspace root, carrying the nineteen rules. Its rule body is
   identical to [`tests/fixtures/ganja-code.depgate.toml`](../../tests/fixtures/ganja-code.depgate.toml),
   the configuration this repository's test suite runs against the pinned graph on every build.

The workflow file is byte-identical between `153bfb1` and every descendant checked so far
(`4284525`, `5911d7f`, and the checkout's current head `3f2145a`), so the patch applies to those
revisions unchanged. This repository's live end-to-end test asserts exactly that, by running
`git apply --check` against a real `ganja-code` checkout.

## Verify, then apply

Run these from a `cargo-depgate` checkout, with `WS` naming a `ganja-code` checkout. (This
repository pins stable Rust 1.98.0; if your shell exports nightly-only `RUSTFLAGS`, clear it for
these commands.)

```sh
git -C "$WS" apply --check "$PWD/docs/migration/ganja-code.diff"   # verification stops here
```

That is the whole verification, and it installs nothing: reviewing this patch must not overwrite the
`cargo-depgate` already on your `PATH`.

After maintainer approval:

```sh
git -C "$WS" apply "$PWD/docs/migration/ganja-code.diff"
cargo install --path . --locked
cargo depgate check --manifest-path "$WS/Cargo.toml" --config "$WS/depgate.toml"
```

The last command should print `ok: 19 rules, 0 violations` and exit 0.

The `--rev` in the added workflow step is pinned to
`844f69ad3398bcf8e5128b5629188a38f491e259`, the `cargo-depgate` commit this migration was verified
against, so the step cannot change under the workflow's feet. Move it to a release tag once
`cargo-depgate` publishes one.

**That rev lives on `feat/depgate-v1` today, not on a default branch**, and the consequence is
sharper than staleness: a squash-merge rewrites the commit, and deleting the branch drops the last
ref keeping the original reachable, after which `cargo install --git … --rev 844f69ad…` fails to
find the object and the step breaks outright rather than merely installing an older gate. Refreshing
the rev — to the merge commit, or better to a release tag — is therefore part of merging
`cargo-depgate`, not a follow-up afterwards. Until then, either keep the source branch alive or
apply this patch with a rev that will survive the merge.

The workflow does not rebuild the gate on every run. `actions/cache` restores
`~/.cargo/bin/cargo-depgate` keyed on that exact rev, and `cargo install` runs only on a cache miss
— that is, the first run after the pin changes. Without the cache, every `lint` job would compile
`cargo-depgate` and its dependency tree from source, which is minutes of wall clock in place of
nineteen steps that compiled nothing. Bumping the rev changes the cache key, so a new pin installs
once and is restored thereafter.

`ganja-code` currently builds and gates on default features, so the replacement step passes no
feature flags either. If the build later adopts `--all-features`, `--no-default-features` or
`--features`, the depgate step has to move with it in the same change: the policy is only as
accurate as the resolve it reads.

## Review baseline: nineteen rules against the retired CI lines

This is the table to review the migration against, and to keep afterwards: a later change to
`depgate.toml` should be able to say which retired CI guarantee it changes. `Declared` is the value
in the added `depgate.toml`.

| Rule id | Replaces `ci.yaml@153bfb1` | Declared |
|---|---|---|
| `rules.ganja-core.deny` | L176, L179 | `["ratatui*", "axum*"]` |
| `rules.ganja-tool.internal` | L187 | `["ganja-permission"]` |
| `rules.ganja-core.internal` | L199 | `["ganja-permission", "ganja-protocol", "ganja-provider", "ganja-storage", "ganja-team", "ganja-tool"]` |
| `rules.ganja-team.internal` | L211 | `["ganja-protocol"]` |
| `rules.ganja-provider.internal` | L221 | `["ganja-permission", "ganja-protocol", "ganja-tool"]` |
| `rules.ganja-provider.deny` | L230 | `["ratatui*", "crossterm*", "arboard*"]` |
| `rules.ganja-permission.leaf` | L248 | `true` |
| `rules.ganja-protocol.leaf` | L248 | `true` |
| `rules.ganja-storage.internal` | L258 | `["ganja-permission", "ganja-protocol"]` |
| `rules.ganja-protocol.direct` | L270 | `["serde", "serde_json", "uuid"]` |
| `rules.ganja-client.internal` | L281 | `["ganja-protocol"]` |
| `rules.ganja-teammate-local.internal` | L295 | `["ganja-core", "ganja-permission", "ganja-protocol", "ganja-provider", "ganja-storage", "ganja-team", "ganja-tool"]` |
| `rules.ganja-tui.deny` | L307 | `["axum*"]` |
| `rules.ganja-serve.deny` | L307 | `["ratatui*", "ganja-teammate-local"]` |
| `rules.ganja-client.deny` | L307 | `["axum*"]` |
| `rules.tmux.leaf` | L319 | `true` |
| `rules.tmux.sealed` | L328 | `true` |
| `rules.tmux.direct` | L343 | `["futures", "thiserror", "tokio"]` |
| `manifest.versions-in-root` | L353 | `true` |

That is 7 `internal` rules, 5 `deny` rules, 3 `leaf` rules, 2 `direct` rules, 1 `sealed` rule and
1 manifest rule. All nineteen pass on the pinned graph — 585 packages, 14 workspace members, 1,586
normal edges, 529 distinct names — with default features and, identically, with `--all-features`,
because no `ganja-code` member declares a feature of its own. That graph is committed to this
repository as a hermetic fixture, so the nineteen rules are re-checked on every build here, not
only against a live checkout.

`rules.tmux.sealed` replaces L328's `cargo metadata --no-deps | jq` loop over the member list: the
workspace members come from the same metadata run the rules already read, so no second Cargo
process is needed.

## Coverage boundary: `ganja-cli` and `ganja-testkit`

Neither package carries a rule, deliberately. The retired lines 176–355 never name either of them,
and a migration that added rules for them would be broadening the policy while claiming to
reproduce it. They are still in the picture where the existing rules reach them: `rules.tmux.sealed`
asks about every other workspace member, including these two, and `manifest.versions-in-root`
covers all fourteen member manifests. If they should carry layering rules of their own, that is a
policy decision to make on its own merits, in its own change.

## Three deliberate differences from the shell

The policy reproduces the intent of the retired block; it does not reproduce its accidents. Three
differences are worth a reviewer's attention.

* **Bans are name matches, not substring greps.** `! cargo tree … | grep -q axum` matched
  `axum-core` and anything else containing the substring, including unrelated crates. `deny` entries
  are exact names unless they contain `*`, `?` or `[`, so the third-party bans are written as globs
  (`ratatui*`, `axum*`, `crossterm*`, `arboard*`) to keep the family coverage the grep had, while
  workspace-member names such as `ganja-teammate-local` stay exact. On this graph the substring
  `ratatui` alone also hits seven other crates.
* **The closure is Cargo's workspace-unified, all-platform resolve.** `cargo tree -p M -e normal`
  answers for one member, one host and that member's own features. `cargo depgate` reads the single
  resolve for the whole workspace and traverses every platform, which is a superset: it can include
  a `cfg(windows)` edge or an optional dependency that a sibling member enabled. That direction is
  safe for `deny`, `leaf` and `sealed`, which only gain findings, and is the measured risk for the
  exact-set rules `internal` and `direct`. It is measured, not assumed — see
  [`docs/differential.md`](../differential.md); the extra-internal count is 0 for every member of
  this workspace, which is why all nineteen rules pass unchanged. That count is a measured property
  of this fixture — no member→member edge in it is optional or target-gated — and not a guarantee
  the tool makes: an internal edge declared `optional = true` would be traversed like any other.
* **`manifest.versions-in-root` also catches string versions.** The retired awk at L355 matched only
  lines containing `version =`, so `serde = "1"` in a member manifest slipped through. Both forms
  are flagged now, and `[workspace.dependencies]` in the workspace-owning manifest is never flagged,
  because that is the canonical version table the rule exists to protect.

## After the migration

Assign the new file to the people who own dependency boundaries, so a pull request cannot add a
dependency and relax the rule that would have caught it in the same change:

```text
/depgate.toml @your-org/architecture-owners
```

Keep this document next to the policy. The rule-to-line table is what lets a future reviewer tell a
deliberate policy change from an accidental one.
