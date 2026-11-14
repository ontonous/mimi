#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
LLVM_SYS_181_PREFIX="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
MIMI_STDLIB="$ROOT_DIR/std"
CARGO_INCREMENTAL=0
export LLVM_SYS_181_PREFIX MIMI_STDLIB CARGO_INCREMENTAL
for tool in cargo rustc sha256sum realpath git find sort xargs; do
    command -v "$tool" >/dev/null || { printf 'missing required tool: %s\n' "$tool" >&2; exit 1; }
done
[ -d "$LLVM_SYS_181_PREFIX" ] || { printf 'LLVM wrapper missing: %s\n' "$LLVM_SYS_181_PREFIX" >&2; exit 1; }
LLVM_SYS_181_PREFIX="$(realpath -- "$LLVM_SYS_181_PREFIX")"
export LLVM_SYS_181_PREFIX

requested="${MIMI_MAP_V2_CLONE_ISOLATION_L1L2_ARTIFACT_DIR:-$ROOT_DIR/evidence/0.1.11-dev/gates/ordinary-map-v2-clone-isolation-l1l2-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
case "$requested" in /*) ;; *) requested="$ROOT_DIR/$requested" ;; esac
[ ! -e "$requested" ] && [ ! -L "$requested" ] || {
    printf 'refusing to overwrite evidence: %s\n' "$requested" >&2; exit 2;
}
artifact_dir="$(realpath -m -- "$requested")"
case "$artifact_dir/" in
    "$ROOT_DIR/src/"*|"$ROOT_DIR/tests/"*|"$ROOT_DIR/std/"*|"$ROOT_DIR/scripts/"*|"$ROOT_DIR/target/"*|"$ROOT_DIR/.git/"*)
        printf 'evidence path overlaps source/build tree: %s\n' "$artifact_dir" >&2; exit 2 ;;
esac
mkdir -p -- "$(dirname -- "$artifact_dir")"
mkdir -- "$artifact_dir"
artifact_dir="$(realpath -- "$artifact_dir")"

source_snapshot() {
    (
        cd "$ROOT_DIR"
        {
            find src tests std \( -type f -o -type l \) -print0
            printf '%s\0' Cargo.toml Cargo.lock build.rs \
                scripts/verify-ordinary-map-v2-clone-isolation-l1l2.sh \
                tests/canonical_mir_ordinary_map_any_v2_clone_isolation_memcheck_l3.rs \
                tests/fixtures/mir_ordinary_map_owned_any_v2_clone_source_first.mimi \
                tests/fixtures/mir_ordinary_map_owned_any_v2_clone_clone_first.mimi
            for f in .cargo/config .cargo/config.toml rust-toolchain rust-toolchain.toml; do
                [ ! -f "$f" ] || printf '%s\0' "$f"
            done
            cargo_home="${CARGO_HOME:-${HOME:-}/.cargo}"
            for f in "$cargo_home/config" "$cargo_home/config.toml"; do
                [ ! -f "$f" ] || printf '%s\0' "$f"
            done
        } | sort -zu | xargs -0 -r sha256sum --
    )
}
source_snapshot > "$artifact_dir/source.before.manifest"
sha256sum "$artifact_dir/source.before.manifest" | awk '{print $1}' > "$artifact_dir/source.before.sha256"
git -C "$ROOT_DIR" rev-parse HEAD > "$artifact_dir/head.before"
git -C "$ROOT_DIR" status --porcelain=v1 --untracked-files=normal > "$artifact_dir/status.before"
{
    git -C "$ROOT_DIR" branch --show-current
    git -C "$ROOT_DIR" config user.name || true
    git -C "$ROOT_DIR" config user.email || true
    env | sort | grep -E '^(PATH|LD_LIBRARY_PATH|LLVM_SYS_181_PREFIX|MIMI_STDLIB|RUSTFLAGS|CARGO_HOME|CARGO_TARGET_DIR|CARGO_BUILD_JOBS|CARGO_INCREMENTAL|RUSTC_WRAPPER|CC|CXX|AR)=' || true
} > "$artifact_dir/environment.txt"
{
    cargo --version
    rustc --version --verbose
    llvm_config="$LLVM_SYS_181_PREFIX/bin/llvm-config"
    [ -x "$llvm_config" ] || llvm_config="$LLVM_SYS_181_PREFIX/llvm-config"
    [ -x "$llvm_config" ]
    printf 'llvm_config_sha256=%s\n' "$(sha256sum "$llvm_config" | awk '{print $1}')"
    printf 'llvm_version=%s\n' "$("$llvm_config" --version)"
    printf 'llvm_prefix=%s\n' "$("$llvm_config" --prefix)"
} > "$artifact_dir/toolchain.txt" 2> "$artifact_dir/toolchain.stderr"

gate_status=0
run_stage() {
    local name="$1"; shift
    local dir="$artifact_dir/$name" code=0 summaries=0
    mkdir "$dir"
    printf 'LLVM_SYS_181_PREFIX=%q MIMI_STDLIB=%q CARGO_INCREMENTAL=0 cargo ' \
        "$LLVM_SYS_181_PREFIX" "$MIMI_STDLIB" > "$dir/command.txt"
    printf '%q ' "$@" >> "$dir/command.txt"
    printf '%q ' -- --nocapture --test-threads=1 >> "$dir/command.txt"
    printf '\n' >> "$dir/command.txt"
    if LLVM_SYS_181_PREFIX="$LLVM_SYS_181_PREFIX" MIMI_STDLIB="$MIMI_STDLIB" CARGO_INCREMENTAL=0 \
        cargo "$@" -- --nocapture --test-threads=1 > "$dir/stdout.txt" 2> "$dir/stderr.txt"; then
        code=0
    else
        code=$?
        gate_status=1
    fi
    printf 'exit_code=%s\n' "$code" > "$dir/process.status"
    summaries="$(grep -Ec '^test result: ok\. [1-9][0-9]* passed; 0 failed; 0 ignored;' "$dir/stdout.txt" || true)"
    if [ "$code" -eq 0 ] && [ "$summaries" -eq 1 ]; then
        printf 'validation=PASS\n' > "$dir/validation.txt"
    else
        printf 'validation=FAIL\nsuccessful_nonignored_summaries=%s\n' "$summaries" > "$dir/validation.txt"
        gate_status=1
    fi
}
run_stage checker-owner test --manifest-path "$ROOT_DIR/Cargo.toml" --lib ordinary_map_owned_any_v2_clone
run_stage bytecode-verifier test --manifest-path "$ROOT_DIR/Cargo.toml" --lib clone_isolation
run_stage raw-mir-rejection test --manifest-path "$ROOT_DIR/Cargo.toml" --lib ordinary_map_owned_any_v2_raw_clone_mir_is_rejected_by_verifier_but_executes_on_consumers
run_stage cli-matrix test --manifest-path "$ROOT_DIR/Cargo.toml" --test canonical_mir_ordinary_map_any_v2_clone_isolation_memcheck_l3 ordinary_map_owned_any_v2_clone_isolation_cli_routes_share_checker_receipt

verifier_log="$artifact_dir/bytecode-verifier/stdout.txt"
verifier_error_log="$artifact_dir/bytecode-verifier/stderr.txt"
if { grep -Fq 'MIMI_CLONE_ISOLATION_VERIFIER z3_available=true outcome=executed' "$verifier_log" \
    || grep -Fq 'MIMI_CLONE_ISOLATION_VERIFIER z3_available=true outcome=executed' "$verifier_error_log"; } \
    && ! { grep -Fq 'MIMI_CLONE_ISOLATION_VERIFIER z3_available=false outcome=skipped' "$verifier_log" \
        || grep -Fq 'MIMI_CLONE_ISOLATION_VERIFIER z3_available=false outcome=skipped' "$verifier_error_log"; }; then
    printf 'z3_verifier_execution=PASS\n' > "$artifact_dir/verifier-validation.txt"
else
    printf 'z3_verifier_execution=FAIL\n' > "$artifact_dir/verifier-validation.txt"
    gate_status=1
fi
source_snapshot > "$artifact_dir/source.after.manifest"
sha256sum "$artifact_dir/source.after.manifest" | awk '{print $1}' > "$artifact_dir/source.after.sha256"
git -C "$ROOT_DIR" rev-parse HEAD > "$artifact_dir/head.after"
git -C "$ROOT_DIR" status --porcelain=v1 --untracked-files=normal > "$artifact_dir/status.after"
if cmp -s "$artifact_dir/source.before.manifest" "$artifact_dir/source.after.manifest" \
    && cmp -s "$artifact_dir/head.before" "$artifact_dir/head.after"; then
    printf 'source_snapshot_identical=true\ngit_head_identical=true\n' > "$artifact_dir/source-stability.txt"
else
    printf 'source_snapshot_identical=false\ngit_head_identical=false\n' > "$artifact_dir/source-stability.txt"
    gate_status=1
fi

write_gate_status() {
    if [ "$gate_status" -eq 0 ]; then
        printf 'validation=PASS\nglobal_map_any_v2_production_ready=false\nroute_profile=map-any-owner-v2-clone-isolation-v1\npositive_fixtures=source-first,clone-first\npositive_modes=default,explicit-mir\npositive_consumers=run,build,verify\nnegative=wrong-source-payload-literal\nnegative_modes=default,explicit-mir\nnegative_consumers=run,build,verify\n' > "$artifact_dir/gate.status"
    else
        printf 'validation=FAIL\n' > "$artifact_dir/gate.status"
    fi
}
write_artifact_manifest() {
    (
        cd "$artifact_dir"
        find . -type f ! -name files.sha256 ! -name files.sha256.sha256 -print0 \
            | sort -z | xargs -0 -r sha256sum -- > files.sha256
        sha256sum files.sha256 > files.sha256.sha256
    )
}
validate_artifact_manifest() {
    (
        cd "$artifact_dir"
        sha256sum --check --status files.sha256
        sha256sum --check --status files.sha256.sha256
    )
}

write_gate_status
write_artifact_manifest
if ! validate_artifact_manifest; then
    gate_status=1
    write_gate_status
    write_artifact_manifest
    validate_artifact_manifest || {
        printf 'artifact manifest could not be stabilized: %s\n' "$artifact_dir" >&2
        exit 1
    }
fi
if [ "$gate_status" -eq 0 ]; then
    printf 'CloneIsolation L1/L2 PASS: %s\n' "$artifact_dir"
else
    printf 'CloneIsolation L1/L2 FAIL: %s\n' "$artifact_dir" >&2
    exit 1
fi
