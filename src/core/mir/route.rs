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

use std::collections::HashSet;

use crate::core::mir::reference::MirProgram;
use crate::core::CheckedProgram;

use super::{
    classify_copy_option_i32_variant_admission, classify_copy_option_variant_admission,
    classify_copy_result_i32_variant_admission, classify_flat_copy_record_admission,
    classify_generic_option_projection_admission,
    classify_generic_option_projection_fallback_admission,
    classify_generic_result_projection_admission,
    classify_generic_result_projection_fallback_admission,
    classify_generic_variant_predicate_admission, classify_managed_result_call_admission,
    classify_option_string_variant_admission, classify_scalar_collection_admission,
    contains_copy_option_i32_variant_candidate, contains_copy_option_variant_candidate,
    contains_copy_result_i32_variant_candidate, contains_flat_copy_record_candidate,
    contains_flow_failure_retry_candidate, contains_generic_option_projection_candidate,
    contains_generic_option_projection_fallback_candidate,
    contains_generic_result_projection_candidate,
    contains_generic_result_projection_fallback_candidate,
    contains_generic_variant_predicate_candidate, contains_managed_result_call_candidate,
    contains_option_string_variant_candidate, contains_owned_record_projection_candidate,
    contains_s8_flow_transition_candidate, contains_scalar_collection_candidate,
    contains_scalar_collection_operation_candidate, is_exact_s8_flow_transition,
    is_flow_failure_retry_candidate, is_s8_flow_transition_candidate,
    CopyOptionI32VariantAdmission, CopyResultI32VariantAdmission, FlatCopyRecordAdmission,
    GenericOptionProjectionAdmission, GenericOptionProjectionFallbackAdmission,
    GenericResultProjectionAdmission, GenericResultProjectionFallbackAdmission,
    GenericVariantPredicateAdmission, ManagedResultCallAdmission, OptionStringVariantAdmission,
    ScalarCollectionAdmission,
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
    ScalarCollection,
    FlatCopyRecord,
    S8FlowTransition,
    FlowFailureRetry,
    NonCopyOptionStringVariant,
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
}

impl CanonicalMirRouteProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ScalarCollection => super::SCALAR_COLLECTION_ISLAND,
            Self::FlatCopyRecord => "flat-copy-record-v1",
            Self::S8FlowTransition => "s8-silent-local-flow-v1",
            Self::FlowFailureRetry => "m3-recoverable-flow-retry-v1",
            Self::NonCopyOptionStringVariant => super::NON_COPY_OPTION_STRING_VARIANT_ISLAND,
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
        }
    }

    /// Return whether checker-owned admission has completed this profile.
    ///
    /// This is deliberately kept next to the profile names and materialized
    /// receipts so verifier and backend route owners cannot grow independent
    /// `is_exact -> construct -> contains_*` tables.
    pub const fn is_admitted(self, admission: CanonicalMirRouteAdmission) -> bool {
        match self {
            Self::ScalarCollection => admission.collection_complete(),
            Self::FlatCopyRecord => admission.record_complete(),
            Self::S8FlowTransition => admission.flow_complete(),
            Self::FlowFailureRetry => admission.flow_failure_retry,
            Self::NonCopyOptionStringVariant => admission.option_string_complete(),
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
        }
    }

    /// Return whether the canonical route contains this profile's operation
    /// materialization receipt.
    pub const fn is_materialized(self, route: &CanonicalMirRouteMaterialization) -> bool {
        match self {
            Self::ScalarCollection => route.materialized_collection_candidate,
            Self::FlatCopyRecord => route.materialized_record_candidate,
            Self::S8FlowTransition => route.materialized_flow_candidate,
            Self::FlowFailureRetry => route.materialized_flow_failure_retry_candidate,
            Self::NonCopyOptionStringVariant => route.materialized_option_string_candidate,
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
        }
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

/// Checker-owned route admission captured alongside one canonical graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalMirRouteAdmission {
    pub collection: ScalarCollectionAdmission,
    pub record: FlatCopyRecordAdmission,
    pub flow: S8FlowAdmission,
    pub flow_failure_retry: bool,
    pub option_string: OptionStringVariantAdmission,
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
}

impl CanonicalMirRouteAdmission {
    pub const fn has_candidate(self) -> bool {
        !matches!(self.collection, ScalarCollectionAdmission::OutsideProfile)
            || !matches!(self.record, FlatCopyRecordAdmission::OutsideProfile)
            || !matches!(self.flow, S8FlowAdmission::OutsideProfile)
            || self.flow_failure_retry
            || !matches!(
                self.option_string,
                OptionStringVariantAdmission::OutsideProfile
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
    }

    pub const fn collection_complete(self) -> bool {
        matches!(self.collection, ScalarCollectionAdmission::CompleteCoverage)
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
}

/// One immutable canonical graph plus the receipts needed by route owners.
///
/// The booleans are materialization receipts, not backend capability claims.
/// Every consumer still validates the graph immediately before use.
#[derive(Debug, Clone)]
pub struct CanonicalMirRouteMaterialization {
    pub program: MirProgram,
    pub admission: CanonicalMirRouteAdmission,
    pub materialized_collection_candidate: bool,
    pub materialized_record_candidate: bool,
    pub materialized_flow_candidate: bool,
    pub materialized_flow_failure_retry_candidate: bool,
    pub materialized_option_string_candidate: bool,
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
}

/// Classify route eligibility once from checker-owned typed facts.
pub fn classify_canonical_mir_route_admission(
    program: &CheckedProgram,
) -> CanonicalMirRouteAdmission {
    CanonicalMirRouteAdmission {
        collection: classify_scalar_collection_admission(program),
        record: classify_flat_copy_record_admission(program),
        flow: classify_s8_flow_admission(program),
        flow_failure_retry: is_flow_failure_retry_candidate(program),
        option_string: classify_option_string_variant_admission(program),
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
    }
}

fn classify_s8_flow_admission(program: &CheckedProgram) -> S8FlowAdmission {
    if is_exact_s8_flow_transition(program) {
        S8FlowAdmission::CompleteCoverage
    } else if is_s8_flow_transition_candidate(program) {
        S8FlowAdmission::MixedCoverage
    } else {
        S8FlowAdmission::OutsideProfile
    }
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
            ))
        }
    };

    let materialized_collection_operation_candidate =
        contains_scalar_collection_operation_candidate(&canonical);
    let materialized_collection_candidate = materialized_collection_operation_candidate
        || (admission.collection_complete()
            && canonical.transitions().is_empty()
            && contains_scalar_collection_candidate(&canonical));
    let materialized_record_candidate = contains_flat_copy_record_candidate(&canonical);
    let materialized_flow_candidate = contains_s8_flow_transition_candidate(&canonical);
    let materialized_flow_failure_retry_candidate =
        contains_flow_failure_retry_candidate(&canonical);
    let materialized_option_string_candidate = contains_option_string_variant_candidate(&canonical);
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
            message: "complete recoverable Flow admission did not materialize a RecoverableLocal FlowTransition boundary".into(),
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

    Ok(CanonicalMirRouteMaterialization {
        program: canonical,
        admission,
        materialized_collection_candidate,
        materialized_record_candidate,
        materialized_flow_candidate,
        materialized_flow_failure_retry_candidate,
        materialized_option_string_candidate,
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
    })
}

fn match_complete_or_compatibility(
    admission: CanonicalMirRouteAdmission,
    stage: CanonicalMirRouteFailureStage,
    message: String,
) -> CanonicalMirRouteMaterializationError {
    if admission.flow_failure_retry {
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
    } else {
        CanonicalMirRouteMaterializationError::Compatibility { admission, message }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(source: &str) -> CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        crate::core::check_program(&file).expect("typecheck")
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
    fn complete_scalar_collection_materialization_failure_is_hard() {
        let program = checked(
            r#"
                func main() -> i32 {
                    let values = [1, 2, 3]
                    let count = len(values)
                    drop(values)
                    for i in range(0, 3) {
                        let copy = i
                        drop(copy)
                    }
                    count
                }
            "#,
        );
        let error = materialize_canonical_mir_route(&program, None)
            .expect_err("complete collection lowering must not become compatibility");
        assert!(matches!(
            error,
            CanonicalMirRouteMaterializationError::Complete {
                profile: CanonicalMirRouteProfile::ScalarCollection,
                stage: CanonicalMirRouteFailureStage::Construction,
                ..
            }
        ));
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
    fn compatibility_materialization_error_preserves_candidate_admission() {
        let admission = CanonicalMirRouteAdmission {
            collection: ScalarCollectionAdmission::MixedCoverage,
            record: FlatCopyRecordAdmission::OutsideProfile,
            flow: S8FlowAdmission::OutsideProfile,
            flow_failure_retry: false,
            option_string: OptionStringVariantAdmission::OutsideProfile,
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
        };
        let error = match_complete_or_compatibility(
            admission,
            CanonicalMirRouteFailureStage::Construction,
            "unsupported mixed graph".into(),
        );
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
            message.contains("generic") && message.contains("f64"),
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
}
