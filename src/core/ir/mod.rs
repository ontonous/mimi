//! Canonical, checker-owned semantic IR.
//!
//! This module is intentionally independent from executable consumers.  The
//! checker constructs these artifacts once; interpreter, native codegen and
//! verifier consume stable identities rather than re-resolving surface AST.

mod body;
mod types;

pub use body::{
    AllocatorKind, BackendRequirement, BuiltinId, CheckedConversion, CheckedConversionKind,
    ContractKind, DelegateTarget, EffectId, MatchArm, MethodId, Permission, ResolvedArgument,
    ResolvedBinaryOp, ResolvedBlock, ResolvedBody, ResolvedBodyError, ResolvedCall, ResolvedCallee,
    ResolvedExpr, ResolvedExprKind, ResolvedIndex, ResolvedLiteral, ResolvedLocal, ResolvedLocalId,
    ResolvedParameterId, ResolvedPattern, ResolvedPatternKind, ResolvedPlace, ResolvedProjection,
    ResolvedRecordField, ResolvedScopeKind, ResolvedStmt, ResolvedStmtKind, ResolvedUnaryOp,
    SessionResidualId, SessionTransition,
};

pub use types::{
    FunctionTypeAbi, NominalTypeId, OwnershipTypeKind, PrimitiveType, ResolvedType,
    ResolvedTypeCapabilities, ResolvedTypeError, ResolvedTypeId, ResolvedTypeName,
    ResolvedTypeTable, TraitTypeKind, RESOLVED_TYPE_SCHEMA_VERSION,
};
