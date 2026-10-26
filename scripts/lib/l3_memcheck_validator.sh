# Shared strict parser for Valgrind 3.22 captures produced by the Values* L3
# runners. Source this file from Bash; it deliberately has no top-level effects.

_l3_memcheck_error() {
    printf '%s: %s\n' "$2" "$1" >&2
    return 1
}

_l3_memcheck_count_regex() {
    local file="$1" expression="$2"
    grep -Ec -- "$expression" "$file" || true
}

_l3_memcheck_valid_count() {
    [[ "$1" =~ ^([0-9]+|[1-9][0-9]{0,2}(,[0-9]{3})+)$ ]]
}

_l3_memcheck_normalize_count() {
    local value="${1//,/}"
    while [[ ${#value} -gt 1 && "$value" == 0* ]]; do
        value="${value#0}"
    done
    printf '%s' "$value"
}

_l3_memcheck_decimal_ge() {
    local left right
    left="$(_l3_memcheck_normalize_count "$1")"
    right="$(_l3_memcheck_normalize_count "$2")"
    if [[ ${#left} -gt ${#right} ]]; then
        return 0
    fi
    if [[ ${#left} -lt ${#right} ]]; then
        return 1
    fi
    [[ "$left" == "$right" || "$left" > "$right" ]]
}

_l3_memcheck_decimal_add() {
    local left right carry=0 digit_left digit_right sum result=""
    left="$(_l3_memcheck_normalize_count "$1")"
    right="$(_l3_memcheck_normalize_count "$2")"
    while [[ -n "$left" || -n "$right" || "$carry" != 0 ]]; do
        digit_left=0
        digit_right=0
        if [[ -n "$left" ]]; then
            digit_left="${left: -1}"
            left="${left%?}"
        fi
        if [[ -n "$right" ]]; then
            digit_right="${right: -1}"
            right="${right%?}"
        fi
        sum=$((digit_left + digit_right + carry))
        result="$((sum % 10))$result"
        carry=$((sum / 10))
    done
    _l3_memcheck_normalize_count "$result"
}

_l3_memcheck_decimal_equal() {
    [[ "$(_l3_memcheck_normalize_count "$1")" == "$(_l3_memcheck_normalize_count "$2")" ]]
}

_l3_memcheck_line_number() {
    local file="$1" expression="$2"
    grep -n -m 1 -E -- "$expression" "$file" | cut -d: -f1
}

_l3_memcheck_validate_layout() {
    local raw="$1" pid="$2" rows="$3" expected_command="$4" label="$5"
    local reachable_bytes="${6:-0}" reachable_blocks="${7:-0}"
    local prefix="==${pid}==" line index record_bytes record_blocks record_number record_total
    local heap_in_use_bytes heap_in_use_blocks heap_allocations heap_frees heap_allocated_bytes
    local expected_record_number=0 observed_record_total="" sum_bytes=0 sum_blocks=0 frames
    local expected_command_line reported_command_line redacted_command command_matches=0
    local -a raw_rows=()

    mapfile -t raw_rows < "$raw"
    if (( ${#raw_rows[@]} < 15 )); then
        _l3_memcheck_error "raw log is truncated before the complete Valgrind report" "$label"
        return 1
    fi
    expected_command_line="$prefix Command: $expected_command"
    reported_command_line="${raw_rows[3]}"
    redacted_command="$(LC_ALL=C printf '%s' "$expected_command" | LC_ALL=C sed 's/[^[:print:]]/_/g')"
    if [[ "$reported_command_line" == "$expected_command_line" \
        || "$reported_command_line" == "$prefix Command: $redacted_command" ]]; then
        command_matches=1
    fi
    if [[ "${raw_rows[0]}" != "$prefix Memcheck, a memory error detector" \
        || "${raw_rows[1]}" != "$prefix Copyright (C) 2002-2022, and GNU GPL'd, by Julian Seward et al." \
        || ! "${raw_rows[2]}" =~ ^==${pid}==\ Using\ Valgrind-3\.22\.[0-9]+\ and\ LibVEX\;\ rerun\ with\ -h\ for\ copyright\ info$ \
        || "$command_matches" != 1 \
        || ! "${raw_rows[4]}" =~ ^==${pid}==\ Parent\ PID:\ [1-9][0-9]*$ \
        || "${raw_rows[5]}" != "$prefix " || "${raw_rows[6]}" != "$prefix " \
        || "${raw_rows[7]}" != "$prefix HEAP SUMMARY:" \
        || "${raw_rows[10]}" != "$prefix " ]]; then
        _l3_memcheck_error "Valgrind 3.22 raw header or heap-section prefix is malformed" "$label"
        return 1
    fi
    line="${raw_rows[8]}"
    if [[ ! "$line" =~ ^==${pid}==[[:space:]]+in\ use\ at\ exit:\ ([0-9,]+)\ bytes\ in\ ([0-9,]+)\ blocks$ ]]; then
        _l3_memcheck_error "raw heap in-use row is malformed" "$label"
        return 1
    fi
    heap_in_use_bytes="${BASH_REMATCH[1]}"
    heap_in_use_blocks="${BASH_REMATCH[2]}"
    line="${raw_rows[9]}"
    if [[ ! "$line" =~ ^==${pid}==[[:space:]]+total\ heap\ usage:\ ([0-9,]+)\ allocs,\ ([0-9,]+)\ frees,\ ([0-9,]+)\ bytes\ allocated$ ]]; then
        _l3_memcheck_error "raw total-heap row is malformed" "$label"
        return 1
    fi
    heap_allocations="${BASH_REMATCH[1]}"
    heap_frees="${BASH_REMATCH[2]}"
    heap_allocated_bytes="${BASH_REMATCH[3]}"
    for line in "$heap_in_use_bytes" "$heap_in_use_blocks" "$heap_allocations" "$heap_frees" "$heap_allocated_bytes"; do
        if ! _l3_memcheck_valid_count "$line"; then
            _l3_memcheck_error "raw heap totals use malformed numeric counts" "$label"
            return 1
        fi
    done
    if [[ "${raw_rows[8]}" != "$prefix     in use at exit: $heap_in_use_bytes bytes in $heap_in_use_blocks blocks" \
        || "${raw_rows[9]}" != "$prefix   total heap usage: $heap_allocations allocs, $heap_frees frees, $heap_allocated_bytes bytes allocated" ]]; then
        _l3_memcheck_error "raw heap totals do not use the complete Valgrind 3.22 row format" "$label"
        return 1
    fi

    if [[ "$rows" == 3 ]]; then
        if (( ${#raw_rows[@]} != 15 )) \
            || [[ "${raw_rows[11]}" != "$prefix All heap blocks were freed -- no leaks are possible" \
                || "${raw_rows[12]}" != "$prefix " \
                || "${raw_rows[13]}" != "$prefix For lists of detected and suppressed errors, rerun with: -s" \
                || "${raw_rows[14]}" != "$prefix ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)" ]]; then
            _l3_memcheck_error "all-free raw log is truncated, has extra rows, or is out of order" "$label"
            return 1
        fi
        return 0
    fi

    index=11
    while :; do
        if (( index >= ${#raw_rows[@]} )) || [[ ! "${raw_rows[index]}" =~ ^==${pid}==\ ([0-9,]+)\ bytes\ in\ ([0-9,]+)\ blocks\ are\ still\ reachable\ in\ loss\ record\ ([0-9]+)\ of\ ([0-9]+)$ ]]; then
            _l3_memcheck_error "reachable raw log has a missing or malformed loss record" "$label"
            return 1
        fi
        line="${raw_rows[index]}"
        if [[ ! "$line" =~ ^==${pid}==\ ([0-9,]+)\ bytes\ in\ ([0-9,]+)\ blocks\ are\ still\ reachable\ in\ loss\ record\ ([0-9]+)\ of\ ([0-9]+)$ ]]; then
            _l3_memcheck_error "reachable raw log has a malformed loss record" "$label"
            return 1
        fi
        record_bytes="${BASH_REMATCH[1]}"
        record_blocks="${BASH_REMATCH[2]}"
        record_number="${BASH_REMATCH[3]}"
        record_total="${BASH_REMATCH[4]}"
        if ! _l3_memcheck_valid_count "$record_bytes" || ! _l3_memcheck_valid_count "$record_blocks" \
            || ! _l3_memcheck_valid_count "$record_number" || ! _l3_memcheck_valid_count "$record_total"; then
            _l3_memcheck_error "reachable raw loss-record counts are malformed" "$label"
            return 1
        fi
        expected_record_number=$((expected_record_number + 1))
        if ! _l3_memcheck_decimal_equal "$record_number" "$expected_record_number" \
            || ! _l3_memcheck_decimal_ge "$record_blocks" 1 \
            || [[ "$(_l3_memcheck_normalize_count "$record_total")" == 0 ]]; then
            _l3_memcheck_error "reachable raw loss records are duplicated or out of sequence" "$label"
            return 1
        fi
        if [[ -z "$observed_record_total" ]]; then
            observed_record_total="$(_l3_memcheck_normalize_count "$record_total")"
        elif ! _l3_memcheck_decimal_equal "$record_total" "$observed_record_total"; then
            _l3_memcheck_error "reachable raw loss records disagree on their total count" "$label"
            return 1
        fi
        sum_bytes="$(_l3_memcheck_decimal_add "$sum_bytes" "$record_bytes")"
        sum_blocks="$(_l3_memcheck_decimal_add "$sum_blocks" "$record_blocks")"
        index=$((index + 1))
        frames=0
        while (( index < ${#raw_rows[@]} )) && [[ "${raw_rows[index]}" =~ ^==${pid}==[[:space:]]+(at|by)\ 0x[[:xdigit:]]+:\ .+$ ]]; do
            frames=$((frames + 1))
            index=$((index + 1))
        done
        if (( frames == 0 )) || (( index >= ${#raw_rows[@]} )) || [[ "${raw_rows[index]}" != "$prefix " ]]; then
            _l3_memcheck_error "reachable loss record is missing its stack frames or separator" "$label"
            return 1
        fi
        index=$((index + 1))
        if (( index < ${#raw_rows[@]} )) && [[ "${raw_rows[index]}" == "$prefix LEAK SUMMARY:" ]]; then
            break
        fi
    done

    if ! _l3_memcheck_decimal_equal "$expected_record_number" "$observed_record_total" \
        || ! _l3_memcheck_decimal_equal "$sum_bytes" "$reachable_bytes" \
        || ! _l3_memcheck_decimal_equal "$sum_blocks" "$reachable_blocks"; then
        _l3_memcheck_error "reachable loss records do not sum to the complete reachable totals" "$label"
        return 1
    fi
    if (( index + 9 != ${#raw_rows[@]} )) \
        || [[ "${raw_rows[index]}" != "$prefix LEAK SUMMARY:" \
            || "${raw_rows[index + 1]}" != "$prefix    definitely lost: 0 bytes in 0 blocks" \
            || "${raw_rows[index + 2]}" != "$prefix    indirectly lost: 0 bytes in 0 blocks" \
            || "${raw_rows[index + 3]}" != "$prefix      possibly lost: 0 bytes in 0 blocks" \
            || "${raw_rows[index + 4]}" != "$prefix    still reachable: $reachable_bytes bytes in $reachable_blocks blocks" \
            || "${raw_rows[index + 5]}" != "$prefix         suppressed: 0 bytes in 0 blocks" \
            || "${raw_rows[index + 6]}" != "$prefix " \
            || "${raw_rows[index + 7]}" != "$prefix For lists of detected and suppressed errors, rerun with: -s" \
            || "${raw_rows[index + 8]}" != "$prefix ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)" ]]; then
        _l3_memcheck_error "reachable raw leak summary/footer is truncated, duplicated, or out of order" "$label"
        return 1
    fi
    return 0
}

_l3_memcheck_validate_capture() {
    local directory="$1" label="$2" expected_command="$3"
    local summary="$directory/memcheck-summary.txt" raw="$directory/valgrind.log"
    local expected_command_line reported_command_line redacted_command
    local rows summary_pid raw_pid line message count last_byte summary_last_byte
    local memcheck_line copyright_line version_line command_line parent_line
    local heap_line in_use_line total_line free_line leak_line footer_line error_line
    local in_use_bytes in_use_blocks allocations frees allocated_bytes
    local reachable_line reachable_bytes reachable_blocks loss_records stack_frames
    local -a summary_rows=()

    if [[ ! -s "$summary" || ! -s "$raw" ]]; then
        _l3_memcheck_error "missing non-empty summary or raw Valgrind log" "$label"
        return 1
    fi
    last_byte="$(tail -c 1 -- "$raw" | od -An -t x1 | tr -d '[:space:]')"
    if [[ "$last_byte" != 0a ]]; then
        _l3_memcheck_error "raw Valgrind log is truncated or lacks its final newline" "$label"
        return 1
    fi
    summary_last_byte="$(tail -c 1 -- "$summary" | od -An -t x1 | tr -d '[:space:]')"
    if [[ "$summary_last_byte" != 0a ]]; then
        _l3_memcheck_error "Memcheck summary is truncated or lacks its final newline" "$label"
        return 1
    fi
    mapfile -t summary_rows < "$summary"
    rows="${#summary_rows[@]}"
    if [[ "$rows" != 3 && "$rows" != 6 ]]; then
        _l3_memcheck_error "summary must contain exactly three all-free rows or six reachable rows" "$label"
        return 1
    fi
    if [[ ! "${summary_rows[0]}" =~ ^==([0-9]+)==[[:space:]]+ ]]; then
        _l3_memcheck_error "summary has a malformed PID" "$label"
        return 1
    fi
    summary_pid="${BASH_REMATCH[1]}"
    if [[ ! "$summary_pid" =~ ^[1-9][0-9]*$ ]]; then
        _l3_memcheck_error "summary PID must be a positive decimal process ID" "$label"
        return 1
    fi
    for line in "${summary_rows[@]}"; do
        if [[ ! "$line" =~ ^==([0-9]+)==[[:space:]]+ ]] || [[ "${BASH_REMATCH[1]}" != "$summary_pid" ]]; then
            _l3_memcheck_error "summary rows contain a malformed or foreign PID" "$label"
            return 1
        fi
    done

    # Every Valgrind 3.22 output line, including its visually blank separators,
    # carries the child PID. Reject injected untagged text and mixed processes.
    while IFS= read -r line || [[ -n "$line" ]]; do
        if [[ ! "$line" =~ ^==([0-9]+)==(.*)$ ]]; then
            _l3_memcheck_error "raw log contains an untagged line" "$label"
            return 1
        fi
        raw_pid="${BASH_REMATCH[1]}"
        message="${BASH_REMATCH[2]}"
        if [[ "$raw_pid" != "$summary_pid" ]]; then
            _l3_memcheck_error "raw log PID differs from its summary" "$label"
            return 1
        fi

        # Keep the raw capture in the Valgrind 3.22 text grammar. This prevents
        # a hand-written collection of summary rows from masquerading as a
        # complete Memcheck report.
        if [[ "$message" =~ ^[[:space:]]+Memcheck,\ a\ memory\ error\ detector$ ]] \
            || [[ "$message" == " Copyright (C) 2002-2022, and GNU GPL'd, by Julian Seward et al." ]] \
            || [[ "$message" =~ ^[[:space:]]+Using\ Valgrind-3\.22\.[0-9]+\ and\ LibVEX\;\ rerun\ with\ -h\ for\ copyright\ info$ ]] \
            || [[ "$message" =~ ^[[:space:]]+Command:\ .+$ ]] \
            || [[ "$message" =~ ^[[:space:]]+Parent\ PID:\ [0-9]+$ ]] \
            || [[ "$message" =~ ^[[:space:]]*$ ]] \
            || [[ "$message" =~ ^[[:space:]]+HEAP\ SUMMARY:$ ]] \
            || [[ "$message" =~ ^[[:space:]]+in\ use\ at\ exit:\ [0-9,]+\ bytes\ in\ [0-9,]+\ blocks$ ]] \
            || [[ "$message" =~ ^[[:space:]]+total\ heap\ usage:\ [0-9,]+\ allocs,\ [0-9,]+\ frees,\ [0-9,]+\ bytes\ allocated$ ]] \
            || [[ "$message" =~ ^[[:space:]]+All\ heap\ blocks\ were\ freed\ --\ no\ leaks\ are\ possible$ ]] \
            || [[ "$message" =~ ^[[:space:]]+LEAK\ SUMMARY:$ ]] \
            || [[ "$message" =~ ^[[:space:]]+(definitely\ lost|indirectly\ lost|possibly\ lost|still\ reachable|suppressed):\ [0-9,]+\ bytes\ in\ [0-9,]+\ blocks$ ]] \
            || [[ "$message" =~ ^[[:space:]]+[0-9,]+\ bytes\ in\ [0-9,]+\ blocks\ are\ (definitely\ lost|indirectly\ lost|possibly\ lost|still\ reachable)\ in\ loss\ record\ [0-9]+\ of\ [0-9]+$ ]] \
            || [[ "$message" =~ ^[[:space:]]+(at|by)\ 0x[[:xdigit:]]+:\ .+$ ]] \
            || [[ "$message" =~ ^[[:space:]]+For\ lists\ of\ detected\ and\ suppressed\ errors,\ rerun\ with:\ -s$ ]] \
            || [[ "$message" =~ ^[[:space:]]+ERROR\ SUMMARY:\ [0-9]+\ errors\ from\ [0-9]+\ contexts\ \(suppressed:\ [0-9]+\ from\ [0-9]+\)$ ]]; then
            :
        else
            _l3_memcheck_error "raw log contains an unexpected or malformed Valgrind row: ${line#==${summary_pid}==}" "$label"
            return 1
        fi
    done < "$raw"

    memcheck_line="$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}== Memcheck, a memory error detector$")"
    copyright_line="$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}== Copyright \\(C\\) 2002-2022, and GNU GPL'd, by Julian Seward et al\\.$")"
    version_line="$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}== Using Valgrind-3\\.22\\.[0-9]+ and LibVEX; rerun with -h for copyright info$")"
    command_line="$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}== Command: .+$")"
    parent_line="$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}== Parent PID: [0-9]+$")"
    if [[ "$memcheck_line" != 1 || "$copyright_line" != 1 || "$version_line" != 1 || "$command_line" != 1 || "$parent_line" != 1 ]] \
        || [[ "$(sed -n '1p' "$raw")" != "==${summary_pid}== Memcheck, a memory error detector" ]]; then
        _l3_memcheck_error "raw log lacks the complete Valgrind 3.22 header" "$label"
        return 1
    fi
    expected_command_line="==${summary_pid}== Command: $expected_command"
    reported_command_line="$(sed -n '4p' "$raw")"
    # Some host log collectors replace each non-ASCII path byte with `_` in
    # Valgrind's raw Command row. Keep the match exact after that reversible
    # display-only normalization; argv.json and the binary digest still bind
    # this reported path to the actual generated executable.
    redacted_command="$(LC_ALL=C printf '%s' "$expected_command" | LC_ALL=C sed 's/[^[:print:]]/_/g')"
    if [[ "$reported_command_line" != "$expected_command_line" \
        && "$reported_command_line" != "==${summary_pid}== Command: $redacted_command" ]]; then
        _l3_memcheck_error "raw log command does not match the captured native executable" "$label"
        return 1
    fi
    copyright_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}== Copyright \\(C\\) 2002-2022, and GNU GPL'd, by Julian Seward et al\\.$")"
    version_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}== Using Valgrind-3\\.22\\.[0-9]+ and LibVEX; rerun with -h for copyright info$")"
    command_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}== Command: .+$")"
    parent_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}== Parent PID: [0-9]+$")"
    if [[ "$copyright_line" != 2 || "$version_line" != 3 || "$command_line" != 4 || "$parent_line" != 5 ]]; then
        _l3_memcheck_error "Valgrind 3.22 header rows are missing, repeated, or out of order" "$label"
        return 1
    fi

    heap_line="$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}==[[:space:]]+HEAP SUMMARY:$")"
    in_use_line="$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}==[[:space:]]+in use at exit:")"
    total_line="$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}==[[:space:]]+total heap usage:")"
    footer_line="$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}== For lists of detected and suppressed errors, rerun with: -s$")"
    error_line="$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}==[[:space:]]+ERROR SUMMARY:")"
    if [[ "$heap_line" != 1 || "$in_use_line" != 1 || "$total_line" != 1 || "$footer_line" != 1 || "$error_line" != 1 ]] \
        || [[ "$(tail -n 1 -- "$raw")" != "==${summary_pid}== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)" ]]; then
        _l3_memcheck_error "raw log is missing, duplicates, or truncates a required summary/footer row" "$label"
        return 1
    fi

    line="$(grep -E "^==${summary_pid}==[[:space:]]+in use at exit:" "$raw")"
    if [[ ! "$line" =~ ^==${summary_pid}==[[:space:]]+in\ use\ at\ exit:\ ([0-9,]+)\ bytes\ in\ ([0-9,]+)\ blocks$ ]]; then
        _l3_memcheck_error "raw in-use row is malformed" "$label"
        return 1
    fi
    in_use_bytes="${BASH_REMATCH[1]}"
    in_use_blocks="${BASH_REMATCH[2]}"
    if ! _l3_memcheck_valid_count "$in_use_bytes" || ! _l3_memcheck_valid_count "$in_use_blocks"; then
        _l3_memcheck_error "raw in-use counts are malformed" "$label"
        return 1
    fi

    line="$(grep -E "^==${summary_pid}==[[:space:]]+total heap usage:" "$raw")"
    if [[ ! "$line" =~ ^==${summary_pid}==[[:space:]]+total\ heap\ usage:\ ([0-9,]+)\ allocs,\ ([0-9,]+)\ frees,\ ([0-9,]+)\ bytes\ allocated$ ]]; then
        _l3_memcheck_error "raw total-heap row is malformed" "$label"
        return 1
    fi
    allocations="${BASH_REMATCH[1]}"
    frees="${BASH_REMATCH[2]}"
    allocated_bytes="${BASH_REMATCH[3]}"
    if ! _l3_memcheck_valid_count "$allocations" || ! _l3_memcheck_valid_count "$frees" \
        || ! _l3_memcheck_valid_count "$allocated_bytes"; then
        _l3_memcheck_error "raw total-heap counts are malformed" "$label"
        return 1
    fi

    if [[ "$rows" == 3 ]]; then
        local expected_all_free
        expected_all_free="==${summary_pid}== in use at exit: 0 bytes in 0 blocks
==${summary_pid}== All heap blocks were freed -- no leaks are possible
==${summary_pid}== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)"
        if [[ "$(printf '%s\n' "${summary_rows[@]}")" != "$expected_all_free" ]]; then
            _l3_memcheck_error "three-row summary is not the exact all-free shape" "$label"
            return 1
        fi
        if [[ "$in_use_bytes" != 0 || "$in_use_blocks" != 0 ]] \
            || [[ "$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}==[[:space:]]+All heap blocks were freed -- no leaks are possible$")" != 1 ]] \
            || [[ "$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}==[[:space:]]+LEAK SUMMARY:$")" != 0 ]] \
            || [[ "$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}==[[:space:]]+(definitely lost|indirectly lost|possibly lost|still reachable|suppressed):")" != 0 ]] \
            || [[ "$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}==[[:space:]]+[0-9,]+ bytes in [0-9,]+ blocks are .+ in loss record [0-9]+ of [0-9]+$")" != 0 ]]; then
            _l3_memcheck_error "all-free raw log contains non-free leak rows" "$label"
            return 1
        fi
        if ! _l3_memcheck_decimal_ge "$allocations" "$frees" \
            || ! _l3_memcheck_decimal_ge "$frees" "$allocations"; then
            _l3_memcheck_error "all-free allocation and free totals differ" "$label"
            return 1
        fi
        in_use_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}==[[:space:]]+in use at exit:")"
        total_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}==[[:space:]]+total heap usage:")"
        free_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}==[[:space:]]+All heap blocks were freed -- no leaks are possible$")"
        footer_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}== For lists of detected and suppressed errors, rerun with: -s$")"
        error_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}== ERROR SUMMARY: 0 errors from 0 contexts \\(suppressed: 0 from 0\\)$")"
        if (( heap_line >= in_use_line || in_use_line >= total_line || total_line >= free_line \
            || free_line >= footer_line || footer_line >= error_line )); then
            _l3_memcheck_error "all-free raw sections are out of order" "$label"
            return 1
        fi
        _l3_memcheck_validate_layout "$raw" "$summary_pid" "$rows" "$expected_command" "$label" || return 1
        return 0
    fi

    if [[ "${summary_rows[0]}" != "==${summary_pid}== definitely lost: 0 bytes in 0 blocks" \
        || "${summary_rows[1]}" != "==${summary_pid}== indirectly lost: 0 bytes in 0 blocks" \
        || "${summary_rows[2]}" != "==${summary_pid}== possibly lost: 0 bytes in 0 blocks" \
        || "${summary_rows[4]}" != "==${summary_pid}== suppressed: 0 bytes in 0 blocks" \
        || "${summary_rows[5]}" != "==${summary_pid}== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)" ]]; then
        _l3_memcheck_error "six-row summary has a missing or nonzero ownership/error row" "$label"
        return 1
    fi
    if [[ ! "${summary_rows[3]}" =~ ^==${summary_pid}==\ still\ reachable:\ ([0-9,]+)\ bytes\ in\ ([0-9,]+)\ blocks$ ]]; then
        _l3_memcheck_error "six-row summary has a malformed still-reachable row" "$label"
        return 1
    fi
    reachable_bytes="${BASH_REMATCH[1]}"
    reachable_blocks="${BASH_REMATCH[2]}"
    if ! _l3_memcheck_valid_count "$reachable_bytes" || ! _l3_memcheck_valid_count "$reachable_blocks" \
        || [[ "$(_l3_memcheck_normalize_count "$reachable_bytes")" == 0 || "$(_l3_memcheck_normalize_count "$reachable_blocks")" == 0 ]]; then
        _l3_memcheck_error "reachable summary must contain positive, well-formed counts" "$label"
        return 1
    fi
    if ! _l3_memcheck_decimal_ge "$allocations" "$frees"; then
        _l3_memcheck_error "reachable heap frees exceed allocations" "$label"
        return 1
    fi
    if [[ "$in_use_bytes" != "$reachable_bytes" || "$in_use_blocks" != "$reachable_blocks" ]]; then
        _l3_memcheck_error "in-use and still-reachable totals differ" "$label"
        return 1
    fi

    if [[ "$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}==[[:space:]]+All heap blocks were freed -- no leaks are possible$")" != 0 ]] \
        || [[ "$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}==[[:space:]]+LEAK SUMMARY:$")" != 1 ]]; then
        _l3_memcheck_error "reachable raw log has an invalid all-free/leak-summary shape" "$label"
        return 1
    fi
    for line in 'definitely lost' 'indirectly lost' 'possibly lost' 'still reachable' 'suppressed'; do
        count="$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}==[[:space:]]+${line}:")"
        if [[ "$count" != 1 ]]; then
            _l3_memcheck_error "raw leak summary must contain exactly one ${line} row" "$label"
            return 1
        fi
    done
    for line in 'definitely lost' 'indirectly lost' 'possibly lost' 'suppressed'; do
        count="$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}==[[:space:]]+${line}: 0 bytes in 0 blocks$")"
        if [[ "$count" != 1 ]]; then
            _l3_memcheck_error "raw leak summary has missing or nonzero ${line} row" "$label"
            return 1
        fi
    done
    reachable_line="$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}==[[:space:]]+still reachable: ${reachable_bytes} bytes in ${reachable_blocks} blocks$")"
    leak_line="$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}== LEAK SUMMARY:$")"
    loss_records="$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}== [0-9,]+ bytes in [0-9,]+ blocks are still reachable in loss record [0-9]+ of [0-9]+$")"
    stack_frames="$(_l3_memcheck_count_regex "$raw" "^==${summary_pid}==[[:space:]]+(at|by) 0x[[:xdigit:]]+: .+$")"
    if [[ "$reachable_line" != 1 || "$loss_records" -lt 1 || "$stack_frames" -lt 1 ]]; then
        _l3_memcheck_error "reachable raw log lacks matching totals or loss-record stack details" "$label"
        return 1
    fi
    in_use_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}==[[:space:]]+in use at exit:")"
    total_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}==[[:space:]]+total heap usage:")"
    leak_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}== LEAK SUMMARY:$")"
    local first_loss_line definitely_line indirectly_line possibly_line suppressed_line
    first_loss_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}== [0-9,]+ bytes in [0-9,]+ blocks are still reachable in loss record [0-9]+ of [0-9]+$")"
    definitely_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}==[[:space:]]+definitely lost:")"
    indirectly_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}==[[:space:]]+indirectly lost:")"
    possibly_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}==[[:space:]]+possibly lost:")"
    footer_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}== For lists of detected and suppressed errors, rerun with: -s$")"
    error_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}== ERROR SUMMARY: 0 errors from 0 contexts \\(suppressed: 0 from 0\\)$")"
    reachable_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}==[[:space:]]+still reachable: ${reachable_bytes} bytes in ${reachable_blocks} blocks$")"
    suppressed_line="$(_l3_memcheck_line_number "$raw" "^==${summary_pid}==[[:space:]]+suppressed:")"
    if (( heap_line >= in_use_line || in_use_line >= total_line || total_line >= first_loss_line \
        || first_loss_line >= leak_line || leak_line >= definitely_line \
        || definitely_line >= indirectly_line || indirectly_line >= possibly_line \
        || possibly_line >= reachable_line || reachable_line >= suppressed_line \
        || suppressed_line >= footer_line || footer_line >= error_line )); then
        _l3_memcheck_error "reachable raw sections are truncated or out of order" "$label"
        return 1
    fi
    _l3_memcheck_validate_layout "$raw" "$summary_pid" "$rows" "$expected_command" "$label" \
        "$reachable_bytes" "$reachable_blocks" || return 1
    return 0
}
