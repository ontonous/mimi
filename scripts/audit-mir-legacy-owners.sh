#!/usr/bin/env bash
# Emit a reproducible static inventory of the retained surface-body owners.
# Dynamic zero-access probes live in the Rust test suite; this script keeps the
# production call-site count and owner labels auditable without parsing output
# from a compiler build.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC_DIR="$ROOT_DIR/src"

# These are the current, deliberately explicit compatibility boundaries.  A
# migration that removes an owner must update these values in the same change;
# an accidental new accessor or legacy call then fails this audit instead of
# silently widening the raw-AST surface.
readonly EXPECTED_OWNER_ACCESSOR_CALL_SITES=1
readonly EXPECTED_PRODUCTION_LEGACY_BODY_CALL_SITES=4
readonly EXPECTED_PRODUCTION_RAW_AST_CALL_SITES=0
readonly EXPECTED_PRODUCTION_COMPILE_FUNC_LEGACY_CALL_SITES=8
readonly EXPECTED_SCALAR_FFI_DIRECT_EXPRESSION_LEGACY_REFS=0
# The deletion conditions are part of the compatibility contract. Keep a
# digest beside each prose condition so scope changes are detected by the
# gate and reviewed together with the migration that changes the condition.
readonly EXPECTED_CODEGEN_OWNER_CONDITION_DIGEST=2568b029a170043114b68ea0d35f9061617cc969ea2f9c8b7288d3d7f4f5e40c
readonly EXPECTED_FLOW_VERIFIER_OWNER_CONDITION_DIGEST=7b995e0731f7fd98c881107d01a68f18f72c254a49df0239f62056f80b8d541a
readonly EXPECTED_FFI_VERIFIER_OWNER_CONDITION_DIGEST=f760de7d0a7f11cb703995b9069da4a510935d9ffcb34715418516aa5cc6b5fe
readonly EXPECTED_DUAL_VERIFIER_OWNER_CONDITION_DIGEST=ff30fc215553993cb653cab0746ce34c3b275936e8d699ac25856f8201d210f4
readonly EXPECTED_OWNER_CONDITION_SET_DIGEST=60261d5b3a6c63c9b0ed4604a72e49505f33501960061ba8e8f4540bbbd67e3f
audit_failed=0
closed_scalar_marker_sequence=()

emit_closed_scalar_marker() {
    local marker="$1"
    closed_scalar_marker_sequence+=("$marker")
    printf '%s\n' "$marker"
}

printf 'schema=canonical-mir-legacy-owner-audit-v1\n'
printf 'root=%s\n' "$ROOT_DIR"

# Keep one executable evidence case for each retained owner.  The cases are
# deliberately split between a closed scalar MIR route (which must bypass all
# owners) and a representative compatibility shape (which must still reach
# the named owner).  If a future migration removes or renames one of these
# tests without replacing its evidence, this audit fails instead of silently
# turning an owner into an undocumented deletion candidate.
owner_evidence() {
    local owner="$1"
    local source_file="$2"
    local test_name="$3"
    local scope="$4"
    local expected_marker="$5"
    local test_path="$ROOT_DIR/$source_file"
    if ! rg -q "^[[:space:]]*fn ${test_name}\\(" "$test_path"; then
        printf 'owner_audit_error=%s missing_evidence_test=%s::%s\n' \
            "$owner" "$source_file" "$test_name" >&2
        audit_failed=1
        return
    fi
    if ! sed -n "/^[[:space:]]*fn ${test_name}(/,/^[[:space:]]*#\\[test\\]/p" "$test_path" \
        | rg -q "${expected_marker}"; then
        printf 'owner_audit_error=%s evidence_test_missing_marker=%s marker=%s\n' \
            "$owner" "$test_name" "$expected_marker" >&2
        audit_failed=1
        return
    fi
    printf 'owner=%s evidence_scope=%s evidence_test=%s::%s evidence_marker=%s\n' \
        "$owner" "$scope" "$source_file" "$test_name" "$expected_marker"
}

owner_evidence \
    CodegenLegacyRemainder \
    src/codegen/tests.rs \
    compile_checked_tags_unmigrated_generic_body_with_legacy_owner \
    'closed scalar bypass + generic compatibility reaches codegen remainder' \
    'LegacyBodyConsumer::CodegenLegacyRemainder'
owner_evidence \
    FlowVerifierCompatibility \
    src/verifier/tests.rs \
    compatibility_verifier_access_is_explicitly_tagged \
    'closed scalar bypass + non-closed contract reaches Flow/Z3 compatibility' \
    'LegacyBodyConsumer::FlowVerifierCompatibility'
owner_evidence \
    FfiVerifierCompatibility \
    src/tests/canonical_scalar_ffi.rs \
    ffi_checked_preserves_legacy_for_unmigrated_string_contract \
    'closed scalar bypass + string FFI contract reaches FFI compatibility' \
    'LegacyBodyConsumer::FfiVerifierCompatibility'
owner_evidence \
    DualVerifierCompatibility \
    src/verifier/tests.rs \
    compatibility_verifier_access_is_explicitly_tagged \
    'closed scalar bypass + secondary Flow/VIR compatibility remains reachable' \
    'LegacyBodyConsumer::DualVerifierCompatibility'

# Keep the retained accessor tied to the function that owns its compatibility
# boundary. A count-only check would miss an accidental move into a closed MIR
# route or a new helper that widens the raw-AST surface while preserving the
# same number of calls.
owner_accessor_context() {
    local owner="$1"
    local source_file="$2"
    local start_function="$3"
    local end_function="$4"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg "legacy_body_file\\([^)]*LegacyBodyConsumer::${owner}" >/dev/null; then
        printf 'owner_audit_error=%s accessor_context_missing=%s::%s\n' \
            "$owner" "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    printf 'owner=%s accessor_context=%s::%s\n' \
        "$owner" "$source_file" "$start_function"
}

# The closed scalar guard may live in the public entry function while the
# retained accessor is delegated to a compatibility helper. Verify the guard
# in its owning entry context separately so a count-only audit cannot hide a
# route that re-enters the legacy owner before canonical admission.
owner_closed_route_guard() {
    local owner="$1"
    local source_file="$2"
    local start_function="$3"
    local end_function="$4"
    local guard_name="$5"
    local guard_pattern="$6"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "$guard_pattern" >/dev/null; then
        printf 'owner_audit_error=%s closed_route_guard_missing=%s::%s\n' \
            "$owner" "$source_file" "$guard_name" >&2
        audit_failed=1
        return
    fi
    printf 'owner=%s closed_route_guard=%s context=%s::%s\n' \
        "$owner" "$guard_name" "$source_file" "$start_function"
}

# Bind the scalar route's receipt label to the canonical profile declaration.
# This prevents a renamed or copied receipt from silently drifting away from
# the profile that the closed-route guard is meant to protect.
route_receipt_profile_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local profile_name="$4"
    local receipt_label="$5"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "route_receipt(\"${receipt_label}\")" >/dev/null; then
        printf 'owner_audit_error=scalar_route_receipt_missing=%s::%s label=%s\n' \
            "$source_file" "$start_function" "$receipt_label" >&2
        audit_failed=1
        return
    fi
    if ! rg -F "Self::${profile_name} => \"${receipt_label}\"" \
        "$ROOT_DIR/src/core/mir/route.rs" >/dev/null; then
        printf 'owner_audit_error=scalar_route_profile_receipt_mapping_missing=CanonicalMirRouteProfile::%s label=%s\n' \
            "$profile_name" "$receipt_label" >&2
        audit_failed=1
        return
    fi
    printf 'scalar_route_receipt_binding=CanonicalMirRouteProfile::%s->%s consumer=%s::%s\n' \
        "$profile_name" "$receipt_label" "$source_file" "$start_function"
}

owner_accessor_context \
    CodegenLegacyRemainder \
    src/codegen/compile.rs \
    'fn compile_file_with_resolved(' \
    'fn compile_file_inner('
owner_accessor_context \
    FlowVerifierCompatibility \
    src/verifier/mod.rs \
    'pub fn verify_checked(' \
    'pub fn verify_ffi_source('
owner_accessor_context \
    FfiVerifierCompatibility \
    src/verifier/mod.rs \
    'fn verify_ffi_checked_with_source_hash(' \
    'pub fn is_z3_available('
owner_accessor_context \
    DualVerifierCompatibility \
    src/verifier/mod.rs \
    'pub fn verify_checked_dual(' \
    'fn verify_closed_mir_program('

owner_closed_route_guard \
    CodegenLegacyRemainder \
    src/codegen/compile.rs \
    'pub fn compile_checked(' \
    'fn try_compile_exact_migrated_mir_island(' \
    try_compile_exact_migrated_mir_island \
    'if let Some(canonical) = self.try_compile_exact_migrated_mir_island(program)?'
owner_closed_route_guard \
    FlowVerifierCompatibility \
    src/verifier/mod.rs \
    'pub fn verify_checked(' \
    'pub fn verify_ffi_source(' \
    verify_closed_mir_program \
    'if let Some(results) = verify_closed_mir_program(program, source_hash.clone())?'
owner_closed_route_guard \
    FfiVerifierCompatibility \
    src/verifier/mod.rs \
    'fn verify_ffi_checked_with_source_hash(' \
    'pub fn is_z3_available(' \
    materialize_closed_mir_island \
    'if let Some(canonical) = materialize_closed_mir_island('
owner_closed_route_guard \
    DualVerifierCompatibility \
    src/verifier/mod.rs \
    'pub fn verify_checked_dual(' \
    'fn verify_closed_mir_program(' \
    verify_closed_mir_program \
    'if let Some(results) = verify_closed_mir_program(program, source_hash.clone())?'

route_receipt_profile_binding \
    src/main/canonical_dispatch.rs \
    'fn select_scalar_ffi_route(' \
    'fn reject_migrated_candidates(' \
    ScalarFfi \
    scalar-ffi-v1

# Keep the consumer-specific receipt labels anchored to the entry points that
# consume the same canonical graph. The labels may differ by consumer, but a
# missing label means that a direct API could silently stop binding its proof
# or ABI snapshot to the route it executed.
consumer_receipt_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local receipt_label="$4"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "route_receipt(\"${receipt_label}\")" >/dev/null; then
        printf 'owner_audit_error=consumer_receipt_missing=%s::%s label=%s\n' \
            "$source_file" "$start_function" "$receipt_label" >&2
        audit_failed=1
        return
    fi
    printf 'consumer_receipt_binding=%s consumer=%s::%s\n' \
        "$receipt_label" "$source_file" "$start_function"
}

consumer_receipt_binding \
    src/codegen/compile.rs \
    'pub fn compile_checked(' \
    'fn try_compile_exact_migrated_mir_island(' \
    native-direct-v1
consumer_receipt_binding \
    src/verifier/mod.rs \
    'fn verify_ffi_checked_with_source_hash(' \
    'pub fn is_z3_available(' \
    verify-ffi-v1

# Bind each consumer receipt to the canonical graph and, where applicable, to
# the caller's source provenance.  A label-only check could survive an
# accidental call that manufactures a receipt but drops it before invoking
# the consumer; checking the exact adapter call keeps the immutable MIR
# identity and source hash on the same path.
consumer_receipt_provenance_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local receipt_label="$4"
    local invocation_pattern="$5"
    local provenance_kind="$6"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "$invocation_pattern" >/dev/null; then
        printf 'owner_audit_error=consumer_receipt_invocation_missing=%s::%s label=%s\n' \
            "$source_file" "$start_function" "$receipt_label" >&2
        audit_failed=1
        return
    fi
    printf 'consumer_receipt_provenance_binding=%s graph=canonical provenance=%s consumer=%s::%s\n' \
        "$receipt_label" "$provenance_kind" "$source_file" "$start_function"
}

consumer_receipt_provenance_binding \
    src/codegen/compile.rs \
    'pub fn compile_checked(' \
    'fn try_compile_exact_migrated_mir_island(' \
    native-direct-v1 \
    'self.compile_mir_native_with_route_receipt(&canonical, &receipt)' \
    canonical-mir-graph
consumer_receipt_provenance_binding \
    src/verifier/mod.rs \
    'fn verify_ffi_checked_with_source_hash(' \
    'pub fn is_z3_available(' \
    verify-ffi-v1 \
    'verify_ffi_mir_with_route_receipt(&canonical, &receipt, source_hash)' \
    canonical-mir-graph+source-hash

# Keep the FFI verifier's receipt tied to the same canonical route profile that
# admitted the island.  A source-hash forwarding check alone would still allow
# a copied receipt to be paired with a different profile; requiring the profile
# expression in the consumer context makes that drift fail closed.
consumer_receipt_profile_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local receipt_label="$4"
    local profile_name="$5"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "CanonicalMirRouteProfile::${profile_name}" >/dev/null; then
        printf 'owner_audit_error=consumer_receipt_profile_missing=%s::%s label=%s profile=%s\n' \
            "$source_file" "$start_function" "$receipt_label" "$profile_name" >&2
        audit_failed=1
        return
    fi
    printf 'consumer_receipt_profile_binding=%s profile=CanonicalMirRouteProfile::%s consumer=%s::%s\n' \
        "$receipt_label" "$profile_name" "$source_file" "$start_function"
}

consumer_receipt_profile_binding \
    src/verifier/mod.rs \
    'fn verify_ffi_checked_with_source_hash(' \
    'pub fn is_z3_available(' \
    verify-ffi-v1 \
    ScalarFfi

if ! rg -q '^[[:space:]]*fn scalar_ffi_c_abi_and_side_effect_order_match_three_consumers\(' \
    "$ROOT_DIR/src/tests/canonical_scalar_ffi.rs"; then
    printf 'owner_audit_error=missing_closed_scalar_zero_owner_evidence\n' >&2
    audit_failed=1
else
    emit_closed_scalar_marker 'closed_scalar_zero_owner_evidence=src/tests/canonical_scalar_ffi.rs::scalar_ffi_c_abi_and_side_effect_order_match_three_consumers'
    if ! sed -n "/^[[:space:]]*fn scalar_ffi_c_abi_and_side_effect_order_match_three_consumers(/,/^[[:space:]]*#\[test\]/p" \
        "$ROOT_DIR/src/tests/canonical_scalar_ffi.rs" | rg 'test_legacy_body_access\(\)\.is_empty\(\)' >/dev/null; then
        printf 'owner_audit_error=closed_scalar_zero_owner_evidence_missing_empty_legacy_assertion\n' >&2
        audit_failed=1
    else
        emit_closed_scalar_marker 'closed_scalar_zero_owner_evidence_marker=test_legacy_body_access().is_empty()'
    fi
fi

if ! rg -q '^[[:space:]]*fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts\(' \
    "$ROOT_DIR/tests/real_world_cli.rs"; then
    printf 'owner_audit_error=missing_closed_scalar_cli_evidence\n' >&2
    audit_failed=1
else
    emit_closed_scalar_marker 'closed_scalar_cli_evidence=tests/real_world_cli.rs::canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts'
    if ! sed -n "/^[[:space:]]*fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts(/,/^[[:space:]]*#\[test\]/p" \
        "$ROOT_DIR/tests/real_world_cli.rs" | rg 'for contracts in \[true, false\]' >/dev/null; then
        printf 'owner_audit_error=closed_scalar_cli_evidence_missing_contract_matrix\n' >&2
        audit_failed=1
    elif ! sed -n "/^[[:space:]]*fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts(/,/^[[:space:]]*#\[test\]/p" \
        "$ROOT_DIR/tests/real_world_cli.rs" | rg 'for explicit_mir in \[false, true\]' >/dev/null; then
        printf 'owner_audit_error=closed_scalar_cli_evidence_missing_mir_matrix\n' >&2
        audit_failed=1
    elif ! sed -n "/^[[:space:]]*fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts(/,/^[[:space:]]*#\[test\]/p" \
        "$ROOT_DIR/tests/real_world_cli.rs" | rg 'assert!\(!.*contains\("canonical route disposition: legacy"\)\)' >/dev/null; then
        printf 'owner_audit_error=closed_scalar_cli_evidence_missing_legacy_route_assertion\n' >&2
        audit_failed=1
    else
        cli_evidence_body="$(sed -n "/^[[:space:]]*fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts(/,/^[[:space:]]*#\[test\]/p" "$ROOT_DIR/tests/real_world_cli.rs")"
        missing_abi=0
        for abi_symbol in mir_ffi_i32 mir_ffi_i64 mir_ffi_bool mir_ffi_f64 mir_ffi_store; do
            if ! printf '%s\n' "$cli_evidence_body" | rg "\"${abi_symbol}\"" >/dev/null; then
                printf 'owner_audit_error=closed_scalar_cli_evidence_missing_abi_symbol symbol=%s\n' "$abi_symbol" >&2
                missing_abi=1
            fi
        done
        if [ "$missing_abi" -ne 0 ]; then
            audit_failed=1
        else
            emit_closed_scalar_marker 'closed_scalar_cli_matrix_marker=contracts:[true, false];explicit_mir:[false, true]'
            emit_closed_scalar_marker 'closed_scalar_cli_abi_marker=mir_ffi_i32,mir_ffi_i64,mir_ffi_bool,mir_ffi_f64,mir_ffi_store;legacy_route_asserted_absent'
        fi
    fi
fi

expected_closed_scalar_marker_sequence=(
    'closed_scalar_zero_owner_evidence=src/tests/canonical_scalar_ffi.rs::scalar_ffi_c_abi_and_side_effect_order_match_three_consumers'
    'closed_scalar_zero_owner_evidence_marker=test_legacy_body_access().is_empty()'
    'closed_scalar_cli_evidence=tests/real_world_cli.rs::canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts'
    'closed_scalar_cli_matrix_marker=contracts:[true, false];explicit_mir:[false, true]'
    'closed_scalar_cli_abi_marker=mir_ffi_i32,mir_ffi_i64,mir_ffi_bool,mir_ffi_f64,mir_ffi_store;legacy_route_asserted_absent'
)
if [ "${closed_scalar_marker_sequence[*]}" != "${expected_closed_scalar_marker_sequence[*]}" ]; then
    printf 'owner_audit_error=closed_scalar_evidence_marker_sequence_drift\n' >&2
    audit_failed=1
else
    printf 'scalar_evidence_marker_sequence_status=ok\n'
fi

owner_count=0
condition_inventory=''
for owner in \
    CodegenLegacyRemainder \
    FlowVerifierCompatibility \
    FfiVerifierCompatibility \
    DualVerifierCompatibility; do
    owner_count=$((owner_count + 1))
    matches="$(rg -n \
        --glob '*.rs' \
        --glob '!**/tests.rs' \
        --glob '!src/tests/**' \
        "legacy_body_file\\([^)]*LegacyBodyConsumer::${owner}" \
        "$SRC_DIR" || true)"
    count=0
    if [ -n "$matches" ]; then
        count="$(printf '%s\n' "$matches" | wc -l)"
    fi
    printf 'owner=%s production_accessor_call_sites=%s\n' "$owner" "$count"
    if [ -n "$matches" ]; then
        printf '%s\n' "$matches"
    fi
    if [ "$count" -ne "$EXPECTED_OWNER_ACCESSOR_CALL_SITES" ]; then
        printf 'owner_audit_error=%s expected_accessor_call_sites=%s actual=%s\n' \
            "$owner" "$EXPECTED_OWNER_ACCESSOR_CALL_SITES" "$count" >&2
        audit_failed=1
    fi
    case "$owner" in
        CodegenLegacyRemainder)
            dependency_class='legacy-codegen-remainder'
            deletion_blocker='all legacy body classes and compile_func_legacy production callers must be removed'
            condition='delete after every legacy body class is lowered to MIR and compile_func_legacy has zero production callers'
            ;;
        FlowVerifierCompatibility)
            dependency_class='flow-body-compatibility'
            deletion_blocker='non-closed Flow shapes still require the AST/Z3 compatibility encoder'
            condition='delete after the Flow verifier consumes canonical MIR contracts for every non-closed Flow shape'
            ;;
        FfiVerifierCompatibility)
            dependency_class='ffi-declaration-compatibility'
            deletion_blocker='unmigrated FFI declaration semantics still require the compatibility encoder'
            condition='delete after string/aggregate/variadic/errno/mode/ensures FFI declarations have complete MIR receipts and proofs'
            ;;
        DualVerifierCompatibility)
            dependency_class='secondary-flow-vir-compatibility'
            deletion_blocker='the secondary Flow/VIR engine is not MIR-native for every compatibility profile'
            condition='delete after the secondary Flow/VIR engine is retired or is MIR-native for every compatibility profile'
            ;;
    esac
    if [ "$count" -eq 0 ]; then
        owner_status='unreachable'
        accessor_blocker='owner accessor is unreachable'
    else
        owner_status='retained'
        accessor_blocker='owner accessor remains reachable'
    fi
    # Accessor reachability is only one deletion prerequisite. Keep the
    # readiness gate conservative until a migration explicitly proves the
    # owner-specific condition above; a zero accessor count alone must never
    # be reported as permission to delete the owner.
    deletion_ready=0
    condition_digest="$(printf '%s' "$condition" | sha256sum | awk '{print $1}')"
    case "$owner" in
        CodegenLegacyRemainder)
            expected_condition_digest="$EXPECTED_CODEGEN_OWNER_CONDITION_DIGEST"
            ;;
        FlowVerifierCompatibility)
            expected_condition_digest="$EXPECTED_FLOW_VERIFIER_OWNER_CONDITION_DIGEST"
            ;;
        FfiVerifierCompatibility)
            expected_condition_digest="$EXPECTED_FFI_VERIFIER_OWNER_CONDITION_DIGEST"
            ;;
        DualVerifierCompatibility)
            expected_condition_digest="$EXPECTED_DUAL_VERIFIER_OWNER_CONDITION_DIGEST"
            ;;
    esac
    printf 'owner=%s owner_deletion_condition_digest=%s\n' "$owner" "$condition_digest"
    if [ "$condition_digest" != "$expected_condition_digest" ]; then
        printf 'owner_audit_error=%s owner_deletion_condition_digest expected=%s actual=%s\n' \
            "$owner" "$expected_condition_digest" "$condition_digest" >&2
        audit_failed=1
    fi
    condition_inventory+="${owner}=${condition_digest}"$'\n'
    printf 'owner=%s status=%s dependency_class=%s owner_deletion_ready=%s owner_deletion_blocker=%s; %s\n' \
        "$owner" "$owner_status" "$dependency_class" "$deletion_ready" "$accessor_blocker" "$deletion_blocker"
    printf 'owner_deletion_condition=%s\n' "$condition"
done
condition_set_digest="$(printf '%s' "$condition_inventory" | sha256sum | awk '{print $1}')"
printf 'owner_deletion_condition_set_digest=%s\n' "$condition_set_digest"
if [ "$condition_set_digest" != "$EXPECTED_OWNER_CONDITION_SET_DIGEST" ]; then
    printf 'owner_audit_error=owner_deletion_condition_set_digest expected=%s actual=%s\n' \
        "$EXPECTED_OWNER_CONDITION_SET_DIGEST" "$condition_set_digest" >&2
    audit_failed=1
fi

body_refs="$(rg -n \
    --glob '*.rs' \
    --glob '!**/tests.rs' \
    --glob '!src/tests/**' \
    'legacy_body_file\(' \
    "$SRC_DIR" | rg -v ':[0-9]+:[[:space:]]*(//|.*fn legacy_body_file)' || true)"
body_count=0
if [ -n "$body_refs" ]; then
    body_count="$(printf '%s\n' "$body_refs" | wc -l)"
fi
printf 'production_legacy_body_file_call_sites=%s\n' "$body_count"
if [ "$body_count" -ne "$EXPECTED_PRODUCTION_LEGACY_BODY_CALL_SITES" ]; then
    printf 'owner_audit_error=production_legacy_body_file_call_sites expected=%s actual=%s\n' \
        "$EXPECTED_PRODUCTION_LEGACY_BODY_CALL_SITES" "$body_count" >&2
    audit_failed=1
fi

raw_ast_refs="$(rg -n \
    --glob '*.rs' \
    --glob '!**/tests.rs' \
    --glob '!src/tests/**' \
    '[[:alnum:]_)]\.raw_ast\(' \
    "$SRC_DIR" || true)"
raw_ast_count=0
if [ -n "$raw_ast_refs" ]; then
    raw_ast_count="$(printf '%s\n' "$raw_ast_refs" | wc -l)"
fi
printf 'production_raw_ast_call_sites=%s\n' "$raw_ast_count"
if [ "$raw_ast_count" -ne "$EXPECTED_PRODUCTION_RAW_AST_CALL_SITES" ]; then
    printf 'owner_audit_error=production_raw_ast_call_sites expected=%s actual=%s\n' \
        "$EXPECTED_PRODUCTION_RAW_AST_CALL_SITES" "$raw_ast_count" >&2
    audit_failed=1
fi

legacy_refs="$(rg -n \
    --glob '*.rs' \
    --glob '!**/tests.rs' \
    --glob '!src/tests/**' \
    'compile_func_legacy\(' \
    "$SRC_DIR" || true)"
legacy_count=0
if [ -n "$legacy_refs" ]; then
    legacy_count="$(printf '%s\n' "$legacy_refs" | wc -l)"
fi
legacy_call_sites="$(printf '%s\n' "$legacy_refs" | rg -v 'fn compile_func_legacy' || true)"
legacy_call_count=0
if [ -n "$legacy_call_sites" ]; then
    legacy_call_count="$(printf '%s\n' "$legacy_call_sites" | wc -l)"
fi
printf 'production_compile_func_legacy_call_sites=%s\n' "$legacy_call_count"
if [ "$legacy_call_count" -ne "$EXPECTED_PRODUCTION_COMPILE_FUNC_LEGACY_CALL_SITES" ]; then
    printf 'owner_audit_error=production_compile_func_legacy_call_sites expected=%s actual=%s\n' \
        "$EXPECTED_PRODUCTION_COMPILE_FUNC_LEGACY_CALL_SITES" "$legacy_call_count" >&2
    audit_failed=1
fi
printf 'owner_count=%s\n' "$owner_count"

# R6-15 removed the former direct-expression-only scalar FFI admission helper.
# Keep its names in this audit so a future compatibility refactor cannot
# silently reintroduce a second FFI route policy.
scalar_ffi_legacy_refs="$(rg -n \
    --glob '*.rs' \
    --glob '!**/tests.rs' \
    --glob '!src/tests/**' \
    'contains_scalar_ffi_contract_candidate|direct_scalar_ffi_callees|scalar_ffi_contract_expr' \
    "$SRC_DIR" || true)"
scalar_ffi_legacy_count=0
if [ -n "$scalar_ffi_legacy_refs" ]; then
    scalar_ffi_legacy_count="$(printf '%s\n' "$scalar_ffi_legacy_refs" | wc -l)"
    printf '%s\n' "$scalar_ffi_legacy_refs"
fi
printf 'scalar_ffi_direct_expression_legacy_refs=%s\n' "$scalar_ffi_legacy_count"
if [ "$scalar_ffi_legacy_count" -ne "$EXPECTED_SCALAR_FFI_DIRECT_EXPRESSION_LEGACY_REFS" ]; then
    printf 'owner_audit_error=scalar_ffi_direct_expression_legacy_refs expected=%s actual=%s\n' \
        "$EXPECTED_SCALAR_FFI_DIRECT_EXPRESSION_LEGACY_REFS" "$scalar_ffi_legacy_count" >&2
    audit_failed=1
fi

printf 'dynamic_probe_command=%s\n' \
    'cargo test --features llvm18-host-dynamic legacy_body_access -- --test-threads=1'

if [ "$audit_failed" -ne 0 ]; then
    printf 'audit_status=failed\n'
    exit 1
fi
printf 'audit_status=ok\n'
