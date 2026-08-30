#!/usr/bin/env bash
# AC-P4 baseline: a replica of the shell-and-`cargo tree` dependency policy that
# LemmyNet/lemmy runs in CI, measured against the same pinned checkout the gate is
# measured against so the speedup divides one workspace by itself.
#
# Source: `.woodpecker.yml` at 439734d, the `check_disallowed_dependencies` step,
# L201-204. All four assertions are reproduced verbatim, including L204, which is a
# positive one -- `extism` must stay reachable under `--all-features`.
#
# Read-only and offline: `cargo tree` resolves and prints, it never compiles. The
# `--all-features` line does need every optional crate present in the local registry
# cache, so warm it once with `cargo fetch --locked` in the checkout before measuring;
# offline resolution then costs nothing beyond the read.
#
# scripts/perf.sh exports DEPGATE_PERF_WORKSPACE before invoking this, so AC-P4 never
# divides one workspace's shell replica by another workspace's tool run.
set -euo pipefail
workspace="${DEPGATE_PERF_WORKSPACE:-}"
readonly workspace
if [[ -z "$workspace" ]]; then
    printf 'error: DEPGATE_PERF_WORKSPACE must name the pinned lemmy checkout\n' >&2
    exit 2
fi
printf 'ci-lint-baseline workspace: %s\n' "$workspace" >&2
cd "$workspace"
# The checkout pins `channel = "1.95"` in rust-toolchain.toml, which would make the
# first measurement pay for a toolchain install and then measure a different cargo
# from the one the gate is measured with. Pin the repo's own toolchain instead: the
# four assertions only resolve and print, so the answer does not depend on it.
export CARGO_NET_OFFLINE=true RUSTFLAGS= RUSTUP_TOOLCHAIN="${DEPGATE_PERF_TOOLCHAIN:-1.98.0}"
t() { cargo tree "$@"; }
! t -p lemmy_api_common --no-default-features -i diesel
! t -i aws-lc-sys
! t -i extism
t --all-features -i extism >/dev/null
