//! Shared Canonical MIR admission and materialization boundary.
//!
//! This module owns the small amount of route state that must be identical at
//! the CLI selector, direct native entry, and public verifier boundary. It
//! deliberately stops before backend capability checks: bytecode, native, and
//! verifier validators remain independent consumers of the same returned
//! `MirProgram`.
//!
//! A `CompleteCoverage` admission is a hard boundary. If its canonical
//! producer fails or does not materialize the admitted operation, callers must
//! report the structured failure and may not re-enter a legacy consumer.
//! `MixedCoverage` and `OutsideProfile` remain explicit compatibility states.

use std::collections::{BTreeSet, HashSet};

use crate::core::mir::reference::MirProgram;
use crate::core::{
    CheckedProgram, OrdinaryMapOwnedGetI32StringV1Action, OrdinaryMapOwnedGetI32StringV1ActionKind,
    OrdinaryMapOwnedGetI32StringV1Attempt, OrdinaryMapOwnedGetI32StringV1AttemptKind,
    OrdinaryMapOwnedGetI32StringV1ProfileVersion, OrdinaryMapOwnerLifecycleActionKind,
    OrdinaryMapOwnerLifecycleAttemptKind, OrdinaryMapOwnerLifecycleProfileVersion,
};

use super::{
    classify_copy_option_i32_variant_admission, classify_copy_option_variant_admission,
    classify_copy_result_i32_variant_admission, classify_flat_copy_record_admission,
    classify_generic_option_projection_admission,
    classify_generic_option_projection_fallback_admission,
    classify_generic_result_projection_admission,
    classify_generic_result_projection_fallback_admission,
    classify_generic_variant_predicate_admission, classify_managed_result_call_admission,
    classify_option_nested_tuple_variant_admission, classify_option_string_variant_admission,
    classify_scalar_collection_admission, classify_scalar_generic_identity_admission,
    classify_scalar_generic_identity_i64_admission, classify_session_channel_admission,
    contains_copy_option_i32_variant_candidate, contains_copy_option_variant_candidate,
    contains_copy_result_i32_variant_candidate, contains_flat_copy_record_candidate,
    contains_flow_failure_retry_candidate, contains_generic_option_projection_candidate,
    contains_generic_option_projection_fallback_candidate,
    contains_generic_result_projection_candidate,
    contains_generic_result_projection_fallback_candidate,
    contains_generic_variant_predicate_candidate, contains_managed_result_call_candidate,
    contains_option_nested_tuple_variant_candidate, contains_option_string_variant_candidate,
    contains_s8_flow_transition_candidate, contains_scalar_collection_candidate,
    contains_scalar_collection_operation_candidate, contains_scalar_generic_identity_candidate,
    contains_session_channel_candidate, is_closed_silent_local_flow_candidate,
    is_exact_s8_flow_transition, is_flow_failure_retry_candidate, is_s8_flow_transition_candidate,
    scalar_ffi_boundary_reason, CopyOptionI32VariantAdmission, CopyResultI32VariantAdmission,
    FlatCopyRecordAdmission, GenericOptionProjectionAdmission,
    GenericOptionProjectionFallbackAdmission, GenericResultProjectionAdmission,
    GenericResultProjectionFallbackAdmission, GenericVariantPredicateAdmission,
    ManagedResultCallAdmission, OptionNestedTupleVariantAdmission, OptionStringVariantAdmission,
    ScalarCollectionAdmission, ScalarGenericIdentityAdmission, SessionChannelAdmission,
};

#[cfg(test)]
thread_local! {
    static TEST_ROUTE_MATERIALIZATION_COUNT: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_test_route_materialization_count() {
    TEST_ROUTE_MATERIALIZATION_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn test_route_materialization_count() -> usize {
    TEST_ROUTE_MATERIALIZATION_COUNT.with(std::cell::Cell::get)
}

/// The already-admitted production island whose materialization failed or
/// lacked its canonical operation receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanonicalMirRouteProfile {
    MapRoot,
    MapAnyOwnerV1,
    OrdinaryMapOwnerLifecycleI32StringV1,
    OrdinaryMapOwnedGetI32StringV1,
    ScalarExport,
    ScalarFfi,
    ScalarNested,
    ScalarActor,
    ScalarGenericIdentityI32,
    ScalarGenericIdentityI64,
    ScalarCollection,
    FlatCopyRecord,
    S8FlowTransition,
    FlowFailureRetry,
    NonCopyOptionStringVariant,
    NonCopyOptionNestedTupleVariant,
    GenericOptionPredicate,
    GenericOptionProjection,
    GenericOptionProjectionFallback,
    GenericResultProjection,
    GenericResultProjectionFallback,
    ManagedResultCall,
    CopyOptionI32Variant,
    CopyOptionBoolVariant,
    CopyOptionI64Variant,
    CopyOptionF64Variant,
    CopyResultI32Variant,
    SessionChannel,
}

impl CanonicalMirRouteProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MapRoot => MAP_ROOT_ISLAND,
            Self::MapAnyOwnerV1 => MAP_ANY_OWNER_V1_ISLAND,
            Self::OrdinaryMapOwnerLifecycleI32StringV1 => {
                ORDINARY_MAP_OWNER_LIFECYCLE_I32_STRING_V1_ISLAND
            }
            Self::OrdinaryMapOwnedGetI32StringV1 => ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND,
            Self::ScalarExport => SCALAR_EXPORT_ISLAND,
            Self::ScalarFfi => "scalar-ffi-v1",
            Self::ScalarNested => SCALAR_NESTED_ISLAND,
            Self::ScalarActor => SCALAR_ACTOR_ISLAND,
            Self::ScalarGenericIdentityI32 => super::SCALAR_GENERIC_IDENTITY_I32_ISLAND,
            Self::ScalarGenericIdentityI64 => super::SCALAR_GENERIC_IDENTITY_I64_ISLAND,
            Self::ScalarCollection => super::SCALAR_COLLECTION_ISLAND,
            Self::FlatCopyRecord => "flat-copy-record-v1",
            Self::S8FlowTransition => "s8-silent-local-flow-v1",
            Self::FlowFailureRetry => "recoverable-flow-result-v2",
            Self::NonCopyOptionStringVariant => super::NON_COPY_OPTION_STRING_VARIANT_ISLAND,
            Self::NonCopyOptionNestedTupleVariant => {
                super::NON_COPY_OPTION_NESTED_TUPLE_VARIANT_ISLAND
            }
            Self::GenericOptionPredicate => super::GENERIC_VARIANT_PREDICATE_ISLAND,
            Self::GenericOptionProjection => super::GENERIC_OPTION_PROJECTION_ISLAND,
            Self::GenericOptionProjectionFallback => {
                super::GENERIC_OPTION_PROJECTION_FALLBACK_ISLAND
            }
            Self::GenericResultProjection => super::GENERIC_RESULT_PROJECTION_ISLAND,
            Self::GenericResultProjectionFallback => {
                super::GENERIC_RESULT_PROJECTION_FALLBACK_ISLAND
            }
            Self::ManagedResultCall => super::MANAGED_RESULT_CALL_ISLAND,
            Self::CopyOptionI32Variant => super::COPY_OPTION_I32_VARIANT_ISLAND,
            Self::CopyOptionBoolVariant => super::COPY_OPTION_BOOL_VARIANT_ISLAND,
            Self::CopyOptionI64Variant => super::COPY_OPTION_I64_VARIANT_ISLAND,
            Self::CopyOptionF64Variant => super::COPY_OPTION_F64_VARIANT_ISLAND,
            Self::CopyResultI32Variant => super::COPY_RESULT_I32_VARIANT_ISLAND,
            Self::SessionChannel => super::SESSION_CHANNEL_ISLAND,
        }
    }

    /// Return whether checker-owned admission has completed this profile.
    ///
    /// This is deliberately kept next to the profile names and materialized
    /// receipts so verifier and backend route owners cannot grow independent
    /// `is_exact -> construct -> contains_*` tables.
    pub const fn is_admitted(self, admission: CanonicalMirRouteAdmission) -> bool {
        match self {
            Self::MapRoot => admission.map_root_complete(),
            Self::MapAnyOwnerV1 => admission.map_any_owner_complete(),
            Self::OrdinaryMapOwnerLifecycleI32StringV1 => {
                admission.ordinary_map_owner_lifecycle_i32_string_v1_complete()
            }
            Self::OrdinaryMapOwnedGetI32StringV1 => {
                admission.ordinary_map_owned_get_i32_string_v1_complete()
            }
            Self::ScalarExport => admission.scalar_export,
            Self::ScalarFfi => admission.scalar_ffi,
            Self::ScalarNested => admission.scalar_nested,
            Self::ScalarActor => admission.scalar_actor,
            Self::ScalarGenericIdentityI32 => admission.scalar_generic_identity_i32_complete(),
            Self::ScalarGenericIdentityI64 => admission.scalar_generic_identity_i64_complete(),
            Self::ScalarCollection => admission.collection_complete(),
            Self::FlatCopyRecord => admission.record_complete(),
            Self::S8FlowTransition => admission.flow_complete(),
            Self::FlowFailureRetry => admission.flow_failure_retry,
            Self::NonCopyOptionStringVariant => admission.option_string_complete(),
            Self::NonCopyOptionNestedTupleVariant => admission.option_nested_tuple_complete(),
            Self::GenericOptionPredicate => admission.generic_variant_complete(),
            Self::GenericOptionProjection => admission.generic_option_projection_complete(),
            Self::GenericOptionProjectionFallback => {
                admission.generic_option_projection_fallback_complete()
            }
            Self::GenericResultProjection => admission.generic_result_projection_complete(),
            Self::GenericResultProjectionFallback => {
                admission.generic_result_projection_fallback_complete()
            }
            Self::ManagedResultCall => admission.managed_result_call_complete(),
            Self::CopyOptionI32Variant => admission.copy_option_i32_complete(),
            Self::CopyOptionBoolVariant => admission.copy_option_bool_complete(),
            Self::CopyOptionI64Variant => admission.copy_option_i64_complete(),
            Self::CopyOptionF64Variant => admission.copy_option_f64_complete(),
            Self::CopyResultI32Variant => admission.copy_result_i32_complete(),
            Self::SessionChannel => admission.session_complete(),
        }
    }

    /// Return whether the canonical route contains this profile's operation
    /// materialization receipt.
    pub const fn is_materialized(self, route: &CanonicalMirRouteMaterialization) -> bool {
        match self {
            Self::MapRoot => route.materialized_map_root_candidate,
            Self::MapAnyOwnerV1 => route.materialized_map_any_owner_candidate,
            Self::OrdinaryMapOwnerLifecycleI32StringV1 => {
                route.materialized_ordinary_map_owner_lifecycle_i32_string_v1_candidate
            }
            Self::OrdinaryMapOwnedGetI32StringV1 => {
                route.materialized_ordinary_map_owned_get_i32_string_v1_candidate
            }
            Self::ScalarExport => route.materialized_scalar_export_candidate,
            Self::ScalarFfi => route.materialized_scalar_ffi_candidate,
            Self::ScalarNested => route.materialized_scalar_nested_candidate,
            Self::ScalarActor => route.materialized_scalar_actor_candidate,
            Self::ScalarGenericIdentityI32 => {
                route.materialized_scalar_generic_identity_i32_candidate
            }
            Self::ScalarGenericIdentityI64 => {
                route.materialized_scalar_generic_identity_i64_candidate
            }
            Self::ScalarCollection => route.materialized_collection_candidate,
            Self::FlatCopyRecord => route.materialized_record_candidate,
            Self::S8FlowTransition => route.materialized_flow_candidate,
            Self::FlowFailureRetry => route.materialized_flow_failure_retry_candidate,
            Self::NonCopyOptionStringVariant => route.materialized_option_string_candidate,
            Self::NonCopyOptionNestedTupleVariant => {
                route.materialized_option_nested_tuple_candidate
            }
            Self::GenericOptionPredicate => route.materialized_generic_variant_candidate,
            Self::GenericOptionProjection => route.materialized_generic_option_projection_candidate,
            Self::GenericOptionProjectionFallback => {
                route.materialized_generic_option_projection_fallback_candidate
            }
            Self::GenericResultProjection => route.materialized_generic_result_projection_candidate,
            Self::GenericResultProjectionFallback => {
                route.materialized_generic_result_projection_fallback_candidate
            }
            Self::ManagedResultCall => route.materialized_managed_result_call_candidate,
            Self::CopyOptionI32Variant => route.materialized_copy_option_i32_candidate,
            Self::CopyOptionBoolVariant => route.materialized_copy_option_bool_candidate,
            Self::CopyOptionI64Variant => route.materialized_copy_option_i64_candidate,
            Self::CopyOptionF64Variant => route.materialized_copy_option_f64_candidate,
            Self::CopyResultI32Variant => route.materialized_copy_result_i32_candidate,
            Self::SessionChannel => route.materialized_session_candidate,
        }
    }
}

/// Checker-owned admission for the opaque, local Map-root lifecycle.
///
/// `IncompleteCoverage` means the Checker materialized at least one lifecycle
/// action but the full program's `map_new()` call set does not match the
/// receipted New actions. That is a partial route candidate, so consumers must
/// fail closed instead of mixing canonical and legacy MapRoot representations.
/// Ordinary Map/Any programs with no MapRoot lifecycle receipts remain outside
/// this profile and keep their existing compatibility route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapRootAdmission {
    OutsideProfile,
    IncompleteCoverage,
    CompleteCoverage,
}

/// Finite scalar Actor graph with typed state, mailbox and lexical capture receipts.
pub const SCALAR_ACTOR_ISLAND: &str = "scalar-actor-domain-v1";

pub const SCALAR_EXPORT_ISLAND: &str = "scalar-export-v1";

pub const SCALAR_NESTED_ISLAND: &str = "scalar-nested-callable-v1";

pub const MAP_ROOT_ISLAND: &str = "map-root-v1";

impl MapRootAdmission {
    pub const fn is_candidate(self) -> bool {
        !matches!(self, Self::OutsideProfile)
    }

    pub const fn is_complete(self) -> bool {
        matches!(self, Self::CompleteCoverage)
    }
}

/// Checker-owned admission for the separately versioned MapAnyOwner v1
/// lifecycle. Its coverage is calculated from the independent MapAnyOwner
/// ledger; legacy MapRoot receipts can only make a missing v1 ledger fail
/// closed and can never admit this profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapAnyOwnerAdmission {
    OutsideProfile,
    IncompleteCoverage,
    CompleteCoverage,
}

pub const MAP_ANY_OWNER_V1_ISLAND: &str = "map-any-owner-v1";

impl MapAnyOwnerAdmission {
    pub const fn is_candidate(self) -> bool {
        !matches!(self, Self::OutsideProfile)
    }

    pub const fn is_complete(self) -> bool {
        matches!(self, Self::CompleteCoverage)
    }
}

/// Checker-owned admission for the independently versioned ordinary Map
/// owner lifecycle. An action receipt is required before this profile can be
/// a candidate: constructor or unsupported-operation attempts alone do not
/// authorize the ordinary Map TypeDesc or route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrdinaryMapOwnerLifecycleI32StringV1Admission {
    OutsideProfile,
    IncompleteCoverage,
    CompleteCoverage,
    /// The ordinary source ledger is complete, but every attempt is already
    /// represented by the selected, older MapAnyOwner v1 identity.
    CoveredByMapAnyOwnerV1,
}

pub const ORDINARY_MAP_OWNER_LIFECYCLE_I32_STRING_V1_ISLAND: &str =
    "ordinary-map-owner-lifecycle-i32-string-v1";

impl OrdinaryMapOwnerLifecycleI32StringV1Admission {
    /// Only a complete, independently selected action ledger authorizes this
    /// profile. A complete ledger already covered by MapAnyOwner remains a
    /// source signal but does not authorize a second TypeDesc identity.
    pub const fn is_candidate(self) -> bool {
        self.is_complete()
    }

    pub const fn has_profile_signal(self) -> bool {
        !matches!(self, Self::OutsideProfile)
    }

    pub const fn is_complete(self) -> bool {
        matches!(self, Self::CompleteCoverage)
    }
}

/// Checker-owned admission for the independent owned-result typed-Get Map
/// profile. Its receipt includes the complete ordinary owner lifecycle plus
/// typed getters, but the profile identity and coverage ledger are wholly
/// separate from the older ordinary lifecycle, MapRoot and MapAnyOwner v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrdinaryMapOwnedGetI32StringV1Admission {
    OutsideProfile,
    IncompleteCoverage,
    CompleteCoverage,
}

pub const ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND: &str =
    "ordinary-map-owned-get-i32-string-v1";

impl OrdinaryMapOwnedGetI32StringV1Admission {
    /// A complete independent typed-Get ledger is the only authority for this
    /// identity. Incomplete coverage remains visible for compatibility and
    /// direct-route failure decisions but is never a candidate.
    pub const fn is_candidate(self) -> bool {
        self.is_complete()
    }

    pub const fn has_profile_signal(self) -> bool {
        !matches!(self, Self::OutsideProfile)
    }

    pub const fn is_complete(self) -> bool {
        matches!(self, Self::CompleteCoverage)
    }
}

/// Checker-owned admission state for the already implemented S8 silent-local
/// Flow island.  A candidate is still an explicit fail-closed signal for the
/// default/direct route; only the exact body is a complete production island.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S8FlowAdmission {
    OutsideProfile,
    MixedCoverage,
    CompleteCoverage,
}

/// Stage at which an already complete admission failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanonicalMirRouteFailureStage {
    Construction,
    Coverage,
}

/// A materialization failure which preserves the distinction between an
/// admitted production island and an unrelated legacy compatibility graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalMirRouteMaterializationError {
    /// The checked program is not in a complete production envelope.  The
    /// attached admission lets the caller distinguish an explicit
    /// compatibility input from a recognized candidate that must be rejected;
    /// neither case may claim a canonical route from this error.
    Compatibility {
        admission: CanonicalMirRouteAdmission,
        message: String,
    },
    /// A checker admission crossed the canonical boundary. This is hard and
    /// must never be converted into a legacy compile or execution.
    Complete {
        profile: CanonicalMirRouteProfile,
        stage: CanonicalMirRouteFailureStage,
        message: String,
    },
}

impl std::fmt::Display for CanonicalMirRouteMaterializationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Compatibility { message, .. } => {
                write!(
                    formatter,
                    "canonical MIR compatibility materialization failed: {message}"
                )
            }
            Self::Complete {
                profile,
                stage,
                message,
            } => write!(
                formatter,
                "canonical MIR {} {:?} failed: {message}",
                profile.as_str(),
                stage
            ),
        }
    }
}

impl CanonicalMirRouteMaterializationError {
    /// Return the stable diagnostic for a hard canonical route failure.
    /// Compatibility materialization remains intentionally unclassified: it
    /// is the explicit signal that the caller may keep its legacy route.
    pub fn diagnostic_code(&self) -> Option<&'static str> {
        match self {
            Self::Compatibility { .. } => None,
            Self::Complete { stage, .. } => Some(match stage {
                CanonicalMirRouteFailureStage::Construction => {
                    super::MIR_ROUTE_MATERIALIZATION_ERROR_CODE
                }
                CanonicalMirRouteFailureStage::Coverage => super::MIR_ROUTE_COVERAGE_ERROR_CODE,
            }),
        }
    }

    /// Convert the route failure to the shared diagnostic shape used by CLI,
    /// LSP, and embedded callers.  Hard failures carry their stage code and
    /// runtime-system provenance; compatibility remains an ordinary untyped
    /// error because callers are allowed to continue on the legacy route.
    pub fn to_diagnostic(&self) -> crate::diagnostic::Diagnostic {
        let message = match self.diagnostic_code() {
            Some(code) => format!("{code}: {self}"),
            None => self.to_string(),
        };
        crate::diagnostic::mir_route_error_diagnostic(message, crate::span::Span::UNKNOWN)
    }
}

/// Checker-owned route admission captured alongside one canonical graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalMirRouteAdmission {
    pub map_root: MapRootAdmission,
    pub map_any_owner: MapAnyOwnerAdmission,
    pub ordinary_map_owner_lifecycle_i32_string_v1: OrdinaryMapOwnerLifecycleI32StringV1Admission,
    pub ordinary_map_owned_get_i32_string_v1: OrdinaryMapOwnedGetI32StringV1Admission,
    pub scalar_export: bool,
    pub scalar_ffi: bool,
    pub scalar_nested: bool,
    pub scalar_actor: bool,
    pub scalar_generic_identity_i32: ScalarGenericIdentityAdmission,
    pub scalar_generic_identity_i64: ScalarGenericIdentityAdmission,
    pub collection: ScalarCollectionAdmission,
    pub record: FlatCopyRecordAdmission,
    pub flow: S8FlowAdmission,
    pub flow_failure_retry: bool,
    pub option_string: OptionStringVariantAdmission,
    pub option_nested_tuple: OptionNestedTupleVariantAdmission,
    pub generic_variant: GenericVariantPredicateAdmission,
    pub generic_option_projection: GenericOptionProjectionAdmission,
    pub generic_option_projection_fallback: GenericOptionProjectionFallbackAdmission,
    pub generic_result_projection: GenericResultProjectionAdmission,
    pub generic_result_projection_fallback: GenericResultProjectionFallbackAdmission,
    pub managed_result_call: ManagedResultCallAdmission,
    pub copy_option_i32: CopyOptionI32VariantAdmission,
    pub copy_option_bool: CopyOptionI32VariantAdmission,
    pub copy_option_i64: CopyOptionI32VariantAdmission,
    pub copy_option_f64: CopyOptionI32VariantAdmission,
    pub copy_result_i32: CopyResultI32VariantAdmission,
    pub session: SessionChannelAdmission,
}

impl CanonicalMirRouteAdmission {
    pub const fn has_candidate(self) -> bool {
        self.map_root.is_candidate()
            || self.map_any_owner.is_candidate()
            || self
                .ordinary_map_owner_lifecycle_i32_string_v1
                .is_candidate()
            || self.ordinary_map_owned_get_i32_string_v1.is_candidate()
            || self.scalar_export
            || self.scalar_ffi
            || self.scalar_nested
            || self.scalar_actor
            || self.scalar_generic_identity_i32_complete()
            || self.scalar_generic_identity_i64_complete()
            || !matches!(self.collection, ScalarCollectionAdmission::OutsideProfile)
            || !matches!(self.record, FlatCopyRecordAdmission::OutsideProfile)
            || !matches!(self.flow, S8FlowAdmission::OutsideProfile)
            || self.flow_failure_retry
            || !matches!(
                self.option_string,
                OptionStringVariantAdmission::OutsideProfile
            )
            || !matches!(
                self.option_nested_tuple,
                OptionNestedTupleVariantAdmission::OutsideProfile
            )
            || !matches!(
                self.generic_variant,
                GenericVariantPredicateAdmission::OutsideProfile
            )
            || !matches!(
                self.generic_option_projection,
                GenericOptionProjectionAdmission::OutsideProfile
            )
            || !matches!(
                self.generic_option_projection_fallback,
                GenericOptionProjectionFallbackAdmission::OutsideProfile
            )
            || !matches!(
                self.generic_result_projection,
                GenericResultProjectionAdmission::OutsideProfile
            )
            || !matches!(
                self.generic_result_projection_fallback,
                GenericResultProjectionFallbackAdmission::OutsideProfile
            )
            || !matches!(
                self.managed_result_call,
                ManagedResultCallAdmission::OutsideProfile
            )
            || !matches!(
                self.copy_option_i32,
                CopyOptionI32VariantAdmission::OutsideProfile
            )
            || !matches!(
                self.copy_option_bool,
                CopyOptionI32VariantAdmission::OutsideProfile
            )
            || !matches!(
                self.copy_option_i64,
                CopyOptionI32VariantAdmission::OutsideProfile
            )
            || !matches!(
                self.copy_option_f64,
                CopyOptionI32VariantAdmission::OutsideProfile
            )
            || !matches!(
                self.copy_result_i32,
                CopyResultI32VariantAdmission::OutsideProfile
            )
            || !matches!(self.session, SessionChannelAdmission::OutsideProfile)
    }

    pub const fn collection_complete(self) -> bool {
        matches!(self.collection, ScalarCollectionAdmission::CompleteCoverage)
    }

    pub const fn map_root_complete(self) -> bool {
        self.map_root.is_complete()
    }

    pub const fn map_any_owner_complete(self) -> bool {
        self.map_any_owner.is_complete()
    }

    pub const fn ordinary_map_owner_lifecycle_i32_string_v1_complete(self) -> bool {
        self.ordinary_map_owner_lifecycle_i32_string_v1
            .is_complete()
    }

    pub const fn ordinary_map_owned_get_i32_string_v1_complete(self) -> bool {
        self.ordinary_map_owned_get_i32_string_v1.is_complete()
    }

    pub const fn scalar_generic_identity_i32_complete(self) -> bool {
        matches!(
            self.scalar_generic_identity_i32,
            ScalarGenericIdentityAdmission::CompleteCoverage
        )
    }

    pub const fn scalar_generic_identity_i64_complete(self) -> bool {
        matches!(
            self.scalar_generic_identity_i64,
            ScalarGenericIdentityAdmission::CompleteCoverage
        )
    }

    pub const fn record_complete(self) -> bool {
        matches!(self.record, FlatCopyRecordAdmission::CompleteCoverage)
    }

    pub const fn flow_complete(self) -> bool {
        matches!(self.flow, S8FlowAdmission::CompleteCoverage)
    }

    pub const fn option_string_complete(self) -> bool {
        matches!(
            self.option_string,
            OptionStringVariantAdmission::CompleteCoverage
        )
    }

    pub const fn option_nested_tuple_complete(self) -> bool {
        matches!(
            self.option_nested_tuple,
            OptionNestedTupleVariantAdmission::CompleteCoverage
        )
    }

    pub const fn generic_variant_complete(self) -> bool {
        matches!(
            self.generic_variant,
            GenericVariantPredicateAdmission::CompleteCoverage
        )
    }

    pub const fn generic_option_projection_complete(self) -> bool {
        matches!(
            self.generic_option_projection,
            GenericOptionProjectionAdmission::CompleteCoverage
        )
    }

    pub const fn generic_result_projection_complete(self) -> bool {
        matches!(
            self.generic_result_projection,
            GenericResultProjectionAdmission::CompleteCoverage
        )
    }

    pub const fn generic_result_projection_fallback_complete(self) -> bool {
        matches!(
            self.generic_result_projection_fallback,
            GenericResultProjectionFallbackAdmission::CompleteCoverage
        )
    }

    pub const fn managed_result_call_complete(self) -> bool {
        matches!(
            self.managed_result_call,
            ManagedResultCallAdmission::CompleteCoverage
        )
    }

    pub const fn generic_option_projection_fallback_complete(self) -> bool {
        matches!(
            self.generic_option_projection_fallback,
            GenericOptionProjectionFallbackAdmission::CompleteCoverage
        )
    }

    pub const fn copy_option_i32_complete(self) -> bool {
        matches!(
            self.copy_option_i32,
            CopyOptionI32VariantAdmission::CompleteCoverage
        )
    }

    pub const fn copy_option_bool_complete(self) -> bool {
        matches!(
            self.copy_option_bool,
            CopyOptionI32VariantAdmission::CompleteCoverage
        )
    }

    pub const fn copy_option_i64_complete(self) -> bool {
        matches!(
            self.copy_option_i64,
            CopyOptionI32VariantAdmission::CompleteCoverage
        )
    }

    pub const fn copy_option_f64_complete(self) -> bool {
        matches!(
            self.copy_option_f64,
            CopyOptionI32VariantAdmission::CompleteCoverage
        )
    }

    pub const fn copy_result_i32_complete(self) -> bool {
        matches!(
            self.copy_result_i32,
            CopyResultI32VariantAdmission::CompleteCoverage
        )
    }

    pub const fn session_complete(self) -> bool {
        matches!(self.session, SessionChannelAdmission::CompleteCoverage)
    }
}

/// One immutable canonical graph plus the receipts needed by route owners.
///
/// The booleans are materialization receipts, not backend capability claims.
/// Every consumer still validates the graph immediately before use.
#[derive(Debug, Clone)]
pub struct CanonicalMirRouteMaterialization {
    pub program: MirProgram,
    pub admission: CanonicalMirRouteAdmission,
    pub materialized_map_root_candidate: bool,
    pub materialized_map_any_owner_candidate: bool,
    pub materialized_ordinary_map_owner_lifecycle_i32_string_v1_candidate: bool,
    pub materialized_ordinary_map_owned_get_i32_string_v1_candidate: bool,
    pub materialized_scalar_export_candidate: bool,
    pub materialized_scalar_ffi_candidate: bool,
    pub materialized_scalar_nested_candidate: bool,
    pub materialized_scalar_actor_candidate: bool,
    pub materialized_scalar_generic_identity_i32_candidate: bool,
    pub materialized_scalar_generic_identity_i64_candidate: bool,
    pub materialized_collection_candidate: bool,
    /// R6-1052: whether the receipt came from a migrated collection
    /// operation (List/Set op face) rather than the plain-scalar stdout
    /// candidacy.  Only the operation face carries the cannot-re-enter-legacy
    /// tripwire; an stdout-only candidate that fails the island preflight is
    /// an explicit compatibility input.
    pub materialized_collection_operation_candidate: bool,
    pub materialized_record_candidate: bool,
    pub materialized_flow_candidate: bool,
    pub materialized_flow_failure_retry_candidate: bool,
    pub materialized_option_string_candidate: bool,
    pub materialized_option_nested_tuple_candidate: bool,
    pub materialized_generic_variant_candidate: bool,
    pub materialized_generic_option_projection_candidate: bool,
    pub materialized_generic_option_projection_fallback_candidate: bool,
    pub materialized_generic_result_projection_candidate: bool,
    pub materialized_generic_result_projection_fallback_candidate: bool,
    pub materialized_managed_result_call_candidate: bool,
    pub materialized_copy_option_i32_candidate: bool,
    pub materialized_copy_option_bool_candidate: bool,
    pub materialized_copy_option_i64_candidate: bool,
    pub materialized_copy_option_f64_candidate: bool,
    pub materialized_copy_result_i32_candidate: bool,
    pub materialized_session_candidate: bool,
}

/// The map-specific profile slice needed while constructing synthetic type
/// descriptors. This deliberately excludes whole-route admission: Actor
/// admission itself materializes a type catalog, so calling the full selector
/// from the catalog would recursively re-enter Actor materialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MapOwnerProfileAdmission {
    pub(super) map_root: MapRootAdmission,
    pub(super) map_any_owner: MapAnyOwnerAdmission,
    pub(super) ordinary_map_owner_lifecycle_i32_string_v1:
        OrdinaryMapOwnerLifecycleI32StringV1Admission,
    pub(super) ordinary_map_owned_get_i32_string_v1: OrdinaryMapOwnedGetI32StringV1Admission,
}

impl MapOwnerProfileAdmission {
    pub(super) const fn map_any_owner_complete(self) -> bool {
        self.map_any_owner.is_complete()
    }

    pub(super) const fn ordinary_map_owner_lifecycle_i32_string_v1_complete(self) -> bool {
        self.ordinary_map_owner_lifecycle_i32_string_v1
            .is_complete()
    }
}

/// Classify only the checker-owned Map profile facts consumed by TypeDesc
/// construction. Keep this independent from Actor and every backend route
/// predicate so type materialization remains a terminal dependency.
pub(super) fn classify_map_owner_profile_admission(
    program: &CheckedProgram,
) -> MapOwnerProfileAdmission {
    let closure_source_plan =
        super::closure_callable::CheckedClosureCallablePlanV1::derive_source(program, None).ok();
    classify_map_owner_profile_admission_with_closure_source(program, closure_source_plan.as_ref())
}

/// TypeCatalog construction derives the source-only closure plan before
/// materializing its descriptors. Accept that exact plan here so Map route
/// admission and TypeDesc evidence consume the same callable-local receipts
/// without making TypeCatalog a dependency of its own classifier.
pub(super) fn classify_map_owner_profile_admission_with_closure_source(
    program: &CheckedProgram,
    closure_source_plan: Option<&super::closure_callable::CheckedClosureCallablePlanV1>,
) -> MapOwnerProfileAdmission {
    let ordinary_map_owned_get_i32_string_v1 =
        classify_ordinary_map_owned_get_i32_string_v1_admission(program, closure_source_plan);
    let ordinary_map_owner_lifecycle_i32_string_v1 =
        classify_ordinary_map_owner_lifecycle_i32_string_v1_admission(program);
    let legacy_map_root = classify_map_root_admission(program);
    let mut map_any_owner = classify_map_any_owner_admission(program, legacy_map_root);
    // A complete persistent ordinary-Map ledger with a borrowed dynamic
    // String key is a separate applicable owner profile. Older MapRoot
    // recognition can still make the compatibility-derived MapAny status
    // look incomplete when no MapAny action exists; that legacy observation
    // must not suppress the exact ordinary profile. Do not apply this rule
    // when any MapAny action or checked closure candidate exists: those are
    // independent source signals and remain hard boundaries unless receipted.
    if map_any_owner == MapAnyOwnerAdmission::IncompleteCoverage
        && ordinary_map_owner_lifecycle_i32_string_v1.is_complete()
        && !program
            .resource_analyses()
            .values()
            .any(|analysis| !analysis.map_any_owner_actions.is_empty())
        && !super::closure_callable::CheckedClosureCallablePlanV1::has_source_candidate(program)
        && ordinary_map_owner_has_direct_string_key_local(program)
    {
        map_any_owner = MapAnyOwnerAdmission::OutsideProfile;
    }
    // Each admitted owner profile owns its route decision. The ordinary Map
    // owner has a separate persistent-root contract and identity, so its
    // checker receipt must never fall through to the overlapping MapRoot or
    // MapAnyOwner representations.
    let ordinary_attempts_fully_covered_by_map_any_owner_v1 =
        ordinary_map_attempts_fully_covered_by_map_any_owner_v1(program);
    let (map_root, map_any_owner, ordinary_map_owner_lifecycle_i32_string_v1) =
        if ordinary_map_owned_get_i32_string_v1.has_profile_signal() {
            // Typed owned-get is a separate profile over the ordinary Map
            // lifecycle. Any typed-get signal owns arbitration even when its
            // receipt is incomplete; older lifecycle/Map identities cannot
            // claim the source or authorize the typed getter by implication.
            // However, an independently recognized incomplete MapAnyOwner
            // ledger is itself a hard source signal. Do not erase that
            // signal merely because typed-get arbitration also observed the
            // source; public callers must reject the whole mixed candidate
            // before considering a compatibility route.
            (
                MapRootAdmission::OutsideProfile,
                if ordinary_map_owned_get_i32_string_v1
                    == OrdinaryMapOwnedGetI32StringV1Admission::IncompleteCoverage
                    && map_any_owner == MapAnyOwnerAdmission::IncompleteCoverage
                {
                    MapAnyOwnerAdmission::IncompleteCoverage
                } else {
                    MapAnyOwnerAdmission::OutsideProfile
                },
                OrdinaryMapOwnerLifecycleI32StringV1Admission::OutsideProfile,
            )
        } else {
            let (map_root, map_any_owner) = select_map_owner_admissions(
                legacy_map_root,
                map_any_owner,
                ordinary_map_owner_lifecycle_i32_string_v1,
                ordinary_attempts_fully_covered_by_map_any_owner_v1,
            );
            // When the complete MapAnyOwner ledger covers every ordinary
            // lifecycle attempt, it owns the one materialized identity for
            // those same source roots and operations. Preserve the separate
            // ordinary source signal as covered, while independent ordinary
            // roots remain visible and block composition.
            let selected_ordinary_map_owner = if map_any_owner
                == MapAnyOwnerAdmission::CompleteCoverage
                && ordinary_attempts_fully_covered_by_map_any_owner_v1
            {
                OrdinaryMapOwnerLifecycleI32StringV1Admission::CoveredByMapAnyOwnerV1
            } else {
                ordinary_map_owner_lifecycle_i32_string_v1
            };
            (map_root, map_any_owner, selected_ordinary_map_owner)
        };
    MapOwnerProfileAdmission {
        map_root,
        map_any_owner,
        ordinary_map_owner_lifecycle_i32_string_v1: ordinary_map_owner_lifecycle_i32_string_v1,
        ordinary_map_owned_get_i32_string_v1,
    }
}

/// Classify route eligibility once from checker-owned typed facts.
pub fn classify_canonical_mir_route_admission(
    program: &CheckedProgram,
) -> CanonicalMirRouteAdmission {
    let map_owner = classify_map_owner_profile_admission(program);
    CanonicalMirRouteAdmission {
        map_root: map_owner.map_root,
        map_any_owner: map_owner.map_any_owner,
        ordinary_map_owner_lifecycle_i32_string_v1: map_owner
            .ordinary_map_owner_lifecycle_i32_string_v1,
        ordinary_map_owned_get_i32_string_v1: map_owner.ordinary_map_owned_get_i32_string_v1,
        scalar_export: super::is_scalar_export_candidate(program),
        scalar_ffi: super::is_scalar_ffi_candidate(program),
        scalar_nested: super::nested_callable::is_scalar_nested_candidate(program),
        scalar_actor: classify_scalar_actor_admission(program),
        scalar_generic_identity_i32: classify_scalar_generic_identity_admission(program),
        scalar_generic_identity_i64: classify_scalar_generic_identity_i64_admission(program),
        collection: classify_scalar_collection_admission(program),
        record: classify_flat_copy_record_admission(program),
        flow: classify_s8_flow_admission(program),
        flow_failure_retry: is_flow_failure_retry_candidate(program),
        option_string: classify_option_string_variant_admission(program),
        option_nested_tuple: classify_option_nested_tuple_variant_admission(program),
        generic_variant: classify_generic_variant_predicate_admission(program),
        generic_option_projection: classify_generic_option_projection_admission(program),
        generic_option_projection_fallback: classify_generic_option_projection_fallback_admission(
            program,
        ),
        generic_result_projection: classify_generic_result_projection_admission(program),
        generic_result_projection_fallback: classify_generic_result_projection_fallback_admission(
            program,
        ),
        managed_result_call: classify_managed_result_call_admission(program),
        copy_option_i32: classify_copy_option_i32_variant_admission(program),
        copy_option_bool: classify_copy_option_variant_admission(
            program,
            crate::core::PrimitiveType::Bool,
        ),
        copy_option_i64: classify_copy_option_variant_admission(
            program,
            crate::core::PrimitiveType::I64,
        ),
        copy_option_f64: classify_copy_option_variant_admission(
            program,
            crate::core::PrimitiveType::F64,
        ),
        copy_result_i32: classify_copy_result_i32_variant_admission(program),
        session: classify_session_channel_admission(program),
    }
}

/// Return a stable route diagnostic when checker-finalized effects contain an
/// opaque/unknown operation.  Unknown effects are never treated as an empty
/// effect list: an unresolved call, dynamic callable, or foreign operation
/// must keep the whole program out of a canonical MIR route.  This check is
/// intentionally route-owned so native, bytecode, and verifier dispatchers
/// share the same fail-closed boundary before any backend materializes code.
pub fn unknown_effect_boundary_reason(program: &CheckedProgram) -> Option<String> {
    let mut functions = program.functions().values().collect::<Vec<_>>();
    functions.sort_by(|left, right| {
        left.qualified_name
            .cmp(&right.qualified_name)
            .then_with(|| left.node_id.cmp(&right.node_id))
    });
    for function in functions {
        // The prelude contains effect-polymorphic higher-order helpers such
        // as `apply`. Their bodies necessarily contain a dynamic call, but a
        // dormant helper body is not a reachable program effect. A user call
        // to such a helper is still checked at its user call site below.
        if is_prelude_origin(program, &function.origin) {
            continue;
        }
        for effect in &function.effects {
            match crate::core::effects::EffectKind::parse(effect) {
                Some(crate::core::effects::EffectKind::Unknown) | None => {
                    if actor_unknown_effects_are_receipted(program, &function.node_id) {
                        continue;
                    }
                    return Some(format!(
                        "callable '{}' carries unknown effect '{}'; canonical MIR requires a finite effect boundary",
                        function.qualified_name, effect
                    ));
                }
                Some(_) => {}
            }
        }
    }
    for site in program.call_sites_sorted() {
        // Compiler-generated Flow transition scaffolding may contain a
        // constructor-shaped call whose surface name is not present in the
        // user callable directory (for example a generated fault state). It
        // is not a user dynamic/FFI boundary; user-origin call sites remain
        // fail-closed below.
        if is_prelude_origin(program, &site.origin)
            || !matches!(&site.origin, crate::core::Origin::User(_))
        {
            continue;
        }
        if site.effects.is_empty()
            && matches!(
                site.kind,
                crate::core::ResolvedCallKind::Extern | crate::core::ResolvedCallKind::Unknown
            )
        {
            return Some(format!(
                "call site '{}' in '{}' has no finite effect summary; canonical MIR refuses an opaque foreign/dynamic call",
                site.callee, site.owner
            ));
        }
        if site.effects.iter().any(|effect| {
            matches!(
                crate::core::effects::EffectKind::parse(effect),
                Some(crate::core::effects::EffectKind::Unknown) | None
            )
        }) && site.effects.iter().all(|effect| {
            matches!(
                crate::core::effects::EffectKind::parse(effect),
                Some(crate::core::effects::EffectKind::Unknown)
            )
        }) && actor_unknown_call_site_is_receipted(program, site)
        {
            continue;
        }
        for effect in &site.effects {
            match crate::core::effects::EffectKind::parse(effect) {
                Some(crate::core::effects::EffectKind::Unknown) | None => {
                    return Some(format!(
                        "call site '{}' in '{}' carries unknown effect '{}'; canonical MIR requires a finite effect boundary",
                        site.callee, site.owner, effect
                    ));
                }
                Some(_) => {}
            }
        }
    }
    None
}

/// The scalar Actor island owns two deliberately narrow sources of the
/// checker's conservative `unknown` summary: a direct nested helper call and
/// the typed mailbox/spawn boundary.  The helper is used only by the shared
/// route gate; Actor lowering still validates the complete lexical, payload,
/// and operation receipts before materializing code.
fn actor_unknown_effects_are_receipted(
    program: &CheckedProgram,
    owner: &crate::core::NodeId,
) -> bool {
    let Some(callable) = program.callable(owner) else {
        return false;
    };
    let unknown_sites = program
        .call_sites_sorted()
        .into_iter()
        .filter(|site| site.owner == owner.0)
        .filter(|site| {
            site.effects.iter().any(|effect| {
                matches!(
                    crate::core::effects::EffectKind::parse(effect),
                    Some(crate::core::effects::EffectKind::Unknown) | None
                )
            })
        })
        .collect::<Vec<_>>();
    !unknown_sites.is_empty()
        && unknown_sites
            .iter()
            .all(|site| actor_unknown_call_site_is_receipted(program, site))
        && (callable.body.root.statements.iter().any(|statement| {
            matches!(
                statement.kind,
                crate::core::ir::ResolvedStmtKind::NestedCallable(_)
            )
        }) || owner.0 == "function:main")
}

fn actor_unknown_call_site_is_receipted(
    program: &CheckedProgram,
    site: &crate::core::ResolvedCallSite,
) -> bool {
    match site.resolved_callee.as_ref() {
        Some(crate::core::ir::ResolvedCallee::Function(target)) => {
            let parent = crate::core::NodeId(site.owner.clone());
            program.actor_descriptors().values().any(|descriptor| {
                descriptor.scalar_i32_receipt(program).is_ok()
                    && descriptor.methods.iter().any(|method| {
                        method.owner == parent
                            && program.callable(&parent).is_some_and(|callable| {
                                callable.body.root.statements.iter().any(|statement| {
                                    matches!(
                                        &statement.kind,
                                        crate::core::ir::ResolvedStmtKind::NestedCallable(owner)
                                            if owner == target
                                    )
                                })
                            })
                    })
            })
        }
        Some(crate::core::ir::ResolvedCallee::ActorMethod { actor, method }) => program
            .actor_descriptors()
            .get(actor)
            .is_some_and(|descriptor| {
                descriptor.scalar_i32_receipt(program).is_ok()
                    && descriptor
                        .methods
                        .iter()
                        .any(|entry| entry.owner.0 == method.as_str())
            }),
        Some(crate::core::ir::ResolvedCallee::Builtin(builtin))
            if builtin.as_str() == "actor.spawn" =>
        {
            program
                .actor_descriptors()
                .values()
                .any(|descriptor| descriptor.scalar_i32_receipt(program).is_ok())
        }
        _ => false,
    }
}

fn is_prelude_origin(program: &CheckedProgram, origin: &crate::core::Origin) -> bool {
    program
        .source_registry()
        .key(origin.user_span().source_id)
        .is_some_and(|key| key.as_str() == "stdlib:prelude.mimi")
}

fn classify_ordinary_map_owned_get_i32_string_v1_admission(
    program: &CheckedProgram,
    closure_source_plan: Option<&super::closure_callable::CheckedClosureCallablePlanV1>,
) -> OrdinaryMapOwnedGetI32StringV1Admission {
    use OrdinaryMapOwnedGetI32StringV1ActionKind as Action;
    use OrdinaryMapOwnedGetI32StringV1AttemptKind as Attempt;

    let mut actions = Vec::<(crate::core::NodeId, OrdinaryMapOwnedGetI32StringV1Action)>::new();
    let mut new_attempts = std::collections::BTreeSet::new();
    let mut operation_attempts = std::collections::BTreeSet::new();
    let mut valid = true;
    let closure_source_candidate =
        super::closure_callable::CheckedClosureCallablePlanV1::has_source_candidate(program);

    for analysis in program.resource_analyses().values() {
        new_attempts.extend(
            analysis
                .ordinary_map_owned_get_i32_string_v1_new_attempts
                .iter()
                .cloned()
                .map(|point| (analysis.owner.clone(), point)),
        );
        operation_attempts.extend(
            analysis
                .ordinary_map_owned_get_i32_string_v1_operation_attempts
                .iter()
                .cloned()
                .map(|attempt| (analysis.owner.clone(), attempt)),
        );
        for action in &analysis.ordinary_map_owned_get_i32_string_v1_actions {
            valid &= action.profile_version == OrdinaryMapOwnedGetI32StringV1ProfileVersion::V1;
            actions.push((analysis.owner.clone(), action.clone()));
        }
    }

    if let Some(closure_plan) = closure_source_plan {
        for (owner, child_actions, child_new_attempts, child_operation_attempts) in
            closure_plan.ordinary_map_ledgers()
        {
            for action in child_actions {
                valid &= action.profile_version == OrdinaryMapOwnedGetI32StringV1ProfileVersion::V1;
                actions.push((owner.clone(), action));
            }
            new_attempts.extend(
                child_new_attempts
                    .into_iter()
                    .map(|point| (owner.clone(), point)),
            );
            operation_attempts.extend(
                child_operation_attempts
                    .into_iter()
                    .map(|attempt| (owner.clone(), attempt)),
            );
        }
    } else if closure_source_candidate {
        // A checked Any-bearing closure with no complete independent child
        // receipt is an incomplete Map/GetAny source candidate. Keep the
        // ordinary Map route from falling through to a compatibility emitter.
        valid = false;
    }

    let has_typed_get_attempt = operation_attempts.iter().any(|(_, attempt)| {
        matches!(
            attempt.kind,
            Attempt::GetI32 | Attempt::GetString | Attempt::UntypedGet | Attempt::Values
        )
    });
    let has_typed_get_action = actions.iter().any(|(_, action)| {
        matches!(
            action.kind,
            Action::GetI32 | Action::GetString | Action::GetAny | Action::Values
        )
    });
    if !has_typed_get_attempt && !has_typed_get_action && !closure_source_candidate {
        // This checker also scans common Map operations to prove whole-program
        // coverage once a typed getter opts the source into this profile. A
        // program with no typed-get signal remains eligible for older Map
        // profiles rather than being treated as an incomplete getter island.
        return OrdinaryMapOwnedGetI32StringV1Admission::OutsideProfile;
    }

    let mut action_new_points = std::collections::BTreeSet::new();
    let mut operation_new_points = std::collections::BTreeSet::new();
    for (owner, attempt) in &operation_attempts {
        if attempt.kind == Attempt::New {
            operation_new_points.insert((owner.clone(), attempt.point.clone()));
        }
    }
    for (owner, action) in &actions {
        if action.kind == Action::New {
            action_new_points.insert((owner.clone(), action.point.clone()));
        }
    }

    // Match both ledgers one-to-one. The operation point and exact key/value
    // expression identities bind source calls; Drop shares a statement point
    // when several locals are settled, so its root-local argument participates
    // in the identity too.
    let mut unmatched_attempts = operation_attempts.clone();
    for (owner, action) in &actions {
        let matched = unmatched_attempts
            .iter()
            .find(|(attempt_owner, attempt)| {
                attempt_owner == owner && owned_get_action_matches_attempt(action, attempt)
            })
            .cloned();
        if let Some(matched) = matched {
            unmatched_attempts.remove(&matched);
        } else {
            valid = false;
        }
    }
    if !unmatched_attempts.is_empty() {
        // This includes old tuple-valued map_get and any recognized operation
        // not covered by the frozen typed-get receipt.
        valid = false;
    }

    let getter_receipt_complete = (has_typed_get_attempt && has_typed_get_action) && valid;
    let new_coverage_complete = !new_attempts.is_empty()
        && new_attempts == action_new_points
        && new_attempts == operation_new_points;
    if valid && getter_receipt_complete && new_coverage_complete {
        OrdinaryMapOwnedGetI32StringV1Admission::CompleteCoverage
    } else {
        OrdinaryMapOwnedGetI32StringV1Admission::IncompleteCoverage
    }
}

fn ordinary_map_owner_has_direct_string_key_local(program: &CheckedProgram) -> bool {
    use crate::core::OrdinaryMapOwnerLifecycleActionKind as Kind;

    program.resource_analyses().values().any(|analysis| {
        analysis
            .ordinary_map_owner_lifecycle_v1_actions
            .iter()
            .any(|action| {
                matches!(action.kind, Kind::Set | Kind::Remove | Kind::HasKey)
                    && action.key_local.is_some()
            })
    })
}

fn owned_get_action_matches_attempt(
    action: &OrdinaryMapOwnedGetI32StringV1Action,
    attempt: &OrdinaryMapOwnedGetI32StringV1Attempt,
) -> bool {
    use OrdinaryMapOwnedGetI32StringV1ActionKind as Action;
    use OrdinaryMapOwnedGetI32StringV1AttemptKind as Attempt;

    if action.point != attempt.point {
        return false;
    }
    match (action.kind, attempt.kind) {
        (Action::New, Attempt::New) => attempt.arguments.is_empty(),
        (Action::Clone, Attempt::Clone) => {
            attempt.arguments.len() == 2
                && action
                    .source_local
                    .as_ref()
                    .is_some_and(|source| source.0 == attempt.arguments[0])
                && action.local.0 == attempt.arguments[1]
        }
        (Action::Set, Attempt::Set) => {
            attempt.arguments.len() == 3
                && action.key.as_ref() == attempt.arguments.get(1)
                && action.value.as_ref() == attempt.arguments.get(2)
        }
        (Action::Remove, Attempt::Remove) => {
            attempt.arguments.len() == 2 && action.key.as_ref() == attempt.arguments.get(1)
        }
        (Action::Size, Attempt::Size) => attempt.arguments.len() == 1,
        (Action::HasKey, Attempt::HasKey)
        | (Action::GetI32, Attempt::GetI32)
        | (Action::GetString, Attempt::GetString)
        | (Action::GetAny, Attempt::UntypedGet) => {
            attempt.arguments.len() == 2 && action.key.as_ref() == attempt.arguments.get(1)
        }
        (Action::Values, Attempt::Values) => attempt.arguments.len() == 1,
        (Action::Drop, Attempt::Drop | Attempt::ScopeDrop) => {
            attempt.arguments.len() == 1 && action.local.0 == attempt.arguments[0]
        }
        _ => false,
    }
}

fn classify_ordinary_map_owner_lifecycle_i32_string_v1_admission(
    program: &CheckedProgram,
) -> OrdinaryMapOwnerLifecycleI32StringV1Admission {
    let mut valid = true;
    let mut new_attempts = std::collections::BTreeSet::new();
    let mut operation_attempts = std::collections::BTreeSet::new();
    let mut actions = Vec::new();
    for analysis in program.resource_analyses().values() {
        new_attempts.extend(
            analysis
                .ordinary_map_owner_lifecycle_v1_new_attempts
                .iter()
                .cloned()
                .map(|point| (analysis.owner.clone(), point)),
        );
        operation_attempts.extend(
            analysis
                .ordinary_map_owner_lifecycle_v1_operation_attempts
                .iter()
                .cloned()
                .map(|attempt| (analysis.owner.clone(), attempt)),
        );
        for action in &analysis.ordinary_map_owner_lifecycle_v1_actions {
            valid &= action.profile_version == OrdinaryMapOwnerLifecycleProfileVersion::V1;
            actions.push((analysis.owner.clone(), action));
        }
    }
    if actions.is_empty() {
        return if new_attempts.is_empty() && operation_attempts.is_empty() {
            OrdinaryMapOwnerLifecycleI32StringV1Admission::OutsideProfile
        } else {
            OrdinaryMapOwnerLifecycleI32StringV1Admission::IncompleteCoverage
        };
    }
    let action_new_points = actions
        .iter()
        .filter(|(_, action)| action.kind == OrdinaryMapOwnerLifecycleActionKind::New)
        .map(|(owner, action)| (owner.clone(), action.point.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    let operation_new_points = operation_attempts
        .iter()
        .filter(|(_, attempt)| attempt.kind == OrdinaryMapOwnerLifecycleAttemptKind::New)
        .map(|(owner, attempt)| (owner.clone(), attempt.point.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    // Scope cleanup can share the callable's final CFG point with another
    // operation. Bind every action to one exact attempt including its root
    // local, rather than collapsing a point into one root operation.
    let mut unmatched = operation_attempts.clone();
    for (owner, action) in actions {
        let matched = unmatched
            .iter()
            .find(|(attempt_owner, attempt)| {
                attempt_owner == &owner
                    && action.point == attempt.point
                    && ordinary_action_matches_attempt(action, attempt)
            })
            .cloned();
        if let Some(matched) = matched {
            unmatched.remove(&matched);
        } else {
            valid = false;
        }
    }
    valid &= unmatched.is_empty();
    let new_coverage_complete = !new_attempts.is_empty()
        && new_attempts == action_new_points
        && new_attempts == operation_new_points;
    if valid && new_coverage_complete {
        OrdinaryMapOwnerLifecycleI32StringV1Admission::CompleteCoverage
    } else {
        OrdinaryMapOwnerLifecycleI32StringV1Admission::IncompleteCoverage
    }
}

fn ordinary_action_matches_attempt(
    action: &crate::core::OrdinaryMapOwnerLifecycleAction,
    attempt: &crate::core::OrdinaryMapOwnerLifecycleAttempt,
) -> bool {
    use OrdinaryMapOwnerLifecycleActionKind as Action;
    use OrdinaryMapOwnerLifecycleAttemptKind as Attempt;

    match (action.kind, attempt.kind) {
        (Action::New, Attempt::New) => attempt.arguments.is_empty(),
        (Action::Clone, Attempt::Clone) => {
            attempt.arguments.len() == 2
                && action
                    .source_local
                    .as_ref()
                    .is_some_and(|source| source.0 == attempt.arguments[0])
                && action.local.0 == attempt.arguments[1]
        }
        (Action::Set, Attempt::Set) => {
            attempt.arguments.len() == 3
                && action.key.as_ref() == attempt.arguments.get(1)
                && action.value.as_ref() == attempt.arguments.get(2)
        }
        (Action::Remove, Attempt::Remove) => {
            attempt.arguments.len() == 2 && action.key.as_ref() == attempt.arguments.get(1)
        }
        (Action::Size, Attempt::Size) => attempt.arguments.len() == 1,
        (Action::HasKey, Attempt::HasKey) => {
            attempt.arguments.len() == 2 && action.key.as_ref() == attempt.arguments.get(1)
        }
        (Action::Drop, Attempt::Drop | Attempt::ScopeDrop) => {
            attempt.arguments.len() == 1 && action.local.0 == attempt.arguments[0]
        }
        // All other combinations include unsupported or mismatched coverage.
        _ => false,
    }
}

fn select_map_owner_admissions(
    legacy_map_root: MapRootAdmission,
    map_any_owner: MapAnyOwnerAdmission,
    ordinary_map_owner: OrdinaryMapOwnerLifecycleI32StringV1Admission,
    ordinary_attempts_fully_covered_by_map_any_owner_v1: bool,
) -> (MapRootAdmission, MapAnyOwnerAdmission) {
    // Keep the established MapAnyOwner route when its independent ledger
    // covers every overlapping ordinary-Map attempt. The ordinary checker
    // intentionally recognizes the same supported operations, so letting
    // its complete ledger win first would erase MapAnyOwner's own identity.
    if ordinary_attempts_fully_covered_by_map_any_owner_v1
        && map_any_owner == MapAnyOwnerAdmission::CompleteCoverage
    {
        return (MapRootAdmission::OutsideProfile, map_any_owner);
    }
    // A recognized MapAnyOwner candidate with missing or extra receipts is a
    // hard boundary. Do not hide that incomplete ledger behind a complete
    // ordinary-Map profile or re-open the legacy MapRoot route.
    if map_any_owner == MapAnyOwnerAdmission::IncompleteCoverage {
        return (MapRootAdmission::OutsideProfile, map_any_owner);
    }
    if map_any_owner == MapAnyOwnerAdmission::CompleteCoverage && ordinary_map_owner.is_complete() {
        // Preserve both complete identities until the caller proves that the
        // MapAnyOwner receipt covers the ordinary attempts. A distinct
        // ordinary owner must not disappear behind a complete MapAnyOwner
        // profile merely because both recognize some of the same operations.
        return (MapRootAdmission::OutsideProfile, map_any_owner);
    }
    if ordinary_map_owner.is_complete() {
        return (
            MapRootAdmission::OutsideProfile,
            MapAnyOwnerAdmission::OutsideProfile,
        );
    }
    if ordinary_map_owner.has_profile_signal() {
        // Unsupported ordinary Map shapes stay on the compatibility route.
        // They cannot borrow a MapRoot or MapAnyOwner receipt for a supported
        // subset of the same source program.
        return (
            MapRootAdmission::OutsideProfile,
            MapAnyOwnerAdmission::OutsideProfile,
        );
    }
    (
        select_map_root_admission(legacy_map_root, map_any_owner),
        map_any_owner,
    )
}

/// Whether the complete MapAnyOwner v1 receipt covers every ordinary
/// lifecycle attempt with the same checker root and operation operands, with
/// no extra MapAnyOwner action or independent ordinary owner. The predicate
/// is non-vacuous and one-to-one across the whole checked program.
#[doc(hidden)]
pub fn ordinary_map_attempts_fully_covered_by_map_any_owner_v1(program: &CheckedProgram) -> bool {
    let mut saw_ordinary_attempts = false;
    for analysis in program.resource_analyses().values() {
        let has_attempts = analysis.has_ordinary_map_owner_lifecycle_v1_attempts();
        let has_map_any_actions = !analysis.map_any_owner_actions.is_empty();
        if !has_attempts && has_map_any_actions {
            return false;
        }
        if !has_attempts {
            continue;
        }
        saw_ordinary_attempts = true;
        if !analysis.ordinary_map_attempts_fully_covered_by_map_any_owner_v1()
            || !analysis.map_any_owner_v1_actions_are_within_ordinary_map_profile()
        {
            return false;
        }
    }
    saw_ordinary_attempts
}

/// Whether every MapAnyOwner v1 action has one exact ordinary lifecycle source
/// attempt. This is descriptive evidence only; it never authorizes an
/// incomplete MapAnyOwner ledger.
#[doc(hidden)]
pub fn map_any_owner_v1_actions_are_within_ordinary_map_profile(program: &CheckedProgram) -> bool {
    let mut saw_map_any_actions = false;
    for analysis in program.resource_analyses().values() {
        if analysis.map_any_owner_actions.is_empty() {
            continue;
        }
        saw_map_any_actions = true;
        if !analysis.map_any_owner_v1_actions_are_within_ordinary_map_profile() {
            return false;
        }
    }
    saw_map_any_actions
}

fn classify_map_root_admission(program: &CheckedProgram) -> MapRootAdmission {
    let mut has_receipted_lifecycle = false;
    let mut attempted_new_points = std::collections::BTreeSet::new();
    let mut receipted_new_points = std::collections::BTreeSet::new();
    for analysis in program.resource_analyses().values() {
        attempted_new_points.extend(
            analysis
                .map_root_new_attempts
                .iter()
                .cloned()
                .map(|point| (analysis.owner.clone(), point)),
        );
        for action in &analysis.map_root_actions {
            has_receipted_lifecycle = true;
            if action.kind == crate::core::MapRootActionKind::New {
                receipted_new_points.insert((analysis.owner.clone(), action.point.clone()));
            }
        }
    }
    if !has_receipted_lifecycle {
        return MapRootAdmission::OutsideProfile;
    }
    if !attempted_new_points.is_empty() && attempted_new_points == receipted_new_points {
        MapRootAdmission::CompleteCoverage
    } else {
        MapRootAdmission::IncompleteCoverage
    }
}

fn classify_map_any_owner_admission(
    program: &CheckedProgram,
    legacy_map_root: MapRootAdmission,
) -> MapAnyOwnerAdmission {
    let mut has_actions = false;
    let mut has_non_v1_action = false;
    let mut attempted_new_points = std::collections::BTreeSet::new();
    let mut receipted_new_points = std::collections::BTreeSet::new();
    let mut new_action_count = 0usize;
    for analysis in program.resource_analyses().values() {
        attempted_new_points.extend(
            analysis
                .map_any_owner_new_attempts
                .iter()
                .cloned()
                .map(|point| (analysis.owner.clone(), point)),
        );
        has_actions |= !analysis.map_any_owner_actions.is_empty();
        has_non_v1_action |= analysis
            .ordinary_map_owner_lifecycle_v1_operation_attempts
            .iter()
            .any(|attempt| !ordinary_attempt_has_map_any_owner_v1_receipt(analysis, attempt));
        for action in &analysis.map_any_owner_actions {
            if action.profile_version != crate::core::MapAnyOwnerProfileVersion::V1 {
                has_non_v1_action = true;
            }
            if action.kind == crate::core::MapAnyOwnerActionKind::New {
                new_action_count += 1;
                receipted_new_points.insert((analysis.owner.clone(), action.point.clone()));
            }
        }
    }
    let attempts_match_receipts = !attempted_new_points.is_empty()
        && attempted_new_points == receipted_new_points
        && new_action_count == receipted_new_points.len();
    classify_map_any_owner_ledger(
        has_actions,
        !attempted_new_points.is_empty(),
        attempts_match_receipts,
        has_non_v1_action,
        legacy_map_root,
    )
}

fn ordinary_attempt_has_map_any_owner_v1_receipt(
    analysis: &crate::core::ResourceAnalysis,
    attempt: &crate::core::OrdinaryMapOwnerLifecycleAttempt,
) -> bool {
    use OrdinaryMapOwnerLifecycleAttemptKind as Attempt;

    if matches!(
        attempt.kind,
        Attempt::New
            | Attempt::Set
            | Attempt::Remove
            | Attempt::Size
            | Attempt::HasKey
            | Attempt::Drop
            | Attempt::ScopeDrop
    ) {
        analysis.map_any_owner_v1_covers_ordinary_attempt(attempt)
    } else {
        false
    }
}

fn classify_map_any_owner_ledger(
    has_actions: bool,
    has_attempts: bool,
    attempts_match_receipts: bool,
    has_non_v1_action: bool,
    legacy_map_root: MapRootAdmission,
) -> MapAnyOwnerAdmission {
    if !has_actions {
        // Construction attempts are not typed authority: ordinary dynamic
        // Map/Any programs also call map_new(), often in shapes that the v1
        // recognizer deliberately does not model. Keep those programs on the
        // compatibility route unless either lifecycle ledger contains an
        // actual receipt. A legacy receipt can still expose a missing v1
        // ledger, but it may never re-admit the old MapRoot identity.
        return if legacy_map_root.is_candidate() {
            MapAnyOwnerAdmission::IncompleteCoverage
        } else {
            MapAnyOwnerAdmission::OutsideProfile
        };
    }
    if has_actions && has_attempts && attempts_match_receipts && !has_non_v1_action {
        MapAnyOwnerAdmission::CompleteCoverage
    } else {
        MapAnyOwnerAdmission::IncompleteCoverage
    }
}

fn select_map_root_admission(
    legacy_map_root: MapRootAdmission,
    map_any_owner: MapAnyOwnerAdmission,
) -> MapRootAdmission {
    if map_any_owner.is_candidate() {
        MapRootAdmission::OutsideProfile
    } else {
        legacy_map_root
    }
}

fn classify_s8_flow_admission(program: &CheckedProgram) -> S8FlowAdmission {
    if is_exact_s8_flow_transition(program) {
        S8FlowAdmission::CompleteCoverage
    } else if is_closed_silent_local_flow_candidate(program) {
        // The canonical MIR native adapter already validates direct
        // SilentLocal and multi-transition records.  Keep the
        // checker admission complete for this broader scalar shape so the
        // route materializer owns it before compile_flow can be reached.
        S8FlowAdmission::CompleteCoverage
    } else if is_s8_flow_transition_candidate(program) {
        S8FlowAdmission::MixedCoverage
    } else {
        S8FlowAdmission::OutsideProfile
    }
}

/// Whole checked-source proof boundary for the bounded Actor NoContracts
/// route. Default consumers retain their full contract-verification path;
/// explicit Actor MIR construction rejects before any graph is trimmed.
pub fn scalar_actor_contract_boundary_reason(program: &CheckedProgram) -> Option<String> {
    let excluded = program
        .source_registry()
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<HashSet<_>>();
    if !super::actor_lower::has_actor_candidate(program, Some(&excluded)) {
        return None;
    }
    super::actor_lower::contract_boundary_reason(program, Some(&excluded))
}

/// Detect the selected nested-callable Actor shape before route selection.
/// This is intentionally narrower than `scalar_actor_contract_boundary_reason`:
/// proof-bearing Actor programs retain their explicit compatibility verifier,
/// while a no-contract capture/shadow candidate must be canonical or fail
/// closed and may never silently enter the legacy emitter.
pub fn has_scalar_actor_nested_callable_candidate(program: &CheckedProgram) -> bool {
    let excluded = program
        .source_registry()
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<HashSet<_>>();
    super::actor_lower::has_nested_callable_candidate(program, Some(&excluded))
}

/// Admission uses checker-owned typed bodies and private operation/environment
/// receipts. Unsupported Actor bodies stay outside this bounded profile;
/// successful admission owns a hard materialization boundary for every caller.
fn classify_scalar_actor_admission(program: &CheckedProgram) -> bool {
    if super::exported_abi_body_boundary_reason(program).is_some()
        || scalar_actor_contract_boundary_reason(program).is_some()
    {
        return false;
    }
    let excluded = program
        .source_registry()
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<HashSet<_>>();
    if !super::actor_lower::has_actor_candidate(program, Some(&excluded)) {
        return false;
    }
    super::actor_lower::materialize(program, Some(&excluded))
        .is_ok_and(|canonical| validate_scalar_actor_island(&canonical).is_ok())
}

/// Recheck the exact scalar Actor identity before an execution or verification
/// consumer accepts the shared graph. No mailbox contract is statically proved.
pub fn validate_scalar_actor_island(program: &MirProgram) -> Result<(), Vec<String>> {
    program.validate_actor_contracts()?;
    let Some(plan) = program.actor_execution_plan() else {
        return Err(vec![
            "scalar Actor island has no checked execution plan".into()
        ]);
    };
    if program.actors().is_empty() || plan.root.0 != "function:main" {
        return Err(vec![
            "scalar Actor island has no checked Actor/root contract".into(),
        ]);
    }
    if program
        .functions()
        .values()
        .any(|function| !function.contracts.is_empty())
    {
        return Err(vec![
            "Actor mailbox contracts have no trusted static proof model".into(),
        ]);
    }
    Ok(())
}

/// Construct the one canonical graph shared by the current production route
/// owners, and attach operation materialization receipts.
pub fn materialize_canonical_mir_route(
    program: &CheckedProgram,
    excluded_sources: Option<&HashSet<crate::span::SourceId>>,
) -> Result<CanonicalMirRouteMaterialization, CanonicalMirRouteMaterializationError> {
    #[cfg(test)]
    TEST_ROUTE_MATERIALIZATION_COUNT.with(|count| count.set(count.get() + 1));
    let admission = classify_canonical_mir_route_admission(program);
    if admission.map_any_owner == MapAnyOwnerAdmission::IncompleteCoverage {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::MapAnyOwnerV1,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message: "Checker recognized a MapAnyOwner v1 candidate but withheld its complete independent lifecycle receipt".into(),
        });
    }
    if admission.ordinary_map_owned_get_i32_string_v1
        == OrdinaryMapOwnedGetI32StringV1Admission::IncompleteCoverage
    {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message: "Checker recognized a typed owned-get Map candidate but withheld its complete independent operation-attempt receipt".into(),
        });
    }
    if admission.ordinary_map_owned_get_i32_string_v1_complete() {
        if let Some(message) = ordinary_map_owned_get_source_result_settlement_reason(program) {
            return Err(CanonicalMirRouteMaterializationError::Complete {
                profile: CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1,
                stage: CanonicalMirRouteFailureStage::Coverage,
                message,
            });
        }
    }
    if admission.ordinary_map_owner_lifecycle_i32_string_v1
        == OrdinaryMapOwnerLifecycleI32StringV1Admission::IncompleteCoverage
        && !(admission.map_any_owner_complete()
            && ordinary_map_attempts_fully_covered_by_map_any_owner_v1(program))
    {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message: "Checker recognized an ordinary Map owner lifecycle candidate but withheld complete operation-attempt coverage".into(),
        });
    }
    if admission.map_root == MapRootAdmission::IncompleteCoverage {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::MapRoot,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message: "Checker recognized a MapRoot candidate but withheld its complete local lifecycle receipt".into(),
        });
    }
    // The Actor producer already owns a complete finite whole-graph receipt.
    // Do not compose unrelated scalar/collection profile predicates with its
    // private state and status ABI. Every route owner receives this same graph.
    if admission.scalar_actor {
        let mut excluded = excluded_sources.cloned().unwrap_or_default();
        excluded.extend(
            program
                .source_registry()
                .records()
                .iter()
                .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
                .map(|record| record.id),
        );
        let canonical =
            super::actor_lower::materialize(program, Some(&excluded)).map_err(|error| {
                CanonicalMirRouteMaterializationError::Complete {
                    profile: CanonicalMirRouteProfile::ScalarActor,
                    stage: CanonicalMirRouteFailureStage::Construction,
                    message: error.to_string(),
                }
            })?;
        validate_scalar_actor_island(&canonical).map_err(|errors| {
            CanonicalMirRouteMaterializationError::Complete {
                profile: CanonicalMirRouteProfile::ScalarActor,
                stage: CanonicalMirRouteFailureStage::Coverage,
                message: format!("scalar Actor receipt rejected: {errors:?}"),
            }
        })?;
        return Ok(CanonicalMirRouteMaterialization {
            program: canonical,
            admission,
            materialized_map_root_candidate: false,
            materialized_map_any_owner_candidate: false,
            materialized_ordinary_map_owner_lifecycle_i32_string_v1_candidate: false,
            materialized_ordinary_map_owned_get_i32_string_v1_candidate: false,
            materialized_scalar_export_candidate: false,
            materialized_scalar_ffi_candidate: false,
            materialized_scalar_nested_candidate: false,
            materialized_scalar_actor_candidate: true,
            materialized_scalar_generic_identity_i32_candidate: false,
            materialized_scalar_generic_identity_i64_candidate: false,
            materialized_collection_candidate: false,
            materialized_collection_operation_candidate: false,
            materialized_record_candidate: false,
            materialized_flow_candidate: false,
            materialized_flow_failure_retry_candidate: false,
            materialized_option_string_candidate: false,
            materialized_option_nested_tuple_candidate: false,
            materialized_generic_variant_candidate: false,
            materialized_generic_option_projection_candidate: false,
            materialized_generic_option_projection_fallback_candidate: false,
            materialized_generic_result_projection_candidate: false,
            materialized_generic_result_projection_fallback_candidate: false,
            materialized_managed_result_call_candidate: false,
            materialized_copy_option_i32_candidate: false,
            materialized_copy_option_bool_candidate: false,
            materialized_copy_option_i64_candidate: false,
            materialized_copy_option_f64_candidate: false,
            materialized_copy_result_i32_candidate: false,
            materialized_session_candidate: false,
        });
    }
    if !admission.scalar_ffi {
        if let Some(reason) = scalar_ffi_boundary_reason(program) {
            return Err(CanonicalMirRouteMaterializationError::Compatibility {
                admission,
                message: format!("scalar FFI declaration boundary: {reason}"),
            });
        }
    }
    // All public scalar-FFI consumers use the same known prelude exclusion.
    // Imported/user code stays in the graph; calls into excluded prelude code
    // remain missing-target errors, never compatibility retries.  The
    // session-channel, generic Option/Result projection, and managed Result
    // islands share the exclusion: direct native/public verify paths must
    // observe the same prelude-free graph as CLI dispatch. A complete flat
    // Copy-record admission shares it for the same reason: preloaded prelude
    // bodies carry Assign statements MIR Phase 0 cannot lower, and dispatch
    // never lowers them.
    // The plain-scalar collection island (R6-1052) shares the exclusion for
    // the same reason, so a direct `materialize_canonical_mir_route` caller
    // observes the same prelude-free graph as the CLI dispatch wrapper.
    let mut selected_exclusions = excluded_sources.cloned().unwrap_or_default();
    if admission.scalar_export
        || admission.scalar_ffi
        || admission.scalar_nested
        || admission.map_root_complete()
        || admission.map_any_owner_complete()
        || admission.ordinary_map_owner_lifecycle_i32_string_v1_complete()
        || admission.ordinary_map_owned_get_i32_string_v1_complete()
        || admission.scalar_generic_identity_i32_complete()
        || admission.scalar_generic_identity_i64_complete()
        || admission.generic_variant_complete()
        || admission.generic_option_projection_complete()
        || admission.generic_option_projection_fallback_complete()
        || admission.generic_result_projection_complete()
        || admission.generic_result_projection_fallback_complete()
        || admission.managed_result_call_complete()
        || admission.session_complete()
        || admission.record_complete()
        || admission.collection_complete()
    {
        selected_exclusions.extend(
            program
                .source_registry()
                .records()
                .iter()
                .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
                .map(|record| record.id),
        );
    }
    let excluded_sources = (!selected_exclusions.is_empty()).then_some(&selected_exclusions);
    let canonical = match excluded_sources {
        Some(excluded_sources) => {
            MirProgram::from_checked_program_excluding_sources(program, excluded_sources)
        }
        None => MirProgram::from_checked_program(program),
    };
    let canonical = match canonical {
        Ok(canonical) => canonical,
        Err(error) => {
            return Err(match_complete_or_compatibility(
                admission,
                CanonicalMirRouteFailureStage::Construction,
                error.to_string(),
            ));
        }
    };

    let materialized_scalar_export_candidate =
        admission.scalar_export && !canonical.export_declarations().is_empty();
    let materialized_scalar_ffi_candidate = !canonical.ffi_calls().is_empty();
    let materialized_scalar_nested_candidate =
        admission.scalar_nested && !canonical.nested_callable_scopes().is_empty();
    let materialized_map_root_candidate = canonical
        .functions()
        .values()
        .any(|function| function.ownership.has_checker_map_root_actions());
    let materialized_map_any_owner_candidate = canonical
        .functions()
        .values()
        .any(|function| function.ownership.has_checker_map_any_owner_actions());
    let materialized_ordinary_map_owner_lifecycle_i32_string_v1_candidate = admission
        .ordinary_map_owner_lifecycle_i32_string_v1_complete()
        && contains_ordinary_map_owner_lifecycle_i32_string_v1_candidate(&canonical);
    let materialized_ordinary_map_owned_get_i32_string_v1_candidate = admission
        .ordinary_map_owned_get_i32_string_v1_complete()
        && contains_ordinary_map_owned_get_i32_string_v1_candidate(&canonical);
    if materialized_ordinary_map_owned_get_i32_string_v1_candidate {
        if let Some(message) = ordinary_map_owned_get_result_settlement_reason(&canonical) {
            return Err(CanonicalMirRouteMaterializationError::Complete {
                profile: CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1,
                stage: CanonicalMirRouteFailureStage::Coverage,
                message,
            });
        }
    }
    if admission.map_root_complete() && !materialized_map_root_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::MapRoot,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message:
                "complete Checker MapRoot admission did not materialize a MapRoot action receipt"
                    .into(),
        });
    }
    if admission.map_any_owner_complete() && !materialized_map_any_owner_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::MapAnyOwnerV1,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message: "complete Checker MapAnyOwner v1 admission did not materialize an independent v1 action receipt".into(),
        });
    }
    if admission.ordinary_map_owner_lifecycle_i32_string_v1_complete()
        && (!materialized_ordinary_map_owner_lifecycle_i32_string_v1_candidate
            || materialized_map_root_candidate
            || materialized_map_any_owner_candidate)
    {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message: "complete ordinary Map owner admission did not materialize its isolated operation identity".into(),
        });
    }
    if admission.ordinary_map_owned_get_i32_string_v1_complete()
        && (!materialized_ordinary_map_owned_get_i32_string_v1_candidate
            || materialized_map_root_candidate
            || materialized_map_any_owner_candidate
            || contains_ordinary_map_owner_lifecycle_i32_string_v1_candidate(&canonical))
    {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message: "complete typed owned-get Map admission did not materialize its isolated Checker/MIR operation receipt".into(),
        });
    }
    if admission.scalar_ffi && !materialized_scalar_ffi_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::ScalarFfi,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message: "scalar FFI admission did not materialize a canonical FFI receipt".into(),
        });
    }
    if admission.scalar_nested && !materialized_scalar_nested_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::ScalarNested,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message:
                "scalar nested admission did not materialize a canonical nested-callable receipt"
                    .into(),
        });
    }
    if admission.scalar_export && !materialized_scalar_export_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::ScalarExport,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message:
                "scalar export admission did not materialize a canonical export wrapper receipt"
                    .into(),
        });
    }
    let scalar_generic_identity_candidate = contains_scalar_generic_identity_candidate(&canonical);
    let materialized_scalar_generic_identity_i32_candidate =
        admission.scalar_generic_identity_i32_complete() && scalar_generic_identity_candidate;
    if admission.scalar_generic_identity_i32_complete()
        && !materialized_scalar_generic_identity_i32_candidate
    {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::ScalarGenericIdentityI32,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message: "complete generic scalar identity admission did not materialize a ScalarIdentity instance".into(),
        });
    }
    if admission.scalar_generic_identity_i32_complete() {
        super::validate_scalar_generic_identity_island(&canonical).map_err(|errors| {
            CanonicalMirRouteMaterializationError::Complete {
                profile: CanonicalMirRouteProfile::ScalarGenericIdentityI32,
                stage: CanonicalMirRouteFailureStage::Coverage,
                message: format!(
                    "generic scalar identity whole-program validator rejected the graph: {errors:?}"
                ),
            }
        })?;
    }
    let materialized_scalar_generic_identity_i64_candidate =
        admission.scalar_generic_identity_i64_complete() && scalar_generic_identity_candidate;
    if admission.scalar_generic_identity_i64_complete()
        && !materialized_scalar_generic_identity_i64_candidate
    {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::ScalarGenericIdentityI64,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message: "complete generic scalar identity admission did not materialize a ScalarIdentity instance".into(),
        });
    }
    if admission.scalar_generic_identity_i64_complete() {
        super::validate_scalar_generic_identity_i64_island(&canonical).map_err(|errors| {
            CanonicalMirRouteMaterializationError::Complete {
                profile: CanonicalMirRouteProfile::ScalarGenericIdentityI64,
                stage: CanonicalMirRouteFailureStage::Coverage,
                message: format!(
                    "generic scalar identity whole-program validator rejected the graph: {errors:?}"
                ),
            }
        })?;
    }
    let materialized_collection_operation_candidate =
        contains_scalar_collection_operation_candidate(&canonical)
            && !contains_only_managed_any_values_list_len(&canonical);
    let materialized_collection_candidate = materialized_collection_operation_candidate
        || (admission.collection_complete()
            && canonical.transitions().is_empty()
            && contains_scalar_collection_candidate(&canonical));
    let materialized_record_candidate = contains_flat_copy_record_candidate(&canonical);
    let materialized_flow_candidate = contains_s8_flow_transition_candidate(&canonical);
    let materialized_flow_failure_retry_candidate =
        contains_flow_failure_retry_candidate(&canonical);
    let materialized_option_string_candidate = contains_option_string_variant_candidate(&canonical);
    let materialized_option_nested_tuple_candidate =
        contains_option_nested_tuple_variant_candidate(&canonical);
    let materialized_generic_variant_candidate =
        contains_generic_variant_predicate_candidate(&canonical);
    let materialized_generic_option_projection_candidate =
        contains_generic_option_projection_candidate(&canonical);
    let materialized_generic_option_projection_fallback_candidate =
        contains_generic_option_projection_fallback_candidate(&canonical);
    let materialized_generic_result_projection_candidate =
        contains_generic_result_projection_candidate(&canonical);
    let materialized_generic_result_projection_fallback_candidate =
        contains_generic_result_projection_fallback_candidate(&canonical);
    let materialized_managed_result_call_candidate =
        contains_managed_result_call_candidate(&canonical);
    let materialized_copy_option_i32_candidate =
        contains_copy_option_i32_variant_candidate(&canonical);
    let materialized_copy_option_bool_candidate =
        contains_copy_option_variant_candidate(&canonical, crate::core::PrimitiveType::Bool);
    let materialized_copy_option_i64_candidate =
        super::contains_copy_option_i64_variant_candidate(&canonical);
    let materialized_copy_option_f64_candidate =
        super::contains_copy_option_f64_variant_candidate(&canonical);
    let materialized_copy_result_i32_candidate =
        contains_copy_result_i32_variant_candidate(&canonical);
    let materialized_session_candidate = contains_session_channel_candidate(&canonical);
    let map_root_composition_blockers = unsupported_map_root_composition_reasons(
        admission,
        materialized_collection_operation_candidate,
        &canonical,
    );
    if let Some(error) = map_owner_composition_failure(admission, &map_root_composition_blockers) {
        return Err(error);
    }
    if admission.scalar_ffi {
        // A scalar FFI call does not opt the whole graph into every other
        // migrated island. Keep these intersection gates in the shared
        // materializer so CLI, direct native and public verifiers reject the
        // same graph.
        if admission.record != FlatCopyRecordAdmission::OutsideProfile
            || materialized_record_candidate
        {
            return Err(CanonicalMirRouteMaterializationError::Complete {
                profile: CanonicalMirRouteProfile::ScalarFfi,
                stage: CanonicalMirRouteFailureStage::Coverage,
                message: "scalar FFI composition contains a flat Copy-record candidate outside the closed FFI profile".into(),
            });
        }
        if admission.flow != S8FlowAdmission::OutsideProfile
            || materialized_flow_candidate
            || !canonical.transitions().is_empty()
        {
            return Err(CanonicalMirRouteMaterializationError::Complete {
                profile: CanonicalMirRouteProfile::ScalarFfi,
                stage: CanonicalMirRouteFailureStage::Coverage,
                message: "scalar FFI composition contains a Flow transition candidate outside the closed FFI profile".into(),
            });
        }
        type IslandGate = fn(&MirProgram) -> Result<(), Vec<String>>;
        let gates: &[(bool, IslandGate)] = &[
            (
                materialized_collection_candidate,
                super::validate_scalar_collection_island,
            ),
            (
                materialized_option_string_candidate,
                super::validate_option_string_variant_island,
            ),
            (
                materialized_option_nested_tuple_candidate,
                super::validate_option_nested_tuple_variant_island,
            ),
            (
                materialized_managed_result_call_candidate,
                super::validate_managed_result_call_island,
            ),
            (
                materialized_copy_option_i32_candidate,
                super::validate_copy_option_i32_variant_island,
            ),
            (
                materialized_copy_option_i64_candidate,
                super::validate_copy_option_i64_variant_island,
            ),
            (
                materialized_copy_option_f64_candidate,
                super::validate_copy_option_f64_variant_island,
            ),
            (
                materialized_copy_result_i32_candidate,
                super::validate_copy_result_i32_variant_island,
            ),
            (materialized_copy_option_bool_candidate, |canonical| {
                super::validate_copy_option_variant_island(
                    canonical,
                    crate::core::PrimitiveType::Bool,
                    super::COPY_OPTION_BOOL_VARIANT_ISLAND,
                )
            }),
        ];
        for (candidate, gate) in gates {
            if *candidate {
                gate(&canonical).map_err(|errors| {
                    CanonicalMirRouteMaterializationError::Complete {
                        profile: CanonicalMirRouteProfile::ScalarFfi,
                        stage: CanonicalMirRouteFailureStage::Coverage,
                        message: format!(
                            "scalar FFI composition exceeds an existing island: {errors:?}"
                        ),
                    }
                })?;
            }
        }
    }
    if admission.collection_complete() && !materialized_collection_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::ScalarCollection,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message: "complete scalar collection admission did not materialize a native collection boundary"
                .into(),
        });
    }
    if admission.record_complete() && !materialized_record_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::FlatCopyRecord,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message:
                "complete flat Copy-record admission did not materialize a native record boundary"
                    .into(),
        });
    }
    if admission.flow_complete() && !materialized_flow_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::S8FlowTransition,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message: "complete S8 Flow admission did not materialize a FlowTransition boundary"
                .into(),
        });
    }
    if admission.flow_failure_retry && !materialized_flow_failure_retry_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::FlowFailureRetry,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message: "complete recoverable Flow admission did not materialize a recoverable FlowTransition boundary".into(),
        });
    }
    if admission.option_string_complete() && !materialized_option_string_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::NonCopyOptionStringVariant,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message: "complete Option<string> admission did not materialize a variant boundary"
                .into(),
        });
    }
    if admission.option_nested_tuple_complete() && !materialized_option_nested_tuple_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::NonCopyOptionNestedTupleVariant,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message:
                "complete nested Option tuple admission did not materialize a nested tuple receipt"
                    .into(),
        });
    }
    if admission.generic_variant_complete() && !materialized_generic_variant_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::GenericOptionPredicate,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message:
                "complete generic variant predicate admission did not materialize a VariantPredicate instance"
                    .into(),
        });
    }
    if admission.generic_option_projection_complete()
        && !materialized_generic_option_projection_candidate
    {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::GenericOptionProjection,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message:
                "complete generic Option projection admission did not materialize a VariantProject instance"
                    .into(),
        });
    }
    if admission.generic_result_projection_complete()
        && !materialized_generic_result_projection_candidate
    {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::GenericResultProjection,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message:
                "complete generic Result projection admission did not materialize a VariantProject instance"
                    .into(),
        });
    }
    if admission.generic_result_projection_fallback_complete()
        && !materialized_generic_result_projection_fallback_candidate
    {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::GenericResultProjectionFallback,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message:
                "complete generic Result fallback projection admission did not materialize a VariantProjectOr instance"
                    .into(),
        });
    }
    if admission.managed_result_call_complete() && !materialized_managed_result_call_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::ManagedResultCall,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message:
                "complete managed Result direct-call admission did not materialize a call ABI receipt"
                    .into(),
        });
    }
    if admission.generic_option_projection_fallback_complete()
        && !materialized_generic_option_projection_fallback_candidate
    {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::GenericOptionProjectionFallback,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message:
                "complete generic Option fallback projection admission did not materialize a VariantProjectOr instance"
                    .into(),
        });
    }
    if admission.copy_option_i32_complete() && !materialized_copy_option_i32_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::CopyOptionI32Variant,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message:
                "complete Copy Option<i32> admission did not materialize a VariantProject boundary"
                    .into(),
        });
    }
    if admission.copy_option_bool_complete() && !materialized_copy_option_bool_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::CopyOptionBoolVariant,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message:
                "complete Copy Option<bool> admission did not materialize a VariantProject boundary"
                    .into(),
        });
    }
    if admission.copy_option_i64_complete() && !materialized_copy_option_i64_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::CopyOptionI64Variant,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message:
                "complete Copy Option<i64> admission did not materialize a VariantProject boundary"
                    .into(),
        });
    }
    if admission.copy_option_f64_complete() && !materialized_copy_option_f64_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::CopyOptionF64Variant,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message:
                "complete Copy Option<f64> admission did not materialize a VariantProject boundary"
                    .into(),
        });
    }
    if admission.copy_result_i32_complete() && !materialized_copy_result_i32_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::CopyResultI32Variant,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message:
                "complete Copy Result<i32, i32> admission did not materialize a VariantProject boundary"
                    .into(),
        });
    }
    if admission.session_complete() && !materialized_session_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::SessionChannel,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message:
                "complete session-channel admission did not materialize a session operation receipt"
                    .into(),
        });
    }

    Ok(CanonicalMirRouteMaterialization {
        program: canonical,
        admission,
        materialized_map_root_candidate,
        materialized_map_any_owner_candidate,
        materialized_ordinary_map_owner_lifecycle_i32_string_v1_candidate,
        materialized_ordinary_map_owned_get_i32_string_v1_candidate,
        materialized_scalar_export_candidate,
        materialized_scalar_ffi_candidate,
        materialized_scalar_nested_candidate,
        materialized_scalar_actor_candidate: false,
        materialized_scalar_generic_identity_i32_candidate,
        materialized_scalar_generic_identity_i64_candidate,
        materialized_collection_candidate,
        materialized_collection_operation_candidate,
        materialized_record_candidate,
        materialized_flow_candidate,
        materialized_flow_failure_retry_candidate,
        materialized_option_string_candidate,
        materialized_option_nested_tuple_candidate,
        materialized_generic_variant_candidate,
        materialized_generic_option_projection_candidate,
        materialized_generic_option_projection_fallback_candidate,
        materialized_generic_result_projection_candidate,
        materialized_generic_result_projection_fallback_candidate,
        materialized_managed_result_call_candidate,
        materialized_copy_option_i32_candidate,
        materialized_copy_option_bool_candidate,
        materialized_copy_option_i64_candidate,
        materialized_copy_option_f64_candidate,
        materialized_copy_result_i32_candidate,
        materialized_session_candidate,
    })
}

/// Recognize the one scalar stdout effect that shares a value already owned
/// by a complete MapAnyOwner receipt: a single PrintlnInt consuming a direct
/// Copy/Clone/Move chain from Size, or a single PrintlnBool consuming the same
/// chain from Contains. A free-standing println, transformed value, second
/// output, or collection value stays in MixedCoverage and cannot borrow this
/// exception.
fn contains_exact_map_any_scalar_stdout(program: &MirProgram) -> bool {
    use crate::core::mir::types::{MirAbiClass, MirBuiltinKind, MirOwnership};
    use crate::core::mir::{MirInstructionKind, MirManagedAnyOperation, MirValueId};

    let mut stdout_count = 0usize;
    let mut matched_size_stdout = false;
    for function in program.functions().values() {
        let size_results = function
            .ownership
            .checker_map_any_owners
            .iter()
            .filter(|action| {
                action.profile_version == crate::core::MapAnyOwnerProfileVersion::V1
                    && action.kind == crate::core::MapAnyOwnerActionKind::Size
            })
            .filter_map(|action| action.result.clone())
            .collect::<Vec<_>>();
        let contains_results = function
            .ownership
            .checker_map_any_owners
            .iter()
            .filter(|action| {
                action.profile_version == crate::core::MapAnyOwnerProfileVersion::V1
                    && action.kind == crate::core::MapAnyOwnerActionKind::Contains
            })
            .filter_map(|action| action.result.clone())
            .collect::<Vec<_>>();
        let map_any_scalar_results = size_results
            .iter()
            .chain(contains_results.iter())
            .cloned()
            .collect::<Vec<_>>();
        let size_value_origin = |value: &MirValueId| {
            let mut current = value.clone();
            let mut seen = Vec::new();
            for _ in 0..=function.values.len() {
                if map_any_scalar_results.contains(&current) {
                    return true;
                }
                if seen.contains(&current) {
                    return false;
                }
                seen.push(current.clone());
                let mut sources = function
                    .blocks
                    .values()
                    .flat_map(|block| block.instructions.iter())
                    .filter_map(|instruction| match &instruction.kind {
                        MirInstructionKind::Clone { result, source }
                        | MirInstructionKind::Copy { result, source }
                        | MirInstructionKind::Move { result, source }
                            if result == &current =>
                        {
                            Some(source.clone())
                        }
                        _ => None,
                    });
                let Some(source) = sources.next() else {
                    return false;
                };
                if sources.next().is_some() {
                    return false;
                }
                current = source;
            }
            false
        };
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                let (kind, arguments, has_string_field_contract) = match &instruction.kind {
                    MirInstructionKind::BuiltinCall {
                        kind:
                            kind @ (MirBuiltinKind::PrintlnBool
                            | MirBuiltinKind::PrintlnInt
                            | MirBuiltinKind::PrintlnFloat
                            | MirBuiltinKind::PrintlnString),
                        arguments,
                        string_field_contract,
                        ..
                    } => (*kind, arguments, string_field_contract.is_some()),
                    MirInstructionKind::ManagedAny {
                        operation: MirManagedAnyOperation::Println,
                        ..
                    } => {
                        stdout_count += 1;
                        continue;
                    }
                    _ => continue,
                };
                stdout_count += 1;
                if has_string_field_contract || arguments.len() != 1 {
                    continue;
                }
                let expected_results = match kind {
                    MirBuiltinKind::PrintlnInt => &size_results,
                    MirBuiltinKind::PrintlnBool => &contains_results,
                    _ => continue,
                };
                let argument = &arguments[0];
                let Some(argument_value) = function.values.get(argument) else {
                    continue;
                };
                let Some(argument_type) = program.type_catalog().get(&argument_value.ty) else {
                    continue;
                };
                let expected_abi = match kind {
                    MirBuiltinKind::PrintlnInt => MirAbiClass::Integer {
                        bits: 32,
                        signed: true,
                    },
                    MirBuiltinKind::PrintlnBool => MirAbiClass::Bool,
                    _ => continue,
                };
                if argument_type.abi != expected_abi
                    || argument_type.ownership != MirOwnership::Copy
                {
                    continue;
                }

                for scalar_result in expected_results {
                    let Some(size_value) = function.values.get(scalar_result) else {
                        continue;
                    };
                    let Some(size_type) = program.type_catalog().get(&size_value.ty) else {
                        continue;
                    };
                    if size_type.abi != expected_abi
                        || size_type.ownership != MirOwnership::Copy
                        || size_value.ty != argument_value.ty
                    {
                        continue;
                    }

                    matched_size_stdout |= size_value_origin(argument);
                }
            }
        }
    }
    stdout_count == 1 && matched_size_stdout
}

fn unsupported_map_root_composition_reasons(
    admission: CanonicalMirRouteAdmission,
    materialized_collection_operation_candidate: bool,
    canonical: &MirProgram,
) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if admission.scalar_ffi {
        reasons.push("scalar FFI");
    }
    if admission.scalar_generic_identity_i32_complete() {
        reasons.push("scalar generic identity i32");
    }
    if admission.scalar_generic_identity_i64_complete() {
        reasons.push("scalar generic identity i64");
    }
    if materialized_collection_operation_candidate {
        reasons.push("materialized List/Set operation");
    }
    // Complete Map owner receipts may carry only their exact scalar stdout
    // tail through MixedCoverage. The older typed owned-get profile owns its
    // heterogeneous values list; MapAnyOwner admits only a direct PrintlnInt
    // of its receipted Size result. Both paths keep bare List/Set values and
    // materialized collection operations outside the exception.
    let ordinary_owned_get_scalar_tail = admission.ordinary_map_owned_get_i32_string_v1_complete()
        && !materialized_collection_operation_candidate
        && !contains_unadmitted_collection_materialization(
            canonical,
            admission.ordinary_map_owned_get_i32_string_v1_complete()
                && contains_ordinary_map_owned_get_values_instruction(canonical),
        );
    let map_any_owner_scalar_tail = admission.map_any_owner_complete()
        && !materialized_collection_operation_candidate
        && !contains_unadmitted_collection_materialization(canonical, false)
        && contains_exact_map_any_scalar_stdout(canonical);
    if !(ordinary_owned_get_scalar_tail || map_any_owner_scalar_tail)
        && !matches!(
            admission.collection,
            ScalarCollectionAdmission::OutsideProfile
        )
        && !admission.collection_complete()
    {
        reasons.push("mixed scalar-collection admission");
    }
    if !matches!(admission.record, FlatCopyRecordAdmission::OutsideProfile) {
        reasons.push("flat Copy-record profile");
    }
    if !matches!(admission.flow, S8FlowAdmission::OutsideProfile) {
        reasons.push("Flow profile");
    }
    if admission.flow_failure_retry {
        reasons.push("Flow failure-retry profile");
    }
    if !matches!(
        admission.option_string,
        OptionStringVariantAdmission::OutsideProfile
    ) {
        reasons.push("Option<string> profile");
    }
    if !matches!(
        admission.option_nested_tuple,
        OptionNestedTupleVariantAdmission::OutsideProfile
    ) {
        reasons.push("nested Option tuple profile");
    }
    if !matches!(
        admission.generic_variant,
        GenericVariantPredicateAdmission::OutsideProfile
    ) {
        reasons.push("generic variant profile");
    }
    if !matches!(
        admission.generic_option_projection,
        GenericOptionProjectionAdmission::OutsideProfile
    ) {
        reasons.push("generic Option projection profile");
    }
    if !matches!(
        admission.generic_option_projection_fallback,
        GenericOptionProjectionFallbackAdmission::OutsideProfile
    ) {
        reasons.push("generic Option fallback profile");
    }
    if !matches!(
        admission.generic_result_projection,
        GenericResultProjectionAdmission::OutsideProfile
    ) {
        reasons.push("generic Result projection profile");
    }
    if !matches!(
        admission.generic_result_projection_fallback,
        GenericResultProjectionFallbackAdmission::OutsideProfile
    ) {
        reasons.push("generic Result fallback profile");
    }
    if !matches!(
        admission.managed_result_call,
        ManagedResultCallAdmission::OutsideProfile
    ) {
        reasons.push("managed Result call profile");
    }
    if !matches!(
        admission.copy_option_i32,
        CopyOptionI32VariantAdmission::OutsideProfile
    ) {
        reasons.push("Copy Option<i32> profile");
    }
    if !matches!(
        admission.copy_option_bool,
        CopyOptionI32VariantAdmission::OutsideProfile
    ) {
        reasons.push("Copy Option<bool> profile");
    }
    if !matches!(
        admission.copy_option_i64,
        CopyOptionI32VariantAdmission::OutsideProfile
    ) {
        reasons.push("Copy Option<i64> profile");
    }
    if !matches!(
        admission.copy_option_f64,
        CopyOptionI32VariantAdmission::OutsideProfile
    ) {
        reasons.push("Copy Option<f64> profile");
    }
    if !matches!(
        admission.copy_result_i32,
        CopyResultI32VariantAdmission::OutsideProfile
    ) {
        reasons.push("Copy Result<i32, i32> profile");
    }
    if !matches!(admission.session, SessionChannelAdmission::OutsideProfile) {
        reasons.push("session-channel profile");
    }
    reasons
}

fn map_owner_composition_failure(
    admission: CanonicalMirRouteAdmission,
    blockers: &[&'static str],
) -> Option<CanonicalMirRouteMaterializationError> {
    if blockers.is_empty() {
        return None;
    }
    let (profile, label) = if admission.ordinary_map_owned_get_i32_string_v1_complete() {
        (
            CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1,
            "ordinary typed owned-get Map profile",
        )
    } else if admission.ordinary_map_owner_lifecycle_i32_string_v1_complete() {
        (
            CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1,
            "ordinary Map owner lifecycle profile",
        )
    } else if admission.map_any_owner_complete() {
        (
            CanonicalMirRouteProfile::MapAnyOwnerV1,
            "MapAnyOwner v1 profile",
        )
    } else if admission.map_root_complete() {
        (CanonicalMirRouteProfile::MapRoot, "MapRoot profile")
    } else {
        return None;
    };
    Some(CanonicalMirRouteMaterializationError::Complete {
        profile,
        stage: CanonicalMirRouteFailureStage::Coverage,
        message: format!("{label} cannot be combined with another candidate: {blockers:?}"),
    })
}

/// The same Map-owner composition hard gate used after materialization,
/// exposed for default dispatchers that must decide before an unrelated
/// compatibility disposition can hide the typed owner combination.
pub fn scalar_ffi_map_owner_composition_failure(
    admission: CanonicalMirRouteAdmission,
) -> Option<CanonicalMirRouteMaterializationError> {
    if admission.scalar_ffi {
        map_owner_composition_failure(admission, &["scalar FFI"])
    } else {
        None
    }
}

fn contains_ordinary_map_owner_lifecycle_i32_string_v1_candidate(program: &MirProgram) -> bool {
    program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleNew { .. }
                        | crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleSet { .. }
                        | crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleRemove { .. }
                        | crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleSize { .. }
                        | crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleContains { .. }
                        | crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { .. }
                )
            })
        })
    })
}

fn contains_ordinary_map_owned_get_i32_string_v1_candidate(program: &MirProgram) -> bool {
    program.functions().values().any(|function| {
        function
            .ownership
            .has_checker_ordinary_map_owned_get_i32_string_v1_actions()
            && function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        &instruction.kind,
                        crate::core::mir::MirInstructionKind::OrdinaryMapOwnedGetI32StringV1New { .. }
                            | crate::core::mir::MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Set { .. }
                            | crate::core::mir::MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Remove { .. }
                            | crate::core::mir::MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Size { .. }
                            | crate::core::mir::MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Contains { .. }
                            | crate::core::mir::MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetI32 { .. }
                            | crate::core::mir::MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetString { .. }
                            | crate::core::mir::MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
                            | crate::core::mir::MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. }
                            | crate::core::mir::MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Drop { .. }
                    )
                })
            })
    })
}

fn contains_ordinary_map_owned_get_values_instruction(program: &MirProgram) -> bool {
    program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values {
                        ..
                    }
                )
            })
        })
    })
}

/// Check the owner produced by the ordinary Map `map_get`/`values` face.
///
/// The Checker receipt records the operation and the Map root lifecycle, but
/// it does not by itself prove that the aggregate result was subsequently
/// settled.  A projection (`pair.0` or `snapshot.len()`) is a read and keeps
/// the aggregate owner live.  The canonical route therefore requires an
/// explicit drop, a consuming move/call/construct, or an exact return/fault
/// transfer on every reachable path.  Scope cleanup is deliberately not
/// accepted here: its synthetic `drop.managed_scope:*` instruction is a
/// compatibility cleanup and cannot serve as the owner-result receipt.
///
/// This check is shared by default route materialization and direct `--mir`
/// construction through the public MIR helper below.  Keep it independent of
/// a backend so bytecode, native and verifier callers cannot disagree about
/// the owner boundary.
pub fn ordinary_map_owned_get_result_settlement_reason(program: &MirProgram) -> Option<String> {
    for function in program.functions().values() {
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                let (result, label) = match &instruction.kind {
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny {
                        result,
                        ..
                    } => (result, "map_get"),
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values {
                        result,
                        ..
                    } => (result, "values"),
                    _ => continue,
                };
                if ordinary_map_owned_get_result_unsettled_on_any_path(function, result) {
                    return Some(format!(
                        "ordinary Map owned-get result '{}' from {} is not settled on every canonical MIR path; explicit drop, ownership transfer, or return is required",
                        result, label,
                    ));
                }
            }
        }
    }
    None
}

/// Apply the same owner-result check to a checked program for verifier callers
/// that have not selected a canonical route yet (notably the no-Z3 public
/// verifier APIs).  Construction failures remain the responsibility of the
/// normal route boundary; this helper only reports an observed unsettled
/// `GetAny`/`Values` result.
pub fn ordinary_map_owned_get_result_settlement_reason_checked(
    program: &CheckedProgram,
) -> Option<String> {
    let admission = classify_canonical_mir_route_admission(program);
    if !admission.ordinary_map_owned_get_i32_string_v1_complete() {
        return None;
    }
    if let Some(message) = ordinary_map_owned_get_source_result_settlement_reason(program) {
        return Some(message);
    }
    let excluded_sources = program
        .source_registry()
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<HashSet<_>>();
    let canonical =
        MirProgram::from_checked_program_excluding_sources(program, &excluded_sources).ok()?;
    ordinary_map_owned_get_result_settlement_reason(&canonical)
}

/// Check the source-owned result receipts before a checked program is lowered
/// to MIR.  This is the authoritative L1 boundary for `map_get`/`values`:
/// synthetic scope cleanup is retained as evidence, but it cannot discharge
/// the aggregate result owner.  The MIR alias walk remains a second L2/L3
/// check after lowering.
pub fn ordinary_map_owned_get_source_result_settlement_reason(
    program: &CheckedProgram,
) -> Option<String> {
    for (owner, analysis) in program.resource_analyses() {
        let Some(receipt) = analysis
            .ordinary_map_owned_get_i32_string_v1_cfg_receipt
            .as_ref()
        else {
            continue;
        };
        for result in &receipt.result_receipts {
            if !result.settlement.is_receipted() {
                return Some(format!(
                    "Checker source receipt for ordinary Map owned-get result '{}' in '{}' is not settled: '{}' rather than an explicit drop, ownership transfer, or return",
                    result.result.0,
                    owner.0,
                    result.settlement.as_str(),
                ));
            }
        }
    }
    None
}

fn ordinary_map_owned_get_result_unsettled_on_any_path(
    function: &crate::core::mir::MirFunction,
    root: &crate::core::mir::MirValueId,
) -> bool {
    let mut pending = vec![(function.entry.clone(), BTreeSet::from([root.clone()]))];
    let mut seen = BTreeSet::new();
    let mut explored = 0usize;

    while let Some((block_id, mut aliases)) = pending.pop() {
        if !seen.insert((block_id.clone(), aliases.clone())) {
            continue;
        }
        explored += 1;
        // A malformed/hostile CFG must not turn route admission into an
        // unbounded search.  Failing closed preserves the ownership gate.
        if explored > 4096 {
            return true;
        }
        let Some(block) = function.blocks.get(&block_id) else {
            return true;
        };

        for instruction in &block.instructions {
            match &instruction.kind {
                crate::core::mir::MirInstructionKind::Move { result, source }
                    if aliases.remove(source) =>
                {
                    aliases.insert(result.clone());
                }
                crate::core::mir::MirInstructionKind::Clone { result, source }
                    if aliases.contains(source)
                        && !instruction.id.as_str().contains("list-borrow") =>
                {
                    aliases.insert(result.clone());
                }
                crate::core::mir::MirInstructionKind::Drop { value }
                    if (instruction.id.as_str().contains("stmt.drop")
                        || instruction.id.as_str().contains("list-borrow"))
                        && aliases.remove(value) => {}
                crate::core::mir::MirInstructionKind::ManagedAny {
                    source,
                    operation: crate::core::mir::MirManagedAnyOperation::Drop,
                    ..
                } if aliases.remove(source) => {}
                crate::core::mir::MirInstructionKind::MoveProject { base, .. }
                | crate::core::mir::MirInstructionKind::MoveProjectDrop { base, .. }
                | crate::core::mir::MirInstructionKind::VariantProjectMove { base, .. }
                    if aliases.remove(base) =>
                {
                    // These operations consume the aggregate base.  The
                    // owned-get tuple/list profiles have no field-level
                    // projection receipt that can recreate the owner here.
                }
                crate::core::mir::MirInstructionKind::Call {
                    result, arguments, ..
                }
                | crate::core::mir::MirInstructionKind::ClosureInvoke {
                    result, arguments, ..
                } => {
                    let consumed = arguments.iter().any(|argument| aliases.contains(argument));
                    if consumed {
                        aliases.retain(|alias| !arguments.contains(alias));
                        if let Some(result) = result {
                            aliases.insert(result.clone());
                        }
                    }
                }
                crate::core::mir::MirInstructionKind::FlowTransition {
                    result, arguments, ..
                } => {
                    let consumed = arguments.iter().any(|argument| aliases.contains(argument));
                    if consumed {
                        aliases.retain(|alias| !arguments.contains(alias));
                        aliases.insert(result.clone());
                    }
                }
                crate::core::mir::MirInstructionKind::BuiltinCall {
                    result,
                    arguments,
                    string_field_contract,
                    ..
                } if string_field_contract.is_none() => {
                    let consumed = arguments.iter().any(|argument| aliases.contains(argument));
                    if consumed {
                        aliases.retain(|alias| !arguments.contains(alias));
                        aliases.insert(result.clone());
                    }
                }
                crate::core::mir::MirInstructionKind::Construct { result, fields, .. }
                | crate::core::mir::MirInstructionKind::ConstructList {
                    result,
                    elements: fields,
                    ..
                }
                | crate::core::mir::MirInstructionKind::ConstructSet {
                    result,
                    elements: fields,
                } => {
                    if fields.iter().any(|field| aliases.contains(field)) {
                        aliases.retain(|alias| !fields.contains(alias));
                        aliases.insert(result.clone());
                    }
                }
                crate::core::mir::MirInstructionKind::ConstructVariant {
                    result, fields, ..
                }
                | crate::core::mir::MirInstructionKind::ConstructVariantMove {
                    result,
                    fields,
                    ..
                } => {
                    if fields.iter().any(|(_, field)| aliases.contains(field)) {
                        aliases.retain(|alias| !fields.iter().any(|(_, field)| field == alias));
                        aliases.insert(result.clone());
                    }
                }
                crate::core::mir::MirInstructionKind::UpdateRecord {
                    result,
                    base,
                    fields,
                    ..
                } => {
                    let consumed = aliases.contains(base)
                        || fields.iter().any(|field| aliases.contains(field));
                    if consumed {
                        aliases.remove(base);
                        aliases.retain(|alias| !fields.contains(alias));
                        aliases.insert(result.clone());
                    }
                }
                crate::core::mir::MirInstructionKind::ListOp {
                    result,
                    operation: crate::core::mir::MirListOperation::Concat,
                    list,
                    argument: Some(argument),
                    ..
                } if aliases.contains(list) || aliases.contains(argument) => {
                    aliases.remove(list);
                    aliases.remove(argument);
                    aliases.insert(result.clone());
                }
                _ => {}
            }
        }

        match &block.terminator {
            crate::core::mir::MirTerminator::Goto {
                target, arguments, ..
            } => enqueue_owned_get_edge(function, &mut pending, target, arguments, &aliases),
            crate::core::mir::MirTerminator::Branch {
                then_target,
                then_arguments,
                else_target,
                else_arguments,
                ..
            } => {
                enqueue_owned_get_edge(
                    function,
                    &mut pending,
                    then_target,
                    then_arguments,
                    &aliases,
                );
                enqueue_owned_get_edge(
                    function,
                    &mut pending,
                    else_target,
                    else_arguments,
                    &aliases,
                );
            }
            crate::core::mir::MirTerminator::Switch { arms, .. }
            | crate::core::mir::MirTerminator::SwitchMove { arms, .. } => {
                for arm in arms {
                    enqueue_owned_get_edge(
                        function,
                        &mut pending,
                        &arm.target,
                        &arm.arguments,
                        &aliases,
                    );
                }
            }
            crate::core::mir::MirTerminator::Return { value }
            | crate::core::mir::MirTerminator::Fault { value } => {
                if value.as_ref().is_some_and(|value| aliases.contains(value)) {
                    aliases.remove(value.as_ref().expect("value checked above"));
                }
                if !aliases.is_empty() {
                    return true;
                }
            }
            crate::core::mir::MirTerminator::Trap { .. }
            | crate::core::mir::MirTerminator::Unreachable => {
                if !aliases.is_empty() {
                    return true;
                }
            }
        }
    }
    false
}

fn enqueue_owned_get_edge(
    function: &crate::core::mir::MirFunction,
    pending: &mut Vec<(
        crate::core::mir::MirBlockId,
        BTreeSet<crate::core::mir::MirValueId>,
    )>,
    target: &crate::core::mir::MirBlockId,
    arguments: &[crate::core::mir::MirValueId],
    aliases: &BTreeSet<crate::core::mir::MirValueId>,
) {
    let mut next = aliases.clone();
    let Some(block) = function.blocks.get(target) else {
        pending.push((target.clone(), next));
        return;
    };
    for (parameter, argument) in block.parameters.iter().zip(arguments) {
        if next.remove(argument) {
            next.insert(parameter.value.clone());
        }
    }
    pending.push((target.clone(), next));
}

/// Return true when a canonical ordinary owned-get graph materializes a
/// collection value without an admitted collection profile.  A complete
/// Values receipt owns both the checker `List<DynamicAny>` descriptor and its
/// executable `List<ManagedAny>` successor; every other List/Set remains a
/// separate ownership boundary and is rejected here. Checking both the
/// TypeDesc catalog and instruction stream keeps the gate closed for a bare
/// literal as well as for an operation candidate.
fn contains_unadmitted_collection_materialization(
    program: &MirProgram,
    allow_managed_any_values_list: bool,
) -> bool {
    let catalog_contains_collection = program.type_catalog().iter().any(|(ty, descriptor)| {
        // The canonical catalog retains the original checker `List<DynamicAny>`
        // descriptor as well as the executable `List<ManagedAny>` identity.
        // Both are one Values receipt's type graph; permitting only the latter
        // would make a complete Values body look like an unrelated collection.
        let values_list = allow_managed_any_values_list
            && is_managed_any_values_list_type(program, ty, descriptor);
        matches!(
            &descriptor.kind,
            crate::core::mir::types::MirTypeKind::List | crate::core::mir::types::MirTypeKind::Set
        ) && !values_list
    });
    if catalog_contains_collection {
        return true;
    }
    program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::ConstructList { .. }
                        | crate::core::mir::MirInstructionKind::ConstructSet { .. }
                )
            })
        })
    })
}

fn is_managed_any_values_list_type(
    program: &MirProgram,
    ty: &crate::core::ir::ResolvedTypeId,
    descriptor: &crate::core::mir::types::MirTypeDesc,
) -> bool {
    // A source `List<DynamicAny>` is exempt only when this exact source type
    // has a checker-receipted synthetic `List<ManagedAny>` successor.  The
    // synthetic id is content-hashed, so compare through the catalog's
    // canonicalization instead of guessing from an id prefix.  This keeps an
    // unrelated bare `List<DynamicAny>` in a typed-get program on the mixed
    // collection rejection path.
    let canonical = program.type_catalog().canonical_managed_any_type(ty);
    let paired_source = canonical != *ty
        && program
            .type_catalog()
            .validate_managed_any_list_type(&canonical)
            .is_ok();
    let canonical_list = program
        .type_catalog()
        .validate_managed_any_list_type(ty)
        .is_ok();
    let _ = descriptor;
    paired_source || canonical_list
}

/// The `values()` result is a move-owned `List<ManagedAny>`, so its `len()`
/// read is part of the ordinary Map owned-get receipt rather than the scalar
/// Copy-list island. Keep the collection composition gate closed for every
/// other List/Set operation and generic instance.
fn contains_only_managed_any_values_list_len(program: &MirProgram) -> bool {
    let mut saw_values_len = false;
    for function in program.functions().values() {
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                match &instruction.kind {
                    crate::core::mir::MirInstructionKind::ListOp {
                        operation: crate::core::mir::MirListOperation::Len,
                        list,
                        ..
                    } => {
                        let is_values_list = function.values.get(list).is_some_and(|value| {
                            program
                                .type_catalog()
                                .get(&value.ty)
                                .is_some_and(|descriptor| {
                                    is_managed_any_values_list_type(program, &value.ty, descriptor)
                                })
                        });
                        if !is_values_list {
                            return false;
                        }
                        saw_values_len = true;
                    }
                    crate::core::mir::MirInstructionKind::ListOp { .. }
                    | crate::core::mir::MirInstructionKind::SetOp { .. } => return false,
                    crate::core::mir::MirInstructionKind::Project {
                        list_index_contract: Some(contract),
                        ..
                    } if contract.mode
                        == crate::core::mir::types::MirListIndexProjectionMode::CloneNestedList =>
                    {
                        return false;
                    }
                    _ => {}
                }
            }
        }
    }
    if program.instances().values().any(|instance| {
        matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarSetFacade { .. }
                | crate::core::mir::MirGenericInstanceContract::ScalarListFacade { .. }
                | crate::core::mir::MirGenericInstanceContract::ScalarListConstruct { .. }
                | crate::core::mir::MirGenericInstanceContract::ScalarListProjection { .. }
        )
    }) {
        return false;
    }
    saw_values_len
}

fn match_complete_or_compatibility(
    admission: CanonicalMirRouteAdmission,
    stage: CanonicalMirRouteFailureStage,
    message: String,
) -> CanonicalMirRouteMaterializationError {
    if admission
        .ordinary_map_owned_get_i32_string_v1
        .is_candidate()
    {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1,
            stage,
            message,
        }
    } else if admission
        .ordinary_map_owner_lifecycle_i32_string_v1
        .is_candidate()
    {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1,
            stage,
            message,
        }
    } else if admission.map_any_owner.is_candidate() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::MapAnyOwnerV1,
            stage,
            message,
        }
    } else if admission.map_root.is_candidate() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::MapRoot,
            stage,
            message,
        }
    } else if admission.scalar_ffi {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::ScalarFfi,
            stage,
            message,
        }
    } else if admission.scalar_nested {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::ScalarNested,
            stage,
            message,
        }
    } else if admission.scalar_generic_identity_i32_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::ScalarGenericIdentityI32,
            stage,
            message,
        }
    } else if admission.scalar_generic_identity_i64_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::ScalarGenericIdentityI64,
            stage,
            message,
        }
    } else if admission.flow_failure_retry {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::FlowFailureRetry,
            stage,
            message,
        }
    } else if admission.managed_result_call_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::ManagedResultCall,
            stage,
            message,
        }
    } else if admission.collection_complete()
        && stage == CanonicalMirRouteFailureStage::Construction
    {
        // R6-1052 guard: the scalar collection admission is a checker-side
        // type/coverage scan, while construction is the ground truth for the
        // executable statement surface (for example an assignment nested in
        // a block is outside the MIR Phase 0 scalar-assign face).  A shape
        // construction cannot lower is precisely a compatibility input: the
        // working legacy route stays alive for it, and the lowering error
        // travels in the compatibility message instead of becoming a hard
        // reject of a program that ran before this slice.
        CanonicalMirRouteMaterializationError::Compatibility { admission, message }
    } else if admission.collection_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::ScalarCollection,
            stage,
            message,
        }
    } else if admission.record_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::FlatCopyRecord,
            stage,
            message,
        }
    } else if admission.flow_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::S8FlowTransition,
            stage,
            message,
        }
    } else if admission.option_string_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::NonCopyOptionStringVariant,
            stage,
            message,
        }
    } else if admission.option_nested_tuple_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::NonCopyOptionNestedTupleVariant,
            stage,
            message,
        }
    } else if admission.generic_variant_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::GenericOptionPredicate,
            stage,
            message,
        }
    } else if admission.generic_option_projection_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::GenericOptionProjection,
            stage,
            message,
        }
    } else if admission.generic_result_projection_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::GenericResultProjection,
            stage,
            message,
        }
    } else if admission.generic_result_projection_fallback_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::GenericResultProjectionFallback,
            stage,
            message,
        }
    } else if admission.generic_option_projection_fallback_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::GenericOptionProjectionFallback,
            stage,
            message,
        }
    } else if admission.copy_option_i32_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::CopyOptionI32Variant,
            stage,
            message,
        }
    } else if admission.copy_option_bool_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::CopyOptionBoolVariant,
            stage,
            message,
        }
    } else if admission.copy_option_i64_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::CopyOptionI64Variant,
            stage,
            message,
        }
    } else if admission.copy_option_f64_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::CopyOptionF64Variant,
            stage,
            message,
        }
    } else if admission.copy_result_i32_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::CopyResultI32Variant,
            stage,
            message,
        }
    } else if admission.session_complete() {
        CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::SessionChannel,
            stage,
            message,
        }
    } else {
        CanonicalMirRouteMaterializationError::Compatibility { admission, message }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{mir::MirInstructionKind, NodeId};

    fn checked(source: &str) -> CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        crate::core::check_program(&file).expect("typecheck")
    }

    #[test]
    fn scalar_generic_identity_admission_is_exact_and_materializes_a_closed_route() {
        let program =
            checked("func pass<T>(value: T) -> T { value }\nfunc main() -> i32 { pass(41) }");
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.scalar_generic_identity_i32,
            ScalarGenericIdentityAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("exact scalar generic identity route must materialize");
        assert!(CanonicalMirRouteProfile::ScalarGenericIdentityI32.is_admitted(admission));
        assert!(CanonicalMirRouteProfile::ScalarGenericIdentityI32.is_materialized(&route));
        crate::core::mir::validate_scalar_generic_identity_island(&route.program)
            .expect("materialized scalar generic identity route must validate");

        for source in [
            "func pass<T>(value: T) -> T { value }\nfunc main() -> bool { pass(true) }",
            "func pass<T>(value: T) -> T { value }\nfunc helper() -> i32 { 0 }\nfunc main() -> i32 { pass(41) }",
            "func pass<T>(value: T) -> T { value }\nfunc main() -> i32 { pass(pass(41)) }",
            "func pass<T>(value: T) -> T { value }\nfunc main() -> i32 { requires: true; pass(41) }",
        ] {
            let program = checked(source);
            assert_eq!(
                classify_scalar_generic_identity_admission(&program),
                ScalarGenericIdentityAdmission::OutsideProfile,
                "unsupported source shape must remain outside the exact identity profile: {source}"
            );
        }

        let i64_program = checked(
            "func pass<T>(value: T) -> T { value }\nfunc main() -> i64 { pass(2147483690) }",
        );
        let i64_admission = classify_canonical_mir_route_admission(&i64_program);
        assert_eq!(
            i64_admission.scalar_generic_identity_i32,
            ScalarGenericIdentityAdmission::OutsideProfile
        );
        assert_eq!(
            i64_admission.scalar_generic_identity_i64,
            ScalarGenericIdentityAdmission::CompleteCoverage
        );
        let i64_route = materialize_canonical_mir_route(&i64_program, None)
            .expect("exact i64 scalar generic identity route must materialize");
        assert!(CanonicalMirRouteProfile::ScalarGenericIdentityI64.is_admitted(i64_admission));
        assert!(CanonicalMirRouteProfile::ScalarGenericIdentityI64.is_materialized(&i64_route));
        crate::core::mir::validate_scalar_generic_identity_i64_island(&i64_route.program)
            .expect("materialized i64 scalar generic identity route must validate");

        for source in [
            "func pass<T>(value: T) -> T { value }\nfunc main() -> bool { pass(true) }",
            "func pass<T>(value: T) -> T { value }\nfunc helper() -> i64 { 0 }\nfunc main() -> i64 { pass(41) }",
            "func pass<T>(value: T) -> T { value }\nfunc main() -> i64 { pass(41) }",
            "func pass<T>(value: T) -> T { value }\nfunc main() -> i64 { pass(pass(41)) }",
            "func pass<T>(value: T) -> T { value }\nfunc main() -> i64 { requires: true; pass(2147483690) }",
        ] {
            let program = checked(source);
            assert_eq!(
                classify_scalar_generic_identity_i64_admission(&program),
                ScalarGenericIdentityAdmission::OutsideProfile,
                "unsupported source shape must remain outside the exact i64 identity profile: {source}"
            );
        }
    }

    #[test]
    fn ordinary_scalar_contract_admission_preserves_unmigrated_and_no_contract_faces() {
        let admitted = checked(
            "func yes(flag: bool) -> bool { ensures: result == flag
 flag } func main() -> i32 { if yes(true) { 42 } else { 0 } }",
        );
        let route =
            materialize_canonical_mir_route(&admitted, None).expect("scalar contract route");
        assert!(route.admission.collection_complete());
        assert!(route.materialized_collection_candidate);
        assert!(!route.materialized_collection_operation_candidate);
        for source in [
            "func main() -> i32 { 42 }",
            "func recur(value: i32) -> i32 { ensures: result >= 0
 if value == 0 { 0 } else { recur(value - 1) } } func main() -> i32 { 0 }",
            "func yes() -> bool { ensures: result == true
 true } func main() -> i32 { let mut n = 0
 while n < 2 { n = n + 1 }
 n }",
            "func yes() -> bool { ensures: result == true
 true } func text(value: string) -> string { value } func main() -> i32 { 0 }",
            "func lemma(value: i32) -> i32 { requires: value > 0
 math: { value > 0 }
 ensures: result > 0
 value } func main() -> i32 { 0 }",
            "func guarded(value: i32) -> i32 { requires: value > 0
 invariant: value > 0
 ensures: result > 0
 value } func main() -> i32 { 0 }",
        ] {
            assert_eq!(
                classify_canonical_mir_route_admission(&checked(source)).collection,
                ScalarCollectionAdmission::OutsideProfile,
                "a contract must not promote an unrelated compatibility body: {source}"
            );
        }
    }

    #[test]
    fn complete_scalar_collection_materialization_carries_one_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_list_len.mimi"
        ));
        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete collection route must materialize");
        assert_eq!(
            route.admission.collection,
            ScalarCollectionAdmission::CompleteCoverage
        );
        assert!(route.materialized_collection_candidate);
        assert!(!route.materialized_record_candidate);
        assert!(!route.materialized_flow_candidate);
        assert!(!route.materialized_option_string_candidate);
        assert!(!route.materialized_generic_variant_candidate);
    }

    #[test]
    fn complete_scalar_collection_preflight_policy_is_stage_specific() {
        // R6-1052 restatement of complete_scalar_collection_materialization_failure_is_hard:
        // the coverage scan is a checker-side type heuristic while
        // construction is the ground truth for the executable statement
        // surface, so a shape construction cannot lower is an explicit
        // compatibility input carrying the lowering error — never a hard
        // rejection of a previously working program.  A complete scan whose
        // construction succeeds but materializes no receipt at all stays a
        // hard Coverage failure (an internal scan/receipt inconsistency).
        let construction_failure = checked(
            r#"
                func main() -> i32 {
                    let mut total = 0
                    let flag = true
                    if flag { total = 5 }
                    total
                }
            "#,
        );
        let error = materialize_canonical_mir_route(&construction_failure, None)
            .expect_err("nested-assign construction failure must not route canonical");
        match error {
            CanonicalMirRouteMaterializationError::Compatibility { message, .. } => assert!(
                message.contains("assign inside a nested block"),
                "the compatibility message must carry the lowering error: {message}"
            ),
            other => panic!("construction failure must downgrade, got: {other:?}"),
        }

        // R6-1052 stdout gate: plain-scalar candidacy is stdout-gated. A body
        // with no scalar stdout effect and no collection operation never
        // forms a candidate, so its graph materializes with an empty receipt
        // set and the dispatcher's no-candidate veto keeps it on the legacy
        // route.  The Coverage-stage hard failure stays a defensive invariant
        // for receipt loss under a complete scan; aligned scans cannot
        // produce that shape from a real program, and the diagnostic
        // taxonomy test pins the stage's error code.
        let stdout_free = checked(
            r#"
                func main() -> i32 {
                    let total = 1 + 1
                    drop(total)
                    0
                }
            "#,
        );
        assert_eq!(
            crate::core::mir::classify_scalar_collection_admission(&stdout_free),
            ScalarCollectionAdmission::OutsideProfile
        );
        let route = materialize_canonical_mir_route(&stdout_free, None)
            .expect("a stdout-free plain-scalar body makes no canonical claim");
        assert!(!route.materialized_collection_candidate);
        assert!(!route.materialized_collection_operation_candidate);
        assert!(!route.materialized_scalar_ffi_candidate);
    }

    #[test]
    fn route_materialization_failures_expose_stage_specific_diagnostic_codes() {
        let construction = CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::ScalarCollection,
            stage: CanonicalMirRouteFailureStage::Construction,
            message: "builder rejected the admitted body".into(),
        };
        assert_eq!(
            construction.diagnostic_code(),
            Some(crate::core::mir::MIR_ROUTE_MATERIALIZATION_ERROR_CODE)
        );
        let construction_diagnostic = construction.to_diagnostic();
        assert_eq!(
            construction_diagnostic.code.as_deref(),
            Some(crate::core::mir::MIR_ROUTE_MATERIALIZATION_ERROR_CODE)
        );
        let origin = construction_diagnostic
            .origin
            .expect("hard route failure keeps runtime provenance");
        assert_eq!(origin.rule.as_deref(), Some("mir.route"));
        assert_eq!(construction_diagnostic.span, crate::span::Span::UNKNOWN);

        let coverage = CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::ScalarCollection,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message: "receipt omitted the admitted operation".into(),
        };
        assert_eq!(
            coverage.diagnostic_code(),
            Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        );
    }

    #[test]
    fn ordinary_map_owned_get_v1_has_precedence_and_materializes_its_exact_profile() {
        let program = checked(
            r#"
                func main() -> i32 {
                    let key = "answer"
                    let source = map_new()
                    let inserted = map_set(source, key, 17)
                    let integer = map_get_i32(inserted, key)
                    let with_text = map_set(inserted, key, "seventeen")
                    let text = map_get_string(with_text, key)
                    drop(source)
                    drop(inserted)
                    drop(with_text)
                    0
                }
            "#,
        );
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.ordinary_map_owned_get_i32_string_v1,
            OrdinaryMapOwnedGetI32StringV1Admission::CompleteCoverage
        );
        assert!(CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1.is_admitted(admission));
        assert_eq!(
            CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1.as_str(),
            ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND
        );
        assert_eq!(admission.map_root, MapRootAdmission::OutsideProfile);
        assert_eq!(
            admission.map_any_owner,
            MapAnyOwnerAdmission::OutsideProfile
        );
        assert_eq!(
            admission.ordinary_map_owner_lifecycle_i32_string_v1,
            OrdinaryMapOwnerLifecycleI32StringV1Admission::OutsideProfile
        );

        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete independent typed-get receipt must materialize");
        assert!(CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1.is_materialized(&route));
        assert!(!route.materialized_map_root_candidate);
        assert!(!route.materialized_map_any_owner_candidate);
        assert!(!route.materialized_ordinary_map_owner_lifecycle_i32_string_v1_candidate);
    }

    #[test]
    fn ordinary_map_owner_keeps_persistent_roots_with_a_reused_dynamic_key() {
        let program = checked(
            r#"
                func main() -> i32 {
                    let key = "pre" + "fix"
                    let source = map_new()
                    let updated = map_set(source, key, 7)
                    let removed = map_remove(updated, key)
                    let present = has_key(source, key)
                    drop(source)
                    drop(updated)
                    drop(removed)
                    if present { 1 } else { 0 }
                }
            "#,
        );
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.ordinary_map_owner_lifecycle_i32_string_v1,
            OrdinaryMapOwnerLifecycleI32StringV1Admission::CompleteCoverage
        );
        assert_eq!(
            admission.map_any_owner,
            MapAnyOwnerAdmission::OutsideProfile
        );
        assert_eq!(admission.map_root, MapRootAdmission::OutsideProfile);

        let route = materialize_canonical_mir_route(&program, None)
            .expect("persistent ordinary Map roots retain the ordinary owner route");
        assert!(route.materialized_ordinary_map_owner_lifecycle_i32_string_v1_candidate);
        assert!(!route.materialized_map_any_owner_candidate);

        let function = route
            .program
            .functions()
            .get(&NodeId("function:main".into()))
            .expect("main MIR");
        function
            .validate()
            .expect("the settled prefix and scalar tail branch keep complete Map receipts");
        let has_key_receipt = function
            .ownership
            .checker_ordinary_map_owner_lifecycle_v1
            .iter()
            .position(|receipt| receipt.kind == OrdinaryMapOwnerLifecycleActionKind::HasKey)
            .expect("HasKey receipt from the original branched fixture");
        let mut forged_key_local = function.clone();
        forged_key_local
            .ownership
            .checker_ordinary_map_owner_lifecycle_v1[has_key_receipt]
            .key_local = Some(crate::core::ir::ResolvedLocalId(NodeId(
            "function:main/node:pattern.variable@forged/local".into(),
        )));
        assert!(
            forged_key_local.validate().is_err(),
            "a complete CFG route still rejects a key-local-only Checker receipt forgery"
        );
    }

    #[test]
    fn ordinary_map_owned_get_mixed_typed_and_values_materializes_complete_coverage() {
        let program = checked(
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let typed = map_get_i32(root, "answer")
                    let snapshot = values(root)
                    drop(snapshot)
                    drop(root)
                    0
                }
            "#,
        );
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.ordinary_map_owned_get_i32_string_v1,
            OrdinaryMapOwnedGetI32StringV1Admission::CompleteCoverage
        );
        assert!(admission
            .ordinary_map_owned_get_i32_string_v1
            .has_profile_signal());
        assert!(admission
            .ordinary_map_owned_get_i32_string_v1
            .is_candidate());
        assert!(CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1.is_admitted(admission));
        assert_eq!(admission.map_root, MapRootAdmission::OutsideProfile);
        assert_eq!(
            admission.map_any_owner,
            MapAnyOwnerAdmission::OutsideProfile
        );
        assert_eq!(
            admission.ordinary_map_owner_lifecycle_i32_string_v1,
            OrdinaryMapOwnerLifecycleI32StringV1Admission::OutsideProfile
        );

        let route = materialize_canonical_mir_route(&program, None)
            .expect("typed GetI32 and Values can share one owned-get root receipt");
        assert!(CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1.is_materialized(&route));
        let function = route
            .program
            .functions()
            .get(&NodeId("function:main".into()))
            .expect("mixed typed-get/Values MIR");
        function
            .validate()
            .expect("mixed typed-get/Values root lifecycle validates");
    }

    #[test]
    fn ordinary_map_owned_get_values_materializes_managed_any_list() {
        let program = checked(
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let snapshot = values(root)
                    let count = snapshot.len()
                    drop(snapshot)
                    drop(root)
                    count
                }
            "#,
        );
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.ordinary_map_owned_get_i32_string_v1,
            OrdinaryMapOwnedGetI32StringV1Admission::CompleteCoverage
        );
        assert_eq!(
            admission.ordinary_map_owner_lifecycle_i32_string_v1,
            OrdinaryMapOwnerLifecycleI32StringV1Admission::OutsideProfile
        );
        assert!(CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1.is_admitted(admission));

        let route = materialize_canonical_mir_route(&program, None)
            .expect("ordinary Map values must materialize its owned-get route");
        assert!(CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1.is_materialized(&route));
        assert!(!route.materialized_ordinary_map_owner_lifecycle_i32_string_v1_candidate);
        let function = route
            .program
            .functions()
            .get(&NodeId("function:main".into()))
            .expect("main MIR");
        function
            .validate()
            .expect("Values route MIR must satisfy structural validation");
        assert!(function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. }
                )
            })
        }));
    }

    #[test]
    fn ordinary_map_owned_get_values_scope_cleanup_does_not_settle_result_owner() {
        let program = checked(
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let snapshot = values(root)
                    let count = snapshot.len()
                    drop(root)
                    count
                }
            "#,
        );
        assert_eq!(
            classify_canonical_mir_route_admission(&program)
                .ordinary_map_owned_get_i32_string_v1,
            OrdinaryMapOwnedGetI32StringV1Admission::CompleteCoverage,
            "Values operation and Map root lifecycle are checker-complete even though the result owner is only scope-cleaned"
        );

        let error = materialize_canonical_mir_route(&program, None)
            .expect_err("scope cleanup must not discharge a Values result owner");
        match error {
            CanonicalMirRouteMaterializationError::Complete {
                profile: CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1,
                stage: CanonicalMirRouteFailureStage::Coverage,
                message,
            } => assert!(message.contains("not settled"), "{message}"),
            other => panic!("scope-only Values result must fail at coverage, got {other:?}"),
        }
    }

    #[test]
    fn ordinary_map_owned_get_any_materializes_a_get_any_only_candidate() {
        let program = checked(
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let pair = map_get(root, "answer")
                    let found = pair.0
                    drop(pair)
                    drop(root)
                    if found { 1 } else { 0 }
                }
            "#,
        );
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.ordinary_map_owned_get_i32_string_v1,
            OrdinaryMapOwnedGetI32StringV1Admission::CompleteCoverage,
            "GetAny must count as an owned-get profile action"
        );
        assert_eq!(
            admission.ordinary_map_owner_lifecycle_i32_string_v1,
            OrdinaryMapOwnerLifecycleI32StringV1Admission::OutsideProfile
        );

        let route = materialize_canonical_mir_route(&program, None)
            .expect("GetAny-only ordinary Map route must materialize");
        assert!(CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1.is_materialized(&route));
        let function = route
            .program
            .functions()
            .get(&NodeId("function:main".into()))
            .expect("main MIR");
        function
            .validate()
            .expect("GetAny-only route MIR must satisfy structural validation");
        assert!(function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
                )
            })
        }));
    }

    #[test]
    fn ordinary_map_owned_get_any_scope_cleanup_does_not_settle_result_owner() {
        let program = checked(
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let pair = map_get(root, "answer")
                    let found = pair.0
                    drop(root)
                    if found { 1 } else { 0 }
                }
            "#,
        );
        assert_eq!(
            classify_canonical_mir_route_admission(&program)
                .ordinary_map_owned_get_i32_string_v1,
            OrdinaryMapOwnedGetI32StringV1Admission::CompleteCoverage,
            "GetAny operation and Map root lifecycle are checker-complete even though the tuple owner is only scope-cleaned"
        );

        let error = materialize_canonical_mir_route(&program, None)
            .expect_err("scope cleanup must not discharge a GetAny result owner");
        match error {
            CanonicalMirRouteMaterializationError::Complete {
                profile: CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1,
                stage: CanonicalMirRouteFailureStage::Coverage,
                message,
            } => assert!(message.contains("not settled"), "{message}"),
            other => panic!("scope-only GetAny result must fail at coverage, got {other:?}"),
        }
    }

    #[test]
    fn ordinary_map_owner_v1_admits_persistent_set_and_remove_with_isolated_identity() {
        let program = checked(
            r#"
                func main() -> i32 {
                    let source = map_new()
                    let inserted = map_set(source, "answer", 42)
                    let removed = map_remove(inserted, "answer")
                    let source_size = map_size(source)
                    let inserted_size = map_size(inserted)
                    drop(source)
                    drop(inserted)
                    drop(removed)
                    source_size + inserted_size
                }
            "#,
        );
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.ordinary_map_owner_lifecycle_i32_string_v1,
            OrdinaryMapOwnerLifecycleI32StringV1Admission::CompleteCoverage
        );
        assert_eq!(
            admission.ordinary_map_owned_get_i32_string_v1,
            OrdinaryMapOwnedGetI32StringV1Admission::OutsideProfile
        );
        assert!(
            CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1.is_admitted(admission)
        );
        assert_eq!(
            CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1.as_str(),
            ORDINARY_MAP_OWNER_LIFECYCLE_I32_STRING_V1_ISLAND
        );
        assert_eq!(admission.map_root, MapRootAdmission::OutsideProfile);
        assert_eq!(
            admission.map_any_owner,
            MapAnyOwnerAdmission::OutsideProfile
        );

        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete ordinary Map lifecycle must materialize its own profile");
        assert!(
            CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1.is_materialized(&route)
        );
        assert!(!route.materialized_map_root_candidate);
        assert!(!route.materialized_map_any_owner_candidate);
    }

    #[test]
    fn ordinary_map_owner_v1_attempts_without_complete_actions_stay_compatibility() {
        let unsupported_only = checked(
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let unsupported = keys(root)
                    drop(unsupported)
                    drop(root)
                    0
                }
            "#,
        );
        let unsupported_admission = classify_canonical_mir_route_admission(&unsupported_only);
        assert_eq!(
            unsupported_admission.ordinary_map_owner_lifecycle_i32_string_v1,
            OrdinaryMapOwnerLifecycleI32StringV1Admission::IncompleteCoverage
        );
        assert!(
            !CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1
                .is_admitted(unsupported_admission)
        );
        assert!(!unsupported_admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .is_candidate());
        assert_eq!(
            unsupported_admission.map_root,
            MapRootAdmission::OutsideProfile
        );
        assert_eq!(
            unsupported_admission.map_any_owner,
            MapAnyOwnerAdmission::OutsideProfile
        );

        let partially_covered = checked(
            r#"
                func unsupported_helper() -> i32 {
                    let root = map_new()
                    let size = map_size(root)
                    let unsupported = keys(root)
                    drop(unsupported)
                    drop(root)
                    size
                }
                func main() -> i32 {
                    let source = map_new()
                    let inserted = map_set(source, "answer", 42)
                    let removed = map_remove(inserted, "answer")
                    drop(source)
                    drop(inserted)
                    drop(removed)
                    0
                }
            "#,
        );
        let admission = classify_canonical_mir_route_admission(&partially_covered);
        assert_eq!(
            admission.ordinary_map_owner_lifecycle_i32_string_v1,
            OrdinaryMapOwnerLifecycleI32StringV1Admission::IncompleteCoverage
        );
        assert!(!admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .is_candidate());
        assert_eq!(admission.map_root, MapRootAdmission::OutsideProfile);
        assert_eq!(
            admission.map_any_owner,
            MapAnyOwnerAdmission::OutsideProfile
        );

        let direct_error = materialize_canonical_mir_route(&partially_covered, None).expect_err(
            "direct canonical materialization must reject incomplete ordinary coverage",
        );
        assert!(matches!(
            direct_error,
            CanonicalMirRouteMaterializationError::Complete {
                profile: CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleI32StringV1,
                stage: CanonicalMirRouteFailureStage::Coverage,
                ..
            }
        ));
    }

    #[test]
    fn complete_map_any_owner_identity_wins_only_when_it_covers_all_ordinary_attempts() {
        let covered = checked(
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let updated = map_set(root, "answer", 42)
                    let size = map_size(updated)
                    drop(updated)
                    size
                }
            "#,
        );
        let analysis = covered
            .resource_analysis(&NodeId("function:main".into()))
            .expect("main resource analysis");
        assert!(analysis.has_ordinary_map_owner_lifecycle_v1_receipt());
        assert!(analysis.map_any_owner_v1_actions_are_within_ordinary_map_profile());
        assert_eq!(
            analysis
                .ordinary_map_owner_lifecycle_v1_operation_attempts
                .iter()
                .filter(|attempt| {
                    attempt.kind != OrdinaryMapOwnerLifecycleAttemptKind::ScopeDrop
                })
                .count(),
            analysis.map_any_owner_actions.len(),
            "same-source actions and supported attempts form a complete one-to-one census"
        );
        assert!(ordinary_map_attempts_fully_covered_by_map_any_owner_v1(
            &covered
        ));
        let admission = classify_canonical_mir_route_admission(&covered);
        assert_eq!(
            admission.map_any_owner,
            MapAnyOwnerAdmission::CompleteCoverage
        );
        assert_eq!(
            admission.ordinary_map_owner_lifecycle_i32_string_v1,
            OrdinaryMapOwnerLifecycleI32StringV1Admission::CoveredByMapAnyOwnerV1,
            "the fully joined ordinary attempts share the selected MapAnyOwner identity"
        );
        let route = materialize_canonical_mir_route(&covered, None)
            .expect("complete covering MapAnyOwner receipt materializes");
        assert!(CanonicalMirRouteProfile::MapAnyOwnerV1.is_materialized(&route));
        assert!(!route.materialized_ordinary_map_owner_lifecycle_i32_string_v1_candidate);

        let independent_ordinary_owner = checked(
            r#"
                func map_any_owner() -> i32 {
                    let root = map_new()
                    let updated = map_set(root, "fixed", 42)
                    let size = map_size(updated)
                    drop(updated)
                    size
                }
                func ordinary_dynamic_owner() -> i32 {
                    let key = "dyn" + "amic"
                    let root = map_new()
                    let updated = map_set(root, key, 7)
                    let removed = map_remove(updated, key)
                    let present = has_key(root, key)
                    drop(root)
                    drop(updated)
                    drop(removed)
                    if present { 1 } else { 0 }
                }
                func main() -> i32 { 0 }
            "#,
        );
        let mixed_admission = classify_canonical_mir_route_admission(&independent_ordinary_owner);
        let any_analysis = independent_ordinary_owner
            .resource_analysis(&NodeId("function:map_any_owner".into()))
            .expect("MapAnyOwner callable analysis");
        let ordinary_analysis = independent_ordinary_owner
            .resource_analysis(&NodeId("function:ordinary_dynamic_owner".into()))
            .expect("independent ordinary callable analysis");
        assert!(any_analysis.map_any_owner_v1_actions_are_within_ordinary_map_profile());
        assert!(
            ordinary_analysis.map_any_owner_actions.is_empty(),
            "the second owner has no MapAny action receipt to cover its dynamic-key operations"
        );
        assert!(
            !ordinary_map_attempts_fully_covered_by_map_any_owner_v1(&independent_ordinary_owner),
            "coverage is whole-program and cannot omit a separate callable/root"
        );
        assert_eq!(
            mixed_admission.ordinary_map_owner_lifecycle_i32_string_v1,
            OrdinaryMapOwnerLifecycleI32StringV1Admission::CompleteCoverage
        );
        assert_eq!(
            mixed_admission.map_any_owner,
            MapAnyOwnerAdmission::IncompleteCoverage,
            "the separate dynamic-key owner is absent from MapAnyOwner's source receipts"
        );
        let error = materialize_canonical_mir_route(&independent_ordinary_owner, None)
            .expect_err("an independent ordinary owner cannot be hidden by the other owner");
        assert!(matches!(
            error,
            CanonicalMirRouteMaterializationError::Complete {
                profile: CanonicalMirRouteProfile::MapAnyOwnerV1,
                stage: CanonicalMirRouteFailureStage::Coverage,
                ..
            }
        ));
    }

    #[test]
    fn map_any_owner_mixed_collection_allows_only_its_exact_size_stdout_value() {
        for (printed, admits_route) in [("size", true), ("1", false), ("size + 1", false)] {
            let program = checked(&format!(
                r#"
                    func main() -> i32 {{
                        let root = map_new()
                        let updated = map_set(root, "answer", 42)
                        let size = map_size(updated)
                        println({printed})
                        drop(updated)
                        0
                    }}
                "#
            ));
            let admission = classify_canonical_mir_route_admission(&program);
            assert_eq!(
                admission.map_any_owner,
                MapAnyOwnerAdmission::CompleteCoverage,
                "{printed} retains the complete independent MapAnyOwner receipt"
            );
            assert_eq!(
                admission.collection,
                ScalarCollectionAdmission::MixedCoverage,
                "the standalone scalar-collection classifier remains unchanged"
            );

            let route = materialize_canonical_mir_route(&program, None);
            if admits_route {
                let route = route.expect("only the receipted Size value may cross stdout");
                assert!(CanonicalMirRouteProfile::MapAnyOwnerV1.is_materialized(&route));
            } else {
                assert!(matches!(
                    route,
                    Err(CanonicalMirRouteMaterializationError::Complete {
                        profile: CanonicalMirRouteProfile::MapAnyOwnerV1,
                        stage: CanonicalMirRouteFailureStage::Coverage,
                        message,
                    }) if message.contains("mixed scalar-collection admission")
                ));
            }
        }
    }

    #[test]
    fn ordinary_map_owner_profile_selection_preserves_raw_profiles_and_blocks_fallback() {
        assert_eq!(
            select_map_owner_admissions(
                MapRootAdmission::CompleteCoverage,
                MapAnyOwnerAdmission::OutsideProfile,
                OrdinaryMapOwnerLifecycleI32StringV1Admission::OutsideProfile,
                false
            ),
            (
                MapRootAdmission::CompleteCoverage,
                MapAnyOwnerAdmission::OutsideProfile
            )
        );
        assert_eq!(
            select_map_owner_admissions(
                MapRootAdmission::CompleteCoverage,
                MapAnyOwnerAdmission::CompleteCoverage,
                OrdinaryMapOwnerLifecycleI32StringV1Admission::OutsideProfile,
                false
            ),
            (
                MapRootAdmission::OutsideProfile,
                MapAnyOwnerAdmission::CompleteCoverage
            )
        );
        assert_eq!(
            select_map_owner_admissions(
                MapRootAdmission::CompleteCoverage,
                MapAnyOwnerAdmission::CompleteCoverage,
                OrdinaryMapOwnerLifecycleI32StringV1Admission::IncompleteCoverage,
                false
            ),
            (
                MapRootAdmission::OutsideProfile,
                MapAnyOwnerAdmission::OutsideProfile
            ),
            "incomplete ordinary coverage keeps the default compatibility fallback"
        );
        assert_eq!(
            select_map_owner_admissions(
                MapRootAdmission::CompleteCoverage,
                MapAnyOwnerAdmission::CompleteCoverage,
                OrdinaryMapOwnerLifecycleI32StringV1Admission::CompleteCoverage,
                false
            ),
            (
                MapRootAdmission::OutsideProfile,
                MapAnyOwnerAdmission::CompleteCoverage
            ),
            "two complete but incompatible profile ledgers must not select ordinary Map"
        );
        assert_eq!(
            select_map_owner_admissions(
                MapRootAdmission::CompleteCoverage,
                MapAnyOwnerAdmission::CompleteCoverage,
                OrdinaryMapOwnerLifecycleI32StringV1Admission::IncompleteCoverage,
                true),
            (
                MapRootAdmission::OutsideProfile,
                MapAnyOwnerAdmission::CompleteCoverage
            ),
            "only the fully covered consuming-root v1 exception may survive incomplete ordinary coverage"
        );
        assert_eq!(
            select_map_owner_admissions(
                MapRootAdmission::CompleteCoverage,
                MapAnyOwnerAdmission::IncompleteCoverage,
                OrdinaryMapOwnerLifecycleI32StringV1Admission::IncompleteCoverage,
                true),
            (
                MapRootAdmission::OutsideProfile,
                MapAnyOwnerAdmission::IncompleteCoverage
            ),
            "missing or extra MapAnyOwner receipts remain incomplete even when ordinary coverage is complete"
        );
        assert_eq!(
            select_map_owner_admissions(
                MapRootAdmission::CompleteCoverage,
                MapAnyOwnerAdmission::CompleteCoverage,
                OrdinaryMapOwnerLifecycleI32StringV1Admission::CompleteCoverage,
                true
            ),
            (
                MapRootAdmission::OutsideProfile,
                MapAnyOwnerAdmission::CompleteCoverage
            ),
            "a fully covered ordinary operation set preserves the older MapAnyOwner v1 route"
        );
    }

    #[test]
    fn incomplete_map_any_owner_remains_a_hard_boundary_even_if_ordinary_is_complete() {
        assert_eq!(
            select_map_owner_admissions(
                MapRootAdmission::CompleteCoverage,
                MapAnyOwnerAdmission::IncompleteCoverage,
                OrdinaryMapOwnerLifecycleI32StringV1Admission::CompleteCoverage,
                false
            ),
            (
                MapRootAdmission::OutsideProfile,
                MapAnyOwnerAdmission::IncompleteCoverage,
            ),
            "an incomplete MapAnyOwner ledger is never erased by ordinary coverage"
        );
        assert_eq!(
            select_map_owner_admissions(
                MapRootAdmission::CompleteCoverage,
                MapAnyOwnerAdmission::IncompleteCoverage,
                OrdinaryMapOwnerLifecycleI32StringV1Admission::CompleteCoverage,
                false
            ),
            (
                MapRootAdmission::OutsideProfile,
                MapAnyOwnerAdmission::IncompleteCoverage,
            ),
            "an uncovered MapAny action preserves its incomplete hard boundary"
        );
    }

    #[test]
    fn map_any_owner_v1_admits_only_a_complete_new_ledger() {
        let program = checked(
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let inserted = map_set(root, "answer", 42)
                    let size = map_size(inserted)
                    drop(inserted)
                    size
                }
            "#,
        );
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.map_any_owner,
            MapAnyOwnerAdmission::CompleteCoverage
        );
        assert_eq!(
            admission.ordinary_map_owned_get_i32_string_v1,
            OrdinaryMapOwnedGetI32StringV1Admission::OutsideProfile,
            "ordinary Maps without typed-get attempts must remain eligible for the independent older route"
        );
        assert!(
            admission
                .ordinary_map_owner_lifecycle_i32_string_v1
                .has_profile_signal(),
            "recognized ordinary Map attempts stay visible when MapAnyOwner v1 selects their shared identity"
        );
        assert_eq!(
            admission.ordinary_map_owner_lifecycle_i32_string_v1,
            OrdinaryMapOwnerLifecycleI32StringV1Admission::CoveredByMapAnyOwnerV1,
            "a complete overlapping ordinary ledger remains visible after MapAnyOwner v1 wins identity selection"
        );
        assert_eq!(admission.map_root, MapRootAdmission::OutsideProfile);
        assert!(CanonicalMirRouteProfile::MapAnyOwnerV1.is_admitted(admission));
        assert_eq!(
            CanonicalMirRouteProfile::MapAnyOwnerV1.as_str(),
            "map-any-owner-v1"
        );

        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete independent MapAnyOwner v1 route must materialize");
        assert!(route.materialized_map_any_owner_candidate);
        assert!(CanonicalMirRouteProfile::MapAnyOwnerV1.is_materialized(&route));
    }

    #[test]
    fn ordinary_map_owner_does_not_cross_a_branch_with_live_roots_or_branch_local_mutations() {
        let live_root = checked(
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let result = if true { 1 } else { 0 }
                    drop(root)
                    result
                }
            "#,
        );
        let live_root_admission = classify_canonical_mir_route_admission(&live_root);
        assert_eq!(
            live_root_admission.ordinary_map_owner_lifecycle_i32_string_v1,
            OrdinaryMapOwnerLifecycleI32StringV1Admission::IncompleteCoverage,
            "a root that remains live across the scalar branch cannot receive a prefix-only receipt"
        );

        let branch_local_action = checked(
            r#"
                func main() -> i32 {
                    let root = map_new()
                    if true {
                        let updated = map_set(root, "branch", 1)
                        drop(updated)
                    } else {
                    }
                    drop(root)
                    0
                }
            "#,
        );
        let branch_action_admission = classify_canonical_mir_route_admission(&branch_local_action);
        assert_eq!(
            branch_action_admission.ordinary_map_owner_lifecycle_i32_string_v1,
            OrdinaryMapOwnerLifecycleI32StringV1Admission::IncompleteCoverage,
            "branch-local Map mutations require full path-specific ledgers before admission"
        );
        assert!(materialize_canonical_mir_route(&branch_local_action, None).is_err());
    }

    #[test]
    fn missing_or_partial_map_any_owner_ledger_cannot_fall_back_to_map_root() {
        // A complete legacy MapRoot receipt is only a candidate signal when
        // the independent v1 ledger is absent. It cannot grant the new
        // profile, and the selection rule suppresses the old route fallback.
        let missing = classify_map_any_owner_ledger(
            false,
            false,
            false,
            false,
            MapRootAdmission::CompleteCoverage,
        );
        assert_eq!(missing, MapAnyOwnerAdmission::IncompleteCoverage);
        assert_eq!(
            select_map_root_admission(MapRootAdmission::CompleteCoverage, missing),
            MapRootAdmission::OutsideProfile
        );

        // `map_new()` attempts by themselves do not identify a typed v1
        // candidate. Unsupported ordinary Map/Any operations must preserve
        // their compatibility route when both lifecycle ledgers are empty.
        let unsupported = classify_map_any_owner_ledger(
            false,
            true,
            false,
            false,
            MapRootAdmission::OutsideProfile,
        );
        assert_eq!(unsupported, MapAnyOwnerAdmission::OutsideProfile);
        assert_eq!(
            select_map_root_admission(MapRootAdmission::OutsideProfile, unsupported),
            MapRootAdmission::OutsideProfile
        );

        // A legacy receipt is still evidence that the Checker recognized a
        // lifecycle. Missing independent v1 authority stays a hard boundary.
        let missing_with_legacy_receipt = classify_map_any_owner_ledger(
            false,
            true,
            false,
            false,
            MapRootAdmission::IncompleteCoverage,
        );
        assert_eq!(
            missing_with_legacy_receipt,
            MapAnyOwnerAdmission::IncompleteCoverage
        );

        // An observed extra construction attempt makes real Checker coverage
        // partial. Materialization must hard-fail at the v1 profile boundary,
        // even though the older MapRoot classifier also sees Map activity.
        let program = checked(
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let inserted = map_set(root, "answer", 42)
                    let size = map_size(inserted)
                    let extra = map_size(map_new())
                    let unsupported = map_get(map_new(), "missing")
                    drop(inserted)
                    size + extra
                }
            "#,
        );
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.map_any_owner,
            MapAnyOwnerAdmission::IncompleteCoverage
        );
        assert_eq!(admission.map_root, MapRootAdmission::OutsideProfile);
        let error = materialize_canonical_mir_route(&program, None)
            .expect_err("partial v1 coverage must not retry the raw MapRoot route");
        assert!(matches!(
            error,
            CanonicalMirRouteMaterializationError::Complete {
                profile: CanonicalMirRouteProfile::MapAnyOwnerV1,
                stage: CanonicalMirRouteFailureStage::Coverage,
                ..
            }
        ));
    }

    #[test]
    fn map_root_and_map_any_owner_are_outside_without_a_closed_candidate() {
        let program = checked("func main() -> i32 { 42 }");
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(admission.map_root, MapRootAdmission::OutsideProfile);
        assert_eq!(
            admission.map_any_owner,
            MapAnyOwnerAdmission::OutsideProfile
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("ordinary non-Map program remains a compatibility materialization");
        assert!(!route.materialized_map_root_candidate);
        assert!(!route.materialized_map_any_owner_candidate);
    }

    #[test]
    fn complete_s8_flow_materialization_carries_one_receipt() {
        let program = checked(
            "flow Counter { state Zero { n: i32 } transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } } } func main() -> i32 { let c = Zero { n: 41 } let c2 = Counter::inc(c) c2.n }",
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete S8 Flow route must materialize");
        assert_eq!(route.admission.flow, S8FlowAdmission::CompleteCoverage);
        assert!(route.materialized_flow_candidate);
        assert!(!route.materialized_collection_candidate);
        assert!(!route.materialized_record_candidate);
        assert!(!route.materialized_option_string_candidate);
        assert!(!route.materialized_generic_variant_candidate);
    }

    #[test]
    fn complete_session_channel_materialization_carries_one_receipt() {
        let program = checked(
            r#"
                session Proto = !i32 . ?i32 . end

                func main() -> i64 {
                    let (tx, rx) = session_pair::<Proto>()
                    session_send(tx, 10)
                    let first = session_recv(rx)
                    session_send(rx, first + 1)
                    let second = session_recv(tx)
                    session_close(rx)
                    session_close(tx)
                    println(first)
                    println(second)
                    0
                }
            "#,
        );
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(admission.session, SessionChannelAdmission::CompleteCoverage);
        assert!(admission.has_candidate());
        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete session-channel route must materialize");
        assert!(route.materialized_session_candidate);
        assert!(CanonicalMirRouteProfile::SessionChannel.is_admitted(route.admission));
        assert!(CanonicalMirRouteProfile::SessionChannel.is_materialized(&route));
        let island_validation = crate::core::mir::validate_session_channel_island(&route.program);
        assert!(
            island_validation.is_ok(),
            "island validator: {island_validation:?}"
        );
    }

    #[test]
    fn session_assign_sibling_keeps_compatibility_route() {
        // The scalar-assign face lowers root-level assigns (R6-1049), so an
        // assign sibling now CONSTRUCTS a canonical graph.  The compatibility
        // route is still kept: the session island only routes canonical when
        // admission is CompleteCoverage, and MixedCoverage here means the
        // session profile is not admitted — never a hard rejection of a
        // working legacy program and never an unproven canonical admission.
        let program = checked(
            r#"
                session Proto = !i32 . ?i32 . end

                func main() -> i64 {
                    let (tx, rx) = session_pair::<Proto>()
                    let mut sent = 0
                    sent = 1
                    session_send(tx, sent)
                    let first = session_recv(rx)
                    session_send(rx, first)
                    let second = session_recv(tx)
                    session_close(rx)
                    session_close(tx)
                    0
                }
            "#,
        );
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(admission.session, SessionChannelAdmission::MixedCoverage);
        let route = materialize_canonical_mir_route(&program, None)
            .expect("the scalar-assign face lowers the sibling program's graph");
        assert_eq!(
            route.admission.session,
            SessionChannelAdmission::MixedCoverage
        );
        assert!(
            !CanonicalMirRouteProfile::SessionChannel.is_admitted(route.admission),
            "mixed session coverage must not admit the session-channel route"
        );
    }

    #[test]
    fn compatibility_materialization_error_preserves_candidate_admission() {
        let admission = CanonicalMirRouteAdmission {
            map_root: MapRootAdmission::OutsideProfile,
            map_any_owner: MapAnyOwnerAdmission::OutsideProfile,
            ordinary_map_owner_lifecycle_i32_string_v1:
                OrdinaryMapOwnerLifecycleI32StringV1Admission::OutsideProfile,
            ordinary_map_owned_get_i32_string_v1:
                OrdinaryMapOwnedGetI32StringV1Admission::OutsideProfile,
            scalar_export: false,
            scalar_ffi: false,
            scalar_nested: false,
            scalar_actor: false,
            scalar_generic_identity_i32: ScalarGenericIdentityAdmission::OutsideProfile,
            scalar_generic_identity_i64: ScalarGenericIdentityAdmission::OutsideProfile,
            collection: ScalarCollectionAdmission::MixedCoverage,
            record: FlatCopyRecordAdmission::OutsideProfile,
            flow: S8FlowAdmission::OutsideProfile,
            flow_failure_retry: false,
            option_string: OptionStringVariantAdmission::OutsideProfile,
            option_nested_tuple: OptionNestedTupleVariantAdmission::OutsideProfile,
            generic_variant: GenericVariantPredicateAdmission::OutsideProfile,
            generic_option_projection: GenericOptionProjectionAdmission::OutsideProfile,
            generic_option_projection_fallback:
                GenericOptionProjectionFallbackAdmission::OutsideProfile,
            generic_result_projection: GenericResultProjectionAdmission::OutsideProfile,
            generic_result_projection_fallback:
                GenericResultProjectionFallbackAdmission::OutsideProfile,
            managed_result_call: ManagedResultCallAdmission::OutsideProfile,
            copy_option_i32: CopyOptionI32VariantAdmission::OutsideProfile,
            copy_option_bool: CopyOptionI32VariantAdmission::OutsideProfile,
            copy_option_i64: CopyOptionI32VariantAdmission::OutsideProfile,
            copy_option_f64: CopyOptionI32VariantAdmission::OutsideProfile,
            copy_result_i32: CopyResultI32VariantAdmission::OutsideProfile,
            session: SessionChannelAdmission::OutsideProfile,
        };
        let error = match_complete_or_compatibility(
            admission,
            CanonicalMirRouteFailureStage::Construction,
            "unsupported mixed graph".into(),
        );
        assert_eq!(error.diagnostic_code(), None);
        let compatibility_diagnostic = error.to_diagnostic();
        assert!(compatibility_diagnostic.code.is_none());
        assert!(compatibility_diagnostic.origin.is_none());
        let CanonicalMirRouteMaterializationError::Compatibility {
            admission: preserved,
            ..
        } = error
        else {
            panic!("mixed admission must remain an explicit compatibility error");
        };
        assert_eq!(preserved, admission);
        assert!(preserved.has_candidate());
    }

    #[test]
    fn option_string_variant_materialization_carries_one_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_option_string_switch_move.mimi"
        ));
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.option_string,
            OptionStringVariantAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete Option<string> route must materialize");
        assert!(route.materialized_option_string_candidate);
        assert!(!route.materialized_generic_variant_candidate);
        assert!(crate::core::mir::validate_option_string_variant_island(&route.program).is_ok());
    }

    #[test]
    fn copy_option_i32_materialization_carries_one_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_option_i32_unwrap.mimi"
        ));
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.copy_option_i32,
            CopyOptionI32VariantAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete Copy Option<i32> route must materialize");
        assert!(route.materialized_copy_option_i32_candidate);
        assert!(crate::core::mir::validate_copy_option_i32_variant_island(&route.program).is_ok());
    }

    #[test]
    fn copy_option_i32_unwrap_or_materialization_carries_fallback_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_option_i32_unwrap_or.mimi"
        ));
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.copy_option_i32,
            CopyOptionI32VariantAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete Copy Option<i32>.unwrap_or route must materialize");
        assert!(route.materialized_copy_option_i32_candidate);
        crate::core::mir::validate_copy_option_i32_variant_island(&route.program)
            .expect("Option<i32>.unwrap_or island validator");
    }

    #[test]
    fn copy_option_bool_materialization_carries_one_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_option_bool_unwrap.mimi"
        ));
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.copy_option_bool,
            CopyOptionI32VariantAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete Copy Option<bool> route must materialize");
        assert!(route.materialized_copy_option_bool_candidate);
        assert!(crate::core::mir::validate_copy_option_variant_island(
            &route.program,
            crate::core::PrimitiveType::Bool,
            crate::core::mir::COPY_OPTION_BOOL_VARIANT_ISLAND,
        )
        .is_ok());
    }

    #[test]
    fn copy_option_i64_materialization_carries_one_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_option_i64_unwrap.mimi"
        ));
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.copy_option_i64,
            CopyOptionI32VariantAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete Copy Option<i64> route must materialize");
        assert!(route.materialized_copy_option_i64_candidate);
        assert!(crate::core::mir::validate_copy_option_i64_variant_island(&route.program).is_ok());
    }

    #[test]
    fn copy_option_f64_materialization_carries_one_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_option_f64_unwrap.mimi"
        ));
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.copy_option_f64,
            CopyOptionI32VariantAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete Copy Option<f64> route must materialize");
        assert!(route.materialized_copy_option_f64_candidate);
        assert!(crate::core::mir::validate_copy_option_f64_variant_island(&route.program).is_ok());
    }

    #[test]
    fn copy_result_i32_materialization_carries_one_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_result_i32_unwrap.mimi"
        ));
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.copy_result_i32,
            CopyResultI32VariantAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete Copy Result<i32, i32> route must materialize");
        assert!(route.materialized_copy_result_i32_candidate);
        assert!(crate::core::mir::validate_copy_result_i32_variant_island(&route.program).is_ok());
    }

    #[test]
    fn copy_result_i32_unwrap_or_materialization_carries_fallback_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_result_i32_unwrap_or.mimi"
        ));
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.copy_result_i32,
            CopyResultI32VariantAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete Copy Result unwrap_or route must materialize");
        assert!(route.materialized_copy_result_i32_candidate);
        let receipt = route
            .program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::VariantProjectOr {
                    contract: Some(receipt),
                    ..
                } => Some(receipt),
                _ => None,
            })
            .expect("unwrap_or must materialize a fallback receipt");
        assert_eq!(receipt.fallback_variant_name, "Err");
        assert_eq!(receipt.fallback_discriminant, 1);
        assert_eq!(receipt.result_ty, receipt.fallback_ty);
    }

    #[test]
    fn generic_option_predicate_materialization_carries_one_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_predicate.mimi"
        ));
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.generic_variant,
            GenericVariantPredicateAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete generic Option predicate route must materialize");
        assert!(route.materialized_generic_variant_candidate);
        assert!(route.program.instances().values().any(|instance| matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantPredicate { .. }
        )));
    }

    #[test]
    fn generic_option_projection_materialization_carries_one_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap.mimi"
        ));
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.generic_option_projection,
            GenericOptionProjectionAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete generic Option projection route must materialize");
        assert!(route.materialized_generic_option_projection_candidate);
        assert!(!route.materialized_generic_variant_candidate);
        assert!(route.program.instances().values().any(|instance| matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { .. }
        )));
    }

    #[test]
    fn generic_owned_option_projection_materialization_carries_move_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_owned_string.mimi"
        ));
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.generic_option_projection,
            GenericOptionProjectionAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete generic owned Option projection route must materialize");
        assert!(route.materialized_generic_option_projection_candidate);
        let instance = route
            .program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership == crate::core::mir::types::MirOwnership::Move
                )
            })
            .expect("owned generic Option projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { .. }
        ));
    }

    #[test]
    fn generic_owned_list_option_projection_is_not_misclassified_as_option_string() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_owned_list.mimi"
        ));
        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete generic Option<List> route must materialize");
        assert!(!route.materialized_option_string_candidate);
        assert!(route.materialized_generic_option_projection_candidate);
        assert!(route.admission.generic_option_projection_complete());
    }

    #[test]
    fn generic_result_projection_materialization_carries_one_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_unwrap.mimi"
        ));
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.generic_result_projection,
            GenericResultProjectionAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete generic Result projection route must materialize");
        assert!(route.materialized_generic_result_projection_candidate);
        assert!(!route.materialized_generic_variant_candidate);
        assert!(!route.materialized_generic_option_projection_candidate);
        assert!(route.program.instances().values().any(|instance| {
            matches!(
                &instance.contract,
                crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Result"
            )
        }));
    }

    #[test]
    fn generic_result_f64_projection_materialization_carries_heterogeneous_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_unwrap_f64.mimi"
        ));
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.generic_result_projection,
            GenericResultProjectionAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("generic Result<T,i32> f64 route must materialize");
        assert!(route.materialized_generic_result_projection_candidate);
        let instance = route
            .program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Result"
                )
            })
            .expect("generic Result f64 projection instance");
        let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract } =
            &instance.contract
        else {
            unreachable!("filtered above");
        };
        assert!(matches!(
            route
                .program
                .type_catalog()
                .get(&contract.result_ty)
                .map(|descriptor| descriptor.abi),
            Some(crate::core::mir::types::MirAbiClass::Float { bits: 64 })
        ));
    }

    #[test]
    fn generic_result_homogeneous_f64_projection_is_not_admitted() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_unwrap_homogeneous_f64_rejected.mimi"
        ));
        assert_eq!(
            classify_canonical_mir_route_admission(&program).generic_result_projection,
            GenericResultProjectionAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect_err("homogeneous Result<T,T> f64 must fail closed");
        let message = route.to_string();
        assert!(
            message.contains("generic MIR instance") && message.contains("outside scalar contract"),
            "unexpected route rejection: {message}"
        );
    }

    #[test]
    fn generic_result_f64_unwrap_or_materialization_carries_heterogeneous_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_unwrap_or_f64.mimi"
        ));
        assert_eq!(
            classify_canonical_mir_route_admission(&program).generic_result_projection_fallback,
            GenericResultProjectionFallbackAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("generic Result<T,i32> f64 unwrap_or route must materialize");
        assert!(route.materialized_generic_result_projection_fallback_candidate);
        let instance = route
            .program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Result"
                )
            })
            .expect("generic Result f64 fallback instance");
        let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
            contract,
        } = &instance.contract
        else {
            unreachable!("filtered above");
        };
        assert!(matches!(
            route
                .program
                .type_catalog()
                .get(&contract.result_ty)
                .map(|descriptor| descriptor.abi),
            Some(crate::core::mir::types::MirAbiClass::Float { bits: 64 })
        ));
    }

    #[test]
    fn generic_result_bool_f64_unwrap_or_materialization_carries_heterogeneous_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_bool_unwrap_or_f64.mimi"
        ));
        assert_eq!(
            classify_canonical_mir_route_admission(&program).generic_result_projection_fallback,
            GenericResultProjectionFallbackAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("generic Result<T,bool> f64 unwrap_or route must materialize");
        assert!(route.materialized_generic_result_projection_fallback_candidate);
        let instance = route
            .program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Result"
                )
            })
            .expect("generic Result bool/f64 fallback instance");
        let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
            contract,
        } = &instance.contract
        else {
            unreachable!("filtered above");
        };
        assert!(matches!(
            route
                .program
                .type_catalog()
                .get(&contract.result_ty)
                .map(|descriptor| descriptor.abi),
            Some(crate::core::mir::types::MirAbiClass::Float { bits: 64 })
        ));
    }

    #[test]
    fn managed_result_call_materialization_carries_one_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_result_list_i32_call_return.mimi"
        ));
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.managed_result_call,
            ManagedResultCallAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("managed Result direct-call route must materialize");
        assert!(route.materialized_managed_result_call_candidate);
        assert!(route
            .program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::Call {
                        variant_call_contract: Some(_),
                        ..
                    }
                )
            }));
        crate::core::mir::validate_managed_result_call_island(&route.program)
            .expect("managed Result direct-call island validator");
    }

    #[test]
    fn managed_result_call_materialization_accepts_i64_and_bool_list_payloads() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_result_list_i64_bool_call_return.mimi"
        ));
        let route = materialize_canonical_mir_route(&program, None)
            .expect("List<i64|bool> managed Result route must materialize");
        assert_eq!(
            route.admission.managed_result_call,
            ManagedResultCallAdmission::CompleteCoverage
        );
        assert!(route.materialized_managed_result_call_candidate);
        crate::core::mir::validate_managed_result_call_island(&route.program)
            .expect("List<i64|bool> managed Result island validator");
    }

    #[test]
    fn unsupported_managed_result_call_admission_is_fail_closed() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_result_list_f64_call_rejected.mimi"
        ));
        assert_eq!(
            classify_canonical_mir_route_admission(&program).managed_result_call,
            ManagedResultCallAdmission::MixedCoverage
        );
    }

    #[test]
    fn generic_option_projection_fallback_materialization_carries_one_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_or.mimi"
        ));
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.generic_option_projection_fallback,
            GenericOptionProjectionFallbackAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete generic Option fallback route must materialize");
        assert!(route.materialized_generic_option_projection_fallback_candidate);
        assert!(route.program.instances().values().any(|instance| {
            matches!(
                &instance.contract,
                crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                    contract
                } if contract.projection.nominal.as_str() == "builtin:type:Option"
            )
        }));
    }

    #[test]
    fn generic_option_f64_projection_fallback_materialization_carries_float_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_or_f64.mimi"
        ));
        let admission = classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.generic_option_projection_fallback,
            GenericOptionProjectionFallbackAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("complete generic Option<f64> fallback route must materialize");
        assert!(route.materialized_generic_option_projection_fallback_candidate);
        let instance = route
            .program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership
                            == crate::core::mir::types::MirOwnership::Copy
                )
            })
            .expect("generic Option<f64> fallback instance");
        assert_eq!(
            route
                .program
                .type_catalog()
                .get(&instance.arguments[0])
                .expect("generic Option<f64> fallback TypeDesc")
                .abi,
            crate::core::mir::types::MirAbiClass::Float { bits: 64 }
        );
    }

    #[test]
    fn profile_matrix_owns_admission_and_materialization_mapping() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_list_len.mimi"
        ));
        let route = materialize_canonical_mir_route(&program, None)
            .expect("scalar collection route must materialize");
        let admission = route.admission;
        let profiles = [
            CanonicalMirRouteProfile::MapRoot,
            CanonicalMirRouteProfile::MapAnyOwnerV1,
            CanonicalMirRouteProfile::OrdinaryMapOwnedGetI32StringV1,
            CanonicalMirRouteProfile::ScalarGenericIdentityI32,
            CanonicalMirRouteProfile::ScalarGenericIdentityI64,
            CanonicalMirRouteProfile::ScalarCollection,
            CanonicalMirRouteProfile::FlatCopyRecord,
            CanonicalMirRouteProfile::S8FlowTransition,
            CanonicalMirRouteProfile::NonCopyOptionStringVariant,
            CanonicalMirRouteProfile::GenericOptionPredicate,
            CanonicalMirRouteProfile::GenericOptionProjection,
            CanonicalMirRouteProfile::GenericOptionProjectionFallback,
            CanonicalMirRouteProfile::GenericResultProjection,
            CanonicalMirRouteProfile::ManagedResultCall,
            CanonicalMirRouteProfile::CopyOptionI32Variant,
            CanonicalMirRouteProfile::CopyOptionBoolVariant,
            CanonicalMirRouteProfile::CopyOptionI64Variant,
            CanonicalMirRouteProfile::CopyOptionF64Variant,
            CanonicalMirRouteProfile::CopyResultI32Variant,
        ];
        for profile in profiles {
            assert_eq!(
                profile.is_admitted(admission),
                matches!(profile, CanonicalMirRouteProfile::ScalarCollection),
                "profile admission mapping drifted for {}",
                profile.as_str()
            );
            assert_eq!(
                profile.is_materialized(&route),
                matches!(profile, CanonicalMirRouteProfile::ScalarCollection),
                "profile materialization mapping drifted for {}",
                profile.as_str()
            );
        }

        let i64_program = checked(
            "func pass<T>(value: T) -> T { value }\nfunc main() -> i64 { pass(2147483690) }",
        );
        let i64_route = materialize_canonical_mir_route(&i64_program, None)
            .expect("i64 generic identity route for profile matrix");
        for profile in [
            CanonicalMirRouteProfile::ScalarGenericIdentityI32,
            CanonicalMirRouteProfile::ScalarGenericIdentityI64,
            CanonicalMirRouteProfile::ScalarCollection,
        ] {
            assert_eq!(
                profile.is_admitted(i64_route.admission),
                matches!(profile, CanonicalMirRouteProfile::ScalarGenericIdentityI64),
                "i64 fixture admission mapping drifted for {}",
                profile.as_str()
            );
            assert_eq!(
                profile.is_materialized(&i64_route),
                matches!(profile, CanonicalMirRouteProfile::ScalarGenericIdentityI64),
                "i64 fixture materialization mapping drifted for {}",
                profile.as_str()
            );
        }
    }

    #[test]
    fn generic_record_f64_projection_materializes_float_record_receipt() {
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_projection_f64.mimi"
        ));
        assert_eq!(
            classify_flat_copy_record_admission(&program),
            FlatCopyRecordAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("generic Record<f64> route must materialize");
        assert!(route.materialized_record_candidate);
        let instance = route
            .program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                        contract
                    } if contract.arity == 1 && contract.name == "value"
                )
            })
            .expect("generic Record<f64> route instance");
        assert_eq!(
            route
                .program
                .type_catalog()
                .get(&instance.arguments[0])
                .expect("generic Record<f64> argument TypeDesc")
                .abi,
            crate::core::mir::types::MirAbiClass::Float { bits: 64 }
        );
    }

    #[test]
    fn record_program_with_integer_print_is_complete_admission() {
        // R6-1048 regression repair: once numeric-widen call receipts let the
        // graph materialize the record candidate inside mixed coverage, the
        // doctrine forbids the legacy fallback — the record island must close
        // admission for the checker-legal integer-print face instead, keeping
        // the classifier in parity with demonstrated graph construction.
        let program = checked(include_str!(
            "../../../tests/fixtures/mir_record_print_face.mimi"
        ));
        assert_eq!(
            classify_flat_copy_record_admission(&program),
            FlatCopyRecordAdmission::CompleteCoverage
        );
        let route = materialize_canonical_mir_route(&program, None)
            .expect("record print face must materialize");
        assert!(route.materialized_record_candidate);
        for function in route.program.functions().values() {
            function
                .validate()
                .expect("record print face graph must be structurally valid");
        }
    }

    #[test]
    fn record_program_with_non_integer_print_stays_mixed_coverage() {
        // The admitted scalar-print face is exactly the signed-integer
        // builtin println the graph demonstrably lowers; a float argument
        // keeps the explicit compatibility boundary (construction fails on
        // the unsupported builtin kind and the program stays a legacy
        // compatibility input).
        let program = checked(
            r#"
                type Point { x: i64, y: i64 }

                func main() -> i32 {
                    let p = Point { x: 1, y: 2 }
                    println(p.x)
                    println(1.5)
                    0
                }
            "#,
        );
        assert_eq!(
            classify_flat_copy_record_admission(&program),
            FlatCopyRecordAdmission::MixedCoverage
        );
    }
}
