use crate::ast::*;
use crate::diagnostic::Diagnostic;

pub(crate) use std::collections::HashMap;

pub mod builtins;
pub mod cfg;
mod checker;
pub(crate) mod helpers;
pub mod phase;
pub mod type_folder;
pub mod unification;

mod check_stmt;
mod infer;
mod infer_expr;
pub mod ir;
mod ownership;
pub mod resolved;

pub(crate) use checker::Checker;
pub(crate) use checker::PendingNestedRestore;
pub use helpers::{fmt_type, is_type_param, subst_type_params};
pub(crate) use helpers::{is_bool, is_numeric_coercion, is_trait_coercion};
#[cfg(test)]
pub(crate) use helpers::{is_int, is_numeric, is_string, same_type};
pub use ir::{
    BackendRequirement, BuiltinId, CheckedConversion, CheckedConversionKind, EffectId,
    FunctionTypeAbi, MethodId, NominalTypeId, OwnershipTypeKind, Permission, PrimitiveType,
    ResolvedArgument, ResolvedBlock, ResolvedBody, ResolvedBodyError, ResolvedCall,
    ResolvedCallable, ResolvedCallee, ResolvedContract, ResolvedExpr, ResolvedExprKind,
    ResolvedIndex, ResolvedLiteral, ResolvedLocal, ResolvedLocalId, ResolvedParameter,
    ResolvedParameterId, ResolvedPattern, ResolvedPatternKind, ResolvedPlace, ResolvedProjection,
    ResolvedSessionAction, ResolvedSignature, ResolvedSignatureError, ResolvedStmt,
    ResolvedStmtKind, ResolvedType, ResolvedTypeCapabilities, ResolvedTypeError, ResolvedTypeId,
    ResolvedTypeName, ResolvedTypeTable, SessionResidualId, SessionTransition, TraitTypeKind,
    RESOLVED_TYPE_SCHEMA_VERSION,
};
pub use ownership::{
    Availability, BranchMerge, CanonicalActionKind, CanonicalResourceAction, CfgLocation,
    IndexProjection, Loan, LoanId, LoanKind, LocalId, Place, PlaceProjection, ResourceAnalysis,
    ResourceFact, ResourceId,
};
pub use resolved::{
    BackendProfile, CheckedProgram, FlowId, NodeId, NodeMeta, Origin, ResolvedActor,
    ResolvedActorMethod, ResolvedCallKind, ResolvedCallSite, ResolvedCapability,
    ResolvedConstValue, ResolvedConstant, ResolvedExternBlock, ResolvedExternFunc, ResolvedFlow,
    ResolvedFunction, ResolvedImpl, ResolvedItem, ResolvedItemKind, ResolvedMethodSig,
    ResolvedProtocol, ResolvedProtocolState, ResolvedProtocolTransition, ResolvedSession,
    ResolvedState, ResolvedTrait, ResolvedTypeDef, ResolvedTypeKind, ResolvedVariantMember,
    ResolvedVariantSchema, ResolvedVariantShape, SpanPrecision, StateId, TransitionId,
    TransitionTables, RESOLVED_IR_VERSION,
};

pub fn check(file: &File) -> Result<(), Vec<Diagnostic>> {
    check_program(file).map(|_| ())
}

pub fn check_strict(file: &File) -> Result<(), Vec<Diagnostic>> {
    check_program_strict(file).map(|_| ())
}

pub fn check_program(file: &File) -> Result<CheckedProgram, Vec<Diagnostic>> {
    let acc = checker::flow::flow_check_with_artifacts(file).map_err(|mut errors| {
        sort_diagnostics(&mut errors);
        errors
    })?;
    CheckedProgram::from_flow_acc(file, acc).map_err(|mut errors| {
        sort_diagnostics(&mut errors);
        errors
    })
}

pub fn check_program_strict(file: &File) -> Result<CheckedProgram, Vec<Diagnostic>> {
    let acc = checker::flow::flow_check_strict_with_artifacts(file).map_err(|mut errors| {
        sort_diagnostics(&mut errors);
        errors
    })?;
    CheckedProgram::from_flow_acc(file, acc).map_err(|mut errors| {
        sort_diagnostics(&mut errors);
        errors
    })
}

/// Stable diagnostic order for all `check_program` entry points.
///
/// §4-#43 (closed 0.36.77): the resolved IR pipeline uses `HashMap` catalogs
/// internally; without a final sort the same invalid program can surface
/// diagnostics in different orders across runs. Sort by source position
/// first, then message/code, to keep CLI and test output deterministic while
/// preserving conventional source-order reporting.
fn sort_diagnostics(errors: &mut Vec<Diagnostic>) {
    errors.sort_by(|a, b| {
        a.span
            .start_line
            .cmp(&b.span.start_line)
            .then(a.span.start_col.cmp(&b.span.start_col))
            .then(a.message.cmp(&b.message))
            .then(a.code.cmp(&b.code))
    });
}

/// Verify that MMS rule attachments are consistent.
/// 0.35.13 (DX backlog #10 trivia-ization): `rule:` statements are consumed
/// by the parser as trivia and never reach the AST, so there is nothing left
/// to verify. The entrypoint stays (CLI `--verify-rules` flag, test suite)
/// and always reports clean.
pub fn verify_rules(_file: &File) -> Vec<String> {
    Vec::new()
}
