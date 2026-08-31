#!/usr/bin/env bash
set -euo pipefail

# Performance gates from cargo-depgate-plan.md §3.7. The script deliberately
# measures the already-built release binary on the committed hermetic fixture;
# cargo metadata is never part of the AC-P1/P2 own-work timing.
# Set DEPGATE_PERF_LIVE=1 to run the live AC-P3/P4 checks as well; they need the
# pinned lemmy checkout, which is materialised from $DEPGATE_FIXTURE_CLONES (the
# same clone directory scripts/fixture.sh uses) unless DEPGATE_PERF_WORKSPACE
# names one. CI keeps this opt-in branch disabled.
# AC-P5 measures the hermetic command and therefore always runs.
#
# The hermetic fixture is lemmy, the largest of the three committed examples
# (4,073,164 decompressed bytes), so the gates describe the worst case that ships.
#
# The measured policies are written by this script rather than taken from
# tests/fixtures/lemmy.depgate.toml: a gate has to bound the tool's own work on a
# fixed workload, and the committed policy is free to change shape (P4 turned it
# into three feature-aware rules, which silently invalidated three bounds derived
# against one unified rule). Both policies below are the same one deny rule rooted
# at the member that closes over the workspace, matching a name the graph does not
# carry; they differ only in the `features` key, so the difference between their
# own-work medians is the cost of the feature-aware path and nothing else. The live
# AC-P3/AC-P4 section does use the committed policy, because what it measures is
# precisely the shipped policy against the shell it replaces.
#
# On --profile ci, and only there, a measurement that reports a FAIL is re-run once
# at the same bounds and the second result stands; see run_measurement below.

# The script's real standard error, captured before run_measurement starts folding a
# measurement's stderr into its log with `2>&1` and then printing that log as the gate
# report. Two kinds of diagnostic go here. One has to reach the terminal rather than the
# gate report -- hyperfine's failure dump, for one. The other has to stay out of the
# report entirely: the feature-aware peak RSS figure, which the AC-P5 ruling deliberately
# does not gate on (plan §6, 2026-08-31). Neither can become something that looks like a
# gate line, so standard output keeps exactly one line per gate.
exec 3>&2

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
readonly repo_root
readonly fixture_root="$repo_root/tests/fixtures/lemmy-439734d"
readonly config_path="$repo_root/tests/fixtures/lemmy.depgate.toml"
# The pinned commit the live AC-P3/P4 workspace is materialised at; it must match
# the commit scripts/fixture.sh froze the hermetic document from.
readonly live_commit="439734dd638a2c06a2f907beab7dcf4646e88f86"
# The checkout pins `channel = "1.95"`; using this repo's toolchain instead keeps the
# AC-P3 `cargo metadata` and the AC-P4 replica on the same cargo the gate ships with,
# and spares the first live run a toolchain install.
readonly live_toolchain="${DEPGATE_PERF_TOOLCHAIN:-1.98.0}"
# `--profile dev|ci` (plan §10) or DEPGATE_BENCH_PROFILE; the flag wins.
profile="${DEPGATE_BENCH_PROFILE:-dev}"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --profile)
            [[ $# -ge 2 ]] || { printf 'error: --profile needs a value\n' >&2; exit 2; }
            profile="$2"
            shift 2
            ;;
        --profile=*)
            profile="${1#--profile=}"
            shift
            ;;
        *)
            printf 'error: unknown argument %s\n' "$1" >&2
            exit 2
            ;;
    esac
done
readonly profile
readonly runs="${DEPGATE_PERF_RUNS:-10}"
readonly live="${DEPGATE_PERF_LIVE:-0}"

# AC-P2 and AC-P5 keep the bounds they shipped with, re-measured on the regenerated
# 4,073,164-byte lemmy document (aarch64-apple-darwin, release, 10 runs after 3 discarded):
# own-work total median 5.06-5.19 ms against 8.0 and peak RSS 2.23-2.26x the document
# against 3x, both tight across sessions. The ci own-work bound keeps the 1.8x dev-to-ci
# ratio it has always had, for shared, noisier runners.
#
# The AC-P1 dev bound moves from 13.0 to 18.0 (plan §6, the 2026-09-01 re-derivation). Its
# hyperfine mean over seven sessions on this document spans 7.48-11.58 ms and individual
# runs reached 13.87, so 13.0 sat inside the measurement's own spread: the worst session
# mean left 11%, and a single unlucky run could clear the bound on an unchanged build. Own
# work over those same sessions barely moves, so what varies is process spawn rather than
# anything this tool does, and no bound on a wall-clock mean can be tighter than the host's
# spawn noise. 18.0 is 1.55x the worst session mean -- the same rationale as AC-P6b's move
# below. The 13.0 was derived when AC-P1 read 7.68-9.19 ms on the 3,526,964-byte fixture,
# which this document has since outgrown by 15%. The ci bound stays 32.0 and is not
# re-derived, so AC-P1's dev-to-ci ratio narrows from 2.5x to 1.78x.
#
# The feature-aware bound is new (plan AC 14). Its measured own-work median on the same
# document, with a rule whose activation reaches the whole graph, is 11.703 ms; the dev
# bound is twice that, the ratio AC-P2's 8.0 has to its own 4.07 ms measurement, and the
# ci bound applies the same 1.8x AC-P2 uses. This script reads 11.24-11.81 ms for the same
# rule across three sessions, so the bound it enforces is the one the plan §6 ruling
# derived, not a wider one fitted to a friendlier run.
#
# The AC-P6b dev bound moves from 20.0 to 30.0 (plan §6, the 2026-08-31 re-derivation).
# The synthetic 20k own-work measured 19.221 ms, spread 18.81-19.59, which is 2-4% under
# the old bound and would flake on any shared host. Against v0.1.0's 15.270/17.521 and the
# wave-A tip's 17.607 the drift is at or under this measurement's ~2 ms noise floor, so it
# is not attributable to any one change; 30.0 is 1.56x the measurement. The ci bound was
# already 60.0 and keeps that headroom, so only the dev arm moves.
case "$profile" in
    dev)
        readonly p1_bound="18.0"
        readonly own_work_bound="8.0"
        readonly feature_own_work_bound="23.5"
        readonly parse_rate_bound="0.8"
        readonly synthetic_own_work_bound="30.0"
        ;;
    ci)
        readonly p1_bound="32.0"
        readonly own_work_bound="14.5"
        readonly feature_own_work_bound="42.5"
        readonly parse_rate_bound="0.4"
        readonly synthetic_own_work_bound="60.0"
        ;;
    *)
        printf 'error: DEPGATE_BENCH_PROFILE must be dev or ci (got %s)\n' "$profile" >&2
        exit 2
        ;;
esac

case "$live" in
    0|1) ;;
    *)
        printf 'error: DEPGATE_PERF_LIVE must be 0 or 1 (got %s)\n' "$live" >&2
        exit 2
        ;;
esac

case "$runs" in
    ''|*[!0-9]*)
        printf 'error: DEPGATE_PERF_RUNS must be a positive integer (got %s)\n' "$runs" >&2
        exit 2
        ;;
esac
if (( runs < 1 )); then
    printf 'error: DEPGATE_PERF_RUNS must be a positive integer (got %s)\n' "$runs" >&2
    exit 2
fi

target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
if [[ "$target_dir" != /* ]]; then
    target_dir="$repo_root/$target_dir"
fi
readonly target_dir
export CARGO_TARGET_DIR="$target_dir"
export RUSTFLAGS=
cd "$repo_root"

work_root="$(mktemp -d "${TMPDIR:-/tmp}/cargo-depgate-perf.XXXXXX")"
readonly work_root
cleanup() {
    rm -rf "$work_root"
}
trap cleanup EXIT

readonly metadata_path="$work_root/metadata.json"
readonly build_log="$work_root/build.log"
readonly measure_log="$work_root/measure.log"
readonly bench_log="$work_root/bench.log"
readonly synthetic_log="$work_root/synthetic.log"
readonly p1_log="$work_root/p1.log"
readonly p1_json="$work_root/p1.json"
gunzip -c "$fixture_root/metadata.json.gz" >"$metadata_path"

# The two measured policies (see the header note). Both are one `deny` rule rooted at
# lemmy_server, the binary that closes over every other member, naming a package the graph
# does not carry: every run therefore walks the whole closure and none of them reaches the
# violation-reporting path, so the medians measure traversal and nothing else. Composing
# them from one body makes "they differ only in the `features` key" structural rather than
# a claim a later edit can quietly break, which is what AC 14's derivation rests on.
# `versions-in-root = false` follows the committed policy: lemmy does not enforce version
# inheritance and this fixture ships no member manifests, so leaving the manifest rule on
# would time a workload that does not exist here.
readonly default_policy="$work_root/policy-default.toml"
readonly feature_policy="$work_root/policy-feature.toml"
write_policy() {
    # $1 is the destination; $2 is the rule's `features` line, empty for the unified default.
    {
        printf 'schema = 1\n\n[manifest]\nversions-in-root = false\n\n[rules.lemmy_server]\n'
        if [[ -n "$2" ]]; then
            printf '%s\n' "$2"
        fi
        printf 'deny = ["depgate-perf-absent-package"]\n'
    } >"$1"
}
write_policy "$default_policy" ""
write_policy "$feature_policy" 'features = "all"'

# Retry policy, --profile ci only. The ci bounds are met on shared GitHub runners
# with little room to spare -- AC-P6b at 33.7 of 60 ms is the thinnest margin at
# the bounds in force here -- where a neighbouring workload on the same host can
# push an unchanged build past a bound. (Own-work read 5.94 of 9 ms in the same
# run, but that was the pre-lemmy fixture against the then-current 9 ms bound;
# 867e7c2 moved own_work_bound to 14.5 when the fixture grew to lemmy, so own-work
# is no longer one of the thin margins.) Re-running the failing measurement once,
# against the same bounds, separates a flake from a regression: a regression fails
# twice. No bound is widened here -- moving one requires a recorded re-derivation
# in plan §13 -- and the dev profile stays single-shot, where a FAIL is worth
# looking at directly. AC-P2-feature-own-work is produced by the same measurement
# as AC-P1 and AC-P2, so it is retried on the same terms as its siblings.
#
# $1 names the measurement, $2 is its log, $3 is the function that performs it and
# writes its report to standard output. Only the surviving attempt reaches standard
# output, so the one-line-per-gate contract holds; a discarded first attempt is
# echoed to standard error with every line marked, so both attempts are in the log
# and a flake cannot be mistaken for a clean run. The measurement functions are
# called with errexit disabled: inside them a failing step is a result to report,
# not a reason to abort.
run_measurement() {
    local name="$1"
    local log="$2"
    local measurement="$3"
    local status
    "$measurement" >"$log" 2>&1
    status=$?
    if [[ "$profile" == "ci" ]] && { (( status != 0 )) || grep -q $'\tFAIL$' "$log"; }; then
        {
            printf 'retry: %s attempt 1 reported a FAIL; re-running it once at the same bounds\n' \
                "$name"
            sed 's/^/retry attempt 1: /' "$log"
        } >&2
        "$measurement" >"$log" 2>&1
        status=$?
        printf 'retry: %s attempt 2 follows on stdout and is the result that stands\n' "$name" >&2
    fi
    cat "$log"
    return "$status"
}

if ! RUSTFLAGS='' cargo build --release --locked >"$build_log" 2>&1; then
    cat "$build_log" >&2
    printf 'AC-P1-%s\t0.000\t%s\tFAIL\n' "$profile" "$p1_bound"
    printf 'AC-P2-own-work\t0.000\t%s\tFAIL\n' "$own_work_bound"
    printf 'AC-P2-feature-own-work\t0.000\t%s\tFAIL\n' "$feature_own_work_bound"
    printf 'AC-P6a-parse-gbps\t0.000\t%s\tFAIL\n' "$parse_rate_bound"
    printf 'AC-P6b-own-work\t0.000\t%s\tFAIL\n' "$synthetic_own_work_bound"
    exit 1
fi

readonly binary="$target_dir/release/cargo-depgate"
if [[ ! -x "$binary" ]]; then
    printf 'error: release binary was not produced at %s\n' "$binary" >&2
    printf 'AC-P1-%s\t0.000\t%s\tFAIL\n' "$profile" "$p1_bound"
    printf 'AC-P2-own-work\t0.000\t%s\tFAIL\n' "$own_work_bound"
    printf 'AC-P2-feature-own-work\t0.000\t%s\tFAIL\n' "$feature_own_work_bound"
    printf 'AC-P6a-parse-gbps\t0.000\t%s\tFAIL\n' "$parse_rate_bound"
    printf 'AC-P6b-own-work\t0.000\t%s\tFAIL\n' "$synthetic_own_work_bound"
    exit 1
fi

# hyperfine -N splits the command string itself, so build it from a quoted array:
# an unquoted concatenation mis-parses as soon as a path contains a space.
p1_argv=(
    "$binary" depgate check
    --metadata-json "$metadata_path"
    --workspace-root "$fixture_root"
    --config "$default_policy"
    --format json
)
p1_command="$(printf '%q ' "${p1_argv[@]}")"
p1_command="${p1_command% }"
readonly p1_command
# One attempt at the AC-P1 wall-clock gate and the two own-work gates: hyperfine first,
# then the medians of the tool's own --timings lines, for the default policy and for the
# feature-aware one. Both gate lines and the per-phase diagnostics go to standard output,
# so run_measurement can hold on to the attempt. The AC-P6 lines are the caller's to
# print: they belong to a measurement this one may never reach.
measure_wall_and_own_work() {
    local p1_mean
    if ! hyperfine -N --warmup 3 --runs "$runs" --export-json "$p1_json" "$p1_command" >"$p1_log" 2>&1; then
        cat "$p1_log" >&3
        printf 'AC-P1-%s\t0.000\t%s\tFAIL\n' "$profile" "$p1_bound"
        printf 'AC-P2-own-work\t0.000\t%s\tFAIL\n' "$own_work_bound"
        printf 'AC-P2-feature-own-work\t0.000\t%s\tFAIL\n' "$feature_own_work_bound"
        return 1
    fi
    if ! p1_mean="$(python3 - "$p1_json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as report:
    results = json.load(report).get("results", [])
if len(results) != 1 or not isinstance(results[0].get("mean"), (int, float)):
    raise SystemExit("hyperfine JSON did not contain exactly one numeric mean")
print(float(results[0]["mean"]) * 1_000.0)
PY
)"; then
        cat "$p1_log" >&3
        printf 'error: unable to parse hyperfine AC-P1 output\n' >&3
        printf 'AC-P1-%s\t0.000\t%s\tFAIL\n' "$profile" "$p1_bound"
        printf 'AC-P2-own-work\t0.000\t%s\tFAIL\n' "$own_work_bound"
        printf 'AC-P2-feature-own-work\t0.000\t%s\tFAIL\n' "$feature_own_work_bound"
        return 1
    fi

    python3 - "$binary" "$metadata_path" "$fixture_root" "$default_policy" "$feature_policy" \
        "$runs" "$profile" "$p1_mean" "$p1_bound" "$own_work_bound" "$feature_own_work_bound" \
        2>&1 <<'PY'
import os
import subprocess
import sys
import time

(
    binary,
    metadata,
    workspace_root,
    default_policy,
    feature_policy,
    runs,
    profile,
    p1_mean,
    p1_bound,
    own_work_bound,
    feature_own_work_bound,
) = sys.argv[1:]
runs = int(runs)
p1_mean = float(p1_mean)
p1_bound = float(p1_bound)
own_work_bound = float(own_work_bound)
feature_own_work_bound = float(feature_own_work_bound)
PHASES = ("read", "parse", "graph", "traversals", "evaluate", "manifest", "report", "total")
env = dict(os.environ)
env["RUSTFLAGS"] = ""

def command_for(policy):
    return [
        binary,
        "depgate",
        "check",
        "--metadata-json",
        metadata,
        "--workspace-root",
        workspace_root,
        "--config",
        policy,
        "--format",
        "json",
    ]

def invoke(policy, extra):
    started = time.perf_counter_ns()
    result = subprocess.run(
        command_for(policy) + extra,
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        check=False,
    )
    elapsed = (time.perf_counter_ns() - started) / 1_000_000.0
    if result.returncode != 0:
        diagnostic = result.stderr.decode("utf-8", "replace").strip()
        raise RuntimeError(f"{result.returncode}: {diagnostic}")
    return elapsed, result.stderr.decode("utf-8", "replace")

def median(values):
    values = sorted(values)
    middle = len(values) // 2
    if len(values) % 2:
        return values[middle]
    return (values[middle - 1] + values[middle]) / 2.0

def phase_medians(policy):
    # Keep the tool's own phase readings separate from the hyperfine wall-clock
    # gate above: discard three cold-start runs, then collect the requested
    # steady-state timing lines for the median diagnostics.
    phases = {name: [] for name in PHASES}
    for _ in range(3):
        invoke(policy, ["--timings"])
    for _ in range(runs):
        _elapsed, stderr = invoke(policy, ["--timings"])
        observed = {}
        for line in stderr.splitlines():
            fields = line.split("\t")
            if len(fields) != 2 or fields[0] not in phases:
                continue
            try:
                observed[fields[0]] = float(fields[1])
            except ValueError:
                continue
        missing = sorted(set(phases) - set(observed))
        if missing:
            raise RuntimeError(f"timings missing phases: {', '.join(missing)}")
        for name, value in observed.items():
            phases[name].append(value)
    return {name: median(values) for name, values in phases.items()}

try:
    # One policy is measured to completion before the other starts, never interleaved,
    # so neither run's cache state is a function of the other's sample order.
    default_medians = phase_medians(default_policy)
    feature_medians = phase_medians(feature_policy)

    print(f"AC-P1-{profile}\t{p1_mean:.3f}\t{p1_bound:.3f}\t{'PASS' if p1_mean <= p1_bound else 'FAIL'}")
    total = default_medians["total"]
    print(
        f"AC-P2-own-work\t{total:.3f}\t{own_work_bound:.3f}\t"
        f"{'PASS' if total <= own_work_bound else 'FAIL'}"
    )
    # AC 14's second gate: the same rule under `features = "all"`, whose activation walk
    # reaches the whole graph. It is a separate line rather than a wider AC-P2 because a
    # policy that carries no `features` key must keep paying the unchanged 8 ms bound.
    feature_total = feature_medians["total"]
    print(
        f"AC-P2-feature-own-work\t{feature_total:.3f}\t{feature_own_work_bound:.3f}\t"
        f"{'PASS' if feature_total <= feature_own_work_bound else 'FAIL'}"
    )
    # Diagnostics, not gates, and read off the default path only -- the feature-aware run
    # spends several milliseconds in `traversals` by design, which is what its own gate
    # above bounds. Each budget is roughly twice the measured lemmy median, with the two
    # sub-0.05 ms phases floored where the timer noise lives rather than at 2x. `manifest`
    # measures 0 here because the measured policy turns the manifest rule off; the budget
    # is kept for the shape ckb's policy exercises.
    budgets = {
        "read": 0.6 if profile == "dev" else 1.2,
        "parse": 6.2 if profile == "dev" else 12.4,
        "graph": 1.0 if profile == "dev" else 2.0,
        "traversals": 0.05 if profile == "dev" else 0.1,
        "evaluate": 0.1 if profile == "dev" else 0.2,
        "manifest": 2.0 if profile == "dev" else 3.0,
        "report": 0.1 if profile == "dev" else 0.2,
    }
    for name, budget in budgets.items():
        value = default_medians[name]
        print(f"{name}\t{value:.3f}\t{budget:.3f}\t{'OK' if value <= budget else 'WARN'}")
except Exception as error:
    print(f"AC-P1-{profile}\t{p1_mean:.3f}\t{p1_bound:.3f}\t{'PASS' if p1_mean <= p1_bound else 'FAIL'}")
    print(f"AC-P2-own-work\t0.000\t{own_work_bound:.3f}\tFAIL")
    print(f"AC-P2-feature-own-work\t0.000\t{feature_own_work_bound:.3f}\tFAIL")
    print(f"measurement-error\t{error}", file=sys.stderr)
    sys.exit(1)
PY
}

set +e
run_measurement "AC-P1/AC-P2" "$measure_log" measure_wall_and_own_work
measure_status=$?
set -e
if (( measure_status != 0 )); then
    printf 'AC-P6a-parse-gbps\t0.000\t%s\tFAIL\n' "$parse_rate_bound"
    printf 'AC-P6b-own-work\t0.000\t%s\tFAIL\n' "$synthetic_own_work_bound"
    exit 1
fi

# One attempt at the two synthetic gates, AC-P6a (parse throughput) and AC-P6b
# (non-parse own work). The bench log is part of the attempt, so it is printed from
# here. Called with errexit disabled by run_measurement.
measure_synthetic() {
    local bench_status parse_rate own_work parse_pass own_pass parse_result own_result
    DEPGATE_BENCH_PROFILE="$profile" RUSTFLAGS='' cargo \
        bench --locked --bench pipeline >"$bench_log" 2>&1
    bench_status=$?
    cat "$bench_log"

    parse_rate="$(sed -nE 's/.*achieved parse GB\/s at 1k, 5k, 20k: [^,]+, [^,]+, ([0-9.]+).*/\1/p' "$bench_log" | tail -n 1)"
    own_work="$(sed -nE 's/.*synthetic non-parse own-work at 20k: ([0-9.]+) ms.*/\1/p' "$bench_log" | tail -n 1)"
    if [[ -z "$parse_rate" ]]; then
        parse_rate="0.000"
    fi
    if [[ -z "$own_work" ]]; then
        own_work="0.000"
    fi

    parse_pass=0
    own_pass=0
    if [[ "$bench_status" -eq 0 ]] && awk -v value="$parse_rate" -v bound="$parse_rate_bound" 'BEGIN { exit !(value >= bound) }'; then
        parse_pass=1
    fi
    if [[ "$bench_status" -eq 0 ]] && awk -v value="$own_work" -v bound="$synthetic_own_work_bound" 'BEGIN { exit !(value <= bound) }'; then
        own_pass=1
    fi
    if (( parse_pass )); then parse_result=PASS; else parse_result=FAIL; fi
    if (( own_pass )); then own_result=PASS; else own_result=FAIL; fi
    printf 'AC-P6a-parse-gbps\t%s\t%s\t%s\n' "$parse_rate" "$parse_rate_bound" "$parse_result"
    printf 'AC-P6b-own-work\t%s\t%s\t%s\n' "$own_work" "$synthetic_own_work_bound" "$own_result"
    if (( parse_pass && own_pass )); then
        return 0
    fi
    return 1
}

set +e
run_measurement AC-P6 "$synthetic_log" measure_synthetic
synthetic_status=$?
set -e

gate_failed=0
if grep -q $'\tFAIL$' "$measure_log" || (( synthetic_status != 0 )); then
    gate_failed=1
fi

# AC-P5 measures the *hermetic* command, so it runs unconditionally rather than
# behind DEPGATE_PERF_LIVE: it needs no checkout and CI can enforce it. The
# measurement requires /usr/bin/time (the `time` package on Debian/Ubuntu, not
# the shell builtin) and fails closed when it is absent.
readonly rss_log="$work_root/rss.log"
# One attempt at AC-P5. Called with errexit disabled by run_measurement.
measure_rss() {
    python3 - "$binary" "$metadata_path" "$fixture_root" "$default_policy" "$feature_policy" \
        2>&1 <<'PY'
import os
import re
import subprocess
import sys

binary, metadata, workspace_root, default_policy, feature_policy = sys.argv[1:]
env = dict(os.environ)
env["RUSTFLAGS"] = ""

def hermetic_command(policy):
    return [
        binary,
        "depgate",
        "check",
        "--metadata-json",
        metadata,
        "--workspace-root",
        workspace_root,
        "--config",
        policy,
        "--format",
        "json",
    ]

def note(message):
    """Write a diagnostic to the script's fd 3, which is its real standard error.

    Everything this process writes to stdout or stderr is folded into the gate report;
    fd 3 is the only channel that reaches the terminal without becoming a report line.
    """
    try:
        os.write(3, f"{message}\n".encode("utf-8"))
    except OSError:
        pass

def run_rss_measurement(command):
    time_command = "/usr/bin/time"
    if not os.path.isfile(time_command):
        raise RuntimeError(
            "/usr/bin/time is unavailable; install it (Debian/Ubuntu: `apt-get install time`)"
        )
    time_args = ["-l"] if sys.platform == "darwin" else ["-v"]
    result = subprocess.run(
        [time_command, *time_args, *command],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        check=False,
    )
    timing = result.stderr.decode("utf-8", "replace")
    if result.returncode == 0:
        if sys.platform == "darwin":
            match = re.search(r"(\d+)\s+maximum resident set size", timing)
            if match is None:
                raise RuntimeError("macOS time output has no maximum resident set size")
            return int(match.group(1))
        match = re.search(r"Maximum resident set size \(kbytes\):\s*(\d+)", timing)
        if match is None:
            raise RuntimeError("GNU time output has no maximum resident set size")
        return int(match.group(1)) * 1024

    sandbox_denial = re.search(
        r"(?m)^time: sysctl kern\.clockrate: Operation not permitted\s*$", timing
    )
    if sys.platform != "darwin" or result.returncode != 1 or sandbox_denial is None:
        diagnostic = timing.strip()
        raise RuntimeError(f"time failed ({result.returncode}): {diagnostic}")

    # Some sandboxed macOS hosts deny the sysctl used by `time -l`. Keep the
    # required measurement as the primary path, but isolate a getrusage
    # fallback in a fresh helper so earlier child processes do not inflate
    # RUSAGE_CHILDREN. The helper also verifies the benchmarked command's exit.
    helper = (
        "import resource, subprocess, sys; "
        "p = subprocess.run(sys.argv[1:], stdout=subprocess.DEVNULL, "
        "stderr=subprocess.PIPE, check=False); "
        "print(resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss); "
        "sys.exit(p.returncode)"
    )
    fallback = subprocess.run(
        [sys.executable, "-c", helper, *command],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if fallback.returncode != 0:
        diagnostic = timing.strip() or fallback.stderr.decode("utf-8", "replace").strip()
        raise RuntimeError(f"time and getrusage failed: {diagnostic}")
    raw_rss = fallback.stdout.decode("ascii", "strict").strip()
    if not raw_rss.isdigit():
        raise RuntimeError(f"getrusage returned a non-numeric RSS: {raw_rss!r}")
    print(
        "P5 detail: /usr/bin/time reported the known macOS sandbox sysctl denial; "
        f"used isolated getrusage fallback (original: {timing.strip()})",
        file=sys.stderr,
    )
    return int(raw_rss)

try:
    rss_bytes = run_rss_measurement(hermetic_command(default_policy))
    json_bytes = os.path.getsize(metadata)
    rss_limit = json_bytes * 3
    rss_pass = rss_bytes <= rss_limit
    print(f"AC-P5-rss-bytes\t{rss_bytes}\t{rss_limit}\t{'PASS' if rss_pass else 'FAIL'}")
    print(
        f"P5 detail: peak RSS={rss_bytes} bytes, JSON={json_bytes} bytes, "
        f"limit={rss_limit} bytes",
        file=sys.stderr,
    )
except Exception as error:
    print("AC-P5-rss-bytes\t0\t0\tFAIL")
    print(f"P5 measurement error: {error}", file=sys.stderr)
    sys.exit(1)

# The same measurement on the feature-aware policy, reported and never gated: AC 14 holds
# AC-P5 to the default path, because the activation walk's per-package decode caches are a
# cost only a policy that asks for them pays. It goes to fd 3 so the gate report keeps one
# line per gate, and a failure here is a lost diagnostic, not a failed run.
try:
    feature_rss = run_rss_measurement(hermetic_command(feature_policy))
    note(
        f"P5 diagnostic (not gated): feature-aware peak RSS={feature_rss} bytes, "
        f"{feature_rss / json_bytes:.2f}x the document against the default path's "
        f"{rss_bytes / json_bytes:.2f}x"
    )
except Exception as error:
    note(f"P5 diagnostic (not gated): feature-aware RSS measurement failed: {error}")

sys.exit(0 if rss_pass else 1)
PY
}

set +e
run_measurement AC-P5 "$rss_log" measure_rss
rss_status=$?
set -e
if (( rss_status != 0 )); then
    gate_failed=1
fi

# One attempt at the live AC-P3 and AC-P4 gates, over the workspace materialised
# below. Called with errexit disabled by run_measurement.
measure_live() {
    python3 - "$binary" "$live_workspace" "$config_path" \
        "$repo_root/benches/baseline/ci-lint-baseline.sh" "$runs" 2>&1 <<'PY'
import json
import os
import shlex
import subprocess
import sys
import tempfile

binary, workspace, config, baseline, runs = sys.argv[1:]
runs = int(runs)
env = dict(os.environ)
env["RUSTFLAGS"] = ""
env["CARGO_NET_OFFLINE"] = "true"
# The pinned checkout carries its own rust-toolchain.toml. Both measured commands run
# cargo -- one directly, one inside the gate -- so both must be held to the toolchain
# the shell replica also uses, or AC-P3 divides two different cargos.
env["RUSTUP_TOOLCHAIN"] = os.environ.get("DEPGATE_PERF_TOOLCHAIN", "1.98.0")

# Both commands resolve with --all-features, and neither is free to drop it. The committed
# policy this section measures carries per-rule `features` keys, and a rule that narrows a
# closure is sound only over a document that left no member's features off -- so the gate
# rejects a default-features resolve of this workspace with exit 2 rather than answering
# from it. AC-P3 divides the two means, so `cargo metadata` has to resolve the same
# selection the gate does or the ratio compares two different workloads. The shell replica
# in benches/baseline/ci-lint-baseline.sh already pays for one --all-features resolve at
# its L204 line, which is the assertion that put the key in the policy.
metadata_command = [
    "cargo",
    "metadata",
    "--format-version",
    "1",
    "--locked",
    "--all-features",
    "--manifest-path",
    os.path.join(workspace, "Cargo.toml"),
]
depgate_command = [
    binary,
    "depgate",
    "check",
    "--manifest-path",
    os.path.join(workspace, "Cargo.toml"),
    "--config",
    config,
    "--format",
    "json",
    "--all-features",
    "--offline",
]

def hyperfine_means(commands, count):
    descriptor, report_path = tempfile.mkstemp(prefix="cargo-depgate-hyperfine-", suffix=".json")
    os.close(descriptor)
    try:
        result = subprocess.run(
            [
                "hyperfine",
                "-N",
                "--warmup",
                "3",
                "--runs",
                str(count),
                "--export-json",
                report_path,
                *[shlex.join(command) for command in commands],
            ],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if result.returncode != 0:
            diagnostic = result.stderr.decode("utf-8", "replace").strip()
            raise RuntimeError(f"hyperfine failed ({result.returncode}): {diagnostic}")
        with open(report_path, encoding="utf-8") as report:
            results = json.load(report).get("results", [])
        if len(results) != len(commands) or any(
            not isinstance(result.get("mean"), (int, float)) for result in results
        ):
            raise RuntimeError("hyperfine JSON did not contain one numeric mean per command")
        return [float(result["mean"]) * 1_000.0 for result in results]
    finally:
        try:
            os.unlink(report_path)
        except FileNotFoundError:
            pass

def hyperfine_mean(command, count):
    return hyperfine_means([command], count)[0]

failed = False
metadata_mean = None
depgate_mean = None
try:
    if not os.path.isfile(os.path.join(workspace, "Cargo.toml")):
        raise RuntimeError(f"live workspace is missing Cargo.toml: {workspace}")
    metadata_mean, depgate_mean = hyperfine_means([metadata_command, depgate_command], runs)
    ratio = depgate_mean / metadata_mean if metadata_mean else float("inf")
    overhead = depgate_mean - metadata_mean
    # Unchanged from the previous fixture and still comfortable on lemmy: measured
    # ratio 1.027 and overhead 7.217 ms against a 264.6 ms `--all-features` resolve.
    ratio_pass = ratio <= 1.1
    overhead_pass = overhead <= 15.0
    print(f"AC-P3-spawn-ratio\t{ratio:.3f}\t1.100\t{'PASS' if ratio_pass else 'FAIL'}")
    print(f"AC-P3-spawn-overhead\t{overhead:.3f}\t15.000\t{'PASS' if overhead_pass else 'FAIL'}")
    print(
        f"P3 detail: cargo metadata mean={metadata_mean:.3f} ms, "
        f"cargo-depgate mean={depgate_mean:.3f} ms",
        file=sys.stderr,
    )
    failed |= not (ratio_pass and overhead_pass)
except Exception as error:
    print("AC-P3-spawn-ratio\t0.000\t1.100\tFAIL")
    print("AC-P3-spawn-overhead\t0.000\t15.000\tFAIL")
    print(f"P3 measurement error: {error}", file=sys.stderr)
    failed = True

try:
    if not os.path.isfile(baseline):
        raise RuntimeError(f"baseline replica is missing: {baseline}")
    baseline_mean = hyperfine_mean([baseline], runs)
    if depgate_mean is None:
        depgate_mean = hyperfine_mean(depgate_command, runs)
    speedup = baseline_mean / depgate_mean if depgate_mean else 0.0
    # The speedup a `cargo tree` policy can give up is bounded by how many times it
    # spawns cargo: each invocation pays a full resolve, and the gate pays one. lemmy's
    # `check_disallowed_dependencies` is four invocations, so ~4x is the ceiling here by
    # construction, not a regression in the gate. Measured on aarch64-apple-darwin:
    # baseline 906.2 ms, gate 271.8 ms, speedup 3.334, cargo metadata alone 264.6 ms. The
    # README quotes that same pair; both absolute means move with the host, while the ratio
    # is the property the gate below actually bounds.
    speedup_pass = speedup >= 2.5
    latency_pass = depgate_mean <= 400.0
    print(f"AC-P4-speedup\t{speedup:.3f}\t2.500\t{'PASS' if speedup_pass else 'FAIL'}")
    print(f"AC-P4-depgate-ms\t{depgate_mean:.3f}\t400.000\t{'PASS' if latency_pass else 'FAIL'}")
    print(
        f"P4 detail: baseline mean={baseline_mean:.3f} ms, "
        f"cargo-depgate mean={depgate_mean:.3f} ms",
        file=sys.stderr,
    )
    failed |= not (speedup_pass and latency_pass)
except Exception as error:
    print("AC-P4-speedup\t0.000\t2.500\tFAIL")
    print("AC-P4-depgate-ms\t0.000\t400.000\tFAIL")
    print(f"P4 measurement error: {error}", file=sys.stderr)
    failed = True

sys.exit(1 if failed else 0)
PY
}

if [[ "$live" == "1" ]]; then
    # AC-P3 and AC-P4 must run against the *pinned* commit, otherwise they compare
    # today's upstream tree against a document frozen months earlier. An explicit
    # DEPGATE_PERF_WORKSPACE is taken as-is (the caller vouches for it); otherwise the
    # tree is materialised with `git archive` out of the same clone directory
    # scripts/fixture.sh uses, which is the only place the commit is ever checked out.
    if [[ -n "${DEPGATE_PERF_WORKSPACE:-}" ]]; then
        live_workspace="$DEPGATE_PERF_WORKSPACE"
    else
        clone_root="${DEPGATE_FIXTURE_CLONES:-${TMPDIR:-/tmp}/cargo-depgate-fixture-clones}"
        readonly clone_root
        readonly clone="$clone_root/lemmy"
        if [[ ! -d "$clone/.git" ]]; then
            printf 'error: no lemmy clone at %s; run scripts/fixture.sh lemmy --check first, or set DEPGATE_PERF_WORKSPACE\n' \
                "$clone" >&2
            exit 1
        fi
        if ! git -C "$clone" cat-file -e "$live_commit^{commit}" 2>/dev/null; then
            printf 'error: commit %s is not present in %s\n' "$live_commit" "$clone" >&2
            exit 1
        fi
        live_workspace="$work_root/live-workspace"
        mkdir -p "$live_workspace"
        git -C "$clone" archive "$live_commit" | tar -x -C "$live_workspace"
        # `cargo tree --all-features` (the upstream L204 assertion the AC-P4 replica
        # reproduces) resolves optional crates the default feature set never names, so
        # warm the registry cache once before anything is timed. It downloads; it never
        # compiles, and it is outside every measured command.
        (cd "$live_workspace" && RUSTFLAGS='' RUSTUP_TOOLCHAIN="$live_toolchain" \
            cargo fetch --locked >/dev/null 2>&1) || {
            printf 'error: cargo fetch failed for the pinned lemmy tree\n' >&2
            exit 1
        }
    fi
    readonly live_workspace
    # Hand the resolved workspace to benches/baseline/ci-lint-baseline.sh, which reads
    # the same variable: without this the AC-P4 speedup could divide one workspace's
    # shell replica by another workspace's tool run and still PASS.
    export DEPGATE_PERF_WORKSPACE="$live_workspace"
    export DEPGATE_PERF_TOOLCHAIN="$live_toolchain"
    printf 'live workspace: %s\n' "$live_workspace" >&2
    readonly live_log="$work_root/live.log"
    set +e
    run_measurement "AC-P3/AC-P4" "$live_log" measure_live
    live_status=$?
    set -e
    if (( live_status != 0 )); then
        gate_failed=1
    fi
fi

if (( gate_failed )); then
    exit 1
fi
