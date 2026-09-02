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
    classify_flat_copy_record_admission, classify_option_string_variant_admission,
    classify_scalar_collection_admission, contains_flat_copy_record_candidate,
    contains_option_string_variant_candidate, contains_s8_flow_transition_candidate,
    contains_scalar_collection_candidate, contains_scalar_collection_operation_candidate,
    is_exact_s8_flow_transition, is_s8_flow_transition_candidate, FlatCopyRecordAdmission,
    OptionStringVariantAdmission, ScalarCollectionAdmission,
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
    NonCopyOptionStringVariant,
}

impl CanonicalMirRouteProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ScalarCollection => super::SCALAR_COLLECTION_ISLAND,
            Self::FlatCopyRecord => "flat-copy-record-v1",
            Self::S8FlowTransition => "s8-silent-local-flow-v1",
            Self::NonCopyOptionStringVariant => super::NON_COPY_OPTION_STRING_VARIANT_ISLAND,
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
            Self::NonCopyOptionStringVariant => admission.option_string_complete(),
        }
    }

    /// Return whether the canonical route contains this profile's operation
    /// materialization receipt.
    pub const fn is_materialized(self, route: &CanonicalMirRouteMaterialization) -> bool {
        match self {
            Self::ScalarCollection => route.materialized_collection_candidate,
            Self::FlatCopyRecord => route.materialized_record_candidate,
            Self::S8FlowTransition => route.materialized_flow_candidate,
            Self::NonCopyOptionStringVariant => route.materialized_option_string_candidate,
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
    pub option_string: OptionStringVariantAdmission,
}

impl CanonicalMirRouteAdmission {
    pub const fn has_candidate(self) -> bool {
        !matches!(self.collection, ScalarCollectionAdmission::OutsideProfile)
            || !matches!(self.record, FlatCopyRecordAdmission::OutsideProfile)
            || !matches!(self.flow, S8FlowAdmission::OutsideProfile)
            || !matches!(
                self.option_string,
                OptionStringVariantAdmission::OutsideProfile
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
    pub materialized_option_string_candidate: bool,
}

/// Classify route eligibility once from checker-owned typed facts.
pub fn classify_canonical_mir_route_admission(
    program: &CheckedProgram,
) -> CanonicalMirRouteAdmission {
    CanonicalMirRouteAdmission {
        collection: classify_scalar_collection_admission(program),
        record: classify_flat_copy_record_admission(program),
        flow: classify_s8_flow_admission(program),
        option_string: classify_option_string_variant_admission(program),
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
    let materialized_option_string_candidate = contains_option_string_variant_candidate(&canonical);
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
    if admission.option_string_complete() && !materialized_option_string_candidate {
        return Err(CanonicalMirRouteMaterializationError::Complete {
            profile: CanonicalMirRouteProfile::NonCopyOptionStringVariant,
            stage: CanonicalMirRouteFailureStage::Coverage,
            message: "complete Option<string> admission did not materialize a variant boundary"
                .into(),
        });
    }

    Ok(CanonicalMirRouteMaterialization {
        program: canonical,
        admission,
        materialized_collection_candidate,
        materialized_record_candidate,
        materialized_flow_candidate,
        materialized_option_string_candidate,
    })
}

fn match_complete_or_compatibility(
    admission: CanonicalMirRouteAdmission,
    stage: CanonicalMirRouteFailureStage,
    message: String,
) -> CanonicalMirRouteMaterializationError {
    if admission.collection_complete() {
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
    }

    #[test]
    fn compatibility_materialization_error_preserves_candidate_admission() {
        let admission = CanonicalMirRouteAdmission {
            collection: ScalarCollectionAdmission::MixedCoverage,
            record: FlatCopyRecordAdmission::OutsideProfile,
            flow: S8FlowAdmission::OutsideProfile,
            option_string: OptionStringVariantAdmission::OutsideProfile,
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
        assert!(crate::core::mir::validate_option_string_variant_island(&route.program).is_ok());
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
}
