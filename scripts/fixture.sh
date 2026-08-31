#!/usr/bin/env bash
set -euo pipefail

# Regenerates one hermetic example fixture, or verifies the committed one.
#
#     scripts/fixture.sh <example> [--check]
#
# Examples: lemmy, ckb, coreutils. Each is a real upstream workspace pinned to
# one commit; the fixture is that commit's `cargo metadata` document with every
# absolute path rewritten to a neutral /fixture/<example> prefix, gzipped, plus
# (where a config enables the manifest rule) the member Cargo.toml files that
# rule re-reads.
#
# Clones live outside the repo, under $DEPGATE_FIXTURE_CLONES. They are kept
# between runs: a regeneration is a `git archive` out of the clone, so the
# network is touched only for the first clone and for the registry index.

usage() {
    printf 'usage: %s <lemmy|ckb|coreutils> [--check]\n' "${BASH_SOURCE[0]##*/}" >&2
}

example=""
check_only=false
for argument in "$@"; do
    case "$argument" in
        --check)
            if [[ "$check_only" == true ]]; then
                printf 'error: --check may only be specified once\n' >&2
                exit 2
            fi
            check_only=true
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        -*)
            printf 'error: unknown option %s\n' "$argument" >&2
            exit 2
            ;;
        *)
            if [[ -n "$example" ]]; then
                printf 'error: expected exactly one example argument\n' >&2
                exit 2
            fi
            example="$argument"
            ;;
    esac
done

if [[ -z "$example" ]]; then
    usage
    exit 2
fi

# Per-example recipe. `metadata_flags` is passed to `cargo metadata`, not to the
# gate: --metadata-json makes the gate's own feature flags inert, because the
# document was already resolved with its own selection. lemmy and coreutils are
# resolved with --all-features because their policies carry per-rule `features`
# keys, and an activation walk narrows soundly only from a document that left no
# member's features off; each rule then names the selection its own CI line asks
# about. ckb has no feature rule and takes the default selection.
#
# `expected_shape` is (packages, members, nodes, normal_edges, names) and
# `expected_counters` is the JSON report's counters that must hold at the pinned
# commit. `expected_exit` is the gate's exit code there: 0 when the modelled
# policy passes, 1 when it reports a violation that is real at that commit.
# The `${arr[@]+"${arr[@]}"}` form below is not decoration: bash before 4.4 -- the
# /bin/bash 3.2 that ships with macOS -- treats "${arr[@]}" on an empty array as an
# unbound variable under `set -u`, and two of the three examples pass no flags.
metadata_flags=()
case "$example" in
    lemmy)
        repo="https://github.com/LemmyNet/lemmy.git"
        commit="439734dd638a2c06a2f907beab7dcf4646e88f86"
        short="439734d"
        toolchain="1.98.0"
        metadata_flags=(--all-features)
        metadata_sha256="8cd7fc3b8c8e789bcd880c8b15ec229e0611b278b0738ea0c081fc7782a84770"
        expected_shape="833 41 833 2950 704"
        expected_counters="packages=833 members=41 normal_edges=2950 names=704 rules=3 violations=0"
        expected_exit=0
        member_manifests=false
        expected_member_count=0
        ;;
    ckb)
        repo="https://github.com/nervosnetwork/ckb.git"
        commit="17d7db5bb423a1b2177e14a132a41d5a91a515f3"
        short="17d7db5"
        toolchain="1.98.0"
        metadata_sha256="4cf9b240311752795f5d2b754c5fc4981e7e668c31ce66733a6045d0479e3e04"
        expected_shape="714 75 714 2351 641"
        expected_counters="packages=714 members=75 normal_edges=2351 names=641 rules=1 violations=1"
        expected_exit=1
        member_manifests=true
        expected_member_count=75
        ;;
    coreutils)
        repo="https://github.com/uutils/coreutils.git"
        commit="63410845ef59674fcf4c5b1a8d02a7337e133de9"
        short="6341084"
        toolchain="1.98.0"
        metadata_flags=(--all-features)
        metadata_sha256="c9bb40830a2ebcc8f21ff57dc10478e6020c157f6f6e08791a42486aae7326fe"
        expected_shape="512 114 512 1493 482"
        expected_counters="packages=512 members=114 normal_edges=1493 names=482 rules=1 violations=0"
        expected_exit=0
        member_manifests=false
        expected_member_count=0
        ;;
    *)
        printf 'error: unknown example %s (expected lemmy, ckb or coreutils)\n' "$example" >&2
        exit 2
        ;;
esac
readonly example repo commit short toolchain metadata_sha256
readonly expected_shape expected_counters expected_exit
readonly member_manifests expected_member_count

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
readonly repo_root
readonly fixture_root="$repo_root/tests/fixtures/$example-$short"
readonly config_path="$repo_root/tests/fixtures/$example.depgate.toml"
readonly scratch_root="${DEPGATE_FIXTURE_TMPDIR:-${TMPDIR:-/tmp}}"
readonly clone_root="${DEPGATE_FIXTURE_CLONES:-${TMPDIR:-/tmp}/cargo-depgate-fixture-clones}"
readonly clone="$clone_root/$example"

run_root=""
cleanup() {
    if [[ -n "$run_root" && -d "$run_root" ]]; then
        rm -rf "$run_root"
    fi
}
trap cleanup EXIT

mkdir -p "$scratch_root" "$clone_root"
run_root="$(mktemp -d "${scratch_root%/}/cargo-depgate-fixture.XXXXXX")"
# Canonicalised for the same reason $CARGO_HOME is below: cargo emits canonical paths, so
# where TMPDIR is a symlink (macOS /var -> /private/var, /tmp -> /private/tmp) the literal
# byte substitution in the normalisation step would no-op and the run would abort at the
# stray-path guard. Fail-closed is correct there, but it makes the default macOS shell
# unusable, and the fix is to compare the paths cargo will actually print.
run_root="$(cd "$run_root" && pwd -P)"
readonly scratch="$run_root/$example-$short"
mkdir -p "$scratch"

if [[ -n "${CARGO_HOME:-}" ]]; then
    cargo_home="$CARGO_HOME"
else
    cargo_home="$HOME/.cargo"
fi
if [[ "$cargo_home" != /* ]]; then
    cargo_home="$PWD/$cargo_home"
fi
cargo_home="$(cd "$cargo_home" && pwd -P)"
readonly cargo_home

ensure_clone() {
    if [[ ! -d "$clone/.git" ]]; then
        printf 'cloning %s into %s\n' "$repo" "$clone" >&2
        git clone --quiet "$repo" "$clone"
    fi
    if ! git -C "$clone" cat-file -e "$commit^{commit}" 2>/dev/null; then
        printf 'fetching %s from %s\n' "$commit" "$repo" >&2
        git -C "$clone" fetch --quiet origin
    fi
    if ! git -C "$clone" cat-file -e "$commit^{commit}" 2>/dev/null; then
        printf 'error: commit %s is not reachable in %s\n' "$commit" "$clone" >&2
        exit 1
    fi
}

generate_fixture() {
    local output_root="$1"
    local raw_metadata="$run_root/metadata.raw.json"
    local normalized_metadata="$run_root/metadata.normalized.json"
    local metadata_gzip="$output_root/metadata.json.gz"

    ensure_clone
    git -C "$clone" archive "$commit" | tar -x -C "$scratch"

    # RUSTUP_TOOLCHAIN pins the toolchain that reads the upstream manifests, so an
    # upstream rust-toolchain.toml never triggers a toolchain install. RUSTFLAGS is
    # cleared for the same reason it is cleared in this repo: the ambient value is
    # nightly-only. The clone is never built -- metadata only.
    (
        cd "$scratch"
        RUSTFLAGS='' RUSTUP_TOOLCHAIN="$toolchain" cargo metadata \
            --format-version 1 --locked ${metadata_flags[@]+"${metadata_flags[@]}"} \
            --manifest-path "$scratch/Cargo.toml" >"$raw_metadata"
    )

    python3 - "$raw_metadata" "$normalized_metadata" "$scratch" "$cargo_home" \
        "/fixture/$example" "$expected_shape" <<'PY'
from pathlib import Path
import json
import re
import sys

raw_path, normalized_path, scratch, cargo_home, prefix, expected_shape = sys.argv[1:]
data = Path(raw_path).read_bytes()
data = data.replace(scratch.encode(), prefix.encode())
data = data.replace(cargo_home.encode(), b"/fixture/cargo-home")
# The two replacements above are raw byte substitutions, so they silently no-op
# when cargo emits a canonicalised path that differs from the literal $scratch /
# $CARGO_HOME string (a symlinked TMPDIR, /tmp -> /private/tmp on macOS). The
# shape assertion below would still pass, committing a fixture full of developer
# paths, so assert the negative before trusting the substitution.
stray = sorted(set(re.findall(rb'"(/(?!fixture/)[^"]*)"', data)))
if stray:
    offenders = ", ".join(item.decode("utf-8", "replace") for item in stray[:5])
    raise SystemExit(f"un-normalised absolute paths survived ({len(stray)}): {offenders}")
decoded = json.loads(data)
packages = decoded.get("packages", [])
members = decoded.get("workspace_members", [])
resolve = decoded.get("resolve") or {}
nodes = resolve.get("nodes", [])
normal_edges = sum(
    any(kind.get("kind") is None for kind in dep.get("dep_kinds", []))
    for node in nodes
    for dep in node.get("deps", [])
)
shape = (len(packages), len(members), len(nodes), normal_edges, len({pkg["name"] for pkg in packages}))
expected = tuple(int(value) for value in expected_shape.split())
if shape != expected:
    raise SystemExit(f"unexpected metadata shape: expected {expected}, got {shape}")
Path(normalized_path).write_bytes(data)
PY

    mkdir -p "$output_root"
    # One compression code path (python's zlib) rather than the system gzip, so
    # the generated container does not vary with the host's gzip implementation.
    # mtime=0 and an empty FNAME field are the `gzip -n` equivalents.
    python3 - "$normalized_metadata" "$metadata_gzip" <<'GZIP_PY'
import gzip
import shutil
import sys

source, destination = sys.argv[1:]
with open(source, "rb") as raw_input, open(destination, "wb") as raw_output:
    with gzip.GzipFile(filename="", mode="wb", compresslevel=9, fileobj=raw_output, mtime=0) as compressed:
        shutil.copyfileobj(raw_input, compressed)
GZIP_PY

    if [[ "$member_manifests" == true ]]; then
        # The member manifest paths are derived from the normalised metadata rather
        # than hardcoded, so a member added upstream cannot be silently dropped; the
        # count is asserted so it cannot silently change either.
        member_list="$run_root/members.txt"
        python3 - "$normalized_metadata" "/fixture/$example" "$expected_member_count" \
            >"$member_list" <<'MEMBERS_PY'
import json
import sys

normalized_path, prefix, expected_count = sys.argv[1:]
with open(normalized_path, encoding="utf-8") as handle:
    decoded = json.load(handle)
manifests = {pkg["id"]: pkg["manifest_path"] for pkg in decoded["packages"]}
relative = []
for member in decoded["workspace_members"]:
    path = manifests[member]
    if not path.startswith(prefix + "/"):
        raise SystemExit(f"member manifest outside the workspace prefix: {path}")
    relative.append(path[len(prefix) + 1:])
if len(relative) != int(expected_count):
    raise SystemExit(f"expected {expected_count} member manifests, got {len(relative)}")
print("\n".join(sorted(relative)))
MEMBERS_PY
        while IFS= read -r relative_manifest; do
            source_manifest="$scratch/$relative_manifest"
            destination_manifest="$output_root/$relative_manifest"
            if [[ ! -f "$source_manifest" ]]; then
                printf 'error: workspace member manifest is missing: %s\n' "$source_manifest" >&2
                exit 1
            fi
            mkdir -p "$(dirname "$destination_manifest")"
            cp "$source_manifest" "$destination_manifest"
        done <"$member_list"
    fi

    raw_size="$(wc -c <"$raw_metadata" | tr -d '[:space:]')"
    normalized_size="$(wc -c <"$normalized_metadata" | tr -d '[:space:]')"
    compressed_size="$(wc -c <"$metadata_gzip" | tr -d '[:space:]')"
    printf 'example: %s@%s\n' "$example" "$short"
    printf 'raw metadata bytes: %s\n' "$raw_size"
    printf 'pre-gzip bytes: %s\n' "$normalized_size"
    printf 'post-gzip bytes: %s\n' "$compressed_size"
    du -sk "$output_root"
}

decompressed_sha256() {
    python3 - "$1" <<'SHA_PY'
import gzip
import hashlib
import sys

with gzip.open(sys.argv[1], "rb") as handle:
    print(hashlib.sha256(handle.read()).hexdigest())
SHA_PY
}

if [[ "$check_only" == false ]]; then
    generate_fixture "$fixture_root"
    printf 'decompressed metadata sha256: %s\n' "$(decompressed_sha256 "$fixture_root/metadata.json.gz")"
    exit 0
fi

generated_root="$run_root/generated/$example-$short"
mkdir -p "$generated_root"
generate_fixture "$generated_root"

# The gz container is compressor-dependent (Apple gzip, GNU gzip and zlib each
# emit different bytes from identical input), so compare what the tool actually
# reads -- the decompressed JSON -- against the digest recorded above. Two
# independent checks; each reports itself.
generated_sha="$(decompressed_sha256 "$generated_root/metadata.json.gz")"
committed_sha="$(decompressed_sha256 "$fixture_root/metadata.json.gz")"
readonly generated_sha committed_sha

digest_failed=0
if [[ "$generated_sha" != "$metadata_sha256" ]]; then
    printf 'mismatch: regenerated metadata digest %s != expected %s\n' \
        "$generated_sha" "$metadata_sha256" >&2
    digest_failed=1
fi
if [[ "$committed_sha" != "$metadata_sha256" ]]; then
    printf 'mismatch: committed %s digest %s != expected %s\n' \
        "$fixture_root/metadata.json.gz" "$committed_sha" "$metadata_sha256" >&2
    digest_failed=1
fi
if ((digest_failed)); then
    exit 1
fi
printf 'decompressed metadata sha256: %s\n' "$metadata_sha256"

if [[ "$member_manifests" == true ]]; then
    # Compare the two relative-path *sets* before comparing bytes. Walking only the
    # generated tree would never report a manifest that is committed under the fixture but
    # no longer produced by regeneration -- a removed member would read as success here and
    # be caught only by CI's tracked-manifest count.
    generated_list="$run_root/generated-manifests.txt"
    committed_list="$run_root/committed-manifests.txt"
    (cd "$generated_root" && find . -name Cargo.toml -type f | sort) >"$generated_list"
    (cd "$fixture_root" && find . -name Cargo.toml -type f | sort) >"$committed_list"
    if ! cmp -s "$committed_list" "$generated_list"; then
        printf 'mismatch: the committed and regenerated member manifest sets differ\n' >&2
        diff -u "$committed_list" "$generated_list" >&2 || true
        exit 1
    fi
    while IFS= read -r relative_manifest; do
        if ! cmp -s "$generated_root/$relative_manifest" "$fixture_root/$relative_manifest"; then
            printf 'mismatch: %s differs from %s\n' \
                "$fixture_root/$relative_manifest" "$generated_root/$relative_manifest" >&2
            exit 1
        fi
    done <"$generated_list"
fi

cargo_config_args=()
if [[ -f "$HOME/.config/rust/config.dev.toml" ]]; then
    cargo_config_args+=(--config "$HOME/.config/rust/config.dev.toml")
fi
RUSTFLAGS='' cargo ${cargo_config_args[@]+"${cargo_config_args[@]}"} build --locked
binary_dir="$(RUSTFLAGS='' cargo ${cargo_config_args[@]+"${cargo_config_args[@]}"} metadata --format-version 1 --no-deps 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
readonly binary="$binary_dir/debug/cargo-depgate"

metadata_file="$(mktemp "$run_root/metadata.XXXXXX.json")"
gzip -dc "$generated_root/metadata.json.gz" >"$metadata_file"
report_file="$(mktemp "$run_root/report.XXXXXX.json")"
set +e
"$binary" check \
    --metadata-json "$metadata_file" \
    --workspace-root "$fixture_root" \
    --config "$config_path" \
    --format json >"$report_file"
actual_exit=$?
set -e
if [[ "$actual_exit" -ne "$expected_exit" ]]; then
    printf 'error: gate exited %s, expected %s\n' "$actual_exit" "$expected_exit" >&2
    cat "$report_file" >&2
    exit 1
fi

python3 - "$report_file" "$expected_counters" <<'PY'
import json
import sys

report_path, expected_spec = sys.argv[1:]
with open(report_path, encoding="utf-8") as report_file:
    report = json.load(report_file)
counters = report.get("counters", {})
expected = {}
for entry in expected_spec.split():
    key, value = entry.split("=")
    expected[key] = int(value)
actual = {key: counters.get(key) for key in expected}
if actual != expected:
    raise SystemExit(f"counter mismatch: expected {expected}, got {actual}")
print(f"superset_extra_edges: {counters.get('superset_extra_edges')}")
print(f"matches: {counters.get('matches')}")
PY
