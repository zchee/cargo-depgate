#!/usr/bin/env bash
set -euo pipefail

readonly DEFAULT_COMMIT="153bfb1"
# SHA-256 of the *decompressed* normalised metadata JSON for DEFAULT_COMMIT.
# `--check` compares this rather than the raw .gz bytes: the gzip container is
# compressor-dependent (Apple gzip, GNU gzip and zlib each emit different bytes
# from byte-identical input), so a byte-identity assertion on the .gz would fail
# on a correctly reproduced fixture generated on another platform.
readonly DEFAULT_METADATA_SHA256="19879439714e71022c62d3c60d5bfbe24d43636e04cdccf5423f209dad1fd0c4"
readonly DEFAULT_WS="${HOME}/rust/src/github.com/zchee/ganja-code"

check_only=false
commit="$DEFAULT_COMMIT"
commit_argument_seen=false
for argument in "$@"; do
    case "$argument" in
        --check)
            if [[ "$check_only" == true ]]; then
                printf 'error: --check may only be specified once\n' >&2
                exit 2
            fi
            check_only=true
            ;;
        *)
            if [[ "$argument" == -* ]]; then
                printf 'error: unknown option %s\n' "$argument" >&2
                exit 2
            fi
            if [[ "$commit_argument_seen" == true ]]; then
                printf 'error: expected at most one commit argument\n' >&2
                exit 2
            fi
            commit_argument_seen=true
            commit="$argument"
            ;;
    esac
done

if [[ -z "$commit" || "$commit" == */* ]]; then
    printf 'error: commit must be a non-empty path-free revision\n' >&2
    exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
readonly repo_root
readonly ws="${WS:-$DEFAULT_WS}"
readonly scratch_root="${DEPGATE_FIXTURE_TMPDIR:-${TMPDIR:-/tmp}}"
readonly fixture_root="$repo_root/tests/fixtures/ganja-code-$commit"
readonly config_path="$repo_root/tests/fixtures/ganja-code.depgate.toml"

readonly members=(
    ganja-cli
    ganja-client
    ganja-core
    ganja-permission
    ganja-protocol
    ganja-provider
    ganja-serve
    ganja-storage
    ganja-team
    ganja-teammate-local
    ganja-testkit
    ganja-tool
    ganja-tui
    tmux
)

run_root=""
cleanup() {
    if [[ -n "$run_root" && -d "$run_root" ]]; then
        rm -rf "$run_root"
    fi
}
trap cleanup EXIT

mkdir -p "$scratch_root"
run_root="$(mktemp -d "${scratch_root%/}/cargo-depgate-fixture.XXXXXX")"
readonly scratch="$run_root/ganja-$commit"
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

archive_source() {
    git -C "$ws" archive "$commit" | tar -x -C "$scratch"
}

generate_fixture() {
    local output_root="$1"
    local raw_metadata="$run_root/metadata.raw.json"
    local normalized_metadata="$run_root/metadata.normalized.json"
    local metadata_gzip="$output_root/metadata.json.gz"

    archive_source
    (
        cd "$scratch"
        RUSTFLAGS='' cargo metadata --format-version 1 --locked --offline \
            --manifest-path "$scratch/Cargo.toml" >"$raw_metadata"
    )

    python3 - "$raw_metadata" "$normalized_metadata" "$scratch" "$cargo_home" <<'PY'
from pathlib import Path
import json
import re
import sys

raw_path, normalized_path, scratch, cargo_home = sys.argv[1:]
data = Path(raw_path).read_bytes()
data = data.replace(scratch.encode(), b"/fixture/ganja-code")
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
expected = (585, 14, 585, 1586, 529)
if shape != expected:
    raise SystemExit(f"unexpected metadata shape: expected {expected}, got {shape}")
Path(normalized_path).write_bytes(data)
PY

    mkdir -p "$output_root/crates"
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
    for member in "${members[@]}"; do
        local source_manifest="$scratch/crates/$member/Cargo.toml"
        local destination_manifest="$output_root/crates/$member/Cargo.toml"
        if [[ ! -f "$source_manifest" ]]; then
            printf 'error: workspace member manifest is missing: %s\n' "$source_manifest" >&2
            exit 1
        fi
        mkdir -p "$(dirname "$destination_manifest")"
        cp "$source_manifest" "$destination_manifest"
    done

    raw_size="$(wc -c <"$raw_metadata" | tr -d '[:space:]')"
    normalized_size="$(wc -c <"$normalized_metadata" | tr -d '[:space:]')"
    compressed_size="$(wc -c <"$metadata_gzip" | tr -d '[:space:]')"
    printf 'raw metadata bytes: %s\n' "$raw_size"
    printf 'pre-gzip bytes: %s\n' "$normalized_size"
    printf 'post-gzip bytes: %s\n' "$compressed_size"
    du -sk "$output_root"
}

if [[ "$check_only" == false ]]; then
    generate_fixture "$fixture_root"
    exit 0
fi

generated_root="$run_root/generated/ganja-code-$commit"
mkdir -p "$generated_root"
generate_fixture "$generated_root"

compare_file() {
    local generated="$1"
    local committed="$2"
    if ! cmp -s "$generated" "$committed"; then
        printf 'mismatch: %s differs from %s\n' "$committed" "$generated" >&2
        exit 1
    fi
}

# The gz container is compressor-dependent (Apple gzip, GNU gzip and zlib each
# emit different bytes from identical input), so compare what the tool actually
# reads -- the decompressed JSON -- against the digest recorded next to
# DEFAULT_COMMIT. Two independent checks; each reports itself.
decompressed_sha256() {
    python3 - "$1" <<'SHA_PY'
import gzip
import hashlib
import sys

with gzip.open(sys.argv[1], "rb") as handle:
    print(hashlib.sha256(handle.read()).hexdigest())
SHA_PY
}

generated_sha="$(decompressed_sha256 "$generated_root/metadata.json.gz")"
committed_sha="$(decompressed_sha256 "$fixture_root/metadata.json.gz")"
if [[ "$commit" == "$DEFAULT_COMMIT" ]]; then
    expected_sha="$DEFAULT_METADATA_SHA256"
else
    printf 'note: no digest is recorded for commit %s; the regenerated fixture is compared against the committed one only\n' "$commit" >&2
    expected_sha="$committed_sha"
fi
readonly generated_sha committed_sha expected_sha

digest_failed=0
if [[ "$generated_sha" != "$expected_sha" ]]; then
    printf 'mismatch: regenerated metadata digest %s != expected %s\n' \
        "$generated_sha" "$expected_sha" >&2
    digest_failed=1
fi
if [[ "$committed_sha" != "$expected_sha" ]]; then
    printf 'mismatch: committed %s digest %s != expected %s\n' \
        "$fixture_root/metadata.json.gz" "$committed_sha" "$expected_sha" >&2
    digest_failed=1
fi
if (( digest_failed )); then
    exit 1
fi
printf 'decompressed metadata sha256: %s\n' "$expected_sha"

for member in "${members[@]}"; do
    compare_file \
        "$generated_root/crates/$member/Cargo.toml" \
        "$fixture_root/crates/$member/Cargo.toml"
done

# resolved after the build below from cargo's own target directory (the dev config may redirect it)
cargo_config_args=()
if [[ -f "$HOME/.config/rust/config.dev.toml" ]]; then
    cargo_config_args+=(--config "$HOME/.config/rust/config.dev.toml")
fi
RUSTFLAGS='' cargo "${cargo_config_args[@]}" build --locked
binary_dir="$(RUSTFLAGS='' cargo "${cargo_config_args[@]}" metadata --format-version 1 --no-deps 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"

readonly binary="$binary_dir/debug/cargo-depgate"
metadata_file="$(mktemp "$run_root/metadata.XXXXXX.json")"
gzip -dc "$generated_root/metadata.json.gz" >"$metadata_file"
report_file="$(mktemp "$run_root/report.XXXXXX.json")"
if ! "$binary" check \
    --metadata-json "$metadata_file" \
    --workspace-root "$fixture_root" \
    --config "$config_path" \
    --format json >"$report_file"; then
    printf 'error: generated metadata policy check failed\n' >&2
    cat "$report_file" >&2
    exit 1
fi

python3 - "$report_file" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as report_file:
    report = json.load(report_file)
counters = report.get("counters", {})
expected = {
    "packages": 585,
    "members": 14,
    "normal_edges": 1586,
    "names": 529,
    "rules": 19,
    "violations": 0,
}
actual = {key: counters.get(key) for key in expected}
if actual != expected:
    raise SystemExit(f"counter mismatch: expected {expected}, got {actual}")
print(f"superset_extra_edges: {counters.get('superset_extra_edges')}")
PY
