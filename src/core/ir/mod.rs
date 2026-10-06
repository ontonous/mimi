//! Canonical, checker-owned semantic IR.
//!
//! This module is intentionally independent from executable consumers.  The
//! checker constructs these artifacts once; interpreter, native codegen and
//! verifier consume stable identities rather than re-resolving surface AST.

mod actor;
mod body;
mod callable;
mod ffi_contract;
pub mod loop_proof;
pub mod lower;
mod native_declaration;
mod types;

pub(crate) use actor::build_checked_actor_descriptors;
pub use actor::{
    ResolvedActorDescriptor, ResolvedActorError, ResolvedActorFieldDescriptor,
    ResolvedActorFieldInitializer, ResolvedActorInitializerKind, ResolvedActorMethodDescriptor,
    ResolvedScalarActorReceipt, RESOLVED_ACTOR_SCHEMA,
};
pub use body::{
    AllocatorKind, BackendRequirement, BuiltinId, CheckedConversion, CheckedConversionKind,
    ContractKind, EffectId, MatchArm, MethodId, Permission, ResolvedArgument, ResolvedBinaryOp,
    ResolvedBlock, ResolvedBody, ResolvedBodyError, ResolvedCall, ResolvedCallee, ResolvedExpr,
    ResolvedExprKind, ResolvedFStringPart, ResolvedIndex, ResolvedLambda, ResolvedLiteral,
    ResolvedLocal, ResolvedLocalId, ResolvedParameterId, ResolvedPattern, ResolvedPatternKind,
    ResolvedPlace, ResolvedProjection, ResolvedRecordField, ResolvedScopeKind,
    ResolvedSessionAction, ResolvedStmt, ResolvedStmtKind, ResolvedUnaryOp,
    ResolvedValueProjection, SessionResidualId, SessionTransition,
};
pub use callable::{
    ResolvedCallable, ResolvedContract, ResolvedParameter, ResolvedSignature,
    ResolvedSignatureError, TypedCallEffectAuthority, TypedCallEffectReceipt,
    TypedCallExecutionDisposition, TypedCallGuard, TypedCallProofPoint, TypedDefaultProofRoot,
    TypedDefaultProofUse, TypedLoopInvariantPoint, TypedMathProofPoint, TypedProofBlock,
    TypedProofBoundary, TypedProofOperation, TypedProofOrderedEvent, TypedProofPointId,
    TypedProofPointWorklist, TypedProofScopeFrame,
};
pub use loop_proof::{
    derive_typed_loop_proof_receipt, type_path_fingerprint, typed_loop_expression_fingerprint,
    validate_loop_expression_site, validate_typed_loop_proof_receipt, TypedLoopBranchGuard,
    TypedLoopEdgeKind, TypedLoopEdgeSite, TypedLoopExpressionRole, TypedLoopExpressionSite,
    TypedLoopProofReceipt, TypedLoopResourceBlockState, TypedLoopResourceEdgeState,
    TypedLoopStateLocal, TypedLoopWriteSite, TYPED_LOOP_PROOF_SCHEMA_VERSION,
};
pub(crate) use native_declaration::build_checked_native_declarations;
pub use native_declaration::{
    NativeActorDeclaration, NativeConstructorRecipe, NativeDropRecipe, NativeExportDeclaration,
    NativeExportScalarKind, NativeFieldDeclaration, NativeFlowDeclaration,
    NativeFlowTransitionDeclaration, NativeGenericBinder, NativeGenericBound, NativeGenericKind,
    NativeImplDeclaration, NativeImplMethodDeclaration, NativeRepresentation,
    NativeStateDeclaration, NativeTraitDeclaration, NativeTraitSlotDeclaration,
    NativeTypeDeclaration, NativeTypeDeclarationKind, NativeVariantDeclaration,
    NativeVariantPayloadRecipe, NativeVariantRecipe, ResolvedNativeDeclarations,
    RESOLVED_NATIVE_DECLARATION_SCHEMA,
};

pub(crate) use types::primitive_type_id;
pub(crate) use types::result_type_id;
pub use types::{
    FunctionTypeAbi, NominalTypeId, OwnershipTypeKind, PrimitiveType, ResolvedType,
    ResolvedTypeCapabilities, ResolvedTypeError, ResolvedTypeId, ResolvedTypeName,
    ResolvedTypeTable, TraitTypeKind, RESOLVED_TYPE_SCHEMA_VERSION,
};

pub(crate) use ffi_contract::build_checked_ffi_declarations;
pub use ffi_contract::{
    ResolvedFfiContractRoot, ResolvedFfiDeclarationContract, ResolvedFfiDefaultPromotionKind,
    ResolvedFfiInputBinding, ResolvedFfiInvocationMetadata, ResolvedFfiParameter,
    ResolvedFfiParameterMode, ResolvedFfiPendingEffect, ResolvedFfiPromotionState,
    ResolvedFfiSignature, ResolvedFfiTypedCallContext, ResolvedFfiVariadicArgument,
    RESOLVED_FFI_DECLARATION_SCHEMA,
};
