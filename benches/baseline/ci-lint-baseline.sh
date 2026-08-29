#!/usr/bin/env bash
# Captured from .omc/research/probes/ci-lint-baseline.sh on 2026-08-29 as the AC-P4 ci-lint-baseline replica (3.728 s measured on aarch64-apple-darwin, cargo 1.100.0-nightly, per the plan).
# Exact replica of ganja-code ci.yaml L181-355 dependency steps (read-only, offline).
set -euo pipefail
# Same workspace-resolution chain as scripts/perf.sh, which exports the resolved
# value before invoking this replica: AC-P4 divides one workspace's shell replica
# by the same workspace's tool run, never two different trees.
workspace="${DEPGATE_PERF_WORKSPACE:-${DEPGATE_E2E_WORKSPACE:-$HOME/rust/src/github.com/zchee/ganja-code}}"
readonly workspace
printf 'ci-lint-baseline workspace: %s\n' "$workspace" >&2
cd "$workspace"
export CARGO_NET_OFFLINE=true
t() { cargo tree "$@"; }
! t -p ganja-core -e normal | grep -q ratatui
! t -p ganja-core -e normal | grep -q axum
internal="$(t -p ganja-tool -e normal --prefix none | awk '{print $1}' | grep '^ganja-' | grep -v '^ganja-tool$' | sort -u | tr '\n' ' ')"; test "$internal" = "ganja-permission "
internal="$(t -p ganja-core -e normal --prefix none | awk '{print $1}' | grep '^ganja-' | grep -v '^ganja-core$' | sort -u | tr '\n' ' ')"; test "$internal" = "ganja-permission ganja-protocol ganja-provider ganja-storage ganja-team ganja-tool "
internal="$(t -p ganja-team -e normal --prefix none | awk '{print $1}' | grep '^ganja-' | grep -v '^ganja-team$' | sort -u | tr '\n' ' ')"; test "$internal" = "ganja-protocol "
internal="$(t -p ganja-provider -e normal --prefix none | awk '{print $1}' | grep '^ganja-' | grep -v '^ganja-provider$' | sort -u | tr '\n' ' ')"; test "$internal" = "ganja-permission ganja-protocol ganja-tool "
! t -p ganja-provider -e normal | grep -q ratatui
! t -p ganja-provider -e normal | grep -q crossterm
! t -p ganja-provider -e normal | grep -q arboard
! t -p ganja-permission -e normal | tail -n +2 | grep -q ganja-
! t -p ganja-protocol -e normal | tail -n +2 | grep -q ganja-
internal="$(t -p ganja-storage -e normal --prefix none | awk '{print $1}' | grep '^ganja-' | grep -v '^ganja-storage$' | sort -u | tr '\n' ' ')"; test "$internal" = "ganja-permission ganja-protocol "
external="$(t -p ganja-protocol -e normal --depth 1 --prefix none | tail -n +2 | awk '{print $1}' | sort -u | tr '\n' ' ')"; test "$external" = "serde serde_json uuid "
internal="$(t -p ganja-client -e normal --prefix none | awk '{print $1}' | grep '^ganja-' | grep -v '^ganja-client$' | sort -u | tr '\n' ' ')"; test "$internal" = "ganja-protocol "
internal="$(t -p ganja-teammate-local -e normal --prefix none | awk '{print $1}' | grep '^ganja-' | grep -v '^ganja-teammate-local$' | sort -u | tr '\n' ' ')"; test "$internal" = "ganja-core ganja-permission ganja-protocol ganja-provider ganja-storage ganja-team ganja-tool "
! t -p ganja-tui -e normal | grep -q axum
! t -p ganja-serve -e normal | grep -q ratatui
! t -p ganja-client -e normal | grep -q axum
! t -p ganja-serve -e normal | grep -q ganja-teammate-local
! t -p tmux -e normal | tail -n +2 | grep -q ganja-
for m in $(cargo metadata --no-deps --format-version 1 | jq -r '.packages[].name' | grep -v '^tmux$'); do
  if t -p "$m" -e normal --prefix none | awk '{print $1}' | grep -qx tmux; then echo "$m consumes tmux" >&2; exit 1; fi
done
external="$(t -p tmux -e normal --depth 1 --prefix none | tail -n +2 | awk '{print $1}' | sort -u | tr '\n' ' ')"; test "$external" = "futures thiserror tokio "
awk 'FNR==1{sec=""} /^\[/{sec=$0} sec ~ /dependencies/ && /version[[:space:]]*=/ {print FILENAME ":" FNR ": " $0; bad=1} END{exit bad}' crates/*/Cargo.toml
