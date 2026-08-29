# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Running cargo here: clear RUSTFLAGS first

The shell environment exports a global `RUSTFLAGS` tuned for a nightly toolchain
(`-Z dylib-lto`, `-Z mir-opt-level`, `-Z inline-mir`). This repo pins **stable 1.98.0** via
`rust-toolchain.toml`, so every cargo command fails up front with
`error: the option 'Z' is only accepted on the nightly compiler`. A repo-local
`.cargo/config.toml` cannot fix this — the `RUSTFLAGS` environment variable overrides
`build.rustflags` from any config file. Clear it per invocation instead:

```sh
RUSTFLAGS= cargo --config ~/.config/rust/config.dev.toml clippy --all-targets -- -D warnings
RUSTFLAGS= cargo --config ~/.config/rust/config.dev.toml nextest run
```

`cargo fmt` is unaffected (it does not invoke rustc).

`Cargo.toml` declares an empty `[workspace]` table. That is load-bearing, not boilerplate:
without it cargo's upward workspace search escapes the repo root and picks up an unrelated
manifest in the parent directory, failing before it can build. Do not remove it.

## Architectural constraint: never compile the target

`cargo-depgate` evaluates dependency graphs from `Cargo.lock` and `cargo metadata` output.
"Zero compilation overhead" is a product guarantee, not an optimization — do not shell out
to `cargo build`/`cargo check` or link against the inspected crates to answer a policy
question.

Exit codes are part of the CLI contract because pipelines gate on them. Add new codes
rather than renumbering existing ones.

## Cargo subcommand argv handling

The binary runs two ways: directly as `cargo-depgate …`, and through Cargo as
`cargo depgate …`. In the second form Cargo passes `depgate` as `argv[1]`, so argument
parsing must skip a leading `depgate` token. Omitting this breaks every `cargo depgate`
invocation while direct runs keep working, so it will not surface in a naive test.

## Lints and formatting

`Cargo.toml` carries the lint policy: `unsafe_code` and `unwrap_used` are denied, along
with `todo`, `unimplemented`, and `dbg_macro`; clippy `pedantic` and `missing_docs` are
warnings. `-D warnings` in CI turns all of them into errors, so a placeholder `todo!()`
will not compile.

`rustfmt.toml` sets `max_width = 100` and `use_small_heuristics = "Max"`. Run `cargo fmt`
rather than hand-wrapping — these settings keep struct literals, calls, and match arms on
one line far longer than rustfmt's defaults, so hand-wrapped code gets rejoined.

## Issue tracking

Work is tracked with beads (`br` CLI) in `.beads/`, not GitHub Issues.

- `.beads/issues.jsonl` is the committed source of truth; `.beads/beads.db` and its
  sidecars are local-only and gitignored.
- Never hand-edit `issues.jsonl`. Mutate through `br` so the database and the export stay
  consistent, then commit the regenerated JSONL alongside the code change it describes.

<!-- br-agent-instructions-v1 -->

---

## Beads Workflow Integration

This project uses [beads_rust](https://github.com/Dicklesworthstone/beads_rust) (`br`/`bd`) for issue tracking. Issues are stored in `.beads/` and tracked in git.

### Essential Commands

```bash
# View ready issues (open, unblocked, not deferred)
br ready              # or: bd ready

# List and search
br list --status=open # All open issues
br show <id>          # Full issue details with dependencies
br search "keyword"   # Full-text search

# Create and update
br create --title="..." --description="..." --type=task --priority=2
br update <id> --status=in_progress
br close <id> --reason="Completed"
br close <id1> <id2>  # Close multiple issues at once

# Sync with git
br sync --flush-only  # Export DB to JSONL
br sync --status      # Check sync status
```

### Workflow Pattern

1. **Start**: Run `br ready` to find actionable work
2. **Claim**: Use `br update <id> --status=in_progress`
3. **Work**: Implement the task
4. **Complete**: Use `br close <id>`
5. **Sync**: Always run `br sync --flush-only` at session end

### Key Concepts

- **Dependencies**: Issues can block other issues. `br ready` shows only open, unblocked work.
- **Priority**: P0=critical, P1=high, P2=medium, P3=low, P4=backlog (use numbers 0-4, not words)
- **Types**: task, bug, feature, epic, chore, docs, question
- **Blocking**: `br dep add <issue> <depends-on>` to add dependencies

### Session Protocol

**Before ending any session, run this checklist:**

```bash
git status              # Check what changed
git add <files>         # Stage code changes
br sync --flush-only    # Export beads changes to JSONL
git commit -m "..."     # Commit everything
git push                # Push to remote
```

### Best Practices

- Check `br ready` at session start to find available work
- Update status as you work (in_progress → closed)
- Create new issues with `br create` when you discover tasks
- Use descriptive titles and set appropriate priority/type
- Always sync before ending session

<!-- end-br-agent-instructions -->
