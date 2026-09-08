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
audit_failed=0

printf 'schema=canonical-mir-legacy-owner-audit-v1\n'
printf 'root=%s\n' "$ROOT_DIR"

owner_count=0
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
    printf 'owner=%s status=%s dependency_class=%s owner_deletion_ready=%s owner_deletion_blocker=%s; %s\n' \
        "$owner" "$owner_status" "$dependency_class" "$deletion_ready" "$accessor_blocker" "$deletion_blocker"
    printf 'owner_deletion_condition=%s\n' "$condition"
done

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
