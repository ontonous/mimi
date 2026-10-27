#!/usr/bin/env bash
# Serial, source-bound native Memcheck gate for the Map-root GetAny/String-unpack slice.
set -Eeuo pipefail

if [[ $# -gt 1 || ${1:-} == --help ]]; then
    printf 'Usage: %s [current-YYYYMMDD-rN]\n' "$0"
    exit 2
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd -- "$script_dir/.." && pwd -P)"
run_id="${1:-current-$(date -u +%Y%m%d-%H%M%S)-$$}"
[[ "$run_id" =~ ^[a-z0-9][a-z0-9-]{0,80}$ ]] || {
    printf 'Invalid run id: %s\n' "$run_id" >&2
    exit 2
}
artifact_dir="$repo_root/evidence/0.1.11-dev/gates/map-root-get-any-caller-l3-$run_id"
[[ ! -e "$artifact_dir" && ! -L "$artifact_dir" ]] || {
    printf 'Refusing to overwrite %s\n' "$artifact_dir" >&2
    exit 2
}
mkdir -p -- "$artifact_dir"
cd -- "$repo_root"

profile=ordinary-map-owned-any-v2-map-root-get-any-caller-v1
fixture="$repo_root/tests/fixtures/mir_ordinary_map_owned_any_v2_map_root_get_any_caller.mimi"
llvm_prefix="${LLVM_SYS_181_PREFIX:-/tmp/llvm-wrapper}"
target_dir="${MIMI_MAP_ROOT_GET_ANY_L3_TARGET_DIR:-${TMPDIR:-/tmp}/mimi-target-cache-map-root-get-any-caller-l3}"
[[ "$target_dir" == /* ]] || target_dir="$repo_root/$target_dir"
target_dir="$(realpath -m -- "$target_dir")"
case "$target_dir/" in
    "$repo_root/"*)
        printf 'Cargo target directory must stay outside the repository: %s\n' "$target_dir" >&2
        exit 2
        ;;
esac
[[ -d "$llvm_prefix" ]] || {
    printf 'LLVM wrapper directory is unavailable: %s\n' "$llvm_prefix" >&2
    exit 2
}
llvm_prefix="$(realpath -- "$llvm_prefix")"
[[ -f "$fixture" ]] || {
    printf 'Source fixture is unavailable: %s\n' "$fixture" >&2
    exit 2
}
for tool in cargo rustc valgrind sha256sum realpath git find sort cmp grep sed head; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'Map-root GetAny strict L3 requires %s\n' "$tool" >&2
        exit 2
    }
done

source_snapshot() {
    (
        cd -- "$repo_root"
        {
            for directory in src tests scripts .cargo; do
                [[ ! -d "$directory" ]] || find "$directory" -type f -print
            done
            find . -maxdepth 1 -type f \
                \( -name 'Cargo*' -o -name '.gitignore' -o -name 'build.rs' \
                    -o -name 'rust-toolchain*' -o -name 'rustfmt.toml' -o -name '.rustfmt.toml' \) \
                -print
        } | LC_ALL=C sort -u
    )
}

status=0
finalize() {
    local shell_status=$?
    trap - EXIT
    [[ "$shell_status" == 0 ]] || status="$shell_status"

    local stable=false
    if [[ -f "$artifact_dir/source-files.before.txt" ]]; then
        source_snapshot > "$artifact_dir/source-files.after.txt" \
            2> "$artifact_dir/source-snapshot.after.stderr" || status=98
        if cmp -s "$artifact_dir/source-files.before.txt" "$artifact_dir/source-files.after.txt"; then
            while IFS= read -r file; do sha256sum -- "$file"; done \
                < "$artifact_dir/source-files.after.txt" > "$artifact_dir/source.after.sha256"
            if cmp -s "$artifact_dir/source.before.sha256" "$artifact_dir/source.after.sha256"; then
                stable=true
            else
                status=98
            fi
        else
            status=98
        fi
    else
        status=98
    fi

    printf 'profile=%s\nexit_code=%s\nsource_tree_unchanged=%s\nglobal_map_any_v2_production_ready=false\n' \
        "$profile" "$status" "$stable" > "$artifact_dir/gate.status"
    (
        cd -- "$artifact_dir"
        find . -type f ! -name SHA256SUMS -print0 \
            | LC_ALL=C sort -z | xargs -0 -r sha256sum
    ) > "$artifact_dir/SHA256SUMS"
    if [[ "$status" == 0 ]]; then
        printf 'Map-root GetAny caller strict L3 passed; evidence: %s\n' "$artifact_dir"
    else
        printf 'Map-root GetAny caller strict L3 failed (exit %s); evidence: %s\n' \
            "$status" "$artifact_dir" >&2
    fi
    exit "$status"
}
trap finalize EXIT

source_snapshot > "$artifact_dir/source-files.before.txt"
while IFS= read -r file; do sha256sum -- "$file"; done \
    < "$artifact_dir/source-files.before.txt" > "$artifact_dir/source.before.sha256"
cp -- "$fixture" "$artifact_dir/source.fixture.mimi"
sha256sum -- "$fixture" > "$artifact_dir/source.fixture.sha256"
{
    printf 'repo_root=%s\nrun_id=%s\nprofile=%s\ncargo_target_dir=%s\nllvm_sys_181_prefix=%s\nfixture=%s\n' \
        "$repo_root" "$run_id" "$profile" "$target_dir" "$llvm_prefix" "$fixture"
    printf 'cargo='; cargo --version
    printf 'rustc='; rustc --version --verbose
    printf 'valgrind='; valgrind --version
    printf 'llvm_config='; "$llvm_prefix/bin/llvm-config" --version
} > "$artifact_dir/environment.txt" 2>&1

mkdir -p -- "$artifact_dir/native/default" "$artifact_dir/native/explicit-mir"
{
    for mode in default explicit-mir; do
        binary="$artifact_dir/native/$mode/program"
        if ! git check-ignore -q -- "$binary"; then
            printf 'expected native binary is not ignored: %s\n' "$binary" >&2
            exit 97
        fi
        git check-ignore -v -- "$binary"
    done
    if git check-ignore -q -- "$artifact_dir/gate.status"; then
        printf 'L3 evidence record must remain visible: %s\n' "$artifact_dir/gate.status" >&2
        exit 97
    fi
    printf 'gate_evidence_visible=true\n'
} > "$artifact_dir/ignore-preflight.txt" 2> "$artifact_dir/ignore-preflight.stderr"

for mode in default explicit-mir; do
    mode_dir="$artifact_dir/native/$mode"
    binary="$mode_dir/program"
    command_args=(run --locked --quiet --manifest-path "$repo_root/Cargo.toml" \
        --target-dir "$target_dir" -- build)
    [[ "$mode" != explicit-mir ]] || command_args+=(--mir)
    command_args+=("$fixture" --output "$binary")
    {
        printf 'LLVM_SYS_181_PREFIX=%q CARGO_BUILD_JOBS=1 MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 ' "$llvm_prefix"
        printf '%q ' cargo "${command_args[@]}"
        printf '\n'
    } > "$mode_dir/build.command.txt"
    printf '%s\0' cargo "${command_args[@]}" > "$mode_dir/build.argv.nul"
    build_status=0
    if LLVM_SYS_181_PREFIX="$llvm_prefix" CARGO_TARGET_DIR="$target_dir" CARGO_BUILD_JOBS=1 \
        MIMI_ROUTE_CENSUS=1 MIMI_VERBOSE=1 cargo "${command_args[@]}" \
        > "$mode_dir/build.stdout" 2> "$mode_dir/build.stderr"; then
        build_status=0
    else
        build_status=$?
    fi
    printf 'exit_code=%s\n' "$build_status" > "$mode_dir/build.status"
    if [[ "$build_status" != 0 || ! -x "$binary" ]]; then
        printf '%s native build failed; inspect %s\n' "$mode" "$mode_dir" >&2
        [[ "$build_status" == 0 ]] && exit 1
        exit "$build_status"
    fi

    admission="$mode_dir/build.stderr"
    grep -Fq "checker_profile=$profile" "$admission"
    [[ "$(grep -Fc 'MIMI_ROUTE_CENSUS route-admission ' "$admission")" == 1 ]]
    [[ "$(grep -Fc 'MIMI_ROUTE_CENSUS canonical-native-build ' "$admission")" == 1 ]]
    route_line="$(grep -F 'MIMI_ROUTE_CENSUS route-admission ' "$admission")"
    native_line="$(grep -F 'MIMI_ROUTE_CENSUS canonical-native-build ' "$admission")"
    grep -Fq "mode=$mode" <<< "$route_line"
    grep -Fq "checker_profile=$profile" <<< "$route_line"
    grep -Fq "route_id=$profile" <<< "$native_line"
    grep -Fq "profile=$profile" <<< "$native_line"
    grep -Fq "consumer=$mode" <<< "$native_line"
    mir_digest="$(sed -n 's/.*mir_digest=\([0-9a-f]\{64\}\).*/\1/p' <<< "$route_line")"
    receipt_digest="$(sed -n 's/.*receipt_identity_digest=\([0-9a-f]\{64\}\).*/\1/p' <<< "$route_line")"
    [[ "$mir_digest" =~ ^[0-9a-f]{64}$ && "$receipt_digest" =~ ^[0-9a-f]{64}$ ]]
    grep -Fq "mir_digest=$mir_digest" <<< "$native_line"
    grep -Fq "route_identity_digest=$receipt_digest" <<< "$native_line"
    if grep -Fq 'canonical route disposition: legacy' "$admission"; then
        printf '%s native build selected a legacy fallback\n' "$mode" >&2
        exit 97
    fi
    printf 'mode=%s\nprofile=%s\nmir_digest=%s\nreceipt_identity_digest=%s\n' \
        "$mode" "$profile" "$mir_digest" "$receipt_digest" > "$mode_dir/route-identity.txt"
    sha256sum -- "$binary" > "$mode_dir/program.sha256"

    report="$mode_dir/valgrind.log"
    isolated_home="$mode_dir/valgrind-home"
    mkdir -- "$isolated_home"
    memcheck_status=0
    if env -i PATH="$PATH" HOME="$isolated_home" LANG=C LC_ALL=C \
        valgrind --command-line-only=yes --default-suppressions=no --tool=memcheck \
        --leak-check=full --show-leak-kinds=all \
        --errors-for-leak-kinds=definite,indirect,possible --error-exitcode=97 \
        "--log-file=$report" "$binary" \
        > "$mode_dir/stdout.bin" 2> "$mode_dir/stderr.bin"; then
        memcheck_status=0
    else
        memcheck_status=$?
    fi
    printf 'exit_code=%s\n' "$memcheck_status" > "$mode_dir/memcheck.status"
    [[ "$memcheck_status" == 0 ]] || {
        printf '%s strict Memcheck failed; inspect %s\n' "$mode" "$report" >&2
        exit "$memcheck_status"
    }
    printf '0\n' | cmp -s - "$mode_dir/stdout.bin"
    [[ ! -s "$mode_dir/stderr.bin" ]]
    for expected in \
        'in use at exit: 0 bytes in 0 blocks' \
        'All heap blocks were freed -- no leaks are possible' \
        'ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)'; do
        grep -Fq "$expected" "$report"
    done
    cp -- "$report" "$mode_dir/memcheck-summary.txt"
done

default_identity="$artifact_dir/native/default/route-identity.txt"
explicit_identity="$artifact_dir/native/explicit-mir/route-identity.txt"
mir_default="$(sed -n 's/^mir_digest=//p' "$default_identity")"
mir_explicit="$(sed -n 's/^mir_digest=//p' "$explicit_identity")"
receipt_default="$(sed -n 's/^receipt_identity_digest=//p' "$default_identity")"
receipt_explicit="$(sed -n 's/^receipt_identity_digest=//p' "$explicit_identity")"
[[ "$mir_default" == "$mir_explicit" && "$receipt_default" == "$receipt_explicit" ]]
cat > "$artifact_dir/l3-summary.txt" <<EOF
route_profile=$profile
global_map_any_v2_production_ready=false
fixture_count=1
native_build_count=2
strict_memcheck_count=2
default_explicit_identity_equal=true
expected_stdout_hex=300a
mir_digest=$mir_default
receipt_identity_digest=$receipt_default
EOF

status=0
