//! Program-level Canonical MIR route selection.
//!
//! The default entry points must make one capability decision for the whole
//! checked program.  A canonical backend is never attempted and then replaced
//! by legacy code on failure: programs either pass this preflight and use the
//! canonical route, or remain on the legacy route because their capability
//! set is not yet migrated. Once the scalar collection or exact non-Copy
//! `Option<string>` island is recognized, its failure is an explicit rejection
//! rather than compatibility fallback.

use std::collections::HashSet;

use mimi::ast::File;
use mimi::core::mir::reference::MirProgram;
use mimi::core::CheckedProgram;
use mimi::verifier::VerifStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegacyRouteReason {
    /// No checker-owned migration profile was recognized.
    OutsideMigratedProfile,
    /// A compatibility-shaped program did not materialize a migrated MIR
    /// operation, so it remains outside the current production island.
    MixedCoverageWithoutMaterializedCandidate,
}

impl LegacyRouteReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::OutsideMigratedProfile => "outside-migrated-profile",
            Self::MixedCoverageWithoutMaterializedCandidate => {
                "mixed-coverage-without-materialized-candidate"
            }
        }
    }
}

pub(crate) fn report_legacy_route(reason: LegacyRouteReason) {
    if std::env::var_os("MIMI_VERBOSE").is_some() {
        eprintln!("canonical route disposition: legacy ({})", reason.as_str());
    }
}

#[derive(Debug)]
pub(crate) enum DefaultMirRoute {
    /// Explicit compatibility route. The reason is part of the route
    /// disposition so callers cannot mistake an unrecognized program for a
    /// canonical preflight failure that was silently downgraded.
    Legacy(LegacyRouteReason),
    Canonical(MirProgram),
    /// A recognized migrated-island candidate failed canonical preflight.
    /// Once an island is recognized, its old production path is deleted and
    /// the CLI must fail closed instead of silently compiling it with legacy.
    Rejected(String),
}

/// Materialize the production route graph with only the known prelude
/// compatibility source excluded.  Admission and materialization receipts
/// come from the shared core-MIR boundary, so direct native and verifier
/// callers cannot grow a second frontend route policy.
pub(crate) fn materialize_canonical_route(
    checked: &CheckedProgram,
    merged_file: &File,
) -> Result<
    mimi::core::mir::CanonicalMirRouteMaterialization,
    mimi::core::mir::CanonicalMirRouteMaterializationError,
> {
    let excluded_sources = merged_file
        .sources
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<HashSet<_>>();
    mimi::core::mir::materialize_canonical_mir_route(checked, Some(&excluded_sources))
}

/// Build the production MIR graph while excluding only the known prelude
/// compatibility source.  User and imported sources remain in the graph and
/// are all required to lower and validate.
pub(crate) fn build_canonical_program(
    checked: &CheckedProgram,
    merged_file: &File,
) -> Result<MirProgram, String> {
    // Explicit `--mir` is a construction/inspection request rather than a
    // default-route admission. Preserve the canonical builder's detailed
    // lowering/validation diagnostics here; the default selector uses the
    // shared route envelope above for Complete-vs-Compatibility disposition.
    build_canonical_program_for_sources(checked, merged_file, None)
}

/// Build the same canonical graph as the production dispatcher, optionally
/// restricting the graph to a source scope for `mimi mir` inspection.  The
/// source filter is a graph-selection concern only; lowering, generic
/// instance materialization, TypeDesc construction, and validation all remain
/// in the single `MirProgram` production constructor.
pub(crate) fn build_canonical_program_for_sources(
    checked: &CheckedProgram,
    merged_file: &File,
    included_sources: Option<&HashSet<mimi::span::SourceId>>,
) -> Result<MirProgram, String> {
    let excluded_sources = merged_file
        .sources
        .records()
        .iter()
        .filter(|record| {
            record.key.as_str() == "stdlib:prelude.mimi"
                || included_sources.is_some_and(|included| !included.contains(&record.id))
        })
        .map(|record| record.id)
        .collect::<HashSet<_>>();
    MirProgram::from_checked_program_excluding_sources(checked, &excluded_sources)
        .map_err(|error| format!("canonical MIR build error: {error:?}"))
}

/// Select a default route for a complete checked program.
///
/// The current default-switch islands are deliberately narrow: a program must
/// contain either a checker-selected scalar Set facade instance, a flat Copy
/// record value, a concrete scalar List operation (`len`/`reverse`), an exact S8 Flow
/// transition, the concrete non-Copy `Option<string>`/Copy `Option<i32>`/`Option<bool>`/`Option<i64>`/`Option<f64>`/`Result<i32, i32>` variant islands (including `unwrap_or`), or the
/// generic `Option<T>.is_some`/`is_none` predicate island, the generic
/// `Option<T>.unwrap()` projection island, generic `Option<T>.unwrap_or(T)`
/// fallback projection island, generic `Result<T, T>`/`Result<T, i32>`
/// `unwrap()`, or generic `Result<T, T>.unwrap_or(T)` /
/// `Result<T, i32>.unwrap_or(T)` fallback projection
/// island.
/// The candidate then
/// has to pass every consumer preflight before any caller starts execution or
/// LLVM emission. A `Legacy(reason)` result is an explicit
/// compatibility disposition for a program that has not entered a migrated
/// island. `Rejected` means an island was recognized but canonical preflight
/// failed; callers must report the error and must not invoke legacy.
pub(crate) fn select_default_route(
    checked: &CheckedProgram,
    merged_file: &File,
) -> DefaultMirRoute {
    // Admission is checker-owned and must happen before MIR construction.
    // The shared envelope also owns the materialization receipts, preventing
    // this selector from growing a second Set/record lowering walk.
    let admission = mimi::core::mir::classify_canonical_mir_route_admission(checked);
    let collection_admission = admission.collection;
    let option_string_admission = admission.option_string;
    let generic_variant_admission = admission.generic_variant;
    let generic_option_projection_admission = admission.generic_option_projection;
    let generic_option_projection_fallback_admission = admission.generic_option_projection_fallback;
    let generic_result_projection_admission = admission.generic_result_projection;
    let generic_result_projection_fallback_admission = admission.generic_result_projection_fallback;
    let copy_option_i32_admission = admission.copy_option_i32;
    let copy_option_bool_admission = admission.copy_option_bool;
    let copy_option_i64_admission = admission.copy_option_i64;
    let copy_option_f64_admission = admission.copy_option_f64;
    let copy_result_i32_admission = admission.copy_result_i32;
    // Imported stdlib facades are part of the production island once their
    // concrete operations materialize in MIR.  Do not use the retained File
    // import list as a second route policy: checker admission records the
    // mixed provenance, while the canonical MIR island validator decides
    // whether the complete imported executable graph is covered.  This is
    // what lets `std::set` enter the same default Set island without opening
    // an implicit legacy fallback for an unsupported imported graph.
    let collection_hint = !matches!(
        collection_admission,
        mimi::core::mir::ScalarCollectionAdmission::OutsideProfile
    );
    let complete_collection_candidate = matches!(
        collection_admission,
        mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
    );
    let record_admission = admission.record;
    let record_hint = !matches!(
        record_admission,
        mimi::core::mir::FlatCopyRecordAdmission::OutsideProfile
    );
    let complete_record_candidate = matches!(
        record_admission,
        mimi::core::mir::FlatCopyRecordAdmission::CompleteCoverage
    );
    let option_string_hint = !matches!(
        option_string_admission,
        mimi::core::mir::OptionStringVariantAdmission::OutsideProfile
    );
    let complete_option_string_candidate = matches!(
        option_string_admission,
        mimi::core::mir::OptionStringVariantAdmission::CompleteCoverage
    );
    let generic_variant_hint = !matches!(
        generic_variant_admission,
        mimi::core::mir::GenericVariantPredicateAdmission::OutsideProfile
    );
    let complete_generic_variant_candidate = matches!(
        generic_variant_admission,
        mimi::core::mir::GenericVariantPredicateAdmission::CompleteCoverage
    );
    let generic_option_projection_unsupported_hint =
        mimi::core::mir::has_unsupported_generic_option_projection_candidate(checked);
    let generic_option_projection_hint = generic_option_projection_unsupported_hint
        || !matches!(
            generic_option_projection_admission,
            mimi::core::mir::GenericOptionProjectionAdmission::OutsideProfile
        );
    let complete_generic_option_projection_candidate = matches!(
        generic_option_projection_admission,
        mimi::core::mir::GenericOptionProjectionAdmission::CompleteCoverage
    );
    let generic_option_projection_fallback_unsupported_hint =
        mimi::core::mir::has_unsupported_generic_option_projection_fallback_candidate(checked);
    let generic_option_projection_fallback_hint =
        generic_option_projection_fallback_unsupported_hint
            || !matches!(
                generic_option_projection_fallback_admission,
                mimi::core::mir::GenericOptionProjectionFallbackAdmission::OutsideProfile
            );
    let complete_generic_option_projection_fallback_candidate = matches!(
        generic_option_projection_fallback_admission,
        mimi::core::mir::GenericOptionProjectionFallbackAdmission::CompleteCoverage
    );
    let generic_result_projection_unsupported_hint =
        mimi::core::mir::has_unsupported_generic_result_projection_candidate(checked);
    let generic_result_projection_hint = generic_result_projection_unsupported_hint
        || !matches!(
            generic_result_projection_admission,
            mimi::core::mir::GenericResultProjectionAdmission::OutsideProfile
        );
    let complete_generic_result_projection_candidate = matches!(
        generic_result_projection_admission,
        mimi::core::mir::GenericResultProjectionAdmission::CompleteCoverage
    );
    let generic_result_projection_fallback_unsupported_hint =
        mimi::core::mir::has_unsupported_generic_result_projection_fallback_candidate(checked);
    let generic_result_projection_fallback_hint =
        generic_result_projection_fallback_unsupported_hint
            || !matches!(
                generic_result_projection_fallback_admission,
                mimi::core::mir::GenericResultProjectionFallbackAdmission::OutsideProfile
            );
    let complete_generic_result_projection_fallback_candidate = matches!(
        generic_result_projection_fallback_admission,
        mimi::core::mir::GenericResultProjectionFallbackAdmission::CompleteCoverage
    );
    let copy_option_i32_hint = !matches!(
        copy_option_i32_admission,
        mimi::core::mir::CopyOptionI32VariantAdmission::OutsideProfile
    );
    let complete_copy_option_i32_candidate = matches!(
        copy_option_i32_admission,
        mimi::core::mir::CopyOptionI32VariantAdmission::CompleteCoverage
    );
    let copy_option_bool_hint = !matches!(
        copy_option_bool_admission,
        mimi::core::mir::CopyOptionI32VariantAdmission::OutsideProfile
    );
    let complete_copy_option_bool_candidate = matches!(
        copy_option_bool_admission,
        mimi::core::mir::CopyOptionI32VariantAdmission::CompleteCoverage
    );
    let copy_option_i64_hint = !matches!(
        copy_option_i64_admission,
        mimi::core::mir::CopyOptionI32VariantAdmission::OutsideProfile
    );
    let complete_copy_option_i64_candidate = matches!(
        copy_option_i64_admission,
        mimi::core::mir::CopyOptionI32VariantAdmission::CompleteCoverage
    );
    let copy_option_f64_hint = !matches!(
        copy_option_f64_admission,
        mimi::core::mir::CopyOptionI32VariantAdmission::OutsideProfile
    );
    let complete_copy_option_f64_candidate = matches!(
        copy_option_f64_admission,
        mimi::core::mir::CopyOptionI32VariantAdmission::CompleteCoverage
    );
    let copy_result_i32_hint = !matches!(
        copy_result_i32_admission,
        mimi::core::mir::CopyResultI32VariantAdmission::OutsideProfile
    );
    let complete_copy_result_i32_candidate = matches!(
        copy_result_i32_admission,
        mimi::core::mir::CopyResultI32VariantAdmission::CompleteCoverage
    );
    let flow_candidate = may_contain_single_silent_local_transition(checked, merged_file);
    let complete_flow_candidate = matches!(
        admission.flow,
        mimi::core::mir::S8FlowAdmission::CompleteCoverage
    );
    if !collection_hint
        && !record_hint
        && !flow_candidate
        && !option_string_hint
        && !generic_variant_hint
        && !generic_option_projection_hint
        && !generic_option_projection_fallback_hint
        && !generic_result_projection_hint
        && !generic_result_projection_fallback_hint
        && !copy_option_i32_hint
        && !copy_option_bool_hint
        && !copy_option_i64_hint
        && !copy_option_f64_hint
        && !copy_result_i32_hint
    {
        return DefaultMirRoute::Legacy(LegacyRouteReason::OutsideMigratedProfile);
    }
    if generic_variant_hint && !complete_generic_variant_candidate {
        return reject_migrated_candidates(
            flow_candidate,
            false,
            true,
            option_string_hint,
            "generic variant predicate candidate is outside complete coverage",
        );
    }
    if generic_option_projection_hint && !complete_generic_option_projection_candidate {
        return DefaultMirRoute::Rejected(
            "generic Option projection candidate is outside complete coverage".into(),
        );
    }
    if generic_option_projection_fallback_hint
        && !complete_generic_option_projection_fallback_candidate
    {
        return DefaultMirRoute::Rejected(
            "generic Option fallback projection candidate is outside complete coverage".into(),
        );
    }
    if generic_result_projection_hint && !complete_generic_result_projection_candidate {
        return DefaultMirRoute::Rejected(
            "generic Result projection candidate is outside complete coverage".into(),
        );
    }
    if generic_result_projection_fallback_hint
        && !complete_generic_result_projection_fallback_candidate
    {
        return DefaultMirRoute::Rejected(
            "generic Result fallback projection candidate is outside complete coverage".into(),
        );
    }
    if copy_option_i32_hint && !complete_copy_option_i32_candidate {
        return reject_migrated_candidates_with_copy(
            flow_candidate,
            false,
            false,
            option_string_hint,
            true,
            false,
            "Copy Option<i32> projection candidate is outside complete coverage",
        );
    }
    if copy_option_bool_hint && !complete_copy_option_bool_candidate {
        return reject_migrated_candidates_with_copy(
            flow_candidate,
            false,
            false,
            option_string_hint,
            false,
            true,
            "Copy Option<bool> projection candidate is outside complete coverage",
        );
    }
    if copy_option_i64_hint && !complete_copy_option_i64_candidate {
        return reject_migrated_candidates_with_copy_i64(
            flow_candidate,
            false,
            false,
            option_string_hint,
            false,
            false,
            true,
            "Copy Option<i64> projection candidate is outside complete coverage",
        );
    }
    if copy_option_f64_hint && !complete_copy_option_f64_candidate {
        return reject_migrated_candidates_with_copy_f64(
            flow_candidate,
            false,
            false,
            option_string_hint,
            false,
            false,
            false,
            true,
            false,
            "Copy Option<f64> projection candidate is outside complete coverage",
        );
    }
    if copy_result_i32_hint && !complete_copy_result_i32_candidate {
        return reject_migrated_candidates_with_copy_result(
            flow_candidate,
            false,
            false,
            option_string_hint,
            copy_option_i32_hint,
            copy_option_bool_hint,
            copy_option_i64_hint,
            copy_option_f64_hint,
            true,
            "Copy Result<i32, i32> projection candidate is outside complete coverage",
        );
    }

    let route = match materialize_canonical_route(checked, merged_file) {
        Ok(route) => route,
        Err(mimi::core::mir::CanonicalMirRouteMaterializationError::Complete {
            profile,
            stage,
            message,
        }) => {
            let reason = match stage {
                mimi::core::mir::CanonicalMirRouteFailureStage::Construction => {
                    if matches!(
                        profile,
                        mimi::core::mir::CanonicalMirRouteProfile::GenericOptionPredicate
                            | mimi::core::mir::CanonicalMirRouteProfile::GenericOptionProjection
                            | mimi::core::mir::CanonicalMirRouteProfile::GenericOptionProjectionFallback
                            | mimi::core::mir::CanonicalMirRouteProfile::GenericResultProjection
                            | mimi::core::mir::CanonicalMirRouteProfile::GenericResultProjectionFallback
                    ) {
                        if matches!(
                            profile,
                            mimi::core::mir::CanonicalMirRouteProfile::GenericOptionProjection
                        ) {
                            format!(
                                "generic Option projection canonical MIR construction failed: {message}"
                            )
                        } else if matches!(
                            profile,
                            mimi::core::mir::CanonicalMirRouteProfile::GenericResultProjection
                        ) {
                            format!(
                                "generic Result projection canonical MIR construction failed: {message}"
                            )
                        } else if matches!(
                            profile,
                            mimi::core::mir::CanonicalMirRouteProfile::GenericOptionProjectionFallback
                        ) {
                            format!(
                                "generic Option fallback projection canonical MIR construction failed: {message}"
                            )
                        } else if matches!(
                            profile,
                            mimi::core::mir::CanonicalMirRouteProfile::GenericResultProjectionFallback
                        ) {
                            format!(
                                "generic Result fallback projection canonical MIR construction failed: {message}"
                            )
                        } else {
                            format!(
                                "generic variant predicate canonical MIR construction failed: {message}"
                            )
                        }
                    } else {
                        format!("canonical MIR construction failed: {message}")
                    }
                }
                mimi::core::mir::CanonicalMirRouteFailureStage::Coverage => {
                    if matches!(
                        profile,
                        mimi::core::mir::CanonicalMirRouteProfile::GenericOptionPredicate
                            | mimi::core::mir::CanonicalMirRouteProfile::GenericOptionProjection
                            | mimi::core::mir::CanonicalMirRouteProfile::GenericOptionProjectionFallback
                            | mimi::core::mir::CanonicalMirRouteProfile::GenericResultProjection
                            | mimi::core::mir::CanonicalMirRouteProfile::GenericResultProjectionFallback
                    ) {
                        if matches!(
                            profile,
                            mimi::core::mir::CanonicalMirRouteProfile::GenericOptionProjection
                        ) {
                            format!("generic Option projection canonical graph did not materialize the selected production operation: {message}")
                        } else if matches!(
                            profile,
                            mimi::core::mir::CanonicalMirRouteProfile::GenericResultProjection
                        ) {
                            format!("generic Result projection canonical graph did not materialize the selected production operation: {message}")
                        } else if matches!(
                            profile,
                            mimi::core::mir::CanonicalMirRouteProfile::GenericOptionProjectionFallback
                        ) {
                            format!("generic Option fallback projection canonical graph did not materialize the selected production operation: {message}")
                        } else if matches!(
                            profile,
                            mimi::core::mir::CanonicalMirRouteProfile::GenericResultProjectionFallback
                        ) {
                            format!("generic Result fallback projection canonical graph did not materialize the selected production operation: {message}")
                        } else {
                            format!("generic variant predicate canonical graph did not materialize the selected production operation: {message}")
                        }
                    } else {
                        format!("canonical graph did not materialize the selected production operation: {message}")
                    }
                }
            };
            return reject_migrated_candidates_with_copy_f64(
                flow_candidate,
                matches!(
                    profile,
                    mimi::core::mir::CanonicalMirRouteProfile::ScalarCollection
                ),
                matches!(
                    profile,
                    mimi::core::mir::CanonicalMirRouteProfile::FlatCopyRecord
                ) || matches!(
                    profile,
                    mimi::core::mir::CanonicalMirRouteProfile::GenericOptionPredicate
                        | mimi::core::mir::CanonicalMirRouteProfile::GenericOptionProjection
                        | mimi::core::mir::CanonicalMirRouteProfile::GenericOptionProjectionFallback
                        | mimi::core::mir::CanonicalMirRouteProfile::GenericResultProjection
                        | mimi::core::mir::CanonicalMirRouteProfile::GenericResultProjectionFallback
                ),
                matches!(
                    profile,
                    mimi::core::mir::CanonicalMirRouteProfile::NonCopyOptionStringVariant
                ),
                matches!(
                    profile,
                    mimi::core::mir::CanonicalMirRouteProfile::CopyOptionI32Variant
                ),
                matches!(
                    profile,
                    mimi::core::mir::CanonicalMirRouteProfile::CopyOptionBoolVariant
                ),
                matches!(
                    profile,
                    mimi::core::mir::CanonicalMirRouteProfile::CopyOptionI64Variant
                ),
                matches!(
                    profile,
                    mimi::core::mir::CanonicalMirRouteProfile::CopyOptionF64Variant
                ),
                matches!(
                    profile,
                    mimi::core::mir::CanonicalMirRouteProfile::CopyResultI32Variant
                ),
                reason,
            );
        }
        Err(mimi::core::mir::CanonicalMirRouteMaterializationError::Compatibility { .. }) => {
            // A mixed graph with no migrated operation retains the explicit
            // compatibility route.  A checker-recognized List operation is
            // different: its unsupported shape must fail closed even when
            // canonical construction cannot produce a receipt, otherwise a
            // List<T> contract hole would silently enter legacy.
            if collection_hint && mimi::core::mir::has_unsupported_list_reverse_candidate(checked) {
                return reject_migrated_candidates(
                    flow_candidate,
                    true,
                    false,
                    false,
                    "canonical List.reverse candidate did not materialize a supported MIR shape",
                );
            }
            if collection_hint && mimi::core::mir::has_unsupported_list_concat_candidate(checked) {
                return reject_migrated_candidates(
                    flow_candidate,
                    true,
                    false,
                    false,
                    "canonical List.concat candidate did not materialize a supported MIR shape",
                );
            }
            if collection_hint
                && mimi::core::mir::has_unsupported_generic_list_facade_candidate(checked)
            {
                return reject_migrated_candidates(
                    flow_candidate,
                    true,
                    false,
                    false,
                    "canonical generic List facade candidate did not materialize a supported scalar MIR shape",
                );
            }
            if record_hint
                && mimi::core::mir::has_unsupported_generic_record_projection_candidate(checked)
            {
                return reject_migrated_candidates(
                    flow_candidate,
                    false,
                    true,
                    false,
                    "canonical generic record projection candidate did not materialize a supported scalar MIR shape",
                );
            }
            if generic_variant_hint
                && mimi::core::mir::has_unsupported_generic_variant_predicate_candidate(checked)
            {
                return reject_migrated_candidates(
                    flow_candidate,
                    false,
                    true,
                    false,
                    "canonical generic variant predicate candidate did not materialize a supported MIR shape",
                );
            }
            if generic_result_projection_hint
                && mimi::core::mir::has_unsupported_generic_result_projection_candidate(checked)
            {
                return DefaultMirRoute::Rejected(
                    "generic Result projection candidate did not materialize a supported MIR shape"
                        .into(),
                );
            }
            if generic_result_projection_fallback_hint
                && mimi::core::mir::has_unsupported_generic_result_projection_fallback_candidate(
                    checked,
                )
            {
                return DefaultMirRoute::Rejected(
                    "generic Result fallback projection candidate did not materialize a supported MIR shape"
                        .into(),
                );
            }
            if generic_option_projection_fallback_hint
                && mimi::core::mir::has_unsupported_generic_option_projection_fallback_candidate(
                    checked,
                )
            {
                return DefaultMirRoute::Rejected(
                    "generic Option fallback projection candidate did not materialize a supported MIR shape"
                        .into(),
                );
            }
            // S8 keeps its existing candidate hard boundary: the front-end
            // candidate predicate is intentionally stricter than
            // collection/record compatibility and must not fall back.
            if flow_candidate {
                return reject_migrated_candidates(
                    true,
                    false,
                    false,
                    false,
                    "canonical MIR candidate materialization failed",
                );
            }
            if option_string_hint {
                return reject_migrated_candidates(
                    false,
                    false,
                    false,
                    true,
                    "canonical MIR candidate materialization failed",
                );
            }
            if copy_option_i32_hint {
                return reject_migrated_candidates_with_copy(
                    false,
                    false,
                    false,
                    false,
                    true,
                    false,
                    "canonical Copy Option<i32> projection candidate did not materialize a supported MIR shape",
                );
            }
            if copy_option_bool_hint {
                return reject_migrated_candidates_with_copy(
                    false,
                    false,
                    false,
                    false,
                    false,
                    true,
                    "canonical Copy Option<bool> projection candidate did not materialize a supported MIR shape",
                );
            }
            if copy_option_i64_hint {
                return reject_migrated_candidates_with_copy_i64(
                    false,
                    false,
                    false,
                    false,
                    false,
                    false,
                    true,
                    "canonical Copy Option<i64> projection candidate did not materialize a supported MIR shape",
                );
            }
            if copy_option_f64_hint {
                return reject_migrated_candidates_with_copy_f64(
                    false,
                    false,
                    false,
                    false,
                    false,
                    false,
                    false,
                    true,
                    false,
                    "canonical Copy Option<f64> projection candidate did not materialize a supported MIR shape",
                );
            }
            if copy_result_i32_hint {
                return reject_migrated_candidates_with_copy_result(
                    false,
                    false,
                    false,
                    false,
                    false,
                    false,
                    false,
                    false,
                    true,
                    "canonical Copy Result<i32, i32> projection candidate did not materialize a supported MIR shape",
                );
            }
            return DefaultMirRoute::Legacy(
                LegacyRouteReason::MixedCoverageWithoutMaterializedCandidate,
            );
        }
    };
    let canonical = &route.program;
    let copy_record = route.materialized_record_candidate;
    let materialized_collection_candidate = route.materialized_collection_candidate;
    let materialized_flow_candidate = route.materialized_flow_candidate;
    let materialized_option_string_candidate = route.materialized_option_string_candidate;
    let materialized_generic_variant_candidate = route.materialized_generic_variant_candidate;
    let materialized_generic_option_projection_candidate =
        route.materialized_generic_option_projection_candidate;
    let materialized_generic_option_projection_fallback_candidate =
        route.materialized_generic_option_projection_fallback_candidate;
    let materialized_generic_result_projection_candidate =
        route.materialized_generic_result_projection_candidate;
    let materialized_generic_result_projection_fallback_candidate =
        route.materialized_generic_result_projection_fallback_candidate;
    let materialized_copy_option_i32_candidate = route.materialized_copy_option_i32_candidate;
    let materialized_copy_option_bool_candidate = route.materialized_copy_option_bool_candidate;
    let materialized_copy_option_i64_candidate = route.materialized_copy_option_i64_candidate;
    let materialized_copy_option_f64_candidate = route.materialized_copy_option_f64_candidate;
    let materialized_copy_result_i32_candidate = route.materialized_copy_result_i32_candidate;
    let flow_route_candidate = flow_candidate || materialized_flow_candidate;
    let flow_transition_operation =
        mimi::core::mir::contains_s8_flow_transition_candidate(canonical);
    // Mixed coverage remains a compatibility boundary only when construction
    // proves that no migrated operation was materialized.  A Complete
    // admission missing its receipt, however, is a hard route failure.
    let collection_route_candidate =
        complete_collection_candidate || (collection_hint && materialized_collection_candidate);
    let generic_route_candidate = (complete_generic_variant_candidate
        && materialized_generic_variant_candidate)
        || (complete_generic_option_projection_candidate
            && materialized_generic_option_projection_candidate)
        || (complete_generic_option_projection_fallback_candidate
            && materialized_generic_option_projection_fallback_candidate)
        || (complete_generic_result_projection_candidate
            && materialized_generic_result_projection_candidate)
        || (complete_generic_result_projection_fallback_candidate
            && materialized_generic_result_projection_fallback_candidate);
    let record_route_candidate =
        complete_record_candidate || (record_hint && copy_record) || generic_route_candidate;
    let option_string_route_candidate = complete_option_string_candidate
        || (option_string_hint && materialized_option_string_candidate);
    let copy_option_i32_route_candidate = complete_copy_option_i32_candidate
        || (copy_option_i32_hint && materialized_copy_option_i32_candidate);
    let copy_option_bool_route_candidate = complete_copy_option_bool_candidate
        || (copy_option_bool_hint && materialized_copy_option_bool_candidate);
    let copy_option_i64_route_candidate = complete_copy_option_i64_candidate
        || (copy_option_i64_hint && materialized_copy_option_i64_candidate);
    let copy_option_f64_route_candidate = complete_copy_option_f64_candidate
        || (copy_option_f64_hint && materialized_copy_option_f64_candidate);
    let copy_result_i32_route_candidate = complete_copy_result_i32_candidate
        || (copy_result_i32_hint && materialized_copy_result_i32_candidate);
    if flow_route_candidate && !flow_transition_operation {
        return reject_migrated_candidates_with_copy_f64(
            flow_route_candidate,
            collection_route_candidate || (record_route_candidate && copy_record),
            record_route_candidate,
            option_string_route_candidate,
            copy_option_i32_route_candidate,
            copy_option_bool_route_candidate,
            copy_option_i64_route_candidate,
            copy_option_f64_route_candidate,
            copy_result_i32_route_candidate,
            "canonical graph did not materialize the selected production operation",
        );
    }
    if flow_candidate && !complete_flow_candidate {
        return reject_migrated_candidates_with_copy_f64(
            true,
            collection_route_candidate,
            record_route_candidate,
            option_string_route_candidate,
            copy_option_i32_route_candidate,
            copy_option_bool_route_candidate,
            copy_option_i64_route_candidate,
            copy_option_f64_route_candidate,
            copy_result_i32_route_candidate,
            "S8 Flow transition candidate is not complete coverage",
        );
    }

    // A mixed program is not a partial canonical program.  If its graph does
    // contain a migrated boundary, keep the old path deleted for that
    // boundary and reject the whole route.  If it contains no such operation,
    // it remains an explicit compatibility input and may use Legacy.
    if record_route_candidate && !complete_record_candidate && !generic_route_candidate {
        return reject_migrated_candidates_with_copy_f64(
            flow_route_candidate,
            collection_route_candidate,
            true,
            option_string_route_candidate,
            copy_option_i32_route_candidate,
            copy_option_bool_route_candidate,
            copy_option_i64_route_candidate,
            copy_option_f64_route_candidate,
            copy_result_i32_route_candidate,
            "flat Copy record materialized inside mixed coverage",
        );
    }
    if option_string_route_candidate && !complete_option_string_candidate {
        return reject_migrated_candidates_with_copy_f64(
            flow_route_candidate,
            collection_route_candidate,
            record_route_candidate,
            true,
            copy_option_i32_route_candidate,
            copy_option_bool_route_candidate,
            copy_option_i64_route_candidate,
            copy_option_f64_route_candidate,
            copy_result_i32_route_candidate,
            "Option<string> variant materialized inside mixed coverage",
        );
    }
    if copy_option_i32_route_candidate && !complete_copy_option_i32_candidate {
        return reject_migrated_candidates_with_copy_f64(
            flow_route_candidate,
            collection_route_candidate,
            record_route_candidate,
            option_string_route_candidate,
            true,
            copy_option_bool_route_candidate,
            copy_option_i64_route_candidate,
            copy_option_f64_route_candidate,
            false,
            "Copy Option<i32> variant materialized inside mixed coverage",
        );
    }
    if copy_option_bool_route_candidate && !complete_copy_option_bool_candidate {
        return reject_migrated_candidates_with_copy_f64(
            flow_route_candidate,
            collection_route_candidate,
            record_route_candidate,
            option_string_route_candidate,
            copy_option_i32_route_candidate,
            true,
            copy_option_i64_route_candidate,
            copy_option_f64_route_candidate,
            false,
            "Copy Option<bool> variant materialized inside mixed coverage",
        );
    }
    if copy_result_i32_route_candidate && !complete_copy_result_i32_candidate {
        return reject_migrated_candidates_with_copy_result(
            flow_route_candidate,
            collection_route_candidate,
            record_route_candidate,
            option_string_route_candidate,
            copy_option_i32_route_candidate,
            copy_option_bool_route_candidate,
            copy_option_i64_route_candidate,
            copy_option_f64_route_candidate,
            true,
            "Copy Result<i32, i32> variant materialized inside mixed coverage",
        );
    }
    if !collection_route_candidate
        && !record_route_candidate
        && !flow_route_candidate
        && !option_string_route_candidate
        && !copy_option_i32_route_candidate
        && !copy_option_bool_route_candidate
        && !copy_option_i64_route_candidate
        && !copy_option_f64_route_candidate
        && !copy_result_i32_route_candidate
    {
        return DefaultMirRoute::Legacy(
            LegacyRouteReason::MixedCoverageWithoutMaterializedCandidate,
        );
    }

    // S11: the production unit is a complete scalar List/Set executable
    // graph, not an individual opcode.  The island validator consumes only
    // canonical MIR and TypeDesc facts and runs before any verifier/backend
    // preflight.  A real materialized Set facade or List operation is
    // therefore either inside this finite envelope or rejected; it cannot
    // re-enter the legacy route.
    if materialized_collection_candidate {
        if let Err(errors) = mimi::core::mir::validate_scalar_collection_island(&canonical) {
            return reject_migrated_candidates_with_copy_f64(
                flow_route_candidate,
                true,
                record_route_candidate,
                option_string_route_candidate,
                copy_option_i32_route_candidate,
                copy_option_bool_route_candidate,
                copy_option_i64_route_candidate,
                copy_option_f64_route_candidate,
                copy_result_i32_route_candidate,
                format!(
                    "{} capability gate failed: {errors:?}",
                    mimi::core::mir::SCALAR_COLLECTION_ISLAND
                ),
            );
        }
    }

    if materialized_option_string_candidate {
        if let Err(errors) = mimi::core::mir::validate_option_string_variant_island(&canonical) {
            return reject_migrated_candidates_with_copy_f64(
                flow_route_candidate,
                collection_route_candidate,
                record_route_candidate,
                true,
                copy_option_i32_route_candidate,
                copy_option_bool_route_candidate,
                copy_option_i64_route_candidate,
                copy_option_f64_route_candidate,
                copy_result_i32_route_candidate,
                format!(
                    "{} capability gate failed: {errors:?}",
                    mimi::core::mir::NON_COPY_OPTION_STRING_VARIANT_ISLAND
                ),
            );
        }
    }

    if materialized_copy_option_i32_candidate {
        if let Err(errors) = mimi::core::mir::validate_copy_option_i32_variant_island(&canonical) {
            return reject_migrated_candidates_with_copy_f64(
                flow_route_candidate,
                collection_route_candidate,
                record_route_candidate,
                option_string_route_candidate,
                true,
                copy_option_bool_route_candidate,
                copy_option_i64_route_candidate,
                copy_option_f64_route_candidate,
                copy_result_i32_route_candidate,
                format!(
                    "{} capability gate failed: {errors:?}",
                    mimi::core::mir::COPY_OPTION_I32_VARIANT_ISLAND
                ),
            );
        }
    }

    if materialized_copy_option_bool_candidate {
        if let Err(errors) = mimi::core::mir::validate_copy_option_variant_island(
            &canonical,
            mimi::core::PrimitiveType::Bool,
            mimi::core::mir::COPY_OPTION_BOOL_VARIANT_ISLAND,
        ) {
            return reject_migrated_candidates_with_copy_f64(
                flow_route_candidate,
                collection_route_candidate,
                record_route_candidate,
                option_string_route_candidate,
                copy_option_i32_route_candidate,
                true,
                copy_option_i64_route_candidate,
                copy_option_f64_route_candidate,
                copy_result_i32_route_candidate,
                format!(
                    "{} capability gate failed: {errors:?}",
                    mimi::core::mir::COPY_OPTION_BOOL_VARIANT_ISLAND
                ),
            );
        }
    }

    if materialized_copy_option_i64_candidate {
        if let Err(errors) = mimi::core::mir::validate_copy_option_i64_variant_island(&canonical) {
            return reject_migrated_candidates_with_copy_f64(
                flow_route_candidate,
                collection_route_candidate,
                record_route_candidate,
                option_string_route_candidate,
                copy_option_i32_route_candidate,
                copy_option_bool_route_candidate,
                true,
                copy_option_f64_route_candidate,
                copy_result_i32_route_candidate,
                format!(
                    "{} capability gate failed: {errors:?}",
                    mimi::core::mir::COPY_OPTION_I64_VARIANT_ISLAND
                ),
            );
        }
    }

    if materialized_copy_option_f64_candidate {
        if let Err(errors) = mimi::core::mir::validate_copy_option_f64_variant_island(&canonical) {
            return reject_migrated_candidates_with_copy_f64(
                flow_route_candidate,
                collection_route_candidate,
                record_route_candidate,
                option_string_route_candidate,
                copy_option_i32_route_candidate,
                copy_option_bool_route_candidate,
                copy_option_i64_route_candidate,
                true,
                copy_result_i32_route_candidate,
                format!(
                    "{} capability gate failed: {errors:?}",
                    mimi::core::mir::COPY_OPTION_F64_VARIANT_ISLAND
                ),
            );
        }
    }

    if materialized_copy_result_i32_candidate {
        if let Err(errors) = mimi::core::mir::validate_copy_result_i32_variant_island(&canonical) {
            return reject_migrated_candidates_with_copy_result(
                flow_route_candidate,
                collection_route_candidate,
                record_route_candidate,
                option_string_route_candidate,
                copy_option_i32_route_candidate,
                copy_option_bool_route_candidate,
                copy_option_i64_route_candidate,
                copy_option_f64_route_candidate,
                true,
                format!(
                    "{} capability gate failed: {errors:?}",
                    mimi::core::mir::COPY_RESULT_I32_VARIANT_ISLAND
                ),
            );
        }
    }

    // The MIR verifier intentionally skips bodies with no contract.  That is
    // not permission for an unsupported instruction to enter a default
    // native/bytecode island.  Scan the complete canonical graph before the
    // verifier's contract pass so every selected consumer has an explicit
    // capability, including no-obligation functions.
    if let Err(error) = mimi::verifier::validate_mir_capabilities(&canonical) {
        return reject_migrated_candidates_with_copy_f64(
            flow_route_candidate,
            collection_route_candidate,
            record_route_candidate,
            option_string_route_candidate,
            copy_option_i32_route_candidate,
            copy_option_bool_route_candidate,
            copy_option_i64_route_candidate,
            copy_option_f64_route_candidate,
            copy_result_i32_route_candidate,
            format!("verifier capability gate failed: {error:?}"),
        );
    }

    // Bytecode and native are both checked only after the verifier capability
    // gate.  The actual consumers repeat their own validation immediately
    // before use.
    if let Err(errors) = mimi::interp::bytecode::compile_mir_program(&canonical) {
        return reject_migrated_candidates_with_copy_f64(
            flow_route_candidate,
            collection_route_candidate,
            record_route_candidate,
            option_string_route_candidate,
            copy_option_i32_route_candidate,
            copy_option_bool_route_candidate,
            copy_option_i64_route_candidate,
            copy_option_f64_route_candidate,
            copy_result_i32_route_candidate,
            format!("MIR-bytecode preflight failed: {errors:?}"),
        );
    }
    if let Err(errors) = mimi::codegen::mir::validate_mir_native(&canonical) {
        return reject_migrated_candidates_with_copy_f64(
            flow_route_candidate,
            collection_route_candidate,
            record_route_candidate,
            option_string_route_candidate,
            copy_option_i32_route_candidate,
            copy_option_bool_route_candidate,
            copy_option_i64_route_candidate,
            copy_option_f64_route_candidate,
            copy_result_i32_route_candidate,
            format!("native MIR preflight failed: {errors:?}"),
        );
    }

    // The verifier is a fourth consumer of the same program.  A definitive
    // disproven result is still a valid verifier observation; an unsupported
    // or inconclusive verifier result means this program is not yet a complete
    // default-switch island.
    let verifier_ready = match mimi::verifier::verify_mir(&canonical, String::new()) {
        Ok(results) => results.iter().all(|result| {
            matches!(
                result.status,
                VerifStatus::Proven | VerifStatus::NoObligations | VerifStatus::Disproven
            )
        }),
        Err(error) => {
            return reject_migrated_candidates_with_copy_f64(
                flow_route_candidate,
                collection_route_candidate,
                record_route_candidate,
                option_string_route_candidate,
                copy_option_i32_route_candidate,
                copy_option_bool_route_candidate,
                copy_option_i64_route_candidate,
                copy_option_f64_route_candidate,
                copy_result_i32_route_candidate,
                format!("verifier contract pass failed: {error}"),
            )
        }
    };
    if !verifier_ready {
        return reject_migrated_candidates_with_copy_f64(
            flow_route_candidate,
            collection_route_candidate,
            record_route_candidate,
            option_string_route_candidate,
            copy_option_i32_route_candidate,
            copy_option_bool_route_candidate,
            copy_option_i64_route_candidate,
            copy_option_f64_route_candidate,
            copy_result_i32_route_candidate,
            "verifier returned an unsupported or inconclusive result",
        );
    }

    DefaultMirRoute::Canonical(route.program)
}

fn reject_migrated_candidates(
    flow_candidate: bool,
    collection_candidate: bool,
    record_candidate: bool,
    option_string_candidate: bool,
    reason: impl Into<String>,
) -> DefaultMirRoute {
    let reason = reason.into();
    if flow_candidate {
        DefaultMirRoute::Rejected(format!(
            "S8 Flow transition candidate is not eligible for the default route: {}",
            reason
        ))
    } else if collection_candidate {
        DefaultMirRoute::Rejected(format!(
            "S11 scalar collection candidate is not eligible for the default route: {}",
            reason
        ))
    } else if reason.contains("generic variant predicate") {
        DefaultMirRoute::Rejected(format!(
            "generic variant predicate candidate is not eligible for the default route: {}",
            reason
        ))
    } else if reason.contains("generic Option projection") {
        DefaultMirRoute::Rejected(format!(
            "generic Option projection candidate is not eligible for the default route: {}",
            reason
        ))
    } else if reason.contains("generic Result projection") {
        DefaultMirRoute::Rejected(format!(
            "generic Result projection candidate is not eligible for the default route: {}",
            reason
        ))
    } else if record_candidate {
        DefaultMirRoute::Rejected(format!(
            "S0 flat Copy record candidate is not eligible for the default route: {}",
            reason
        ))
    } else if option_string_candidate {
        DefaultMirRoute::Rejected(format!(
            "S30 non-Copy Option<string> variant candidate is not eligible for the default route: {}",
            reason
        ))
    } else {
        DefaultMirRoute::Legacy(LegacyRouteReason::OutsideMigratedProfile)
    }
}

fn reject_migrated_candidates_with_copy(
    flow_candidate: bool,
    collection_candidate: bool,
    record_candidate: bool,
    option_string_candidate: bool,
    copy_option_i32_candidate: bool,
    copy_option_bool_candidate: bool,
    reason: impl Into<String>,
) -> DefaultMirRoute {
    let reason = reason.into();
    if flow_candidate || collection_candidate || record_candidate || option_string_candidate {
        reject_migrated_candidates(
            flow_candidate,
            collection_candidate,
            record_candidate,
            option_string_candidate,
            reason,
        )
    } else if copy_option_i32_candidate {
        DefaultMirRoute::Rejected(format!(
            "S114 Copy Option<i32> variant candidate is not eligible for the default route: {}",
            reason
        ))
    } else if copy_option_bool_candidate {
        DefaultMirRoute::Rejected(format!(
            "S115 Copy Option<bool> variant candidate is not eligible for the default route: {}",
            reason
        ))
    } else {
        DefaultMirRoute::Legacy(LegacyRouteReason::OutsideMigratedProfile)
    }
}

fn reject_migrated_candidates_with_copy_i64(
    flow_candidate: bool,
    collection_candidate: bool,
    record_candidate: bool,
    option_string_candidate: bool,
    copy_option_i32_candidate: bool,
    copy_option_bool_candidate: bool,
    copy_option_i64_candidate: bool,
    reason: impl Into<String>,
) -> DefaultMirRoute {
    reject_migrated_candidates_with_copy_f64(
        flow_candidate,
        collection_candidate,
        record_candidate,
        option_string_candidate,
        copy_option_i32_candidate,
        copy_option_bool_candidate,
        copy_option_i64_candidate,
        false,
        false,
        reason,
    )
}

fn reject_migrated_candidates_with_copy_result(
    flow_candidate: bool,
    collection_candidate: bool,
    record_candidate: bool,
    option_string_candidate: bool,
    copy_option_i32_candidate: bool,
    copy_option_bool_candidate: bool,
    copy_option_i64_candidate: bool,
    copy_option_f64_candidate: bool,
    copy_result_i32_candidate: bool,
    reason: impl Into<String>,
) -> DefaultMirRoute {
    reject_migrated_candidates_with_copy_f64(
        flow_candidate,
        collection_candidate,
        record_candidate,
        option_string_candidate,
        copy_option_i32_candidate,
        copy_option_bool_candidate,
        copy_option_i64_candidate,
        copy_option_f64_candidate,
        copy_result_i32_candidate,
        reason,
    )
}

fn reject_migrated_candidates_with_copy_f64(
    flow_candidate: bool,
    collection_candidate: bool,
    record_candidate: bool,
    option_string_candidate: bool,
    copy_option_i32_candidate: bool,
    copy_option_bool_candidate: bool,
    copy_option_i64_candidate: bool,
    copy_option_f64_candidate: bool,
    copy_result_i32_candidate: bool,
    reason: impl Into<String>,
) -> DefaultMirRoute {
    let reason = reason.into();
    if flow_candidate || collection_candidate || record_candidate || option_string_candidate {
        reject_migrated_candidates(
            flow_candidate,
            collection_candidate,
            record_candidate,
            option_string_candidate,
            reason,
        )
    } else if copy_option_i32_candidate {
        DefaultMirRoute::Rejected(format!(
            "S114 Copy Option<i32> variant candidate is not eligible for the default route: {}",
            reason
        ))
    } else if copy_option_bool_candidate {
        DefaultMirRoute::Rejected(format!(
            "S115 Copy Option<bool> variant candidate is not eligible for the default route: {}",
            reason
        ))
    } else if copy_option_i64_candidate {
        DefaultMirRoute::Rejected(format!(
            "S116 Copy Option<i64> variant candidate is not eligible for the default route: {}",
            reason
        ))
    } else if copy_option_f64_candidate {
        DefaultMirRoute::Rejected(format!(
            "S117 Copy Option<f64> variant candidate is not eligible for the default route: {}",
            reason
        ))
    } else if copy_result_i32_candidate {
        DefaultMirRoute::Rejected(format!(
            "S125 Copy Result<i32, i32> variant candidate is not eligible for the default route: {}",
            reason
        ))
    } else {
        DefaultMirRoute::Legacy(LegacyRouteReason::OutsideMigratedProfile)
    }
}

fn may_contain_single_silent_local_transition(
    checked: &CheckedProgram,
    merged_file: &File,
) -> bool {
    // The typed candidate predicate is shared with the native exact-island
    // gate. Keep the merged-file check as a front-end provenance guard: a caller may
    // construct a CheckedProgram before module merging has been reflected in
    // its directory, but an imported program is never the exact S8 island.
    if !merged_file.imports.is_empty() {
        return false;
    }
    mimi::core::mir::is_s8_flow_transition_candidate(checked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(source: &str) -> (CheckedProgram, File) {
        let tokens = mimi::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = mimi::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let checked = mimi::core::check_program(&file).expect("check");
        (checked, file)
    }

    #[test]
    fn copy_record_island_passes_all_default_consumer_gates() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_record_copy.mimi"
        ));
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Canonical(_)
        ));
    }

    #[test]
    fn copy_record_update_is_complete_and_uses_canonical_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_record_update.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_flat_copy_record_admission(&checked),
            mimi::core::mir::FlatCopyRecordAdmission::CompleteCoverage
        );
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Canonical(_)
        ));
    }

    #[test]
    fn generic_copy_record_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_record_projection.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_flat_copy_record_admission(&checked),
            mimi::core::mir::FlatCopyRecordAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic Copy-record projection must select the canonical default route");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarRecordProjection { .. }
        )));
    }

    #[test]
    fn copy_result_i32_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_result_i32_unwrap.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_copy_result_i32_variant_admission(&checked),
            mimi::core::mir::CopyResultI32VariantAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("Copy Result<i32, i32>.unwrap must select the canonical default route");
        };
        assert!(mimi::core::mir::contains_copy_result_i32_variant_candidate(
            &program
        ));
    }

    #[test]
    fn unsupported_copy_result_i64_projection_cannot_reenter_legacy_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_result_i64_i32_rejected.mimi"
        ));
        let route = select_default_route(&checked, &file);
        let DefaultMirRoute::Rejected(reason) = route else {
            panic!("unsupported Copy Result projection must fail closed before legacy");
        };
        assert!(reason.contains("Copy Result<i32, i32>"), "{reason}");
    }

    #[test]
    fn generic_option_predicate_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_option_predicate.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_variant_predicate_admission(&checked),
            mimi::core::mir::GenericVariantPredicateAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic Option predicate must select the canonical default route");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarVariantPredicate { .. }
        )));
    }

    #[test]
    fn generic_option_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_option_unwrap.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_option_projection_admission(&checked),
            mimi::core::mir::GenericOptionProjectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic Option projection must select the canonical default route");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjection { .. }
        )));
    }

    #[test]
    fn generic_owned_option_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_option_unwrap_owned_string.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_option_projection_admission(&checked),
            mimi::core::mir::GenericOptionProjectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("owned generic Option projection must select the canonical default route");
        };
        assert!(program.instances().values().any(|instance| {
            matches!(
                &instance.contract,
                mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership
                            == mimi::core::mir::types::MirOwnership::Move
                        && contract.projection.move_out_glue
                            == mimi::core::mir::types::MirGlueKind::OwnedString
            )
        }));
    }

    #[test]
    fn generic_owned_list_option_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_option_unwrap_owned_list.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_option_projection_admission(&checked),
            mimi::core::mir::GenericOptionProjectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("owned generic Option<List> projection must select the canonical default route");
        };
        assert!(program.instances().values().any(|instance| {
            matches!(
                &instance.contract,
                mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership
                            == mimi::core::mir::types::MirOwnership::Move
                        && contract.projection.move_out_glue
                            == mimi::core::mir::types::MirGlueKind::List
            )
        }));
    }

    #[test]
    fn generic_owned_list_scalar_family_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_option_unwrap_owned_list_scalars.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_option_projection_admission(&checked),
            mimi::core::mir::GenericOptionProjectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic Option<List<i64|bool>> projection must select canonical route");
        };
        let instances = program
            .instances()
            .values()
            .filter(|instance| {
                matches!(
                    &instance.contract,
                    mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership
                            == mimi::core::mir::types::MirOwnership::Move
                        && contract.projection.move_out_glue
                            == mimi::core::mir::types::MirGlueKind::List
                )
            })
            .count();
        assert_eq!(instances, 2);
    }

    #[test]
    fn unsupported_generic_option_projection_cannot_reenter_legacy_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_option_unwrap_rejected.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_option_projection_admission(&checked),
            mimi::core::mir::GenericOptionProjectionAdmission::CompleteCoverage
        );
        let route = select_default_route(&checked, &file);
        let DefaultMirRoute::Rejected(reason) = route else {
            panic!("non-Copy generic Option projection must fail closed before legacy");
        };
        assert!(reason.contains("generic Option projection"), "{reason}");
    }

    #[test]
    fn generic_option_unwrap_or_is_rejected_as_an_unmigrated_projection_shape() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_option_unwrap_or_rejected.mimi"
        ));
        assert!(mimi::core::mir::has_unsupported_generic_option_projection_candidate(&checked));
        let route = select_default_route(&checked, &file);
        let DefaultMirRoute::Rejected(reason) = route else {
            panic!("generic Option unwrap_or must fail closed before legacy");
        };
        assert!(reason.contains("generic Option projection"), "{reason}");
    }

    #[test]
    fn generic_result_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_unwrap.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_result_projection_admission(&checked),
            mimi::core::mir::GenericResultProjectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic Result projection must select the canonical default route");
        };
        assert!(program.instances().values().any(|instance| {
            matches!(
                &instance.contract,
                mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Result"
            )
        }));
    }

    #[test]
    fn generic_result_distinct_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_distinct_unwrap.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_result_projection_admission(&checked),
            mimi::core::mir::GenericResultProjectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic distinct Result projection must select the canonical default route");
        };
        let instance = program.instances().values().find(|instance| {
            matches!(
                &instance.contract,
                mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Result"
            )
        });
        let Some(instance) = instance else {
            panic!("generic distinct Result projection instance is absent");
        };
        let mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract } =
            &instance.contract
        else {
            unreachable!("filtered above");
        };
        let mimi::core::mir::types::MirLayout::Result { ok, error, .. } = &program
            .type_catalog()
            .get(&contract.source_ty)
            .expect("specialized Result TypeDesc")
            .layout
        else {
            panic!("specialized source must retain a Result layout");
        };
        assert_ne!(ok, error);
    }

    #[test]
    fn unsupported_generic_result_projection_cannot_reenter_legacy_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_unwrap_rejected.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_result_projection_admission(&checked),
            mimi::core::mir::GenericResultProjectionAdmission::CompleteCoverage
        );
        let route = select_default_route(&checked, &file);
        let DefaultMirRoute::Rejected(reason) = route else {
            panic!("unsupported generic Result projection must fail closed before legacy");
        };
        assert!(
            reason.contains("generic-result-projection-v1")
                || reason.contains("generic Result projection"),
            "{reason}"
        );
    }

    #[test]
    fn generic_result_unwrap_or_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_unwrap_or.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_result_projection_fallback_admission(&checked),
            mimi::core::mir::GenericResultProjectionFallbackAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic Result unwrap_or must select the canonical default route");
        };
        assert!(program.instances().values().any(|instance| matches!(
            &instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                contract
            } if contract.projection.nominal.as_str() == "builtin:type:Result"
        )));
    }

    #[test]
    fn generic_result_distinct_unwrap_or_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_distinct_unwrap_or.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_result_projection_fallback_admission(&checked),
            mimi::core::mir::GenericResultProjectionFallbackAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic heterogeneous Result unwrap_or must select canonical route");
        };
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Result"
                )
            })
            .expect("generic heterogeneous Result fallback instance");
        let mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
            contract,
        } = &instance.contract
        else {
            unreachable!("filtered above");
        };
        let mimi::core::mir::types::MirLayout::Result { ok, error, .. } = &program
            .type_catalog()
            .get(&contract.source_ty)
            .expect("heterogeneous Result TypeDesc")
            .layout
        else {
            panic!("specialized source must retain Result layout");
        };
        assert_ne!(ok, error);
    }

    #[test]
    fn unsupported_generic_result_unwrap_or_cannot_reenter_legacy_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_unwrap_or_rejected.mimi"
        ));
        assert!(
            mimi::core::mir::has_unsupported_generic_result_projection_fallback_candidate(&checked)
        );
        let route = select_default_route(&checked, &file);
        let DefaultMirRoute::Rejected(reason) = route else {
            panic!("unsupported generic Result unwrap_or must fail closed before legacy");
        };
        assert!(
            reason.contains("generic Result fallback projection"),
            "{reason}"
        );
    }

    #[test]
    fn unsupported_generic_option_predicate_cannot_reenter_legacy_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_option_predicate_rejected.mimi"
        ));
        assert!(mimi::core::mir::has_unsupported_generic_variant_predicate_candidate(&checked));
        let route = select_default_route(&checked, &file);
        let DefaultMirRoute::Rejected(reason) = route else {
            panic!("non-Copy generic Option predicate must fail closed before legacy");
        };
        assert!(reason.contains("generic variant predicate"), "{reason}");
    }

    #[test]
    fn generic_result_predicate_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_predicate.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_variant_predicate_admission(&checked),
            mimi::core::mir::GenericVariantPredicateAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic Result predicate must select the canonical default route");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarVariantPredicate {
                contract: mimi::core::mir::types::MirVariantPredicateContract {
                    predicate: mimi::core::mir::MirVariantPredicate::IsOk,
                    ..
                }
            }
        )));
    }

    #[test]
    fn unsupported_generic_result_predicate_cannot_reenter_legacy_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_predicate_rejected.mimi"
        ));
        assert!(mimi::core::mir::has_unsupported_generic_variant_predicate_candidate(&checked));
        let route = select_default_route(&checked, &file);
        let DefaultMirRoute::Rejected(reason) = route else {
            panic!("non-Copy generic Result predicate must fail closed before legacy");
        };
        assert!(reason.contains("generic variant predicate"), "{reason}");
    }

    #[test]
    fn generic_result_error_slot_predicate_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_error_slot.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_variant_predicate_admission(&checked),
            mimi::core::mir::GenericVariantPredicateAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("Result<i32, T> predicate must select the canonical default route");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarVariantPredicate {
                contract: mimi::core::mir::types::MirVariantPredicateContract {
                    predicate: mimi::core::mir::MirVariantPredicate::IsErr,
                    ..
                }
            }
        )));
    }

    #[test]
    fn scalar_generic_record_projection_i64_and_bool_enter_canonical_default_route() {
        for source in [
            include_str!("../../tests/fixtures/mir_native_generic_record_projection_i64.mimi"),
            include_str!("../../tests/fixtures/mir_native_generic_record_projection_bool.mimi"),
        ] {
            let (checked, file) = checked(source);
            assert_eq!(
                mimi::core::mir::classify_flat_copy_record_admission(&checked),
                mimi::core::mir::FlatCopyRecordAdmission::CompleteCoverage
            );
            let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
                panic!("supported scalar generic record projection must select canonical MIR");
            };
            assert!(program.instances().values().any(|instance| matches!(
                instance.contract,
                mimi::core::mir::MirGenericInstanceContract::ScalarRecordProjection { .. }
            )));
        }
    }

    #[test]
    fn scalar_generic_record_projection_rvalue_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_record_projection_rvalue.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_flat_copy_record_admission(&checked),
            mimi::core::mir::FlatCopyRecordAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic Copy-record rvalue projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarRecordProjection { .. }
        )));
    }

    #[test]
    fn owned_generic_record_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_record_owned_string_projection.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_flat_copy_record_admission(&checked),
            mimi::core::mir::FlatCopyRecordAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("owned generic record projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::OwnedRecordProjection { .. }
        )));
    }

    #[test]
    fn owned_mixed_generic_record_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_record_owned_string_mixed.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_flat_copy_record_admission(&checked),
            mimi::core::mir::FlatCopyRecordAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("mixed owned generic record projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::OwnedRecordProjection {
                ref contract
            } if contract.arity == 2 && contract.name == "value"
        )));
    }

    #[test]
    fn owned_generic_record_projection_with_residual_drop_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_record_owned_string_residual.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_flat_copy_record_admission(&checked),
            mimi::core::mir::FlatCopyRecordAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("owned generic record residual projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                ref contract
            } if contract.projection.arity == 2
                && contract.projection.name == "value"
                && contract.residual.len() == 1
                && contract.residual[0].name == "note"
        )));
    }

    #[test]
    fn owned_generic_record_projection_rvalue_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_record_owned_string_rvalue_call.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_flat_copy_record_admission(&checked),
            mimi::core::mir::FlatCopyRecordAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("owned generic record rvalue projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::OwnedRecordProjection { .. }
        )));
    }

    #[test]
    fn two_field_generic_record_projection_enters_canonical_default_route() {
        let source =
            include_str!("../../tests/fixtures/mir_native_generic_record_projection_pair.mimi");
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("two-field generic record projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarRecordProjection { ref contract }
                if contract.arity == 2 && contract.name == "left"
        )));
    }

    #[test]
    fn mixed_generic_record_projection_enters_canonical_default_route() {
        let source =
            include_str!("../../tests/fixtures/mir_native_generic_record_projection_mixed.mimi");
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("mixed generic record projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.arity == 2 && contract.name == "value"
        )));
    }

    #[test]
    fn three_field_generic_record_projection_is_rejected_before_legacy_route() {
        let source = "type Triple<T> { first: T, second: T, third: T }\nfunc get<T>(triple: Triple<T>) -> T { triple.first }\nfunc main() -> i32 { let triple = Triple { first: 41, second: 7, third: 9 }; get(triple) }";
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("three-field generic record projection must fail closed");
        };
        assert!(reason.contains("S0 flat Copy record candidate"), "{reason}");
    }

    #[test]
    fn three_field_owned_generic_record_projection_is_rejected_before_legacy_route() {
        let source = "type Triple<T> { first: T, second: T, third: T }\nfunc get<T>(triple: Triple<T>) -> T { triple.first }\nfunc main() -> i32 { let triple = Triple { first: \"selected\", second: \"middle\", third: \"residual\" }; let picked = get(triple); drop(picked); 41 }";
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("three-field owned generic record projection must fail closed");
        };
        assert!(reason.contains("S0 flat Copy record candidate"), "{reason}");
        assert!(
            reason.contains("canonical generic record projection candidate did not materialize"),
            "{reason}"
        );
    }

    #[test]
    fn mixed_managed_generic_record_projection_is_rejected_before_legacy_route() {
        let source = "type Tagged<T> { value: T, tag: string }\nfunc get<T>(tagged: Tagged<T>) -> T { tagged.value }\nfunc main() -> i32 { let tagged = Tagged { value: 41, tag: \"managed\" }; let picked = get(tagged); picked }";
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("mixed managed generic record projection must fail closed");
        };
        assert!(reason.contains("S0 flat Copy record candidate"), "{reason}");
    }

    #[test]
    fn mixed_owned_generic_record_projection_with_noncopy_sibling_is_rejected_before_legacy_route()
    {
        let source = "type Tagged<T> { value: T, tag: string }\nfunc get<T>(tagged: Tagged<T>) -> T { tagged.value }\nfunc main() -> i32 { let tagged = Tagged { value: \"owned\", tag: \"residual\" }; let picked = get(tagged); drop(picked); 41 }";
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("non-Copy sibling must fail closed before legacy route");
        };
        assert!(reason.contains("S0 flat Copy record candidate"), "{reason}");
    }

    #[test]
    fn unsupported_owned_generic_record_projection_is_rejected_before_legacy_route() {
        let source = "type Triple<T> { first: T, second: T, third: T }\nfunc get<T>(triple: Triple<T>) -> T { triple.first }\nfunc main() -> i32 { let triple = Triple { first: \"owned\", second: \"keep\", third: \"also\" }; let picked = get(triple); drop(picked); 41 }";
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("three-field owned generic record projection must fail closed");
        };
        assert!(reason.contains("S0 flat Copy record candidate"), "{reason}");
    }

    #[test]
    fn single_silent_local_flow_transition_switches_as_one_complete_island() {
        let source = "flow Counter { state Zero { n: i32 } transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } } } func main() -> i32 { let c = Zero { n: 41 } let c2 = Counter::inc(c) c2.n }";
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("the closed silent-local Flow island should select canonical MIR");
        };
        assert_eq!(program.transitions().len(), 1);
        assert!(mimi::core::mir::contains_s8_flow_transition_candidate(
            &program
        ));
    }

    #[test]
    fn rejected_flow_candidate_cannot_reenter_legacy_route() {
        let source = "flow Counter { state Zero { n: i32 } transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } } } func main() -> i32 { let c = Zero { n: 41 } let c2 = Counter::inc(c) println(c2.n) c2.n }";
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("a recognized Flow candidate must fail closed instead of using legacy");
        };
        assert!(reason.contains("S8 Flow transition candidate"));
    }

    #[test]
    fn rejected_mixed_record_and_owned_variant_cannot_reenter_legacy_route() {
        let source = r#"
            type Point { x: i32 }

            func make_some() -> Result<string, i32> { Ok("owned") }

            func main() -> i32 {
                let point = Point { x: 1 }
                point.x
            }
        "#;
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!(
                "a recognized flat Copy-record candidate must fail closed instead of using legacy"
            );
        };
        assert!(reason.contains("S0 flat Copy record candidate"));
    }

    #[test]
    fn complete_record_materialization_failure_cannot_reenter_legacy_route() {
        let source = r#"
            type Point { x: i32 }

            func make_fn(p: Point) -> func(i32) -> i32 {
                fn(value: i32) -> i32 { value + 1 }
            }

            func main() -> i32 { 0 }
        "#;
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("complete record admission must reject a failed MIR construction");
        };
        assert!(reason.contains("S0 flat Copy record candidate"), "{reason}");
        assert!(
            reason.contains("canonical MIR construction failed"),
            "{reason}"
        );
    }

    #[test]
    fn scalar_collection_candidate_rejects_a_mixed_managed_graph() {
        let source = r#"
            func main() -> i32 {
                let values = [1, 2, 3]
                let count = len(values)
                drop(values)
                let text = "outside"
                drop(text)
                count
            }
        "#;
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("a List.len candidate with a managed value must fail closed");
        };
        assert!(reason.contains("S11 scalar collection candidate"));
        assert!(reason.contains("copy-scalar-collection-v1"));
    }

    #[test]
    fn scalar_collection_reverse_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_list_reverse.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("Copy-scalar List.reverse must select the canonical default route");
        };
        assert!(
            mimi::core::mir::contains_scalar_collection_operation_candidate(&program),
            "the route must retain a materialized List.reverse candidate"
        );
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        mimi::core::mir::MirInstructionKind::ListOp {
                            operation: mimi::core::mir::MirListOperation::Reverse,
                            ..
                        }
                    )
                })
            })
        }));
    }

    #[test]
    fn scalar_collection_generic_list_len_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_list_len.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("Copy-scalar generic List.len must select the canonical default route");
        };
        assert!(program.instances().values().any(|instance| {
            matches!(
                instance.contract,
                mimi::core::mir::MirGenericInstanceContract::ScalarListFacade {
                    operation: mimi::core::mir::MirListOperation::Len
                }
            )
        }));
    }

    #[test]
    fn scalar_collection_generic_list_reverse_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_list_reverse.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("Copy-scalar generic List.reverse must select the canonical default route");
        };
        assert!(program.instances().values().any(|instance| {
            matches!(
                instance.contract,
                mimi::core::mir::MirGenericInstanceContract::ScalarListFacade {
                    operation: mimi::core::mir::MirListOperation::Reverse
                }
            )
        }));
    }

    #[test]
    fn scalar_collection_generic_list_concat_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_list_concat.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("Copy-scalar generic List.concat must select the canonical default route");
        };
        assert!(program.instances().values().any(|instance| {
            matches!(
                instance.contract,
                mimi::core::mir::MirGenericInstanceContract::ScalarListFacade {
                    operation: mimi::core::mir::MirListOperation::Concat
                }
            )
        }));
    }

    #[test]
    fn scalar_collection_generic_list_construct_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_list_construct.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("Copy-scalar generic List construction must select the canonical default route");
        };
        assert!(program.instances().values().any(|instance| {
            matches!(
                instance.contract,
                mimi::core::mir::MirGenericInstanceContract::ScalarListConstruct { .. }
            )
        }));
    }

    #[test]
    fn scalar_collection_generic_list_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_list_projection.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("Copy-scalar generic List projection must select the canonical default route");
        };
        assert!(program.instances().values().any(|instance| {
            matches!(
                instance.contract,
                mimi::core::mir::MirGenericInstanceContract::ScalarListProjection {
                    index_value: 0,
                    ..
                }
            )
        }));
    }

    #[test]
    fn scalar_collection_generic_list_index_one_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_list_projection_index_one.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("Copy-scalar generic List index-one projection must select canonical route");
        };
        assert!(program.instances().values().any(|instance| {
            matches!(
                instance.contract,
                mimi::core::mir::MirGenericInstanceContract::ScalarListProjection {
                    index_value: 1,
                    ..
                }
            )
        }));
    }

    #[test]
    fn unsupported_generic_list_facade_cannot_reenter_legacy_route() {
        let source = r#"
            func list_concat<T>(left: List<T>, right: List<T>) -> List<T> {
                left.concat(right)
            }

            func main() -> i32 {
                let left: List<string> = ["a"]
                let right: List<string> = ["b"]
                let joined = list_concat(left, right)
                let count = len(joined)
                drop(left)
                drop(right)
                drop(joined)
                count
            }
        "#;
        let (checked, file) = checked(source);
        assert!(mimi::core::mir::has_unsupported_generic_list_facade_candidate(&checked));
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("unsupported generic List facade must fail closed instead of using legacy");
        };
        assert!(
            reason.contains("S11 scalar collection candidate"),
            "{reason}"
        );
        assert!(reason.contains("generic List facade"), "{reason}");
    }

    #[test]
    fn unsupported_generic_list_construct_cannot_reenter_legacy_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_list_construct_rejected.mimi"
        ));
        assert!(mimi::core::mir::has_unsupported_generic_list_facade_candidate(&checked));
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("managed generic List construction must fail closed instead of using legacy");
        };
        assert!(
            reason.contains("S11 scalar collection candidate"),
            "{reason}"
        );
        assert!(reason.contains("generic List facade"), "{reason}");
    }

    #[test]
    fn unsupported_generic_list_projection_cannot_reenter_legacy_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_list_projection_rejected.mimi"
        ));
        assert!(mimi::core::mir::has_unsupported_generic_list_facade_candidate(&checked));
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("managed generic List projection must fail closed instead of using legacy");
        };
        assert!(
            reason.contains("S11 scalar collection candidate"),
            "{reason}"
        );
        assert!(reason.contains("generic List facade"), "{reason}");
    }

    #[test]
    fn scalar_collection_reverse_with_auto_prelude_enters_canonical_default_route() {
        let source = include_str!("../../tests/fixtures/mir_native_list_reverse.mimi");
        let tokens = mimi::lexer::Lexer::new(source).tokenize().expect("lex");
        let mut file = mimi::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        mimi::loader::merge_prelude_into(&mut file);
        let checked = mimi::core::check_program(&file).expect("check");
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::MixedCoverage
        );
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Canonical(_)
        ));
    }

    #[test]
    fn scalar_collection_reverse_method_enters_canonical_default_route() {
        let (checked, file) = checked(
            "func main() -> i32 { let values = [1, 2, 3]; let reversed = values.reverse(); let n = len(reversed); drop(reversed); drop(values); n }",
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("List.reverse method must select the canonical default route");
        };
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        mimi::core::mir::MirInstructionKind::ListOp {
                            operation: mimi::core::mir::MirListOperation::Reverse,
                            list_operation_contract: Some(_),
                            ..
                        }
                    )
                })
            })
        }));
    }

    #[test]
    fn scalar_collection_concat_method_enters_canonical_default_route() {
        let (checked, file) = checked(
            "func main() -> i32 { let left = [1, 2]; let right = [3, 4]; let joined = left.concat(right); let n = len(joined); drop(joined); n }",
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("List.concat method must select the canonical default route");
        };
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        mimi::core::mir::MirInstructionKind::ListOp {
                            operation: mimi::core::mir::MirListOperation::Concat,
                            argument: Some(_),
                            list_operation_contract: Some(_),
                            ..
                        }
                    )
                })
            })
        }));
    }

    #[test]
    fn unsupported_scalar_list_reverse_cannot_reenter_legacy_with_auto_prelude() {
        let source = include_str!("../../tests/fixtures/mir_native_list_reverse_rejected.mimi");
        let tokens = mimi::lexer::Lexer::new(source).tokenize().expect("lex");
        let mut file = mimi::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        mimi::loader::merge_prelude_into(&mut file);
        let checked = mimi::core::check_program(&file).expect("check");
        assert!(mimi::core::mir::has_unsupported_list_reverse_candidate(
            &checked
        ));
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("unsupported List.reverse must fail closed instead of using legacy");
        };
        assert!(
            reason.contains("S11 scalar collection candidate"),
            "{reason}"
        );
        assert!(reason.contains("did not materialize"), "{reason}");
    }

    #[test]
    fn unsupported_scalar_list_concat_cannot_reenter_legacy_with_auto_prelude() {
        let source =
            include_str!("../../tests/fixtures/mir_native_list_concat_method_rejected.mimi");
        let tokens = mimi::lexer::Lexer::new(source).tokenize().expect("lex");
        let mut file = mimi::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        mimi::loader::merge_prelude_into(&mut file);
        let checked = mimi::core::check_program(&file).expect("check");
        assert!(mimi::core::mir::has_unsupported_list_concat_candidate(
            &checked
        ));
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("unsupported List.concat must fail closed instead of using legacy");
        };
        assert!(
            reason.contains("S11 scalar collection candidate"),
            "{reason}"
        );
        assert!(reason.contains("List.concat"), "{reason}");
    }

    #[test]
    fn uncalled_generic_set_template_stays_outside_collection_route() {
        let source = r#"
            func passthrough<T>(value: Set<T>) -> Set<T> { value }

            func main() -> i32 { 42 }
        "#;
        let (checked, file) = checked(source);
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::OutsideProfile
        );
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Legacy(LegacyRouteReason::OutsideMigratedProfile)
        ));
    }

    #[test]
    fn set_function_form_contains_with_bool_output_enters_canonical_route() {
        let source = r#"
            func main() -> i32 {
                let values = {4, 1, 1}
                println(contains(values, 1))
                0
            }
        "#;
        let (checked, file) = checked(source);
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
        );
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Canonical(_)
        ));
    }

    #[test]
    fn standalone_bool_println_enters_canonical_route() {
        let source = r#"
            func main() -> i32 {
                println(1 == 1)
                println(1 == 2)
                0
            }
        "#;
        let (checked, file) = checked(source);
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
        );
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Canonical(_)
        ));
    }

    #[test]
    fn standalone_integer_println_enters_canonical_route() {
        let source = r#"
            func main() -> i32 {
                println(-7)
                let wide = 9223372036854775806 as i64
                println(wide)
                0
            }
        "#;
        let (checked, file) = checked(source);
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
        );
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Canonical(_)
        ));
    }

    #[test]
    fn standalone_unsupported_println_stays_outside_migrated_route() {
        let source = r#"
            func main() -> i32 {
                println("legacy")
                0
            }
        "#;
        let (checked, file) = checked(source);
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::OutsideProfile
        );
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Legacy(LegacyRouteReason::OutsideMigratedProfile)
        ));
    }

    #[test]
    fn scalar_collection_with_unsupported_println_stays_on_explicit_compatibility_route() {
        let source = r#"
            func main() -> i32 {
                let values = {4, 1, 1}
                println(contains(values, 1))
                println("legacy")
                0
            }
        "#;
        let (checked, file) = checked(source);
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::MixedCoverage
        );
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Legacy(LegacyRouteReason::MixedCoverageWithoutMaterializedCandidate)
        ));
    }

    #[test]
    fn unsupported_dynamic_list_len_stays_on_legacy_compatibility_route() {
        let source = r#"
            func main() -> i32 {
                let values = [i for i in range(0, 3)]
                len(values)
            }
        "#;
        let (checked, file) = checked(source);
        let route = select_default_route(&checked, &file);
        assert!(matches!(
            route,
            DefaultMirRoute::Legacy(LegacyRouteReason::MixedCoverageWithoutMaterializedCandidate)
        ));
    }

    #[test]
    fn legacy_route_reason_has_stable_receipt_names() {
        assert_eq!(
            LegacyRouteReason::OutsideMigratedProfile.as_str(),
            "outside-migrated-profile"
        );
        assert_eq!(
            LegacyRouteReason::MixedCoverageWithoutMaterializedCandidate.as_str(),
            "mixed-coverage-without-materialized-candidate"
        );
    }

    #[test]
    fn unsupported_set_facade_candidate_cannot_reenter_legacy_route() {
        let source = r#"
            func bad<T>(value: Set<T>) -> Set<T> { value }

            func main() -> i32 {
                let values: Set<i32> = {1, 2}
                let result = bad(values)
                drop(result)
                0
            }
        "#;
        let (checked, file) = checked(source);
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("an admitted but unsupported Set facade must fail closed");
        };
        assert!(
            reason.contains("S11 scalar collection candidate"),
            "{reason}"
        );
        assert!(
            reason.contains("canonical MIR construction failed"),
            "{reason}"
        );
    }

    #[test]
    fn option_string_variant_switches_to_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_option_string_switch_move.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_option_string_variant_admission(&checked),
            mimi::core::mir::OptionStringVariantAdmission::CompleteCoverage
        );
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Canonical(_)
        ));
    }

    #[test]
    fn option_string_unwrap_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_option_string_unwrap.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_option_string_variant_admission(&checked),
            mimi::core::mir::OptionStringVariantAdmission::CompleteCoverage
        );
        let route = select_default_route(&checked, &file);
        let DefaultMirRoute::Canonical(program) = route else {
            panic!("Option<string>.unwrap route mismatch: {route:?}");
        };
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        mimi::core::mir::MirInstructionKind::VariantProjectMove { .. }
                    )
                })
            })
        }));
    }

    #[test]
    fn option_i32_unwrap_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_option_i32_unwrap.mimi"
        ));
        let route = select_default_route(&checked, &file);
        assert!(matches!(route, DefaultMirRoute::Canonical(_)));
    }

    #[test]
    fn option_bool_unwrap_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_option_bool_unwrap.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_copy_option_variant_admission(
                &checked,
                mimi::core::PrimitiveType::Bool,
            ),
            mimi::core::mir::CopyOptionI32VariantAdmission::CompleteCoverage
        );
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Canonical(_)
        ));
    }

    #[test]
    fn option_i64_unwrap_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_option_i64_unwrap.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_copy_option_variant_admission(
                &checked,
                mimi::core::PrimitiveType::I64,
            ),
            mimi::core::mir::CopyOptionI32VariantAdmission::CompleteCoverage
        );
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Canonical(_)
        ));
    }

    #[test]
    fn option_f64_unwrap_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_option_f64_unwrap.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_copy_option_variant_admission(
                &checked,
                mimi::core::PrimitiveType::F64,
            ),
            mimi::core::mir::CopyOptionI32VariantAdmission::CompleteCoverage
        );
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Canonical(_)
        ));
    }

    #[test]
    fn mixed_copy_option_projection_is_rejected_without_legacy_fallback() {
        let (checked, file) = checked(
            "func i32_unwrap() -> i32 { let value: Option<i32> = Some(41); value.unwrap() } func i64_unwrap() -> i64 { let value: Option<i64> = Some(7); value.unwrap() } func main() -> i32 { i32_unwrap() }",
        );
        assert_eq!(
            mimi::core::mir::classify_copy_option_i32_variant_admission(&checked),
            mimi::core::mir::CopyOptionI32VariantAdmission::MixedCoverage
        );
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("mixed Copy Option projection must fail closed");
        };
        assert!(reason.contains("Copy Option<i32>"), "{reason}");
    }

    #[test]
    fn unsupported_copy_option_payload_stays_outside_known_variant_islands() {
        let (checked, file) = checked(
            "func main() -> i32 { let value: Option<(string, i32)> = Some((\"owned\", 41)); drop(value); 42 }",
        );
        assert_eq!(
            mimi::core::mir::classify_copy_option_i32_variant_admission(&checked),
            mimi::core::mir::CopyOptionI32VariantAdmission::OutsideProfile
        );
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Legacy(LegacyRouteReason::OutsideMigratedProfile)
        ));
    }

    #[test]
    fn mixed_copy_option_bool_projection_is_rejected_without_legacy_fallback() {
        let (checked, file) = checked(
            "func bool_unwrap() -> bool { let value: Option<bool> = Some(true); value.unwrap() } func i64_unwrap() -> i64 { let value: Option<i64> = Some(7); value.unwrap() } func main() -> i32 { 42 }",
        );
        assert_eq!(
            mimi::core::mir::classify_copy_option_variant_admission(
                &checked,
                mimi::core::PrimitiveType::Bool,
            ),
            mimi::core::mir::CopyOptionI32VariantAdmission::MixedCoverage
        );
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("mixed Copy Option<bool> projection must fail closed");
        };
        assert!(reason.contains("Copy Option<bool>"), "{reason}");
    }

    #[test]
    fn mixed_copy_option_i64_projection_is_rejected_without_legacy_fallback() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_option_i64_mixed_rejected.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_copy_option_variant_admission(
                &checked,
                mimi::core::PrimitiveType::I64,
            ),
            mimi::core::mir::CopyOptionI32VariantAdmission::MixedCoverage
        );
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("mixed Copy Option<i64> projection must fail closed");
        };
        assert!(reason.contains("S116 Copy Option<i64>"), "{reason}");
    }

    #[test]
    fn mixed_copy_option_f64_projection_is_rejected_without_legacy_fallback() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_option_f64_mixed_rejected.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_copy_option_variant_admission(
                &checked,
                mimi::core::PrimitiveType::F64,
            ),
            mimi::core::mir::CopyOptionI32VariantAdmission::MixedCoverage
        );
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("mixed Copy Option<f64> projection must fail closed");
        };
        assert!(reason.contains("S117 Copy Option<f64>"), "{reason}");
    }

    #[test]
    fn mixed_copy_and_owned_option_projection_is_rejected() {
        let (checked, file) = checked(
            "func copy_unwrap() -> i32 { let value: Option<i32> = Some(41); value.unwrap() } func owned_unwrap() -> string { let value: Option<string> = Some(\"owned\"); value.unwrap() } func main() -> i32 { copy_unwrap() }",
        );
        assert_eq!(
            mimi::core::mir::classify_copy_option_i32_variant_admission(&checked),
            mimi::core::mir::CopyOptionI32VariantAdmission::MixedCoverage
        );
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Rejected(_)
        ));
    }

    #[test]
    fn option_string_variant_default_arm_is_rejected_without_legacy_fallback() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_option_string_default_rejected.mimi"
        ));
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("an uncovered Option<string> SwitchMove shape must fail closed");
        };
        assert!(
            reason.contains("S30 non-Copy Option<string> variant candidate"),
            "{reason}"
        );
        assert!(reason.contains("None and Some"), "{reason}");
    }
}
