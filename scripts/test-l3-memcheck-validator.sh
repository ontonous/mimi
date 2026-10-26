#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
source "$ROOT_DIR/scripts/lib/l3_memcheck_validator.sh"

scratch="$(mktemp -d "${TMPDIR:-/tmp}/mimi-l3-memcheck-validator.XXXXXX")"
trap 'rm -rf -- "$scratch"' EXIT

write_all_free_capture() {
    local directory="$1" pid="${2:-31415}"
    mkdir -p -- "$directory"
    cat > "$directory/memcheck-summary.txt" <<EOF
==${pid}== in use at exit: 0 bytes in 0 blocks
==${pid}== All heap blocks were freed -- no leaks are possible
==${pid}== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)
EOF
    cat > "$directory/valgrind.log" <<EOF
==${pid}== Memcheck, a memory error detector
==${pid}== Copyright (C) 2002-2022, and GNU GPL'd, by Julian Seward et al.
==${pid}== Using Valgrind-3.22.0 and LibVEX; rerun with -h for copyright info
==${pid}== Command: /tmp/program
==${pid}== Parent PID: 1234
==${pid}==
==${pid}==
==${pid}== HEAP SUMMARY:
==${pid}==     in use at exit: 0 bytes in 0 blocks
==${pid}==   total heap usage: 2 allocs, 2 frees, 64 bytes allocated
==${pid}==
==${pid}== All heap blocks were freed -- no leaks are possible
==${pid}==
==${pid}== For lists of detected and suppressed errors, rerun with: -s
==${pid}== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)
EOF
    sed -i "s/^==${pid}==$/==${pid}== /" "$directory/valgrind.log"
}

write_reachable_capture() {
    local directory="$1" pid="${2:-31415}"
    mkdir -p -- "$directory"
    cat > "$directory/memcheck-summary.txt" <<EOF
==${pid}== definitely lost: 0 bytes in 0 blocks
==${pid}== indirectly lost: 0 bytes in 0 blocks
==${pid}== possibly lost: 0 bytes in 0 blocks
==${pid}== still reachable: 1,449 bytes in 17 blocks
==${pid}== suppressed: 0 bytes in 0 blocks
==${pid}== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)
EOF
    cat > "$directory/valgrind.log" <<EOF
==${pid}== Memcheck, a memory error detector
==${pid}== Copyright (C) 2002-2022, and GNU GPL'd, by Julian Seward et al.
==${pid}== Using Valgrind-3.22.0 and LibVEX; rerun with -h for copyright info
==${pid}== Command: /tmp/program
==${pid}== Parent PID: 1234
==${pid}==
==${pid}==
==${pid}== HEAP SUMMARY:
==${pid}==     in use at exit: 1,449 bytes in 17 blocks
==${pid}==   total heap usage: 90 allocs, 73 frees, 10,188 bytes allocated
==${pid}==
==${pid}== 17 bytes in 1 blocks are still reachable in loss record 1 of 2
==${pid}==    at 0x1234: malloc (fixture)
==${pid}==    by 0x5678: main (fixture)
==${pid}==
==${pid}== 1,432 bytes in 16 blocks are still reachable in loss record 2 of 2
==${pid}==    at 0x1234: malloc (fixture)
==${pid}==    by 0x5678: main (fixture)
==${pid}==
==${pid}== LEAK SUMMARY:
==${pid}==    definitely lost: 0 bytes in 0 blocks
==${pid}==    indirectly lost: 0 bytes in 0 blocks
==${pid}==      possibly lost: 0 bytes in 0 blocks
==${pid}==    still reachable: 1,449 bytes in 17 blocks
==${pid}==         suppressed: 0 bytes in 0 blocks
==${pid}==
==${pid}== For lists of detected and suppressed errors, rerun with: -s
==${pid}== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)
EOF
    sed -i "s/^==${pid}==$/==${pid}== /" "$directory/valgrind.log"
}

expect_valid() {
    local directory="$1"
    _l3_memcheck_validate_capture "$directory" "self-test $(basename "$directory")" /tmp/program
}

expect_invalid() {
    local directory="$1" reason="$2"
    if _l3_memcheck_validate_capture "$directory" "self-test $(basename "$directory")" /tmp/program \
        > "$scratch/stdout" 2> "$scratch/stderr"; then
        printf 'unexpectedly accepted invalid fixture: %s\n' "$directory" >&2
        return 1
    fi
    if ! grep -Fq -- "$reason" "$scratch/stderr"; then
        printf 'invalid fixture failed for an unexpected reason: %s\n' "$directory" >&2
        cat "$scratch/stderr" >&2
        return 1
    fi
}

expect_invalid_command() {
    local directory="$1" expected_command="$2" reason="$3"
    if _l3_memcheck_validate_capture "$directory" "self-test $(basename "$directory")" "$expected_command" \
        > "$scratch/stdout" 2> "$scratch/stderr"; then
        printf 'unexpectedly accepted invalid command fixture: %s\n' "$directory" >&2
        return 1
    fi
    if ! grep -Fq -- "$reason" "$scratch/stderr"; then
        printf 'invalid command fixture failed for an unexpected reason: %s\n' "$directory" >&2
        cat "$scratch/stderr" >&2
        return 1
    fi
}

write_all_free_capture "$scratch/all-free"
expect_valid "$scratch/all-free"

write_all_free_capture "$scratch/redacted-non-ascii-command"
sed -i 's#Command: /tmp/program#Command: /tmp/______/program#' \
    "$scratch/redacted-non-ascii-command/valgrind.log"
_l3_memcheck_validate_capture \
    "$scratch/redacted-non-ascii-command" \
    'self-test redacted non-ASCII command' \
    '/tmp/文档/program'

write_all_free_capture "$scratch/redacted-wrong-command"
sed -i 's#Command: /tmp/program#Command: /tmp/______/forged-program#' \
    "$scratch/redacted-wrong-command/valgrind.log"
expect_invalid_command \
    "$scratch/redacted-wrong-command" \
    '/tmp/文档/program' \
    'raw log command does not match the captured native executable'

write_all_free_capture "$scratch/all-free-mismatched-totals"
sed -i 's/2 allocs, 2 frees/2 allocs, 1 frees/' \
    "$scratch/all-free-mismatched-totals/valgrind.log"
expect_invalid "$scratch/all-free-mismatched-totals" 'all-free allocation and free totals differ'

write_reachable_capture "$scratch/reachable"
expect_valid "$scratch/reachable"

write_all_free_capture "$scratch/duplicate-error"
sed -i '$d' "$scratch/duplicate-error/valgrind.log"
cat >> "$scratch/duplicate-error/valgrind.log" <<'EOF'
==31415== ERROR SUMMARY: 1 errors from 1 contexts (suppressed: 0 from 0)
==31415== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)
EOF
expect_invalid "$scratch/duplicate-error" 'required summary/footer row'

write_reachable_capture "$scratch/duplicate-leak-row"
sed -i '/==31415== LEAK SUMMARY:/i\==31415==    definitely lost: 1 bytes in 1 blocks' \
    "$scratch/duplicate-leak-row/valgrind.log"
expect_invalid "$scratch/duplicate-leak-row" 'exactly one definitely lost row'

write_reachable_capture "$scratch/missing-leak-row"
sed -i '/possibly lost:/d' "$scratch/missing-leak-row/valgrind.log"
expect_invalid "$scratch/missing-leak-row" 'exactly one possibly lost row'

write_reachable_capture "$scratch/reversed-alloc-free"
sed -i 's/90 allocs, 73 frees/72 allocs, 73 frees/' \
    "$scratch/reversed-alloc-free/valgrind.log"
expect_invalid "$scratch/reversed-alloc-free" 'reachable heap frees exceed allocations'

write_reachable_capture "$scratch/mismatched-loss-totals"
sed -i 's/17 bytes in 1 blocks/16 bytes in 1 blocks/' \
    "$scratch/mismatched-loss-totals/valgrind.log"
expect_invalid "$scratch/mismatched-loss-totals" 'loss records do not sum to the complete reachable totals'

write_reachable_capture "$scratch/extra-loss-after-summary"
sed -i '/==31415==         suppressed:/a\==31415== 1 bytes in 1 blocks are still reachable in loss record 3 of 3' \
    "$scratch/extra-loss-after-summary/valgrind.log"
expect_invalid "$scratch/extra-loss-after-summary" 'reachable raw leak summary/footer is truncated, duplicated, or out of order'

write_reachable_capture "$scratch/foreign-pid"
sed -i 's/^==31415== Parent PID:/==99999== Parent PID:/' \
    "$scratch/foreign-pid/valgrind.log"
expect_invalid "$scratch/foreign-pid" 'raw log PID differs from its summary'

write_all_free_capture "$scratch/untagged-line"
sed -i '/HEAP SUMMARY:/i forged summary text' "$scratch/untagged-line/valgrind.log"
expect_invalid "$scratch/untagged-line" 'raw log contains an untagged line'

write_all_free_capture "$scratch/missing-header"
sed -i '1d' "$scratch/missing-header/valgrind.log"
expect_invalid "$scratch/missing-header" 'raw log lacks the complete Valgrind 3.22 header'

write_all_free_capture "$scratch/wrong-command"
sed -i 's#Command: /tmp/program#Command: /tmp/forged-program#' \
    "$scratch/wrong-command/valgrind.log"
expect_invalid "$scratch/wrong-command" 'raw log command does not match the captured native executable'

write_reachable_capture "$scratch/truncated-footer"
sed -i '$d' "$scratch/truncated-footer/valgrind.log"
expect_invalid "$scratch/truncated-footer" 'required summary/footer row'

write_all_free_capture "$scratch/truncated-newline"
truncate -s -1 "$scratch/truncated-newline/valgrind.log"
expect_invalid "$scratch/truncated-newline" 'lacks its final newline'

write_reachable_capture "$scratch/truncated-loss-details"
sed -i '/bytes in .* blocks are still reachable in loss record/d' \
    "$scratch/truncated-loss-details/valgrind.log"
expect_invalid "$scratch/truncated-loss-details" 'lacks matching totals or loss-record stack details'

write_all_free_capture "$scratch/summary-foreign-pid"
sed -i 's/31415/31416/g' "$scratch/summary-foreign-pid/memcheck-summary.txt"
expect_invalid "$scratch/summary-foreign-pid" 'raw log PID differs from its summary'

printf 'PASS strict L3 Memcheck parser self-tests (all-free, reachable, duplicate, PID, truncation)\n'
