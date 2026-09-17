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

# The AST-free bytecode adapter must retain the route receipt that admitted
# the MIR graph.  Without this hand-off the compile-time receipt check would
# end at emission and a reusable VM could no longer prove that its binding
# snapshot belongs to one canonical route identity.
bytecode_route_receipt_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local pattern="$4"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "$pattern" >/dev/null; then
        printf 'owner_audit_error=bytecode_route_receipt_binding_missing=%s::%s pattern=%s\n' \
            "$source_file" "$start_function" "$pattern" >&2
        audit_failed=1
        return
    fi
    printf 'bytecode_route_receipt_binding=%s consumer=%s::%s\n' \
        "$pattern" "$source_file" "$start_function"
}

bytecode_route_receipt_binding \
    src/interp/bytecode/mir.rs \
    'pub fn compile_mir_program_with_route_receipt(' \
    'fn compile_mir_program_inner(' \
    'compile_mir_program_inner(program, Some(receipt))'
bytecode_route_receipt_binding \
    src/interp/bytecode/mir.rs \
    'fn compile_mir_program_inner(' \
    'fn materialize_canonical_ffi_bindings(' \
    'canonical_ffi_route_receipt: if !has_canonical_ffi_bindings'
bytecode_route_receipt_binding \
    src/interp/bytecode/mir.rs \
    'fn compile_mir_program_inner(' \
    'fn materialize_canonical_ffi_bindings(' \
    'binding.route_receipt = Some(receipt.clone())'

bytecode_route_receipt_vm_guard() {
    local source_file="$1"
    local source
    source="$(cat "$ROOT_DIR/$source_file")"
    for pattern in \
        'canonical FFI binding manifest mixes route receipt identities' \
        'canonical FFI binding route receipt disagrees with program anchor' \
        'canonical FFI route receipt cannot be replayed at VM boundary'; do
        if ! printf '%s\n' "$source" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=bytecode_route_receipt_vm_guard_missing=%s pattern=%s\n' \
                "$source_file" "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'bytecode_route_receipt_vm_guard=program-anchor+identity-consistency+manifest-replay consumer=%s\n' \
        "$source_file"
}

bytecode_route_receipt_vm_guard src/interp/bytecode/vm.rs

# The native LLVM adapter is also a reusable single-program consumer.  Keep an
# independent receipt anchor beside its MIR digest so an equivalent profile
# replay is idempotent while a tampered in-memory route cannot be paired with
# an already emitted module.
native_route_receipt_anchor() {
    local field_source
    local consumer_source
    field_source="$(cat "$ROOT_DIR/src/codegen/mod.rs")"
    consumer_source="$(cat "$ROOT_DIR/src/codegen/mir/eligibility.rs")"
    if ! printf '%s\n' "$field_source" | rg -F "mir_native_route_receipt: Option<crate::core::mir::CanonicalMirRouteReceipt>" >/dev/null; then
        printf 'owner_audit_error=native_route_receipt_anchor_missing=src/codegen/mod.rs field\n' >&2
        audit_failed=1
        return
    fi
    for pattern in \
        'bound.same_semantic_identity(receipt)' \
        'self.mir_native_route_receipt = Some(receipt.clone())'; do
        if ! printf '%s\n' "$consumer_source" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=native_route_receipt_anchor_missing=src/codegen/mir/eligibility.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'native_route_receipt_anchor=program-anchor+semantic-replay consumer=src/codegen/mir/eligibility.rs\n'
}

native_route_receipt_anchor

# Proof cache identity must consume the same canonical receipt algorithm as
# native and bytecode route admission.  Keep this as a source-level tripwire
# so a verifier-local field list cannot silently drift from the shared MIR
# identity contract.
mir_route_cache_identity_binding() {
    if ! rg -F "pub fn semantic_identity_digest(&self) -> String" \
        "$ROOT_DIR/src/core/mir/receipt.rs" >/dev/null; then
        printf 'owner_audit_error=mir_route_cache_identity_missing=src/core/mir/receipt.rs\n' >&2
        audit_failed=1
        return
    fi
    if ! rg -F ".map(crate::core::mir::CanonicalMirRouteReceipt::semantic_identity_digest)" \
        "$ROOT_DIR/src/verifier/ctx.rs" >/dev/null; then
        printf 'owner_audit_error=mir_route_cache_identity_consumer_missing=src/verifier/ctx.rs\n' >&2
        audit_failed=1
        return
    fi
    printf 'mir_route_cache_identity_binding=shared-receipt-digest consumer=src/verifier/ctx.rs\n'
}

mir_route_cache_identity_binding

# Native typed receipts must cross the same manifest serialization boundary as
# bytecode and verifier receipts.  This catches a future field-order or parser
# drift before LLVM emission can accept a witness that another consumer cannot
# replay.
native_route_manifest_replay_binding() {
    local source
    source="$(cat "$ROOT_DIR/src/codegen/mir/eligibility.rs")"
    for pattern in \
        'let manifest = receipt.manifest_text()' \
        'CanonicalMirRouteReceipt::from_manifest(&manifest)' \
        'canonical route receipt changed during manifest replay'; do
        if ! printf '%s\n' "$source" | rg -F "$pattern" >/dev/null; then
            printf 'owner_audit_error=native_route_manifest_replay_missing=src/codegen/mir/eligibility.rs pattern=%s\n' \
                "$pattern" >&2
            audit_failed=1
            return
        fi
    done
    printf 'native_route_manifest_replay_binding=manifest-parse+identity-check consumer=src/codegen/mir/eligibility.rs\n'
}

native_route_manifest_replay_binding

# Bind the public source entry to the hash-bearing checked-program adapter.
# This keeps the caller's BLAKE3 provenance on the same path as the verifier
# receipt; a renamed call or an empty/hashless entry must fail closed.
source_hash_entry_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local call_pattern="$4"
    local hash_pattern="$5"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "$call_pattern" >/dev/null; then
        printf 'owner_audit_error=source_hash_entry_call_missing=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$context" | rg -F "$hash_pattern" >/dev/null; then
        printf 'owner_audit_error=source_hash_entry_hash_missing=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    printf 'source_hash_entry_binding=blake3-source-hash consumer=%s::%s\n' \
        "$source_file" "$start_function"
}

source_hash_entry_binding \
    src/verifier/mod.rs \
    'pub fn verify_ffi_source(' \
    'pub fn verify_ffi_checked(' \
    'verify_ffi_checked_with_source_hash(' \
    'blake3::hash(source.as_bytes()).to_hex().to_string(),'

# Tie the checked-program consumer's declared hash parameter to the exact
# route adapter argument.  This is intentionally separate from the public
# entry check so a parameter rename or a forwarding omission cannot hide
# behind a still-correct source entry marker.
source_hash_parameter_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local parameter_pattern="$4"
    local invocation_pattern="$5"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "$parameter_pattern" >/dev/null; then
        printf 'owner_audit_error=source_hash_parameter_missing=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$context" | rg -F "$invocation_pattern" >/dev/null; then
        printf 'owner_audit_error=source_hash_parameter_forward_missing=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    printf 'source_hash_parameter_binding=declared-and-forwarded consumer=%s::%s\n' \
        "$source_file" "$start_function"
}

source_hash_parameter_binding \
    src/verifier/mod.rs \
    'fn verify_ffi_checked_with_source_hash(' \
    'pub fn is_z3_available(' \
    'source_hash: String' \
    'verify_ffi_mir_with_route_receipt(&canonical, &receipt, source_hash)'

# Keep the FFI MIR adapter's proof artifacts sourced from the same hash passed
# into the MIR verifier.  The route-level check above proves the caller passes
# a hash; this check proves the adapter clones it into MIR verification and
# validates every returned artifact against that same value.
mir_verifier_source_provenance_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local execution_pattern="$4"
    local validation_pattern="$5"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "$execution_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_verifier_source_provenance_missing=%s::%s phase=mir-call\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$context" | rg -F "$validation_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_verifier_source_provenance_missing=%s::%s phase=artifact-validation\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    printf 'mir_verifier_source_provenance_binding=single-source-hash consumer=%s::%s\n' \
        "$source_file" "$start_function"
}

mir_verifier_source_provenance_binding \
    src/verifier/mod.rs \
    'pub fn verify_ffi_mir_with_source_hash(' \
    'pub fn verify_ffi_mir_with_route_receipt(' \
    'let results = mir::verify_ffi_program(program, source_hash.clone())?' \
    'validate_mir_result_provenance(&results, &receipt, &source_hash, "verify_ffi_mir")?'

# Keep the shared artifact validator bound to every immutable identity field.
# Source hash, MIR digest, and route receipt are one proof witness; validating
# only one of them would permit a copied artifact to cross a canonical route.
mir_result_identity_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local source_pattern="$4"
    local mir_pattern="$5"
    local receipt_pattern="$6"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "$source_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_result_identity_missing=%s::%s field=source-hash\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$context" | rg -F "$mir_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_result_identity_missing=%s::%s field=mir-hash\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$context" | rg -F "$receipt_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_result_identity_missing=%s::%s field=route-receipt\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    printf 'mir_result_identity_binding=source-hash+mir-hash+route-receipt consumer=%s::%s\n' \
        "$source_file" "$start_function"
}

mir_result_identity_binding \
    src/verifier/mod.rs \
    'fn validate_mir_result_provenance(' \
    'pub fn verify_mir_with_route_manifest(' \
    'if artifact.source_hash != source_hash' \
    'if artifact.mir_hash != receipt.mir_digest' \
    'if artifact.mir_route_receipt.as_ref() != Some(receipt)'

# Both direct MIR entry points must validate a caller-supplied receipt before
# proof execution. This keeps the public general verifier and FFI verifier on
# the same fail-closed route boundary.
mir_route_receipt_validation_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F '.validate_against_program(program)' >/dev/null; then
        printf 'owner_audit_error=mir_route_receipt_validation_missing=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    printf 'mir_route_receipt_validation_binding=validate-against-program consumer=%s::%s\n' \
        "$source_file" "$start_function"
}

mir_route_receipt_validation_binding \
    src/verifier/mod.rs \
    'pub fn verify_mir_with_route_receipt(' \
    'fn bind_route_receipt('
mir_route_receipt_validation_binding \
    src/verifier/mod.rs \
    'pub fn verify_ffi_mir_with_route_receipt(' \
    'pub fn verify_ffi_mir_with_route_manifest('

# Manifest replay must reconstruct a receipt and then use the same direct MIR
# verifier adapter. Parsing a manifest without forwarding through that adapter
# would allow a CLI-only proof path to diverge from the checked API.
mir_route_manifest_replay_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local manifest_pattern="$4"
    local forwarding_pattern="$5"
    local context
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$context" | rg -F "$manifest_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_route_manifest_replay_missing=%s::%s phase=manifest-parse\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$context" | rg -F "$forwarding_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_route_manifest_replay_missing=%s::%s phase=direct-adapter\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    printf 'mir_route_manifest_replay_binding=manifest-parse+direct-adapter consumer=%s::%s\n' \
        "$source_file" "$start_function"
}

mir_route_manifest_replay_binding \
    src/verifier/mod.rs \
    'pub fn verify_mir_with_route_manifest(' \
    'pub fn verify_ffi_mir_with_route_manifest(' \
    'CanonicalMirRouteReceipt::from_manifest(manifest)' \
    'verify_mir_with_route_receipt(program, &receipt, source_hash)'
mir_route_manifest_replay_binding \
    src/verifier/mod.rs \
    'pub fn verify_ffi_mir_with_route_manifest(' \
    'fn verify_ffi_checked_with_source_hash(' \
    'CanonicalMirRouteReceipt::from_manifest(manifest)' \
    'verify_ffi_mir_with_route_receipt(program, &receipt, source_hash)'

# Capability admission must happen before symbolic execution in the direct FFI
# verifier. Compare source-order positions so a future refactor cannot leave a
# provenance validator while moving malformed MIR past the capability gate.
mir_ffi_capability_order_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local capability_pattern="$4"
    local execution_pattern="$5"
    local context
    local capability_line
    local execution_line
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    capability_line="$(printf '%s\n' "$context" | rg -n -F "$capability_pattern" | head -n 1 | cut -d: -f1 || true)"
    execution_line="$(printf '%s\n' "$context" | rg -n -F "$execution_pattern" | head -n 1 | cut -d: -f1 || true)"
    if [ -z "$capability_line" ] || [ -z "$execution_line" ]; then
        printf 'owner_audit_error=mir_ffi_capability_order_missing=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if [ "$capability_line" -ge "$execution_line" ]; then
        printf 'owner_audit_error=mir_ffi_capability_order_drift=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    printf 'mir_ffi_capability_order_binding=capability-before-execution consumer=%s::%s\n' \
        "$source_file" "$start_function"
}

mir_ffi_capability_order_binding \
    src/verifier/mod.rs \
    'pub fn verify_ffi_mir_with_source_hash(' \
    'pub fn verify_ffi_mir_with_route_receipt(' \
    'validate_mir_capabilities(program)' \
    'let results = mir::verify_ffi_program(program, source_hash.clone())?'

# A caller-supplied route receipt is the outer admission boundary. Validate it
# before invoking the source-hash adapter so malformed route metadata cannot
# reach capability/proof work or produce a misleading artifact.
mir_route_receipt_order_binding() {
    local source_file="$1"
    local start_function="$2"
    local end_function="$3"
    local receipt_pattern="$4"
    local adapter_pattern="$5"
    local context
    local receipt_line
    local adapter_line
    context="$(sed -n "/${start_function}/,/${end_function}/p" "$ROOT_DIR/$source_file")"
    receipt_line="$(printf '%s\n' "$context" | rg -n -F "$receipt_pattern" | head -n 1 | cut -d: -f1 || true)"
    adapter_line="$(printf '%s\n' "$context" | rg -n -F "$adapter_pattern" | head -n 1 | cut -d: -f1 || true)"
    if [ -z "$receipt_line" ] || [ -z "$adapter_line" ]; then
        printf 'owner_audit_error=mir_route_receipt_order_missing=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    if [ "$receipt_line" -ge "$adapter_line" ]; then
        printf 'owner_audit_error=mir_route_receipt_order_drift=%s::%s\n' \
            "$source_file" "$start_function" >&2
        audit_failed=1
        return
    fi
    printf 'mir_route_receipt_order_binding=receipt-before-adapter consumer=%s::%s\n' \
        "$source_file" "$start_function"
}

mir_route_receipt_order_binding \
    src/verifier/mod.rs \
    'pub fn verify_mir_with_route_receipt(' \
    'fn bind_route_receipt(' \
    '.validate_against_program(program)' \
    'let mut results = verify_mir(program, source_hash.clone())?'
mir_route_receipt_order_binding \
    src/verifier/mod.rs \
    'pub fn verify_ffi_mir_with_route_receipt(' \
    'pub fn verify_ffi_mir_with_route_manifest(' \
    '.validate_against_program(program)' \
    'let mut results = verify_ffi_mir_with_source_hash(program, source_hash.clone())?'

# Keep the public CLI verifier on the same receipt-bearing adapter as the
# library entry point, and keep the top-level error boundary routed through
# the shared formatter.  These checks prevent a future CLI refactor from
# silently dropping route provenance while all direct APIs remain correct.
mir_cli_verifier_route_binding() {
    local source_file="$1"
    local adapter_pattern="$2"
    if ! rg -F "$adapter_pattern" "$ROOT_DIR/$source_file" >/dev/null; then
        printf 'owner_audit_error=mir_cli_verifier_route_missing=%s pattern=%s\n' \
            "$source_file" "$adapter_pattern" >&2
        audit_failed=1
        return
    fi
    printf 'mir_cli_verifier_route_binding=receipt-bearing-adapter consumer=%s\n' \
        "$source_file"
}

mir_cli_error_boundary_binding() {
    local source_file="$1"
    local boundary_pattern="$2"
    if ! rg -F "$boundary_pattern" "$ROOT_DIR/$source_file" >/dev/null; then
        printf 'owner_audit_error=mir_cli_error_boundary_missing=%s pattern=%s\n' \
            "$source_file" "$boundary_pattern" >&2
        audit_failed=1
        return
    fi
    printf 'mir_cli_error_boundary_binding=shared-format-cli-error consumer=%s\n' \
        "$source_file"
}

mir_cli_verifier_route_binding \
    src/main/verify.rs \
    'verify_mir_with_route_receipt(&canonical, &receipt, source_hash)?'
mir_cli_error_boundary_binding \
    src/main.rs \
    'eprintln!("{}", format_cli_error(&e));'

mir_cli_disasm_route_binding() {
    local source_file="$1"
    local renderer_pattern="$2"
    local rejection_pattern="$3"
    local source
    source="$(cat "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$source" | rg -F "$renderer_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_cli_disasm_route_missing=%s phase=renderer\n' \
            "$source_file" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$source" | rg -F "$rejection_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_cli_disasm_route_missing=%s phase=rejection\n' \
            "$source_file" >&2
        audit_failed=1
        return
    fi
    printf 'mir_cli_disasm_route_binding=shared-format-cli-error consumer=%s\n' \
        "$source_file"
}

mir_cli_disasm_route_binding \
    src/main/disasm_cmd.rs \
    'crate::format_cli_error(&error.to_string())' \
    'crate::format_cli_error(&message)'

mir_cli_route_formatter_provenance_binding() {
    local source_file="$1"
    local classifier_pattern="$2"
    local origin_pattern="$3"
    local source
    source="$(cat "$ROOT_DIR/$source_file")"
    if ! printf '%s\n' "$source" | rg -F "$classifier_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_cli_route_formatter_provenance_missing=%s phase=classifier\n' \
            "$source_file" >&2
        audit_failed=1
        return
    fi
    if ! printf '%s\n' "$source" | rg -F "$origin_pattern" >/dev/null; then
        printf 'owner_audit_error=mir_cli_route_formatter_provenance_missing=%s phase=origin\n' \
            "$source_file" >&2
        audit_failed=1
        return
    fi
    printf 'mir_cli_route_formatter_provenance_binding=registry-code+mir.route-origin consumer=%s\n' \
        "$source_file"
}

mir_cli_route_formatter_provenance_binding \
    src/main.rs \
    'canonical_mir_route_code_location_in_message(message)' \
    'runtime_system("mir.route")'

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
