//! Whole-program Canonical MIR production-island contracts.
//!
//! This module contains route eligibility, not backend lowering.  The first
//! island is intentionally narrower than the individual List/Set adapters:
//! every executable function in the materialized MIR graph must use only
//! Copy scalar values, move-owned scalar Lists/Sets, synchronous scalar CFG,
//! and canonical scalar calls.  The checker type catalog may contain many
//! unrelated declarations; only types and operations that actually cross the
//! executable MIR graph are inspected here.

use std::collections::BTreeSet;

use crate::core::ir::{
    ResolvedBinaryOp, ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedFStringPart,
    ResolvedLiteral, ResolvedPattern, ResolvedPatternKind, ResolvedStmtKind, ResolvedType,
    ResolvedUnaryOp, ResolvedValueProjection,
};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{
    MirAbiClass, MirGlueContract, MirGlueKind, MirGlueOperation, MirLayout, MirOwnership,
    MirTypeKind,
};
use crate::core::{CheckedProgram, NodeId, PrimitiveType, ResolvedTypeId};

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
/// Name of the generic `Result<T, T>.unwrap()` / `Result<T, i32>.unwrap()`
/// projection island. It is separate from the Option projection profile
/// because `Ok` is tag zero and both Result payload slots participate in the
/// aggregate ABI proof. The distinct `Err i32` shape uses the same
/// receipt-bearing MIR node but a two-slot native aggregate ABI.
pub const GENERIC_RESULT_PROJECTION_ISLAND: &str = "generic-result-projection-v1";
/// Name of the generic `Result<T, T>.unwrap_or(T)` /
/// `Result<T, i32>.unwrap_or(T)` total projection island. It remains distinct
/// from trap-bearing Result projection because both payload slots and the
/// explicit fallback operand participate in the ABI.
pub const GENERIC_RESULT_PROJECTION_FALLBACK_ISLAND: &str = "generic-result-projection-fallback-v1";

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
/// `Result<T, i32>.unwrap()` envelopes before MIR materialization. Only one
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
/// `Result<T, i32>.unwrap_or(T)` envelopes. Only one generic binder, one
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
    let Some(generic_ty) = program.resolved_types().iter().find_map(|(id, ty)| {
        matches!(
            ty,
            ResolvedType::GenericParameter(parameter)
                if parameter == &callable.signature.generic_parameters[0]
        )
        .then_some(id.clone())
    }) else {
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
    let Some(ResolvedType::Option(inner)) = program
        .resolved_types()
        .get(&callable.signature.parameters[0].ty)
    else {
        return false;
    };
    if inner != &generic_ty || callable.signature.parameters[1].ty != generic_ty {
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
        && call.arguments[0].value.ty == callable.signature.parameters[0].ty
        && call.arguments[1].value.ty == generic_ty
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
    let Some(ResolvedType::Result { ok, error }) = program
        .resolved_types()
        .get(&callable.signature.parameters[0].ty)
    else {
        return false;
    };
    let error_is_same_generic = error == &generic_ty;
    let error_is_i32 = matches!(
        program.resolved_types().get(error),
        Some(ResolvedType::Primitive(PrimitiveType::I32))
    );
    if ok != &generic_ty || (!error_is_same_generic && !error_is_i32) {
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
    let Some(ResolvedType::Result { ok, error }) = program
        .resolved_types()
        .get(&callable.signature.parameters[0].ty)
    else {
        return false;
    };
    let error_is_same_generic = error == &generic_ty;
    let error_is_i32 = matches!(
        program.resolved_types().get(error),
        Some(ResolvedType::Primitive(PrimitiveType::I32))
    );
    if ok != &generic_ty
        || (!error_is_same_generic && !error_is_i32)
        || callable.signature.parameters[1].ty != generic_ty
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
        && call.arguments[0].value.ty == callable.signature.parameters[0].ty
        && call.arguments[1].value.ty == generic_ty
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
/// identity/Set facade family, List len/reverse/concat, or an exact Copy-scalar stdout effect can make
/// them part of this island.
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

fn scan_scalar_collection_admission(
    program: &CheckedProgram,
) -> ScalarCollectionAdmissionScanner<'_> {
    let mut scanner = ScalarCollectionAdmissionScanner {
        program,
        has_candidate: false,
        has_unsupported_list_reverse_candidate: false,
        has_unsupported_list_concat_candidate: false,
        has_unsupported_generic_list_facade_candidate: false,
        mixed: false,
        seen_types: BTreeSet::new(),
    };

    for callable in program.callables().values() {
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
            for parameter in &callable.signature.parameters {
                scanner.require_profile_type(&parameter.ty);
            }
            scanner.require_profile_type(&callable.signature.result);
        }
        scanner.visit_block(&callable.body.root, concrete);
        if concrete {
            for value in callable.body.default_values.values() {
                scanner.visit_expr(value, true);
            }
        }
    }

    // These declarations are checker-owned executable dependencies.  A
    // scalar collection call must not silently coexist with another consumer
    // family whose semantics are still supplied by a legacy path.
    let is_runtime_origin =
        |origin: &crate::core::Origin| matches!(origin, crate::core::Origin::RuntimeSystem { .. });
    scanner.mixed |= program.has_imports()
        || program
            .flows()
            .values()
            .any(|flow| !is_runtime_origin(&flow.origin))
        || program
            .transitions()
            .values()
            .any(|transition| !is_runtime_origin(&transition.origin))
        || !program.sessions().is_empty()
        || !program.actors().is_empty()
        || !program.capabilities().is_empty()
        || !program.traits().is_empty()
        || !program.impls().is_empty()
        || !program.extern_blocks().is_empty()
        || !program.backend_requirements().is_empty()
        || program.functions().values().any(|function| {
            function.is_async
                || function.is_comptime
                || function.extern_abi.is_some()
                || !function.effects.is_empty()
        });

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
}

impl<'a> ScalarCollectionAdmissionScanner<'a> {
    fn require_profile_type(&mut self, id: &crate::core::ResolvedTypeId) {
        if !self.seen_types.insert(id.clone()) {
            return;
        }
        if !is_scalar_collection_type(self.program, id, &mut BTreeSet::new()) {
            self.mixed = true;
        }
    }

    fn visit_pattern(&mut self, pattern: &ResolvedPattern, concrete: bool) {
        if concrete {
            self.require_profile_type(&pattern.ty);
        }
        match &pattern.kind {
            ResolvedPatternKind::Constructor { fields, .. } => {
                for (_, field) in fields {
                    self.visit_pattern(field, concrete);
                }
            }
            ResolvedPatternKind::Tuple(items) | ResolvedPatternKind::Array(items) => {
                for item in items {
                    self.visit_pattern(item, concrete);
                }
            }
            ResolvedPatternKind::Slice { prefix, rest } => {
                for item in prefix {
                    self.visit_pattern(item, concrete);
                }
                if let Some(rest) = rest {
                    self.visit_pattern(rest, concrete);
                }
            }
            ResolvedPatternKind::Wildcard
            | ResolvedPatternKind::Binding { .. }
            | ResolvedPatternKind::Literal(_) => {}
        }
    }

    fn visit_block(&mut self, block: &crate::core::ir::ResolvedBlock, concrete: bool) {
        if concrete {
            self.require_profile_type(&block.ty);
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
                    self.visit_pattern(pattern, concrete);
                    if let Some(initializer) = initializer {
                        self.visit_expr(initializer, concrete);
                    }
                }
                ResolvedStmtKind::Assign { value, .. } => self.visit_expr(value, concrete),
                ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                    if let Some(value) = value {
                        self.visit_expr(value, concrete);
                    }
                }
                ResolvedStmtKind::Continue => {}
                ResolvedStmtKind::Expr(value) => self.visit_expr(value, concrete),
                ResolvedStmtKind::While { condition, body } => {
                    self.visit_expr(condition, concrete);
                    self.visit_block(body, concrete);
                }
                ResolvedStmtKind::WhileLet {
                    pattern,
                    initializer,
                    body,
                } => {
                    self.visit_pattern(pattern, concrete);
                    self.visit_expr(initializer, concrete);
                    self.visit_block(body, concrete);
                }
                ResolvedStmtKind::IfLet {
                    pattern,
                    initializer,
                    then_block,
                    else_block,
                } => {
                    self.visit_pattern(pattern, concrete);
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
                    self.visit_pattern(pattern, concrete);
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
                    self.visit_expr(value, concrete);
                    self.visit_block(body, concrete);
                }
                ResolvedStmtKind::NestedCallable(_) => {}
            }
        }
        if let Some(result) = &block.result {
            self.visit_expr(result, concrete);
        }
    }

    fn visit_expr(&mut self, expression: &ResolvedExpr, concrete: bool) {
        if concrete {
            self.require_profile_type(&expression.ty);
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
                self.visit_expr(value, concrete);
                if let ResolvedValueProjection::Index(index) = projection {
                    self.visit_expr(index, concrete);
                }
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                self.visit_expr(left, concrete);
                self.visit_expr(right, concrete);
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
                // Only the closed Copy-scalar stdout effects are Canonical MIR
                // nodes in this island. Other println shapes remain on the
                // explicit mixed compatibility route until their output ABI
                // and effect contract is independently materialized.
                if matches!(
                    &call.callee,
                    ResolvedCallee::Builtin(builtin) if builtin.as_str() == "println"
                ) && !is_scalar_println_call(self.program, call)
                {
                    self.mixed = true;
                }
                for argument in &call.arguments {
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
                self.visit_expr(condition, concrete);
                self.visit_block(then_block, concrete);
                self.visit_block(else_block, concrete);
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
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
            ResolvedExprKind::Lambda(lambda) => self.visit_block(&lambda.body, concrete),
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
            is_scalar_collection_type(program, &arguments[0], seen)
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
        && is_resolved_list_type(program, &call.arguments[0].value.ty, &mut BTreeSet::new())
}

fn is_list_reverse_call(program: &CheckedProgram, call: &crate::core::ir::ResolvedCall) -> bool {
    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return false;
    };
    matches!(builtin.as_str(), "reverse" | "builtin.method.list.reverse")
        && call.arguments.len() == 1
        && is_resolved_list_type(program, &call.arguments[0].value.ty, &mut BTreeSet::new())
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
        && is_resolved_set_type(program, &call.arguments[0].value.ty, &mut BTreeSet::new())
        && is_scalar_collection_type(program, &call.arguments[0].value.ty, &mut BTreeSet::new())
}

fn is_scalar_println_call(program: &CheckedProgram, call: &crate::core::ir::ResolvedCall) -> bool {
    let ResolvedCallee::Builtin(builtin) = &call.callee else {
        return false;
    };
    if builtin.as_str() != "println" || call.arguments.len() != 1 {
        return false;
    }
    matches!(
        program.resolved_types().get(&call.arguments[0].value.ty),
        Some(ResolvedType::Primitive(PrimitiveType::Bool))
            | Some(ResolvedType::Primitive(PrimitiveType::I32))
            | Some(ResolvedType::Primitive(PrimitiveType::I64))
    )
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
/// Projection (`first<T>(List<T>)`) and nested container helpers remain
/// compatibility shapes; direct List `len`/`reverse`/`concat` builtins and
/// List construction now cross the migrated-candidate hard boundary below.
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
    block_has_operation(&callable.body.root)
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
    block_has_construction(&callable.body.root)
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
/// S108 candidate but its declaration/body shape is outside the admitted
/// one- or two-field Copy contract (homogeneous generic fields or a concrete
/// Copy-scalar sibling).  Default dispatch uses this only on the mixed
/// compatibility path to reject instead of silently handing the candidate to
/// legacy code.
pub fn has_unsupported_generic_record_projection_candidate(program: &CheckedProgram) -> bool {
    program.callables().values().any(|callable| {
        if callable.signature.generic_parameters.len() != 1
            || callable.signature.parameters.len() != 1
        {
            return false;
        }
        let Some(generic_ty) = program.resolved_types().iter().find_map(|(id, ty)| {
            matches!(
                ty,
                ResolvedType::GenericParameter(candidate)
                    if candidate == &callable.signature.generic_parameters[0]
            )
            .then_some(id.clone())
        }) else {
            return false;
        };
        let Some(ResolvedType::Nominal {
            item, arguments, ..
        }) = program
            .resolved_types()
            .get(&callable.signature.parameters[0].ty)
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
            && !is_scalar_generic_record_definition(program, definition)
    })
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

/// The generic record island admits one or two fields. At least one field must
/// be the sole generic binder; any sibling is either that same binder or a
/// concrete Copy scalar. Concrete `T` is supplied by the nominal use and
/// materialized into TypeDesc before any backend consumes the layout. The
/// two-field form is intentionally the smallest heterogeneous aggregate
/// extension; managed/nested and larger records remain outside this island.
fn is_scalar_generic_record_definition(
    program: &CheckedProgram,
    definition: &crate::core::ResolvedTypeDef,
) -> bool {
    if definition.kind != crate::core::ResolvedTypeKind::Record
        || definition.generic_parameters.len() != 1
        || !matches!(definition.fields.len(), 1 | 2)
    {
        return false;
    }
    let binder = &definition.generic_parameters[0].1;
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
    let Some(generic_ty) = program.resolved_types().iter().find_map(|(id, ty)| {
        matches!(
            ty,
            ResolvedType::GenericParameter(candidate)
                if candidate == &callable.signature.generic_parameters[0]
        )
        .then_some(id.clone())
    }) else {
        return false;
    };
    let Some(ResolvedType::Nominal {
        item, arguments, ..
    }) = program
        .resolved_types()
        .get(&callable.signature.parameters[0].ty)
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

/// Keep the flat-record island closed over the complete typed body, not only
/// over the record declaration.  MIR Phase 0 currently admits scalar
/// expressions, record construction/projection, direct user calls, and
/// structured `if`; collection values, builtin/runtime calls, loops,
/// concurrency, and higher-order expressions belong to other islands.
fn flat_record_body_has_unmigrated_shape(program: &CheckedProgram) -> bool {
    fn expr_has_unmigrated_shape(expression: &ResolvedExpr) -> bool {
        match &expression.kind {
            ResolvedExprKind::List(_)
            | ResolvedExprKind::Map(_)
            | ResolvedExprKind::Set(_)
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
                    || expr_has_unmigrated_shape(value)
                    || matches!(projection, ResolvedValueProjection::Index(_))
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                expr_has_unmigrated_shape(left) || expr_has_unmigrated_shape(right)
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::Cast { value: operand, .. } => expr_has_unmigrated_shape(operand),
            // `old` is a verifier contract wrapper around an otherwise
            // ordinary scalar/record expression, not a runtime shape.
            ResolvedExprKind::Old(value) => expr_has_unmigrated_shape(value),
            ResolvedExprKind::Call(call) => {
                matches!(
                    call.callee,
                    crate::core::ir::ResolvedCallee::Builtin(ref builtin)
                        if !matches!(builtin.as_str(), "Some" | "None" | "Ok" | "Err")
                ) || !call.effects.is_empty()
                    || !call.session.is_empty()
                    || call.permission.is_some()
                    || call
                        .arguments
                        .iter()
                        .any(|argument| expr_has_unmigrated_shape(&argument.value))
            }
            ResolvedExprKind::Record { fields, rest, .. } => {
                rest.as_ref()
                    .is_some_and(|value| expr_has_unmigrated_shape(value))
                    || fields
                        .iter()
                        .any(|field| expr_has_unmigrated_shape(&field.value))
            }
            ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
                block_has_unmigrated_shape(block)
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                expr_has_unmigrated_shape(condition)
                    || block_has_unmigrated_shape(then_block)
                    || block_has_unmigrated_shape(else_block)
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                expr_has_unmigrated_shape(scrutinee)
                    || arms.iter().any(|arm| {
                        arm.guard.as_ref().is_some_and(expr_has_unmigrated_shape)
                            || expr_has_unmigrated_shape(&arm.body)
                    })
            }
            ResolvedExprKind::Lambda(lambda) => block_has_unmigrated_shape(&lambda.body),
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Load(_)
            | ResolvedExprKind::Constant(_) => false,
        }
    }

    fn block_has_unmigrated_shape(block: &crate::core::ir::ResolvedBlock) -> bool {
        block.statements.iter().any(|statement| {
            if !statement.backend_requirements.is_empty() {
                return true;
            }
            match &statement.kind {
                ResolvedStmtKind::Bind { initializer, .. } => {
                    initializer.as_ref().is_some_and(expr_has_unmigrated_shape)
                }
                ResolvedStmtKind::Assign { value, .. }
                | ResolvedStmtKind::Expr(value)
                | ResolvedStmtKind::Contract {
                    condition: value, ..
                } => expr_has_unmigrated_shape(value),
                ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                    value.as_ref().is_some_and(expr_has_unmigrated_shape)
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
        }) || block
            .result
            .as_ref()
            .is_some_and(|value| expr_has_unmigrated_shape(value))
    }

    program
        .resolved_bodies()
        .values()
        .filter(|body| !is_prelude_origin(program, &body.root.origin))
        .any(|body| block_has_unmigrated_shape(&body.root))
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
                    && !is_generic_variant_predicate_callable(program, callable)
                    && !is_generic_option_projection_callable(program, callable)
                    && !is_generic_option_projection_fallback_callable(program, callable)
                    && !is_generic_result_projection_callable(program, callable)
                    && !is_generic_result_projection_fallback_callable(program, callable)
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
/// `ListOp::Len`/`Reverse`/`Concat`, `SetOp::Contains`, or checker-owned scalar
/// Set/List facade instance,
/// or exact scalar `BuiltinCall::PrintlnBool`/`PrintlnInt` has crossed the
/// S11 production boundary.
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
                                | crate::core::mir::types::MirBuiltinKind::PrintlnInt,
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
    has_list_operation
        || has_set_contains
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

/// Return whether the canonical executable graph contains a flat Copy record
/// value at a consumer boundary.
///
/// This is the MIR-side counterpart of the default route's front-end record
/// hint.  It deliberately examines only materialized values, parameters, and
/// results: a declaration in the checker catalog is not executable evidence.
/// Keeping the predicate with the island contract lets direct native callers
/// and the CLI make the same admission decision without re-reading surface
/// record names or duplicating the TypeDesc rule.
pub fn contains_flat_copy_record_candidate(program: &MirProgram) -> bool {
    program.instances().values().any(|instance| {
        matches!(
            instance.contract,
            MirGenericInstanceContract::OwnedRecordProjection { .. }
                | MirGenericInstanceContract::OwnedRecordProjectionDrop { .. }
        )
    }) || program.functions().values().any(|function| {
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

/// Validate the current scalar List/Set whole-program island.
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
            } else if let Err(message) = match instance.contract {
                MirGenericInstanceContract::OwnedRecordProjection { .. }
                | MirGenericInstanceContract::OwnedRecordProjectionDrop { .. } => self
                    .program
                    .type_catalog()
                    .validate_owned_string(&instance.arguments[0]),
                _ => self
                    .program
                    .type_catalog()
                    .validate_copy_scalar(&instance.arguments[0]),
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
                | MirGenericInstanceContract::OwnedRecordProjection { .. }
                | MirGenericInstanceContract::OwnedRecordProjectionDrop { .. }
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
                if descriptor.kind == MirTypeKind::Primitive(PrimitiveType::Unit)
                    && descriptor.abi == MirAbiClass::Unit
                    && descriptor.ownership == MirOwnership::Copy
                    && is_noop_glue(descriptor.glue)
                {
                    Ok(())
                } else {
                    Err("Unit TypeDesc has an inconsistent ABI/ownership/glue contract".into())
                }
            }
            MirLayout::Scalar => self.program.type_catalog().validate_copy_scalar(ty),
            MirLayout::List { element } => self
                .program
                .type_catalog()
                .validate_list_glue(ty, MirGlueOperation::MoveOut)
                .and_then(|()| self.validate_copy_scalar_element(&element)),
            MirLayout::Set { element } => self
                .program
                .type_catalog()
                .validate_set_glue(ty, MirGlueOperation::MoveOut)
                .and_then(|()| self.validate_copy_scalar_element(&element)),
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
                if self
                    .program
                    .type_catalog()
                    .validate_copy_scalar(&source_ty)
                    .is_err()
                    && !self.is_list_type(&source_ty)
                    && !self.is_set_type(&source_ty)
                {
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
                self.require_copy_scalar(&left_ty, subject, "binary left operand");
                self.require_copy_scalar(&right_ty, subject, "binary right operand");
                self.require_copy_scalar(&result_ty, subject, "binary result");
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
            } => {
                if !matches!(
                    kind,
                    crate::core::mir::types::MirBuiltinKind::PrintlnBool
                        | crate::core::mir::types::MirBuiltinKind::PrintlnInt
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
                let Some(argument_ty) = self.value_type(function, &arguments[0], subject) else {
                    return;
                };
                let Some(result_ty) = self.value_type(function, result, subject) else {
                    return;
                };
                self.require_copy_scalar(&argument_ty, subject, "println argument");
                let valid_input = match *kind {
                    crate::core::mir::types::MirBuiltinKind::PrintlnBool => {
                        is_bool(&self.program.type_catalog(), &argument_ty)
                    }
                    crate::core::mir::types::MirBuiltinKind::PrintlnInt => {
                        is_signed_integer(&self.program.type_catalog(), &argument_ty)
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
            MirInstructionKind::Nop => {}
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
            MirTerminator::Switch { .. }
            | MirTerminator::SwitchMove { .. }
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
            || self.is_unit_type(ty);
        if !valid {
            self.error(format!(
                "{subject} {role} type '{}' is outside {SCALAR_COLLECTION_ISLAND}",
                ty.as_str()
            ));
        }
    }

    fn require_copy_scalar(&mut self, ty: &crate::core::ResolvedTypeId, subject: &str, role: &str) {
        if let Err(message) = self.program.type_catalog().validate_copy_scalar(ty) {
            self.error(format!("{subject} {role} rejected: {message}"));
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
        if !self.is_list_type(ty) && !self.is_set_type(ty) {
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
            .is_some_and(|descriptor| {
                descriptor.kind == MirTypeKind::Primitive(PrimitiveType::Unit)
                    && descriptor.abi == MirAbiClass::Unit
                    && descriptor.ownership == MirOwnership::Copy
                    && is_noop_glue(descriptor.glue)
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

fn is_noop_glue(glue: MirGlueContract) -> bool {
    glue == MirGlueContract {
        move_out: MirGlueKind::Noop,
        clone: MirGlueKind::Noop,
        drop: MirGlueKind::Noop,
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

fn binary_supported(
    op: ResolvedBinaryOp,
    left: &crate::core::ResolvedTypeId,
    result: &crate::core::ResolvedTypeId,
    validator: &ScalarCollectionValidator<'_>,
) -> bool {
    let integer = is_signed_integer(&validator.program.type_catalog(), left);
    let boolean = is_bool(&validator.program.type_catalog(), left);
    let result_is_bool = is_bool(&validator.program.type_catalog(), result);
    match op {
        // Keep this matrix identical to the native MIR validator.  The
        // island must be an intersection of consumer capabilities; accepting
        // an operation that only reference/VM can execute would recreate the
        // native-only eligibility drift this gate is meant to prevent.
        ResolvedBinaryOp::Add | ResolvedBinaryOp::Subtract => integer && left == result,
        ResolvedBinaryOp::Equal | ResolvedBinaryOp::NotEqual => {
            (integer || boolean) && result_is_bool
        }
        ResolvedBinaryOp::Less
        | ResolvedBinaryOp::Greater
        | ResolvedBinaryOp::LessEqual
        | ResolvedBinaryOp::GreaterEqual => integer && result_is_bool,
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
        let program = canonical(
            "func main() -> i32 { let values = [1, 2, 3] let count = len(values) drop(values) let text = \"outside\" drop(text) count }",
        );
        let errors = validate_scalar_collection_island(&program)
            .expect_err("managed values must stay outside the scalar collection island");
        assert!(
            errors.iter().any(|error| {
                error.contains("outside") || error.contains("String") || error.contains("Handle")
            }),
            "{SCALAR_COLLECTION_ISLAND}: {errors:?}"
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
    fn rejects_unsupported_println_from_the_canonical_stdout_effect() {
        let tokens = Lexer::new(include_str!(
            "../../../tests/fixtures/mir_native_println_non_bool_rejected.mimi"
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
            .expect_err("non-bool println must fail before a canonical backend");
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
