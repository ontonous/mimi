use crate::ast::*;
use crate::diagnostic::Diagnostic;

pub(crate) use std::collections::HashMap;

pub mod builtins;
pub mod cfg;
mod checker;
pub mod effects;
pub(crate) mod helpers;
mod module_identity;
pub mod native_data_export;
pub mod phase;
pub mod type_folder;
pub mod unification;

mod check_stmt;
mod infer;
mod infer_expr;
pub mod ir;
pub mod mir;
pub(crate) mod ownership;
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
pub(crate) use ownership::{
    ordinary_map_owner_observer_effects, ManagedAnyCfgMatchCase, ManagedAnyCfgReceipt,
    MapAnyOwnerAction, MapAnyOwnerActionKind, MapAnyOwnerProfileVersion, MapRootAction,
    MapRootActionKind, OrdinaryMapOwnedAnyV2Action, OrdinaryMapOwnedAnyV2ActionKind,
    OrdinaryMapOwnedAnyV2Attempt, OrdinaryMapOwnedAnyV2AttemptKind,
    OrdinaryMapOwnedAnyV2BranchSelectorReceipt, OrdinaryMapOwnedAnyV2CfgBlock,
    OrdinaryMapOwnedAnyV2CfgFlowProof, OrdinaryMapOwnedAnyV2CfgFlowReceipt,
    OrdinaryMapOwnedAnyV2CfgReceipt, OrdinaryMapOwnedAnyV2DirectTupleGetUnpackPayload,
    OrdinaryMapOwnedAnyV2DirectTupleGetUnpackProfileVersion,
    OrdinaryMapOwnedAnyV2DirectTupleGetUnpackReceipt,
    OrdinaryMapOwnedAnyV2DynamicKeysGetProfileVersion, OrdinaryMapOwnedAnyV2DynamicKeysGetReceipt,
    OrdinaryMapOwnedAnyV2FoundObservationReceipt, OrdinaryMapOwnedAnyV2GetApi,
    OrdinaryMapOwnedAnyV2KeyProvenance, OrdinaryMapOwnedAnyV2KeysSnapshotOwnerReceipt,
    OrdinaryMapOwnedAnyV2KeysSnapshotProfileVersion, OrdinaryMapOwnedAnyV2KeysSnapshotReceipt,
    OrdinaryMapOwnedAnyV2LiveOwner, OrdinaryMapOwnedAnyV2MapRootGetAnyCallerProfileVersion,
    OrdinaryMapOwnedAnyV2MapRootGetAnyCallerReceipt,
    OrdinaryMapOwnedAnyV2MapRootProducerCallerReceipt,
    OrdinaryMapOwnedAnyV2MapRootProducerReturnReceipt, OrdinaryMapOwnedAnyV2ProfileVersion,
    OrdinaryMapOwnedAnyV2RemovedKeyCheckedMissFormulaReceipt,
    OrdinaryMapOwnedAnyV2RemovedKeyCheckedMissFormulaShape,
    OrdinaryMapOwnedAnyV2RemovedKeyCheckedMissReceipt, OrdinaryMapOwnedAnyV2ResultBranchReceipt,
    OrdinaryMapOwnedAnyV2ResultReceipt,
    OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageProfileVersion,
    OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageReceipt,
    OrdinaryMapOwnedAnyV2StaticKeyGetPayloadSource,
    OrdinaryMapOwnedAnyV2StaticKeyGetProfileVersion, OrdinaryMapOwnedAnyV2TemporaryRootReceipt,
    OrdinaryMapOwnedAnyV2UnpackReceipt,
    OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion,
    OrdinaryMapOwnedAnyV2ValuesIndexDisposition, OrdinaryMapOwnedAnyV2ValuesLocalIndexI32Receipt,
    OrdinaryMapOwnedAnyV2ValuesProducerCallerProfileVersion,
    OrdinaryMapOwnedAnyV2ValuesProducerCallerReceipt,
    OrdinaryMapOwnedAnyV2ValuesProducerPayloadSettlement,
    OrdinaryMapOwnedAnyV2ValuesProducerReturnProfileVersion,
    OrdinaryMapOwnedAnyV2ValuesProducerReturnReceipt,
    OrdinaryMapOwnedAnyV2ValuesProducerRootSettlement, OrdinaryMapOwnedAnyV2ValuesResultPathKind,
    OrdinaryMapOwnedAnyV2ValuesResultProducerCallerProfileVersion,
    OrdinaryMapOwnedAnyV2ValuesResultProducerProfileVersion,
    OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind, OrdinaryMapOwnedAnyV2ValuesUnpackReceipt,
    OrdinaryMapOwnedAnyV2ZeroMissI32ObservationReceipt, OrdinaryMapOwnedGetI32StringV1Action,
    OrdinaryMapOwnedGetI32StringV1ActionKind, OrdinaryMapOwnedGetI32StringV1Attempt,
    OrdinaryMapOwnedGetI32StringV1AttemptKind, OrdinaryMapOwnedGetI32StringV1CfgBlock,
    OrdinaryMapOwnedGetI32StringV1CfgProfileVersion, OrdinaryMapOwnedGetI32StringV1CfgReceipt,
    OrdinaryMapOwnedGetI32StringV1ProfileVersion,
    OrdinaryMapOwnedGetI32StringV1ResultBranchReceipt, OrdinaryMapOwnedGetResultBranchPathReceipt,
    OrdinaryMapOwnedGetResultReceipt, OrdinaryMapOwnedGetResultSettlementKind,
    OrdinaryMapOwnedGetStringMatchReceipt, OrdinaryMapOwnerLifecycleAction,
    OrdinaryMapOwnerLifecycleActionKind, OrdinaryMapOwnerLifecycleAttempt,
    OrdinaryMapOwnerLifecycleAttemptKind, OrdinaryMapOwnerLifecycleProfileVersion,
    OrdinaryMapOwnerObserverAttempt, OrdinaryMapOwnerObserverProfileVersion,
    OrdinaryMapOwnerObserverReceipt,
};
pub use ownership::{
    Availability, BranchMerge, CanonicalActionKind, CanonicalResourceAction, CfgLocation,
    IndexProjection, Loan, LoanId, LoanKind, LocalId, Place, PlaceProjection, ResourceAnalysis,
    ResourceFact, ResourceId,
};
pub use ownership::{
    OrdinaryMapOwnedAnyV2ResultSettlementKind, OrdinaryMapOwnedAnyV2RootSettlementKind,
};
pub(crate) use resolved::{
    BackendProfile, CheckedProgram, FlowId, NodeId, NodeMeta, Origin, ResolvedActor,
    ResolvedCallKind, ResolvedCallSite, ResolvedConstValue, ResolvedConstant, ResolvedExternBlock,
    ResolvedExternFunc, ResolvedFlow, ResolvedFunction, ResolvedImpl, ResolvedItemKind,
    ResolvedTrait, ResolvedTypeDef, ResolvedTypeKind, ResolvedVariantSchema, ResolvedVariantShape,
    SpanPrecision, StateId, TransitionId, TransitionTables, RESOLVED_IR_VERSION,
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
fn sort_diagnostics(errors: &mut [Diagnostic]) {
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
