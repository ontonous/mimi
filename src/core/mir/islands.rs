//! Whole-program Canonical MIR production-island contracts.
//!
//! This module contains route eligibility, not backend lowering.  The first
//! island is intentionally narrower than the individual List/Set adapters:
//! every executable function in the materialized MIR graph must use only
//! Copy scalar values, move-owned bounded Lists/Sets, synchronous scalar CFG,
//! and canonical scalar calls. The List island includes the one-level nested
//! outer-length read; the checker type catalog may contain many
//! unrelated declarations; only types and operations that actually cross the
//! executable MIR graph are inspected here.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::ir::{
    ResolvedBinaryOp, ResolvedCall, ResolvedCallee, ResolvedExpr, ResolvedExprKind,
    ResolvedFStringPart, ResolvedLiteral, ResolvedPattern, ResolvedPatternKind, ResolvedStmtKind,
    ResolvedType, ResolvedUnaryOp, ResolvedValueProjection,
};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{
    MirAbiClass, MirGlueContract, MirGlueKind, MirGlueOperation, MirLayout, MirOwnership,
    MirTypeDesc, MirTypeKind, MirVariantCallAbiMode,
};
use crate::core::{
    CheckedProgram, NodeId, NominalTypeId, PrimitiveType, ResolvedCallKind, ResolvedTypeId,
};

use super::{
    MirFunction, MirGenericInstanceContract, MirInstructionKind, MirListOperation, MirTerminator,
    MirValueId,
};

/// Name of the finite whole-program island closed by this contract.
pub const SCALAR_COLLECTION_ISLAND: &str = "copy-scalar-collection-v1";
/// Name of the narrow generic variant predicate island. It admits only
/// `is_some`/`is_none` over `Option<T>` or `is_ok`/`is_err` over a one-binder
/// `Result` shape where the concrete generic payload is a signed scalar/bool;
/// non-Copy payloads remain outside.
pub const GENERIC_VARIANT_PREDICATE_ISLAND: &str = "generic-option-predicate-v1";
/// Name of the generic `Option<T>.unwrap()` projection island.  This is kept
/// separate from the predicate profile because the projection has a
/// trap-bearing payload receipt and therefore a different ownership/effect
/// contract even though both shapes specialize through the same generic MIR
/// machinery.
pub const GENERIC_OPTION_PROJECTION_ISLAND: &str = "generic-option-projection-v1";
/// Name of the generic `Option<T>.unwrap_or(T)` total projection island. It is
/// separate from the trap-bearing projection because the fallback operand is
/// an explicit second ABI value and the operation is total over both tags.
pub const GENERIC_OPTION_PROJECTION_FALLBACK_ISLAND: &str = "generic-option-projection-fallback-v1";
/// Name of the generic `Result<T, T>.unwrap()` / `Result<T, i32>.unwrap()` /
/// `Result<T, bool>.unwrap()` projection island. It is separate from the
/// Option projection profile because `Ok` is tag zero and both Result payload
/// slots participate in the aggregate ABI proof. Distinct scalar `Err` shapes
/// use the same receipt-bearing MIR node but a two-slot native aggregate ABI.
pub const GENERIC_RESULT_PROJECTION_ISLAND: &str = "generic-result-projection-v1";
/// Name of the generic `Result<T, T>.unwrap_or(T)` /
/// `Result<T, i32>.unwrap_or(T)` / `Result<T, bool>.unwrap_or(T)` total
/// projection island. It remains distinct from trap-bearing Result projection
/// because both payload slots and the explicit fallback operand participate
/// in the ABI.
pub const GENERIC_RESULT_PROJECTION_FALLBACK_ISLAND: &str = "generic-result-projection-fallback-v1";
/// Name of the direct-call managed Result ABI island.  This profile covers
/// concrete calls returning `Result<String, i32>` or
/// `Result<List<Copy scalar>, i32>`; the call receipt carries the aggregate
/// layout and move-owned return merge proof for every consumer.
pub const MANAGED_RESULT_CALL_ISLAND: &str = "managed-result-call-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenericVariantPredicateAdmission {
    OutsideProfile,
    MixedCoverage,
    CompleteCoverage,
}

/// Checker-owned admission for the narrow generic `Option<T>.unwrap()` shape.
/// The complete case is intentionally independent from generic variant
/// predicates: a projection returns the payload and may trap, while a
/// predicate is read-only and returns `bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenericOptionProjectionAdmission {
    OutsideProfile,
    MixedCoverage,
    CompleteCoverage,
}

/// Checker-owned admission for the narrow generic `Option<T>.unwrap_or(T)`
/// shape. The fallback has an explicit Copy scalar ABI, so it cannot share the
/// trap-only projection profile without losing a receipt-bearing operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenericOptionProjectionFallbackAdmission {
    OutsideProfile,
    MixedCoverage,
    CompleteCoverage,
}

/// Checker-owned admission for the narrow generic `Result<T, T>.unwrap()` and
/// `Result<T, i32>.unwrap()` shapes. The complete case is independent from
/// predicates and Option projection because it carries the Result `Ok`
/// tag/trap receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenericResultProjectionAdmission {
    OutsideProfile,
    MixedCoverage,
    CompleteCoverage,
}

/// Checker-owned admission for the narrow generic `Result<T, T>.unwrap_or(T)`
/// and `Result<T, i32>.unwrap_or(T)` shapes. The complete case is independent
/// from the trap-bearing Result projection and concrete `Result<i32, i32>`
/// island.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenericResultProjectionFallbackAdmission {
    OutsideProfile,
    MixedCoverage,
    CompleteCoverage,
}

/// Checker-owned admission for concrete direct calls returning the managed
/// Result ABI.  A mixed/unsupported call graph is still a candidate and must
/// fail closed before a compatibility emitter can observe the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedResultCallAdmission {
    OutsideProfile,
    MixedCoverage,
    CompleteCoverage,
}

/// Classify concrete direct calls whose checker-finalized result is in the
/// managed `Result<_, i32>` ABI family.  Other error payloads remain on the
/// compatibility route: they are not part of this island and must not make
/// an unrelated standard-library or application graph look like an admitted
/// managed-result candidate.  Once the i32 error slot is recognized, the
/// canonical MIR materializer remains the authority for the recursive
/// TypeDesc/glue proof; unsupported Ok payloads (for example
/// `Result<List<f64>, i32>`) therefore still fail closed before legacy.
pub fn classify_managed_result_call_admission(
    program: &CheckedProgram,
) -> ManagedResultCallAdmission {
    let mut has_candidate = false;
    let mut unsupported_shape = false;
    for site in program.call_sites_sorted() {
        if site.kind != ResolvedCallKind::Function {
            continue;
        }
        let Some(result_ty) = program.resolved_node_type(&site.node_id) else {
            continue;
        };
        let Some(ResolvedType::Result { error, .. }) = program.resolved_types().get(result_ty)
        else {
            continue;
        };
        if !matches!(
            program.resolved_types().get(error),
            Some(ResolvedType::Primitive(PrimitiveType::I32))
        ) {
            continue;
        }
        has_candidate = true;
        if !checker_managed_result_shape(program, result_ty) {
            unsupported_shape = true;
        }
    }
    if !has_candidate {
        return ManagedResultCallAdmission::OutsideProfile;
    }
    if unsupported_shape || has_mixed_coverage(program) {
        ManagedResultCallAdmission::MixedCoverage
    } else {
        ManagedResultCallAdmission::CompleteCoverage
    }
}

/// Stable candidate hint for direct managed Result calls.  The hint is based
/// only on checker call-site/type facts and covers the closed i32-error-slot
/// family.  It is intentionally broader than the concrete TypeDesc contract
/// on the Ok side so unsupported payloads are rejected rather than routed
/// through a legacy consumer, but it does not classify unrelated
/// `Result<_, string>`/nominal error APIs as this island.
pub fn has_managed_result_call_candidate(program: &CheckedProgram) -> bool {
    program.call_sites_sorted().into_iter().any(|site| {
        site.kind == ResolvedCallKind::Function
            && program
                .resolved_node_type(&site.node_id)
                .and_then(|ty| program.resolved_types().get(ty))
                .is_some_and(|ty| {
                    matches!(
                        ty,
                        ResolvedType::Result { error, .. }
                            if matches!(
                                program.resolved_types().get(error),
                                Some(ResolvedType::Primitive(PrimitiveType::I32))
                            )
                    )
                })
    })
}

fn checker_managed_result_shape(program: &CheckedProgram, ty: &ResolvedTypeId) -> bool {
    let Some(ResolvedType::Result { ok, error }) = program.resolved_types().get(ty) else {
        return false;
    };
    if !matches!(
        program.resolved_types().get(error),
        Some(ResolvedType::Primitive(PrimitiveType::I32))
    ) {
        return false;
    }
    match program.resolved_types().get(ok) {
        Some(ResolvedType::Primitive(PrimitiveType::String)) => true,
        Some(ResolvedType::Nominal {
            item, arguments, ..
        }) if item.as_str() == "builtin:type:List" && arguments.len() == 1 => {
            arguments.first().is_some_and(|argument| {
                matches!(
                    program.resolved_types().get(argument),
                    Some(ResolvedType::Primitive(
                        PrimitiveType::I32 | PrimitiveType::I64 | PrimitiveType::Bool,
                    ))
                )
            })
        }
        _ => false,
    }
}

/// Classify the checker-owned generic `Option<T>.unwrap()` envelope before MIR
/// materialization.  Only one generic binder, one `Option<T>` parameter, a
/// `T` result, no statements, and the direct builtin unwrap call are admitted.
/// Concrete payload/layout/glue checks remain in MIR specialization.
pub fn classify_generic_option_projection_admission(
    program: &CheckedProgram,
) -> GenericOptionProjectionAdmission {
    let has_candidate = program
        .callables()
        .values()
        .any(|callable| is_generic_option_projection_callable(program, callable));
    if !has_candidate {
        return GenericOptionProjectionAdmission::OutsideProfile;
    }
    if has_mixed_coverage(program)
        || program.callables().values().any(|callable| {
            mentions_generic_option_callable(program, callable)
                && !is_generic_option_projection_callable(program, callable)
                && !is_generic_option_projection_fallback_callable(program, callable)
                && !is_generic_variant_predicate_callable(program, callable)
        })
    {
        GenericOptionProjectionAdmission::MixedCoverage
    } else {
        GenericOptionProjectionAdmission::CompleteCoverage
    }
}

/// Stable candidate hint used by default dispatch when an unsupported generic
/// Option shape must be rejected before a legacy consumer can observe it.
/// `Option<string>` is admitted only by the ownership-aware projection
/// materializer; other managed or non-Copy payloads remain fail-closed.
pub fn has_unsupported_generic_option_projection_candidate(program: &CheckedProgram) -> bool {
    program.callables().values().any(|callable| {
        mentions_generic_option_callable(program, callable)
            && !is_generic_option_projection_callable(program, callable)
            && !is_generic_option_projection_fallback_callable(program, callable)
            && !is_generic_variant_predicate_callable(program, callable)
    })
}

/// Classify the checker-owned generic `Option<T>.unwrap_or(T)` envelope before
/// MIR materialization. Only one generic binder, an `Option<T>` receiver, a
/// `T` fallback parameter, a `T` result, an empty body and the direct builtin
/// call are admitted. Concrete Copy scalar checks remain in specialization.
pub fn classify_generic_option_projection_fallback_admission(
    program: &CheckedProgram,
) -> GenericOptionProjectionFallbackAdmission {
    let has_candidate = program
        .callables()
        .values()
        .any(|callable| is_generic_option_projection_fallback_callable(program, callable));
    if !has_candidate {
        return GenericOptionProjectionFallbackAdmission::OutsideProfile;
    }
    if has_mixed_coverage(program)
        || program.callables().values().any(|callable| {
            mentions_generic_option_callable(program, callable)
                && !is_generic_option_projection_callable(program, callable)
                && !is_generic_option_projection_fallback_callable(program, callable)
                && !is_generic_variant_predicate_callable(program, callable)
        })
    {
        GenericOptionProjectionFallbackAdmission::MixedCoverage
    } else {
        GenericOptionProjectionFallbackAdmission::CompleteCoverage
    }
}

/// Stable candidate hint used by default dispatch when an unsupported generic
/// Option fallback shape must be rejected before legacy consumers observe it.
pub fn has_unsupported_generic_option_projection_fallback_candidate(
    program: &CheckedProgram,
) -> bool {
    program.callables().values().any(|callable| {
        mentions_generic_option_callable(program, callable)
            && !is_generic_option_projection_callable(program, callable)
            && !is_generic_option_projection_fallback_callable(program, callable)
            && !is_generic_variant_predicate_callable(program, callable)
    })
}

/// Classify the checker-owned generic `Result<T, T>.unwrap()` and
/// `Result<T, i32>.unwrap()`/`Result<T, bool>.unwrap()` envelopes before MIR
/// materialization. Only one
/// generic binder, one Result parameter, a `T` result, no statements, and the
/// direct builtin unwrap call are admitted. Concrete scalar/layout checks
/// remain in MIR specialization.
pub fn classify_generic_result_projection_admission(
    program: &CheckedProgram,
) -> GenericResultProjectionAdmission {
    let has_candidate = program
        .callables()
        .values()
        .any(|callable| is_generic_result_projection_callable(program, callable));
    if !has_candidate {
        return GenericResultProjectionAdmission::OutsideProfile;
    }
    if has_mixed_coverage(program)
        || program.callables().values().any(|callable| {
            mentions_generic_result_callable(program, callable)
                && !is_generic_result_projection_callable(program, callable)
                && !is_generic_result_projection_fallback_callable(program, callable)
                && !is_generic_result_projection_fallback_candidate(program, callable)
                && !is_generic_variant_predicate_callable(program, callable)
        })
    {
        GenericResultProjectionAdmission::MixedCoverage
    } else {
        GenericResultProjectionAdmission::CompleteCoverage
    }
}

/// Stable candidate hint used by default dispatch when an unsupported generic
/// Result shape must be rejected before a legacy consumer can observe it.
pub fn has_unsupported_generic_result_projection_candidate(program: &CheckedProgram) -> bool {
    program.callables().values().any(|callable| {
        mentions_generic_result_callable(program, callable)
            && !is_generic_result_projection_callable(program, callable)
            && !is_generic_result_projection_fallback_callable(program, callable)
            && !is_generic_result_projection_fallback_candidate(program, callable)
            && !is_generic_variant_predicate_callable(program, callable)
    })
}

/// Classify the checker-owned generic `Result<T, T>.unwrap_or(T)` and
/// `Result<T, i32>.unwrap_or(T)`/`Result<T, bool>.unwrap_or(T)` envelopes. Only one generic binder, one
/// Result receiver, one `T` fallback, a `T` result, an empty body and the
/// direct builtin call are admitted.
pub fn classify_generic_result_projection_fallback_admission(
    program: &CheckedProgram,
) -> GenericResultProjectionFallbackAdmission {
    let has_candidate = program
        .callables()
        .values()
        .any(|callable| is_generic_result_projection_fallback_callable(program, callable));
    if !has_candidate {
        return GenericResultProjectionFallbackAdmission::OutsideProfile;
    }
    if has_mixed_coverage(program)
        || program.callables().values().any(|callable| {
            mentions_generic_result_callable(program, callable)
                && !is_generic_result_projection_callable(program, callable)
                && !is_generic_result_projection_fallback_callable(program, callable)
                && !is_generic_variant_predicate_callable(program, callable)
        })
    {
        GenericResultProjectionFallbackAdmission::MixedCoverage
    } else {
        GenericResultProjectionFallbackAdmission::CompleteCoverage
    }
}

/// Stable candidate hint used by default dispatch when an unsupported generic
/// Result fallback shape must be rejected before legacy consumers observe it.
pub fn has_unsupported_generic_result_projection_fallback_candidate(
    program: &CheckedProgram,
) -> bool {
    program.callables().values().any(|callable| {
        is_generic_result_projection_fallback_candidate(program, callable)
            && !is_generic_result_projection_fallback_callable(program, callable)
    })
}

/// Classify the checker-owned generic variant predicate envelope before MIR
/// materialization.  This is deliberately a declaration/call-shape gate; the
/// concrete TypeDesc receipt is still rebuilt by generic MIR specialization.
pub fn classify_generic_variant_predicate_admission(
    program: &CheckedProgram,
) -> GenericVariantPredicateAdmission {
    let has_candidate = program
        .callables()
        .values()
        .any(|callable| is_generic_variant_predicate_callable(program, callable));
    if !has_candidate {
        return GenericVariantPredicateAdmission::OutsideProfile;
    }
    if has_mixed_coverage(program)
        || program.callables().values().any(|callable| {
            mentions_generic_option_callable(program, callable)
                && !is_generic_variant_predicate_callable(program, callable)
                && !is_generic_option_projection_callable(program, callable)
                && !is_generic_option_projection_fallback_callable(program, callable)
                || mentions_generic_result_callable(program, callable)
                    && !is_generic_variant_predicate_callable(program, callable)
                    && !is_generic_result_projection_callable(program, callable)
                    && !is_generic_result_projection_fallback_callable(program, callable)
        })
    {
        GenericVariantPredicateAdmission::MixedCoverage
    } else {
        GenericVariantPredicateAdmission::CompleteCoverage
    }
}

/// A generic Option-typed callable is a migrated candidate even when its body
/// or concrete payload is unsupported. Default routing uses this stable hint
/// to reject the shape before a legacy emitter can observe it if canonical
/// materialization cannot produce the receipt.
pub fn has_unsupported_generic_variant_predicate_candidate(program: &CheckedProgram) -> bool {
    program.callables().values().any(|callable| {
        mentions_generic_option_callable(program, callable)
            || mentions_generic_result_callable(program, callable)
    })
}

fn mentions_generic_option_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    if callable.signature.generic_parameters.len() != 1 {
        return false;
    }
    let Some(generic_ty) = generic_parameter_type_id(program, callable) else {
        return false;
    };
    callable.signature.parameters.iter().any(|parameter| {
        matches!(
            program.resolved_types().get(&parameter.ty),
            Some(ResolvedType::Option(inner)) if inner == &generic_ty
        )
    })
}

fn is_generic_option_predicate_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    if !mentions_generic_option_callable(program, callable)
        || callable.signature.parameters.len() != 1
        || callable.signature.generic_parameters.len() != 1
        || !matches!(
            program.resolved_types().get(&callable.signature.result),
            Some(ResolvedType::Primitive(PrimitiveType::Bool))
        )
        || !callable.body.root.statements.is_empty()
    {
        return false;
    }
    let Some(ResolvedExpr {
        kind: ResolvedExprKind::Call(call),
        ..
    }) = callable.body.root.result.as_deref()
    else {
        return false;
    };
    matches!(
        &call.callee,
        ResolvedCallee::Builtin(name)
            if matches!(
                name.as_str(),
                "builtin.method.option.is_some" | "builtin.method.option.is_none"
            )
    ) && call.arguments.len() == 1
}

fn is_generic_option_projection_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    let Some(generic_ty) = generic_parameter_type_id(program, callable) else {
        return false;
    };
    if !mentions_generic_option_callable(program, callable)
        || callable.signature.parameters.len() != 1
        || callable.signature.generic_parameters.len() != 1
        || callable.signature.result != generic_ty
        || !callable.body.root.statements.is_empty()
    {
        return false;
    }
    let Some(ResolvedExpr {
        kind: ResolvedExprKind::Call(call),
        ..
    }) = callable.body.root.result.as_deref()
    else {
        return false;
    };
    matches!(
        &call.callee,
        ResolvedCallee::Builtin(name) if name.as_str() == "builtin.method.option.unwrap"
    ) && call.arguments.len() == 1
}

fn is_generic_option_projection_fallback_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    let Some(generic_ty) = generic_parameter_type_id(program, callable) else {
        return false;
    };
    if !mentions_generic_option_callable(program, callable)
        || callable.signature.parameters.len() != 2
        || callable.signature.generic_parameters.len() != 1
        || callable.signature.result != generic_ty
        || !callable.body.root.statements.is_empty()
    {
        return false;
    }
    let Some(option_parameter) = callable.signature.parameters.first() else {
        return false;
    };
    let Some(fallback_parameter) = callable.signature.parameters.get(1) else {
        return false;
    };
    let Some(ResolvedType::Option(inner)) = program.resolved_types().get(&option_parameter.ty)
    else {
        return false;
    };
    if inner != &generic_ty || fallback_parameter.ty != generic_ty {
        return false;
    }
    let Some(ResolvedExpr {
        kind: ResolvedExprKind::Call(call),
        ..
    }) = callable.body.root.result.as_deref()
    else {
        return false;
    };
    matches!(
        &call.callee,
        ResolvedCallee::Builtin(name)
            if name.as_str() == "builtin.method.option.unwrap_or"
    ) && call.arguments.len() == 2
        && call
            .arguments
            .first()
            .is_some_and(|argument| argument.value.ty == option_parameter.ty)
        && call
            .arguments
            .get(1)
            .is_some_and(|argument| argument.value.ty == generic_ty)
        && call.result == generic_ty
}

pub(crate) fn is_generic_result_projection_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    let Some(generic_ty) = generic_parameter_type_id(program, callable) else {
        return false;
    };
    if !mentions_generic_result_callable(program, callable)
        || callable.signature.parameters.len() != 1
        || callable.signature.generic_parameters.len() != 1
        || callable.signature.result != generic_ty
        || !callable.body.root.statements.is_empty()
    {
        return false;
    }
    let Some(result_parameter) = callable.signature.parameters.first() else {
        return false;
    };
    let Some(ResolvedType::Result { ok, error }) =
        program.resolved_types().get(&result_parameter.ty)
    else {
        return false;
    };
    let error_is_same_generic = error == &generic_ty;
    let error_is_scalar = matches!(
        program.resolved_types().get(error),
        Some(ResolvedType::Primitive(
            PrimitiveType::I32 | PrimitiveType::Bool
        ))
    );
    if ok != &generic_ty || (!error_is_same_generic && !error_is_scalar) {
        return false;
    }
    let Some(ResolvedExpr {
        kind: ResolvedExprKind::Call(call),
        ..
    }) = callable.body.root.result.as_deref()
    else {
        return false;
    };
    matches!(
        &call.callee,
        ResolvedCallee::Builtin(name) if name.as_str() == "builtin.method.result.unwrap"
    ) && call.arguments.len() == 1
}

pub(crate) fn is_generic_result_projection_fallback_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    let Some(generic_ty) = generic_parameter_type_id(program, callable) else {
        return false;
    };
    if !mentions_generic_result_callable(program, callable)
        || callable.signature.parameters.len() != 2
        || callable.signature.generic_parameters.len() != 1
        || callable.signature.result != generic_ty
        || !callable.body.root.statements.is_empty()
    {
        return false;
    }
    let Some(result_parameter) = callable.signature.parameters.first() else {
        return false;
    };
    let Some(fallback_parameter) = callable.signature.parameters.get(1) else {
        return false;
    };
    let Some(ResolvedType::Result { ok, error }) =
        program.resolved_types().get(&result_parameter.ty)
    else {
        return false;
    };
    let error_is_same_generic = error == &generic_ty;
    let error_is_scalar = matches!(
        program.resolved_types().get(error),
        Some(ResolvedType::Primitive(
            PrimitiveType::I32 | PrimitiveType::Bool
        ))
    );
    if ok != &generic_ty
        || (!error_is_same_generic && !error_is_scalar)
        || fallback_parameter.ty != generic_ty
    {
        return false;
    }
    let Some(ResolvedExpr {
        kind: ResolvedExprKind::Call(call),
        ..
    }) = callable.body.root.result.as_deref()
    else {
        return false;
    };
    matches!(
        &call.callee,
        ResolvedCallee::Builtin(name)
            if name.as_str() == "builtin.method.result.unwrap_or"
    ) && call.arguments.len() == 2
        && call
            .arguments
            .first()
            .is_some_and(|argument| argument.value.ty == result_parameter.ty)
        && call
            .arguments
            .get(1)
            .is_some_and(|argument| argument.value.ty == generic_ty)
        && call.result == generic_ty
}

/// Broad checker-owned hint for the `Result<T,T>.unwrap_or(T)` /
/// `Result<T,i32>.unwrap_or(T)` family. This
/// deliberately admits malformed bodies (for example an extra statement) so
/// the fallback route can emit its stable fail-closed diagnostic instead of
/// being misclassified as the trap-bearing `unwrap` projection family.
fn is_generic_result_projection_fallback_candidate(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    if !mentions_generic_result_callable(program, callable)
        || callable.signature.generic_parameters.len() != 1
    {
        return false;
    }
    let Some(ResolvedExpr {
        kind: ResolvedExprKind::Call(call),
        ..
    }) = callable.body.root.result.as_deref()
    else {
        return false;
    };
    matches!(
        &call.callee,
        ResolvedCallee::Builtin(name)
            if name.as_str() == "builtin.method.result.unwrap_or"
    ) && call.arguments.len() == 2
}

fn mentions_generic_result_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    if callable.signature.generic_parameters.len() != 1 {
        return false;
    }
    let Some(generic_ty) = generic_parameter_type_id(program, callable) else {
        return false;
    };
    callable.signature.parameters.iter().any(|parameter| {
        matches!(
            program.resolved_types().get(&parameter.ty),
            Some(ResolvedType::Result { ok, error })
                if (ok == &generic_ty) || (error == &generic_ty)
        )
    })
}

fn generic_parameter_type_id(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> Option<crate::core::ResolvedTypeId> {
    let parameter = callable.signature.generic_parameters.first()?;
    program.resolved_types().iter().find_map(|(id, ty)| {
        matches!(ty, ResolvedType::GenericParameter(candidate) if candidate == parameter)
            .then_some(id.clone())
    })
}

fn is_generic_result_predicate_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    if !mentions_generic_result_callable(program, callable)
        || callable.signature.parameters.len() != 1
        || callable.signature.generic_parameters.len() != 1
        || !matches!(
            program.resolved_types().get(&callable.signature.result),
            Some(ResolvedType::Primitive(PrimitiveType::Bool))
        )
        || !callable.body.root.statements.is_empty()
    {
        return false;
    }
    let Some(ResolvedExpr {
        kind: ResolvedExprKind::Call(call),
        ..
    }) = callable.body.root.result.as_deref()
    else {
        return false;
    };
    matches!(
        &call.callee,
        ResolvedCallee::Builtin(name)
            if matches!(name.as_str(), "builtin.method.result.is_ok" | "builtin.method.result.is_err")
    ) && call.arguments.len() == 1
}

fn is_generic_variant_predicate_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    is_generic_option_predicate_callable(program, callable)
        || is_generic_result_predicate_callable(program, callable)
}

/// Checker-owned admission state for the already implemented scalar
/// collection production island.
///
/// The verifier API is program-scoped, so a materialized List/Set operation
/// cannot by itself prove that the complete checked program belongs to this
/// island.  Admission is therefore computed from typed resolved bodies before
/// MIR construction.  `CompleteCoverage` is the only state that may cross
/// the canonical construction boundary; the other states remain an explicit
/// compatibility boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarCollectionAdmission {
    /// No typed List operation, concrete scalar Set-facade call, or closed
    /// Copy-scalar stdout effect is present.
    OutsideProfile,
    /// A collection candidate exists, but the whole checked program contains
    /// imports, unsupported effects, managed values, or another unresolved
    /// executable dependency.
    MixedCoverage,
    /// Every executable typed body is within the current scalar collection
    /// envelope.  Any subsequent MIR construction or validation failure is a
    /// hard error and must never re-enter a legacy consumer.
    CompleteCoverage,
}

/// Classify scalar collection admission from checker-owned typed facts.
///
/// This is deliberately a pre-materialization predicate.  It does not read
/// the retained source file, invoke a backend, or infer a candidate from the
/// presence of a type declaration.  Generic templates are not executable
/// values on their own; only a checker-resolved concrete call to the narrow
/// identity/Set facade family, List len/reverse/concat, direct nested List
/// index projection, or an exact Copy-scalar stdout effect can make them part
/// of this island.
pub fn classify_scalar_collection_admission(program: &CheckedProgram) -> ScalarCollectionAdmission {
    let scanner = scan_scalar_collection_admission(program);
    if !scanner.has_candidate {
        ScalarCollectionAdmission::OutsideProfile
    } else if scanner.mixed {
        ScalarCollectionAdmission::MixedCoverage
    } else {
        ScalarCollectionAdmission::CompleteCoverage
    }
}

/// Return whether checker-owned resolved bodies contain a direct List
/// `reverse`/`concat` operation that is supposed to cross the scalar collection
/// boundary. This is kept separate from `ScalarCollectionAdmission`: a mixed
/// graph may still be a compatibility input, but an unsupported List operation
/// shape must not silently reach the legacy emitter after MIR construction
/// fails. List `len` retains the pre-existing compatibility policy here.
pub fn has_unsupported_list_reverse_candidate(program: &CheckedProgram) -> bool {
    scan_scalar_collection_admission(program).has_unsupported_list_reverse_candidate
}

pub fn has_unsupported_list_concat_candidate(program: &CheckedProgram) -> bool {
    scan_scalar_collection_admission(program).has_unsupported_list_concat_candidate
}

/// Return whether a checker-owned generic List facade call has a concrete
/// argument outside the admitted Copy-scalar collection set. Such a call is
/// still a migrated candidate, so default routing must reject it before
/// invoking legacy code.
pub fn has_unsupported_generic_list_facade_candidate(program: &CheckedProgram) -> bool {
    scan_scalar_collection_admission(program).has_unsupported_generic_list_facade_candidate
}

/// Return whether user-owned resolved bodies declare a generic `Set<T>` facade
/// callable. No user-facing generic Set shape materializes a supported scalar
/// MIR graph today, so a construction failure under a Complete admission must
/// fail closed instead of re-entering the legacy route (the nested-assign
/// coverage-scan false complete keeps its compatibility disposition because it
/// carries no such callable).
pub fn has_unsupported_generic_set_facade_candidate(program: &CheckedProgram) -> bool {
    program.callables().values().any(|callable| {
        !is_prelude_origin(program, &callable.body.root.origin)
            && is_generic_set_facade_callable(program, callable)
    })
}

/// R6-1076: the owned-String constant callable face — a concrete, effect-free
/// user callable whose entire body is a string-literal result.  This is the
/// checker-side mirror of `validate_owned_string_return_shape`'s constant
/// ledger and of `eval_direct_owned_string_call`'s routing class: the
/// materialized graph is one block of `Const "…" → Return`, every consumer
/// computes the same owned StringHandle, and no provenance escapes the
/// literal.  R6-1077: the face closes over one-edge wrappers — a callable
/// whose whole body is a call to a set member joins the set (see the
/// fixpoint in `scan_scalar_collection_admission`) — while everything else a
/// body could do with a String (arithmetic, second hands, String parameters,
/// branches, calls to non-members) keeps its existing floor.
fn has_owned_string_callable_envelope(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    !is_prelude_origin(program, &callable.body.root.origin)
        && callable.signature.generic_parameters.is_empty()
        && callable.signature.effects.is_empty()
        && !callable.signature.parameters.iter().any(|parameter| {
            matches!(
                parameter.permission,
                Some(crate::core::ir::Permission::View | crate::core::ir::Permission::Mutate)
            )
        })
        && callable.body.captures.is_empty()
        && callable.body.default_values.is_empty()
}

fn is_owned_string_constant_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    if !has_owned_string_callable_envelope(program, callable) {
        return false;
    }
    if !matches!(
        program.resolved_types().get(&callable.signature.result),
        Some(ResolvedType::Primitive(PrimitiveType::String))
    ) {
        return false;
    }
    callable.body.root.statements.is_empty()
        && callable.body.root.result.as_ref().is_some_and(|result| {
            matches!(
                &result.kind,
                ResolvedExprKind::Literal(crate::core::ResolvedLiteral::String(_))
            )
        })
}

/// R6-1079: the owned-String identity face — a concrete, effect-free,
/// non-prelude callable whose whole body returns its single String parameter
/// (`func echo(s: string) -> string { s }`).  The construction ledger proves
/// the parameter live exactly from the entry to the `Move → Return` glue
/// (the glue candidacy the R6-1078 argument-transfer probes established), so
/// the checker-side set can admit the same shape its gate re-proves.  A
/// String parameter in any other position keeps the profile floor.
fn is_owned_string_identity_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    if !has_owned_string_callable_envelope(program, callable) {
        return false;
    }
    if !matches!(
        program.resolved_types().get(&callable.signature.result),
        Some(ResolvedType::Primitive(PrimitiveType::String))
    ) {
        return false;
    }
    if callable.signature.parameters.len() != 1 || callable.body.parameters.len() != 1 {
        return false;
    }
    if !matches!(
        program
            .resolved_types()
            .get(&callable.signature.parameters[0].ty),
        Some(ResolvedType::Primitive(PrimitiveType::String))
    ) {
        return false;
    }
    let parameter_local = &callable.body.parameters[0];
    callable.body.root.statements.is_empty()
        && callable.body.root.result.as_ref().is_some_and(|result| {
            matches!(
                &result.kind,
                ResolvedExprKind::Load(place)
                    if place.base == *parameter_local && place.projections.is_empty()
            )
        })
}

/// R6-1080: the owned-String constant-bind face — a concrete, effect-free,
/// non-prelude callable whose whole body is one String-literal bind followed
/// by returning that binding (`func greet() -> string { let a = "x"; a }`).
/// The construction ledger already proves the shape: the body lowers to
/// `Const → Move → Return` glue (a glue candidate whose shape validation
/// ends with an empty live set), and the R6-1077 journey probes proved the
/// verifier explores it as the constant face.  The checker-side set admits
/// the same exactly-one-statement shape; a parameter, a second bind, or a
/// call-result initializer keeps the floor.
fn is_owned_string_constant_bind_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    if !has_owned_string_callable_envelope(program, callable) {
        return false;
    }
    if !matches!(
        program.resolved_types().get(&callable.signature.result),
        Some(ResolvedType::Primitive(PrimitiveType::String))
    ) {
        return false;
    }
    if !callable.signature.parameters.is_empty() || !callable.body.parameters.is_empty() {
        return false;
    }
    let [statement] = &callable.body.root.statements[..] else {
        return false;
    };
    let ResolvedStmtKind::Bind {
        pattern,
        initializer,
    } = &statement.kind
    else {
        return false;
    };
    let ResolvedPatternKind::Binding {
        local,
        by_reference: None,
    } = &pattern.kind
    else {
        return false;
    };
    if !matches!(
        program.resolved_types().get(&pattern.ty),
        Some(ResolvedType::Primitive(PrimitiveType::String))
    ) {
        return false;
    }
    let literal_initializer = matches!(
        initializer.as_ref().map(|value| &value.kind),
        Some(ResolvedExprKind::Literal(
            crate::core::ResolvedLiteral::String(_)
        ))
    );
    literal_initializer
        && callable.body.root.result.as_ref().is_some_and(|result| {
            matches!(
                &result.kind,
                ResolvedExprKind::Load(place)
                    if place.base == *local && place.projections.is_empty()
            )
        })
}

/// R6-1081: the owned-String call-bind face — a concrete, effect-free,
/// non-prelude callable whose whole body is one bind of a set member's call
/// result followed by returning that binding (`func wrap() -> string { let
/// t = greet(); t }`).  The body lowers to `Call → Move → Return` glue: the
/// ledger's Call arm introduces the binding exactly like a Clone and the
/// Move settles it into the return, so the shape validation ends with an
/// empty live set.  The callee side is set membership, so the face joins
/// the fixpoint closure rather than the pure-shape seed; the predicate runs
/// against the completed set.
fn is_owned_string_call_bind_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
    owned_string_callables: &BTreeSet<NodeId>,
) -> bool {
    if !has_owned_string_callable_envelope(program, callable) {
        return false;
    }
    if !matches!(
        program.resolved_types().get(&callable.signature.result),
        Some(ResolvedType::Primitive(PrimitiveType::String))
    ) {
        return false;
    }
    if !callable.signature.parameters.is_empty() || !callable.body.parameters.is_empty() {
        return false;
    }
    let [statement] = &callable.body.root.statements[..] else {
        return false;
    };
    let ResolvedStmtKind::Bind {
        pattern,
        initializer,
    } = &statement.kind
    else {
        return false;
    };
    let ResolvedPatternKind::Binding {
        local,
        by_reference: None,
    } = &pattern.kind
    else {
        return false;
    };
    if !matches!(
        program.resolved_types().get(&pattern.ty),
        Some(ResolvedType::Primitive(PrimitiveType::String))
    ) {
        return false;
    }
    let member_call_initializer = matches!(
        initializer.as_ref().map(|value| &value.kind),
        Some(ResolvedExprKind::Call(call))
            if matches!(&call.callee, ResolvedCallee::Function(owner)
                if owned_string_callables.contains(owner))
    );
    member_call_initializer
        && callable.body.root.result.as_ref().is_some_and(|result| {
            matches!(
                &result.kind,
                ResolvedExprKind::Load(place)
                    if place.base == *local && place.projections.is_empty()
            )
        })
}

fn scan_scalar_collection_admission(
    program: &CheckedProgram,
) -> ScalarCollectionAdmissionScanner<'_> {
    // R6-1054: the float bind face opens only inside a function that carries
    // the f64 print face — the same per-function contract
    // `ScalarCollectionValidator` enforces on the materialized graph.  The
    // bind site appears textually before the println that evidences the
    // face, so the scan runs twice: the discovery pass records which
    // callables contain an admitted float println, and the classification
    // pass applies the bind exemption from that set.  Both passes walk the
    // identical bodies, so the exemption can never drift from the evidence
    // the island gate later re-proves per materialized function.
    // R6-1070: the cross-function face needs one more step of the same
    // discipline.  Which functions sit on the one-edge f64 print closure
    // (a print function itself, or called directly by one) is only known
    // after discovery completes — collecting callees during discovery would
    // be walk-order dependent for calls that textually precede the println
    // that evidences the face.  So the closure pass runs seeded with the
    // completed print sets and records direct callees, and the
    // classification pass runs seeded with print ∪ direct callees.
    let discovery = scan_scalar_collection_once(
        program,
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
    );
    let closure = scan_scalar_collection_once(
        program,
        discovery.float_print_functions.clone(),
        discovery.string_print_functions,
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
    );
    let mut float_face_callables = discovery.float_print_functions;
    float_face_callables.extend(closure.direct_float_callees);
    // R6-1076: the owned-String constant callable set is shape-derived from
    // checker-owned bodies, so — unlike the print-evidence faces — it needs
    // no discovery pass and can never drift with walk order.  Every pass
    // sees the identical set.
    // R6-1077: the constant set closes over one-edge wrappers — a callable
    // whose whole body is a call to a set member joins the set, so
    // `wrap() { greet() }` composes the same provenance chain its callee
    // already carries.  The fixpoint is bounded by the callable count (each
    // round inserts at least one owner or stops), so walk order cannot drift
    // the closure either.
    // R6-1079: the seed also admits identity members — callables whose whole
    // body returns their single String parameter — so a wrapper feeding a
    // String argument into one (`wrap() { echo("hi") }`) closes over the
    // same set.
    // R6-1080: the seed admits constant-bind members — one String-literal
    // bind returned whole — whose `Const → Move → Return` glue the ledger
    // already proves.
    let owned_string_identity_callables = program
        .callables()
        .iter()
        .filter(|(_, callable)| is_owned_string_identity_callable(program, callable))
        .map(|(owner, _)| owner.clone())
        .collect::<BTreeSet<_>>();
    let owned_string_constant_bind_callables = program
        .callables()
        .iter()
        .filter(|(_, callable)| is_owned_string_constant_bind_callable(program, callable))
        .map(|(owner, _)| owner.clone())
        .collect::<BTreeSet<_>>();
    let mut owned_string_callables = program
        .callables()
        .iter()
        .filter(|(_, callable)| {
            is_owned_string_constant_callable(program, callable)
                || is_owned_string_identity_callable(program, callable)
                || is_owned_string_constant_bind_callable(program, callable)
        })
        .map(|(owner, _)| owner.clone())
        .collect::<BTreeSet<_>>();
    loop {
        let mut grew = false;
        for (owner, callable) in program.callables().iter() {
            if owned_string_callables.contains(owner) {
                continue;
            }
            let envelope_ok = has_owned_string_callable_envelope(program, callable);
            let result_ok = matches!(
                program.resolved_types().get(&callable.signature.result),
                Some(ResolvedType::Primitive(PrimitiveType::String))
            );
            // R6-1077: the zero-statement one-edge wrapper — the whole body
            // is a call to a set member.
            let direct_wrapper = callable.body.root.statements.is_empty()
                && matches!(
                    callable.body.root.result.as_ref().map(|result| &result.kind),
                    Some(ResolvedExprKind::Call(call))
                        if matches!(&call.callee, ResolvedCallee::Function(target)
                            if owned_string_callables.contains(target))
                );
            // R6-1081: the one-statement call-bind wrapper — the whole body
            // binds a member's call result and returns the binding
            // (`Call → Move → Return` glue the ledger proves).
            let call_bind_wrapper =
                is_owned_string_call_bind_callable(program, callable, &owned_string_callables);
            if envelope_ok && result_ok && (direct_wrapper || call_bind_wrapper) {
                owned_string_callables.insert(owner.clone());
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    // R6-1081: the call-bind subset is taken against the completed closure —
    // membership needs the fixpoint, so unlike the pure-shape subsets it
    // cannot be derived before the loop.
    let owned_string_call_bind_callables = program
        .callables()
        .iter()
        .filter(|(_, callable)| {
            is_owned_string_call_bind_callable(program, callable, &owned_string_callables)
        })
        .map(|(owner, _)| owner.clone())
        .collect::<BTreeSet<_>>();
    scan_scalar_collection_once(
        program,
        closure.float_print_functions,
        closure.string_print_functions,
        float_face_callables,
        owned_string_callables,
        owned_string_identity_callables,
        owned_string_constant_bind_callables,
        owned_string_call_bind_callables,
    )
}

fn scan_scalar_collection_once(
    program: &CheckedProgram,
    float_print_functions: BTreeSet<NodeId>,
    string_print_functions: BTreeSet<NodeId>,
    float_face_callables: BTreeSet<NodeId>,
    owned_string_callables: BTreeSet<NodeId>,
    owned_string_identity_callables: BTreeSet<NodeId>,
    owned_string_constant_bind_callables: BTreeSet<NodeId>,
    owned_string_call_bind_callables: BTreeSet<NodeId>,
) -> ScalarCollectionAdmissionScanner<'_> {
    let mut scanner = ScalarCollectionAdmissionScanner {
        program,
        has_candidate: false,
        has_unsupported_list_reverse_candidate: false,
        has_unsupported_list_concat_candidate: false,
        has_unsupported_generic_list_facade_candidate: false,
        mixed: false,
        seen_types: BTreeSet::new(),
        float_print_functions,
        string_print_functions,
        float_face_callables,
        owned_string_callables,
        owned_string_identity_callables,
        owned_string_constant_bind_callables,
        owned_string_call_bind_callables,
        direct_float_callees: BTreeSet::new(),
        current_callable: None,
        float_symbolic_locals: BTreeMap::new(),
        int_literal_locals: BTreeMap::new(),
        branch_generation: 0,
        block_depth: 0,
    };

    // R6-1052: the automatically merged prelude is a compatibility source,
    // not part of the user program's island. Its bodies carry float helpers,
    // capturing lambdas and multi-argument prints that would poison every
    // program's coverage scan; the newer families already filter it through
    // `is_prelude_origin`. Calls from user code into prelude callables stay
    // an explicit mixed boundary below so a graph that still depends on a
    // compatibility body keeps the legacy route.
    for (owner, callable) in program.callables() {
        if is_prelude_origin(program, &callable.body.root.origin) {
            continue;
        }
        scanner.current_callable = Some(owner.clone());
        // R6-1061: the float-symbolic set is a per-callable, in-order walk
        // fact — nothing survives across callable boundaries.
        scanner.float_symbolic_locals.clear();
        scanner.int_literal_locals.clear();
        scanner.branch_generation = 0;
        scanner.block_depth = 0;
        let concrete = callable.signature.generic_parameters.is_empty();
        if !callable.signature.effects.is_empty()
            || callable.signature.parameters.iter().any(|parameter| {
                matches!(
                    parameter.permission,
                    Some(crate::core::ir::Permission::View | crate::core::ir::Permission::Mutate)
                )
            })
            || !callable.body.captures.is_empty()
            || !callable.body.default_values.is_empty()
        {
            scanner.mixed = true;
        }
        if concrete {
            // R6-1070: an F64 parameter or result of a face-closure callable
            // is exactly the cross-function print face — the value flows to
            // (or from) an f64 println one call edge away, and the island
            // gate re-proves the same closure on the materialized graph.
            // Every other out-of-profile parameter/result keeps the floor.
            let float_face = scanner.in_float_face_function();
            let is_face_f64_type =
                |scanner: &ScalarCollectionAdmissionScanner<'_>,
                 ty: &crate::core::ResolvedTypeId| {
                    float_face
                        && matches!(
                            scanner.program.resolved_types().get(ty),
                            Some(ResolvedType::Primitive(PrimitiveType::F64))
                        )
                };
            // R6-1079: an identity member's single String parameter is the
            // face itself — live from the entry to the `Move → Return` glue
            // the construction ledger re-proves — so it skips the profile
            // floor exactly like the member's String result.  Any other
            // String parameter (an unused one above all) keeps the floor.
            let is_identity_string_parameter =
                |scanner: &ScalarCollectionAdmissionScanner<'_>,
                 ty: &crate::core::ResolvedTypeId| {
                    scanner.in_owned_string_identity_callable()
                        && matches!(
                            scanner.program.resolved_types().get(ty),
                            Some(ResolvedType::Primitive(PrimitiveType::String))
                        )
                };
            for parameter in &callable.signature.parameters {
                if !is_face_f64_type(&scanner, &parameter.ty)
                    && !is_identity_string_parameter(&scanner, &parameter.ty)
                {
                    scanner.require_profile_type(&parameter.ty);
                }
            }
            // R6-1076: an owned-String constant callable's String result is
            // the closed one-block literal face itself — the value returns
            // from a `Const "…" → Return` graph the island gate re-proves
            // through `validate_owned_string_return_shape`.  R6-1077: the
            // set is the wrapper-closed set, so a one-edge wrapper's String
            // result skips the floor through the same exemption.  Every
            // other out-of-profile result keeps the floor.
            let is_owned_string_constant_result =
                |scanner: &ScalarCollectionAdmissionScanner<'_>,
                 ty: &crate::core::ResolvedTypeId| {
                    scanner.in_owned_string_constant_callable()
                        && matches!(
                            scanner.program.resolved_types().get(ty),
                            Some(ResolvedType::Primitive(PrimitiveType::String))
                        )
                };
            if !is_face_f64_type(&scanner, &callable.signature.result)
                && !is_owned_string_constant_result(&scanner, &callable.signature.result)
            {
                scanner.require_profile_type(&callable.signature.result);
            }
            // R6-1070: an F64 parameter of a face-closure callable seeds the
            // float-symbolic set at the root walk generation — the parameter
            // load identity is exactly what the body's loads and places
            // read, so `v * 2.0` as the body's root result joins the same
            // verifier-backed symbolic domain a literal bind seeds.
            for (parameter, local) in callable
                .signature
                .parameters
                .iter()
                .zip(callable.body.parameters.iter())
            {
                if is_face_f64_type(&scanner, &parameter.ty) {
                    scanner
                        .float_symbolic_locals
                        .insert(local.clone(), scanner.branch_generation);
                }
            }
        }
        scanner.visit_block(&callable.body.root, concrete);
        if concrete {
            for value in callable.body.default_values.values() {
                scanner.visit_expr(value, true);
            }
        }
    }

    // R6-1052: share the prelude-decoupled declaration-mixedness floor with
    // the other islands instead of the previous inline copy whose unfiltered
    // prelude traits poisoned every program's coverage scan.
    scanner.mixed |= has_mixed_coverage(program);

    scanner
}

struct ScalarCollectionAdmissionScanner<'a> {
    program: &'a CheckedProgram,
    has_candidate: bool,
    has_unsupported_list_reverse_candidate: bool,
    has_unsupported_list_concat_candidate: bool,
    has_unsupported_generic_list_facade_candidate: bool,
    mixed: bool,
    seen_types: BTreeSet<crate::core::ResolvedTypeId>,
    /// Callables whose bodies contain an admitted f64 println.  Populated by
    /// the discovery pass and consulted by the classification pass for the
    /// float bind exemption (R6-1054).
    float_print_functions: BTreeSet<NodeId>,
    /// Callables whose bodies contain an admitted owned-String println —
    /// the StringHandle mirror of the float bind exemption (R6-1055).
    string_print_functions: BTreeSet<NodeId>,
    /// R6-1070: callables on the one-edge f64 print closure — the float
    /// print functions themselves plus the non-prelude functions they call
    /// directly.  Seeded before the classification pass; consulted for the
    /// cross-function f64 parameter/result/root exemptions.
    float_face_callables: BTreeSet<NodeId>,
    /// R6-1076: callables whose whole body is a string-literal result (the
    /// owned-String constant face).  Shape-derived before any walk; see
    /// `is_owned_string_constant_callable`.
    /// R6-1077: the set is the wrapper-closed set.
    owned_string_callables: BTreeSet<NodeId>,
    /// R6-1079: the identity subset of `owned_string_callables` — members
    /// whose single String parameter IS the face (live from the entry to the
    /// `Move → Return` glue the ledger re-proves).  Only these carry the
    /// String-parameter exemption; an unused String parameter elsewhere
    /// keeps the profile floor.
    owned_string_identity_callables: BTreeSet<NodeId>,
    /// R6-1080: the constant-bind subset of `owned_string_callables` —
    /// members whose whole body is one String-literal bind returned whole
    /// (`Const → Move → Return` glue).  Only these carry the literal-bind
    /// pattern and tail-read exemptions; any other multi-statement body
    /// keeps the compatibility floor.
    owned_string_constant_bind_callables: BTreeSet<NodeId>,
    /// R6-1081: the call-bind subset of `owned_string_callables` — members
    /// whose whole body is one bind of a member's call result returned whole
    /// (`Call → Move → Return` glue).  Derived against the completed
    /// closure; any other multi-statement body keeps the floor.
    owned_string_call_bind_callables: BTreeSet<NodeId>,
    /// R6-1070: non-prelude callees recorded inside float print functions
    /// during the closure pass.  Only the pass seeded with the completed
    /// discovery sets fills this meaningfully; the classification pass
    /// re-records the same identities idempotently.
    direct_float_callees: BTreeSet<NodeId>,
    current_callable: Option<NodeId>,
    /// R6-1061: locals whose current value the MIR verifier models in the
    /// IEEE symbolic Float domain — bound by a float literal, a second-hand
    /// float-symbolic read, or an admitted finite-only float arithmetic
    /// (Add/Subtract/Multiply/Divide, R6-1067) of the same.  Each entry
    /// records the branch generation at which the binding
    /// was walked; arithmetic operands consult the set only at the same
    /// generation, so a use after any branch or loop can never lean on a
    /// binding a different path may not have established.  An assignment
    /// from outside the domain removes the target outright.
    float_symbolic_locals: BTreeMap<crate::core::ir::ResolvedLocalId, u64>,
    /// R6-1064: int-typed locals whose current value is derived from an
    /// integer literal — bound by a literal initializer or refreshed by a
    /// literal assign.  These are exactly the reads whose int→f64 widening
    /// the MIR verifier evaluates as a known constant (the verifier
    /// propagates the constant through Load/Copy/Move/Clone into the
    /// Convert widen), so they join the float-symbolic operand domain at
    /// the same walk-generation discipline as `float_symbolic_locals`.  A
    /// non-literal assign removes the target outright.
    int_literal_locals: BTreeMap<crate::core::ir::ResolvedLocalId, u64>,
    /// Monotonic walk generation: bumped on every branch/loop entry and
    /// never decremented, so straight-line code shares one generation and
    /// anything hosted in a branch is visible only inside that region.
    branch_generation: u64,
    /// Current nesting depth of `visit_block` (the callable's root block is
    /// depth 1).  The MIR Phase 0 scalar-assign face is top-level-only —
    /// construction rejects any assign inside a nested block — so the
    /// assign-face predicates consult this to keep classification honest
    /// about the shapes construction accepts (the R6-1048 parity rule).
    block_depth: usize,
}

impl<'a> ScalarCollectionAdmissionScanner<'a> {
    fn in_float_print_function(&self) -> bool {
        self.current_callable
            .as_ref()
            .is_some_and(|owner| self.float_print_functions.contains(owner))
    }

    /// R6-1070: is the current callable on the one-edge f64 print closure —
    /// a float print function itself, or called directly by one?  The
    /// cross-function f64 shapes (parameter, result, call-result value)
    /// open only inside this closure, mirroring the island gate's
    /// `function_in_float_print_closure` envelope on the materialized graph.
    fn in_float_face_function(&self) -> bool {
        self.current_callable
            .as_ref()
            .is_some_and(|owner| self.float_face_callables.contains(owner))
    }

    fn in_string_print_function(&self) -> bool {
        self.current_callable
            .as_ref()
            .is_some_and(|owner| self.string_print_functions.contains(owner))
    }

    /// R6-1076: is the current callable an owned-String constant callable —
    /// the closed one-block literal face the island gate re-proves through
    /// `validate_owned_string_return_shape` on the materialized graph.
    /// R6-1077: the set is the wrapper-closed set, so a one-edge wrapper
    /// whose whole body is a call to a set member carries the same exemption.
    fn in_owned_string_constant_callable(&self) -> bool {
        self.current_callable
            .as_ref()
            .is_some_and(|owner| self.owned_string_callables.contains(owner))
    }

    /// R6-1079: is the current callable an identity member — its single
    /// String parameter flows to the return through the ledger's glue face?
    fn in_owned_string_identity_callable(&self) -> bool {
        self.current_callable
            .as_ref()
            .is_some_and(|owner| self.owned_string_identity_callables.contains(owner))
    }

    /// R6-1080: is the current callable a constant-bind member — one
    /// String-literal bind returned whole, the `Const → Move → Return` glue
    /// the ledger proves?
    fn in_owned_string_constant_bind_callable(&self) -> bool {
        self.current_callable
            .as_ref()
            .is_some_and(|owner| self.owned_string_constant_bind_callables.contains(owner))
    }

    /// R6-1081: is the current callable a call-bind member — one bind of a
    /// member's call result returned whole, the `Call → Move → Return` glue
    /// the ledger proves?
    fn in_owned_string_call_bind_callable(&self) -> bool {
        self.current_callable
            .as_ref()
            .is_some_and(|owner| self.owned_string_call_bind_callables.contains(owner))
    }

    /// R6-1061: is this expression something the MIR verifier models in the
    /// symbolic Float domain?  A float literal, an integer literal (the
    /// lowering widens it as a known constant), a projection-free read of a
    /// float-symbolic local at the current walk generation, or a nested
    /// admitted Add/Subtract of the same.  R6-1064: a projection-free read
    /// of an int-literal local (tracked in `int_literal_locals`) joins the
    /// operand domain — its widening is a known constant the verifier
    /// propagates through Load/Copy/Move/Clone, so the admission is
    /// verifier-backed.  Untracked int local reads (`n + 1.5` where n came
    /// from a call) still license nothing: their widening lands opaque.
    fn expr_is_float_symbolic_operand(&self, expression: &ResolvedExpr) -> bool {
        self.expr_is_float_origin_operand_inner(expression, true)
    }

    /// R6-1068: the comparison face's operand predicate.  It is the same
    /// origin domain as `expr_is_float_symbolic_operand` but WITHOUT the
    /// branch-generation stamp: a comparison consumes no symbolic fact —
    /// every consumer computes the plain IEEE ordered predicate from the
    /// runtime values the paths hold — so it needs only the walk-order
    /// origin admission the tracked sets already maintain (an out-of-face
    /// assign removes the target outright, so membership never outlives
    /// the classified provenance).
    fn expr_is_float_comparison_operand(&self, expression: &ResolvedExpr) -> bool {
        self.expr_is_float_origin_operand_inner(expression, false)
    }

    fn expr_is_float_origin_operand_inner(
        &self,
        expression: &ResolvedExpr,
        require_current_generation: bool,
    ) -> bool {
        let generation_matches =
            |generation: &u64| !require_current_generation || *generation == self.branch_generation;
        match &expression.kind {
            ResolvedExprKind::Literal(ResolvedLiteral::FloatBits(_) | ResolvedLiteral::Int(_)) => {
                true
            }
            ResolvedExprKind::Binary {
                op, left, right, ..
            } => {
                matches!(
                    op,
                    ResolvedBinaryOp::Add
                        | ResolvedBinaryOp::Subtract
                        | ResolvedBinaryOp::Multiply
                        | ResolvedBinaryOp::Divide
                ) && self.expr_is_float_origin_operand_inner(left, require_current_generation)
                    && self.expr_is_float_origin_operand_inner(right, require_current_generation)
            }
            // R6-1069: IEEE sign-bit negation is exact for every finite
            // operand and every consumer models it (`validate_copy_float_unary`
            // admission; the verifier's `(Negate, Float)` evaluator adds no
            // obligation), so a negate of a float-origin operand stays in the
            // float-origin domain under both variants of this predicate.
            ResolvedExprKind::Unary {
                op: ResolvedUnaryOp::Negate,
                operand,
            } => self.expr_is_float_origin_operand_inner(operand, require_current_generation),
            ResolvedExprKind::Load(place) => {
                place.projections.is_empty()
                    && self
                        .float_symbolic_locals
                        .get(&place.base)
                        .is_some_and(generation_matches)
                    || place.projections.is_empty()
                        && self
                            .int_literal_locals
                            .get(&place.base)
                            .is_some_and(generation_matches)
            }
            // R6-1072: admits a direct call to a float-face-closure callable
            // from inside a face function — R6-1075 extends the arm to both
            // variants of this predicate.  The callee's f64 result is one
            // face edge from an admitted print, the verifier symbolically
            // executes the callee body (`eval_direct_scalar_call`), and
            // every consumer computes from the runtime value.  Comparing a
            // call requires calling it and a fresh call result carries no
            // symbolic fact to go stale, so the callee is by construction a
            // direct callee of (or inside) the print closure — the two-edge
            // floor is unreachable here by shape.  The enclosure guard keeps
            // classifier admission and island capability aligned per
            // function (R6-1048): a non-face function's expression over a
            // face call result would otherwise be admitted here and then
            // hard-rejected by the island envelope.  Every consumer that
            // skips `visit_expr` on an admitted root walks its call leaves
            // (`visit_float_origin_call_leaves`), so the predicate admitting
            // a call never becomes the way an unpoliced call enters the
            // graph.
            ResolvedExprKind::Call(call) => {
                self.in_float_face_function()
                    && matches!(
                        self.program.resolved_types().get(&expression.ty),
                        Some(ResolvedType::Primitive(PrimitiveType::F64))
                    )
                    && matches!(
                        &call.callee,
                        ResolvedCallee::Function(owner)
                            if self.float_face_callables.contains(owner)
                    )
            }
            _ => false,
        }
    }

    /// The root-position variant of the operand predicate: the same domain,
    /// minus the bare integer literal (which only carries float identity as
    /// a binary operand, never as a bound/printed value of its own).
    fn expr_is_float_symbolic_root(&self, expression: &ResolvedExpr) -> bool {
        match &expression.kind {
            ResolvedExprKind::Literal(ResolvedLiteral::Int(_)) => false,
            _ => self.expr_is_float_symbolic_operand(expression),
        }
    }

    /// R6-1072: walk a subtree already admitted by the float-origin
    /// predicate and visit only its call leaves.  Literals, tracked loads,
    /// arithmetic and negates carry nothing to police (their own types are
    /// exempted by the face admission), but a call leaf must reach
    /// `visit_expr` so the Call arm keeps policing the prelude floor, arity
    /// and arguments — the predicate admitting a call must not become the
    /// way an unpoliced call enters the graph.  R6-1075: the stamped
    /// symbolic faces share the Call arm, so their `visit_expr`-skipping
    /// consumers walk the same leaves instead of skipping wholesale.
    fn visit_float_origin_call_leaves(&mut self, expression: &ResolvedExpr, concrete: bool) {
        match &expression.kind {
            ResolvedExprKind::Call(_) => self.visit_expr(expression, concrete),
            ResolvedExprKind::Binary { left, right, .. } => {
                self.visit_float_origin_call_leaves(left, concrete);
                self.visit_float_origin_call_leaves(right, concrete);
            }
            ResolvedExprKind::Unary { operand, .. } => {
                self.visit_float_origin_call_leaves(operand, concrete)
            }
            _ => {}
        }
    }

    /// R6-1064: is this expression an integer value derived from a literal —
    /// a literal itself, or a projection-free read of a tracked int-literal
    /// local at the current walk generation?  This is the RHS predicate for
    /// widening assigns (`x = n` into an f64 target) whose Convert the
    /// verifier evaluates as a known constant.  Call-sourced ints never
    /// qualify: their widening is opaque.
    fn expr_is_int_literal_provenance(&self, expression: &ResolvedExpr) -> bool {
        match &expression.kind {
            ResolvedExprKind::Literal(ResolvedLiteral::Int(_)) => true,
            ResolvedExprKind::Load(place) => {
                place.projections.is_empty()
                    && self
                        .int_literal_locals
                        .get(&place.base)
                        .is_some_and(|generation| *generation == self.branch_generation)
            }
            _ => false,
        }
    }

    /// Record every plain binding of an F64-typed pattern as float-symbolic
    /// at the current walk generation.  Reference bindings are excluded:
    /// they alias instead of transferring the value the set models.
    fn introduce_float_symbolic_bindings(&mut self, pattern: &ResolvedPattern) {
        if !matches!(
            self.program.resolved_types().get(&pattern.ty),
            Some(ResolvedType::Primitive(PrimitiveType::F64))
        ) {
            return;
        }
        match &pattern.kind {
            ResolvedPatternKind::Binding {
                local,
                by_reference,
            } => {
                if by_reference.is_none() {
                    self.float_symbolic_locals
                        .insert(local.clone(), self.branch_generation);
                }
            }
            ResolvedPatternKind::Constructor { fields, .. } => {
                for (_, field) in fields {
                    self.introduce_float_symbolic_bindings(field);
                }
            }
            ResolvedPatternKind::Tuple(items) | ResolvedPatternKind::Array(items) => {
                for item in items {
                    self.introduce_float_symbolic_bindings(item);
                }
            }
            ResolvedPatternKind::Slice { prefix, rest } => {
                for item in prefix {
                    self.introduce_float_symbolic_bindings(item);
                }
                if let Some(rest) = rest {
                    self.introduce_float_symbolic_bindings(rest);
                }
            }
            ResolvedPatternKind::Wildcard | ResolvedPatternKind::Literal(_) => {}
        }
    }

    /// R6-1064: record every plain binding of an I32/I64-typed pattern as
    /// int-literal provenance at the current walk generation — the mirror
    /// of `introduce_float_symbolic_bindings` for the widening face.  Only
    /// called when the initializer is an integer literal, so the set never
    /// claims a call- or arithmetic-sourced value.  Reference bindings are
    /// excluded: they alias instead of transferring the value the set
    /// models.
    fn introduce_int_literal_bindings(&mut self, pattern: &ResolvedPattern) {
        if !matches!(
            self.program.resolved_types().get(&pattern.ty),
            Some(ResolvedType::Primitive(
                PrimitiveType::I32 | PrimitiveType::I64
            ))
        ) {
            return;
        }
        match &pattern.kind {
            ResolvedPatternKind::Binding {
                local,
                by_reference,
            } => {
                if by_reference.is_none() {
                    self.int_literal_locals
                        .insert(local.clone(), self.branch_generation);
                }
            }
            ResolvedPatternKind::Constructor { fields, .. } => {
                for (_, field) in fields {
                    self.introduce_int_literal_bindings(field);
                }
            }
            ResolvedPatternKind::Tuple(items) | ResolvedPatternKind::Array(items) => {
                for item in items {
                    self.introduce_int_literal_bindings(item);
                }
            }
            ResolvedPatternKind::Slice { prefix, rest } => {
                for item in prefix {
                    self.introduce_int_literal_bindings(item);
                }
                if let Some(rest) = rest {
                    self.introduce_int_literal_bindings(rest);
                }
            }
            ResolvedPatternKind::Wildcard | ResolvedPatternKind::Literal(_) => {}
        }
    }

    /// R6-1061: every branch/loop entry raises the walk generation so
    /// float-symbolic facts established outside (or inside) can never be
    /// consulted across the region boundary.
    fn enter_branch_region(&mut self) {
        self.branch_generation += 1;
    }

    fn require_profile_type(&mut self, id: &crate::core::ResolvedTypeId) {
        if !self.seen_types.insert(id.clone()) {
            return;
        }
        if !is_scalar_collection_type(self.program, id, &mut BTreeSet::new()) {
            self.mixed = true;
        }
    }

    fn visit_pattern(&mut self, pattern: &ResolvedPattern, concrete: bool, exempt_root_type: bool) {
        if concrete && !exempt_root_type {
            self.require_profile_type(&pattern.ty);
        }
        match &pattern.kind {
            ResolvedPatternKind::Constructor { fields, .. } => {
                for (_, field) in fields {
                    self.visit_pattern(field, concrete, false);
                }
            }
            ResolvedPatternKind::Tuple(items) | ResolvedPatternKind::Array(items) => {
                for item in items {
                    self.visit_pattern(item, concrete, false);
                }
            }
            ResolvedPatternKind::Slice { prefix, rest } => {
                for item in prefix {
                    self.visit_pattern(item, concrete, false);
                }
                if let Some(rest) = rest {
                    self.visit_pattern(rest, concrete, false);
                }
            }
            ResolvedPatternKind::Wildcard
            | ResolvedPatternKind::Binding { .. }
            | ResolvedPatternKind::Literal(_) => {}
        }
    }

    fn visit_block(&mut self, block: &crate::core::ir::ResolvedBlock, concrete: bool) {
        self.block_depth += 1;
        if concrete {
            // R6-1070: the root block of a face-closure callable whose type
            // is the F64 leaf is the cross-function result face — the value
            // returns into the caller's print one edge away.  Nested blocks
            // keep the floor (branchy float returns stay fail-closed), as
            // does every non-face function's block type.  R6-1076: the root
            // block of an owned-String constant callable joins the same
            // result-face shape — its literal result is the closed face
            // itself; branchy String returns still floor through nested
            // blocks.  R6-1077: the set is the wrapper-closed set, so a
            // one-edge wrapper's root block carries the same exemption.
            let is_face_result_block = self.block_depth == 1
                && (self.in_float_face_function()
                    && matches!(
                        self.program.resolved_types().get(&block.ty),
                        Some(ResolvedType::Primitive(PrimitiveType::F64))
                    )
                    || self.in_owned_string_constant_callable()
                        && matches!(
                            self.program.resolved_types().get(&block.ty),
                            Some(ResolvedType::Primitive(PrimitiveType::String))
                        ));
            if !is_face_result_block {
                self.require_profile_type(&block.ty);
            }
        }
        for statement in &block.statements {
            if concrete {
                self.require_profile_type(&statement.ty);
            }
            match &statement.kind {
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer,
                } => {
                    // R6-1054/R6-1055: a print-face literal bind (f64 or
                    // owned String) opens as a print-face value only inside
                    // a function whose print call will materialize the
                    // exact consumption shape.  Every other out-of-profile
                    // origin (call result, arithmetic, second-hand local)
                    // still takes the profile-type floor below.
                    // R6-1071: the f64 half of the literal face follows the
                    // one-edge f64 print closure — a helper on a caller's
                    // print closure binds its own f64 literal under the same
                    // contract the island gate re-proves per function.  The
                    // owned-String half stays print-function-scoped.
                    let print_face_literal_initializer =
                        match initializer.as_ref().map(|value| &value.kind) {
                            Some(ResolvedExprKind::Literal(
                                crate::core::ResolvedLiteral::FloatBits(_),
                            )) => self.in_float_face_function(),
                            Some(ResolvedExprKind::Literal(
                                crate::core::ResolvedLiteral::String(_),
                            )) => {
                                self.in_string_print_function()
                                    // R6-1080: a constant-bind member's one
                                    // String-literal bind is the face itself —
                                    // the ledger proves the binding moves to
                                    // the return exactly once.
                                    || self.in_owned_string_constant_bind_callable()
                            }
                            _ => false,
                        };
                    // R6-1060: a second-hand print-face bind root (a plain
                    // local read as the initializer, f64 or owned String)
                    // joins the same face under the same per-function print
                    // contract — the mirror of the R6-1059 assign root.
                    // Inside a concrete island a local of these types can
                    // only originate in an admitted literal bind, a seeded
                    // parameter or an admitted call-result bind (everything
                    // else floors the whole program before this point), so
                    // the read adds no unclassified provenance; the island
                    // gate's Clone arm re-proves the contract on the
                    // materialized graph.  R6-1071: the f64 half widens to
                    // the one-edge print closure; the owned-String half
                    // stays print-function-scoped.
                    let second_hand_print_face_root = concrete
                        && initializer.as_ref().is_some_and(|value| {
                            let face_active = match &value.kind {
                                ResolvedExprKind::Load(_) => {
                                    matches!(
                                        self.program.resolved_types().get(&value.ty),
                                        Some(ResolvedType::Primitive(PrimitiveType::F64))
                                    ) && self.in_float_face_function()
                                        || matches!(
                                            self.program.resolved_types().get(&value.ty),
                                            Some(ResolvedType::Primitive(PrimitiveType::String))
                                        ) && self.in_string_print_function()
                                }
                                _ => false,
                            };
                            face_active
                        });
                    // R6-1061: an f64 bind whose initializer is an admitted
                    // finite-only float arithmetic (Add/Subtract since
                    // R6-1061, Multiply/Divide since R6-1067) over
                    // float-symbolic roots
                    // produces a value the MIR verifier holds in the
                    // symbolic Float domain, so the binding joins that set
                    // and the arithmetic skips the profile-type floor
                    // exactly like the literal and second-hand roots.  The
                    // per-function float print contract still envelopes the
                    // whole face.  R6-1071: the envelope widens to the
                    // one-edge f64 print closure — a called helper's
                    // straight-line arithmetic over its seeded parameter is
                    // exactly what the closure face materializes.
                    let arithmetic_print_face_root = concrete
                        && self.in_float_face_function()
                        && matches!(
                            self.program.resolved_types().get(&pattern.ty),
                            Some(ResolvedType::Primitive(PrimitiveType::F64))
                        )
                        && initializer
                            .as_ref()
                            .is_some_and(|value| self.expr_is_float_symbolic_root(value));
                    if arithmetic_print_face_root {
                        self.introduce_float_symbolic_bindings(pattern);
                    }
                    // R6-1071: a call-result bind whose callee sits on the
                    // one-edge f64 print closure joins the face — the value
                    // returns from a callable the closure already proves
                    // (parameter/result face), so the binding is a plain
                    // Move of an exactly-modeled f64.  The binding joins the
                    // float-symbolic set so later arithmetic over it stays
                    // in the same verifier-modeled domain every runtime
                    // consumer computes identically (E0813 owns finiteness).
                    // Off-closure callees keep the pattern floor: their f64
                    // provenance has no print face one edge away.
                    let float_call_result_bind_root = concrete
                        && matches!(
                            self.program.resolved_types().get(&pattern.ty),
                            Some(ResolvedType::Primitive(PrimitiveType::F64))
                        )
                        && matches!(
                            initializer.as_ref().map(|value| &value.kind),
                            Some(ResolvedExprKind::Call(call))
                                if matches!(
                                    &call.callee,
                                    ResolvedCallee::Function(owner)
                                        if self.float_face_callables.contains(owner)
                                )
                        );
                    if float_call_result_bind_root {
                        self.introduce_float_symbolic_bindings(pattern);
                    }
                    // R6-1076: a call-result bind whose callee is an
                    // owned-String constant callable joins the same closed
                    // face — the value is the callee's materialized
                    // `Const "…" → Return` StringHandle, the island gate
                    // re-proves the ledger per materialized function, and
                    // the binding is a plain Move of an exactly-modeled
                    // owned String.  R6-1077: the callee set is the
                    // wrapper-closed set, so a one-edge `wrap() { greet() }`
                    // binds through the same exemption.  Off-shape callees
                    // (non-literal, non-wrapper bodies, branches) keep the
                    // pattern floor: their String provenance has no closed
                    // face.
                    let string_call_result_bind_root = concrete
                        && matches!(
                            self.program.resolved_types().get(&pattern.ty),
                            Some(ResolvedType::Primitive(PrimitiveType::String))
                        )
                        && matches!(
                            initializer.as_ref().map(|value| &value.kind),
                            Some(ResolvedExprKind::Call(call))
                                if matches!(
                                    &call.callee,
                                    ResolvedCallee::Function(owner)
                                        if self.owned_string_callables.contains(owner)
                                )
                        );
                    // R6-1064: an int-typed bind of a literal initializer
                    // seeds the int-literal provenance set — the widening
                    // face's mirror of the arithmetic seed above.  The set
                    // is only consulted by verifier-backed predicates (the
                    // widening assign and the float-symbolic operand
                    // domain), and only inside a float print function.
                    let int_literal_bind = concrete
                        && self.in_float_print_function()
                        && matches!(
                            initializer.as_ref().map(|value| &value.kind),
                            Some(ResolvedExprKind::Literal(ResolvedLiteral::Int(_)))
                        );
                    if int_literal_bind {
                        self.introduce_int_literal_bindings(pattern);
                    }
                    self.visit_pattern(
                        pattern,
                        concrete,
                        print_face_literal_initializer
                            || second_hand_print_face_root
                            || arithmetic_print_face_root
                            || float_call_result_bind_root
                            || string_call_result_bind_root,
                    );
                    if let Some(initializer) = initializer {
                        // Literal and second-hand roots keep their existing
                        // walk (the literal exemption floors nothing); only
                        // the arithmetic root must bypass the visit, because
                        // its Binary node carries an out-of-profile type the
                        // top floor would reject.  The float and owned-String
                        // call-result roots walk normally: the visit_expr
                        // call-result face admits the call's own type while
                        // the Call arm still polices the prelude floor,
                        // arity and arguments.  R6-1075: the skipped
                        // arithmetic root can now contain call leaves, so it
                        // walks exactly those.
                        if !second_hand_print_face_root && !arithmetic_print_face_root {
                            self.visit_expr(initializer, concrete);
                        } else if arithmetic_print_face_root {
                            self.visit_float_origin_call_leaves(initializer, concrete);
                        }
                    }
                }
                ResolvedStmtKind::Assign {
                    target,
                    value,
                    conversion,
                } => {
                    // R6-1055: owned-String assign targets are outside the
                    // MIR Phase 0 scalar-assign face
                    // (`resolved_assign_is_admitted_scalar_shape` admits
                    // I32/I64/Bool/F64 since R6-1057; the lowerer fails
                    // construction on the rest), so the graph must stay
                    // mixed instead of luring the canonical route into a
                    // construction failure.  This floor is defense-in-depth
                    // beside the unmigrated-shape check: the print-face
                    // literal exemption below lets a literal RHS pass
                    // visit_expr untouched, so the classification scanner
                    // must not depend on another pass to hold the shape
                    // closed.  The former f64 half of this floor flipped
                    // with the face widening: float assigns are now a
                    // migrated shape, classified through the normal visit.
                    if concrete
                        && matches!(
                            self.program.resolved_types().get(&value.ty),
                            Some(ResolvedType::Primitive(PrimitiveType::String))
                        )
                    {
                        self.mixed = true;
                    }
                    // R6-1059: a second-hand f64 assign root (a plain local
                    // read feeding an admitted scalar assign target) joins
                    // the print face exactly while the function carries the
                    // float println contract — the same per-function
                    // envelope as the bind exemption above and the island
                    // gate's Clone admission, which re-proves it on the
                    // materialized graph.  The profile-type floor is
                    // skipped for this root the way the println-argument
                    // Load root is: inside a concrete island an f64 local
                    // can only originate in an admitted literal bind
                    // (parameters, results and call returns floor the whole
                    // program before this point), so the read has no
                    // unclassified provenance left to police.  Every other
                    // RHS root keeps its normal floor.  R6-1062: the
                    // scalar-assign face is top-level-only (construction
                    // rejects assigns inside nested blocks), so the
                    // `block_depth == 1` gate keeps the classification from
                    // admitting a shape construction would reject.
                    let second_hand_float_root = concrete
                        && self.block_depth == 1
                        && self.in_float_print_function()
                        && matches!(&value.kind, ResolvedExprKind::Load(_))
                        && matches!(
                            self.program.resolved_types().get(&value.ty),
                            Some(ResolvedType::Primitive(PrimitiveType::F64))
                        )
                        && resolved_assign_is_admitted_scalar_shape(self.program, statement);
                    // R6-1061: an assign whose RHS is an admitted
                    // finite-only Add/Subtract over float-symbolic roots
                    // (or a float literal / second-hand read) keeps the
                    // target in the symbolic Float domain — the island
                    // gate re-proves the shape on the materialized graph.
                    // Every other RHS removes the target from the set:
                    // after `x = n` (an opaque widen) arithmetic on x must
                    // floor again, in walk order, so a stale symbolic fact
                    // can never outlive the value that replaced it.
                    let conversion_target_is_f64 = matches!(
                        self.program.resolved_types().get(&conversion.to),
                        Some(ResolvedType::Primitive(PrimitiveType::F64))
                    );
                    let float_symbolic_assign = concrete
                        && self.block_depth == 1
                        && self.in_float_print_function()
                        && conversion_target_is_f64
                        && resolved_assign_is_admitted_scalar_shape(self.program, statement)
                        && self.expr_is_float_symbolic_root(value);
                    // R6-1062: an integer literal widening into an F64
                    // target joins the same face.  The lowering carries the
                    // widen as `assign_numeric_convert` sourced directly
                    // from the literal const, and the MIR verifier widens
                    // that known constant exactly (`Float::from_f64`), so
                    // the target keeps its symbolic Float identity.
                    // R6-1064: a second-hand int read (`x = n` where n
                    // carries literal provenance) joins too — the verifier
                    // propagates the constant through Load into the widen,
                    // so the target is still a known constant in the
                    // Float domain.  A non-constant integer RHS (`x =
                    // f()`) stays outside: its widen lands opaque in the
                    // verifier, and a stale symbolic fact must never
                    // outlive an unmodeled replacement.  Top-level-only,
                    // like every scalar-assign face shape.
                    let int_literal_widen_assign = concrete
                        && self.block_depth == 1
                        && self.in_float_print_function()
                        && conversion_target_is_f64
                        && resolved_assign_is_admitted_scalar_shape(self.program, statement)
                        && self.expr_is_int_literal_provenance(value);
                    // R6-1064: maintain the int-literal provenance set in
                    // walk order.  A literal RHS into an int-typed slot
                    // (re)seeds the target; every other RHS — including
                    // the widening assign above, whose target is an F64
                    // slot — removes it, so provenance can never outlive
                    // the value that replaced it.
                    let int_slot_literal_assign = concrete
                        && self.block_depth == 1
                        && self.in_float_print_function()
                        && matches!(
                            self.program.resolved_types().get(&conversion.to),
                            Some(ResolvedType::Primitive(
                                PrimitiveType::I32 | PrimitiveType::I64
                            ))
                        )
                        && matches!(
                            &value.kind,
                            ResolvedExprKind::Literal(ResolvedLiteral::Int(_))
                        );
                    if target.projections.is_empty() {
                        if float_symbolic_assign || int_literal_widen_assign {
                            self.float_symbolic_locals
                                .insert(target.base.clone(), self.branch_generation);
                        } else {
                            self.float_symbolic_locals.remove(&target.base);
                        }
                        if int_slot_literal_assign {
                            self.int_literal_locals
                                .insert(target.base.clone(), self.branch_generation);
                        } else {
                            self.int_literal_locals.remove(&target.base);
                        }
                    }
                    if !second_hand_float_root
                        && !float_symbolic_assign
                        && !int_literal_widen_assign
                    {
                        self.visit_expr(value, concrete);
                    } else if float_symbolic_assign {
                        // R6-1075: the admitted symbolic RHS can now contain
                        // call leaves; second-hand and literal roots cannot.
                        self.visit_float_origin_call_leaves(value, concrete);
                    }
                }
                ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                    if let Some(value) = value {
                        self.visit_expr(value, concrete);
                    }
                }
                ResolvedStmtKind::Continue => {}
                ResolvedStmtKind::Expr(value) => self.visit_expr(value, concrete),
                ResolvedStmtKind::While { condition, body } => {
                    self.enter_branch_region();
                    self.visit_expr(condition, concrete);
                    self.visit_block(body, concrete);
                }
                ResolvedStmtKind::WhileLet {
                    pattern,
                    initializer,
                    body,
                } => {
                    self.enter_branch_region();
                    self.visit_pattern(pattern, concrete, false);
                    self.visit_expr(initializer, concrete);
                    self.visit_block(body, concrete);
                }
                ResolvedStmtKind::IfLet {
                    pattern,
                    initializer,
                    then_block,
                    else_block,
                } => {
                    self.enter_branch_region();
                    self.visit_pattern(pattern, concrete, false);
                    self.visit_expr(initializer, concrete);
                    self.visit_block(then_block, concrete);
                    if let Some(else_block) = else_block {
                        self.visit_block(else_block, concrete);
                    }
                }
                ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                    self.visit_block(body, concrete)
                }
                ResolvedStmtKind::For {
                    pattern,
                    iterable,
                    body,
                } => {
                    self.enter_branch_region();
                    self.visit_pattern(pattern, concrete, false);
                    self.visit_expr(iterable, concrete);
                    self.visit_block(body, concrete);
                }
                ResolvedStmtKind::Drop(_) => {}
                ResolvedStmtKind::Contract { condition, .. } => {
                    self.visit_expr(condition, concrete)
                }
                ResolvedStmtKind::Math(expressions) => {
                    for expression in expressions {
                        self.visit_expr(expression, concrete);
                    }
                }
                ResolvedStmtKind::Pinned { value, body, .. } => {
                    self.enter_branch_region();
                    self.visit_expr(value, concrete);
                    self.visit_block(body, concrete);
                }
                ResolvedStmtKind::NestedCallable(_) => {}
            }
        }
        if let Some(result) = &block.result {
            // R6-1070: the root result of a face-closure callable joins the
            // cross-function face when it is an F64-typed float-symbolic
            // root — the origin predicate admits literals, tracked loads
            // and their arithmetic/negate closure, so the skipped visit
            // can never hide a projection; R6-1075 adds call leaves to the
            // same closure, and they are walked explicitly below (the
            // visit would otherwise floor on the Binary/Load node's own
            // f64 type).
            let is_face_result_root = self.block_depth == 1
                && concrete
                && self.in_float_face_function()
                && matches!(
                    self.program.resolved_types().get(&result.ty),
                    Some(ResolvedType::Primitive(PrimitiveType::F64))
                )
                && self.expr_is_float_symbolic_root(result);
            if !is_face_result_root {
                self.visit_expr(result, concrete);
            } else {
                self.visit_float_origin_call_leaves(result, concrete);
            }
        }
        self.block_depth -= 1;
    }

    fn visit_expr(&mut self, expression: &ResolvedExpr, concrete: bool) {
        if concrete {
            // R6-1052: a string literal is part of the owned StringHandle
            // stdout print face when it reaches a scalar println (R6-1050);
            // the expression-type scan below would otherwise read its
            // String type as an out-of-profile VALUE.  R6-1053 extends the
            // same literal-only exemption to f64 (shortest round-trip print
            // face).  Composite expressions re-check operand types explicitly
            // so the literal cannot hide inside a non-print operation.
            // R6-1070: a direct call to a face-closure callable whose result
            // is the F64 leaf joins the same cross-function face — the call
            // node itself is still walked below (prelude floor, arity,
            // arguments), only its own out-of-profile type skips the value
            // floor the way the print-face literal does.
            let is_print_face_literal = matches!(
                &expression.kind,
                ResolvedExprKind::Literal(crate::core::ResolvedLiteral::String(_))
                    | ResolvedExprKind::Literal(crate::core::ResolvedLiteral::FloatBits(_))
            );
            let is_float_face_call_result = matches!(
                &expression.kind,
                ResolvedExprKind::Call(call)
                    if matches!(
                        &call.callee,
                        ResolvedCallee::Function(owner)
                            if self.float_face_callables.contains(owner)
                    )
            ) && matches!(
                self.program.resolved_types().get(&expression.ty),
                Some(ResolvedType::Primitive(PrimitiveType::F64))
            );
            // R6-1076: a direct call to an owned-String constant callable
            // joins the same cross-function face — the call node's own
            // out-of-profile String type skips the value floor exactly like
            // the print-face literal, while the Call arm below still polices
            // the prelude floor, arity and arguments.  The callee's
            // materialized `Const "…" → Return` graph is the closed face the
            // island gate re-proves per function.  R6-1077: the callee set is
            // the wrapper-closed set, so a one-edge wrapper call skips the
            // floor through the same exemption.
            let is_string_call_result = matches!(
                &expression.kind,
                ResolvedExprKind::Call(call)
                    if matches!(
                        &call.callee,
                        ResolvedCallee::Function(owner)
                            if self.owned_string_callables.contains(owner)
                    )
            ) && matches!(
                self.program.resolved_types().get(&expression.ty),
                Some(ResolvedType::Primitive(PrimitiveType::String))
            );
            // R6-1079: inside an identity member the only String value is
            // the parameter read itself — the shape predicate constrains the
            // whole body to `Load(param)`, and the ledger proves the
            // parameter live exactly to the `Move → Return` glue.
            // R6-1080: inside a constant-bind member the only String value is
            // the bound local's read — the shape predicate constrains the
            // whole body to `let a = "…"; a`, and the ledger proves the
            // `Const → Move → Return` glue.
            let is_identity_param_value = (self.in_owned_string_identity_callable()
                || self.in_owned_string_constant_bind_callable()
                || self.in_owned_string_call_bind_callable())
                && matches!(
                    self.program.resolved_types().get(&expression.ty),
                    Some(ResolvedType::Primitive(PrimitiveType::String))
                );
            if !is_print_face_literal
                && !is_float_face_call_result
                && !is_string_call_result
                && !is_identity_param_value
            {
                self.require_profile_type(&expression.ty);
            }
        }
        match &expression.kind {
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let ResolvedFStringPart::Interpolation(value) = part {
                        self.visit_expr(value, concrete);
                    }
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                if concrete
                    && matches!(projection, ResolvedValueProjection::Index(_))
                    && is_resolved_nested_list_type(self.program, &value.ty)
                {
                    // Direct nested List indexing is a distinct candidate:
                    // the MIR receipt proves a deep-cloned child handle while
                    // preserving the borrowed parent obligation.
                    self.has_candidate = true;
                }
                self.visit_expr(value, concrete);
                if let ResolvedValueProjection::Index(index) = projection {
                    self.visit_expr(index, concrete);
                }
            }
            ResolvedExprKind::Binary {
                op, left, right, ..
            } => {
                // R6-1068: a comparison over the float-symbolic domain is
                // exactly modeled by every consumer — the plain IEEE
                // ordered predicate, with no symbolic fact to keep fresh —
                // so its operands skip the profile-type floor the way
                // print-face roots do.  The operand check is the
                // generation-agnostic origin predicate: the consumers hold
                // no symbolic fact across the branch boundary, so a
                // comparison after a branch leans on walk-order origin
                // membership, never on a stale generation stamp.
                // R6-1072: the predicate can also admit a call to a
                // float-face-closure callable (enclosure-guarded), so the
                // skipped operands are not wholesale — call leaves are
                // visited for their prelude floor, arity and argument
                // policing; literals/loads/arithmetic/negates carry nothing
                // to police.
                // R6-1052 guard: outside that face a string literal
                // operand must not smuggle a String operation (comparison,
                // concat) into a complete admission; only the scalar print
                // face admits string values.
                let comparison_over_float_domain = concrete
                    && matches!(
                        op,
                        ResolvedBinaryOp::Equal
                            | ResolvedBinaryOp::NotEqual
                            | ResolvedBinaryOp::Less
                            | ResolvedBinaryOp::Greater
                            | ResolvedBinaryOp::LessEqual
                            | ResolvedBinaryOp::GreaterEqual
                    )
                    && self.expr_is_float_comparison_operand(left)
                    && self.expr_is_float_comparison_operand(right);
                if comparison_over_float_domain {
                    self.visit_float_origin_call_leaves(left, concrete);
                    self.visit_float_origin_call_leaves(right, concrete);
                } else {
                    if concrete {
                        self.require_profile_type(&left.ty);
                        self.require_profile_type(&right.ty);
                    }
                    self.visit_expr(left, concrete);
                    self.visit_expr(right, concrete);
                }
            }
            // R6-1069: a negate over the float-origin domain skips the
            // own-type profile floor exactly like the comparison face: every
            // consumer models IEEE sign-bit negation from the runtime value
            // (`validate_copy_float_unary` admission; the verifier adds no
            // obligation), so the unary node's f64 type carries no
            // unclassified provenance.  The operand check is the
            // generation-agnostic origin predicate — R6-1072 lets it admit
            // enclosure-guarded face-closure calls, and the operand walk
            // goes through `visit_float_origin_call_leaves` so call leaves
            // keep their policing while the rest of the subtree stays
            // exempt.
            ResolvedExprKind::Unary {
                op: ResolvedUnaryOp::Negate,
                operand,
            } if concrete && self.expr_is_float_comparison_operand(operand) => {
                self.visit_float_origin_call_leaves(operand, concrete)
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand) => self.visit_expr(operand, concrete),
            ResolvedExprKind::Call(call) => {
                if concrete
                    && (is_list_len_call(self.program, call)
                        || is_list_reverse_call(self.program, call)
                        || is_list_concat_call(self.program, call))
                {
                    self.has_candidate = true;
                }
                if concrete
                    && is_list_reverse_call(self.program, call)
                    && call.arguments.first().is_some_and(|argument| {
                        !is_scalar_collection_type(
                            self.program,
                            &argument.value.ty,
                            &mut BTreeSet::new(),
                        )
                    })
                {
                    self.has_unsupported_list_reverse_candidate = true;
                }
                if concrete
                    && is_list_concat_call(self.program, call)
                    && call.arguments.iter().any(|argument| {
                        !is_scalar_collection_type(
                            self.program,
                            &argument.value.ty,
                            &mut BTreeSet::new(),
                        )
                    })
                {
                    self.has_unsupported_list_concat_candidate = true;
                }
                if is_scalar_set_facade_call(self.program, call)
                    || is_scalar_list_facade_call(self.program, call)
                {
                    self.has_candidate = true;
                }
                if concrete
                    && is_scalar_list_facade_call(self.program, call)
                    && (generic_list_operation_facade_body(self.program, call)
                        || generic_list_construction_facade_body(self.program, call))
                    && call.arguments.iter().any(|argument| {
                        !is_scalar_collection_type(
                            self.program,
                            &argument.value.ty,
                            &mut BTreeSet::new(),
                        )
                    })
                {
                    self.has_unsupported_generic_list_facade_candidate = true;
                }
                if is_scalar_set_contains_call(self.program, call)
                    || is_scalar_println_call(self.program, call)
                {
                    self.has_candidate = true;
                }
                // R6-1054/R6-1055: a print-face println call both evidences
                // the enclosing function's print face (the bind exemption
                // the island gate re-proves per materialized function) and
                // admits its own direct value argument below.
                let print_argument_primitive =
                    |argument: &crate::core::ir::ResolvedArgument, expected: PrimitiveType| {
                        matches!(
                            self.program.resolved_types().get(&argument.value.ty),
                            Some(ResolvedType::Primitive(primitive)) if *primitive == expected
                        )
                    };
                let float_print_call = is_scalar_println_call(self.program, call)
                    && call.arguments.first().is_some_and(|argument| {
                        print_argument_primitive(argument, PrimitiveType::F64)
                    });
                let string_print_call = is_scalar_println_call(self.program, call)
                    && call.arguments.first().is_some_and(|argument| {
                        print_argument_primitive(argument, PrimitiveType::String)
                    });
                if float_print_call || string_print_call {
                    if let Some(owner) = self.current_callable.clone() {
                        if float_print_call {
                            self.float_print_functions.insert(owner.clone());
                        }
                        if string_print_call {
                            self.string_print_functions.insert(owner);
                        }
                    }
                }
                // Only the closed stdout effects of this island (signed
                // integers, bool, and — since R6-1050/R6-1053 — the
                // StringHandle and f64 print faces) are Canonical MIR
                // nodes here. Other println shapes (aggregate, multi-arg)
                // remain on the explicit mixed compatibility route; float
                // arithmetic joins the f64 print face through the
                // float-symbolic root admission below (R6-1061), while
                // any other float shape keeps its Binary operand floor.
                if matches!(
                    &call.callee,
                    ResolvedCallee::Builtin(builtin) if builtin.as_str() == "println"
                ) && !is_scalar_println_call(self.program, call)
                {
                    self.mixed = true;
                }
                // R6-1052: a user call into an automatically merged prelude
                // callable keeps the compatibility route. The prelude itself
                // is excluded from the coverage scan, so this is the one
                // remaining channel through which a complete admission could
                // depend on a compatibility body.
                if let ResolvedCallee::Function(owner) = &call.callee {
                    if let Some(target) = self.program.callables().get(owner) {
                        if is_prelude_origin(self.program, &target.body.root.origin) {
                            self.mixed = true;
                        }
                    }
                }
                // R6-1070: record the non-prelude functions a float print
                // function calls directly — the one-edge closure the
                // classification pass seeds its cross-function f64 face
                // from.  The closure pass runs seeded with the completed
                // discovery sets, so this record is walk-order independent;
                // the classification pass re-records the same identities
                // idempotently.
                if self.in_float_print_function() {
                    if let ResolvedCallee::Function(owner) = &call.callee {
                        if let Some(target) = self.program.callables().get(owner) {
                            if !is_prelude_origin(self.program, &target.body.root.origin) {
                                self.direct_float_callees.insert(owner.clone());
                            }
                        }
                    }
                }
                // R6-1079: a String argument fed into an owned-String member
                // (constant, wrapper, or identity) is that member's input
                // contract — the callee's ledger-proven body consumes the
                // parameter exactly once (the shape ledger's Call arm and the
                // verifier's transfer agree), and a String-literal argument
                // materializes the same canonical StringHandle constant the
                // print face already admits.  Only literal String arguments
                // join the face; a second-hand String argument has no
                // checker-side provenance here and keeps the floor.
                let owned_string_member_call = matches!(
                    &call.callee,
                    ResolvedCallee::Function(owner)
                        if self.owned_string_callables.contains(owner)
                ) && call.arguments.iter().all(|argument| {
                    !matches!(
                        self.program.resolved_types().get(&argument.value.ty),
                        Some(ResolvedType::Primitive(PrimitiveType::String))
                    ) || matches!(
                        &argument.value.kind,
                        ResolvedExprKind::Literal(crate::core::ResolvedLiteral::String(_))
                    )
                });
                if concrete
                    && call.arguments.iter().any(|argument| {
                        matches!(
                            self.program.resolved_types().get(&argument.value.ty),
                            Some(ResolvedType::Primitive(PrimitiveType::String))
                        )
                    })
                    && !is_scalar_println_call(self.program, call)
                    && !owned_string_member_call
                {
                    // A string literal argument to any callee other than the
                    // scalar print face (len, starts_with, user calls, ...)
                    // has no canonical MIR node in this island.
                    self.mixed = true;
                }
                for argument in &call.arguments {
                    let print_face_value_root = match (&argument.value.kind, float_print_call) {
                        (
                            ResolvedExprKind::Literal(crate::core::ResolvedLiteral::FloatBits(_)),
                            true,
                        )
                        | (ResolvedExprKind::Load(_), true) => true,
                        // R6-1061: an admitted finite-only Add/Subtract
                        // over float-symbolic roots prints through the
                        // same face; the recursion polices every operand
                        // against the float-symbolic domain so the Binary
                        // node never reaches the profile-type floor.
                        _ => {
                            string_print_call
                                && matches!(
                                    &argument.value.kind,
                                    ResolvedExprKind::Literal(
                                        crate::core::ResolvedLiteral::String(_)
                                    ) | ResolvedExprKind::Load(_)
                                )
                                || float_print_call
                                    && matches!(
                                        &argument.value.kind,
                                        ResolvedExprKind::Binary { .. }
                                            | ResolvedExprKind::Unary { .. }
                                    )
                                    && self.expr_is_float_symbolic_root(&argument.value)
                        }
                    };
                    if print_face_value_root {
                        // The value shape was admitted by
                        // `is_scalar_println_call`; a print-face literal or a
                        // plain local read has no nested expression left to
                        // police, so skip the value-type floor the way the
                        // literal exemption does.  R6-1075: the admitted
                        // arithmetic root can contain call leaves, so the
                        // walk visits exactly those (a no-op for literals and
                        // loads).  Any other argument root (call result,
                        // arithmetic, projection) keeps its normal floor —
                        // its inner out-of-profile origin must stay outside
                        // this face.
                        self.visit_float_origin_call_leaves(&argument.value, concrete);
                        continue;
                    }
                    self.visit_expr(&argument.value, concrete);
                }
            }
            ResolvedExprKind::Tuple(items)
            | ResolvedExprKind::List(items)
            | ResolvedExprKind::Set(items) => {
                for item in items {
                    self.visit_expr(item, concrete);
                }
            }
            ResolvedExprKind::Map(items) => {
                for (key, value) in items {
                    self.visit_expr(key, concrete);
                    self.visit_expr(value, concrete);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                // The current canonical lowering contract has no
                // comprehension node.  Keep a collection candidate nested in
                // one on the explicit compatibility boundary instead of
                // promoting it to a complete island and discovering the gap
                // only after MIR materialization.
                self.mixed = true;
                self.visit_expr(value, concrete);
                self.visit_expr(iterable, concrete);
                if let Some(guard) = guard {
                    self.visit_expr(guard, concrete);
                }
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => self.visit_expr(receiver, concrete),
            ResolvedExprKind::Record { fields, rest, .. } => {
                for field in fields {
                    self.visit_expr(&field.value, concrete);
                }
                if let Some(rest) = rest {
                    self.visit_expr(rest, concrete);
                }
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => self.visit_block(block, concrete),
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                // R6-1068: the condition evaluates in the environment the
                // branch ENTERS with, so it is visited before the walk
                // generation bumps — a float-symbolic local bound outside
                // stays admissible evidence for a comparison condition.
                // The comparison admission carries no symbolic fact across
                // the boundary (the consumers compute the predicate from
                // whatever values the paths hold), so this cannot stale.
                self.visit_expr(condition, concrete);
                self.enter_branch_region();
                self.visit_block(then_block, concrete);
                self.visit_block(else_block, concrete);
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.enter_branch_region();
                self.visit_expr(scrutinee, concrete);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.visit_expr(guard, concrete);
                    }
                    self.visit_expr(&arm.body, concrete);
                }
            }
            ResolvedExprKind::Try { value, .. } => self.visit_expr(value, concrete),
            ResolvedExprKind::Range { start, end } => {
                self.visit_expr(start, concrete);
                self.visit_expr(end, concrete);
            }
            ResolvedExprKind::Slice { target, start, end } => {
                self.visit_expr(target, concrete);
                if let Some(start) = start {
                    self.visit_expr(start, concrete);
                }
                if let Some(end) = end {
                    self.visit_expr(end, concrete);
                }
            }
            ResolvedExprKind::Cast { value, .. } => self.visit_expr(value, concrete),
            ResolvedExprKind::Lambda(lambda) => {
                self.enter_branch_region();
                self.visit_block(&lambda.body, concrete)
            }
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Load(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => {}
        }
    }
}

fn is_scalar_collection_type(
    program: &CheckedProgram,
    id: &crate::core::ResolvedTypeId,
    seen: &mut BTreeSet<crate::core::ResolvedTypeId>,
) -> bool {
    if !seen.insert(id.clone()) {
        return true;
    }
    match program.resolved_types().get(id) {
        Some(ResolvedType::Primitive(
            crate::core::PrimitiveType::I32
            | crate::core::PrimitiveType::I64
            | crate::core::PrimitiveType::Bool
            | crate::core::PrimitiveType::Unit,
        )) => true,
        Some(ResolvedType::Nominal {
            item, arguments, ..
        }) if matches!(item.as_str(), "builtin:type:List" | "builtin:type:Set")
            && arguments.len() == 1 =>
        {
            arguments
                .first()
                .is_some_and(|argument| is_scalar_collection_type(program, argument, seen))
        }
        _ => false,
    }
}

fn is_list_len_call(program: &CheckedProgram, call: &crate::core::ir::ResolvedCall) -> bool {
    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return false;
    };
    matches!(builtin.as_str(), "len" | "builtin.method.list.len")
        && call.arguments.len() == 1
        && call.arguments.first().is_some_and(|argument| {
            is_resolved_list_type(program, &argument.value.ty, &mut BTreeSet::new())
        })
}

fn is_list_reverse_call(program: &CheckedProgram, call: &crate::core::ir::ResolvedCall) -> bool {
    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return false;
    };
    matches!(builtin.as_str(), "reverse" | "builtin.method.list.reverse")
        && call.arguments.len() == 1
        && call.arguments.first().is_some_and(|argument| {
            is_resolved_list_type(program, &argument.value.ty, &mut BTreeSet::new())
        })
}

fn is_list_concat_call(program: &CheckedProgram, call: &crate::core::ir::ResolvedCall) -> bool {
    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return false;
    };
    builtin.as_str() == "builtin.method.list.concat"
        && call.arguments.len() == 2
        && call.arguments.iter().all(|argument| {
            is_resolved_list_type(program, &argument.value.ty, &mut BTreeSet::new())
        })
}

fn is_scalar_set_contains_call(
    program: &CheckedProgram,
    call: &crate::core::ir::ResolvedCall,
) -> bool {
    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return false;
    };
    builtin.as_str() == "contains"
        && call.arguments.len() == 2
        && call.arguments.first().is_some_and(|argument| {
            is_resolved_set_type(program, &argument.value.ty, &mut BTreeSet::new())
                && is_scalar_collection_type(program, &argument.value.ty, &mut BTreeSet::new())
        })
}

fn is_scalar_println_call(program: &CheckedProgram, call: &crate::core::ir::ResolvedCall) -> bool {
    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return false;
    };
    if builtin.as_str() != "println" || call.arguments.len() != 1 {
        return false;
    }
    call.arguments.first().is_some_and(|argument| {
        matches!(
            program.resolved_types().get(&argument.value.ty),
            Some(ResolvedType::Primitive(PrimitiveType::Bool))
                | Some(ResolvedType::Primitive(PrimitiveType::I32))
                | Some(ResolvedType::Primitive(PrimitiveType::I64))
                // R6-1050: the StringHandle stdout face is differential-pinned
                // owned (fresh-clone consumption) and borrowed (record-field
                // receipt) across all three consumers, so it joins the closed
                // print admission here — leaving it as mixed shape would turn
                // previously compatible programs into hard route rejections.
                | Some(ResolvedType::Primitive(PrimitiveType::String))
                // R6-1053: the f64 print face is differential-pinned
                // through the shared shortest round-trip runtime formatter;
                // R6-1054 admits the print-face local read beside the
                // literal, so a complete float-stdout-only graph routes
                // canonical.
                | Some(ResolvedType::Primitive(PrimitiveType::F64))
        )
    })
}

fn is_resolved_set_type(
    program: &CheckedProgram,
    id: &crate::core::ResolvedTypeId,
    seen: &mut BTreeSet<crate::core::ResolvedTypeId>,
) -> bool {
    if !seen.insert(id.clone()) {
        return false;
    }
    match program.resolved_types().get(id) {
        Some(ResolvedType::Nominal { item, .. }) => item.as_str() == "builtin:type:Set",
        Some(ResolvedType::Reference { target, .. })
        | Some(ResolvedType::Ownership { target, .. })
        | Some(ResolvedType::Newtype { inner: target, .. }) => {
            is_resolved_set_type(program, target, seen)
        }
        _ => false,
    }
}

fn is_resolved_list_type(
    program: &CheckedProgram,
    id: &crate::core::ResolvedTypeId,
    seen: &mut BTreeSet<crate::core::ResolvedTypeId>,
) -> bool {
    if !seen.insert(id.clone()) {
        return false;
    }
    match program.resolved_types().get(id) {
        Some(ResolvedType::Nominal { item, .. }) => item.as_str() == "builtin:type:List",
        Some(ResolvedType::Reference { target, .. })
        | Some(ResolvedType::Ownership { target, .. })
        | Some(ResolvedType::Newtype { inner: target, .. }) => {
            is_resolved_list_type(program, target, seen)
        }
        _ => false,
    }
}

fn is_resolved_nested_list_type(
    program: &CheckedProgram,
    id: &crate::core::ResolvedTypeId,
) -> bool {
    let Some(ResolvedType::Nominal {
        item, arguments, ..
    }) = program.resolved_types().get(id)
    else {
        return false;
    };
    if item.as_str() != "builtin:type:List" || arguments.len() != 1 {
        return false;
    }
    arguments.first().is_some_and(|inner_id| {
        matches!(
            program.resolved_types().get(inner_id),
            Some(ResolvedType::Nominal {
                item: child_item,
                arguments: child_arguments,
                ..
            }) if child_item.as_str() == "builtin:type:List" && child_arguments.len() == 1
        )
    })
}

fn is_scalar_set_facade_call(
    program: &CheckedProgram,
    call: &crate::core::ir::ResolvedCall,
) -> bool {
    let ResolvedCallee::Function(template) = &call.callee else {
        return false;
    };
    let Some(callable) = program.callable(template) else {
        return false;
    };
    !call.type_arguments.is_empty()
        && callable.signature.generic_parameters.len() == 1
        && mentions_generic_set(
            program,
            &callable.signature.parameters,
            &callable.signature.result,
            &callable.signature.generic_parameters,
        )
}

fn is_scalar_list_facade_call(
    program: &CheckedProgram,
    call: &crate::core::ir::ResolvedCall,
) -> bool {
    let ResolvedCallee::Function(template) = &call.callee else {
        return false;
    };
    let Some(callable) = program.callable(template) else {
        return false;
    };
    !call.type_arguments.is_empty()
        && callable.signature.generic_parameters.len() == 1
        && mentions_generic_list(
            program,
            &callable.signature.parameters,
            &callable.signature.result,
            &callable.signature.generic_parameters,
        )
}

/// Distinguish the small generic List operation facades admitted by this
/// island from unrelated generic functions that merely mention `List<T>`.
/// Projection (`first<T>(List<T>)`) and nested container operation helpers
/// remain compatibility shapes; direct List `len`/`reverse`/`concat` builtins
/// and List construction now cross the migrated-candidate hard boundary below.
fn generic_list_operation_facade_body(
    program: &CheckedProgram,
    call: &crate::core::ir::ResolvedCall,
) -> bool {
    let ResolvedCallee::Function(template) = &call.callee else {
        return false;
    };
    let Some(callable) = program.callable(template) else {
        return false;
    };
    generic_list_body_has_operation(&callable.body.root)
}

/// Body-level walker shared by the call-site facade predicate and the
/// callable-level mixedness whitelist: does this block contain a direct List
/// `len`/`reverse`/`concat` operation, nested inside any expression shape?
fn generic_list_body_has_operation(root: &crate::core::ir::ResolvedBlock) -> bool {
    fn expr_has_operation(expression: &ResolvedExpr) -> bool {
        match &expression.kind {
            ResolvedExprKind::Call(call) => {
                let direct = matches!(
                    &call.callee,
                    ResolvedCallee::Builtin(name)
                        if matches!(name.as_str(),
                            "len"
                                | "builtin.method.list.len"
                                | "reverse"
                                | "builtin.method.list.reverse"
                                | "builtin.method.list.concat")
                );
                direct || call
                    .arguments
                    .iter()
                    .any(|argument| expr_has_operation(&argument.value))
            }
            ResolvedExprKind::FString(parts) => parts.iter().any(|part| {
                matches!(part, ResolvedFStringPart::Interpolation(value) if expr_has_operation(value))
            }),
            ResolvedExprKind::Project { value, projection } => {
                matches!(projection, ResolvedValueProjection::Index(_))
                    || expr_has_operation(value)
                    || matches!(projection, ResolvedValueProjection::Index(index) if expr_has_operation(index))
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                expr_has_operation(left) || expr_has_operation(right)
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand) => expr_has_operation(operand),
            ResolvedExprKind::Tuple(items)
            | ResolvedExprKind::List(items)
            | ResolvedExprKind::Set(items) => items.iter().any(expr_has_operation),
            ResolvedExprKind::Map(items) => items
                .iter()
                .any(|(key, value)| expr_has_operation(key) || expr_has_operation(value)),
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                expr_has_operation(value)
                    || expr_has_operation(iterable)
                    || guard.as_ref().is_some_and(|guard| expr_has_operation(guard))
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => expr_has_operation(receiver),
            ResolvedExprKind::Record { fields, rest, .. } => {
                fields.iter().any(|field| expr_has_operation(&field.value))
                    || rest.as_ref().is_some_and(|value| expr_has_operation(value))
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => block_has_operation(block),
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                expr_has_operation(condition)
                    || block_has_operation(then_block)
                    || block_has_operation(else_block)
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                expr_has_operation(scrutinee)
                    || arms.iter().any(|arm| {
                        arm.guard.as_ref().is_some_and(expr_has_operation)
                            || expr_has_operation(&arm.body)
                    })
            }
            ResolvedExprKind::Try { value, .. } => expr_has_operation(value),
            ResolvedExprKind::Range { start, end } => {
                expr_has_operation(start) || expr_has_operation(end)
            }
            ResolvedExprKind::Slice { target, start, end } => {
                expr_has_operation(target)
                    || start.as_ref().is_some_and(|value| expr_has_operation(value))
                    || end.as_ref().is_some_and(|value| expr_has_operation(value))
            }
            ResolvedExprKind::Cast { value, .. } => expr_has_operation(value),
            ResolvedExprKind::Lambda(lambda) => block_has_operation(&lambda.body),
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Load(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => false,
        }
    }
    fn statement_has_operation(statement: &crate::core::ir::ResolvedStmt) -> bool {
        match &statement.kind {
            ResolvedStmtKind::Bind { initializer, .. } => initializer
                .as_ref()
                .is_some_and(|value| expr_has_operation(value)),
            ResolvedStmtKind::Assign { value, .. }
            | ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => expr_has_operation(value),
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => value
                .as_ref()
                .is_some_and(|value| expr_has_operation(value)),
            ResolvedStmtKind::While { condition, body } => {
                expr_has_operation(condition) || block_has_operation(body)
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            } => expr_has_operation(initializer) || block_has_operation(body),
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                expr_has_operation(initializer)
                    || block_has_operation(then_block)
                    || else_block.as_ref().is_some_and(block_has_operation)
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                block_has_operation(body)
            }
            ResolvedStmtKind::For { iterable, body, .. } => {
                expr_has_operation(iterable) || block_has_operation(body)
            }
            ResolvedStmtKind::Math(expressions) => expressions.iter().any(expr_has_operation),
            ResolvedStmtKind::Pinned { value, body, .. } => {
                expr_has_operation(value) || block_has_operation(body)
            }
            ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::Continue
            | ResolvedStmtKind::NestedCallable(_) => false,
        }
    }
    fn block_has_operation(block: &crate::core::ir::ResolvedBlock) -> bool {
        block.statements.iter().any(statement_has_operation)
            || block
                .result
                .as_ref()
                .is_some_and(|value| expr_has_operation(value))
    }
    block_has_operation(root)
}

/// Return whether a generic List facade body constructs a List value. This is
/// a checker-owned hard-boundary hint only; the exact one-element Copy-scalar
/// shape is proven later by Canonical MIR materialization. Any other concrete
/// element (for example `wrap<T>("managed")`) must therefore be rejected
/// before a legacy backend can observe the call.
fn generic_list_construction_facade_body(
    program: &CheckedProgram,
    call: &crate::core::ir::ResolvedCall,
) -> bool {
    let ResolvedCallee::Function(template) = &call.callee else {
        return false;
    };
    let Some(callable) = program.callable(template) else {
        return false;
    };
    generic_list_body_has_construction(&callable.body.root)
}

/// Body-level walker shared by the call-site construction predicate and the
/// callable-level mixedness whitelist: does this block construct a List
/// value, nested inside any expression shape?
fn generic_list_body_has_construction(root: &crate::core::ir::ResolvedBlock) -> bool {
    fn expr_has_construction(expression: &ResolvedExpr) -> bool {
        match &expression.kind {
            ResolvedExprKind::List(_) => true,
            ResolvedExprKind::Call(call) => call
                .arguments
                .iter()
                .any(|argument| expr_has_construction(&argument.value)),
            ResolvedExprKind::FString(parts) => parts.iter().any(|part| {
                matches!(part, ResolvedFStringPart::Interpolation(value) if expr_has_construction(value))
            }),
            ResolvedExprKind::Project { value, projection } => {
                expr_has_construction(value)
                    || matches!(projection, ResolvedValueProjection::Index(index) if expr_has_construction(index))
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                expr_has_construction(left) || expr_has_construction(right)
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand) => expr_has_construction(operand),
            ResolvedExprKind::Tuple(items) | ResolvedExprKind::Set(items) => {
                items.iter().any(expr_has_construction)
            }
            ResolvedExprKind::Map(items) => items.iter().any(|(key, value)| {
                expr_has_construction(key) || expr_has_construction(value)
            }),
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                expr_has_construction(value)
                    || expr_has_construction(iterable)
                    || guard.as_ref().is_some_and(|guard| expr_has_construction(guard))
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => expr_has_construction(receiver),
            ResolvedExprKind::Record { fields, rest, .. } => {
                fields.iter().any(|field| expr_has_construction(&field.value))
                    || rest.as_ref().is_some_and(|value| expr_has_construction(value))
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => block_has_construction(block),
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                expr_has_construction(condition)
                    || block_has_construction(then_block)
                    || block_has_construction(else_block)
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                expr_has_construction(scrutinee)
                    || arms.iter().any(|arm| {
                        arm.guard.as_ref().is_some_and(expr_has_construction)
                            || expr_has_construction(&arm.body)
                    })
            }
            ResolvedExprKind::Try { value, .. } => expr_has_construction(value),
            ResolvedExprKind::Range { start, end } => {
                expr_has_construction(start) || expr_has_construction(end)
            }
            ResolvedExprKind::Slice { target, start, end } => {
                expr_has_construction(target)
                    || start.as_ref().is_some_and(|value| expr_has_construction(value))
                    || end.as_ref().is_some_and(|value| expr_has_construction(value))
            }
            ResolvedExprKind::Cast { value, .. } => expr_has_construction(value),
            ResolvedExprKind::Lambda(lambda) => block_has_construction(&lambda.body),
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Load(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => false,
        }
    }
    fn statement_has_construction(statement: &crate::core::ir::ResolvedStmt) -> bool {
        match &statement.kind {
            ResolvedStmtKind::Bind { initializer, .. } => initializer
                .as_ref()
                .is_some_and(|value| expr_has_construction(value)),
            ResolvedStmtKind::Assign { value, .. }
            | ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => expr_has_construction(value),
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => value
                .as_ref()
                .is_some_and(|value| expr_has_construction(value)),
            ResolvedStmtKind::While { condition, body } => {
                expr_has_construction(condition) || block_has_construction(body)
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            } => expr_has_construction(initializer) || block_has_construction(body),
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                expr_has_construction(initializer)
                    || block_has_construction(then_block)
                    || else_block.as_ref().is_some_and(block_has_construction)
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                block_has_construction(body)
            }
            ResolvedStmtKind::For { iterable, body, .. } => {
                expr_has_construction(iterable) || block_has_construction(body)
            }
            ResolvedStmtKind::Math(expressions) => expressions.iter().any(expr_has_construction),
            ResolvedStmtKind::Pinned { value, body, .. } => {
                expr_has_construction(value) || block_has_construction(body)
            }
            ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::Continue
            | ResolvedStmtKind::NestedCallable(_) => false,
        }
    }
    fn block_has_construction(block: &crate::core::ir::ResolvedBlock) -> bool {
        block.statements.iter().any(statement_has_construction)
            || block
                .result
                .as_ref()
                .is_some_and(|value| expr_has_construction(value))
    }
    block_has_construction(root)
}

/// Callable-level mirror of the admitted generic List facade call shape: one
/// generic parameter, a `List<T>`-mentioning signature, and a body whose
/// direct List operation or List construction the island scanner already
/// admits at the call site.  The shared declaration-mixedness floor uses this
/// to keep an admitted facade distinct from an unrelated generic function
/// that merely mentions `List<T>`.
fn is_generic_list_facade_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    callable.signature.generic_parameters.len() == 1
        && mentions_generic_list(
            program,
            &callable.signature.parameters,
            &callable.signature.result,
            &callable.signature.generic_parameters,
        )
        && (generic_list_body_has_operation(&callable.body.root)
            || generic_list_body_has_construction(&callable.body.root))
}

/// Callable-level mirror of `is_scalar_set_facade_call`: one generic
/// parameter over a `Set<T>`-mentioning signature.  Shape-only by design —
/// the call-site scan independently flags unsupported element instantiations
/// before any route decision consumes the coverage floor.
fn is_generic_set_facade_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    callable.signature.generic_parameters.len() == 1
        && mentions_generic_set(
            program,
            &callable.signature.parameters,
            &callable.signature.result,
            &callable.signature.generic_parameters,
        )
}

fn mentions_generic_list(
    program: &CheckedProgram,
    parameters: &[crate::core::ir::ResolvedParameter],
    result: &crate::core::ResolvedTypeId,
    generic_parameters: &[NodeId],
) -> bool {
    parameters.iter().any(|parameter| {
        mentions_generic_list_type(
            program,
            &parameter.ty,
            generic_parameters,
            &mut BTreeSet::new(),
        )
    }) || mentions_generic_list_type(program, result, generic_parameters, &mut BTreeSet::new())
}

fn mentions_generic_list_type(
    program: &CheckedProgram,
    id: &crate::core::ResolvedTypeId,
    generic_parameters: &[NodeId],
    seen: &mut BTreeSet<crate::core::ResolvedTypeId>,
) -> bool {
    if !seen.insert(id.clone()) {
        return false;
    }
    match program.resolved_types().get(id) {
        Some(ResolvedType::Nominal {
            item, arguments, ..
        }) => {
            (item.as_str() == "builtin:type:List"
                && arguments.iter().any(|argument| {
                    contains_generic_parameter(
                        program,
                        argument,
                        generic_parameters,
                        &mut BTreeSet::new(),
                    )
                }))
                || arguments.iter().any(|argument| {
                    mentions_generic_list_type(program, argument, generic_parameters, seen)
                })
        }
        Some(ResolvedType::Option(inner))
        | Some(ResolvedType::CBuffer(inner))
        | Some(ResolvedType::Ownership { target: inner, .. })
        | Some(ResolvedType::Newtype { inner, .. })
        | Some(ResolvedType::Slice(inner))
        | Some(ResolvedType::RawPointer { target: inner, .. }) => {
            mentions_generic_list_type(program, inner, generic_parameters, seen)
        }
        Some(ResolvedType::Result { ok, error }) => {
            mentions_generic_list_type(program, ok, generic_parameters, seen)
                || mentions_generic_list_type(program, error, generic_parameters, seen)
        }
        Some(ResolvedType::Tuple(items)) => items
            .iter()
            .any(|item| mentions_generic_list_type(program, item, generic_parameters, seen)),
        Some(ResolvedType::Array { element, .. }) => {
            mentions_generic_list_type(program, element, generic_parameters, seen)
        }
        Some(ResolvedType::Function {
            parameters, result, ..
        }) => {
            parameters.iter().any(|parameter| {
                mentions_generic_list_type(program, parameter, generic_parameters, seen)
            }) || mentions_generic_list_type(program, result, generic_parameters, seen)
        }
        _ => false,
    }
}

fn mentions_generic_set(
    program: &CheckedProgram,
    parameters: &[crate::core::ir::ResolvedParameter],
    result: &crate::core::ResolvedTypeId,
    generic_parameters: &[NodeId],
) -> bool {
    parameters.iter().any(|parameter| {
        mentions_generic_set_type(
            program,
            &parameter.ty,
            generic_parameters,
            &mut BTreeSet::new(),
        )
    }) || mentions_generic_set_type(program, result, generic_parameters, &mut BTreeSet::new())
}

fn mentions_generic_set_type(
    program: &CheckedProgram,
    id: &crate::core::ResolvedTypeId,
    generic_parameters: &[NodeId],
    seen: &mut BTreeSet<crate::core::ResolvedTypeId>,
) -> bool {
    if !seen.insert(id.clone()) {
        return false;
    }
    match program.resolved_types().get(id) {
        Some(ResolvedType::Nominal {
            item, arguments, ..
        }) => {
            (item.as_str() == "builtin:type:Set"
                && arguments.iter().any(|argument| {
                    contains_generic_parameter(
                        program,
                        argument,
                        generic_parameters,
                        &mut BTreeSet::new(),
                    )
                }))
                || arguments.iter().any(|argument| {
                    mentions_generic_set_type(program, argument, generic_parameters, seen)
                })
        }
        Some(ResolvedType::Option(inner))
        | Some(ResolvedType::CBuffer(inner))
        | Some(ResolvedType::Ownership { target: inner, .. })
        | Some(ResolvedType::Newtype { inner, .. })
        | Some(ResolvedType::Slice(inner))
        | Some(ResolvedType::RawPointer { target: inner, .. }) => {
            mentions_generic_set_type(program, inner, generic_parameters, seen)
        }
        Some(ResolvedType::Result { ok, error }) => {
            mentions_generic_set_type(program, ok, generic_parameters, seen)
                || mentions_generic_set_type(program, error, generic_parameters, seen)
        }
        Some(ResolvedType::Tuple(items)) => items
            .iter()
            .any(|item| mentions_generic_set_type(program, item, generic_parameters, seen)),
        Some(ResolvedType::Array { element, .. }) => {
            mentions_generic_set_type(program, element, generic_parameters, seen)
        }
        Some(ResolvedType::Function {
            parameters, result, ..
        }) => {
            parameters.iter().any(|parameter| {
                mentions_generic_set_type(program, parameter, generic_parameters, seen)
            }) || mentions_generic_set_type(program, result, generic_parameters, seen)
        }
        _ => false,
    }
}

fn contains_generic_parameter(
    program: &CheckedProgram,
    id: &crate::core::ResolvedTypeId,
    generic_parameters: &[NodeId],
    seen: &mut BTreeSet<crate::core::ResolvedTypeId>,
) -> bool {
    if !seen.insert(id.clone()) {
        return false;
    }
    match program.resolved_types().get(id) {
        Some(ResolvedType::GenericParameter(parameter)) => generic_parameters.contains(parameter),
        Some(ResolvedType::Nominal { arguments, .. }) => arguments.iter().any(|argument| {
            contains_generic_parameter(program, argument, generic_parameters, seen)
        }),
        Some(ResolvedType::Option(inner))
        | Some(ResolvedType::CBuffer(inner))
        | Some(ResolvedType::Ownership { target: inner, .. })
        | Some(ResolvedType::Newtype { inner, .. })
        | Some(ResolvedType::Slice(inner))
        | Some(ResolvedType::RawPointer { target: inner, .. }) => {
            contains_generic_parameter(program, inner, generic_parameters, seen)
        }
        Some(ResolvedType::Result { ok, error }) => {
            contains_generic_parameter(program, ok, generic_parameters, seen)
                || contains_generic_parameter(program, error, generic_parameters, seen)
        }
        Some(ResolvedType::Tuple(items)) => items
            .iter()
            .any(|item| contains_generic_parameter(program, item, generic_parameters, seen)),
        Some(ResolvedType::Array { element, .. }) => {
            contains_generic_parameter(program, element, generic_parameters, seen)
        }
        Some(ResolvedType::Function {
            parameters, result, ..
        }) => {
            parameters.iter().any(|parameter| {
                contains_generic_parameter(program, parameter, generic_parameters, seen)
            }) || contains_generic_parameter(program, result, generic_parameters, seen)
        }
        _ => false,
    }
}

/// Checker-owned admission state for the flat Copy-record verifier island.
///
/// This is intentionally computed before MIR materialization.  A materialized
/// candidate is not enough for a public verifier API: the API verifies a
/// whole checked program, so a generic, imported, effectful, or otherwise
/// mixed sibling must not be silently omitted from the MIR graph and thereby
/// receive a partial green result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlatCopyRecordAdmission {
    /// No executable typed body or signature uses a user record.
    OutsideProfile,
    /// A record is used, but the complete program is outside the currently
    /// closed verifier island.  This is an explicit compatibility boundary;
    /// it is not a MIR construction failure.
    MixedCoverage,
    /// The checker-owned typed program is closed enough that construction
    /// failure is a hard MIR materialization error rather than a fallback.
    CompleteCoverage,
}

/// Classify flat Copy-record verifier admission from checker-owned artifacts.
///
/// The predicate deliberately does not build MIR and never consults the
/// retained surface AST.  Its conservative mixed-coverage checks protect the
/// public whole-program verifier from returning a partial MIR subgraph for a
/// program containing generic templates, imports, effects, or other semantic
/// consumers not yet covered by this island.
pub fn classify_flat_copy_record_admission(program: &CheckedProgram) -> FlatCopyRecordAdmission {
    let unsupported_record_declared = program.type_defs().values().any(|definition| {
        definition.kind == crate::core::ResolvedTypeKind::Record
            && !is_flat_copy_record_definition(program, definition)
            && !is_scalar_generic_record_definition(program, definition)
            && !is_owned_generic_record_definition(program, definition)
            && !is_owned_generic_record_update_definition_used(program, definition)
    });
    let record_ids = program
        .type_defs()
        .values()
        .filter(|definition| definition.kind == crate::core::ResolvedTypeKind::Record)
        .map(|definition| definition.node_id.0.clone())
        .collect::<BTreeSet<_>>();
    if record_ids.is_empty() {
        return FlatCopyRecordAdmission::OutsideProfile;
    }

    let uses_record = program_uses_record(program, &record_ids);
    if !uses_record {
        return FlatCopyRecordAdmission::OutsideProfile;
    }

    if unsupported_record_declared
        || has_mixed_coverage(program)
        || flat_record_body_has_unmigrated_shape(program)
    {
        FlatCopyRecordAdmission::MixedCoverage
    } else {
        FlatCopyRecordAdmission::CompleteCoverage
    }
}

/// Return whether a checker-resolved generic record projection looks like the
/// S108 candidate. Default dispatch uses this only on the mixed compatibility
/// path to reject instead of silently handing a recognized record projection
/// (including one whose concrete argument later fails TypeDesc admission) to
/// legacy code.
pub fn has_unsupported_generic_record_projection_candidate(program: &CheckedProgram) -> bool {
    program.callables().values().any(|callable| {
        if callable.signature.generic_parameters.len() != 1
            || callable.signature.parameters.len() != 1
        {
            return false;
        }
        let Some(generic_ty) = generic_parameter_type_id(program, callable) else {
            return false;
        };
        let Some(parameter) = callable.signature.parameters.first() else {
            return false;
        };
        let Some(ResolvedType::Nominal {
            item, arguments, ..
        }) = program
            .resolved_types()
            .get(&parameter.ty)
        else {
            return false;
        };
        let qualified_name = item
            .as_str()
            .strip_prefix("type:")
            .unwrap_or(item.as_str());
        let Some(definition) = program.type_def(qualified_name) else {
            return false;
        };
        arguments.as_slice() == [generic_ty.clone()]
            && callable.signature.result == generic_ty
            && matches!(
                callable.body.root.result.as_deref().map(|expr| &expr.kind),
                Some(ResolvedExprKind::Load(place))
                    if matches!(place.projections.as_slice(), [crate::core::ir::ResolvedProjection::Field { .. }])
            )
            && definition.kind == crate::core::ResolvedTypeKind::Record
    })
}

/// Return whether a checker-resolved generic record update resembles the S171
/// envelope but falls outside the single-Copy-override contract.  The default
/// dispatcher uses this on the compatibility path to reject before legacy.
pub fn has_unsupported_generic_record_update_candidate(program: &CheckedProgram) -> bool {
    program.callables().values().any(|callable| {
        generic_record_update_envelope(program, callable).is_some()
            && !is_scalar_generic_record_update_callable(program, callable)
            && !is_owned_generic_record_update_callable(program, callable)
    })
}

/// Return whether the checked program contains any generic record-update
/// envelope, including an otherwise recognized envelope whose concrete
/// TypeDesc later fails materialization.  Route diagnostics use this broader
/// predicate to keep a construction failure attached to the generic-record
/// profile instead of collapsing it into an unrelated flat-record message.
pub fn has_generic_record_update_candidate(program: &CheckedProgram) -> bool {
    program
        .callables()
        .values()
        .any(|callable| generic_record_update_envelope(program, callable).is_some())
}

/// Check the checker-owned shape that the flat record island is allowed to
/// admit.  This mirrors the public TypeDesc contract without constructing MIR:
/// the declaration must be concrete, non-empty, and every resolved field must
/// be one of the signed scalar/bool leaves accepted by `validate_copy_scalar`.
fn is_flat_copy_record_definition(
    program: &CheckedProgram,
    definition: &crate::core::ResolvedTypeDef,
) -> bool {
    if definition.kind != crate::core::ResolvedTypeKind::Record
        || !definition.generic_parameters.is_empty()
        || definition.fields.is_empty()
    {
        return false;
    }

    definition.fields.iter().all(|(name, _)| {
        definition
            .field_ids
            .get(name)
            .and_then(|field_id| program.resolved_field_type(field_id))
            .and_then(|field_ty| program.resolved_types().get(field_ty))
            .is_some_and(|field_ty| {
                matches!(
                    field_ty,
                    ResolvedType::Primitive(
                        PrimitiveType::I32 | PrimitiveType::I64 | PrimitiveType::Bool
                    )
                )
            })
    })
}

/// The generic record island admits one, two, three, four, five, six, or seven fields. At least one field must
/// be the sole generic binder; any sibling is either that same binder or a
/// concrete Copy scalar. Concrete `T` is supplied by the nominal use and
/// materialized into TypeDesc before any backend consumes the layout. The
/// two-, three-, four-, five-, six-, and seven-field forms are intentionally bounded heterogeneous
/// aggregate extensions; managed/nested and larger records remain outside this island.
fn is_scalar_generic_record_definition(
    program: &CheckedProgram,
    definition: &crate::core::ResolvedTypeDef,
) -> bool {
    if definition.kind != crate::core::ResolvedTypeKind::Record
        || definition.generic_parameters.len() != 1
        || !matches!(definition.fields.len(), 1 | 2 | 3 | 4 | 5 | 6 | 7)
    {
        return false;
    }
    let Some((_, binder)) = definition.generic_parameters.first() else {
        return false;
    };
    let mut has_generic_field = false;
    let fields_valid = definition.fields.iter().all(|(name, _)| {
        let Some(field_ty) = definition
            .field_ids
            .get(name)
            .and_then(|field_id| program.resolved_field_type(field_id))
            .and_then(|field_id| program.resolved_types().get(field_id))
        else {
            return false;
        };
        match field_ty {
            ResolvedType::GenericParameter(candidate) if candidate == binder => {
                has_generic_field = true;
                true
            }
            ResolvedType::Primitive(
                PrimitiveType::I32 | PrimitiveType::I64 | PrimitiveType::Bool,
            ) => true,
            _ => false,
        }
    });
    fields_valid && has_generic_field
}

/// The managed generic record residual island admits exactly two or three
/// homogeneous fields, or the bounded heterogeneous two-, three-, or
/// four-field forms with one generic field and one, two, or three concrete
/// `String` siblings, plus the two-field generic + one `List<Copy scalar>` form
/// and the three-field generic + two concrete List form, or one List plus one
/// String residual, or one List plus two String residuals, or one Set residual.
/// Concrete managed specialization and Move/Drop glue are proved later by
/// TypeDesc. Keeping this checker-side predicate separate from the Copy-record
/// shape prevents a larger or otherwise mixed managed record from entering
/// the scalar projection island.
fn is_owned_generic_record_definition(
    program: &CheckedProgram,
    definition: &crate::core::ResolvedTypeDef,
) -> bool {
    if definition.kind != crate::core::ResolvedTypeKind::Record
        || definition.generic_parameters.len() != 1
        || !matches!(definition.fields.len(), 2 | 3 | 4)
    {
        return false;
    }
    let Some((_, binder)) = definition.generic_parameters.first() else {
        return false;
    };
    let mut generic_fields = 0usize;
    let mut owned_string_fields = 0usize;
    let mut owned_list_fields = 0usize;
    let mut owned_set_fields = 0usize;
    let fields_admitted = definition.fields.iter().all(|(name, _)| {
        let Some(field_ty) = definition
            .field_ids
            .get(name)
            .and_then(|field_id| program.resolved_field_type(field_id))
            .and_then(|field_id| program.resolved_types().get(field_id))
        else {
            return false;
        };
        match field_ty {
            ResolvedType::GenericParameter(candidate) if candidate == binder => {
                generic_fields += 1;
                true
            }
            ResolvedType::Primitive(PrimitiveType::String) => {
                owned_string_fields += 1;
                true
            }
            ResolvedType::Nominal {
                item, arguments, ..
            } if item.as_str() == "builtin:type:List"
                && arguments.len() == 1
                && arguments.first().is_some_and(|argument| {
                    matches!(
                        program.resolved_types().get(argument),
                        Some(ResolvedType::Primitive(
                            PrimitiveType::I32 | PrimitiveType::I64 | PrimitiveType::Bool
                        ))
                    )
                }) =>
            {
                owned_list_fields += 1;
                true
            }
            ResolvedType::Nominal {
                item, arguments, ..
            } if item.as_str() == "builtin:type:Set"
                && arguments.len() == 1
                && arguments.first().is_some_and(|argument| {
                    matches!(
                        program.resolved_types().get(argument),
                        Some(ResolvedType::Primitive(
                            PrimitiveType::I32 | PrimitiveType::I64 | PrimitiveType::Bool
                        ))
                    )
                }) =>
            {
                owned_set_fields += 1;
                true
            }
            _ => false,
        }
    });
    fields_admitted
        && ((generic_fields == definition.fields.len() && matches!(definition.fields.len(), 2 | 3))
            || (generic_fields == 1
                && matches!(definition.fields.len(), 2 | 3 | 4)
                && owned_string_fields + generic_fields == definition.fields.len())
            || (definition.fields.len() == 2
                && generic_fields == 1
                && owned_list_fields == 1
                && owned_string_fields == 0)
            || (definition.fields.len() == 3
                && generic_fields == 1
                && owned_list_fields == 2
                && owned_string_fields == 0)
            || (definition.fields.len() == 3
                && generic_fields == 1
                && owned_list_fields == 1
                && owned_string_fields == 1)
            || (definition.fields.len() == 4
                && generic_fields == 1
                && owned_list_fields == 1
                && owned_string_fields == 2)
            || (definition.fields.len() == 2
                && generic_fields == 1
                && owned_set_fields == 1
                && owned_string_fields == 0))
}

/// Recognize the heterogeneous declarations admitted by the ownership-bearing
/// record-update envelope: one generic field and two or three concrete owned
/// String siblings. Update admission remains separate from projection
/// admission even where their declaration envelopes overlap; each callable is
/// still checked against its own residual receipt contract.
fn is_owned_generic_record_update_definition(
    program: &CheckedProgram,
    definition: &crate::core::ResolvedTypeDef,
) -> bool {
    if definition.kind != crate::core::ResolvedTypeKind::Record
        || definition.generic_parameters.len() != 1
        || !matches!(definition.fields.len(), 3 | 4)
    {
        return false;
    }
    let Some((_, binder)) = definition.generic_parameters.first() else {
        return false;
    };
    let mut generic_fields = 0usize;
    let mut owned_string_fields = 0usize;
    let fields_admitted = definition.fields.iter().all(|(name, _)| {
        let Some(field_ty) = definition
            .field_ids
            .get(name)
            .and_then(|field_id| program.resolved_field_type(field_id))
            .and_then(|field_id| program.resolved_types().get(field_id))
        else {
            return false;
        };
        match field_ty {
            ResolvedType::GenericParameter(candidate) if candidate == binder => {
                generic_fields += 1;
                true
            }
            ResolvedType::Primitive(PrimitiveType::String) => {
                owned_string_fields += 1;
                true
            }
            _ => false,
        }
    });
    fields_admitted
        && generic_fields == 1
        && owned_string_fields + generic_fields == definition.fields.len()
        && matches!(owned_string_fields, 2 | 3)
}

fn is_owned_generic_record_update_definition_used(
    program: &CheckedProgram,
    definition: &crate::core::ResolvedTypeDef,
) -> bool {
    is_owned_generic_record_update_definition(program, definition)
        && program.callables().values().any(|callable| {
            is_owned_generic_record_update_callable(program, callable)
                && generic_record_update_envelope(program, callable)
                    .is_some_and(|(_, candidate)| candidate.node_id == definition.node_id)
        })
}

/// Recognize the generic callable envelope for the managed residual record
/// island without inspecting surface AST. The body remains a direct field
/// projection; concrete materialization validates the selected field and the
/// complete residual drop schedule.
fn is_owned_generic_record_projection_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    if callable.signature.generic_parameters.len() != 1 || callable.signature.parameters.len() != 1
    {
        return false;
    }
    let Some(generic_ty) = generic_parameter_type_id(program, callable) else {
        return false;
    };
    let Some(parameter) = callable.signature.parameters.first() else {
        return false;
    };
    let Some(ResolvedType::Nominal {
        item, arguments, ..
    }) = program.resolved_types().get(&parameter.ty)
    else {
        return false;
    };
    let qualified_name = item.as_str().strip_prefix("type:").unwrap_or(item.as_str());
    let Some(definition) = program.type_def(qualified_name) else {
        return false;
    };
    is_owned_generic_record_definition(program, definition)
        && arguments.as_slice() == [generic_ty.clone()]
        && callable.signature.result == generic_ty
        && matches!(
            callable.body.root.result.as_deref().map(|expr| &expr.kind),
            Some(ResolvedExprKind::Load(place))
                if matches!(place.projections.as_slice(), [crate::core::ir::ResolvedProjection::Field { .. }])
        )
}

/// Recognize the only generic callable admitted with the generic record
/// declaration: `get<T>(Record<T>) -> T { record.field }`.  The body check is
/// intentionally syntactic over Resolved IR only; all TypeDesc and receipt
/// details are revalidated after specialization by the MIR builder.
fn is_scalar_generic_record_projection_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    if callable.signature.generic_parameters.len() != 1 || callable.signature.parameters.len() != 1
    {
        return false;
    }
    let Some(generic_ty) = generic_parameter_type_id(program, callable) else {
        return false;
    };
    let Some(parameter) = callable.signature.parameters.first() else {
        return false;
    };
    let Some(ResolvedType::Nominal {
        item, arguments, ..
    }) = program.resolved_types().get(&parameter.ty)
    else {
        return false;
    };
    let qualified_name = item.as_str().strip_prefix("type:").unwrap_or(item.as_str());
    let Some(definition) = program.type_def(qualified_name) else {
        return false;
    };
    is_scalar_generic_record_definition(program, definition)
        && arguments.as_slice() == [generic_ty.clone()]
        && callable.signature.result == generic_ty
        && matches!(
            callable.body.root.result.as_deref().map(|expr| &expr.kind),
            Some(ResolvedExprKind::Load(place))
                if matches!(place.projections.as_slice(), [crate::core::ir::ResolvedProjection::Field { .. }])
        )
}

fn generic_record_update_envelope<'a>(
    program: &'a CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> Option<(
    crate::core::ResolvedTypeId,
    &'a crate::core::ResolvedTypeDef,
)> {
    if callable.signature.generic_parameters.len() != 1 || callable.signature.parameters.len() != 1
    {
        return None;
    }
    let generic_ty = generic_parameter_type_id(program, callable)?;
    let parameter = callable.signature.parameters.first()?;
    let ResolvedType::Nominal {
        item, arguments, ..
    } = program.resolved_types().get(&parameter.ty)?
    else {
        return None;
    };
    if arguments.as_slice() != [generic_ty.clone()] {
        return None;
    }
    let qualified_name = item.as_str().strip_prefix("type:").unwrap_or(item.as_str());
    let definition = program.type_def(qualified_name)?;
    if callable.signature.result != parameter.ty {
        return None;
    }
    let Some(ResolvedExprKind::Record {
        fields,
        rest: Some(rest),
        ..
    }) = callable.body.root.result.as_deref().map(|expr| &expr.kind)
    else {
        return None;
    };
    if fields.is_empty()
        || !matches!(
            &rest.kind,
            ResolvedExprKind::Load(place) if place.projections.is_empty()
        )
    {
        return None;
    }
    Some((generic_ty, definition))
}

/// Recognize the exact bounded generic record update envelope: one generic
/// `Record<T>` parameter/result, one or two explicit concrete Copy-scalar
/// overrides, and a direct record-rest expression.  The complete TypeDesc
/// contract is replayed after specialization by the MIR lowerer.
fn is_scalar_generic_record_update_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    let Some((generic_ty, definition)) = generic_record_update_envelope(program, callable) else {
        return false;
    };
    if definition.kind != crate::core::ResolvedTypeKind::Record
        || definition.generic_parameters.len() != 1
        || !matches!(definition.fields.len(), 2 | 3 | 4)
    {
        return false;
    }
    let Some(ResolvedExprKind::Record { fields, .. }) =
        callable.body.root.result.as_deref().map(|expr| &expr.kind)
    else {
        return false;
    };
    if !matches!(fields.len(), 1 | 2) {
        return false;
    }
    fields.iter().all(|field| {
        if field.value.ty == generic_ty {
            return false;
        }
        let Some(updated_ty) = program.resolved_types().get(&field.value.ty) else {
            return false;
        };
        matches!(
            updated_ty,
            ResolvedType::Primitive(PrimitiveType::I32 | PrimitiveType::I64 | PrimitiveType::Bool)
        )
    })
}

/// Recognize the bounded ownership-bearing update envelope: a two- or
/// three- or four-field `Record<T>` with either homogeneous generic fields, the
/// existing two-field generic-plus-String form, or one generic plus two/three
/// String fields, a single direct String-literal override, and a record-rest
/// expression. The concrete TypeDesc/glue receipt is still materialized only
/// after specialization.
pub(crate) fn is_owned_generic_record_update_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    let Some((generic_ty, definition)) = generic_record_update_envelope(program, callable) else {
        return false;
    };
    if definition.kind != crate::core::ResolvedTypeKind::Record
        || definition.generic_parameters.len() != 1
        || !matches!(definition.fields.len(), 2 | 3 | 4)
    {
        return false;
    }
    let Some((_, binder)) = definition.generic_parameters.first() else {
        return false;
    };
    let mut generic_fields = 0usize;
    let mut string_fields = 0usize;
    for (name, _) in &definition.fields {
        let Some(field_ty) = definition
            .field_ids
            .get(name)
            .and_then(|field_id| program.resolved_field_type(field_id))
            .and_then(|field_id| program.resolved_types().get(field_id))
        else {
            return false;
        };
        match field_ty {
            ResolvedType::GenericParameter(candidate) if candidate == binder => {
                generic_fields += 1;
            }
            ResolvedType::Primitive(PrimitiveType::String) => string_fields += 1,
            _ => return false,
        }
    }
    let Some(ResolvedExprKind::Record { fields, .. }) =
        callable.body.root.result.as_deref().map(|expr| &expr.kind)
    else {
        return false;
    };
    let homogeneous = generic_fields == definition.fields.len() && string_fields == 0;
    let heterogeneous_two =
        definition.fields.len() == 2 && generic_fields == 1 && string_fields == 1;
    let heterogeneous_three =
        definition.fields.len() == 3 && generic_fields == 1 && string_fields == 2;
    let heterogeneous_four =
        definition.fields.len() == 4 && generic_fields == 1 && string_fields == 3;
    let Some(field) = fields.first() else {
        return false;
    };
    (homogeneous || heterogeneous_two || heterogeneous_three || heterogeneous_four)
        && fields.len() == 1
        && field.value.ty != generic_ty
        && matches!(
            &field.value.kind,
            ResolvedExprKind::Literal(crate::core::ir::ResolvedLiteral::String(_))
        )
}

/// Keep the flat-record island closed over the complete typed body, not only
/// over the record declaration.  MIR Phase 0 currently admits scalar
/// expressions, record construction/projection, direct user calls, and
/// structured `if`; collection values, builtin/runtime calls, loops,
/// concurrency, and higher-order expressions belong to other islands.
/// A builtin `println` whose every argument is a signed 32/64-bit integer or
/// a bool lowers as a `PrintlnInt`/`PrintlnBool` builtin inside the
/// flat-record island graph — the same scalar-print face the session-channel
/// island admits (the bool face is differential-pinned standalone and in the
/// Set.contains island).  Every other builtin (and println over any other
/// argument type) keeps the compatibility boundary.  Classifying the admitted
/// face as unmigrated would strand checker-legal record programs on a hard
/// route rejection: once graph construction materializes the record candidate
/// inside mixed coverage, the doctrine forbids falling back to legacy
/// (R6-1048 parity repair after numeric-widen call receipts widened
/// construction; bool widened in R6-1049 under the same rule; the StringHandle
/// face — owned clone and borrowed record-field receipt — widened in R6-1050
/// under the same rule).
fn is_admitted_scalar_print_call(program: &CheckedProgram, call: &ResolvedCall) -> bool {
    if !matches!(
        call.callee,
        ResolvedCallee::Builtin(ref builtin) if builtin.as_str() == "println"
    ) {
        return false;
    }
    if !call.effects.is_empty() || !call.session.is_empty() || call.permission.is_some() {
        return false;
    }
    matches!(
        program.resolved_types().get(&call.result),
        Some(ResolvedType::Primitive(PrimitiveType::Unit))
    ) && call.arguments.iter().all(|argument| {
        matches!(
            program.resolved_types().get(&argument.value.ty),
            Some(ResolvedType::Primitive(
                PrimitiveType::I32 | PrimitiveType::I64 | PrimitiveType::Bool
            )) | Some(ResolvedType::Primitive(PrimitiveType::String))
        )
    })
}

/// The MIR Phase 0 scalar-assign face (R6-1049; R6-1057 adds the Copy f64
/// target): a direct local target with no projections, an Identity or
/// NumericWiden conversion receipt into a signed 32/64-bit integer, bool, or
/// 64-bit float, and nothing else.  Island classifiers must consult this
/// exact predicate before treating an Assign statement as unmigrated shape,
/// so admission can never disagree with what construction accepts: a
/// construction-capability widening that outruns its classifier turns
/// working compatibility programs into hard route rejections (the R6-1048
/// parity lesson).  The lowering side enforces the same face against the
/// target local's materialized ABI class.
pub(crate) fn resolved_assign_is_admitted_scalar_shape(
    program: &CheckedProgram,
    statement: &crate::core::ir::ResolvedStmt,
) -> bool {
    let crate::core::ir::ResolvedStmtKind::Assign {
        target, conversion, ..
    } = &statement.kind
    else {
        return false;
    };
    if !target.projections.is_empty() {
        return false;
    }
    if !matches!(
        conversion.kind,
        crate::core::ir::CheckedConversionKind::Identity
            | crate::core::ir::CheckedConversionKind::NumericWiden
    ) {
        return false;
    }
    matches!(
        program.resolved_types().get(&conversion.to),
        Some(ResolvedType::Primitive(
            PrimitiveType::I32 | PrimitiveType::I64 | PrimitiveType::Bool | PrimitiveType::F64
        ))
    )
}

fn flat_record_body_has_unmigrated_shape(program: &CheckedProgram) -> bool {
    // A managed generic record projection may materialize a one-level
    // `List<Copy scalar>` or `Set<Copy scalar>` payload in its caller.  The
    // collection literal itself is still checker-owned and its MIR
    // construction/glue are validated by the managed record island; rejecting
    // it here would route the whole program to legacy before that island can
    // be selected.  Keep this exception scoped to a program that actually
    // contains the admitted projection, and continue rejecting nested/opaque
    // collection shapes below.
    let admits_managed_record_collection = program.callables().values().any(|callable| {
        is_owned_generic_record_projection_callable(program, callable)
            || is_owned_generic_record_update_callable(program, callable)
    })
        || has_unsupported_generic_record_projection_candidate(program);

    fn is_managed_record_collection_literal(
        program: &CheckedProgram,
        expression: &ResolvedExpr,
        admits_managed_record_collection: bool,
    ) -> bool {
        if !admits_managed_record_collection {
            return false;
        }
        let Some(ResolvedType::Nominal {
            item, arguments, ..
        }) = program.resolved_types().get(&expression.ty)
        else {
            return false;
        };
        if !matches!(item.as_str(), "builtin:type:List" | "builtin:type:Set")
            || arguments.len() != 1
        {
            return false;
        }
        arguments.first().is_some_and(|argument| {
            matches!(
                program.resolved_types().get(argument),
                Some(ResolvedType::Primitive(
                    PrimitiveType::I32 | PrimitiveType::I64 | PrimitiveType::Bool
                ))
            )
        })
    }

    fn expr_has_unmigrated_shape(
        program: &CheckedProgram,
        expression: &ResolvedExpr,
        admits_managed_record_collection: bool,
    ) -> bool {
        match &expression.kind {
            ResolvedExprKind::List(_) => {
                let admitted = is_managed_record_collection_literal(
                    program,
                    expression,
                    admits_managed_record_collection,
                );
                !admitted
            }
            ResolvedExprKind::Set(_) => {
                let admitted = is_managed_record_collection_literal(
                    program,
                    expression,
                    admits_managed_record_collection,
                );
                !admitted
            }
            ResolvedExprKind::Map(_)
            | ResolvedExprKind::Tuple(_)
            | ResolvedExprKind::Comprehension { .. }
            | ResolvedExprKind::OptionalChain { .. }
            | ResolvedExprKind::Try { .. }
            | ResolvedExprKind::Range { .. }
            | ResolvedExprKind::Slice { .. }
            | ResolvedExprKind::Spawn(_)
            | ResolvedExprKind::Await(_)
            | ResolvedExprKind::FString(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::TypeValue(_)
            | ResolvedExprKind::Comptime(_)
            | ResolvedExprKind::Quote(_)
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::TypeOf(_) => true,
            ResolvedExprKind::Project { value, projection } => {
                !matches!(projection, ResolvedValueProjection::Field(_))
                    || expr_has_unmigrated_shape(program, value, admits_managed_record_collection)
                    || matches!(projection, ResolvedValueProjection::Index(_))
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                expr_has_unmigrated_shape(program, left, admits_managed_record_collection)
                    || expr_has_unmigrated_shape(program, right, admits_managed_record_collection)
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::Cast { value: operand, .. } => {
                expr_has_unmigrated_shape(program, operand, admits_managed_record_collection)
            }
            // `old` is a verifier contract wrapper around an otherwise
            // ordinary scalar/record expression, not a runtime shape.
            ResolvedExprKind::Old(value) => {
                expr_has_unmigrated_shape(program, value, admits_managed_record_collection)
            }
            ResolvedExprKind::Call(call) => {
                if is_admitted_scalar_print_call(program, call) {
                    false
                } else {
                    matches!(
                        call.callee,
                        crate::core::ir::ResolvedCallee::Builtin(ref builtin)
                            if !matches!(builtin.as_str(), "Some" | "None" | "Ok" | "Err")
                    ) || !call.effects.is_empty()
                        || !call.session.is_empty()
                        || call.permission.is_some()
                        || call.arguments.iter().any(|argument| {
                            expr_has_unmigrated_shape(
                                program,
                                &argument.value,
                                admits_managed_record_collection,
                            )
                        })
                }
            }
            ResolvedExprKind::Record { fields, rest, .. } => {
                rest.as_ref().is_some_and(|value| {
                    expr_has_unmigrated_shape(program, value, admits_managed_record_collection)
                }) || fields.iter().any(|field| {
                    expr_has_unmigrated_shape(
                        program,
                        &field.value,
                        admits_managed_record_collection,
                    )
                })
            }
            ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
                block_has_unmigrated_shape(program, block, admits_managed_record_collection, false)
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                expr_has_unmigrated_shape(program, condition, admits_managed_record_collection)
                    || block_has_unmigrated_shape(
                        program,
                        then_block,
                        admits_managed_record_collection,
                        false,
                    )
                    || block_has_unmigrated_shape(
                        program,
                        else_block,
                        admits_managed_record_collection,
                        false,
                    )
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                expr_has_unmigrated_shape(program, scrutinee, admits_managed_record_collection)
                    || arms.iter().any(|arm| {
                        arm.guard.as_ref().is_some_and(|guard| {
                            expr_has_unmigrated_shape(
                                program,
                                guard,
                                admits_managed_record_collection,
                            )
                        }) || expr_has_unmigrated_shape(
                            program,
                            &arm.body,
                            admits_managed_record_collection,
                        )
                    })
            }
            ResolvedExprKind::Lambda(lambda) => block_has_unmigrated_shape(
                program,
                &lambda.body,
                admits_managed_record_collection,
                false,
            ),
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Load(_)
            | ResolvedExprKind::Constant(_) => false,
        }
    }

    fn block_has_unmigrated_shape(
        program: &CheckedProgram,
        block: &crate::core::ir::ResolvedBlock,
        admits_managed_record_collection: bool,
        admits_assign: bool,
    ) -> bool {
        block.statements.iter().any(|statement| {
            if !statement.backend_requirements.is_empty() {
                return true;
            }
            match &statement.kind {
                ResolvedStmtKind::Bind { initializer, .. } => {
                    initializer.as_ref().is_some_and(|value| {
                        expr_has_unmigrated_shape(program, value, admits_managed_record_collection)
                    })
                }
                // R6-1049: the root-level scalar-assign face is
                // construction-proven (differential matrix in
                // src/tests/canonical_assign.rs), so it is a migrated shape
                // exactly when its RHS is itself migrated.  Nested-block
                // assigns need block-parameter merges (construction keeps
                // them fail-closed), and every other assign shape stays
                // unmigrated so the compatibility route stands.
                ResolvedStmtKind::Assign { value, .. } => {
                    if admits_assign && resolved_assign_is_admitted_scalar_shape(program, statement)
                    {
                        expr_has_unmigrated_shape(program, value, admits_managed_record_collection)
                    } else {
                        true
                    }
                }
                ResolvedStmtKind::Expr(value)
                | ResolvedStmtKind::Contract {
                    condition: value, ..
                } => expr_has_unmigrated_shape(program, value, admits_managed_record_collection),
                ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                    value.as_ref().is_some_and(|value| {
                        expr_has_unmigrated_shape(program, value, admits_managed_record_collection)
                    })
                }
                ResolvedStmtKind::While { .. }
                | ResolvedStmtKind::WhileLet { .. }
                | ResolvedStmtKind::IfLet { .. }
                | ResolvedStmtKind::Loop(_)
                | ResolvedStmtKind::For { .. }
                | ResolvedStmtKind::Math(_)
                | ResolvedStmtKind::Scope { .. }
                | ResolvedStmtKind::Pinned { .. }
                | ResolvedStmtKind::NestedCallable(_) => true,
                ResolvedStmtKind::Continue | ResolvedStmtKind::Drop(_) => false,
            }
        }) || block.result.as_ref().is_some_and(|value| {
            expr_has_unmigrated_shape(program, value, admits_managed_record_collection)
        })
    }

    program
        .resolved_bodies()
        .values()
        .filter(|body| !is_prelude_origin(program, &body.root.origin))
        .any(|body| {
            block_has_unmigrated_shape(program, &body.root, admits_managed_record_collection, true)
        })
}

pub(super) fn is_prelude_origin(program: &CheckedProgram, origin: &crate::core::Origin) -> bool {
    program
        .source_registry()
        .key(origin.user_span().source_id)
        .is_some_and(|key| key.as_str() == "stdlib:prelude.mimi")
}

pub(super) fn has_mixed_coverage(program: &CheckedProgram) -> bool {
    fn is_runtime_origin(origin: &crate::core::Origin) -> bool {
        matches!(origin, crate::core::Origin::RuntimeSystem { .. })
    }

    let mixed = program.has_imports()
        || program
            .flows()
            .values()
            .any(|flow| !is_runtime_origin(&flow.origin))
        || !program.sessions().is_empty()
        || !program.actors().is_empty()
        || !program.capabilities().is_empty()
        || program
            .traits()
            .values()
            .any(|trait_def| !is_prelude_origin(program, &trait_def.origin))
        || program
            .impls()
            .values()
            .any(|impl_def| !is_prelude_origin(program, &impl_def.origin))
        || !program.extern_blocks().is_empty()
        || program
            .transitions()
            .values()
            .any(|transition| !is_runtime_origin(&transition.origin))
        || !program.backend_requirements().is_empty()
        || program.type_defs().values().any(|definition| {
            matches!(definition.origin, crate::core::Origin::User(_))
                && (!is_scalar_generic_record_definition(program, definition)
                    && !is_owned_generic_record_definition(program, definition)
                    && !is_owned_generic_record_update_definition_used(program, definition)
                    && !definition.generic_parameters.is_empty()
                    || definition.kind != crate::core::ResolvedTypeKind::Record
                        && definition.kind != crate::core::ResolvedTypeKind::Alias
                        && definition.kind != crate::core::ResolvedTypeKind::Newtype)
        })
        || program
            .functions()
            .values()
            .filter(|function| !is_prelude_origin(program, &function.origin))
            .any(|function| {
                    let generic_record_callable = program
                    .callables()
                    .get(&function.node_id)
                    .is_some_and(|callable| {
                        is_scalar_generic_record_projection_callable(program, callable)
                            || is_scalar_generic_record_update_callable(program, callable)
                            || is_owned_generic_record_projection_callable(program, callable)
                            || is_owned_generic_record_update_callable(program, callable)
                    });
                let generic_variant_callable = program
                    .callables()
                    .get(&function.node_id)
                    .is_some_and(|callable| {
                        is_generic_variant_predicate_callable(program, callable)
                            || is_generic_option_projection_callable(program, callable)
                            || is_generic_option_projection_fallback_callable(program, callable)
                            || is_generic_result_projection_callable(program, callable)
                            || is_generic_result_projection_fallback_callable(program, callable)
                            || is_generic_list_facade_callable(program, callable)
                            || is_generic_set_facade_callable(program, callable)
                    });
                ((!generic_record_callable && !generic_variant_callable)
                    && !function.generics.is_empty())
                    || ((!generic_record_callable && !generic_variant_callable)
                        && !function.generic_binders.is_empty())
                    || !function.effects.is_empty()
                    || function.is_async
                    || function.is_comptime
                    || function.extern_abi.is_some()
            })
        || program
            .callables()
            .values()
            .filter(|callable| !is_prelude_origin(program, &callable.body.root.origin))
            .any(|callable| {
                (!is_scalar_generic_record_projection_callable(program, callable)
                    && !is_scalar_generic_record_update_callable(program, callable)
                    && !is_owned_generic_record_projection_callable(program, callable)
                    && !is_owned_generic_record_update_callable(program, callable)
                    && !is_generic_variant_predicate_callable(program, callable)
                    && !is_generic_option_projection_callable(program, callable)
                    && !is_generic_option_projection_fallback_callable(program, callable)
                    && !is_generic_result_projection_callable(program, callable)
                    && !is_generic_result_projection_fallback_callable(program, callable)
                    && !is_generic_list_facade_callable(program, callable)
                    && !is_generic_set_facade_callable(program, callable)
                    && !callable.signature.generic_parameters.is_empty())
                    || !callable.signature.effects.is_empty()
                    || !callable.body.captures.is_empty()
                    || !callable.body.default_values.is_empty()
            })
        // The closed record verifier island proves value semantics for Copy
        // records. A view/mutate parameter is a borrow/effect contract with a
        // separate ownership proof and must remain on the compatibility
        // verifier until that contract has its own MIR consumer island.
        || program.resolved_signatures().values().any(|signature| {
            signature.parameters.iter().any(|parameter| {
                matches!(
                    parameter.permission,
                    Some(crate::core::ir::Permission::View | crate::core::ir::Permission::Mutate)
                )
            })
        });
    mixed
}

/// Scan the checker-owned type references that make up a whole program.
/// `resolved_node_types` is populated for every typed body node; the other
/// maps cover declaration and generated type edges that do not have an
/// expression node.  This keeps admission independent of both source AST and
/// MIR materialization.
fn program_uses_record(program: &CheckedProgram, record_ids: &BTreeSet<String>) -> bool {
    fn contains(
        program: &CheckedProgram,
        ty: &ResolvedTypeId,
        record_ids: &BTreeSet<String>,
        visited: &mut BTreeSet<ResolvedTypeId>,
    ) -> bool {
        if !visited.insert(ty.clone()) {
            return false;
        }
        let Some(resolved) = program.resolved_types().get(ty) else {
            return false;
        };
        match resolved {
            ResolvedType::Nominal {
                item, arguments, ..
            } => {
                record_ids.contains(item.as_str())
                    || arguments
                        .iter()
                        .any(|argument| contains(program, argument, record_ids, visited))
            }
            ResolvedType::Reference { target, .. }
            | ResolvedType::CBuffer(target)
            | ResolvedType::Ownership { target, .. }
            | ResolvedType::Newtype { inner: target, .. }
            | ResolvedType::Slice(target)
            | ResolvedType::RawPointer { target, .. }
            | ResolvedType::Option(target) => contains(program, target, record_ids, visited),
            ResolvedType::Result { ok, error } => {
                contains(program, ok, record_ids, visited)
                    || contains(program, error, record_ids, visited)
            }
            ResolvedType::Tuple(elements) => elements
                .iter()
                .any(|element| contains(program, element, record_ids, visited)),
            ResolvedType::Function {
                parameters, result, ..
            } => {
                parameters
                    .iter()
                    .any(|parameter| contains(program, parameter, record_ids, visited))
                    || contains(program, result, record_ids, visited)
            }
            ResolvedType::Array { element, .. } => contains(program, element, record_ids, visited),
            ResolvedType::FlowStateSet { .. }
            | ResolvedType::Primitive(_)
            | ResolvedType::GenericParameter(_)
            | ResolvedType::Capability(_)
            | ResolvedType::Trait { .. }
            | ResolvedType::DynamicAny { .. } => false,
        }
    }

    let mut visited = BTreeSet::new();
    let mut check = |ty: &ResolvedTypeId| contains(program, ty, record_ids, &mut visited);

    program.resolved_node_types().values().any(&mut check)
        || program.resolved_field_types().values().any(&mut check)
        || program.resolved_type_operands().values().any(&mut check)
        || program
            .resolved_type_arguments()
            .values()
            .flatten()
            .any(&mut check)
        || program.resolved_type_targets().values().any(&mut check)
        || program.resolved_signatures().values().any(|signature| {
            signature
                .parameters
                .iter()
                .any(|parameter| check(&parameter.ty))
                || check(&signature.result)
        })
        || program
            .resolved_bodies()
            .values()
            .any(|body| body.locals.values().any(|local| check(&local.ty)) || check(&body.root.ty))
}

/// Return whether the canonical graph contains an operation that the default
/// scalar production selector recognizes as a migrated production candidate.
///
/// This is intentionally narrower than "the graph mentions a List/Set".  A
/// plain collection value is still a compatibility input; only a materialized
/// `ListOp::Len`/`Reverse`/`Concat`, a receipt-bearing nested List index,
/// `SetOp::Contains`, or checker-owned scalar Set/List facade instance,
/// or exact scalar `BuiltinCall::PrintlnBool`/`PrintlnInt`/`PrintlnString`/
/// `PrintlnFloat`
/// has crossed the S11 production boundary.  R6-1052 admits the owned
/// StringHandle print face into the stdout receipt so a complete
/// string-stdout-only graph routes canonical like an integer/bool one; R6-1053
/// admits the f64 literal print face the same way.
/// Keeping this fact next to the island contract prevents the CLI and direct
/// native entry points from growing independent candidate predicates.
pub fn contains_scalar_collection_candidate(program: &MirProgram) -> bool {
    contains_scalar_collection_operation_candidate(program)
        || program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::BuiltinCall {
                            kind: crate::core::mir::types::MirBuiltinKind::PrintlnBool
                                | crate::core::mir::types::MirBuiltinKind::PrintlnInt
                                | crate::core::mir::types::MirBuiltinKind::PrintlnString
                                | crate::core::mir::types::MirBuiltinKind::PrintlnFloat,
                            ..
                        }
                    )
                })
            })
        })
}

/// Return whether the canonical graph contains a collection operation, as
/// opposed to only the scalar stdout effect.  Route owners use this narrower
/// receipt so an unsupported mixed graph containing `println(i32)` does not
/// accidentally become a collection-island candidate; a pure scalar stdout
/// graph is admitted by its checker-side `CompleteCoverage` state instead.
pub fn contains_scalar_collection_operation_candidate(program: &MirProgram) -> bool {
    let has_list_operation = program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    MirInstructionKind::ListOp {
                        operation: MirListOperation::Len
                            | MirListOperation::Reverse
                            | MirListOperation::Concat,
                        ..
                    }
                )
            })
        })
    });
    let has_set_contains = program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    MirInstructionKind::SetOp {
                        operation: super::MirSetOperation::Contains,
                        ..
                    }
                )
            })
        })
    });
    let has_nested_list_index = program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    MirInstructionKind::Project {
                        projection: super::MirProjection::Index(_),
                        list_index_contract: Some(ref receipt),
                        ..
                    } if receipt.mode
                        == crate::core::mir::types::MirListIndexProjectionMode::CloneNestedList
                )
            })
        })
    });
    has_list_operation
        || has_set_contains
        || has_nested_list_index
        || program.instances().values().any(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarSetFacade { .. }
                    | MirGenericInstanceContract::ScalarListFacade { .. }
                    | MirGenericInstanceContract::ScalarListConstruct { .. }
                    | MirGenericInstanceContract::ScalarListProjection { .. }
            )
        })
}

/// Return whether the canonical executable graph contains a record projection
/// value at a consumer boundary, including the owned residual-record contract.
///
/// This is the MIR-side counterpart of the default route's front-end record
/// hint.  It deliberately examines only materialized values, parameters, and
/// results: a declaration in the checker catalog is not executable evidence.
/// Keeping the predicate with the island contract lets direct native callers
/// and the CLI make the same admission decision without re-reading surface
/// record names or duplicating the TypeDesc rule.
pub fn contains_flat_copy_record_candidate(program: &MirProgram) -> bool {
    let promoted_generic_record_projection = program.instances().values().any(|instance| {
        matches!(
            instance.contract,
            MirGenericInstanceContract::ScalarRecordProjection { .. }
        ) && program
            .functions()
            .get(&instance.function)
            .and_then(|function| {
                function
                    .parameters
                    .first()
                    .and_then(|parameter| function.values.get(parameter))
            })
            .is_some_and(|value| {
                program
                    .type_catalog()
                    .validate_flat_copy_record_with_float(&value.ty, true)
                    .is_ok()
            })
    });
    promoted_generic_record_projection
        || program.instances().values().any(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::OwnedRecordProjection { .. }
                    | MirGenericInstanceContract::OwnedRecordProjectionDrop { .. }
                    | MirGenericInstanceContract::OwnedRecordUpdate { .. }
                    | MirGenericInstanceContract::ScalarRecordUpdate { .. }
            )
        })
        || program.functions().values().any(|function| {
            // The current flat-record native contract emits only simple function
            // symbols.  A qualified trait/impl method may carry an implicit
            // receiver whose type is a flat record, but that declaration is not a
            // record value consumed by this production island.  Treating it as a
            // candidate would make unrelated metadata-only programs cross the
            // default route boundary.
            let Some(owner) = function.owner.0.strip_prefix("function:") else {
                return false;
            };
            if owner.contains(':') {
                return false;
            }
            function
                .parameters
                .iter()
                .filter_map(|parameter| function.values.get(parameter))
                .any(|value| {
                    program
                        .type_catalog()
                        .validate_flat_copy_record(&value.ty)
                        .is_ok()
                })
                || program
                    .type_catalog()
                    .validate_flat_copy_record(&function.result)
                    .is_ok()
                || function.values.values().any(|value| {
                    program
                        .type_catalog()
                        .validate_flat_copy_record(&value.ty)
                        .is_ok()
                })
        })
}

/// Return whether the canonical graph contains an ownership-bearing generic
/// record operation.  Unlike the flat Copy-record receipt, this family is
/// explicitly allowed to coexist with the scalar List island: its
/// `MoveProject`/`MoveProjectDrop` receipt owns the residual release plan.
pub fn contains_owned_record_projection_candidate(program: &MirProgram) -> bool {
    program.instances().values().any(|instance| {
        matches!(
            instance.contract,
            MirGenericInstanceContract::OwnedRecordProjection { .. }
                | MirGenericInstanceContract::OwnedRecordProjectionDrop { .. }
                | MirGenericInstanceContract::OwnedRecordUpdate { .. }
        )
    })
}

/// Return whether a canonical graph contains a materialized generic Option
/// predicate instance. The instance contract is the executable receipt; no
/// source-level generic name or backend representation participates here.
pub fn contains_generic_variant_predicate_candidate(program: &MirProgram) -> bool {
    program.instances().values().any(|instance| {
        matches!(
            instance.contract,
            MirGenericInstanceContract::ScalarVariantPredicate { .. }
        )
    })
}

/// Return whether a canonical graph contains a materialized generic
/// `Option<T>.unwrap()` projection instance.  The specialized projection
/// receipt is the only source of this fact for route consumers.
pub fn contains_generic_option_projection_candidate(program: &MirProgram) -> bool {
    program.instances().values().any(|instance| {
        matches!(
            &instance.contract,
            MirGenericInstanceContract::ScalarVariantProjection { contract }
                if contract.projection.nominal.as_str() == "builtin:type:Option"
        )
    })
}

/// Return whether a canonical graph contains a materialized generic
/// `Option<T>.unwrap_or(T)` projection instance. The fallback receipt is the
/// only source of this fact for route consumers.
pub fn contains_generic_option_projection_fallback_candidate(program: &MirProgram) -> bool {
    program.instances().values().any(|instance| {
        matches!(
            &instance.contract,
            MirGenericInstanceContract::ScalarVariantProjectionFallback { contract }
                if contract.projection.nominal.as_str() == "builtin:type:Option"
        )
    })
}

/// Return whether a canonical graph contains a materialized generic Result
/// `unwrap()` projection instance. The specialized Result receipt is the only
/// source of this fact for route consumers.
pub fn contains_generic_result_projection_candidate(program: &MirProgram) -> bool {
    program.instances().values().any(|instance| {
        matches!(
            &instance.contract,
            MirGenericInstanceContract::ScalarVariantProjection { contract }
                if contract.projection.nominal.as_str() == "builtin:type:Result"
        )
    })
}

/// Return whether a canonical graph contains a materialized generic
/// `Result<T, T>`/`Result<T, i32>.unwrap_or(T)` projection instance. The
/// specialized fallback receipt is the only source of this route fact for
/// consumers.
pub fn contains_generic_result_projection_fallback_candidate(program: &MirProgram) -> bool {
    program.instances().values().any(|instance| {
        matches!(
            &instance.contract,
            MirGenericInstanceContract::ScalarVariantProjectionFallback { contract }
                if contract.projection.nominal.as_str() == "builtin:type:Result"
        )
    })
}

/// Return whether the canonical executable graph contains a direct call
/// carrying the move-owned managed Result ABI receipt.  The receipt, rather
/// than a backend representation, is the materialization fact consumed by
/// route owners.
pub fn contains_managed_result_call_candidate(program: &MirProgram) -> bool {
    program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                let MirInstructionKind::Call {
                    variant_call_contract: Some(receipt),
                    ..
                } = &instruction.kind
                else {
                    return false;
                };
                receipt.mode == crate::core::mir::types::MirVariantCallAbiMode::MoveOwned
                    && program
                        .type_catalog()
                        .get(&receipt.result_ty)
                        .is_some_and(|descriptor| descriptor.kind == MirTypeKind::Result)
            })
        })
    })
}

/// Validate the complete direct managed Result call island before a backend
/// consumes the graph.  Every Result-typed direct call must carry the exact
/// TypeDesc-derived receipt, and every move-owned target must carry the
/// exclusive-return-path proof attached to that receipt.  A missing receipt,
/// target/signature drift, or return-merge failure is a hard MIR error, not an
/// invitation to re-check the source or call a legacy emitter.
pub fn validate_managed_result_call_island(program: &MirProgram) -> Result<(), Vec<String>> {
    let mut errors = BTreeSet::new();
    for function in program.functions().values() {
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                let MirInstructionKind::Call {
                    result,
                    callee: ResolvedCallee::Function(callee),
                    type_arguments,
                    arguments,
                    variant_call_contract,
                    ..
                } = &instruction.kind
                else {
                    continue;
                };
                let Some(result) = result else { continue };
                let Some(result_value) = function.values.get(result) else {
                    errors.insert(format!(
                        "{} managed Result call result value '{}' is absent",
                        MANAGED_RESULT_CALL_ISLAND, result.0
                    ));
                    continue;
                };
                let Some(result_desc) = program.type_catalog().get(&result_value.ty) else {
                    errors.insert(format!(
                        "{} managed Result call result type '{}' is absent",
                        MANAGED_RESULT_CALL_ISLAND,
                        result_value.ty.as_str()
                    ));
                    continue;
                };
                if result_desc.kind != MirTypeKind::Result {
                    continue;
                }
                let Some(target) = program.functions().get(callee) else {
                    errors.insert(format!(
                        "{} direct Result call '{}' target is absent from canonical MIR",
                        MANAGED_RESULT_CALL_ISLAND, callee.0
                    ));
                    continue;
                };
                let Some(receipt) = variant_call_contract else {
                    errors.insert(format!(
                        "{} direct Result call '{}' has no ABI receipt",
                        MANAGED_RESULT_CALL_ISLAND, callee.0
                    ));
                    continue;
                };
                let parameter_types = arguments
                    .iter()
                    .filter_map(|argument| {
                        function.values.get(argument).map(|value| value.ty.clone())
                    })
                    .collect::<Vec<_>>();
                if parameter_types.len() != arguments.len() {
                    errors.insert(format!(
                        "{} direct Result call '{}' has an absent argument value",
                        MANAGED_RESULT_CALL_ISLAND, callee.0
                    ));
                    continue;
                }
                let target_parameter_types = target
                    .parameters
                    .iter()
                    .filter_map(|parameter| target.values.get(parameter))
                    .map(|value| value.ty.clone())
                    .collect::<Vec<_>>();
                if target_parameter_types.len() != target.parameters.len() {
                    errors.insert(format!(
                        "{} direct Result call '{}' target parameter type is absent",
                        MANAGED_RESULT_CALL_ISLAND, callee.0
                    ));
                    continue;
                }
                if parameter_types != target_parameter_types {
                    errors.insert(format!(
                        "{} direct Result call '{}' argument types disagree with callee signature",
                        MANAGED_RESULT_CALL_ISLAND, callee.0
                    ));
                }
                if result_value.ty != target.result {
                    errors.insert(format!(
                        "{} direct Result call '{}' result type disagrees with callee signature",
                        MANAGED_RESULT_CALL_ISLAND, callee.0
                    ));
                    continue;
                }
                if let Err(message) = program.type_catalog().validate_variant_call_abi_receipt(
                    callee,
                    type_arguments,
                    &parameter_types,
                    &target.result,
                    receipt,
                ) {
                    errors.insert(format!(
                        "{} direct Result call '{}' receipt failed: {message}",
                        MANAGED_RESULT_CALL_ISLAND, callee.0
                    ));
                }
                if receipt.mode == MirVariantCallAbiMode::MoveOwned {
                    if let Err(message) = super::validate_move_owned_result_return_merge(
                        target,
                        program.type_catalog(),
                    ) {
                        errors.insert(format!(
                            "{} direct Result call '{}' target return merge failed: {message}",
                            MANAGED_RESULT_CALL_ISLAND, callee.0
                        ));
                    }
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.into_iter().collect())
    }
}

/// Return whether the canonical executable graph contains the S8 silent-local
/// Flow transition operation.
///
/// The checker-owned `is_exact_s8_flow_transition` predicate decides whether a
/// whole checked program may enter the closed S8 island.  This MIR-side
/// predicate is the corresponding materialization receipt for consumers: it
/// prevents a verifier or backend from treating a successful construction with
/// no actual `FlowTransition` node as proof that the admitted operation was
/// lowered.  The operation itself is validated by the shared MIR capability
/// gates before any consumer uses it.
pub fn contains_s8_flow_transition_candidate(program: &MirProgram) -> bool {
    program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(instruction.kind, MirInstructionKind::FlowTransition { .. })
            })
        })
    })
}

/// Materialization receipt for the recoverable Flow profiles (M3 local retry
/// and F2 cross-state Result). The effect is intentionally distinct from S8
/// `SilentLocal`, so a caller cannot mistake a successful graph build for
/// proof that source-return failure semantics were preserved.
pub fn contains_flow_failure_retry_candidate(program: &MirProgram) -> bool {
    program.transitions().values().any(|contract| {
        contract.effect.is_recoverable()
            && contract.targets.len() == 1
            && contract.failure.is_some()
    })
}

/// Materialization receipt for the multi-target Flow union face (`-> A | B`).
/// A graph carrying this receipt has the checker-owned tagged-union return
/// materialized; the route layer still decides whether the face closed onto
/// the executable native contract via [`multi_target_flow_union_face_closed`].
pub fn contains_multi_target_flow_union_candidate(program: &MirProgram) -> bool {
    program.transitions().values().any(|contract| {
        contract.targets.len() > 1 && !contract.is_fallback && !contract.is_ffi_pinned
    })
}

/// Whether every multi-target Flow union in the graph carries the promoted
/// tagged-union contract (one Copy-scalar or owned-String payload field per
/// variant, `validate_multi_target_union_variant`).  A graph whose union
/// face is not fully closed keeps the explicit compatibility route; admitting
/// it would hard-reject working legacy union programs on the default entries.
pub fn multi_target_flow_union_face_closed(program: &MirProgram) -> bool {
    program
        .transitions()
        .values()
        .filter(|contract| {
            contract.targets.len() > 1 && !contract.is_fallback && !contract.is_ffi_pinned
        })
        .all(|contract| {
            super::multi_target_union_shape(contract, program.type_catalog())
                && program
                    .type_catalog()
                    .validate_multi_target_union_variant(&contract.result)
                    .is_ok()
        })
}

/// Whether this function's executable graph contains the f64 literal
/// `PrintlnFloat` print face (R6-1053).  Like the StringHandle face, the
/// island's value/literal admission accepts exactly the Copy f64 values this
/// face materializes, mirroring the checker-level classifier so admission and
/// capability can never disagree.
fn function_contains_println_float(function: &MirFunction) -> bool {
    function.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::BuiltinCall {
                    kind: crate::core::mir::types::MirBuiltinKind::PrintlnFloat,
                    ..
                }
            )
        })
    })
}

/// R6-1068: whether this function's executable graph contains an f64
/// comparison (f64×f64 → Bool).  The comparison face leans on no
/// per-function print envelope — every consumer computes the plain IEEE
/// ordered predicate from the runtime values — so the f64 values and
/// literals that feed it are admitted without the float-print contract the
/// arithmetic and print faces require.  The per-operation checks stay
/// strict: an f64 arithmetic or print shape in the same function still
/// floors on its own envelope.
fn function_contains_float_comparison(program: &MirProgram, function: &MirFunction) -> bool {
    function.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            let MirInstructionKind::Binary {
                result,
                op,
                left,
                right,
            } = &instruction.kind
            else {
                return false;
            };
            if !matches!(
                op,
                ResolvedBinaryOp::Equal
                    | ResolvedBinaryOp::NotEqual
                    | ResolvedBinaryOp::Less
                    | ResolvedBinaryOp::Greater
                    | ResolvedBinaryOp::LessEqual
                    | ResolvedBinaryOp::GreaterEqual
            ) {
                return false;
            }
            let ty_of = |value: &MirValueId| function.values.get(value).map(|value| &value.ty);
            let (Some(result_ty), Some(left_ty), Some(right_ty)) =
                (ty_of(result), ty_of(left), ty_of(right))
            else {
                return false;
            };
            program
                .type_catalog()
                .validate_copy_float_binary(result_ty, left_ty, right_ty, *op)
                .is_ok()
        })
    })
}

/// Whether `function` sits in the one-edge f64 print closure (R6-1070): it
/// prints an f64 itself, or it calls a function whose graph prints an f64,
/// or a function whose graph prints an f64 calls it.  The cross-function f64
/// shapes (a literal argument into a print helper, a called helper's f64
/// parameter/result/root) are exactly modeled by every consumer the same way
/// the single-function print face is, so the island's value/type admission
/// leans on this closure instead of the per-function println alone.  One
/// edge only: a helper of a helper keeps the strict compatibility floor.
fn function_in_float_print_closure(program: &MirProgram, function: &MirFunction) -> bool {
    let calls_function = |caller: &MirFunction, owner: &NodeId| {
        caller.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Call {
                        callee: ResolvedCallee::Function(target),
                        ..
                    } if target == owner
                )
            })
        })
    };
    function_contains_println_float(function)
        || function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                let MirInstructionKind::Call {
                    callee: ResolvedCallee::Function(owner),
                    ..
                } = &instruction.kind
                else {
                    return false;
                };
                program
                    .functions()
                    .get(owner)
                    .is_some_and(function_contains_println_float)
            })
        })
        || program.functions().values().any(|caller| {
            function_contains_println_float(caller) && calls_function(caller, &function.owner)
        })
}

/// Validate the current bounded List/Set whole-program island.
///
/// This is deliberately a second, island-level gate above the generic MIR
/// validator.  The generic validator proves that each instruction is legal;
/// this gate proves that the *entire executable graph* belongs to the same
/// finite consumer envelope.  It never reads `CheckedProgram`, `ResolvedBody`,
/// source names, or a backend ABI.
pub fn validate_scalar_collection_island(program: &MirProgram) -> Result<(), Vec<String>> {
    let mut validator = ScalarCollectionValidator {
        program,
        errors: BTreeSet::new(),
        checked_types: BTreeSet::new(),
        allow_owned_record_family: contains_owned_record_projection_candidate(program),
        function_admits_float_print: false,
        function_admits_float_comparison: false,
        function_admits_float_face_closure: false,
    };
    validator.validate();
    if validator.errors.is_empty() {
        Ok(())
    } else {
        Err(validator.errors.into_iter().collect())
    }
}

struct ScalarCollectionValidator<'a> {
    program: &'a MirProgram,
    errors: BTreeSet<String>,
    checked_types: BTreeSet<crate::core::ResolvedTypeId>,
    allow_owned_record_family: bool,
    function_admits_float_print: bool,
    function_admits_float_comparison: bool,
    function_admits_float_face_closure: bool,
}

impl<'a> ScalarCollectionValidator<'a> {
    fn validate(&mut self) {
        let main = NodeId("function:main".into());
        if !self.program.functions().contains_key(&main) {
            self.error("program has no canonical function:main".into());
        }
        if !self.program.transitions().is_empty() {
            self.error(format!(
                "{SCALAR_COLLECTION_ISLAND} does not admit Flow transition contracts"
            ));
        }

        // `MirProgram` is the executable graph handed to every current
        // consumer.  Inspecting every materialized function is therefore the
        // sound whole-program boundary; unmaterialized checker declarations
        // are intentionally not part of this scan.
        for function in self.program.functions().values() {
            self.function_admits_float_print = function_contains_println_float(function);
            self.function_admits_float_comparison =
                function_contains_float_comparison(self.program, function);
            self.function_admits_float_face_closure =
                function_in_float_print_closure(self.program, function);
            self.validate_function(function);
        }
        for instance in self.program.instances().values() {
            let Some(function) = self.program.functions().get(&instance.function) else {
                self.error(format!(
                    "instance '{}' executable '{}' is absent",
                    instance.id, instance.function.0
                ));
                continue;
            };
            if instance.arguments.len() != 1 {
                self.error(format!(
                    "instance '{}' has {} type arguments; the scalar island requires one",
                    instance.id,
                    instance.arguments.len()
                ));
                continue;
            }
            let Some(argument) = instance.arguments.first() else {
                self.error(format!(
                    "instance '{}' has no type argument after arity validation",
                    instance.id
                ));
                continue;
            };
            if let Err(message) = match &instance.contract {
                MirGenericInstanceContract::OwnedRecordUpdate { .. } => self
                    .program
                    .type_catalog()
                    .validate_owned_record_update_generic_argument(argument),
                MirGenericInstanceContract::OwnedRecordProjection { .. }
                | MirGenericInstanceContract::OwnedRecordProjectionDrop { .. } => self
                    .program
                    .type_catalog()
                    .validate_move_owned_record_payload(argument)
                    .map(|_| ()),
                // Generic Option and heterogeneous generic Result projection
                // islands share the concrete float leaf contract with their
                // promoted direct layouts. Keep each exception receipt- and
                // instance-scoped; collection/record instances remain on the
                // signed-integer/bool scalar boundary until their own ABI
                // contracts are promoted.
                MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Option" =>
                {
                    self.program
                        .type_catalog()
                        .validate_generic_variant_projection_arguments(
                            &instance.arguments,
                            contract,
                        )
                }
                MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } => self
                    .program
                    .type_catalog()
                    .validate_generic_variant_projection_fallback_arguments(
                        &instance.arguments,
                        contract,
                    ),
                _ => self.program.type_catalog().validate_copy_scalar(argument),
            } {
                self.error(format!(
                    "instance '{}' argument is outside the Copy scalar contract: {message}",
                    instance.id
                ));
            }
            match instance.contract {
                MirGenericInstanceContract::ScalarIdentity
                | MirGenericInstanceContract::OwnedStringIdentity
                | MirGenericInstanceContract::ScalarSetFacade { .. }
                | MirGenericInstanceContract::ScalarListFacade { .. }
                | MirGenericInstanceContract::ScalarListConstruct { .. }
                | MirGenericInstanceContract::ScalarListProjection { .. }
                | MirGenericInstanceContract::ScalarRecordProjection { .. }
                | MirGenericInstanceContract::ScalarRecordUpdate { .. }
                | MirGenericInstanceContract::ScalarTupleProjection { .. }
                | MirGenericInstanceContract::OwnedRecordProjection { .. }
                | MirGenericInstanceContract::OwnedRecordProjectionDrop { .. }
                | MirGenericInstanceContract::OwnedRecordUpdate { .. }
                | MirGenericInstanceContract::ScalarVariantPredicate { .. }
                | MirGenericInstanceContract::ScalarVariantProjection { .. }
                | MirGenericInstanceContract::ScalarVariantProjectionFallback { .. } => {}
            }
            // The program constructor and the generic MIR validator already
            // prove the exact instance body.  Keep the island gate explicit
            // about the allowed contract family so a future enum extension
            // cannot silently widen this route.
            if matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarSetFacade { .. }
            ) && !function
                .values
                .values()
                .any(|value| self.is_set_type(&value.ty))
            {
                self.error(format!(
                    "instance '{}' Set facade has no Set value in its canonical body",
                    instance.id
                ));
            }
            if matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantPredicate { .. }
            ) && !function.values.values().any(|value| {
                self.program
                    .type_catalog()
                    .variant_layout(&value.ty)
                    .is_some()
            }) {
                self.error(format!(
                    "instance '{}' Option predicate has no canonical variant value in its body",
                    instance.id
                ));
            }
            if matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarListFacade { .. }
                    | MirGenericInstanceContract::ScalarListConstruct { .. }
                    | MirGenericInstanceContract::ScalarListProjection { .. }
            ) && !function
                .values
                .values()
                .any(|value| self.is_list_type(&value.ty))
            {
                self.error(format!(
                    "instance '{}' List facade has no List value in its canonical body",
                    instance.id
                ));
            }
        }
    }

    fn validate_function(&mut self, function: &MirFunction) {
        for value in function.values.values() {
            self.validate_type(&value.ty, &format!("function '{}' value", function.owner.0));
        }
        self.validate_type(
            &function.result,
            &format!("function '{}' result", function.owner.0),
        );
        if function
            .contracts
            .iter()
            .any(|contract| contract.kind == super::MirContractKind::Invariant)
        {
            self.error(format!(
                "function '{}' invariant contract is outside {SCALAR_COLLECTION_ISLAND}",
                function.owner.0
            ));
        }
        for event in &function.ownership.events {
            if matches!(
                event.kind,
                super::MirOwnershipEventKind::TransferSession
                    | super::MirOwnershipEventKind::TransferChild
                    | super::MirOwnershipEventKind::BorrowShared
                    | super::MirOwnershipEventKind::BorrowMut
                    | super::MirOwnershipEventKind::BorrowEnd
            ) {
                self.error(format!(
                    "function '{}' ownership effect '{}' is outside {SCALAR_COLLECTION_ISLAND}",
                    function.owner.0,
                    event.kind.as_str()
                ));
            }
        }
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                self.validate_instruction(function, &instruction.kind, instruction.id.as_str());
            }
            self.validate_terminator(function, &block.terminator, block.id.as_str());
        }
    }

    fn validate_type(&mut self, ty: &crate::core::ResolvedTypeId, subject: &str) {
        if !self.checked_types.insert(ty.clone()) {
            return;
        }
        let Some(descriptor) = self.program.type_catalog().get(ty).cloned() else {
            self.error(format!("{subject} TypeDesc '{}' is absent", ty.as_str()));
            return;
        };
        let result = match descriptor.layout {
            MirLayout::Unit => {
                if descriptor.is_canonical_ffi_unit() {
                    Ok(())
                } else {
                    Err("Unit TypeDesc has an inconsistent ABI/ownership/glue contract".into())
                }
            }
            MirLayout::Scalar => {
                // R6-1068: the f64 comparison face admits the Copy f64 leaf
                // without the print envelope — the predicate is exactly
                // modeled for any f64 pair — while arithmetic and print
                // shapes keep leaning on their own per-function contracts.
                // R6-1070: the print side of the envelope widens to the
                // one-edge f64 print closure, matching the classifier's
                // cross-function face admission (R6-1048 parity rule).
                if (self.function_admits_float_print
                    || self.function_admits_float_comparison
                    || self.function_admits_float_face_closure)
                    && self.is_print_face_f64(ty)
                {
                    Ok(())
                } else {
                    self.program.type_catalog().validate_copy_scalar(ty)
                }
            }
            MirLayout::List { .. } => self
                .program
                .type_catalog()
                .validate_list_glue(ty, MirGlueOperation::MoveOut),
            MirLayout::Set { element } => self
                .program
                .type_catalog()
                .validate_set_glue(ty, MirGlueOperation::MoveOut)
                .and_then(|()| self.validate_copy_scalar_element(&element)),
            MirLayout::Record { .. } if self.allow_owned_record_family => self
                .program
                .type_catalog()
                .validate_aggregate_glue(ty, MirGlueOperation::MoveOut)
                .and_then(|()| {
                    self.program
                        .type_catalog()
                        .validate_aggregate_glue(ty, MirGlueOperation::Drop)
                }),
            // R6-1076: the canonical owned StringHandle contract is admitted
            // beside the Copy scalars.  The former print-face/owned-record
            // flags narrowed exactly this same `validate_owned_string`
            // predicate; provenance policing stays with the checker-side
            // admission (only string-constant callables, string print faces
            // and admitted literal binds open a String value) and with the
            // per-instruction Move/Clone/Drop/PrintlnString/Call arms below,
            // so a graph cannot create a String value outside those shapes.
            MirLayout::Handle
                if self
                    .program
                    .type_catalog()
                    .validate_owned_string(ty)
                    .is_ok() =>
            {
                Ok(())
            }
            layout => Err(format!(
                "layout {layout:?} is outside {SCALAR_COLLECTION_ISLAND}"
            )),
        };
        if let Err(message) = result {
            self.error(format!(
                "{subject} type '{}' rejected: {message}",
                ty.as_str()
            ));
        }
    }

    fn validate_copy_scalar_element(
        &mut self,
        ty: &crate::core::ResolvedTypeId,
    ) -> Result<(), String> {
        self.program.type_catalog().validate_copy_scalar(ty)
    }

    /// Whether `ty` is exactly the Copy f64 leaf the `PrintlnFloat` print face
    /// materializes.  Deliberately f64-only: f32 values have no print receipt
    /// in this island.
    fn is_print_face_f64(&self, ty: &crate::core::ResolvedTypeId) -> bool {
        self.program
            .type_catalog()
            .get(ty)
            .is_some_and(|descriptor| {
                descriptor.is_canonical_copy_scalar(true)
                    && descriptor.abi == (MirAbiClass::Float { bits: 64 })
            })
    }

    fn validate_instruction(
        &mut self,
        function: &MirFunction,
        instruction: &MirInstructionKind,
        subject: &str,
    ) {
        match instruction {
            MirInstructionKind::Const { result, literal } => {
                let Some(result_ty) = self.value_type(function, result, subject) else {
                    return;
                };
                match literal {
                    ResolvedLiteral::Int(_) | ResolvedLiteral::Bool(_) => {
                        self.require_copy_scalar(&result_ty, subject, "constant result");
                    }
                    ResolvedLiteral::Unit => self.require_unit(&result_ty, subject),
                    ResolvedLiteral::FloatBits(_)
                        if (self.function_admits_float_print
                            || self.function_admits_float_comparison
                            || self.function_admits_float_face_closure)
                            && self.is_print_face_f64(&result_ty) => {}
                    // R6-1076: a String constant is admitted whenever it
                    // carries the canonical owned StringHandle contract —
                    // the same reduction as the Handle arm above; the
                    // literal's provenance (string-constant callables,
                    // string print faces) is the admission scan's contract.
                    ResolvedLiteral::String(_)
                        if self
                            .program
                            .type_catalog()
                            .validate_owned_string(&result_ty)
                            .is_ok() => {}
                    ResolvedLiteral::FloatBits(_) | ResolvedLiteral::String(_) => {
                        self.error(format!(
                            "{subject} literal {literal:?} is outside {SCALAR_COLLECTION_ISLAND}"
                        ))
                    }
                }
            }
            MirInstructionKind::Load { result, place } => {
                if !place.projections.is_empty() {
                    self.error(format!(
                        "{subject} projected Load is outside {SCALAR_COLLECTION_ISLAND}"
                    ));
                }
                if let Some(result_ty) = self.value_type(function, result, subject) {
                    self.require_admitted_type(&result_ty, subject, "Load result");
                }
            }
            MirInstructionKind::Copy { result, source }
            | MirInstructionKind::Move { result, source } => {
                let (Some(result_ty), Some(source_ty)) = (
                    self.value_type(function, result, subject),
                    self.value_type(function, source, subject),
                ) else {
                    return;
                };
                self.require_same_type(&result_ty, &source_ty, subject);
                if matches!(instruction, MirInstructionKind::Copy { .. }) {
                    self.require_copy_scalar(&source_ty, subject, "Copy source");
                } else {
                    self.require_move_or_copy(&source_ty, subject, "Move source");
                }
            }
            MirInstructionKind::Clone { result, source } => {
                let (Some(result_ty), Some(source_ty)) = (
                    self.value_type(function, result, subject),
                    self.value_type(function, source, subject),
                ) else {
                    return;
                };
                self.require_same_type(&result_ty, &source_ty, subject);
                // R6-1054: the print-face f64 slot read (a local read lowers
                // as Clone) joins the admitted set exactly while the
                // surrounding function carries the float println contract —
                // the same per-function shape the type table, Const, and
                // PrintlnFloat arms enforce.  R6-1070: the contract widens to
                // the one-edge f64 print closure so a called helper's f64
                // slot read stays on the same face the classifier admits.
                let admitted = self
                    .program
                    .type_catalog()
                    .validate_copy_scalar(&source_ty)
                    .is_ok()
                    || ((self.function_admits_float_print
                        || self.function_admits_float_face_closure)
                        && self.is_print_face_f64(&source_ty))
                    || self.is_list_type(&source_ty)
                    || self.is_set_type(&source_ty)
                    || self.is_owned_record_or_string_type(&source_ty);
                if !admitted {
                    self.error(format!(
                        "{subject} Clone source '{}' is outside {SCALAR_COLLECTION_ISLAND}",
                        source_ty.as_str()
                    ));
                }
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_glue(&source_ty, MirGlueOperation::Clone)
                {
                    self.error(format!("{subject} Clone glue rejected: {message}"));
                }
            }
            MirInstructionKind::Drop { value } => {
                let Some(ty) = self.value_type(function, value, subject) else {
                    return;
                };
                self.require_move_or_copy(&ty, subject, "Drop value");
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_glue(&ty, MirGlueOperation::Drop)
                {
                    self.error(format!("{subject} Drop glue rejected: {message}"));
                }
            }
            MirInstructionKind::ConstructList {
                result, elements, ..
            } => {
                let Some(result_ty) = self.value_type(function, result, subject) else {
                    return;
                };
                let element_types = elements
                    .iter()
                    .filter_map(|value| self.value_type(function, value, subject))
                    .collect::<Vec<_>>();
                if element_types.len() != elements.len() {
                    return;
                }
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_list_construct(&result_ty, &element_types)
                {
                    self.error(format!("{subject} List construction rejected: {message}"));
                }
            }
            MirInstructionKind::ListOp {
                result,
                operation,
                list,
                argument,
                list_operation_contract,
            } => {
                let (Some(result_ty), Some(list_ty)) = (
                    self.value_type(function, result, subject),
                    self.value_type(function, list, subject),
                ) else {
                    return;
                };
                if !matches!(
                    operation,
                    MirListOperation::Len | MirListOperation::Reverse | MirListOperation::Concat
                ) {
                    self.error(format!(
                        "{subject} List operation {operation:?} is outside {SCALAR_COLLECTION_ISLAND}"
                    ));
                }
                let Some(receipt) = list_operation_contract.as_ref() else {
                    self.error(format!("{subject} List operation has no canonical receipt"));
                    return;
                };
                let argument_ty = argument
                    .as_ref()
                    .and_then(|value| function.values.get(value))
                    .map(|value| value.ty.clone());
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_list_operation_receipt_with_argument(
                        &result_ty,
                        &list_ty,
                        argument_ty.as_ref(),
                        *operation,
                        receipt,
                    )
                {
                    self.error(format!("{subject} List operation rejected: {message}"));
                }
            }
            MirInstructionKind::Project {
                result,
                base,
                projection: super::MirProjection::Index(index),
                list_index_contract,
            } => {
                let (Some(result_ty), Some(base_ty), Some(index_ty)) = (
                    self.value_type(function, result, subject),
                    self.value_type(function, base, subject),
                    self.value_type(function, index, subject),
                ) else {
                    return;
                };
                let Some(receipt) = list_index_contract.as_ref() else {
                    self.error(format!(
                        "{subject} List index projection has no canonical receipt"
                    ));
                    return;
                };
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_list_index_projection_receipt(
                        &base_ty, &index_ty, &result_ty, receipt,
                    )
                {
                    self.error(format!(
                        "{subject} List index projection rejected: {message}"
                    ));
                }
            }
            MirInstructionKind::ConstructSet { result, elements } => {
                let Some(result_ty) = self.value_type(function, result, subject) else {
                    return;
                };
                let element_types = elements
                    .iter()
                    .filter_map(|value| self.value_type(function, value, subject))
                    .collect::<Vec<_>>();
                if element_types.len() != elements.len() {
                    return;
                }
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_set_construct(&result_ty, &element_types)
                {
                    self.error(format!("{subject} Set construction rejected: {message}"));
                }
            }
            MirInstructionKind::SetOp {
                result,
                operation,
                set,
                argument,
            } => {
                let (Some(result_ty), Some(set_ty)) = (
                    self.value_type(function, result, subject),
                    self.value_type(function, set, subject),
                ) else {
                    return;
                };
                let argument_ty = argument
                    .as_ref()
                    .and_then(|value| self.value_type(function, value, subject));
                if argument.is_some() && argument_ty.is_none() {
                    return;
                }
                if let Err(message) = self.program.type_catalog().validate_set_operation(
                    &result_ty,
                    &set_ty,
                    argument_ty.as_ref(),
                    *operation,
                ) {
                    self.error(format!("{subject} Set operation rejected: {message}"));
                }
            }
            MirInstructionKind::Project { .. } => self.error(format!(
                "{subject} non-index projection is outside {SCALAR_COLLECTION_ISLAND}"
            )),
            MirInstructionKind::Binary {
                result,
                op,
                left,
                right,
            } => {
                let (Some(result_ty), Some(left_ty), Some(right_ty)) = (
                    self.value_type(function, result, subject),
                    self.value_type(function, left, subject),
                    self.value_type(function, right, subject),
                ) else {
                    return;
                };
                // R6-1068: the f64 comparison face (f64×f64 → Bool) is
                // admitted on the shared validator contract — the same
                // identity the native validator and the verifier capability
                // gate enforce — so its operands do not lean on the
                // per-function float-print envelope the arithmetic faces
                // use: the predicate is exactly modeled for any f64 pair,
                // symbolic or not.
                let float_comparison_face = matches!(
                    op,
                    ResolvedBinaryOp::Equal
                        | ResolvedBinaryOp::NotEqual
                        | ResolvedBinaryOp::Less
                        | ResolvedBinaryOp::Greater
                        | ResolvedBinaryOp::LessEqual
                        | ResolvedBinaryOp::GreaterEqual
                ) && self
                    .program
                    .type_catalog()
                    .validate_copy_float_binary(&result_ty, &left_ty, &right_ty, *op)
                    .is_ok();
                if !float_comparison_face {
                    self.require_copy_scalar(&left_ty, subject, "binary left operand");
                    self.require_copy_scalar(&right_ty, subject, "binary right operand");
                    self.require_copy_scalar(&result_ty, subject, "binary result");
                }
                if left_ty != right_ty || !binary_supported(*op, &left_ty, &result_ty, self) {
                    self.error(format!(
                        "{subject} binary operator {op:?} is outside {SCALAR_COLLECTION_ISLAND}"
                    ));
                }
            }
            MirInstructionKind::Unary {
                result,
                op,
                operand,
            } => {
                let (Some(result_ty), Some(operand_ty)) = (
                    self.value_type(function, result, subject),
                    self.value_type(function, operand, subject),
                ) else {
                    return;
                };
                match op {
                    ResolvedUnaryOp::Negate => {
                        // R6-1069: the f64 negate face is admitted on the
                        // shared validator contract — the same identity the
                        // native validator and the verifier capability gate
                        // enforce — so it does not lean on the per-function
                        // float-print envelope the arithmetic faces use: IEEE
                        // negation is exactly modeled for any finite f64,
                        // symbolic or not.  Everything else (f32, mixed
                        // widths, non-f64 leaves) keeps the strict
                        // signed-integer face below.
                        let float_negate_face = self
                            .program
                            .type_catalog()
                            .validate_copy_float_unary(&result_ty, &operand_ty, *op)
                            .is_ok();
                        if !float_negate_face {
                            self.require_copy_scalar(&operand_ty, subject, "negate operand");
                            self.require_copy_scalar(&result_ty, subject, "negate result");
                            if result_ty != operand_ty
                                || !is_signed_integer(&self.program.type_catalog(), &operand_ty)
                            {
                                self.error(format!(
                                    "{subject} negate is outside {SCALAR_COLLECTION_ISLAND}"
                                ));
                            }
                        }
                    }
                    ResolvedUnaryOp::Not => {
                        self.require_copy_scalar(&operand_ty, subject, "Not operand");
                        self.require_copy_scalar(&result_ty, subject, "Not result");
                        if !is_bool(&self.program.type_catalog(), &operand_ty)
                            || !is_bool(&self.program.type_catalog(), &result_ty)
                        {
                            self.error(format!(
                                "{subject} Not is outside {SCALAR_COLLECTION_ISLAND}"
                            ));
                        }
                    }
                    ResolvedUnaryOp::BorrowShared
                    | ResolvedUnaryOp::BorrowMutable
                    | ResolvedUnaryOp::Dereference => self.error(format!(
                        "{subject} unary {op:?} is outside {SCALAR_COLLECTION_ISLAND}"
                    )),
                }
            }
            MirInstructionKind::Call {
                result,
                callee,
                type_arguments,
                arguments,
                ..
            } => self.validate_call(
                function,
                result.clone(),
                callee,
                type_arguments,
                arguments,
                subject,
            ),
            MirInstructionKind::Convert { result, source } => {
                let (Some(result_ty), Some(source_ty)) = (
                    self.value_type(function, result, subject),
                    self.value_type(function, source, subject),
                ) else {
                    return;
                };
                self.require_copy_scalar(&source_ty, subject, "conversion source");
                self.require_copy_scalar(&result_ty, subject, "conversion result");
                if self
                    .program
                    .type_catalog()
                    .validate_conversion(&source_ty, &result_ty)
                    .is_err()
                {
                    self.error(format!(
                        "{subject} conversion is outside {SCALAR_COLLECTION_ISLAND}"
                    ));
                }
            }
            MirInstructionKind::BuiltinCall {
                result,
                kind,
                arguments,
                string_field_contract,
            } => {
                if *kind == crate::core::mir::types::MirBuiltinKind::PrintlnString {
                    // R6-1050: the owned StringHandle print face is
                    // differential-pinned across reference, bytecode and
                    // native consumers, so it is admitted next to the scalar
                    // prints.  The borrowed record-field receipt stays with
                    // its owning record island.
                    if string_field_contract.is_some() {
                        self.error(format!(
                            "{subject} borrowed String-field receipt is outside {SCALAR_COLLECTION_ISLAND}"
                        ));
                        return;
                    }
                    if arguments.len() != 1 {
                        self.error(format!(
                            "{subject} builtin '{}' has {} arguments; contract requires 1",
                            crate::core::mir::types::MirBuiltinContract::for_kind(*kind).name,
                            arguments.len()
                        ));
                        return;
                    }
                    let argument = &arguments[0];
                    let Some(argument_ty) = self.value_type(function, argument, subject) else {
                        return;
                    };
                    let Some(result_ty) = self.value_type(function, result, subject) else {
                        return;
                    };
                    if self
                        .program
                        .type_catalog()
                        .validate_owned_string(&argument_ty)
                        .is_err()
                    {
                        self.error(format!(
                            "{subject} builtin 'println' argument is not the canonical owned StringHandle contract"
                        ));
                    }
                    self.require_unit(&result_ty, subject);
                    return;
                }
                if !matches!(
                    kind,
                    crate::core::mir::types::MirBuiltinKind::PrintlnBool
                        | crate::core::mir::types::MirBuiltinKind::PrintlnInt
                        | crate::core::mir::types::MirBuiltinKind::PrintlnFloat
                ) {
                    self.error(format!(
                        "{subject} builtin {kind:?} is outside {SCALAR_COLLECTION_ISLAND}"
                    ));
                    return;
                }
                let contract = crate::core::mir::types::MirBuiltinContract::for_kind(*kind);
                if arguments.len() != contract.arity {
                    self.error(format!(
                        "{subject} builtin '{}' has {} arguments; contract requires {}",
                        contract.name,
                        arguments.len(),
                        contract.arity
                    ));
                    return;
                }
                let Some(argument) = arguments.first() else {
                    self.error(format!(
                        "{subject} builtin '{}' has no argument after arity validation",
                        contract.name
                    ));
                    return;
                };
                let Some(argument_ty) = self.value_type(function, argument, subject) else {
                    return;
                };
                let Some(result_ty) = self.value_type(function, result, subject) else {
                    return;
                };
                if *kind == crate::core::mir::types::MirBuiltinKind::PrintlnFloat {
                    if !self.is_print_face_f64(&argument_ty) {
                        self.error(format!(
                            "{subject} builtin 'println' argument rejected: type '{}' is not the Copy f64 print-face contract",
                            argument_ty.as_str()
                        ));
                    }
                } else {
                    self.require_copy_scalar(&argument_ty, subject, "println argument");
                }
                let valid_input = match *kind {
                    crate::core::mir::types::MirBuiltinKind::PrintlnBool => {
                        is_bool(&self.program.type_catalog(), &argument_ty)
                    }
                    crate::core::mir::types::MirBuiltinKind::PrintlnInt => {
                        is_signed_integer(&self.program.type_catalog(), &argument_ty)
                    }
                    crate::core::mir::types::MirBuiltinKind::PrintlnFloat => {
                        is_f64(&self.program.type_catalog(), &argument_ty)
                    }
                    _ => false,
                };
                if !valid_input {
                    self.error(format!(
                        "{subject} builtin 'println' input does not satisfy its canonical {} contract",
                        contract.accepted_abi_description()
                    ));
                }
                self.require_unit(&result_ty, subject);
            }
            MirInstructionKind::SessionCall { .. } => self.error(format!(
                "{subject} SessionCall is outside {SCALAR_COLLECTION_ISLAND}"
            )),
            MirInstructionKind::SessionPairBind { .. } => self.error(format!(
                "{subject} typed session_pair binding is outside {SCALAR_COLLECTION_ISLAND}"
            )),
            MirInstructionKind::Nop => {}
            MirInstructionKind::MoveProjectDrop {
                result,
                base,
                contract: Some(receipt),
                ..
            } if self.allow_owned_record_family => {
                let (Some(result_ty), Some(base_ty)) = (
                    self.value_type(function, result, subject),
                    self.value_type(function, base, subject),
                ) else {
                    return;
                };
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_record_move_projection_drop_receipt(&base_ty, &result_ty, receipt)
                {
                    self.error(format!(
                        "{subject} record move/drop projection rejected: {message}"
                    ));
                }
            }
            MirInstructionKind::Construct {
                result,
                kind:
                    super::MirAggregateKind::Record {
                        nominal,
                        fields: field_ids,
                    },
                fields,
            } if self.allow_owned_record_family => {
                self.validate_record_construct(
                    function, result, nominal, field_ids, fields, subject,
                );
            }
            MirInstructionKind::Borrow { .. }
            | MirInstructionKind::EndBorrow { .. }
            | MirInstructionKind::MoveProject { .. }
            | MirInstructionKind::MoveProjectDrop { .. }
            | MirInstructionKind::VariantProject { .. }
            | MirInstructionKind::VariantProjectOr { .. }
            | MirInstructionKind::VariantProjectMove { .. }
            | MirInstructionKind::Construct { .. }
            | MirInstructionKind::ConstructVariant { .. }
            | MirInstructionKind::ConstructVariantMove { .. }
            | MirInstructionKind::UpdateRecord { .. }
            | MirInstructionKind::VariantPredicate { .. }
            | MirInstructionKind::FlowTransition { .. } => self.error(format!(
                "{subject} MIR operation is outside {SCALAR_COLLECTION_ISLAND}"
            )),
        }
    }

    fn validate_record_construct(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        nominal: &NominalTypeId,
        field_ids: &[NodeId],
        fields: &[MirValueId],
        subject: &str,
    ) {
        let Some(result_ty) = self.value_type(function, result, subject) else {
            return;
        };
        let Some(descriptor) = self.program.type_catalog().get(&result_ty) else {
            return;
        };
        let MirLayout::Record {
            nominal: result_nominal,
            fields: layout_fields,
        } = &descriptor.layout
        else {
            self.error(format!(
                "{subject} record construction result is not a canonical record TypeDesc"
            ));
            return;
        };
        if result_nominal != nominal
            || layout_fields.len() != field_ids.len()
            || fields.len() != field_ids.len()
        {
            self.error(format!(
                "{subject} record construction shape disagrees with its TypeDesc"
            ));
            return;
        }
        for ((field_id, field), layout_field) in field_ids.iter().zip(fields).zip(layout_fields) {
            if field_id != &layout_field.id {
                self.error(format!(
                    "{subject} record construction field identity disagrees with its TypeDesc"
                ));
            }
            if let Some(field_ty) = self.value_type(function, field, subject) {
                if field_ty != layout_field.ty {
                    self.error(format!(
                        "{subject} record construction field type disagrees with its TypeDesc"
                    ));
                }
            }
        }
        if let Err(message) = self
            .program
            .type_catalog()
            .validate_aggregate_glue(&result_ty, MirGlueOperation::MoveOut)
        {
            self.error(format!(
                "{subject} record construction glue rejected: {message}"
            ));
        }
    }

    fn validate_call(
        &mut self,
        caller: &MirFunction,
        result: Option<MirValueId>,
        callee: &ResolvedCallee,
        type_arguments: &[crate::core::ResolvedTypeId],
        arguments: &[MirValueId],
        subject: &str,
    ) {
        let ResolvedCallee::Function(owner) = callee else {
            self.error(format!(
                "{subject} callee {callee:?} is outside {SCALAR_COLLECTION_ISLAND}"
            ));
            return;
        };
        let Some(target) = self.program.functions().get(owner) else {
            self.error(format!("{subject} callee '{}' is absent", owner.0));
            return;
        };
        let instance = self
            .program
            .instances()
            .values()
            .find(|instance| instance.function == *owner);
        if let Some(instance) = instance {
            if instance.arguments != type_arguments {
                self.error(format!(
                    "{subject} generic arguments disagree with instance '{}'",
                    instance.id
                ));
            }
        } else if !type_arguments.is_empty() {
            self.error(format!(
                "{subject} generic arguments target a non-instance function"
            ));
        }
        if arguments.len() != target.parameters.len() {
            self.error(format!("{subject} call arity disagrees with callee"));
        }
        for (index, (argument, parameter)) in arguments.iter().zip(&target.parameters).enumerate() {
            let (Some(argument_ty), Some(parameter_ty)) = (
                self.value_type(caller, argument, subject),
                self.value_type(target, parameter, subject),
            ) else {
                continue;
            };
            if argument_ty != parameter_ty {
                self.error(format!(
                    "{subject} call argument {index} TypeDesc disagrees with callee"
                ));
            }
        }
        match result {
            Some(result) => {
                let Some(result_ty) = self.value_type(caller, &result, subject) else {
                    return;
                };
                if result_ty != target.result {
                    self.error(format!(
                        "{subject} call result TypeDesc disagrees with callee"
                    ));
                }
            }
            None => {
                if !self.is_unit_type(&target.result) {
                    self.error(format!(
                        "{subject} non-unit call has no result in {SCALAR_COLLECTION_ISLAND}"
                    ));
                }
            }
        }
    }

    fn validate_terminator(
        &mut self,
        function: &MirFunction,
        terminator: &MirTerminator,
        subject: &str,
    ) {
        match terminator {
            MirTerminator::Goto { .. } => {}
            MirTerminator::Branch { condition, .. } => {
                if let Some(ty) = self.value_type(function, condition, subject) {
                    if !is_bool(&self.program.type_catalog(), &ty) {
                        self.error(format!(
                            "{subject} branch condition is outside {SCALAR_COLLECTION_ISLAND}"
                        ));
                    }
                }
            }
            MirTerminator::Return { value } => match value {
                Some(value) => {
                    if let Some(ty) = self.value_type(function, value, subject) {
                        if ty != function.result {
                            self.error(format!(
                                "{subject} return TypeDesc disagrees with function result"
                            ));
                        }
                    }
                }
                None if !self.is_unit_type(&function.result) => self.error(format!(
                    "{subject} missing non-unit return value in {SCALAR_COLLECTION_ISLAND}"
                )),
                None => {}
            },
            MirTerminator::Trap { .. } => {}
            MirTerminator::Switch { scrutinee, arms } => {
                // R6-1052: a scalar literal switch over a Copy-scalar
                // scrutinee is part of the plain-scalar island (R6-1051
                // admitted the face across construction and both backends).
                // The shared catalog contract keeps every consumer aligned;
                // variant-payload switches keep their fail-closed boundary.
                if let Some(ty) = self.value_type(function, scrutinee, subject) {
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_scalar_switch(&ty, arms)
                    {
                        self.error(format!("{subject} scalar switch rejected: {message}"));
                    }
                }
            }
            MirTerminator::SwitchMove { .. }
            | MirTerminator::Fault { .. }
            | MirTerminator::Unreachable => self.error(format!(
                "{subject} terminator is outside {SCALAR_COLLECTION_ISLAND}"
            )),
        }
    }

    fn value_type(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
    ) -> Option<crate::core::ResolvedTypeId> {
        function
            .values
            .get(value)
            .map(|value| value.ty.clone())
            .or_else(|| {
                self.error(format!("{subject} value '{}' is absent", value));
                None
            })
    }

    fn require_admitted_type(
        &mut self,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
        role: &str,
    ) {
        let valid = self.program.type_catalog().validate_copy_scalar(ty).is_ok()
            || self.is_list_type(ty)
            || self.is_set_type(ty)
            || self.is_owned_record_or_string_type(ty)
            || self.is_unit_type(ty);
        if !valid {
            self.error(format!(
                "{subject} {role} type '{}' is outside {SCALAR_COLLECTION_ISLAND}",
                ty.as_str()
            ));
        }
    }

    fn require_copy_scalar(&mut self, ty: &crate::core::ResolvedTypeId, subject: &str, role: &str) {
        match self.program.type_catalog().validate_copy_scalar(ty) {
            Ok(()) => {}
            Err(message) => {
                // R6-1057: a Copy-role f64 (the widen-assign Convert result,
                // the float arithmetic operands) carries the same
                // per-function print-contract admission as the Move/Clone
                // arms (R6-1054); the envelope stays exactly the
                // classification's float-print function set.  R6-1070: the
                // envelope widens to the one-edge f64 print closure so a
                // called helper's arithmetic leans on the caller's print the
                // same way the classifier admits it.
                if (self.function_admits_float_print || self.function_admits_float_face_closure)
                    && self.is_print_face_f64(ty)
                {
                    return;
                }
                self.error(format!("{subject} {role} rejected: {message}"));
            }
        }
    }

    fn require_move_or_copy(
        &mut self,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
        role: &str,
    ) {
        if self.program.type_catalog().validate_copy_scalar(ty).is_ok() || self.is_unit_type(ty) {
            return;
        }
        // R6-1054: the print-face f64 bind (Const → Move into the local
        // slot) carries the same per-function print contract as the Clone
        // slot read it feeds.  R6-1070: the contract widens to the one-edge
        // f64 print closure, mirroring the Copy-role admission above.
        if (self.function_admits_float_print || self.function_admits_float_face_closure)
            && self.is_print_face_f64(ty)
        {
            return;
        }
        if !self.is_list_type(ty)
            && !self.is_set_type(ty)
            && !self.is_owned_record_or_string_type(ty)
        {
            self.error(format!(
                "{subject} {role} type '{}' is outside {SCALAR_COLLECTION_ISLAND}",
                ty.as_str()
            ));
        }
    }

    fn require_same_type(
        &mut self,
        result: &crate::core::ResolvedTypeId,
        source: &crate::core::ResolvedTypeId,
        subject: &str,
    ) {
        if result != source {
            self.error(format!(
                "{subject} result/source TypeDesc identities disagree"
            ));
        }
    }

    fn require_unit(&mut self, ty: &crate::core::ResolvedTypeId, subject: &str) {
        if !self.is_unit_type(ty) {
            self.error(format!(
                "{subject} unit literal has non-unit TypeDesc '{}'",
                ty.as_str()
            ));
        }
    }

    fn is_unit_type(&self, ty: &crate::core::ResolvedTypeId) -> bool {
        self.program
            .type_catalog()
            .get(ty)
            .is_some_and(MirTypeDesc::is_canonical_ffi_unit)
    }

    fn is_owned_record_or_string_type(&self, ty: &crate::core::ResolvedTypeId) -> bool {
        // R6-1076: the owned-String half is the canonical StringHandle
        // contract itself (same reduction as the Handle value arm) — the
        // per-instruction arms keep policing every Move/Clone/Drop/print of
        // the value, so the flag envelope added no second proof here.
        if self
            .program
            .type_catalog()
            .validate_owned_string(ty)
            .is_ok()
        {
            return true;
        }
        if !self.allow_owned_record_family {
            return false;
        }
        self.program
            .type_catalog()
            .get(ty)
            .is_some_and(|descriptor| {
                matches!(descriptor.layout, MirLayout::Record { .. })
                    && self
                        .program
                        .type_catalog()
                        .validate_aggregate_glue(ty, MirGlueOperation::MoveOut)
                        .is_ok()
                    && self
                        .program
                        .type_catalog()
                        .validate_aggregate_glue(ty, MirGlueOperation::Drop)
                        .is_ok()
            })
    }

    fn is_list_type(&self, ty: &crate::core::ResolvedTypeId) -> bool {
        self.program
            .type_catalog()
            .get(ty)
            .is_some_and(|descriptor| {
                matches!(&descriptor.layout, MirLayout::List { .. })
                    && descriptor.kind == MirTypeKind::List
                    && descriptor.abi == MirAbiClass::OpaqueHandle
                    && descriptor.ownership == MirOwnership::Move
                    && descriptor.glue
                        == (MirGlueContract {
                            move_out: MirGlueKind::List,
                            clone: MirGlueKind::List,
                            drop: MirGlueKind::List,
                        })
            })
    }

    fn is_set_type(&self, ty: &crate::core::ResolvedTypeId) -> bool {
        self.program
            .type_catalog()
            .get(ty)
            .is_some_and(|descriptor| {
                matches!(&descriptor.layout, MirLayout::Set { .. })
                    && descriptor.kind == MirTypeKind::Set
                    && descriptor.abi == MirAbiClass::SetHandle
                    && descriptor.ownership == MirOwnership::Move
                    && descriptor.glue
                        == (MirGlueContract {
                            move_out: MirGlueKind::Set,
                            clone: MirGlueKind::Set,
                            drop: MirGlueKind::Set,
                        })
            })
    }

    fn error(&mut self, message: String) {
        self.errors.insert(message);
    }
}

fn is_signed_integer(
    catalog: &crate::core::mir::types::MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> bool {
    catalog.get(ty).is_some_and(|descriptor| {
        matches!(
            descriptor.abi,
            MirAbiClass::Integer {
                bits: 32 | 64,
                signed: true
            }
        )
    })
}

fn is_bool(
    catalog: &crate::core::mir::types::MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> bool {
    catalog
        .get(ty)
        .is_some_and(|descriptor| descriptor.abi == MirAbiClass::Bool)
}

fn is_f64(
    catalog: &crate::core::mir::types::MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> bool {
    catalog
        .get(ty)
        .is_some_and(|descriptor| descriptor.abi == (MirAbiClass::Float { bits: 64 }))
}

fn binary_supported(
    op: ResolvedBinaryOp,
    left: &crate::core::ResolvedTypeId,
    result: &crate::core::ResolvedTypeId,
    validator: &ScalarCollectionValidator<'_>,
) -> bool {
    let integer = is_signed_integer(&validator.program.type_catalog(), left);
    let boolean = is_bool(&validator.program.type_catalog(), left);
    let float = is_f64(validator.program.type_catalog(), left);
    let result_is_bool = is_bool(&validator.program.type_catalog(), result);
    match op {
        // Keep this matrix identical to the native MIR validator and the
        // verifier capability gate.  The island must be an intersection of
        // consumer capabilities; accepting an operation that only
        // reference/VM can execute would recreate the native-only eligibility
        // drift this gate is meant to prevent.  Multiply/Divide/Remainder on
        // signed integers are admitted by both consumer gates (checked
        // overflow and MIN/-1 / zero-divide traps, SD-7/SD-8) and R6-1052's
        // prelude decoupling made real programs with them visible to this
        // matrix for the first time.  R6-1061: the finite-only f64
        // Add/Subtract face joins under the same rule — the native emitter
        // traps on non-finite operands/results (E0813 guard), the bytecode
        // VM carries the matching float traps, and the MIR verifier models
        // the operation in its IEEE symbolic domain with the E0813
        // definedness obligation.  R6-1067: f64 Multiply/Divide join on the
        // same evidence — overflow stays IEEE-defined (±inf) owned by the
        // shared E0813 finiteness trap, and the ±0.0 divisor is the
        // language-level E0801 violation every consumer raises (the
        // reference executor and native MIR emitter check it explicitly,
        // the bytecode VM through DivFloat's own guard).  R6-1068: the
        // f64 comparison face joins — every consumer computes the plain
        // IEEE ordered predicate with no finiteness trap (the AST VM's
        // LtFloat/GtFloat/LeFloat/GeFloat plus generic Eq/Ne, the AST
        // native's OLT/OGT/OLE/OGE plus OEQ fcmp), so f64×f64 → Bool
        // shapes are admitted obligation-free.  Remainder stays
        // integer-only (no consumer emits frem).
        ResolvedBinaryOp::Add
        | ResolvedBinaryOp::Subtract
        | ResolvedBinaryOp::Multiply
        | ResolvedBinaryOp::Divide => (integer || float) && left == result,
        ResolvedBinaryOp::Remainder => integer && left == result,
        ResolvedBinaryOp::Equal | ResolvedBinaryOp::NotEqual => {
            (integer || boolean || float) && result_is_bool
        }
        ResolvedBinaryOp::Less
        | ResolvedBinaryOp::Greater
        | ResolvedBinaryOp::LessEqual
        | ResolvedBinaryOp::GreaterEqual => (integer || float) && result_is_bool,
        ResolvedBinaryOp::LogicalAnd | ResolvedBinaryOp::LogicalOr => boolean && result_is_bool,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        classify_scalar_collection_admission, contains_scalar_collection_candidate,
        validate_scalar_collection_island, ScalarCollectionAdmission, SCALAR_COLLECTION_ISLAND,
    };
    use crate::core::mir::reference::MirProgram;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn canonical(source: &str) -> MirProgram {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        MirProgram::from_checked_program(&checked).expect("canonical MIR")
    }

    #[test]
    fn accepts_the_complete_scalar_list_set_graph() {
        let program = canonical(include_str!(
            "../../../tests/fixtures/mir_native_list_len.mimi"
        ));
        validate_scalar_collection_island(&program).expect("scalar collection island");
    }

    #[test]
    fn rejects_a_managed_value_mixed_into_the_collection_graph() {
        // R6-1076 restatement: the former pin used an owned StringHandle
        // constant whose value/const arms this slice reduced to the
        // canonical StringHandle contract itself (provenance policing moved
        // to the checker-side admission floor, pinned right below).  The
        // narrowest value still outside the island envelope is the f64 leaf
        // without its print/comparison face.
        let source = "func main() -> i32 { let values = [1, 2, 3] let count = len(values) drop(values) let text = 1.5 drop(text) count }";
        let program = canonical(source);
        let errors = validate_scalar_collection_island(&program)
            .expect_err("managed values must stay outside the scalar collection island");
        assert!(
            errors.iter().any(|error| error.contains("outside")
                || error.contains("Float")
                || error.contains("f64")),
            "{SCALAR_COLLECTION_ISLAND}: {errors:?}"
        );
        // The checker-side provenance floor holds for both shapes: a String
        // constant bound outside any string print face stays mixed.
        let string_source = "func main() -> i32 { let values = [1, 2, 3] let count = len(values) drop(values) let text = \"outside\" drop(text) count }";
        let tokens = Lexer::new(string_source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        assert_eq!(
            classify_scalar_collection_admission(&checked),
            ScalarCollectionAdmission::MixedCoverage,
            "an un-faced String constant bind must keep the compatibility route"
        );
    }

    #[test]
    fn rejects_flow_effects_even_when_the_other_values_are_scalar() {
        let program = canonical(
            "flow Counter { state Zero { n: i32 } transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } } } func main() -> i32 { let c = Zero { n: 1 } let c2 = Counter::inc(c) c2.n }",
        );
        let errors = validate_scalar_collection_island(&program)
            .expect_err("Flow must not enter the synchronous collection island");
        assert!(errors
            .iter()
            .any(|error| error.contains("Flow transition contracts")));
    }

    #[test]
    fn admits_the_typed_scalar_collection_profile_before_materialization() {
        let tokens = Lexer::new(include_str!(
            "../../../tests/fixtures/mir_native_list_len.mimi"
        ))
        .tokenize()
        .expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        assert_eq!(
            classify_scalar_collection_admission(&checked),
            ScalarCollectionAdmission::CompleteCoverage
        );
    }

    #[test]
    fn admits_bare_set_contains_and_materializes_the_shared_set_operation() {
        let tokens = Lexer::new(include_str!(
            "../../../tests/fixtures/mir_native_set_contains_function.mimi"
        ))
        .tokenize()
        .expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        assert_eq!(
            classify_scalar_collection_admission(&checked),
            ScalarCollectionAdmission::CompleteCoverage
        );
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        super::MirInstructionKind::SetOp {
                            operation: crate::core::mir::MirSetOperation::Contains,
                            ..
                        }
                    )
                })
            })
        }));
        validate_scalar_collection_island(&program).expect("SetOp::Contains contract");
        let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference SetOp::Contains execution");
        assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
    }

    #[test]
    fn admits_bool_println_as_a_canonical_stdout_effect() {
        let tokens = Lexer::new(include_str!(
            "../../../tests/fixtures/mir_native_set_contains_println.mimi"
        ))
        .tokenize()
        .expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        assert_eq!(
            classify_scalar_collection_admission(&checked),
            ScalarCollectionAdmission::CompleteCoverage
        );
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let instructions = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .collect::<Vec<_>>();
        assert!(instructions.iter().any(|instruction| {
            matches!(
                instruction.kind,
                super::MirInstructionKind::BuiltinCall {
                    kind: crate::core::mir::types::MirBuiltinKind::PrintlnBool
                        | crate::core::mir::types::MirBuiltinKind::PrintlnInt,
                    ..
                }
            )
        }));
        validate_scalar_collection_island(&program).expect("println(bool) effect contract");
    }

    #[test]
    fn admits_float_literal_println_as_a_canonical_stdout_effect() {
        // R6-1053 restatement: the f64 literal print face joined the closed
        // stdout contract through the shared shortest round-trip formatter,
        // so a float-stdout-only graph is a complete canonical admission.
        let tokens = Lexer::new(include_str!(
            "../../../tests/fixtures/mir_native_println_float.mimi"
        ))
        .tokenize()
        .expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        assert_eq!(
            classify_scalar_collection_admission(&checked),
            ScalarCollectionAdmission::CompleteCoverage
        );
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        super::MirInstructionKind::BuiltinCall {
                            kind: crate::core::mir::types::MirBuiltinKind::PrintlnFloat,
                            ..
                        }
                    )
                })
            })
        }));
        validate_scalar_collection_island(&program).expect("println f64 effect contract");
    }

    #[test]
    fn rejects_aggregate_println_from_the_canonical_stdout_effect() {
        // R6-1053 restatement: with the f64 literal face admitted, the pinned
        // unsupported println shape is the aggregate print — it keeps the
        // stable canonical construction diagnostic and stays an explicit
        // mixed compatibility input on the default route.
        let tokens = Lexer::new(include_str!(
            "../../../tests/fixtures/mir_native_println_aggregate_rejected.mimi"
        ))
        .tokenize()
        .expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        assert_eq!(
            classify_scalar_collection_admission(&checked),
            ScalarCollectionAdmission::MixedCoverage
        );
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("aggregate println must fail before a canonical backend");
        assert!(format!("{error:?}").contains("canonical contract accepts signed i32 or i64"));
    }

    #[test]
    fn admits_signed_integer_println_effect_for_both_widths() {
        let tokens = Lexer::new(include_str!(
            "../../../tests/fixtures/mir_native_println_int.mimi"
        ))
        .tokenize()
        .expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        assert_eq!(
            classify_scalar_collection_admission(&checked),
            ScalarCollectionAdmission::CompleteCoverage
        );
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        super::MirInstructionKind::BuiltinCall {
                            kind: crate::core::mir::types::MirBuiltinKind::PrintlnInt,
                            ..
                        }
                    )
                })
            })
        }));
        validate_scalar_collection_island(&program).expect("println integer effect contract");
    }

    #[test]
    fn admits_standalone_bool_println_without_a_collection_candidate() {
        let tokens = Lexer::new(include_str!(
            "../../../tests/fixtures/mir_native_println_bool_standalone.mimi"
        ))
        .tokenize()
        .expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        assert_eq!(
            classify_scalar_collection_admission(&checked),
            ScalarCollectionAdmission::CompleteCoverage
        );
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        assert!(contains_scalar_collection_candidate(&program));
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        super::MirInstructionKind::BuiltinCall {
                            kind: crate::core::mir::types::MirBuiltinKind::PrintlnBool,
                            ..
                        }
                    )
                })
            })
        }));
        validate_scalar_collection_island(&program).expect("standalone stdout effect contract");
    }

    #[test]
    fn keeps_a_program_without_a_collection_operation_outside_the_profile() {
        let tokens = Lexer::new("func main() -> i32 { 42 }")
            .tokenize()
            .expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        assert_eq!(
            classify_scalar_collection_admission(&checked),
            ScalarCollectionAdmission::OutsideProfile
        );
    }

    #[test]
    fn classifies_a_managed_sibling_as_mixed_before_mir_construction() {
        let tokens = Lexer::new(include_str!(
            "../../../tests/fixtures/mir_test_scalar_collection_mixed.mimi"
        ))
        .tokenize()
        .expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        assert_eq!(
            classify_scalar_collection_admission(&checked),
            ScalarCollectionAdmission::MixedCoverage
        );
    }

    #[test]
    fn keeps_a_collection_comprehension_on_the_compatibility_boundary() {
        let tokens =
            Lexer::new("func main() -> i32 { let xs = [i for i in range(0, 3)]; len(xs) }")
                .tokenize()
                .expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        assert_eq!(
            classify_scalar_collection_admission(&checked),
            ScalarCollectionAdmission::MixedCoverage
        );
    }
}
