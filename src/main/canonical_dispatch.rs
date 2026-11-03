//! Program-level Canonical MIR route selection.
//!
//! The default entry points must make one capability decision for the whole
//! checked program.  A canonical backend is never attempted and then replaced
//! by legacy code on failure: programs either pass this preflight and use the
//! canonical route, or remain on the legacy route because their capability
//! set is not yet migrated. Once the bounded collection or exact non-Copy
//! `Option<string>`, a narrow ordinary Map/Any owner-v2 or Map lifecycle
//! observer profile, or managed direct-Result-call island is recognized, its
//! failure is an explicit rejection rather than compatibility fallback.

use std::collections::HashSet;

pub(crate) const NATIVE_DATA_EXPORT_ROUTE_PROFILE: &str = "native-data-export-v1";

use mimi::ast::File;
use mimi::core::mir::reference::{MirProgram, MirProgramBuildError};
use mimi::core::CheckedProgram;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegacyRouteReason {
    /// No checker-owned migration profile was recognized.
    OutsideMigratedProfile,
    /// A compatibility-shaped program did not materialize a migrated MIR
    /// operation, so it remains outside the current production island.
    MixedCoverageWithoutMaterializedCandidate,
    /// R6-1052: a plain-scalar stdout candidate failed the island preflight
    /// (the graph carries an operation outside the island envelope), so the
    /// program keeps its explicit compatibility route.
    PlainScalarIslandPreflightCompatibility,
    /// A defined C-export body has no canonical export-wrapper receipt yet.
    /// This is an early, explicit default compatibility disposition; explicit
    /// MIR requests still fail during construction.
    ExportedAbiBodyCompatibility,
    /// Actor execution trims dormant bodies; user proof roots require the
    /// complete checked-program verifier rather than the NoContracts island.
    ActorProofObligationsCompatibility,
}

impl LegacyRouteReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::OutsideMigratedProfile => "outside-migrated-profile",
            Self::MixedCoverageWithoutMaterializedCandidate => {
                "mixed-coverage-without-materialized-candidate"
            }
            Self::PlainScalarIslandPreflightCompatibility => {
                "plain-scalar-island-preflight-compatibility"
            }
            Self::ExportedAbiBodyCompatibility => "exported-abi-body-compatibility",
            Self::ActorProofObligationsCompatibility => "actor-proof-obligations-compatibility",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalarCollectionGateFailureDisposition {
    PlainScalarStdoutCompatibility,
    RejectMigratedCandidate,
}

/// Decide disposition after the scalar collection island gate has failed.
/// Checker-issued ordinary scalar contracts are migrated candidates even
/// though they do not materialize a List/Set operation; only the plain stdout
/// face retains its explicit compatibility fallback.
fn scalar_collection_gate_failure_disposition(
    ordinary_scalar_contract_candidate: bool,
    materialized_collection_operation_candidate: bool,
    flow_route_candidate: bool,
) -> ScalarCollectionGateFailureDisposition {
    if ordinary_scalar_contract_candidate
        || materialized_collection_operation_candidate
        || flow_route_candidate
    {
        ScalarCollectionGateFailureDisposition::RejectMigratedCandidate
    } else {
        ScalarCollectionGateFailureDisposition::PlainScalarStdoutCompatibility
    }
}

pub(crate) fn report_legacy_route(reason: LegacyRouteReason) {
    if std::env::var_os("MIMI_VERBOSE").is_some() || std::env::var_os("MIMI_ROUTE_CENSUS").is_some()
    {
        eprintln!("canonical route disposition: legacy ({})", reason.as_str());
    }
}

/// Emit the global owner-v2 readiness bit alongside Observer V1 route census
/// evidence. This profile is admitted independently while the broad owner-v2
/// production gate remains closed.
fn observer_v1_readiness_census_line() -> String {
    format!(
        "MIMI_ROUTE_CENSUS observer-v1-readiness global_map_any_v2_production_ready={}",
        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY
    )
}

fn report_observer_v1_readiness_census() {
    eprintln!("{}", observer_v1_readiness_census_line());
}

#[derive(Debug)]
pub(crate) enum DefaultMirRoute {
    /// Explicit compatibility route. The reason is part of the route
    /// disposition so callers cannot mistake an unrecognized program for a
    /// canonical preflight failure that was silently downgraded.
    Legacy(LegacyRouteReason),
    Canonical(Box<MirProgram>),
    /// A recognized migrated-island candidate failed canonical preflight.
    /// Once an island is recognized, its old production path is deleted and
    /// the CLI must fail closed instead of silently compiling it with legacy.
    Rejected(String),
}

const MAP_NEW_RESULT_SOURCE_PENDING_REASON: &str = "MIR-COVERAGE-001: Checker-recognized Map NewResult source receipt has no canonical native/route receipt yet";

/// A Checker NewResult receipt recognizes the exact source-level recoverable
/// `map_new()` shape, but it is not yet an admission for production MIR.
/// Keep it visible to both default selection and explicit canonical builds so
/// it cannot disappear into a legacy fallback.
fn map_new_result_source_pending_reason(checked: &CheckedProgram) -> Option<&'static str> {
    checked
        .resource_analyses()
        .values()
        .any(|analysis| analysis.has_pending_ordinary_map_owned_any_v2_new_result_source_receipt())
        .then_some(MAP_NEW_RESULT_SOURCE_PENDING_REASON)
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
    if map_new_result_source_pending_reason(checked).is_some() {
        return match mimi::core::mir::materialize_ordinary_map_new_result_route(checked) {
            Ok(program) => Ok(program),
            Err(reason) => Err(reason),
        };
    }
    let admission = mimi::core::mir::classify_canonical_mir_route_admission(checked);
    if let Some(reason) = scalar_ffi_map_owner_composition_failure(admission) {
        return Err(reason);
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage
    {
        return Err(format!(
            "{}: explicit --mir refuses an incomplete ordinary Map/Any owner-v2 attempt, including unreceipted Values producer/caller or cross-call String caller intent, before generic MIR construction",
            mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if admission.ordinary_map_owned_any_v2_persistent_root_alias_replacement() {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        if !ordinary_map_owned_any_v2_persistent_root_alias_replacement_materialization_is_exact(
            &route,
        ) {
            return Err(format!(
                "{}: Checker PersistentRootAliasReplacement admission did not materialize its exact independent route receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        preflight_ordinary_map_owned_any_v2_persistent_root_alias_replacement(
            checked,
            &route.program,
        )
        .map_err(|reason| {
            format!("canonical PersistentRootAliasReplacement route rejected: {reason}")
        })?;
        return Ok(route.program);
    }
    if admission.ordinary_map_owned_any_v2_get_or_default_string() {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        preflight_ordinary_map_owned_any_v2_get_or_default_string(checked, &route.program)
            .map_err(|reason| format!("canonical GetOrDefault<String> route rejected: {reason}"))?;
        return Ok(route.program);
    }
    if admission.ordinary_map_owned_any_v2_get_or_default_i32() {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        preflight_ordinary_map_owned_any_v2_get_or_default_i32(checked, &route.program)
            .map_err(|reason| format!("canonical GetOrDefault<i32> route rejected: {reason}"))?;
        return Ok(route.program);
    }
    if admission.ordinary_map_owned_any_v2_computed_string_key_owner_chain() {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        preflight_ordinary_map_owned_any_v2_computed_string_key_owner_chain(
            checked,
            &route.program,
        )
        .map_err(|reason| {
            format!("canonical ComputedStringKeyOwnerChain route rejected: {reason}")
        })?;
        return Ok(route.program);
    }
    if admission
        .ordinary_map_owned_any_v2
        .ordinary_map_owned_any_v2_dynamic_keys_get()
    {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        if !ordinary_map_owned_any_v2_dynamic_keys_get_materialization_is_exact(&route) {
            return Err(format!(
                "{}: Checker DynamicKeysGet admission did not materialize its exact independent route receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        preflight_ordinary_map_owned_any_v2_dynamic_keys_get(checked, &route.program)
            .map_err(|reason| format!("canonical DynamicKeysGet route rejected: {reason}"))?;
        return Ok(route.program);
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::KeysSnapshot
    {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        if !ordinary_map_owned_any_v2_keys_snapshot_materialization_is_exact(&route) {
            return Err(format!(
                "{}: Checker KeysSnapshot admission did not materialize its exact independent route receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        preflight_ordinary_map_owned_any_v2_keys_snapshot(checked, &route.program)
            .map_err(|reason| format!("canonical KeysSnapshot route rejected: {reason}"))?;
        return Ok(route.program);
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::MapEmptyRootRootQueriesV1
    {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|e| e.to_diagnostic().message)?;
        preflight_map_empty_root_root_queries_v1(checked, &route.program)?;
        return Ok(route.program);
    }
    if admission.ordinary_map_owned_any_v2_zero_miss_i32() {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        if !ordinary_map_owned_any_v2_zero_miss_i32_materialization_is_exact(&route) {
            return Err(format!(
                "{}: Checker ZeroMissI32 admission did not materialize its exact independent operation receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        preflight_ordinary_map_owned_any_v2_zero_miss_i32(checked, &route.program)
            .map_err(|reason| format!("canonical ZeroMissI32 route rejected: {reason}"))?;
        return Ok(route.program);
    }
    if admission.ordinary_map_owned_any_v2_removed_key_checked_miss() {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        if !ordinary_map_owned_any_v2_removed_key_checked_miss_materialization_is_exact(&route) {
            return Err(format!(
                "{}: Checker RemovedKeyCheckedMiss admission did not materialize its exact independent operation receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        preflight_ordinary_map_owned_any_v2_removed_key_checked_miss(checked, &route.program)
            .map_err(|reason| {
                format!("canonical RemovedKeyCheckedMiss route rejected: {reason}")
            })?;
        return Ok(route.program);
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::CloneIsolation
    {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        if !ordinary_map_owned_any_v2_clone_isolation_materialization_is_exact(&route) {
            return Err(format!(
                "{}: Checker CloneIsolation admission did not materialize its exact independent operation receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        preflight_ordinary_map_owned_any_v2_clone_isolation(checked, &route.program)
            .map_err(|reason| format!("canonical CloneIsolation route rejected: {reason}"))?;
        return Ok(route.program);
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::RecordUnit
    {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        if !ordinary_map_owned_any_v2_record_unit_materialization_is_exact(&route) {
            return Err(format!(
                "{}: Checker RecordUnit admission did not materialize its exact independent operation receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        preflight_ordinary_map_owned_any_v2_record_unit(checked, &route.program)
            .map_err(|reason| format!("canonical RecordUnit route rejected: {reason}"))?;
        return Ok(route.program);
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::IsEmpty
    {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        if !ordinary_map_owned_any_v2_is_empty_materialization_is_exact(&route) {
            return Err(format!(
                "{}: Checker IsEmpty admission did not materialize its exact independent operation receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        preflight_ordinary_map_owned_any_v2_is_empty(checked, &route.program)
            .map_err(|reason| format!("canonical IsEmpty route rejected: {reason}"))?;
        return Ok(route.program);
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControl
    {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        if !ordinary_map_owned_any_v2_map_set_drop_control_materialization_is_exact(&route) {
            return Err(format!(
                "{}: Checker MapSetDropControl admission did not materialize its exact independent operation receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        preflight_ordinary_map_owned_any_v2_map_set_drop_control(checked, &route.program)
            .map_err(|reason| format!("canonical MapSetDropControl route rejected: {reason}"))?;
        return Ok(route.program);
    }
    if admission.ordinary_map_owned_any_v2_branch_local_set_diamond() {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        if !ordinary_map_owned_any_v2_branch_local_set_diamond_materialization_is_exact(&route) {
            return Err(format!(
                "{}: Checker BranchLocalSetDiamond admission did not materialize its exact independent operation receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        preflight_ordinary_map_owned_any_v2_branch_local_set_diamond(checked, &route.program)
            .map_err(|reason| {
                format!("canonical BranchLocalSetDiamond route rejected: {reason}")
            })?;
        return Ok(route.program);
    }
    if admission.ordinary_map_owned_any_v2_values_crosscall_string_caller_index1() {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        let profile = mimi::core::mir::CanonicalMirRouteProfile::
            OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1
            .as_str();
        if !route
            .ordinary_map_owned_any_v2_values_crosscall_string_caller_route_receipt
            .as_ref()
            .is_some_and(|receipt| receipt == &route.program.route_receipt(profile))
        {
            return Err(format!(
                "{}: Checker ValuesCrosscallStringCallerIndex1 admission did not materialize its exact independent route receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        preflight_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1(
            checked,
            &route.program,
        )
        .map_err(|reason| {
            format!("canonical ValuesCrosscallStringCallerIndex1 route rejected: {reason}")
        })?;
        return Ok(route.program);
    }
    if admission.ordinary_map_owned_any_v2_values_crosscall_string_caller_v1() {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        let profile =
            mimi::core::mir::CanonicalMirRouteProfile::
            OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller
            .as_str();
        if !route
            .ordinary_map_owned_any_v2_values_crosscall_string_caller_route_receipt
            .as_ref()
            .is_some_and(|receipt| receipt == &route.program.route_receipt(profile))
        {
            return Err(format!(
                "{}: Checker ValuesCrosscallStringCaller admission did not materialize its exact independent route receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        preflight_ordinary_map_owned_any_v2_values_crosscall_string_caller(checked, &route.program)
            .map_err(|reason| {
                format!("canonical ValuesCrosscallStringCaller route rejected: {reason}")
            })?;
        return Ok(route.program);
    }
    if admission.ordinary_map_owned_any_v2_values_producer_caller() {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        let profile =
            mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesProducerCaller
                .as_str();
        if !route
            .ordinary_map_owned_any_v2_values_producer_caller_route_receipt
            .as_ref()
            .is_some_and(|receipt| receipt == &route.program.route_receipt(profile))
        {
            return Err(format!(
                "{}: Checker ValuesProducerCaller admission did not materialize its exact independent route receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        preflight_ordinary_map_owned_any_v2_values_producer_caller(checked, &route.program)
            .map_err(|reason| format!("canonical ValuesProducerCaller route rejected: {reason}"))?;
        return Ok(route.program);
    }
    if admission.ordinary_map_owned_any_v2_map_root_get_any_caller() {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        let profile =
            mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootGetAnyCaller
                .as_str();
        if !route
            .ordinary_map_owned_any_v2_map_root_get_any_caller_route_receipt
            .as_ref()
            .is_some_and(|receipt| receipt == &route.program.route_receipt(profile))
        {
            return Err(format!(
                "{}: Checker MapRootGetAnyCaller admission did not materialize its exact independent route receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        preflight_ordinary_map_owned_any_v2_map_root_get_any_caller(checked, &route.program)
            .map_err(|reason| format!("canonical MapRootGetAnyCaller route rejected: {reason}"))?;
        return Ok(route.program);
    }
    if admission.ordinary_map_owned_any_v2_map_root_producer_caller() {
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        let profile =
            mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootProducerCaller
                .as_str();
        if !route
            .ordinary_map_owned_any_v2_map_root_producer_caller_route_receipt
            .as_ref()
            .is_some_and(|receipt| receipt == &route.program.route_receipt(profile))
        {
            return Err(format!(
                "{}: Checker MapRootProducerCaller admission did not materialize its exact independent route receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        preflight_ordinary_map_owned_any_v2_map_root_producer_caller(checked, &route.program)
            .map_err(|reason| {
                format!("canonical MapRootProducerCaller route rejected: {reason}")
            })?;
        return Ok(route.program);
    }
    let observer_admission = admission.ordinary_map_owner_lifecycle_observer_v1;
    if observer_admission.has_profile_signal() {
        if !observer_admission.is_complete() {
            return Err(format!(
                "{}: Checker recognized an ordinary Map owner lifecycle observer attempt without complete Observer V1 coverage",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        let route = materialize_canonical_route(checked, merged_file)
            .map_err(|error| error.to_diagnostic().message)?;
        if !ordinary_map_owner_lifecycle_observer_v1_materialization_is_exact(&route) {
            return Err(format!(
                "{}: complete ordinary Map owner Observer V1 admission did not materialize its exact route receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        preflight_ordinary_map_owner_lifecycle_observer_v1(checked, &route.program)
            .map_err(|reason| format!("canonical Observer V1 route rejected: {reason}"))?;
        if std::env::var_os("MIMI_ROUTE_CENSUS").is_some() {
            report_observer_v1_readiness_census();
        }
        return Ok(route.program);
    }
    let v2_admission = admission.ordinary_map_owned_any_v2;
    if v2_admission.has_profile_signal()
        && !v2_admission.is_direct_print()
        && !v2_admission.is_checked_get()
        && !v2_admission.is_computed_key_get()
        && !v2_admission.is_computed_string_key_owner_chain()
        && !v2_admission.ordinary_map_owned_any_v2_dynamic_keys_get()
        && !admission.ordinary_map_owned_any_v2_zero_miss_i32()
        && !admission.ordinary_map_owned_any_v2_removed_key_checked_miss()
        && !v2_admission.is_values_field_move()
        && !v2_admission.is_values_local_index_i32()
        && !v2_admission.is_values_heterogeneous_string_index1()
        && v2_admission
            != mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesMixedSnapshotRemove
        && !v2_admission.is_values_mixed_snapshot()
        && v2_admission
            != mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesSnapshotReplacement
        && !v2_admission.is_ordinary_map_owned_any_v2_persistent_root_alias_replacement()
        && (!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY
            || !v2_admission.is_complete())
    {
        return Err(format!(
            "{}: ordinary Map/Any owner-v2 route is not production-ready for explicit canonical execution or verification ({v2_admission:?}); use `mimi mir --receipt` for read-only inspection",
            mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    // Explicit `--mir` is a construction/inspection request rather than a
    // default-route admission. Preserve the canonical builder's detailed
    // lowering/validation diagnostics here; the default selector uses the
    // shared route envelope above for Complete-vs-Compatibility disposition.
    // The V2 production-readiness boundary above is shared by explicit run,
    // build, and verify requests; `mimi mir` uses the diagnostics builder
    // directly and remains available for read-only receipts.
    let program = build_canonical_program_for_sources(checked, merged_file, None)?;
    if v2_admission.is_ordinary_map_owned_any_v2_persistent_root_alias_replacement() {
        preflight_ordinary_map_owned_any_v2_persistent_root_alias_replacement(checked, &program)
            .map_err(|reason| {
                format!("canonical PersistentRootAliasReplacement route rejected: {reason}")
            })?;
    } else if v2_admission.is_direct_print() {
        preflight_ordinary_map_owned_any_v2_direct_print(&program)
            .map_err(|reason| format!("canonical DirectPrint route rejected: {reason}"))?;
    } else if v2_admission.is_checked_get() {
        preflight_ordinary_map_owned_any_v2_checked_get(checked, &program)
            .map_err(|reason| format!("canonical CheckedGet route rejected: {reason}"))?;
    } else if v2_admission.is_computed_key_get() {
        preflight_ordinary_map_owned_any_v2_computed_key_get(checked, &program)
            .map_err(|reason| format!("canonical ComputedKeyGet route rejected: {reason}"))?;
    } else if v2_admission.is_computed_string_key_owner_chain() {
        preflight_ordinary_map_owned_any_v2_computed_string_key_owner_chain(checked, &program)
            .map_err(|reason| {
                format!("canonical ComputedStringKeyOwnerChain route rejected: {reason}")
            })?;
    } else if v2_admission.ordinary_map_owned_any_v2_dynamic_keys_get() {
        preflight_ordinary_map_owned_any_v2_dynamic_keys_get(checked, &program)
            .map_err(|reason| format!("canonical DynamicKeysGet route rejected: {reason}"))?;
    } else if v2_admission.is_values_field_move() {
        preflight_ordinary_map_owned_any_v2_values_field_move(checked, &program)
            .map_err(|reason| format!("canonical ValuesFieldMove route rejected: {reason}"))?;
    } else if v2_admission.is_values_local_index_i32() {
        preflight_ordinary_map_owned_any_v2_values_local_index_i32(checked, &program)
            .map_err(|reason| format!("canonical ValuesLocalIndexI32 route rejected: {reason}"))?;
    } else if v2_admission.is_values_heterogeneous_string_index1() {
        preflight_ordinary_map_owned_any_v2_values_heterogeneous_string_index1(checked, &program)
            .map_err(|reason| {
            format!("canonical ValuesHeterogeneousStringIndex1 route rejected: {reason}")
        })?;
    } else if v2_admission
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesMixedSnapshotRemove
    {
        preflight_ordinary_map_owned_any_v2_values_mixed_snapshot_remove(checked, &program)
            .map_err(|reason| {
                format!("canonical ValuesMixedSnapshotRemove route rejected: {reason}")
            })?;
    } else if v2_admission
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesSnapshotReplacement
    {
        preflight_ordinary_map_owned_any_v2_values_snapshot_replacement(checked, &program)
            .map_err(|reason| {
                format!("canonical ValuesSnapshotReplacement route rejected: {reason}")
            })?;
    } else if v2_admission.is_values_mixed_snapshot() {
        preflight_ordinary_map_owned_any_v2_values_mixed_snapshot(checked, &program)
            .map_err(|reason| format!("canonical ValuesMixedSnapshot route rejected: {reason}"))?;
    } else if v2_admission.is_removed_key_checked_miss() {
        preflight_ordinary_map_owned_any_v2_removed_key_checked_miss(checked, &program).map_err(
            |reason| format!("canonical RemovedKeyCheckedMiss route rejected: {reason}"),
        )?;
    }
    Ok(program)
}

/// Return the shared no-composition boundary before either default dispatch
/// or an explicit MIR builder can choose a Map-owner profile.  The core
/// helper covers the established MapRoot/V1 profiles; the second branch
/// extends the same contract to the narrow ordinary Map/Any owner-v2
/// profiles without opening the global V2 readiness gate.
fn scalar_ffi_map_owner_composition_failure(
    admission: mimi::core::mir::CanonicalMirRouteAdmission,
) -> Option<String> {
    if !admission.scalar_ffi {
        return None;
    }

    // `is_candidate` is the semantic discriminator for every owner-v2 signal,
    // including narrow CheckedGet, ComputedKeyGet, and IsEmpty profiles. Keep
    // this composition gate independent of the raw receipt-profile string table.
    let has_map_owner_candidate = admission.map_root.is_candidate()
        || admission.map_any_owner.is_candidate()
        || admission.ordinary_map_owned_any_v2.is_candidate()
        || admission
            .ordinary_map_owner_lifecycle_observer_v1
            .has_profile_signal()
        || admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .has_profile_signal()
        || admission
            .ordinary_map_owned_get_i32_string_v1
            .has_profile_signal();
    if !has_map_owner_candidate {
        return None;
    }

    if let Some(error) = mimi::core::mir::scalar_ffi_map_owner_composition_failure(admission) {
        return Some(error.to_diagnostic().message);
    }

    Some(format!(
        "{}: Map/Any owner profile cannot be combined with scalar FFI; no combined canonical route exists",
        mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
    ))
}

/// Select the route identity used by CLI consumers for a checker-owned graph.
/// The recoverable ordinary Map NewResult receipt leads the chain: it is
/// shape-disjoint from every admission-based profile below (its exact source
/// shape admits no other ordinary Map operation), so the early return cannot
/// shadow them and the pending-source dispatch cannot be shadowed either.
/// Observer V1, DirectPrint, CheckedGet, ComputedKeyGet,
/// ComputedStringKeyOwnerChain, IsEmpty,
/// MapSetDropControl, BranchLocalSetDiamond, CloneIsolation, ValuesFieldMove,
/// ValuesLocalIndexI32, KeysSnapshot,
/// ValuesSnapshotReplacement, ValuesMixedSnapshot, ValuesMixedSnapshotRemove,
/// RemovedKeyCheckedMiss, ValuesProducerCaller, ValuesCrosscallStringCaller,
/// ValuesCrosscallStringCallerIndex1, MapRootProducerCaller, and
/// MapRootGetAnyCaller each have a separate narrow receipt; all unrelated
/// routes keep the profile chosen by their existing CLI entry point.
pub(crate) fn route_receipt_profile_for_checked(
    checked: &CheckedProgram,
    default_profile: &'static str,
) -> &'static str {
    let admission = mimi::core::mir::classify_canonical_mir_route_admission(checked);
    if mimi::core::mir::has_ordinary_map_new_result_route_candidate(checked) {
        return mimi::core::mir::ORDINARY_MAP_NEW_RESULT_EXECUTION_PROFILE;
    }
    if admission.ordinary_map_owned_any_v2_persistent_root_alias_replacement() {
        return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE;
    }
    if admission.ordinary_map_owned_any_v2_get_or_default_string() {
        return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_PROFILE;
    }
    if admission.ordinary_map_owned_any_v2_get_or_default_i32() {
        return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_I32_PROFILE;
    }
    if admission.ordinary_map_owned_any_v2_removed_key_checked_miss() {
        return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE;
    }
    if admission.ordinary_map_owned_any_v2_zero_miss_i32() {
        return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE;
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::KeysSnapshot
    {
        return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE;
    }
    if admission.ordinary_map_owned_any_v2_dynamic_keys_get() {
        return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE;
    }
    if admission.ordinary_map_owned_any_v2_values_crosscall_string_caller_index1() {
        return mimi::core::mir::CanonicalMirRouteProfile::
            OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1
            .as_str();
    }
    if admission.ordinary_map_owned_any_v2_values_crosscall_string_caller_v1() {
        return mimi::core::mir::CanonicalMirRouteProfile::
            OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller
            .as_str();
    }
    if admission.ordinary_map_owned_any_v2_values_producer_caller() {
        return mimi::core::mir::CanonicalMirRouteProfile::
            OrdinaryMapOwnedAnyV2ValuesProducerCaller
            .as_str();
    }
    if admission.ordinary_map_owned_any_v2_map_root_get_any_caller() {
        return mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootGetAnyCaller
            .as_str();
    }
    if admission.ordinary_map_owned_any_v2_map_root_producer_caller() {
        return mimi::core::mir::CanonicalMirRouteProfile::
            OrdinaryMapOwnedAnyV2MapRootProducerCaller
            .as_str();
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::MapEmptyRootRootQueriesV1
    {
        return mimi::core::mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE;
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::RecordUnit
    {
        return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE;
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::IsEmpty
    {
        return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE;
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControl
    {
        return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE;
    }
    if admission.ordinary_map_owned_any_v2_branch_local_set_diamond() {
        return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE;
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::CloneIsolation
    {
        return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE;
    }
    if admission
        .ordinary_map_owner_lifecycle_observer_v1
        .is_candidate()
    {
        mimi::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE
    } else if admission.ordinary_map_owned_any_v2.is_direct_print() {
        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE
    } else if admission.ordinary_map_owned_any_v2.is_checked_get() {
        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE
    } else if admission.ordinary_map_owned_any_v2.is_computed_key_get() {
        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE
    } else if admission
        .ordinary_map_owned_any_v2
        .is_computed_string_key_owner_chain()
    {
        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE
    } else if admission.ordinary_map_owned_any_v2.is_values_field_move() {
        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE
    } else if admission
        .ordinary_map_owned_any_v2
        .is_values_heterogeneous_string_index1()
    {
        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_HETEROGENEOUS_STRING_INDEX1_PROFILE
    } else if admission
        .ordinary_map_owned_any_v2
        .is_values_local_index_i32()
    {
        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_PROFILE
    } else if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesMixedSnapshotRemove
    {
        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE
    } else if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesSnapshotReplacement
    {
        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE
    } else if admission
        .ordinary_map_owned_any_v2
        .is_values_mixed_snapshot()
    {
        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE
    } else if admission.map_any_owner_complete() {
        mimi::core::mir::MAP_ANY_OWNER_V1_ISLAND
    } else if admission.ordinary_map_owned_any_v2_complete() {
        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND
    } else if admission.ordinary_map_owned_get_i32_string_v1_complete() {
        mimi::core::mir::ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND
    } else if admission.ordinary_map_owner_lifecycle_i32_string_v1_complete() {
        mimi::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_I32_STRING_V1_ISLAND
    } else if admission.map_root_complete() {
        mimi::core::mir::MAP_ROOT_ISLAND
    } else if admission.scalar_export {
        mimi::core::mir::SCALAR_EXPORT_ISLAND
    } else if admission.scalar_actor {
        mimi::core::mir::SCALAR_ACTOR_ISLAND
    } else if admission.scalar_ffi {
        "scalar-ffi-v1"
    } else if admission.scalar_nested {
        mimi::core::mir::SCALAR_NESTED_ISLAND
    } else if admission.scalar_generic_identity_i32_complete() {
        mimi::core::mir::SCALAR_GENERIC_IDENTITY_I32_ISLAND
    } else if admission.scalar_generic_identity_i64_complete() {
        mimi::core::mir::SCALAR_GENERIC_IDENTITY_I64_ISLAND
    } else if has_native_data_export_declaration(checked) && !admission.has_candidate() {
        NATIVE_DATA_EXPORT_ROUTE_PROFILE
    } else if mimi::codegen::has_static_generic_trait_specialization_candidate(checked) {
        mimi::codegen::STATIC_GENERIC_TRAIT_SPECIALIZATION_ROUTE_PROFILE
    } else {
        default_profile
    }
}

fn has_native_data_export_declaration(checked: &CheckedProgram) -> bool {
    checked
        .constants()
        .values()
        .any(|constant| constant.extern_abi.is_some() || constant.native_data_export.is_some())
}

/// Return the Checker-selected specialized receipt schema for the two
/// cross-call Values/String caller routes. The generic route manifest has its
/// own stable schema; this separately exposes the narrow route contract that
/// authorized the selected MIR receipt.
pub(crate) fn values_crosscall_string_caller_schema_for_checked(
    checked: &CheckedProgram,
) -> Option<&'static str> {
    let admission = mimi::core::mir::classify_canonical_mir_route_admission(checked);
    if admission.ordinary_map_owned_any_v2_values_crosscall_string_caller_index1() {
        Some(mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_V2_SCHEMA)
    } else if admission.ordinary_map_owned_any_v2_values_crosscall_string_caller_v1() {
        Some(mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_SCHEMA)
    } else {
        None
    }
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
    build_canonical_program_for_sources_with_diagnostics(checked, merged_file, included_sources)
        // Use the stable error contract rather than Rust's debug shape.  The
        // latter exposed enum/field syntax (`Validation([...])`) through the
        // CLI and made construction diagnostics depend on internal layout.
        .map_err(|error| format!("canonical MIR build error: {error}"))
}

/// Build a source-scoped canonical graph while retaining the typed aggregate
/// error until the caller chooses a presentation boundary.  The source filter
/// is only graph selection; lowering, generic instance materialization,
/// TypeDesc construction, and validation remain owned by `MirProgram`.
pub(crate) fn build_canonical_program_for_sources_with_diagnostics(
    checked: &CheckedProgram,
    merged_file: &File,
    included_sources: Option<&HashSet<mimi::span::SourceId>>,
) -> Result<MirProgram, MirProgramBuildError> {
    if let Some(included_sources) = included_sources {
        if let Some(error) =
            source_scope_import_boundary_error(checked, merged_file, included_sources)
        {
            return Err(MirProgramBuildError::Validation(vec![error]));
        }
    }
    if let Some(reason) = mimi::core::mir::unknown_effect_boundary_reason(checked) {
        return Err(MirProgramBuildError::Validation(vec![
            mimi::core::mir::MirValidationError {
                subject: "effect boundary".into(),
                message: format!(
                    "{}: {reason}",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            },
        ]));
    }
    let admission = mimi::core::mir::classify_canonical_mir_route_admission(checked);
    if !admission.scalar_export {
        if let Some(reason) = mimi::core::mir::exported_abi_body_boundary_reason(checked) {
            return Err(MirProgramBuildError::Validation(vec![
                mimi::core::mir::MirValidationError {
                    subject: "scalar export boundary".into(),
                    message: format!(
                        "{}: {reason}; aggregate/contract/unsupported export wrappers are outside the trusted canonical scalar subset",
                        mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                    ),
                },
            ]));
        }
    }
    if admission
        .ordinary_map_owned_get_i32_string_v1
        .has_profile_signal()
        && !admission.ordinary_map_owned_get_i32_string_v1_complete()
    {
        return Err(MirProgramBuildError::Validation(vec![
            mimi::core::mir::MirValidationError {
                subject: "route coverage".into(),
                message: format!(
                    "{}: Checker recognized a typed owned-get Map candidate without complete operation-attempt coverage",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            },
        ]));
    }
    let ordinary_map_v1_exception = admission.map_any_owner_complete()
        && mimi::core::mir::ordinary_map_attempts_fully_covered_by_map_any_owner_v1(checked);
    if admission
        .ordinary_map_owner_lifecycle_i32_string_v1
        .has_profile_signal()
        && !admission.ordinary_map_owner_lifecycle_i32_string_v1_complete()
        && !ordinary_map_v1_exception
    {
        return Err(MirProgramBuildError::Validation(vec![
            mimi::core::mir::MirValidationError {
                subject: "route coverage".into(),
                message: format!(
                    "{}: Checker recognized an ordinary Map owner lifecycle attempt without complete operation-attempt coverage",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ),
            },
        ]));
    }
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
    let canonical = MirProgram::from_checked_program_excluding_sources(checked, &excluded_sources)?;
    if admission.ordinary_map_owned_get_i32_string_v1_complete() {
        if let Some(reason) =
            mimi::core::mir::ordinary_map_owned_get_result_settlement_reason(&canonical)
        {
            return Err(MirProgramBuildError::Validation(vec![
                mimi::core::mir::MirValidationError {
                    subject: "route coverage".into(),
                    message: format!(
                        "{}: {reason}",
                        mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                    ),
                },
            ]));
        }
    }
    Ok(canonical)
}

/// A source-scoped `mimi mir` graph cannot claim to inspect a closed call graph
/// when a retained callable directly targets a callable whose source was
/// filtered out. Detect that boundary from typed Checker call identities
/// before MIR lowering so an incomplete graph is reported as coverage, rather
/// than leaking a downstream ManagedAny call-census implementation error.
fn source_scope_import_boundary_error(
    checked: &CheckedProgram,
    merged_file: &File,
    included_sources: &HashSet<mimi::span::SourceId>,
) -> Option<mimi::core::mir::MirValidationError> {
    let mut first_boundary = None;
    for site in checked.call_sites_sorted() {
        let Some(mimi::core::ir::ResolvedCallee::Function(target_owner)) =
            site.resolved_callee.as_ref()
        else {
            continue;
        };
        let caller_owner = mimi::core::NodeId(site.owner.clone());
        let Some(caller) = checked.callable(&caller_owner) else {
            continue;
        };
        let caller_source = caller.body.root.origin.user_span().source_id;
        if !included_sources.contains(&caller_source) {
            continue;
        }
        let Some(target) = checked.callable(target_owner) else {
            continue;
        };
        let target_source = target.body.root.origin.user_span().source_id;
        if included_sources.contains(&target_source)
            || merged_file
                .sources
                .key(target_source)
                .is_some_and(|key| key.as_str() == "stdlib:prelude.mimi")
        {
            continue;
        }

        let error = mimi::core::mir::MirValidationError {
            subject: "source-scope coverage".into(),
            message: format!(
                "{}: source-scoped MIR excludes imported callable '{}' from source {}; included caller '{}' from source {} calls it. Use `mimi mir --all` to inspect the complete user import graph",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE,
                target_owner.0,
                source_scope_source_label(merged_file, target_source),
                site.owner,
                source_scope_source_label(merged_file, caller_source),
            ),
        };

        // Report the entry edge first when present. It is the most direct
        // explanation for an inspection requested on a single source file.
        if site.owner == "function:main" {
            return Some(error);
        }
        if first_boundary.is_none() {
            first_boundary = Some(error);
        }
    }
    first_boundary
}

fn source_scope_source_label(merged_file: &File, source_id: mimi::span::SourceId) -> String {
    let Some(record) = merged_file.sources.record(source_id) else {
        return format!("source-id:{}", source_id.raw());
    };
    let key = record.key.as_str();
    let file_name = record
        .disk_path
        .as_deref()
        .and_then(std::path::Path::file_name)
        .map(|name| name.to_string_lossy());
    match file_name {
        Some(file_name) if !key.ends_with(file_name.as_ref()) => {
            format!("'{}' ({})", key, file_name)
        }
        _ => format!("'{}'", key),
    }
}

/// Return the source-level diagnostic shared by Canonical MIR execution and
/// verifier entry points when a called extern declaration is outside the
/// migrated scalar C ABI. Keep this check ahead of MIR construction so
/// `run`, `build`, and `verify` explain the same source boundary instead of
/// exposing backend-specific layout failures. Read-only `mimi mir` inspection
/// intentionally remains able to represent declarations outside that ABI.
pub(crate) fn scalar_ffi_declaration_boundary_diagnostic(
    checked: &CheckedProgram,
) -> Option<String> {
    mimi::core::mir::scalar_ffi_boundary_reason(checked).map(|reason| {
        format!(
            "{}: canonical scalar FFI declaration boundary: {reason}. Supported declarations use extern \"C\", scalar i32/i64/f32/f64/bool parameters, and a scalar or unit result; string, pointer, callback, aggregate, variadic, parameter-mode, errno, no_panic, and non-C ABI declarations remain unsupported.",
            mimi::core::mir::MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE
        )
    })
}

/// Select a default route for a complete checked program.
///
/// The current default-switch islands are deliberately narrow: a program must
/// contain either a checker-selected scalar C FFI call, a scalar Set facade instance, a flat Copy
/// record value, a one-, two-, three-, four-, five-, six-, or seven-field generic Copy record projection, a concrete bounded List operation (`len`/`reverse`), a direct nested List index projection, an exact S8 Flow
/// transition, the concrete non-Copy `Option<string>`/Copy `Option<i32>`/`Option<bool>`/`Option<i64>`/`Option<f64>`/`Result<i32, i32>` variant islands (including `unwrap_or`), or the
/// generic `Option<T>.is_some`/`is_none` predicate island, the generic
/// `Option<T>.unwrap()` projection island, generic `Option<T>.unwrap_or(T)`
/// fallback projection island, generic `Result<T, T>`/`Result<T, i32>`
/// `unwrap()`, or generic `Result<T, T>.unwrap_or(T)` /
/// `Result<T, i32>.unwrap_or(T)` fallback projection
/// island, or a concrete managed direct `Result` call with the closed
/// String/List payload contract.
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
    select_default_route_for_purpose(checked, merged_file, RoutePurpose::Execution)
}

/// Contract libraries are valid verification inputs without an executable
/// main. Keep admission and graph checks shared with execution; only the
/// entry-dependent consumer preflight differs for such a library.
pub(crate) fn select_default_verification_route(
    checked: &CheckedProgram,
    merged_file: &File,
) -> DefaultMirRoute {
    select_default_route_for_purpose(checked, merged_file, RoutePurpose::Verification)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RoutePurpose {
    Execution,
    Verification,
}

fn select_default_route_for_purpose(
    checked: &CheckedProgram,
    merged_file: &File,
    purpose: RoutePurpose,
) -> DefaultMirRoute {
    let route = select_default_route_inner(checked, merged_file, purpose);
    let verbose = std::env::var_os("MIMI_VERBOSE").is_some();
    let census = std::env::var_os("MIMI_ROUTE_CENSUS").is_some();
    if let DefaultMirRoute::Canonical(program) = &route {
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(checked);
        let is_map_root = admission.map_root.is_candidate();
        let is_map_any_owner = admission.map_any_owner.is_candidate();
        let is_ordinary_map_owned_any_v2 = admission.ordinary_map_owned_any_v2.is_candidate();
        let is_ordinary_map_owner = admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .is_candidate();
        let is_ordinary_map_owner_observer = admission
            .ordinary_map_owner_lifecycle_observer_v1
            .is_candidate();
        let is_ordinary_map_owned_get = admission
            .ordinary_map_owned_get_i32_string_v1
            .is_candidate();
        let is_scalar_export = admission.scalar_export;
        let is_scalar_nested = admission.scalar_nested;
        // `MIMI_VERBOSE` is an established CLI diagnostic surface and must
        // not add stderr to ordinary canonical programs. Keep the
        // MapRoot/MapAnyOwner-specific evidence there; the whole-program
        // route census gets every canonical disposition through its dedicated
        // opt-in.
        if census
            || (verbose
                && (is_map_root
                    || is_map_any_owner
                    || is_ordinary_map_owned_any_v2
                    || is_ordinary_map_owner_observer
                    || is_ordinary_map_owner
                    || is_ordinary_map_owned_get
                    || is_scalar_export
                    || is_scalar_nested))
        {
            if census && is_ordinary_map_owner_observer {
                report_observer_v1_readiness_census();
            }
            let profile = route_receipt_profile_for_checked(checked, "default-route");
            eprintln!(
                "canonical route disposition: canonical ({profile}) mir_digest={}",
                program.canonical_digest()
            );
        }
    }
    route
}

fn select_default_route_inner(
    checked: &CheckedProgram,
    merged_file: &File,
    purpose: RoutePurpose,
) -> DefaultMirRoute {
    if map_new_result_source_pending_reason(checked).is_some() {
        return select_map_new_result_route(checked, purpose);
    }
    // A called extern declaration that is outside the migrated scalar C ABI
    // is still a canonical FFI boundary.  Reject it before considering any
    // unrelated island or the compatibility route; otherwise default `run`,
    // `build`, or `verify` could silently hand the call to the old emitter.
    if let Some(reason) = scalar_ffi_declaration_boundary_diagnostic(checked) {
        return DefaultMirRoute::Rejected(reason);
    }
    // Admission is checker-owned and must happen before any compatibility
    // disposition. Typed owned-get has no legacy bytecode or LLVM builtin,
    // so an unrelated C-export body cannot route a program containing it to
    // the AST-backed emitter.
    let admission = mimi::core::mir::classify_canonical_mir_route_admission(checked);
    if has_native_data_export_declaration(checked) {
        if let Err(errors) =
            mimi::core::native_data_export::validate_checked_native_data_exports(checked)
        {
            return DefaultMirRoute::Rejected(format!(
                "{}: checked native data export validation failed: {errors:?}",
                mimi::core::native_data_export::NATIVE_DATA_EXPORT_ERROR_CODE
            ));
        }
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage
    {
        return DefaultMirRoute::Rejected(format!(
            "{}: Checker observed an incomplete ordinary Map/Any owner-v2 attempt, including unreceipted Values producer/caller or cross-call String caller intent; default dispatch cannot use legacy or a generic collection route",
            mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if admission.ordinary_map_owned_any_v2_persistent_root_alias_replacement() {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route)
                if ordinary_map_owned_any_v2_persistent_root_alias_replacement_materialization_is_exact(
                    &route,
                ) =>
            {
                match preflight_ordinary_map_owned_any_v2_persistent_root_alias_replacement(
                    checked,
                    &route.program,
                ) {
                    Ok(()) => DefaultMirRoute::Canonical(Box::new(route.program)),
                    Err(reason) => DefaultMirRoute::Rejected(format!(
                        "ordinary Map/Any owner-v2 PersistentRootAliasReplacement preflight failed: {reason}"
                    )),
                }
            }
            Ok(_) => DefaultMirRoute::Rejected(format!(
                "{}: Checker PersistentRootAliasReplacement admission did not materialize its exact independent route receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    // A source-qualified module call is itself a production boundary: once
    // the checker resolved `alias::function` to a concrete exported owner,
    // it must use that owner in the default route. Leaving this shape
    // outside every migration island would send the caller to bytecode even
    // though the same checked graph can be materialized as Canonical MIR.
    let module_alias_call_hint = checked.call_sites().values().any(|site| {
        matches!(site.kind, mimi::core::ResolvedCallKind::Function)
            && site.target_function.is_some()
            && site.callee.contains("::")
    });
    // Map owner receipts and scalar FFI have no combined route.  Apply the
    // boundary before any default consumer can choose a narrow Map profile
    // or fall back to the compatibility verifier.
    if let Some(reason) = scalar_ffi_map_owner_composition_failure(admission) {
        return DefaultMirRoute::Rejected(reason);
    }
    if admission.ordinary_map_owned_any_v2_get_or_default_string() {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route) => {
                select_ordinary_map_owned_any_v2_get_or_default_string_route(checked, route.program)
            }
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    if admission.ordinary_map_owned_any_v2_get_or_default_i32() {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route) => {
                select_ordinary_map_owned_any_v2_get_or_default_i32_route(checked, route.program)
            }
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::KeysSnapshot
    {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route) if ordinary_map_owned_any_v2_keys_snapshot_materialization_is_exact(&route) => {
                match preflight_ordinary_map_owned_any_v2_keys_snapshot(checked, &route.program) {
                    Ok(()) => DefaultMirRoute::Canonical(Box::new(route.program)),
                    Err(reason) => DefaultMirRoute::Rejected(format!(
                        "ordinary Map/Any owner-v2 KeysSnapshot preflight failed: {reason}"
                    )),
                }
            }
            Ok(_) => DefaultMirRoute::Rejected(format!(
                "{}: Checker KeysSnapshot admission did not materialize its exact independent route receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    if admission.ordinary_map_owned_any_v2_dynamic_keys_get() {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route)
                if ordinary_map_owned_any_v2_dynamic_keys_get_materialization_is_exact(&route) =>
            {
                match preflight_ordinary_map_owned_any_v2_dynamic_keys_get(checked, &route.program) {
                    Ok(()) => DefaultMirRoute::Canonical(Box::new(route.program)),
                    Err(reason) => DefaultMirRoute::Rejected(format!(
                        "ordinary Map/Any owner-v2 DynamicKeysGet preflight failed: {reason}"
                    )),
                }
            }
            Ok(_) => DefaultMirRoute::Rejected(format!(
                "{}: Checker DynamicKeysGet admission did not materialize its exact independent route receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::MapEmptyRootRootQueriesV1
    {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route) => match preflight_map_empty_root_root_queries_v1(checked, &route.program) {
                Ok(()) => DefaultMirRoute::Canonical(Box::new(route.program)),
                Err(e) => DefaultMirRoute::Rejected(format!(
                    "MIR-COVERAGE-001: MapEmptyRootRootQueriesV1 preflight: {e}"
                )),
            },
            Err(e) => DefaultMirRoute::Rejected(e.to_diagnostic().message),
        };
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::RecordUnit
    {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route) if ordinary_map_owned_any_v2_record_unit_materialization_is_exact(&route) => {
                select_ordinary_map_owned_any_v2_record_unit_route(checked, route.program)
            }
            Ok(_) => DefaultMirRoute::Rejected(format!(
                "{}: Checker RecordUnit admission did not materialize its exact independent operation receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::IsEmpty
    {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route) if ordinary_map_owned_any_v2_is_empty_materialization_is_exact(&route) => {
                select_ordinary_map_owned_any_v2_is_empty_route(checked, route.program)
            }
            Ok(_) => DefaultMirRoute::Rejected(format!(
                "{}: Checker IsEmpty admission did not materialize its exact independent operation receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControl
    {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route)
                if ordinary_map_owned_any_v2_map_set_drop_control_materialization_is_exact(
                    &route,
                ) =>
            {
                match preflight_ordinary_map_owned_any_v2_map_set_drop_control(
                    checked,
                    &route.program,
                ) {
                    Ok(()) => DefaultMirRoute::Canonical(Box::new(route.program)),
                    Err(reason) => DefaultMirRoute::Rejected(format!(
                        "ordinary Map/Any owner-v2 MapSetDropControl preflight failed: {reason}"
                    )),
                }
            }
            Ok(_) => DefaultMirRoute::Rejected(format!(
                "{}: Checker MapSetDropControl admission did not materialize its exact independent operation receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    if admission.ordinary_map_owned_any_v2_branch_local_set_diamond() {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route)
                if ordinary_map_owned_any_v2_branch_local_set_diamond_materialization_is_exact(
                    &route,
                ) =>
            {
                match preflight_ordinary_map_owned_any_v2_branch_local_set_diamond(
                    checked,
                    &route.program,
                ) {
                    Ok(()) => DefaultMirRoute::Canonical(Box::new(route.program)),
                    Err(reason) => DefaultMirRoute::Rejected(format!(
                        "ordinary Map/Any owner-v2 BranchLocalSetDiamond preflight failed: {reason}"
                    )),
                }
            }
            Ok(_) => DefaultMirRoute::Rejected(format!(
                "{}: Checker BranchLocalSetDiamond admission did not materialize its exact independent operation receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    if admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::CloneIsolation
    {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route)
                if ordinary_map_owned_any_v2_clone_isolation_materialization_is_exact(&route) =>
            {
                match preflight_ordinary_map_owned_any_v2_clone_isolation(checked, &route.program) {
                    Ok(()) => DefaultMirRoute::Canonical(Box::new(route.program)),
                    Err(reason) => DefaultMirRoute::Rejected(format!(
                        "ordinary Map/Any owner-v2 CloneIsolation preflight failed: {reason}"
                    )),
                }
            }
            Ok(_) => DefaultMirRoute::Rejected(format!(
                "{}: Checker CloneIsolation admission did not materialize its exact independent operation receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    let observer_admission = admission.ordinary_map_owner_lifecycle_observer_v1;
    if observer_admission.has_profile_signal() && !observer_admission.is_complete() {
        return DefaultMirRoute::Rejected(format!(
            "{}: Checker recognized an ordinary Map owner lifecycle observer attempt without complete Observer V1 coverage",
            mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if admission.scalar_export && !observer_admission.is_complete() {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route) if route.materialized_scalar_export_candidate => {
                select_scalar_export_route(route.program)
            }
            Ok(_) => DefaultMirRoute::Rejected(format!(
                "{}: complete scalar export admission did not materialize its wrapper receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    if let Some(reason) =
        mimi::core::mir::unsupported_nested_callable_boundary_reason(checked, None)
    {
        return DefaultMirRoute::Rejected(format!(
            "{}: {reason}",
            mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if mimi::core::mir::exported_abi_body_boundary_reason(checked).is_some() {
        return DefaultMirRoute::Rejected(format!(
            "{}: aggregate, generic, variadic, errno, parameter-mode, async, and contract-bearing exports are outside the trusted canonical scalar subset; no legacy export body is reachable",
            mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if let Some(reason) = mimi::core::mir::scalar_actor_contract_boundary_reason(checked) {
        // The bounded Actor NoContracts route cannot carry proof-bearing
        // bodies.  A map owner in the same checked program is still a
        // migrated ownership boundary, so returning the Actor compatibility
        // route here would hide the map receipt behind the old AST backend.
        // Keep the compatibility disposition only for a standalone
        // contract-bearing Actor program; mixed Actor+Map candidates fail
        // closed before any consumer can select legacy.
        let map_owner_candidate = admission.map_root.is_candidate()
            || admission.map_any_owner.is_candidate()
            || admission.ordinary_map_owned_any_v2.is_candidate()
            || admission
                .ordinary_map_owner_lifecycle_observer_v1
                .has_profile_signal()
            || admission
                .ordinary_map_owner_lifecycle_i32_string_v1
                .has_profile_signal()
            || admission
                .ordinary_map_owned_get_i32_string_v1
                .has_profile_signal();
        if map_owner_candidate {
            return DefaultMirRoute::Rejected(format!(
                "{}: {reason}; scalar FFI profile cannot be combined with Map/Any owner profiles; Actor contract boundary cannot hide a Map/Any owner candidate behind compatibility",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        if module_alias_call_hint {
            return DefaultMirRoute::Rejected(format!(
                "{}: resolved module-qualified function graph cannot use the Actor proof-obligations compatibility route: {reason}",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        return DefaultMirRoute::Legacy(LegacyRouteReason::ActorProofObligationsCompatibility);
    }
    // Resolve the finite-effect boundary only after the Actor proof owner and
    // Map/Any composition gates above.  Proof-bearing Actor compatibility
    // deliberately retains its named verifier, while execution graphs with
    // an independent Map/Any owner are rejected by the shared composition
    // gate before an opaque FFI/dynamic effect can mask that reason.
    if let Some(reason) = mimi::core::mir::unknown_effect_boundary_reason(checked) {
        return DefaultMirRoute::Rejected(format!(
            "{}: {reason}",
            mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    // The two historical Actor fallbacks are direct nested-callable methods
    // (capture/shadow).  Once such a candidate is visible in the checked
    // graph, an incomplete lexical-environment receipt is a production route
    // error, never permission to compile the method with the old AST emitter.
    // Keep this after the proof boundary above so explicit Actor proof
    // obligations retain their named compatibility verifier.
    if mimi::core::mir::has_scalar_actor_nested_callable_candidate(checked)
        && !admission.scalar_actor
    {
        return DefaultMirRoute::Rejected(format!(
            "{}: nested Actor callable candidate has no complete canonical lexical-environment receipt; legacy Actor fallback is disabled",
            mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if observer_admission.is_complete() {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route)
                if ordinary_map_owner_lifecycle_observer_v1_materialization_is_exact(&route) =>
            {
                select_ordinary_map_owner_lifecycle_observer_v1_route(checked, route.program)
            }
            Ok(_) => DefaultMirRoute::Rejected(format!(
                "{}: complete ordinary Map owner Observer V1 admission did not materialize its exact route receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    if admission.scalar_actor {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route) if route.materialized_scalar_actor_candidate => {
                select_scalar_actor_route(route.program)
            }
            Ok(_) => DefaultMirRoute::Rejected(format!(
                "{}: complete scalar Actor admission did not materialize its domain receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    // The shared envelope also owns the materialization receipts, preventing
    // this selector from growing a second Set/record lowering walk.
    if admission
        .ordinary_map_owned_get_i32_string_v1
        .has_profile_signal()
    {
        if !admission.ordinary_map_owned_get_i32_string_v1_complete() {
            // This is a new typed API with no legacy Bytecode/LLVM semantics.
            // Preserve compatibility for existing untyped Map APIs, but never
            // send an incomplete typed-Get attempt to a backend that cannot
            // represent its Result or ownership contract.
            return DefaultMirRoute::Rejected(format!(
                "{}: Checker recognized an ordinary Map owned-get attempt without complete consumer coverage",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route)
                if route.materialized_ordinary_map_owned_get_i32_string_v1_candidate =>
            {
                select_ordinary_map_owned_get_i32_string_v1_route(route.program)
            }
            Ok(_) => DefaultMirRoute::Rejected(format!(
                "{}: complete ordinary Map owned-get admission did not materialize its operation receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    if admission.ordinary_map_owned_any_v2.has_profile_signal() {
        if admission.ordinary_map_owned_any_v2_removed_key_checked_miss() {
            return match materialize_canonical_route(checked, merged_file) {
                Ok(route)
                    if ordinary_map_owned_any_v2_removed_key_checked_miss_materialization_is_exact(
                        &route,
                    ) =>
                {
                    select_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
                        checked,
                        route.program,
                    )
                }
                Ok(_) => DefaultMirRoute::Rejected(format!(
                    "{}: Checker RemovedKeyCheckedMiss admission did not materialize its exact independent operation receipt",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )),
                Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
            };
        }
        if admission.ordinary_map_owned_any_v2_zero_miss_i32() {
            return match materialize_canonical_route(checked, merged_file) {
                Ok(route) if ordinary_map_owned_any_v2_zero_miss_i32_materialization_is_exact(&route) => {
                    select_ordinary_map_owned_any_v2_zero_miss_i32_route(
                        checked,
                        route.program,
                    )
                }
                Ok(_) => DefaultMirRoute::Rejected(format!(
                    "{}: Checker ZeroMissI32 admission did not materialize its exact independent operation receipt",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )),
                Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
            };
        }
        if admission.ordinary_map_owned_any_v2_values_crosscall_string_caller_index1() {
            return match materialize_canonical_route(checked, merged_file) {
                Ok(route)
                    if route
                        .ordinary_map_owned_any_v2_values_crosscall_string_caller_route_receipt
                        .as_ref()
                        .is_some_and(|receipt| {
                            receipt
                                == &route.program.route_receipt(
                                    mimi::core::mir::CanonicalMirRouteProfile::
                                        OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1
                                        .as_str(),
                                )
                        }) =>
                {
                    select_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
                        checked,
                        route.program,
                    )
                }
                Ok(_) => DefaultMirRoute::Rejected(format!(
                    "{}: Checker ValuesCrosscallStringCallerIndex1 admission did not materialize its exact independent route receipt",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )),
                Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
            };
        }
        if admission.ordinary_map_owned_any_v2_values_crosscall_string_caller_v1() {
            return match materialize_canonical_route(checked, merged_file) {
                Ok(route)
                    if route
                        .ordinary_map_owned_any_v2_values_crosscall_string_caller_route_receipt
                        .as_ref()
                        .is_some_and(|receipt| {
                            receipt
                                == &route.program.route_receipt(
                                    mimi::core::mir::CanonicalMirRouteProfile::
                                        OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller
                                        .as_str(),
                                )
                        }) =>
                {
                    select_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(
                        checked,
                        route.program,
                    )
                }
                Ok(_) => DefaultMirRoute::Rejected(format!(
                    "{}: Checker ValuesCrosscallStringCaller admission did not materialize its exact independent route receipt",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )),
                Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
            };
        }
        if admission.ordinary_map_owned_any_v2_values_producer_caller() {
            return match materialize_canonical_route(checked, merged_file) {
                Ok(route)
                    if route
                        .ordinary_map_owned_any_v2_values_producer_caller_route_receipt
                        .as_ref()
                        .is_some_and(|receipt| {
                            receipt
                                == &route.program.route_receipt(
                                    mimi::core::mir::CanonicalMirRouteProfile::
                                        OrdinaryMapOwnedAnyV2ValuesProducerCaller
                                        .as_str(),
                                )
                        }) =>
                {
                    select_ordinary_map_owned_any_v2_values_producer_caller_route(
                        checked,
                        route.program,
                    )
                }
                Ok(_) => DefaultMirRoute::Rejected(format!(
                    "{}: Checker ValuesProducerCaller admission did not materialize its exact independent route receipt",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )),
                Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
            };
        }
        if admission.ordinary_map_owned_any_v2_map_root_get_any_caller() {
            return match materialize_canonical_route(checked, merged_file) {
                Ok(route)
                    if route
                        .ordinary_map_owned_any_v2_map_root_get_any_caller_route_receipt
                        .as_ref()
                        .is_some_and(|receipt| {
                            receipt
                                == &route.program.route_receipt(
                                    mimi::core::mir::CanonicalMirRouteProfile::
                                        OrdinaryMapOwnedAnyV2MapRootGetAnyCaller
                                        .as_str(),
                                )
                        }) =>
                {
                    select_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
                        checked,
                        route.program,
                    )
                }
                Ok(_) => DefaultMirRoute::Rejected(format!(
                    "{}: Checker MapRootGetAnyCaller admission did not materialize its exact independent route receipt",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )),
                Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
            };
        }
        if admission.ordinary_map_owned_any_v2_map_root_producer_caller() {
            return match materialize_canonical_route(checked, merged_file) {
                Ok(route)
                    if route
                        .ordinary_map_owned_any_v2_map_root_producer_caller_route_receipt
                        .as_ref()
                        .is_some_and(|receipt| {
                            receipt
                                == &route.program.route_receipt(
                                    mimi::core::mir::CanonicalMirRouteProfile::
                                        OrdinaryMapOwnedAnyV2MapRootProducerCaller
                                        .as_str(),
                                )
                        }) =>
                {
                    select_ordinary_map_owned_any_v2_map_root_producer_caller_route(
                        checked,
                        route.program,
                    )
                }
                Ok(_) => DefaultMirRoute::Rejected(format!(
                    "{}: Checker MapRootProducerCaller admission did not materialize its exact independent route receipt",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )),
                Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
            };
        }
        if admission.ordinary_map_owned_any_v2.is_direct_print() {
            return match materialize_canonical_route(checked, merged_file) {
                Ok(route)
                    if route.materialized_ordinary_map_owned_any_v2_direct_print_candidate
                        && route
                            .ordinary_map_owned_any_v2_direct_print_route_receipt
                            .as_ref()
                            .is_some_and(|receipt| {
                                receipt
                                    == &route.program.route_receipt(
                                        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE,
                                    )
                            }) =>
                {
                    select_ordinary_map_owned_any_v2_direct_print_route(route.program)
                }
                Ok(_) => DefaultMirRoute::Rejected(format!(
                    "{}: Checker DirectPrint admission did not materialize its exact independent println receipt",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )),
                Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
            };
        }
        if admission.ordinary_map_owned_any_v2
            == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesMixedSnapshotRemove
        {
            return match materialize_canonical_route(checked, merged_file) {
                Ok(route)
                    if route.materialized_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_candidate
                        && route
                            .ordinary_map_owned_any_v2_values_mixed_snapshot_remove_route_receipt
                            .as_ref()
                            .is_some_and(|receipt| {
                                receipt
                                    == &route.program.route_receipt(
                                        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE,
                                    )
                            }) =>
                {
                    select_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_route(
                        checked,
                        route.program,
                    )
                }
                Ok(_) => DefaultMirRoute::Rejected(format!(
                    "{}: Checker ValuesMixedSnapshotRemove admission did not materialize its exact independent operation receipt",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )),
                Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
            };
        }
        if admission.ordinary_map_owned_any_v2
            == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesSnapshotReplacement
        {
            return match materialize_canonical_route(checked, merged_file) {
                Ok(route)
                    if route
                        .materialized_ordinary_map_owned_any_v2_values_snapshot_replacement_candidate
                        && route
                            .ordinary_map_owned_any_v2_values_snapshot_replacement_route_receipt
                            .as_ref()
                            .is_some_and(|receipt| {
                                receipt
                                    == &route.program.route_receipt(
                                        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE,
                                    )
                            }) =>
                {
                    select_ordinary_map_owned_any_v2_values_snapshot_replacement_route(
                        checked,
                        route.program,
                    )
                }
                Ok(_) => DefaultMirRoute::Rejected(format!(
                    "{}: Checker ValuesSnapshotReplacement admission did not materialize its exact independent operation receipt",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )),
                Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
            };
        }
        if admission
            .ordinary_map_owned_any_v2
            .is_values_mixed_snapshot()
        {
            return match materialize_canonical_route(checked, merged_file) {
                Ok(route)
                    if route.materialized_ordinary_map_owned_any_v2_values_mixed_snapshot_candidate
                        && route
                            .ordinary_map_owned_any_v2_values_mixed_snapshot_route_receipt
                            .as_ref()
                            .is_some_and(|receipt| {
                                receipt
                                    == &route.program.route_receipt(
                                        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE,
                                    )
                            }) =>
                {
                    select_ordinary_map_owned_any_v2_values_mixed_snapshot_route(
                        checked,
                        route.program,
                    )
                }
                Ok(_) => DefaultMirRoute::Rejected(format!(
                    "{}: Checker ValuesMixedSnapshot admission did not materialize its exact independent operation receipt",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )),
                Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
            };
        }
        if admission.ordinary_map_owned_any_v2.is_values_field_move() {
            return match materialize_canonical_route(checked, merged_file) {
                Ok(route)
                    if route.materialized_ordinary_map_owned_any_v2_values_field_move_candidate
                        && route
                            .ordinary_map_owned_any_v2_values_field_move_route_receipt
                            .as_ref()
                            .is_some_and(|receipt| {
                                receipt
                                    == &route.program.route_receipt(
                                        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE,
                                    )
                            }) =>
                {
                    select_ordinary_map_owned_any_v2_values_field_move_route(
                        checked,
                        route.program,
                    )
                }
                Ok(_) => DefaultMirRoute::Rejected(format!(
                    "{}: Checker ValuesFieldMove admission did not materialize its exact independent operation receipt",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )),
                Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
            };
        }
        if admission
            .ordinary_map_owned_any_v2
            .is_values_local_index_i32()
        {
            return match materialize_canonical_route(checked, merged_file) {
                Ok(route)
                    if route
                        .ordinary_map_owned_any_v2_values_local_index_i32_route_receipt
                        .as_ref()
                        .is_some_and(|receipt| {
                            receipt
                                == &route.program.route_receipt(
                                    mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_PROFILE,
                                )
                        }) =>
                {
                    select_ordinary_map_owned_any_v2_values_local_index_i32_route(
                        checked,
                        route.program,
                    )
                }
                Ok(_) => DefaultMirRoute::Rejected(format!(
                    "{}: Checker ValuesLocalIndexI32 admission did not materialize its exact independent route receipt",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )),
                Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
            };
        }
        if admission
            .ordinary_map_owned_any_v2
            .is_values_heterogeneous_string_index1()
        {
            return match materialize_canonical_route(checked, merged_file) {
                Ok(route)
                    if route
                        .ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route_receipt
                        .as_ref()
                        .is_some_and(|receipt| {
                            receipt
                                == &route.program.route_receipt(
                                    mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_HETEROGENEOUS_STRING_INDEX1_PROFILE,
                                )
                        }) =>
                {
                    select_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route(
                        checked,
                        route.program,
                    )
                }
                Ok(_) => DefaultMirRoute::Rejected(format!(
                    "{}: Checker ValuesHeterogeneousStringIndex1 admission did not materialize its exact independent route receipt",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )),
                Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
            };
        }
        if admission.ordinary_map_owned_any_v2
            == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ComputedKeyGet
        {
            return match materialize_canonical_route(checked, merged_file) {
                Ok(route)
                    if route
                        .materialized_ordinary_map_owned_any_v2_computed_key_get_candidate
                        && route
                            .ordinary_map_owned_any_v2_computed_key_get_route_receipt
                            .as_ref()
                            .is_some_and(|receipt| {
                                receipt
                                    == &route.program.route_receipt(
                                        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE,
                                    )
                            }) =>
                {
                    select_ordinary_map_owned_any_v2_computed_key_get_route(
                        checked,
                        route.program,
                    )
                }
                Ok(_) => DefaultMirRoute::Rejected(format!(
                    "{}: Checker ComputedKeyGet admission did not materialize its exact independent operation receipt",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )),
                Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
            };
        }
        if admission
            .ordinary_map_owned_any_v2
            .is_computed_string_key_owner_chain()
        {
            return match materialize_canonical_route(checked, merged_file) {
                Ok(route)
                    if route
                        .ordinary_map_owned_any_v2_computed_string_key_owner_chain_route_receipt
                        .as_ref()
                        .is_some_and(|receipt| {
                            receipt
                                == &route.program.route_receipt(
                                    mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE,
                                )
                        }) =>
                {
                    select_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route(
                        checked,
                        route.program,
                    )
                }
                Ok(_) => DefaultMirRoute::Rejected(format!(
                    "{}: Checker ComputedStringKeyOwnerChain admission did not materialize its exact route receipt",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )),
                Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
            };
        }
        if admission.ordinary_map_owned_any_v2_checked_get() {
            return match materialize_canonical_route(checked, merged_file) {
                Ok(route)
                    if route.materialized_ordinary_map_owned_any_v2_checked_get_candidate =>
                {
                    select_ordinary_map_owned_any_v2_checked_get_route(checked, route.program)
                }
                Ok(_) => DefaultMirRoute::Rejected(format!(
                    "{}: Checker CheckedGet admission did not materialize its independent narrow operation receipt",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                )),
                Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
            };
        }
        if !admission.ordinary_map_owned_any_v2_complete() {
            return DefaultMirRoute::Rejected(format!(
                "{}: Checker recognized an ordinary Map/Any owner-v2 candidate without complete operation-attempt coverage",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route) if route.materialized_ordinary_map_owned_any_v2_candidate => {
                select_ordinary_map_owned_any_v2_route(route.program)
            }
            Ok(_) => DefaultMirRoute::Rejected(format!(
                "{}: complete ordinary Map/Any owner-v2 admission did not materialize its operation receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    let ordinary_profile_covers_incomplete_map_any = admission
        .ordinary_map_owner_lifecycle_i32_string_v1_complete()
        && mimi::core::mir::map_any_owner_v1_actions_are_within_ordinary_map_profile(checked);
    if admission.map_any_owner.is_candidate()
        && !admission.map_any_owner_complete()
        && !ordinary_profile_covers_incomplete_map_any
    {
        return DefaultMirRoute::Rejected(format!(
            "{}: Checker recognized a MapAnyOwner v1 candidate but withheld its complete independent lifecycle receipt",
            mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if admission.map_any_owner.is_candidate() && admission.map_any_owner_complete() {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route) if route.materialized_map_any_owner_candidate => {
                select_map_any_owner_v1_route(route.program)
            }
            Ok(_) => DefaultMirRoute::Rejected(format!(
                "{}: complete MapAnyOwner v1 admission did not materialize its operation receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    if admission
        .ordinary_map_owner_lifecycle_i32_string_v1
        .has_profile_signal()
    {
        if !admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .is_complete()
        {
            // Ordinary Map/Any is a production ownership boundary.  Its
            // incomplete checker ledger must be rejected at the default
            // route as well as by direct MIR construction; otherwise the
            // AST compatibility emitter could silently retain an unreceipted
            // borrow/escape/cleanup operation.
            return DefaultMirRoute::Rejected(format!(
                "{}: Checker recognized an ordinary Map owner lifecycle attempt without complete ownership coverage",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route)
                if route
                    .materialized_ordinary_map_owner_lifecycle_i32_string_v1_candidate =>
            {
                select_ordinary_map_owner_lifecycle_i32_string_v1_route(route.program)
            }
            Ok(_) => DefaultMirRoute::Rejected(format!(
                "{}: complete ordinary Map owner admission did not materialize its operation receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    if admission.map_root.is_candidate() {
        if !admission.map_root_complete() {
            return DefaultMirRoute::Rejected(format!(
                "{}: Checker recognized a MapRoot candidate but withheld its complete local lifecycle receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            ));
        }
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route) if route.materialized_map_root_candidate => {
                select_map_root_route(route.program)
            }
            Ok(_) => DefaultMirRoute::Rejected(format!(
                "{}: complete MapRoot admission did not materialize its operation receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    if admission.scalar_ffi {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route) => select_scalar_ffi_route(route.program),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    if admission.scalar_nested {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route) if route.materialized_scalar_nested_candidate => {
                select_scalar_nested_route(route.program)
            }
            Ok(_) => DefaultMirRoute::Rejected(format!(
                "{}: complete scalar nested admission did not materialize its nested-callable receipt",
                mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    if admission.scalar_generic_identity_i32_complete() {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route) => select_scalar_generic_identity_route(
                route.program,
                mimi::core::mir::SCALAR_GENERIC_IDENTITY_I32_ISLAND,
                mimi::core::mir::validate_scalar_generic_identity_island,
            ),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    if admission.scalar_generic_identity_i64_complete() {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route) => select_scalar_generic_identity_route(
                route.program,
                mimi::core::mir::SCALAR_GENERIC_IDENTITY_I64_ISLAND,
                mimi::core::mir::validate_scalar_generic_identity_i64_island,
            ),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    let collection_admission = admission.collection;
    let option_string_admission = admission.option_string;
    let option_nested_tuple_admission = admission.option_nested_tuple;
    let generic_variant_admission = admission.generic_variant;
    let generic_option_projection_admission = admission.generic_option_projection;
    let generic_option_projection_fallback_admission = admission.generic_option_projection_fallback;
    let generic_result_projection_admission = admission.generic_result_projection;
    let generic_result_projection_fallback_admission = admission.generic_result_projection_fallback;
    let managed_result_call_admission = admission.managed_result_call;
    let copy_option_i32_admission = admission.copy_option_i32;
    let copy_option_bool_admission = admission.copy_option_bool;
    let copy_option_i64_admission = admission.copy_option_i64;
    let copy_option_f64_admission = admission.copy_option_f64;
    let copy_result_i32_admission = admission.copy_result_i32;
    let flow_failure_retry_hint = admission.flow_failure_retry;
    let session_hint = !matches!(
        admission.session,
        mimi::core::mir::SessionChannelAdmission::OutsideProfile
    );
    let complete_session_candidate = matches!(
        admission.session,
        mimi::core::mir::SessionChannelAdmission::CompleteCoverage
    );
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
    let generic_record_projection_unsupported_hint =
        mimi::core::mir::has_unsupported_generic_record_projection_candidate(checked);
    let generic_record_update_unsupported_hint =
        mimi::core::mir::has_unsupported_generic_record_update_candidate(checked);
    let generic_record_update_candidate_hint = generic_record_update_unsupported_hint
        || mimi::core::mir::has_generic_record_update_candidate(checked);
    let user_record_hint = checked.type_defs().values().any(|definition| {
        definition.kind == mimi::core::ResolvedTypeKind::Record
            && matches!(definition.origin, mimi::core::Origin::User(_))
    });
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
    let option_nested_tuple_hint = !matches!(
        option_nested_tuple_admission,
        mimi::core::mir::OptionNestedTupleVariantAdmission::OutsideProfile
    );
    let complete_option_nested_tuple_candidate = matches!(
        option_nested_tuple_admission,
        mimi::core::mir::OptionNestedTupleVariantAdmission::CompleteCoverage
    );
    let generic_variant_hint =
        mimi::core::mir::has_generic_variant_predicate_operation_candidate(checked)
            || !matches!(
                generic_variant_admission,
                mimi::core::mir::GenericVariantPredicateAdmission::OutsideProfile
            );
    let complete_generic_variant_candidate = matches!(
        generic_variant_admission,
        mimi::core::mir::GenericVariantPredicateAdmission::CompleteCoverage
    );
    let generic_option_projection_unsupported_hint =
        mimi::core::mir::has_generic_option_projection_operation_candidate(checked);
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
        mimi::core::mir::has_generic_option_projection_fallback_operation_candidate(checked);
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
        mimi::core::mir::has_generic_result_projection_operation_candidate(checked);
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
        mimi::core::mir::has_generic_result_projection_fallback_operation_candidate(checked);
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
    let managed_result_call_hint = mimi::core::mir::has_managed_result_call_candidate(checked);
    let complete_managed_result_call_candidate = matches!(
        managed_result_call_admission,
        mimi::core::mir::ManagedResultCallAdmission::CompleteCoverage
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
    // A complete checker Flow admission is a migrated route even when the
    // narrow single-transition hint is false (for example, a multi-transition
    // graph such as `flow_counter.mimi`).  If canonical materialization then
    // fails, that graph must fail closed; passing only `flow_candidate` to the
    // rejection helper would incorrectly turn the construction failure into
    // an implicit legacy route.
    let flow_rejection_candidate =
        flow_candidate || complete_flow_candidate || flow_failure_retry_hint;
    // R6-1040: a checker-legal multi-target union (the compiler-owned Fault
    // absorption face) is its own migrated candidate, not an outside-profile
    // input.  Whether the graph actually routes canonical is decided by the
    // promoted tagged-union contract after materialization; the open face
    // still trips the union compatibility veto below.
    let union_candidate_hint = may_contain_multi_target_flow_union(checked);
    let generic_trait_candidate =
        mimi::codegen::has_static_generic_trait_specialization_candidate(checked);
    let generic_trait_only_candidate = generic_trait_candidate
        && !record_hint
        && !flow_candidate
        && !complete_flow_candidate
        && !flow_failure_retry_hint
        && !union_candidate_hint
        && !user_record_hint
        && !option_string_hint
        && !option_nested_tuple_hint
        && !generic_variant_hint
        && !generic_option_projection_hint
        && !generic_option_projection_fallback_hint
        && !generic_result_projection_hint
        && !generic_result_projection_fallback_hint
        && !managed_result_call_hint
        && !copy_option_i32_hint
        && !copy_option_bool_hint
        && !copy_option_i64_hint
        && !copy_option_f64_hint
        && !copy_result_i32_hint
        && !session_hint
        && !module_alias_call_hint;
    if !admission.has_candidate() && has_native_data_export_declaration(checked) {
        return match materialize_canonical_route(checked, merged_file) {
            Ok(route) => select_native_data_export_route(checked, route.program),
            Err(error) => DefaultMirRoute::Rejected(error.to_diagnostic().message),
        };
    }
    if !collection_hint
        && !record_hint
        && !flow_candidate
        && !complete_flow_candidate
        && !flow_failure_retry_hint
        && !union_candidate_hint
        && !option_string_hint
        && !option_nested_tuple_hint
        && !generic_variant_hint
        && !generic_option_projection_hint
        && !generic_option_projection_fallback_hint
        && !generic_result_projection_hint
        && !generic_result_projection_fallback_hint
        && !managed_result_call_hint
        && !copy_option_i32_hint
        && !copy_option_bool_hint
        && !copy_option_i64_hint
        && !copy_option_f64_hint
        && !copy_result_i32_hint
        && !session_hint
        && !module_alias_call_hint
        && !generic_trait_only_candidate
    {
        return DefaultMirRoute::Legacy(LegacyRouteReason::OutsideMigratedProfile);
    }
    if managed_result_call_hint
        && !complete_managed_result_call_candidate
        && !flow_failure_retry_hint
    {
        return DefaultMirRoute::Rejected(
            "managed Result direct-call candidate is outside complete coverage".into(),
        );
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
            "generic Option projection fallback candidate is outside complete coverage".into(),
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
            CopyCandidateFlags {
                option_i32: true,
                option_bool: false,
                option_i64: false,
                option_f64: false,
                result_i32: false,
            },
            "Copy Option<i32> projection candidate is outside complete coverage",
        );
    }
    if copy_option_bool_hint && !complete_copy_option_bool_candidate {
        return reject_migrated_candidates_with_copy(
            flow_candidate,
            false,
            false,
            option_string_hint,
            CopyCandidateFlags {
                option_i32: false,
                option_bool: true,
                option_i64: false,
                option_f64: false,
                result_i32: false,
            },
            "Copy Option<bool> projection candidate is outside complete coverage",
        );
    }
    if copy_option_i64_hint && !complete_copy_option_i64_candidate {
        return reject_migrated_candidates_with_copy(
            flow_candidate,
            false,
            false,
            option_string_hint,
            CopyCandidateFlags {
                option_i32: false,
                option_bool: false,
                option_i64: true,
                option_f64: false,
                result_i32: false,
            },
            "Copy Option<i64> projection candidate is outside complete coverage",
        );
    }
    if copy_option_f64_hint && !complete_copy_option_f64_candidate {
        return reject_migrated_candidates_with_copy(
            flow_candidate,
            false,
            false,
            option_string_hint,
            CopyCandidateFlags {
                option_i32: false,
                option_bool: false,
                option_i64: false,
                option_f64: true,
                result_i32: false,
            },
            "Copy Option<f64> projection candidate is outside complete coverage",
        );
    }
    if copy_result_i32_hint && !complete_copy_result_i32_candidate {
        return reject_migrated_candidates_with_copy(
            flow_candidate,
            false,
            false,
            option_string_hint,
            CopyCandidateFlags {
                option_i32: copy_option_i32_hint,
                option_bool: copy_option_bool_hint,
                option_i64: copy_option_i64_hint,
                option_f64: copy_option_f64_hint,
                result_i32: true,
            },
            "Copy Result<i32, i32> projection candidate is outside complete coverage",
        );
    }
    if option_nested_tuple_hint && !complete_option_nested_tuple_candidate {
        return reject_migrated_candidates(
            flow_candidate,
            false,
            false,
            true,
            "nested Option tuple variant candidate is outside complete coverage",
        );
    }

    if generic_trait_only_candidate {
        return match build_canonical_program(checked, merged_file) {
            Ok(program) => select_static_generic_trait_specialization_route(program),
            Err(reason) => DefaultMirRoute::Rejected(format!(
                "static generic trait specialization canonical MIR construction failed: {reason}"
            )),
        };
    }

    let route = match materialize_canonical_route(checked, merged_file) {
        Ok(route) => route,
        Err(mimi::core::mir::CanonicalMirRouteMaterializationError::Complete {
            profile,
            stage,
            message,
        }) => {
            if matches!(
                profile,
                mimi::core::mir::CanonicalMirRouteProfile::ManagedResultCall
            ) {
                return DefaultMirRoute::Rejected(format!(
                    "managed Result direct-call canonical MIR {stage:?} failed: {message}"
                ));
            }
            if matches!(
                profile,
                mimi::core::mir::CanonicalMirRouteProfile::SessionChannel
            ) {
                return DefaultMirRoute::Rejected(format!(
                    "session-channel canonical MIR {stage:?} failed: {message}"
                ));
            }
            if module_alias_call_hint {
                return DefaultMirRoute::Rejected(format!(
                    "resolved module-qualified function graph failed canonical MIR {stage:?}: {message}"
                ));
            }
            let reason = match stage {
                mimi::core::mir::CanonicalMirRouteFailureStage::Construction => {
                    if generic_record_projection_unsupported_hint {
                        "canonical generic record projection candidate did not materialize a supported scalar MIR shape".into()
                    } else if generic_record_update_candidate_hint {
                        "canonical generic record update candidate did not materialize a supported scalar MIR shape".into()
                    } else if matches!(
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
            return reject_migrated_candidates_with_copy(
                flow_rejection_candidate,
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
                        | mimi::core::mir::CanonicalMirRouteProfile::NonCopyOptionNestedTupleVariant
                ),
                CopyCandidateFlags {
                    option_i32: matches!(
                        profile,
                        mimi::core::mir::CanonicalMirRouteProfile::CopyOptionI32Variant
                    ),
                    option_bool: matches!(
                        profile,
                        mimi::core::mir::CanonicalMirRouteProfile::CopyOptionBoolVariant
                    ),
                    option_i64: matches!(
                        profile,
                        mimi::core::mir::CanonicalMirRouteProfile::CopyOptionI64Variant
                    ),
                    option_f64: matches!(
                        profile,
                        mimi::core::mir::CanonicalMirRouteProfile::CopyOptionF64Variant
                    ),
                    result_i32: matches!(
                        profile,
                        mimi::core::mir::CanonicalMirRouteProfile::CopyResultI32Variant
                    ),
                },
                reason,
            );
        }
        Err(mimi::core::mir::CanonicalMirRouteMaterializationError::Compatibility { .. }) => {
            // A mixed graph with no migrated operation retains the explicit
            // compatibility route.  A checker-recognized List operation is
            // different: its unsupported shape must fail closed even when
            // canonical construction cannot produce a receipt, otherwise a
            // List<T> contract hole would silently enter legacy.
            // The same rule applies to the broad complete Flow admission:
            // cross-state nominal payloads are already a migrated boundary,
            // even when a mixed collection scan classified the construction
            // error as compatibility.  Do not let that combination re-enter
            // the AST emitter.
            if complete_flow_candidate {
                return reject_migrated_candidates(
                    true,
                    false,
                    false,
                    false,
                    "canonical Flow MIR construction failed",
                );
            }
            if managed_result_call_hint && !flow_failure_retry_hint {
                return DefaultMirRoute::Rejected(
                    "managed Result direct-call candidate did not materialize a supported MIR shape"
                        .into(),
                );
            }
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
            // A generic Set facade admitted under a Complete scan carries no
            // compatibility floor: a construction failure means the facade
            // graph itself did not lower, so it must fail closed instead of
            // re-entering the legacy route.  A Complete scan without such a
            // callable is the nested-assign coverage-scan false complete and
            // keeps its designed compatibility disposition.
            if complete_collection_candidate
                && mimi::core::mir::has_unsupported_generic_set_facade_candidate(checked)
            {
                return reject_migrated_candidates(
                    flow_candidate,
                    true,
                    false,
                    false,
                    "canonical MIR construction failed",
                );
            }
            if record_hint && generic_record_projection_unsupported_hint {
                return reject_migrated_candidates(
                    flow_candidate,
                    false,
                    true,
                    false,
                    "canonical generic record projection candidate did not materialize a supported scalar MIR shape",
                );
            }
            if record_hint && generic_record_update_unsupported_hint {
                return reject_migrated_candidates(
                    flow_candidate,
                    false,
                    true,
                    false,
                    "canonical generic record update candidate did not materialize a supported scalar MIR shape",
                );
            }
            if generic_variant_hint
                && mimi::core::mir::has_generic_variant_predicate_operation_candidate(checked)
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
            if option_nested_tuple_hint {
                return reject_migrated_candidates(
                    false,
                    false,
                    false,
                    true,
                    "canonical nested Option tuple candidate did not materialize a supported MIR shape",
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
                    CopyCandidateFlags {
                        option_i32: true,
                        option_bool: false,
                        option_i64: false,
                        option_f64: false,
                        result_i32: false,
                    },
                    "canonical Copy Option<i32> projection candidate did not materialize a supported MIR shape",
                );
            }
            if copy_option_bool_hint {
                return reject_migrated_candidates_with_copy(
                    false,
                    false,
                    false,
                    false,
                    CopyCandidateFlags {
                        option_i32: false,
                        option_bool: true,
                        option_i64: false,
                        option_f64: false,
                        result_i32: false,
                    },
                    "canonical Copy Option<bool> projection candidate did not materialize a supported MIR shape",
                );
            }
            if copy_option_i64_hint {
                return reject_migrated_candidates_with_copy(
                    false,
                    false,
                    false,
                    false,
                    CopyCandidateFlags {
                        option_i32: false,
                        option_bool: false,
                        option_i64: true,
                        option_f64: false,
                        result_i32: false,
                    },
                    "canonical Copy Option<i64> projection candidate did not materialize a supported MIR shape",
                );
            }
            if copy_option_f64_hint {
                return reject_migrated_candidates_with_copy(
                    false,
                    false,
                    false,
                    false,
                    CopyCandidateFlags {
                        option_i32: false,
                        option_bool: false,
                        option_i64: false,
                        option_f64: true,
                        result_i32: false,
                    },
                    "canonical Copy Option<f64> projection candidate did not materialize a supported MIR shape",
                );
            }
            if copy_result_i32_hint {
                return reject_migrated_candidates_with_copy(
                    false,
                    false,
                    false,
                    false,
                    CopyCandidateFlags {
                        option_i32: false,
                        option_bool: false,
                        option_i64: false,
                        option_f64: false,
                        result_i32: true,
                    },
                    "canonical Copy Result<i32, i32> projection candidate did not materialize a supported MIR shape",
                );
            }
            if module_alias_call_hint {
                return DefaultMirRoute::Rejected(format!(
                    "{}: resolved module-qualified function call did not materialize a complete Canonical MIR graph",
                    mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                ));
            }
            return DefaultMirRoute::Legacy(
                LegacyRouteReason::MixedCoverageWithoutMaterializedCandidate,
            );
        }
    };
    let canonical = &route.program;
    let copy_record = route.materialized_record_candidate;
    let owned_record_route_candidate =
        mimi::core::mir::contains_owned_record_projection_candidate(canonical);
    let materialized_collection_candidate = route.materialized_collection_candidate;
    let materialized_flow_candidate = route.materialized_flow_candidate;
    let materialized_flow_failure_retry_candidate = route.materialized_flow_failure_retry_candidate;
    let materialized_option_string_candidate = route.materialized_option_string_candidate;
    let materialized_option_nested_tuple_candidate =
        route.materialized_option_nested_tuple_candidate;
    let materialized_generic_variant_candidate = route.materialized_generic_variant_candidate;
    let materialized_generic_option_projection_candidate =
        route.materialized_generic_option_projection_candidate;
    let materialized_generic_option_projection_fallback_candidate =
        route.materialized_generic_option_projection_fallback_candidate;
    let materialized_generic_result_projection_candidate =
        route.materialized_generic_result_projection_candidate;
    let materialized_generic_result_projection_fallback_candidate =
        route.materialized_generic_result_projection_fallback_candidate;
    let materialized_managed_result_call_candidate =
        route.materialized_managed_result_call_candidate;
    let materialized_copy_option_i32_candidate = route.materialized_copy_option_i32_candidate;
    let materialized_copy_option_bool_candidate = route.materialized_copy_option_bool_candidate;
    let materialized_copy_option_i64_candidate = route.materialized_copy_option_i64_candidate;
    let materialized_copy_option_f64_candidate = route.materialized_copy_option_f64_candidate;
    let materialized_copy_result_i32_candidate = route.materialized_copy_result_i32_candidate;
    let materialized_session_candidate = route.materialized_session_candidate;
    let flow_route_candidate = flow_candidate
        || complete_flow_candidate
        || materialized_flow_candidate
        || flow_failure_retry_hint;
    let flow_failure_route_candidate =
        flow_failure_retry_hint || materialized_flow_failure_retry_candidate;
    let exact_f64_flow_failure_route_candidate =
        mimi::core::mir::is_exact_cross_state_f64_failure_receipt(checked);
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
    let managed_result_call_route_candidate = complete_managed_result_call_candidate
        || (managed_result_call_hint && materialized_managed_result_call_candidate);
    // The multi-target Flow union face is a first-class route candidate only
    // when it fully closed onto the flat Copy native contract; the open face
    // is vetoed to the compatibility route below.
    let multi_target_union_route_candidate =
        mimi::core::mir::contains_multi_target_flow_union_candidate(canonical)
            && mimi::core::mir::multi_target_flow_union_face_closed(canonical);
    let record_route_candidate =
        complete_record_candidate || (record_hint && copy_record) || generic_route_candidate;
    let option_string_route_candidate = complete_option_string_candidate
        || (option_string_hint && materialized_option_string_candidate)
        || (complete_option_nested_tuple_candidate
            || (option_nested_tuple_hint && materialized_option_nested_tuple_candidate));
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
    // The session island has no partial canonical admission: only the exact
    // differential-proven face whose graph also materializes session
    // receipts routes canonical.  Every mixed-coverage shape keeps the
    // compatibility route unchanged; an unproven hint never becomes a
    // rejection or a canonical admission.
    let session_route_candidate = complete_session_candidate && materialized_session_candidate;
    if flow_route_candidate && !flow_transition_operation {
        return reject_migrated_candidates_with_copy(
            flow_route_candidate,
            collection_route_candidate || (record_route_candidate && copy_record),
            record_route_candidate,
            option_string_route_candidate,
            CopyCandidateFlags {
                option_i32: copy_option_i32_route_candidate,
                option_bool: copy_option_bool_route_candidate,
                option_i64: copy_option_i64_route_candidate,
                option_f64: copy_option_f64_route_candidate,
                result_i32: copy_result_i32_route_candidate,
            },
            "canonical graph did not materialize the selected production operation",
        );
    }
    if flow_candidate && !complete_flow_candidate && !flow_failure_retry_hint {
        return reject_migrated_candidates_with_copy(
            true,
            collection_route_candidate,
            record_route_candidate,
            option_string_route_candidate,
            CopyCandidateFlags {
                option_i32: copy_option_i32_route_candidate,
                option_bool: copy_option_bool_route_candidate,
                option_i64: copy_option_i64_route_candidate,
                option_f64: copy_option_f64_route_candidate,
                result_i32: copy_result_i32_route_candidate,
            },
            "S8 Flow transition candidate is not complete coverage",
        );
    }
    if flow_failure_retry_hint && !materialized_flow_failure_retry_candidate {
        return DefaultMirRoute::Rejected(
            "recoverable Flow candidate did not materialize a canonical failure boundary".into(),
        );
    }
    // Multi-target Flow union compatibility boundary.  A graph whose union
    // face has not closed onto the promoted tagged-union contract (one
    // Copy-scalar or owned-String payload field per variant) keeps the
    // explicit compatibility route: the legacy union path is still alive and
    // reachable for those shapes (R6-1037A audit pins), and admitting the
    // graph would hard-reject working union programs on the default entries.
    if mimi::core::mir::contains_multi_target_flow_union_candidate(canonical)
        && !mimi::core::mir::multi_target_flow_union_face_closed(canonical)
    {
        if module_alias_call_hint {
            return DefaultMirRoute::Rejected(
                "resolved module-qualified function graph includes an unsupported multi-target Flow union".into(),
            );
        }
        return DefaultMirRoute::Legacy(
            LegacyRouteReason::MixedCoverageWithoutMaterializedCandidate,
        );
    }

    // A mixed program is not a partial canonical program.  If its graph does
    // contain a migrated boundary, keep the old path deleted for that
    // boundary and reject the whole route.  If it contains no such operation,
    // it remains an explicit compatibility input and may use Legacy.
    if record_route_candidate
        && !complete_record_candidate
        && !generic_route_candidate
        && !owned_record_route_candidate
    {
        return reject_migrated_candidates_with_copy(
            flow_route_candidate,
            collection_route_candidate,
            true,
            option_string_route_candidate,
            CopyCandidateFlags {
                option_i32: copy_option_i32_route_candidate,
                option_bool: copy_option_bool_route_candidate,
                option_i64: copy_option_i64_route_candidate,
                option_f64: copy_option_f64_route_candidate,
                result_i32: copy_result_i32_route_candidate,
            },
            "flat Copy record materialized inside mixed coverage",
        );
    }
    if option_nested_tuple_hint
        && !complete_option_nested_tuple_candidate
        && materialized_option_nested_tuple_candidate
    {
        return reject_migrated_candidates(
            flow_route_candidate,
            collection_route_candidate,
            record_route_candidate,
            true,
            "nested Option tuple variant materialized inside mixed coverage",
        );
    }
    if option_string_route_candidate
        && !complete_option_string_candidate
        && !complete_option_nested_tuple_candidate
    {
        return reject_migrated_candidates_with_copy(
            flow_route_candidate,
            collection_route_candidate,
            record_route_candidate,
            true,
            CopyCandidateFlags {
                option_i32: copy_option_i32_route_candidate,
                option_bool: copy_option_bool_route_candidate,
                option_i64: copy_option_i64_route_candidate,
                option_f64: copy_option_f64_route_candidate,
                result_i32: copy_result_i32_route_candidate,
            },
            "Option<string> variant materialized inside mixed coverage",
        );
    }
    if copy_option_i32_route_candidate && !complete_copy_option_i32_candidate {
        return reject_migrated_candidates_with_copy(
            flow_route_candidate,
            collection_route_candidate,
            record_route_candidate,
            option_string_route_candidate,
            CopyCandidateFlags {
                option_i32: true,
                option_bool: copy_option_bool_route_candidate,
                option_i64: copy_option_i64_route_candidate,
                option_f64: copy_option_f64_route_candidate,
                result_i32: false,
            },
            "Copy Option<i32> variant materialized inside mixed coverage",
        );
    }
    if copy_option_bool_route_candidate && !complete_copy_option_bool_candidate {
        return reject_migrated_candidates_with_copy(
            flow_route_candidate,
            collection_route_candidate,
            record_route_candidate,
            option_string_route_candidate,
            CopyCandidateFlags {
                option_i32: copy_option_i32_route_candidate,
                option_bool: true,
                option_i64: copy_option_i64_route_candidate,
                option_f64: copy_option_f64_route_candidate,
                result_i32: false,
            },
            "Copy Option<bool> variant materialized inside mixed coverage",
        );
    }
    if copy_result_i32_route_candidate && !complete_copy_result_i32_candidate {
        return reject_migrated_candidates_with_copy(
            flow_route_candidate,
            collection_route_candidate,
            record_route_candidate,
            option_string_route_candidate,
            CopyCandidateFlags {
                option_i32: copy_option_i32_route_candidate,
                option_bool: copy_option_bool_route_candidate,
                option_i64: copy_option_i64_route_candidate,
                option_f64: copy_option_f64_route_candidate,
                result_i32: true,
            },
            "Copy Result<i32, i32> variant materialized inside mixed coverage",
        );
    }
    if !collection_route_candidate
        && !record_route_candidate
        && !flow_route_candidate
        && !flow_failure_route_candidate
        && !option_string_route_candidate
        && !copy_option_i32_route_candidate
        && !copy_option_bool_route_candidate
        && !copy_option_i64_route_candidate
        && !copy_option_f64_route_candidate
        && !copy_result_i32_route_candidate
        && !managed_result_call_route_candidate
        && !multi_target_union_route_candidate
        && !session_route_candidate
        && !module_alias_call_hint
    {
        return DefaultMirRoute::Legacy(
            LegacyRouteReason::MixedCoverageWithoutMaterializedCandidate,
        );
    }

    // S11: the production unit is a complete bounded List/Set executable
    // graph, not an individual opcode.  The island validator consumes only
    // canonical MIR and TypeDesc facts and runs before any verifier/backend
    // preflight.  A real materialized Set facade or List operation is
    // therefore either inside this finite envelope or rejected; it cannot
    // re-enter the legacy route.
    if materialized_collection_candidate {
        let validated = match purpose {
            RoutePurpose::Execution => {
                mimi::core::mir::validate_scalar_collection_island(canonical)
            }
            RoutePurpose::Verification => {
                mimi::core::mir::validate_scalar_collection_graph(canonical)
            }
        };
        if let Err(errors) = validated {
            // R6-1052: the plain-scalar stdout face is a route candidacy over
            // a checker-side type scan, not a migrated operation face.  When
            // the island preflight proves the graph carries an operation the
            // island cannot execute (a shift or power beside the stdout
            // receipt), the program is an explicit compatibility input — the
            // route it already ran on — instead of a hard rejection of a
            // previously working program.  A materialized List/Set OPERATION
            // keeps its cannot-re-enter-legacy tripwire below.
            let ordinary_scalar_contract_candidate =
                mimi::core::mir::has_closed_scalar_contract_candidate(checked);
            if scalar_collection_gate_failure_disposition(
                ordinary_scalar_contract_candidate,
                route.materialized_collection_operation_candidate,
                flow_route_candidate,
            ) == ScalarCollectionGateFailureDisposition::PlainScalarStdoutCompatibility
            {
                if module_alias_call_hint {
                    return DefaultMirRoute::Rejected(format!(
                        "{}: resolved module-qualified function graph failed scalar collection capability gate: {errors:?}",
                        mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
                    ));
                }
                return DefaultMirRoute::Legacy(
                    LegacyRouteReason::PlainScalarIslandPreflightCompatibility,
                );
            }
            return reject_migrated_candidates_with_copy(
                flow_route_candidate,
                true,
                record_route_candidate,
                option_string_route_candidate,
                CopyCandidateFlags {
                    option_i32: copy_option_i32_route_candidate,
                    option_bool: copy_option_bool_route_candidate,
                    option_i64: copy_option_i64_route_candidate,
                    option_f64: copy_option_f64_route_candidate,
                    result_i32: copy_result_i32_route_candidate,
                },
                format!(
                    "{} capability gate failed: {errors:?}",
                    mimi::core::mir::SCALAR_COLLECTION_ISLAND
                ),
            );
        }
    }

    if materialized_option_string_candidate {
        if let Err(errors) = mimi::core::mir::validate_option_string_variant_island(canonical) {
            return reject_migrated_candidates_with_copy(
                flow_route_candidate,
                collection_route_candidate,
                record_route_candidate,
                true,
                CopyCandidateFlags {
                    option_i32: copy_option_i32_route_candidate,
                    option_bool: copy_option_bool_route_candidate,
                    option_i64: copy_option_i64_route_candidate,
                    option_f64: copy_option_f64_route_candidate,
                    result_i32: copy_result_i32_route_candidate,
                },
                format!(
                    "{} capability gate failed: {errors:?}",
                    mimi::core::mir::NON_COPY_OPTION_STRING_VARIANT_ISLAND
                ),
            );
        }
    }

    if materialized_option_nested_tuple_candidate {
        if let Err(errors) = mimi::core::mir::validate_option_nested_tuple_variant_island(canonical)
        {
            return reject_migrated_candidates_with_copy(
                flow_route_candidate,
                collection_route_candidate,
                record_route_candidate,
                true,
                CopyCandidateFlags {
                    option_i32: copy_option_i32_route_candidate,
                    option_bool: copy_option_bool_route_candidate,
                    option_i64: copy_option_i64_route_candidate,
                    option_f64: copy_option_f64_route_candidate,
                    result_i32: copy_result_i32_route_candidate,
                },
                format!(
                    "nested Option tuple variant {} capability gate failed: {errors:?}",
                    mimi::core::mir::NON_COPY_OPTION_NESTED_TUPLE_VARIANT_ISLAND,
                ),
            );
        }
    }

    if materialized_copy_option_i32_candidate {
        if let Err(errors) = mimi::core::mir::validate_copy_option_i32_variant_island(canonical) {
            return reject_migrated_candidates_with_copy(
                flow_route_candidate,
                collection_route_candidate,
                record_route_candidate,
                option_string_route_candidate,
                CopyCandidateFlags {
                    option_i32: true,
                    option_bool: copy_option_bool_route_candidate,
                    option_i64: copy_option_i64_route_candidate,
                    option_f64: copy_option_f64_route_candidate,
                    result_i32: copy_result_i32_route_candidate,
                },
                format!(
                    "{} capability gate failed: {errors:?}",
                    mimi::core::mir::COPY_OPTION_I32_VARIANT_ISLAND
                ),
            );
        }
    }

    if materialized_copy_option_bool_candidate {
        if let Err(errors) = mimi::core::mir::validate_copy_option_variant_island(
            canonical,
            mimi::core::PrimitiveType::Bool,
            mimi::core::mir::COPY_OPTION_BOOL_VARIANT_ISLAND,
        ) {
            return reject_migrated_candidates_with_copy(
                flow_route_candidate,
                collection_route_candidate,
                record_route_candidate,
                option_string_route_candidate,
                CopyCandidateFlags {
                    option_i32: copy_option_i32_route_candidate,
                    option_bool: true,
                    option_i64: copy_option_i64_route_candidate,
                    option_f64: copy_option_f64_route_candidate,
                    result_i32: copy_result_i32_route_candidate,
                },
                format!(
                    "{} capability gate failed: {errors:?}",
                    mimi::core::mir::COPY_OPTION_BOOL_VARIANT_ISLAND
                ),
            );
        }
    }

    if materialized_copy_option_i64_candidate {
        if let Err(errors) = mimi::core::mir::validate_copy_option_i64_variant_island(canonical) {
            return reject_migrated_candidates_with_copy(
                flow_route_candidate,
                collection_route_candidate,
                record_route_candidate,
                option_string_route_candidate,
                CopyCandidateFlags {
                    option_i32: copy_option_i32_route_candidate,
                    option_bool: copy_option_bool_route_candidate,
                    option_i64: true,
                    option_f64: copy_option_f64_route_candidate,
                    result_i32: copy_result_i32_route_candidate,
                },
                format!(
                    "{} capability gate failed: {errors:?}",
                    mimi::core::mir::COPY_OPTION_I64_VARIANT_ISLAND
                ),
            );
        }
    }

    if materialized_copy_option_f64_candidate {
        if let Err(errors) = mimi::core::mir::validate_copy_option_f64_variant_island(canonical) {
            return reject_migrated_candidates_with_copy(
                flow_route_candidate,
                collection_route_candidate,
                record_route_candidate,
                option_string_route_candidate,
                CopyCandidateFlags {
                    option_i32: copy_option_i32_route_candidate,
                    option_bool: copy_option_bool_route_candidate,
                    option_i64: copy_option_i64_route_candidate,
                    option_f64: true,
                    result_i32: copy_result_i32_route_candidate,
                },
                format!(
                    "{} capability gate failed: {errors:?}",
                    mimi::core::mir::COPY_OPTION_F64_VARIANT_ISLAND
                ),
            );
        }
    }

    if materialized_copy_result_i32_candidate {
        if let Err(errors) = mimi::core::mir::validate_copy_result_i32_variant_island(canonical) {
            return reject_migrated_candidates_with_copy(
                flow_route_candidate,
                collection_route_candidate,
                record_route_candidate,
                option_string_route_candidate,
                CopyCandidateFlags {
                    option_i32: copy_option_i32_route_candidate,
                    option_bool: copy_option_bool_route_candidate,
                    option_i64: copy_option_i64_route_candidate,
                    option_f64: copy_option_f64_route_candidate,
                    result_i32: true,
                },
                format!(
                    "{} capability gate failed: {errors:?}",
                    mimi::core::mir::COPY_RESULT_I32_VARIANT_ISLAND
                ),
            );
        }
    }

    if materialized_managed_result_call_candidate {
        if let Err(errors) = mimi::core::mir::validate_managed_result_call_island(canonical) {
            return DefaultMirRoute::Rejected(format!(
                "{} capability gate failed: {errors:?}",
                mimi::core::mir::MANAGED_RESULT_CALL_ISLAND
            ));
        }
    }

    if materialized_session_candidate {
        if let Err(errors) = mimi::core::mir::validate_session_channel_island(canonical) {
            return DefaultMirRoute::Rejected(format!(
                "{} capability gate failed: {errors:?}",
                mimi::core::mir::SESSION_CHANNEL_ISLAND
            ));
        }
    }

    // The MIR verifier intentionally skips bodies with no contract.  That is
    // not permission for an unsupported instruction to enter a default
    // native/bytecode island.  Scan the complete canonical graph before the
    // verifier's contract pass so every selected consumer has an explicit
    // capability, including no-obligation functions.
    if let Err(error) = mimi::verifier::validate_mir_capabilities(canonical) {
        if materialized_managed_result_call_candidate {
            return DefaultMirRoute::Rejected(format!(
                "managed Result direct-call canonical MIR verifier capability gate failed: {error:?}"
            ));
        }
        if materialized_session_candidate {
            return DefaultMirRoute::Rejected(format!(
                "{} MIR verifier capability gate failed: {error:?}",
                mimi::core::mir::SESSION_CHANNEL_ISLAND
            ));
        }
        if user_record_hint
            && error
                .iter()
                .any(|message| message.contains("ConstructVariantMove"))
        {
            return DefaultMirRoute::Rejected(format!(
                "S0 flat Copy record candidate is not eligible for the default route: verifier capability gate failed: {error:?}"
            ));
        }
        if module_alias_call_hint {
            return DefaultMirRoute::Rejected(format!(
                "resolved module-qualified function graph failed MIR verifier capability gate: {error:?}"
            ));
        }
        return reject_migrated_candidates_with_copy(
            flow_route_candidate,
            collection_route_candidate,
            record_route_candidate,
            option_string_route_candidate,
            CopyCandidateFlags {
                option_i32: copy_option_i32_route_candidate,
                option_bool: copy_option_bool_route_candidate,
                option_i64: copy_option_i64_route_candidate,
                option_f64: copy_option_f64_route_candidate,
                result_i32: copy_result_i32_route_candidate,
            },
            format!("verifier capability gate failed: {error:?}"),
        );
    }

    // A verified library has no process entry to compile or execute. Its
    // complete MIR graph has passed the same profile and capability checks;
    // the actual verifier reports proof failures with source provenance.
    // Execution never takes this branch and still requires main.
    if purpose == RoutePurpose::Verification
        && materialized_collection_candidate
        && !canonical
            .functions()
            .contains_key(&mimi::core::NodeId("function:main".into()))
    {
        return DefaultMirRoute::Canonical(Box::new(route.program));
    }

    // Bytecode and native are both checked only after the verifier capability
    // gate.  The actual consumers repeat their own validation immediately
    // before use.
    let receipt = canonical.route_receipt("canonical-dispatch-v1");
    if let Err(errors) =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(canonical, &receipt)
    {
        if materialized_managed_result_call_candidate {
            return DefaultMirRoute::Rejected(format!(
                "managed Result direct-call MIR-bytecode preflight failed: {errors:?}"
            ));
        }
        if materialized_session_candidate {
            return DefaultMirRoute::Rejected(format!(
                "{} MIR-bytecode preflight failed: {errors:?}",
                mimi::core::mir::SESSION_CHANNEL_ISLAND
            ));
        }
        if module_alias_call_hint {
            return DefaultMirRoute::Rejected(format!(
                "resolved module-qualified function graph failed MIR-bytecode preflight: {errors:?}"
            ));
        }
        return reject_migrated_candidates_with_copy(
            flow_route_candidate,
            collection_route_candidate,
            record_route_candidate,
            option_string_route_candidate,
            CopyCandidateFlags {
                option_i32: copy_option_i32_route_candidate,
                option_bool: copy_option_bool_route_candidate,
                option_i64: copy_option_i64_route_candidate,
                option_f64: copy_option_f64_route_candidate,
                result_i32: copy_result_i32_route_candidate,
            },
            format!("MIR-bytecode preflight failed: {errors:?}"),
        );
    }
    if let Err(errors) = mimi::codegen::mir::validate_mir_native(canonical) {
        if materialized_managed_result_call_candidate {
            return DefaultMirRoute::Rejected(format!(
                "managed Result direct-call native MIR preflight failed: {errors:?}"
            ));
        }
        if materialized_session_candidate {
            return DefaultMirRoute::Rejected(format!(
                "{} native MIR preflight failed: {errors:?}",
                mimi::core::mir::SESSION_CHANNEL_ISLAND
            ));
        }
        if module_alias_call_hint {
            return DefaultMirRoute::Rejected(format!(
                "resolved module-qualified function graph failed native MIR preflight: {errors:?}"
            ));
        }
        return reject_migrated_candidates_with_copy(
            flow_route_candidate,
            collection_route_candidate,
            record_route_candidate,
            option_string_route_candidate,
            CopyCandidateFlags {
                option_i32: copy_option_i32_route_candidate,
                option_bool: copy_option_bool_route_candidate,
                option_i64: copy_option_i64_route_candidate,
                option_f64: copy_option_f64_route_candidate,
                result_i32: copy_result_i32_route_candidate,
            },
            format!("native MIR preflight failed: {errors:?}"),
        );
    }

    // The verifier is a fourth consumer of the same program.  A definitive
    // disproven result is still a valid verifier observation; an unsupported
    // or inconclusive verifier result means this program is not yet a complete
    // default-switch island.
    let verifier_ready = match mimi::verifier::verify_mir_with_route_receipt(
        canonical,
        &receipt,
        String::new(),
    ) {
        Ok(results) => mimi::verifier::canonical_execution_route_verifier_ready(
            &results,
            exact_f64_flow_failure_route_candidate,
            mimi::core::mir::contains_multi_target_flow_union_candidate(canonical),
        ),
        Err(error) => {
            if materialized_managed_result_call_candidate {
                return DefaultMirRoute::Rejected(format!(
                    "managed Result direct-call verifier contract pass failed: {error}"
                ));
            }
            if materialized_session_candidate {
                return DefaultMirRoute::Rejected(format!(
                    "{} verifier contract pass failed: {error}",
                    mimi::core::mir::SESSION_CHANNEL_ISLAND
                ));
            }
            if module_alias_call_hint {
                return DefaultMirRoute::Rejected(format!(
                        "resolved module-qualified function graph failed verifier contract pass: {error}"
                    ));
            }
            return reject_migrated_candidates_with_copy(
                flow_route_candidate,
                collection_route_candidate,
                record_route_candidate,
                option_string_route_candidate,
                CopyCandidateFlags {
                    option_i32: copy_option_i32_route_candidate,
                    option_bool: copy_option_bool_route_candidate,
                    option_i64: copy_option_i64_route_candidate,
                    option_f64: copy_option_f64_route_candidate,
                    result_i32: copy_result_i32_route_candidate,
                },
                format!("verifier contract pass failed: {error}"),
            );
        }
    };
    if !verifier_ready {
        if materialized_managed_result_call_candidate {
            return DefaultMirRoute::Rejected(
                "managed Result direct-call verifier returned an unsupported or inconclusive result"
                    .into(),
            );
        }
        if materialized_session_candidate {
            return DefaultMirRoute::Rejected(format!(
                "{} verifier returned an unsupported or inconclusive result",
                mimi::core::mir::SESSION_CHANNEL_ISLAND
            ));
        }
        if module_alias_call_hint {
            return DefaultMirRoute::Rejected(
                "resolved module-qualified function graph has an unsupported or inconclusive verifier result".into(),
            );
        }
        return reject_migrated_candidates_with_copy(
            flow_route_candidate,
            collection_route_candidate,
            record_route_candidate,
            option_string_route_candidate,
            CopyCandidateFlags {
                option_i32: copy_option_i32_route_candidate,
                option_bool: copy_option_bool_route_candidate,
                option_i64: copy_option_i64_route_candidate,
                option_f64: copy_option_f64_route_candidate,
                result_i32: copy_result_i32_route_candidate,
            },
            "verifier returned an unsupported or inconclusive result",
        );
    }

    DefaultMirRoute::Canonical(Box::new(route.program))
}

/// Selection for the recoverable ordinary Map NewResult slice. Execution
/// binds the exact Checker receipt through the contract-free L1 graph and its
/// Core MIR route receipt; verification binds the same receipt through the
/// isolated L2 verifier projection and its full source-census validation.
/// Any other shape keeps the pending-source rejection so the incomplete
/// surface stays visible instead of disappearing into a compatibility route.
fn select_map_new_result_route(checked: &CheckedProgram, purpose: RoutePurpose) -> DefaultMirRoute {
    match purpose {
        RoutePurpose::Execution => {
            match mimi::core::mir::materialize_ordinary_map_new_result_route(checked) {
                Ok(program) => DefaultMirRoute::Canonical(Box::new(program)),
                Err(reason) => DefaultMirRoute::Rejected(reason),
            }
        }
        RoutePurpose::Verification => {
            match mimi::core::mir::materialize_ordinary_map_new_result_verifier_route(checked) {
                Ok(program) => DefaultMirRoute::Canonical(Box::new(program)),
                Err(reason) => DefaultMirRoute::Rejected(reason),
            }
        }
    }
}

/// Consumer preflight for the closed Checker-owned MapRoot lifecycle. Every
/// consumer receives the same immutable program and the same route receipt;
/// no failure may resume in the legacy Record ABI.
fn select_map_root_route(program: MirProgram) -> DefaultMirRoute {
    if let Err(errors) = mimi::verifier::validate_mir_capabilities(&program) {
        return DefaultMirRoute::Rejected(format!("MapRoot MIR verifier capability: {errors:?}"));
    }
    let receipt = program.route_receipt(mimi::core::mir::MAP_ROOT_ISLAND);
    if let Err(errors) =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&program, &receipt)
    {
        return DefaultMirRoute::Rejected(format!("MapRoot MIR bytecode capability: {errors:?}"));
    }
    if let Err(errors) = mimi::codegen::mir::validate_mir_native(&program) {
        return DefaultMirRoute::Rejected(format!("MapRoot MIR native capability: {errors:?}"));
    }
    match mimi::verifier::verify_mir_with_route_receipt(&program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            DefaultMirRoute::Canonical(Box::new(program))
        }
        Ok(results) => DefaultMirRoute::Rejected(format!(
            "MapRoot MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => DefaultMirRoute::Rejected(format!("MapRoot MIR verifier pass: {error}")),
    }
}

/// Consumer preflight for the versioned typed Any-owner Map lifecycle. Keep
/// every consumer behind the independent v1 receipt so no legacy raw-value
/// MapRoot path can claim its runtime tokens.
fn select_map_any_owner_v1_route(program: MirProgram) -> DefaultMirRoute {
    if let Err(errors) = mimi::verifier::validate_mir_capabilities(&program) {
        return DefaultMirRoute::Rejected(format!(
            "MapAnyOwner v1 MIR verifier capability: {errors:?}"
        ));
    }
    let receipt = program.route_receipt(mimi::core::mir::MAP_ANY_OWNER_V1_ISLAND);
    if let Err(errors) =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&program, &receipt)
    {
        return DefaultMirRoute::Rejected(format!(
            "MapAnyOwner v1 MIR bytecode capability: {errors:?}"
        ));
    }
    if let Err(errors) = mimi::codegen::mir::validate_mir_native(&program) {
        return DefaultMirRoute::Rejected(format!(
            "MapAnyOwner v1 MIR native capability: {errors:?}"
        ));
    }
    match mimi::verifier::verify_mir_with_route_receipt(&program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            DefaultMirRoute::Canonical(Box::new(program))
        }
        Ok(results) => DefaultMirRoute::Rejected(format!(
            "MapAnyOwner v1 MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => DefaultMirRoute::Rejected(format!(
            "MapAnyOwner v1 MIR verifier pass: {error}"
        )),
    }
}

/// Consumer preflight for the source-preserving ordinary Map lifecycle.
/// This profile has its own TypeDesc, operations, runtime storage mode, and
/// receipt; a MapRoot or MapAnyOwner receipt cannot authorize it.
fn select_ordinary_map_owner_lifecycle_i32_string_v1_route(program: MirProgram) -> DefaultMirRoute {
    if let Err(errors) = mimi::verifier::validate_mir_capabilities(&program) {
        return DefaultMirRoute::Rejected(format!(
            "ordinary Map owner lifecycle MIR verifier capability: {errors:?}"
        ));
    }
    let receipt =
        program.route_receipt(mimi::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_I32_STRING_V1_ISLAND);
    if let Err(errors) =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&program, &receipt)
    {
        return DefaultMirRoute::Rejected(format!(
            "ordinary Map owner lifecycle MIR bytecode capability: {errors:?}"
        ));
    }
    if let Err(errors) = mimi::codegen::mir::validate_mir_native(&program) {
        return DefaultMirRoute::Rejected(format!(
            "ordinary Map owner lifecycle MIR native capability: {errors:?}"
        ));
    }
    match mimi::verifier::verify_mir_with_route_receipt(&program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            DefaultMirRoute::Canonical(Box::new(program))
        }
        Ok(results) => DefaultMirRoute::Rejected(format!(
            "ordinary Map owner lifecycle verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => DefaultMirRoute::Rejected(format!(
            "ordinary Map owner lifecycle MIR verifier pass: {error}"
        )),
    }
}

fn ordinary_map_owner_lifecycle_observer_v1_materialization_is_exact(
    route: &mimi::core::mir::CanonicalMirRouteMaterialization,
) -> bool {
    route.materialized_ordinary_map_owner_lifecycle_observer_v1_candidate
        && route
            .ordinary_map_owner_lifecycle_observer_v1_route_receipt
            .as_ref()
            .is_some_and(|receipt| {
                receipt
                    == &route.program.route_receipt(
                        mimi::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE,
                    )
            })
}

/// Revalidate the Checker-derived Observer V1 witnesses and their exact
/// profile receipt across every production consumer. This dedicated profile
/// remains independent of the ordinary V2 readiness bit and has no Legacy
/// retry arm.
fn preflight_ordinary_map_owner_lifecycle_observer_v1(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    let admission = mimi::core::mir::classify_canonical_mir_route_admission(checked);
    if !admission
        .ordinary_map_owner_lifecycle_observer_v1
        .is_complete()
    {
        return Err("Checker Observer V1 admission is not complete".into());
    }
    if let Some(reason) = scalar_ffi_declaration_boundary_diagnostic(checked) {
        return Err(reason);
    }
    if let Some(reason) = mimi::core::mir::unknown_effect_boundary_reason(checked) {
        return Err(format!(
            "{}: {reason}",
            mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    let validated_observers =
        mimi::core::mir::validate_ordinary_map_owner_lifecycle_observer_v1_mir_route(program)?;
    let mir_observers = program.ordinary_map_owner_lifecycle_observer_v1_receipts()?;
    if validated_observers.is_empty() || validated_observers != mir_observers {
        return Err(
            "independent Observer V1 validation disagrees with the MIR-derived receipt census"
                .into(),
        );
    }
    let profile = mimi::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE;
    let receipt = program.route_receipt(profile);
    if receipt.profile != profile {
        return Err("Observer V1 route receipt has the wrong exact profile".into());
    }
    receipt.validate_against_program(program)?;
    mimi::verifier::validate_mir_capabilities(program)
        .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_route_receipt(program, &receipt)
        .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_mir_native(program)
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    match mimi::verifier::verify_mir_with_route_receipt(program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

fn select_ordinary_map_owner_lifecycle_observer_v1_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owner_lifecycle_observer_v1(checked, &program) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map owner Observer V1 preflight failed: {reason}"
        )),
    }
}

/// Preflight the independently receipted direct `println(pair.1)` profile
/// across all consumers without opening broad owner-v2 production readiness.
fn preflight_ordinary_map_owned_any_v2_direct_print(program: &MirProgram) -> Result<(), String> {
    let receipt =
        program.route_receipt(mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE);
    receipt.validate_against_program(program)?;
    mimi::verifier::validate_mir_capabilities_with_direct_print_route_receipt(program, &receipt)
        .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_route_receipt(program, &receipt)
        .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_mir_native(program)
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    match mimi::verifier::verify_mir_with_route_receipt(program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

/// Select the narrow DirectPrint route only after the exact Checker-derived
/// print receipt and every backend/verifier capability have passed. This
/// identity is independent from broad V2 production readiness, and any
/// failure is terminal for both default and explicit MIR selection.
fn select_ordinary_map_owned_any_v2_direct_print_route(program: MirProgram) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_direct_print(&program) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 DirectPrint preflight failed: {reason}"
        )),
    }
}

/// Consumer preflight for the concrete-record ordinary Map/Any owner-v2
/// profile. The profile owns its TypeDesc and receipt; no older MapRoot or
/// MapAnyOwner adapter may claim the same graph after a consumer failure.
fn select_ordinary_map_owned_any_v2_route(program: MirProgram) -> DefaultMirRoute {
    // This remains a scoped development profile: GetAny accepts only an
    // exact source-known String key (including known literal concatenations),
    // requires a receipted Set on the same root in the same function, and
    // permits only direct explicit-drop settlement. Runtime-symbolic keys
    // remain outside the trusted subset.
    // Values has a real List<DynamicAny> MIR layout and consumer slices. A
    // directly bound snapshot with receipted indexed unpack and exact snapshot
    // and result-owner settlement can reach CompleteCoverage for verifier
    // diagnostics; other populated Values uses remain staged. A direct
    // map_new() root with exact GetAny/Values result Drops and a later explicit
    // root Drop is also a narrow complete diagnostic slice. Cross-function
    // roots, general payload/key coverage, automatic cleanup, and complete
    // L1/L2/L3 evidence remain open. Runtime symbols and partial consumers do
    // not imply production readiness.
    if let Err(errors) = mimi::core::mir::validate_ordinary_map_owned_any_v2_island(&program) {
        return DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 whole-program island: {errors:?}"
        ));
    }
    if !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY {
        return DefaultMirRoute::Rejected(
            "ordinary Map/Any owner-v2 route is not production-ready: GetAny/Values settlement, empty and cross-function roots, payload/key coverage, automatic cleanup, and end-to-end L1/L2/L3 evidence remain incomplete".into(),
        );
    }
    if let Err(errors) = mimi::verifier::validate_mir_capabilities(&program) {
        return DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 MIR verifier capability: {errors:?}"
        ));
    }
    let receipt = program.route_receipt(mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
    if let Err(errors) =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&program, &receipt)
    {
        return DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 MIR bytecode capability: {errors:?}"
        ));
    }
    if let Err(errors) = mimi::codegen::mir::validate_mir_native(&program) {
        return DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 MIR native capability: {errors:?}"
        ));
    }
    match mimi::verifier::verify_mir_with_route_receipt(&program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            DefaultMirRoute::Canonical(Box::new(program))
        }
        Ok(results) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 MIR verifier pass: {error}"
        )),
    }
}

/// Preflight the independent CheckedGet profile across every consumer before
/// either the default dispatcher or explicit `--mir` can select its graph.
/// The route receipt is profile-specific and cannot inherit the broader V2
/// island's CompleteCoverage state.
pub(crate) fn preflight_ordinary_map_owned_any_v2_checked_get(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    let receipt = mimi::core::mir::validate_ordinary_map_owned_any_v2_checked_get_source_route(
        checked, program,
    )?;
    receipt.validate_against_program(program)?;
    mimi::verifier::validate_mir_capabilities(program)
        .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;
    let bytecode =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(program, &receipt)
            .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    if bytecode.ast.is_some() {
        return Err("CheckedGet bytecode capability unexpectedly retained an AST".into());
    }
    mimi::codegen::mir::validate_mir_native_checked_get_route(program, &receipt)
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    match mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_checked_get_route(
        checked,
        program,
        &receipt,
        String::new(),
    ) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

/// Require the IsEmpty admission's own materialization bit and digest-bound
/// route receipt. The broad owner-v2 receipt cannot authorize this island.
fn ordinary_map_owned_any_v2_record_unit_materialization_is_exact(
    route: &mimi::core::mir::CanonicalMirRouteMaterialization,
) -> bool {
    route.admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::RecordUnit
        && route.materialized_ordinary_map_owned_any_v2_record_unit_candidate
        && route
            .ordinary_map_owned_any_v2_record_unit_route_receipt
            .as_ref()
            .is_some_and(|receipt| {
                receipt
                    == &route.program.route_receipt(
                        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE,
                    )
            })
}

fn ordinary_map_owned_any_v2_is_empty_materialization_is_exact(
    route: &mimi::core::mir::CanonicalMirRouteMaterialization,
) -> bool {
    route.materialized_ordinary_map_owned_any_v2_is_empty_candidate
        && route
            .ordinary_map_owned_any_v2_is_empty_route_receipt
            .as_ref()
            .is_some_and(|receipt| {
                receipt
                    == &route
                        .program
                        .route_receipt(mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE)
            })
}

/// Require the no-query control's own materialization bit and digest-bound
/// receipt. It cannot borrow IsEmpty or broad owner-v2 authorization.
fn ordinary_map_owned_any_v2_map_set_drop_control_materialization_is_exact(
    route: &mimi::core::mir::CanonicalMirRouteMaterialization,
) -> bool {
    route.admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControl
        && route.materialized_ordinary_map_owned_any_v2_map_set_drop_control_candidate
        && route
            .ordinary_map_owned_any_v2_map_set_drop_control_route_receipt
            .as_ref()
            .is_some_and(|receipt| {
                receipt
                    == &route.program.route_receipt(
                        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE,
                    )
            })
}

/// Preflight the independent no-query control through the Checker, verifier,
/// bytecode, and native consumers while broad V2 readiness remains closed.
pub(crate) fn preflight_ordinary_map_owned_any_v2_map_set_drop_control(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    mimi::core::mir::validate_ordinary_map_owned_any_v2_map_set_drop_control_route(
        checked, program,
    )?;
    let receipt = program
        .route_receipt(mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE);
    receipt.validate_against_program(program)?;
    mimi::verifier::validate_mir_capabilities(program)
        .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_map_set_drop_control_route(
        checked, program,
    )
    .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_mir_native(program)
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    match mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_map_set_drop_control_route(
        checked,
        program,
        &receipt,
        String::new(),
    ) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

/// Require the branch-local Set diamond's independent materialization bit
/// and receipt. This receipt cannot borrow the no-query control or broad
/// owner-v2 authority.
fn ordinary_map_owned_any_v2_branch_local_set_diamond_materialization_is_exact(
    route: &mimi::core::mir::CanonicalMirRouteMaterialization,
) -> bool {
    route
        .admission
        .ordinary_map_owned_any_v2_branch_local_set_diamond()
        && route.materialized_ordinary_map_owned_any_v2_branch_local_set_diamond_candidate
        && route
            .ordinary_map_owned_any_v2_branch_local_set_diamond_route_receipt
            .as_ref()
            .is_some_and(|receipt| {
                receipt
                    == &route.program.route_receipt(
                        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE,
                    )
            })
}

/// Preflight the independent Checker-sealed branch-local Set diamond through
/// bytecode, native MIR, and the Checker-aware verifier while broad owner-v2
/// readiness remains closed.
pub(crate) fn preflight_ordinary_map_owned_any_v2_branch_local_set_diamond(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    mimi::core::mir::validate_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
        checked, program,
    )?;
    let receipt = program
        .route_receipt(mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE);
    receipt.validate_against_program(program)?;
    mimi::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
        checked,
        program,
        &receipt,
    )
    .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
        checked,
        program,
    )
    .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_mir_native(program)
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    match mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
        checked,
        program,
        &receipt,
        String::new(),
    ) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

/// Require the ZeroMissI32 admission's own materialization bit and the exact
/// digest-bound receipt identity. A generic owner-v2 receipt cannot authorize
/// the fresh owned canonical i32 zero produced by this miss profile.
fn ordinary_map_owned_any_v2_zero_miss_i32_materialization_is_exact(
    route: &mimi::core::mir::CanonicalMirRouteMaterialization,
) -> bool {
    route.admission.ordinary_map_owned_any_v2_zero_miss_i32()
        && route.materialized_ordinary_map_owned_any_v2_zero_miss_i32_candidate
        && route
            .ordinary_map_owned_any_v2_zero_miss_i32_route_receipt
            .as_ref()
            .is_some_and(|receipt| {
                receipt
                    == &route.program.route_receipt(
                        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE,
                    )
            })
}

/// Preflight the independent Checker-sealed ZeroMissI32 profile across all
/// execution and verification consumers before default dispatch, explicit
/// `--mir`, or native build can select the graph.
pub(crate) fn preflight_ordinary_map_owned_any_v2_zero_miss_i32(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    mimi::core::mir::validate_ordinary_map_owned_any_v2_zero_miss_i32_route(checked, program)?;
    mimi::core::mir::validate_ordinary_map_owned_any_v2_zero_miss_i32_mir_route(program)?;
    let receipt =
        program.route_receipt(mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE);
    receipt.validate_against_program(program)?;
    mimi::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
        checked, program, &receipt,
    )
    .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
        checked, program,
    )
    .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_mir_native(program)
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    match mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
        checked,
        program,
        &receipt,
        String::new(),
    ) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

fn select_ordinary_map_owned_any_v2_zero_miss_i32_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_zero_miss_i32(checked, &program) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 ZeroMissI32 preflight failed: {reason}"
        )),
    }
}

/// Preflight the complete Checker/MIR bridge for one canonical
/// `std::maps.get_or_default<String>` callsite. Each execution consumer
/// receives the same profile-specific route receipt; no broad owner-v2
/// readiness bit or generic helper body can authorize this operation.
pub(crate) fn preflight_ordinary_map_owned_any_v2_get_or_default_string(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<mimi::core::mir::CanonicalMirRouteReceipt, String> {
    let receipt = mimi::core::mir::validate_ordinary_map_owned_any_v2_get_or_default_string_route(
        checked, program,
    )?;
    if receipt.profile != mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_PROFILE
        || receipt
            != program.route_receipt(
                mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_PROFILE,
            )
    {
        return Err(
            "GetOrDefault<String> route receipt is not the exact profile receipt for this MIR graph"
                .into(),
        );
    }
    receipt.validate_against_program(program)?;

    mimi::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_get_or_default_string_route(
        checked, program, &receipt,
    )
    .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;

    mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_get_or_default_string_route(
        checked,
        program,
        &receipt,
    )
    .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;

    mimi::codegen::mir::validate_checked_mir_native_get_or_default_string_route(
        checked, program, &receipt,
    )
    .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    let context = inkwell::context::Context::create();
    let mut generator = mimi::codegen::CodeGenerator::new(
        &context,
        "canonical_ordinary_map_owned_any_v2_get_or_default_string",
    );
    generator
        .compile_checked_mir_native_with_ordinary_map_owned_any_v2_get_or_default_string_route(
            checked, program, &receipt,
        )
        .map_err(|errors| format!("MIR native emission: {errors:?}"))?;

    match mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_get_or_default_string_route(
        checked,
        program,
        &receipt,
        String::new(),
    ) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(receipt)
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

/// Preflight the complete Checker/MIR bridge for one canonical
/// `std::maps.get_or_default<i32>` callsite. Each execution consumer receives
/// the same profile-specific route receipt; no broad owner-v2 readiness bit
/// or generic helper body can authorize this operation.
pub(crate) fn preflight_ordinary_map_owned_any_v2_get_or_default_i32(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<mimi::core::mir::CanonicalMirRouteReceipt, String> {
    let receipt = mimi::core::mir::validate_ordinary_map_owned_any_v2_get_or_default_i32_route(
        checked, program,
    )?;
    receipt.validate_against_program(program)?;

    mimi::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_get_or_default_i32_route(
        checked, program, &receipt,
    )
    .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;

    mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_get_or_default_i32_route(
        checked,
        program,
        &receipt,
    )
    .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;

    mimi::codegen::mir::validate_checked_mir_native_get_or_default_i32_route(
        checked, program, &receipt,
    )
    .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    let context = inkwell::context::Context::create();
    let mut generator = mimi::codegen::CodeGenerator::new(
        &context,
        "canonical_ordinary_map_owned_any_v2_get_or_default_i32",
    );
    generator
        .compile_checked_mir_native_with_ordinary_map_owned_any_v2_get_or_default_i32_route(
            checked, program, &receipt,
        )
        .map_err(|errors| format!("MIR native emission: {errors:?}"))?;

    match mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_get_or_default_i32_route(
        checked,
        program,
        &receipt,
        String::new(),
    ) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(receipt)
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

fn select_ordinary_map_owned_any_v2_get_or_default_string_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_get_or_default_string(checked, &program) {
        Ok(_) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 GetOrDefault<String> preflight failed: {reason}"
        )),
    }
}

fn select_ordinary_map_owned_any_v2_get_or_default_i32_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_get_or_default_i32(checked, &program) {
        Ok(_) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 GetOrDefault<i32> preflight failed: {reason}"
        )),
    }
}

/// Require RemovedKeyCheckedMiss's own materialization bit and independent
/// route receipt; the neighboring ZeroMissI32 receipt cannot authorize it.
fn ordinary_map_owned_any_v2_removed_key_checked_miss_materialization_is_exact(
    route: &mimi::core::mir::CanonicalMirRouteMaterialization,
) -> bool {
    route
        .admission
        .ordinary_map_owned_any_v2_removed_key_checked_miss()
        && route.materialized_ordinary_map_owned_any_v2_removed_key_checked_miss_candidate
        && route
            .ordinary_map_owned_any_v2_removed_key_checked_miss_route_receipt
            .as_ref()
            .is_some_and(|receipt| {
                receipt
                    == &route.program.route_receipt(
                        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE,
                    )
            })
}

/// Preflight the exact persistent-remove miss across the source/MIR route,
/// verifier capability gate, AST-free Bytecode adapter, native adapter, and
/// Checker-aware verifier before default dispatch or explicit `--mir` can
/// select the graph.
pub(crate) fn preflight_ordinary_map_owned_any_v2_removed_key_checked_miss(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    mimi::core::mir::validate_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
        checked, program,
    )?;
    mimi::core::mir::validate_ordinary_map_owned_any_v2_removed_key_checked_miss_mir_route(
        program,
    )?;
    let receipt = program
        .route_receipt(mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE);
    receipt.validate_against_program(program)?;
    mimi::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
        checked,
        program,
        &receipt,
    )
    .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
        checked,
        program,
    )
    .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    let context = inkwell::context::Context::create();
    let mut generator = mimi::codegen::CodeGenerator::new(
        &context,
        "canonical_ordinary_map_owned_any_v2_removed_key_checked_miss",
    );
    generator
        .compile_checked_mir_native_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
            checked, program, &receipt,
        )
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    match mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
        checked,
        program,
        &receipt,
        String::new(),
    ) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

fn select_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_removed_key_checked_miss(checked, &program) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 RemovedKeyCheckedMiss preflight failed: {reason}"
        )),
    }
}

/// Require the CloneIsolation admission's own materialization bit and
/// digest-bound route receipt. The broad owner-v2 receipt cannot authorize
/// this island.
fn ordinary_map_owned_any_v2_clone_isolation_materialization_is_exact(
    route: &mimi::core::mir::CanonicalMirRouteMaterialization,
) -> bool {
    route.materialized_ordinary_map_owned_any_v2_clone_isolation_candidate
        && route
            .ordinary_map_owned_any_v2_clone_isolation_route_receipt
            .as_ref()
            .is_some_and(|receipt| {
                receipt
                    == &route.program.route_receipt(
                        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE,
                    )
            })
}

/// Require the exact source-bound persistent-root alias/replacement receipt.
/// This profile stays independent of broad V2 readiness and cannot borrow
/// CloneIsolation or another Map/Any receipt.
fn ordinary_map_owned_any_v2_persistent_root_alias_replacement_materialization_is_exact(
    route: &mimi::core::mir::CanonicalMirRouteMaterialization,
) -> bool {
    route.materialized_ordinary_map_owned_any_v2_persistent_root_alias_replacement_candidate
        && route
            .ordinary_map_owned_any_v2_persistent_root_alias_replacement_route_receipt
            .as_ref()
            .is_some_and(|receipt| {
                receipt
                    == &route.program.route_receipt(
                        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE,
                    )
            })
}

/// Preflight the exact Checker-sealed persistent-root alias/replacement
/// route across bytecode, native capability, and verifier consumers. Each
/// consumer receives the same canonical graph and exact profile receipt.
pub(crate) fn preflight_ordinary_map_owned_any_v2_persistent_root_alias_replacement(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    let profile =
        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE;
    mimi::core::mir::validate_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route(
        checked, program,
    )?;
    let receipt = program.route_receipt(profile);
    receipt.validate_against_program(program)?;
    mimi::verifier::validate_mir_capabilities(program)
        .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route(
        checked, program,
    )
    .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_mir_native(program)
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    let results =
        mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route(
            checked,
            program,
            &receipt,
            String::new(),
        )?;
    if !mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) {
        return Err(format!(
            "MIR PersistentRootAliasReplacement verifier did not produce a conclusive route proof: {results:?}"
        ));
    }
    Ok(())
}

/// Preflight the independent Checker-sealed CloneIsolation profile across
/// every consumer before default dispatch or explicit `--mir` can select its
/// graph. The dedicated route receipt remains independent from broad V2
/// readiness.
pub(crate) fn preflight_ordinary_map_owned_any_v2_clone_isolation(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    mimi::core::mir::validate_ordinary_map_owned_any_v2_clone_isolation_route(checked, program)?;
    let receipt =
        program.route_receipt(mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE);
    receipt.validate_against_program(program)?;
    mimi::verifier::validate_mir_capabilities(program)
        .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_clone_isolation_route(
        checked, program,
    )
    .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_mir_native(program)
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    match mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_clone_isolation_route(
        checked,
        program,
        &receipt,
        String::new(),
    ) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

/// Preflight the Checker-sealed IsEmpty profile across every consumer before
/// default dispatch or explicit `--mir` can select its graph. The dedicated
/// route receipt is required and remains independent from broad V2 readiness.
pub(crate) fn preflight_ordinary_map_owned_any_v2_is_empty(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    mimi::core::mir::validate_ordinary_map_owned_any_v2_is_empty_route(checked, program)?;
    let receipt =
        program.route_receipt(mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE);
    receipt.validate_against_program(program)?;
    mimi::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_is_empty_route(
        checked, program, &receipt,
    )
    .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_is_empty_route(
        checked, program,
    )
    .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_mir_native(program)
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    match mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_is_empty_route(
        checked,
        program,
        &receipt,
        String::new(),
    ) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

/// Preflight the exact mixed i32/Label/i32 RecordUnit profile through every
/// MIR consumer before default or explicit `--mir` routing selects its graph.
pub(crate) fn preflight_ordinary_map_owned_any_v2_record_unit(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    mimi::core::mir::validate_ordinary_map_owned_any_v2_record_unit_route(checked, program)?;
    let receipt =
        program.route_receipt(mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE);
    receipt.validate_against_program(program)?;
    mimi::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_record_unit_route(
        checked, program, &receipt,
    )
    .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_record_unit_route(
        checked, program,
    )
    .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_mir_native(program)
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    match mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_record_unit_route(
        checked,
        program,
        &receipt,
        String::new(),
    ) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

fn select_ordinary_map_owned_any_v2_record_unit_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_record_unit(checked, &program) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 RecordUnit preflight failed: {reason}"
        )),
    }
}

fn select_ordinary_map_owned_any_v2_is_empty_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_is_empty(checked, &program) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 IsEmpty preflight failed: {reason}"
        )),
    }
}

/// Preflight the independent Checker-sealed computed-key GetAny profile.
/// Dynamic keys deliberately have no static-key digest, so authorization
/// requires the exact CheckedProgram/MIR pair before any consumer is selected.
fn preflight_ordinary_map_owned_any_v2_computed_key_get(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    mimi::core::mir::validate_ordinary_map_owned_any_v2_computed_key_get_route(checked, program)?;
    let receipt =
        program.route_receipt(mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE);
    receipt.validate_against_program(program)?;
    mimi::verifier::validate_mir_capabilities(program)
        .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_computed_key_get_route(checked, program)
        .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_mir_native(program)
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    match mimi::verifier::verify_mir_with_route_receipt(program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

fn preflight_ordinary_map_owned_any_v2_computed_string_key_owner_chain(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    mimi::core::mir::validate_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route(
        checked, program,
    )?;
    let receipt = program.route_receipt(
        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE,
    );
    let results = mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route(
        checked, program, &receipt, String::new(),
    )
    .map_err(|error| format!("MIR verifier route: {error}"))?;
    if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) {
        Ok(())
    } else {
        Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        ))
    }
}

fn select_ordinary_map_owned_any_v2_computed_key_get_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_computed_key_get(checked, &program) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 ComputedKeyGet preflight failed: {reason}"
        )),
    }
}

fn select_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_computed_string_key_owner_chain(checked, &program) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 ComputedStringKeyOwnerChain preflight failed: {reason}"
        )),
    }
}

fn select_ordinary_map_owned_any_v2_checked_get_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_checked_get(checked, &program) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 CheckedGet preflight failed: {reason}"
        )),
    }
}

/// Preflight the separately versioned literal-index-one Values cross-call
/// route through its own Checker, MIR, bytecode, native, and verifier gates.
/// The route receipt shares a Rust type with V1, so this gate pins the V2
/// profile and revalidates semantic identity before any consumer can use it.
pub(crate) fn preflight_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    let profile = mimi::core::mir::CanonicalMirRouteProfile::
        OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1
        .as_str();
    let receipt =
        mimi::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
            checked, program,
        )?;
    if receipt.schema != mimi::core::mir::MIR_ROUTE_RECEIPT_SCHEMA || receipt.profile != profile {
        return Err(format!(
            "exact ValuesCrosscallStringCallerIndex1 route receipt schema/profile required ('{}', '{profile}'), got ('{}', '{}')",
            mimi::core::mir::MIR_ROUTE_RECEIPT_SCHEMA,
            receipt.schema,
            receipt.profile
        ));
    }
    receipt.validate_against_program(program)?;
    let program_receipt = program.route_receipt(profile);
    if !program_receipt.same_semantic_identity(&receipt) {
        return Err(
            "ValuesCrosscallStringCallerIndex1 Checker/MIR route receipt differs from the canonical MIR profile receipt"
                .into(),
        );
    }
    mimi::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_mir_route(
        program,
    )
    .map_err(|reason| {
        format!(
            "MIR ValuesCrosscallStringCallerIndex1 operation/ownership receipt: {reason}"
        )
    })?;
    mimi::interp::bytecode::compile_mir_program_with_values_crosscall_string_caller_index1_route(
        checked, program,
    )
    .map_err(|errors| {
        format!("MIR ValuesCrosscallStringCallerIndex1 bytecode capability: {errors:?}")
    })?;
    mimi::codegen::mir::validate_checked_mir_native_values_crosscall_string_caller_index1_route(
        checked, program, &receipt,
    )
    .map_err(|errors| {
        format!("MIR ValuesCrosscallStringCallerIndex1 native capability: {errors:?}")
    })?;
    mimi::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
        checked, program, &receipt,
    )
    .map_err(|errors| {
        format!(
            "MIR verifier ValuesCrosscallStringCallerIndex1 capability: {errors:?}"
        )
    })?;
    let results = mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
        checked,
        program,
        &receipt,
        String::new(),
    )?;
    if !mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) {
        return Err(format!(
            "MIR ValuesCrosscallStringCallerIndex1 verifier returned an unsupported or inconclusive result: {results:?}"
        ));
    }
    Ok(())
}

fn select_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1(
        checked, &program,
    ) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 ValuesCrosscallStringCallerIndex1 preflight failed: {reason}"
        )),
    }
}

/// Preflight the exact cross-call Values snapshot and String-unpack route
/// through every canonical consumer. The route carries independent Checker,
/// MIR operation/ownership, bytecode, native, and verifier receipts.
pub(crate) fn preflight_ordinary_map_owned_any_v2_values_crosscall_string_caller(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    let profile =
        mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller
            .as_str();
    let receipt =
        mimi::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(
            checked, program,
        )?;
    if receipt.profile != profile {
        return Err(format!(
            "exact ValuesCrosscallStringCaller profile '{profile}' required, got '{}'",
            receipt.profile
        ));
    }
    receipt.validate_against_program(program)?;
    let program_receipt = program.route_receipt(profile);
    if !program_receipt.same_semantic_identity(&receipt) {
        return Err(
            "ValuesCrosscallStringCaller Checker/MIR route receipt differs from the canonical MIR profile receipt"
                .into(),
        );
    }
    mimi::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_mir_route(
        program,
    )
    .map_err(|reason| {
        format!("MIR ValuesCrosscallStringCaller operation/ownership receipt: {reason}")
    })?;
    mimi::interp::bytecode::compile_mir_program_with_values_crosscall_string_caller_route(
        checked, program,
    )
    .map_err(|errors| format!("MIR ValuesCrosscallStringCaller bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_checked_mir_native_values_crosscall_string_caller_route(
        checked, program, &receipt,
    )
    .map_err(|errors| format!("MIR ValuesCrosscallStringCaller native capability: {errors:?}"))?;
    mimi::verifier::
        validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(
            checked, program, &receipt,
        )
        .map_err(|errors| {
            format!("MIR verifier ValuesCrosscallStringCaller capability: {errors:?}")
        })?;
    let results =
        mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(
            checked,
            program,
            &receipt,
            String::new(),
        )?;
    if !mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) {
        return Err(format!(
            "MIR ValuesCrosscallStringCaller verifier returned an unsupported or inconclusive result: {results:?}"
        ));
    }
    Ok(())
}

fn select_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_values_crosscall_string_caller(checked, &program) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 ValuesCrosscallStringCaller preflight failed: {reason}"
        )),
    }
}

/// Preflight the Checker-bound Values producer→caller owner transfer through
/// each independent canonical consumer. This exact profile has its own route
/// receipt, bytecode anchor, native gate, and verifier entry point; generic
/// owner-v2 readiness and generic receipt compilation cannot authorize it.
pub(crate) fn preflight_ordinary_map_owned_any_v2_values_producer_caller(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    let profile =
        mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesProducerCaller
            .as_str();
    let receipt = mimi::core::mir::validate_ordinary_map_owned_any_v2_values_producer_caller_route(
        checked, program,
    )?;
    if receipt.profile != profile {
        return Err(format!(
            "exact ValuesProducerCaller profile '{profile}' required, got '{}'",
            receipt.profile
        ));
    }
    receipt.validate_against_program(program)?;
    let program_receipt = program.route_receipt(profile);
    if !program_receipt.same_semantic_identity(&receipt) {
        return Err(
            "ValuesProducerCaller Checker/MIR route receipt differs from the canonical MIR profile receipt"
                .into(),
        );
    }
    let operation_receipts =
        mimi::core::mir::validate_ordinary_map_owned_any_v2_values_producer_caller_mir_route(
            program,
        )?;
    if operation_receipts.is_empty() {
        return Err("ValuesProducerCaller MIR operation receipt set is empty".into());
    }
    mimi::interp::bytecode::compile_mir_program_with_values_producer_caller_route(checked, program)
        .map_err(|errors| format!("MIR ValuesProducerCaller bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_checked_mir_native_values_producer_caller_route(
        checked, program, &receipt,
    )
    .map_err(|errors| format!("MIR ValuesProducerCaller native capability: {errors:?}"))?;
    let results =
        mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_values_producer_caller_route(
            checked,
            program,
            &receipt,
            String::new(),
        )?;
    if !mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) {
        return Err(format!(
            "MIR ValuesProducerCaller verifier returned an unsupported or inconclusive result: {results:?}"
        ));
    }
    Ok(())
}

fn select_ordinary_map_owned_any_v2_values_producer_caller_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_values_producer_caller(checked, &program) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 ValuesProducerCaller preflight failed: {reason}"
        )),
    }
}

/// Preflight the exact Map-root producer-return/unique-caller owner transfer
/// through every production consumer. The dedicated route receipt is checked
/// at each boundary; broad Map owner readiness stays closed.
pub(crate) fn preflight_ordinary_map_owned_any_v2_map_root_producer_caller(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    let profile =
        mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootProducerCaller
            .as_str();
    let receipt =
        mimi::core::mir::validate_ordinary_map_owned_any_v2_map_root_producer_caller_route(
            checked, program,
        )?;
    if receipt.profile != profile {
        return Err(format!(
            "exact MapRootProducerCaller profile '{profile}' required, got '{}'",
            receipt.profile
        ));
    }
    receipt.validate_against_program(program)?;
    let program_receipt = program.route_receipt(profile);
    if !program_receipt.same_semantic_identity(&receipt) {
        return Err(
            "MapRootProducerCaller Checker/MIR route receipt differs from the canonical MIR profile receipt"
                .into(),
        );
    }
    let operation_receipts =
        mimi::core::mir::validate_ordinary_map_owned_any_v2_map_root_producer_caller_mir_route(
            program,
        )?;
    if operation_receipts.len() != 1 {
        return Err("MapRootProducerCaller MIR operation receipt set is not singular".into());
    }
    mimi::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_producer_caller_route(
        checked, program, &receipt,
    )
    .map_err(|errors| format!("MIR verifier MapRootProducerCaller capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_map_root_producer_caller_route(
        checked, program,
    )
    .map_err(|errors| format!("MIR MapRootProducerCaller bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_checked_mir_native_map_root_producer_caller_route(
        checked, program, &receipt,
    )
    .map_err(|errors| format!("MIR MapRootProducerCaller native capability: {errors:?}"))?;
    let results =
        mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_map_root_producer_caller_route(
            checked,
            program,
            &receipt,
            String::new(),
        )?;
    if !mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) {
        return Err(format!(
            "MIR MapRootProducerCaller verifier did not produce a conclusive route proof: {results:?}"
        ));
    }
    Ok(())
}

fn select_ordinary_map_owned_any_v2_map_root_producer_caller_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_map_root_producer_caller(checked, &program) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 MapRootProducerCaller preflight failed: {reason}"
        )),
    }
}

/// Preflight the exact Map-root producer/direct-GetAny/String-unpack caller
/// through every production consumer. Its route identity and consumer
/// validators are independent of the adjacent root producer/caller profile.
pub(crate) fn preflight_ordinary_map_owned_any_v2_map_root_get_any_caller(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    let profile =
        mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootGetAnyCaller
            .as_str();
    let receipt =
        mimi::core::mir::validate_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
            checked, program,
        )?;
    if receipt.profile != profile {
        return Err(format!(
            "exact MapRootGetAnyCaller profile '{profile}' required, got '{}'",
            receipt.profile
        ));
    }
    receipt.validate_against_program(program)?;
    let program_receipt = program.route_receipt(profile);
    if !program_receipt.same_semantic_identity(&receipt) {
        return Err(
            "MapRootGetAnyCaller Checker/MIR route receipt differs from the canonical MIR profile receipt"
                .into(),
        );
    }
    let operation_receipts =
        mimi::core::mir::validate_ordinary_map_owned_any_v2_map_root_get_any_caller_mir_route(
            program,
        )?;
    if operation_receipts.len() != 1 {
        return Err("MapRootGetAnyCaller MIR operation receipt set is not singular".into());
    }
    mimi::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
        checked, program, &receipt,
    )
    .map_err(|errors| format!("MIR verifier MapRootGetAnyCaller capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_map_root_get_any_caller_route(
        checked, program,
    )
    .map_err(|errors| format!("MIR MapRootGetAnyCaller bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_checked_mir_native_map_root_get_any_caller_route(
        checked, program, &receipt,
    )
    .map_err(|errors| format!("MIR MapRootGetAnyCaller native capability: {errors:?}"))?;
    let results =
        mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
            checked,
            program,
            String::new(),
            &receipt,
        )?;
    if !mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) {
        return Err(format!(
            "MIR MapRootGetAnyCaller verifier did not produce a conclusive route proof: {results:?}"
        ));
    }
    Ok(())
}

fn select_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_map_root_get_any_caller(checked, &program) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 MapRootGetAnyCaller preflight failed: {reason}"
        )),
    }
}

/// Consumer preflight for the exact ValuesFieldMove profile. This route is
/// independent of broad owner-v2 readiness and must carry its own receipt at
/// every consumer boundary.
fn preflight_ordinary_map_owned_any_v2_values_field_move(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    mimi::core::mir::validate_ordinary_map_owned_any_v2_values_field_move_route(checked, program)?;
    let receipt =
        program.route_receipt(mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE);
    receipt.validate_against_program(program)?;
    mimi::verifier::validate_mir_capabilities(program)
        .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_values_field_move_route(checked, program)
        .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_mir_native(program)
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    match mimi::verifier::verify_mir_with_route_receipt(program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

fn select_ordinary_map_owned_any_v2_values_field_move_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_values_field_move(checked, &program) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 ValuesFieldMove preflight failed: {reason}"
        )),
    }
}

/// Consumer preflight for the exact immutable-local i32 Values index. This
/// profile carries its own Checker/MIR receipt and stays independent from the
/// broad owner-v2 readiness flag.
fn preflight_ordinary_map_owned_any_v2_values_local_index_i32(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    let receipt = mimi::core::mir::validate_ordinary_map_owned_any_v2_values_local_index_i32_route(
        checked, program,
    )?;
    if receipt.profile != mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_PROFILE
    {
        return Err(format!(
            "ValuesLocalIndexI32 validator returned unexpected profile {}",
            receipt.profile
        ));
    }
    receipt.validate_against_program(program)?;
    mimi::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_values_local_index_i32_route(
        checked, program, &receipt,
    )
    .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_values_local_index_i32_route(checked, program)
        .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_mir_native(program)
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    match mimi::verifier::verify_mir_with_route_receipt(program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

fn select_ordinary_map_owned_any_v2_values_local_index_i32_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_values_local_index_i32(checked, &program) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 ValuesLocalIndexI32 preflight failed: {reason}"
        )),
    }
}

/// Consumer preflight for the two-entry heterogeneous Values String
/// extraction. Core, bytecode, native and verifier consumers receive the
/// same dedicated route identity before default dispatch can select it.
fn preflight_ordinary_map_owned_any_v2_values_heterogeneous_string_index1(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    let receipt = mimi::core::mir::validate_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route(
        checked,
        program,
    )?;
    if receipt.profile
        != mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_HETEROGENEOUS_STRING_INDEX1_PROFILE
    {
        return Err(format!(
            "ValuesHeterogeneousStringIndex1 validator returned unexpected profile {}",
            receipt.profile
        ));
    }
    receipt.validate_against_program(program)?;
    mimi::interp::bytecode::compile_mir_program_with_values_heterogeneous_string_index1_route(
        checked, program,
    )
    .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_mir_native(program)
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    match mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route(
        checked,
        program,
        &receipt,
        String::new(),
    ) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

fn select_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_values_heterogeneous_string_index1(checked, &program)
    {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 ValuesHeterogeneousStringIndex1 preflight failed: {reason}"
        )),
    }
}

fn ordinary_map_owned_any_v2_keys_snapshot_materialization_is_exact(
    route: &mimi::core::mir::CanonicalMirRouteMaterialization,
) -> bool {
    route.admission.ordinary_map_owned_any_v2
        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::KeysSnapshot
        && route
            .ordinary_map_owned_any_v2_keys_snapshot_route_receipt
            .as_ref()
            .is_some_and(|receipt| {
                receipt
                    == &route.program.route_receipt(
                        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE,
                    )
            })
}

fn ordinary_map_owned_any_v2_dynamic_keys_get_materialization_is_exact(
    route: &mimi::core::mir::CanonicalMirRouteMaterialization,
) -> bool {
    route.admission.ordinary_map_owned_any_v2_dynamic_keys_get()
        && route
            .ordinary_map_owned_any_v2_dynamic_keys_get_route_receipt
            .as_ref()
            .is_some_and(|receipt| {
                receipt
                    == &route.program.route_receipt(
                        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE,
                    )
            })
}

/// Preflight the exact source-bound argv[0] dynamic-key, Keys snapshot, and
/// same-key GetAny route. Every backend consumer validates the same narrow
/// receipt, and this profile remains independent of broad V2 readiness.
pub(crate) fn preflight_ordinary_map_owned_any_v2_dynamic_keys_get(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    let receipt = mimi::core::mir::validate_ordinary_map_owned_any_v2_dynamic_keys_get_route(
        checked, program,
    )?;
    let profile = mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE;
    if receipt.profile != profile {
        return Err(format!(
            "DynamicKeysGet requires exact route profile '{profile}', got '{}'",
            receipt.profile
        ));
    }
    receipt.validate_against_program(program)?;
    if !receipt.same_semantic_identity(&program.route_receipt(profile)) {
        return Err(
            "DynamicKeysGet Checker receipt differs from the canonical MIR profile receipt".into(),
        );
    }
    match mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_dynamic_keys_get_route(
        checked,
        program,
        &receipt,
        String::new(),
    ) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

/// Preflight the exact Checker-owned sorted Keys snapshot and literal-zero
/// String copy. This route consumes the Checker/MIR receipt and independently
/// rederived witness across bytecode, native validation, and verification;
/// broad owner-v2 readiness remains closed.
pub(crate) fn preflight_ordinary_map_owned_any_v2_keys_snapshot(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    let receipt =
        mimi::core::mir::validate_ordinary_map_owned_any_v2_keys_snapshot_route(checked, program)?;
    preflight_ordinary_map_owned_any_v2_keys_snapshot_with_receipt(checked, program, &receipt)
}

fn preflight_ordinary_map_owned_any_v2_keys_snapshot_with_receipt(
    checked: &CheckedProgram,
    program: &MirProgram,
    receipt: &mimi::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), String> {
    let profile = mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE;
    if receipt.profile != profile {
        return Err(format!(
            "KeysSnapshot requires exact route profile '{profile}', got '{}'",
            receipt.profile
        ));
    }
    let checker_receipt =
        mimi::core::mir::validate_ordinary_map_owned_any_v2_keys_snapshot_route(checked, program)?;
    receipt.validate_against_program(program)?;
    if !checker_receipt.same_semantic_identity(receipt) {
        return Err(
            "KeysSnapshot supplied route receipt differs from the CheckedProgram-bound MIR receipt"
                .into(),
        );
    }
    // The Checker-bound route validator above already rederives this exact
    // MIR-only witness before returning its public route receipt.
    mimi::verifier::validate_mir_capabilities_with_ordinary_map_owned_any_v2_keys_snapshot_route(
        checked, program, receipt,
    )
    .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_keys_snapshot_route(checked, program)
        .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_mir_native(program)
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    match mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_keys_snapshot_route(
        checked,
        program,
        receipt,
        String::new(),
    ) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

/// Consumer preflight for the exact snapshot-after-replacement profile. The
/// route owns an independent Checker/MIR receipt and cannot borrow either the
/// one-element ValuesFieldMove proof or broad V2 readiness.
fn preflight_ordinary_map_owned_any_v2_values_snapshot_replacement(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    mimi::core::mir::validate_ordinary_map_owned_any_v2_values_snapshot_replacement_route(
        checked, program,
    )?;
    let receipt = program.route_receipt(
        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE,
    );
    receipt.validate_against_program(program)?;
    mimi::verifier::validate_mir_capabilities(program)
        .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_values_snapshot_replacement_route(
        checked, program,
    )
    .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_mir_native(program)
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    match mimi::verifier::verify_mir_with_route_receipt(program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

fn select_ordinary_map_owned_any_v2_values_snapshot_replacement_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_values_snapshot_replacement(checked, &program) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 ValuesSnapshotReplacement preflight failed: {reason}"
        )),
    }
}

/// Consumer preflight for the mixed Values snapshot profile. Its Checker and
/// MIR receipts bind all three heterogeneous entries to one immutable
/// snapshot, independently of the broader V2 production-readiness gate.
fn preflight_ordinary_map_owned_any_v2_values_mixed_snapshot(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    mimi::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_route(
        checked, program,
    )?;
    let receipt = program
        .route_receipt(mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE);
    receipt.validate_against_program(program)?;
    mimi::verifier::validate_mir_capabilities(program)
        .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_values_mixed_snapshot_route(checked, program)
        .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_mir_native(program)
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    match mimi::verifier::verify_mir_with_route_receipt(program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

fn select_ordinary_map_owned_any_v2_values_mixed_snapshot_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_values_mixed_snapshot(checked, &program) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 ValuesMixedSnapshot preflight failed: {reason}"
        )),
    }
}

/// Consumer preflight for a mixed snapshot followed by a persistent Map
/// removal. Its own receipt binds the post-snapshot root transformation.
fn preflight_map_empty_root_root_queries_v1(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    mimi::core::mir::validate_map_empty_root_root_queries_v1_route(checked, program)?;
    let receipt = program.route_receipt(mimi::core::mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE);
    receipt.validate_against_program(program)?;
    mimi::verifier::validate_mir_capabilities(program)
        .map_err(|e| format!("MIR verifier capability: {e:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_empty_root_root_queries_v1_route(
        checked, program,
    )
    .map_err(|e| format!("MIR bytecode capability: {e:?}"))?;
    mimi::codegen::mir::validate_mir_native(program)
        .map_err(|e| format!("MIR native capability: {e:?}"))?;
    let results = mimi::verifier::verify_mir_with_map_empty_root_root_queries_v1_route(
        checked,
        program,
        &receipt,
        String::new(),
    )?;
    if !mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) {
        return Err(format!("MIR verifier profile unsupported: {results:?}"));
    }
    Ok(())
}

fn preflight_ordinary_map_owned_any_v2_values_mixed_snapshot_remove(
    checked: &CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    mimi::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_route(
        checked, program,
    )?;
    let receipt = program.route_receipt(
        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE,
    );
    receipt.validate_against_program(program)?;
    mimi::verifier::validate_mir_capabilities(program)
        .map_err(|errors| format!("MIR verifier capability: {errors:?}"))?;
    mimi::interp::bytecode::compile_mir_program_with_values_mixed_snapshot_remove_route(
        checked, program,
    )
    .map_err(|errors| format!("MIR bytecode capability: {errors:?}"))?;
    mimi::codegen::mir::validate_mir_native(program)
        .map_err(|errors| format!("MIR native capability: {errors:?}"))?;
    match mimi::verifier::verify_mir_with_route_receipt(program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("MIR verifier pass: {error}")),
    }
}

fn select_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    match preflight_ordinary_map_owned_any_v2_values_mixed_snapshot_remove(checked, &program) {
        Ok(()) => DefaultMirRoute::Canonical(Box::new(program)),
        Err(reason) => DefaultMirRoute::Rejected(format!(
            "ordinary Map/Any owner-v2 ValuesMixedSnapshotRemove preflight failed: {reason}"
        )),
    }
}

/// Consumer preflight for the distinct typed owned-get profile. Its getter
/// authority is not inherited from the old ordinary lifecycle receipt.
fn select_ordinary_map_owned_get_i32_string_v1_route(program: MirProgram) -> DefaultMirRoute {
    if let Err(errors) = mimi::verifier::validate_mir_capabilities(&program) {
        return DefaultMirRoute::Rejected(format!(
            "ordinary Map owned-get MIR verifier capability: {errors:?}"
        ));
    }
    let receipt =
        program.route_receipt(mimi::core::mir::ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND);
    if let Err(errors) =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&program, &receipt)
    {
        return DefaultMirRoute::Rejected(format!(
            "ordinary Map owned-get MIR bytecode capability: {errors:?}"
        ));
    }
    if let Err(errors) = mimi::codegen::mir::validate_mir_native(&program) {
        return DefaultMirRoute::Rejected(format!(
            "ordinary Map owned-get MIR native capability: {errors:?}"
        ));
    }
    match mimi::verifier::verify_mir_with_route_receipt(&program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            DefaultMirRoute::Canonical(Box::new(program))
        }
        Ok(results) => DefaultMirRoute::Rejected(format!(
            "ordinary Map owned-get verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => DefaultMirRoute::Rejected(format!(
            "ordinary Map owned-get MIR verifier pass: {error}"
        )),
    }
}

/// Revalidate every independent consumer against the same finite Actor graph.
/// NoContracts is a neutral observation; any proof result would be a gate bug.
fn select_scalar_actor_route(program: MirProgram) -> DefaultMirRoute {
    let island = mimi::core::mir::SCALAR_ACTOR_ISLAND;
    if let Err(errors) = mimi::core::mir::validate_scalar_actor_island(&program) {
        return DefaultMirRoute::Rejected(format!("{island} receipt capability: {errors:?}"));
    }
    if let Err(errors) = mimi::verifier::validate_mir_capabilities(&program) {
        return DefaultMirRoute::Rejected(format!("{island} verifier capability: {errors:?}"));
    }
    let receipt = program.route_receipt(island);
    if let Err(errors) =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&program, &receipt)
    {
        return DefaultMirRoute::Rejected(format!("{island} bytecode capability: {errors:?}"));
    }
    if let Err(errors) = mimi::codegen::mir::validate_mir_native(&program) {
        return DefaultMirRoute::Rejected(format!("{island} native capability: {errors:?}"));
    }
    match mimi::verifier::verify_mir_with_route_receipt(&program, &receipt, String::new()) {
        Ok(results) if results.is_empty() => DefaultMirRoute::Canonical(Box::new(program)),
        Ok(results) => DefaultMirRoute::Rejected(format!(
            "{island} expected neutral NoContracts, received {results:?}"
        )),
        Err(error) => DefaultMirRoute::Rejected(format!("{island} verifier pass: {error}")),
    }
}

/// Consumer preflight for the shared scalar FFI receipt. An error at any
/// stage is a hard rejection; this adapter has no compatibility return arm.
fn select_scalar_ffi_route(program: MirProgram) -> DefaultMirRoute {
    if let Err(errors) = mimi::verifier::validate_mir_capabilities(&program) {
        return DefaultMirRoute::Rejected(format!(
            "{}: scalar FFI MIR verifier capability: {errors:?}",
            mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    let receipt = program.route_receipt("scalar-ffi-v1");
    if let Err(errors) =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&program, &receipt)
    {
        return DefaultMirRoute::Rejected(format!(
            "{}: scalar FFI MIR bytecode capability: {errors:?}",
            mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    if let Err(errors) = mimi::codegen::mir::validate_mir_native(&program) {
        return DefaultMirRoute::Rejected(format!(
            "{}: scalar FFI MIR native capability: {errors:?}",
            mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        ));
    }
    match mimi::verifier::verify_mir_with_route_receipt(&program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            DefaultMirRoute::Canonical(Box::new(program))
        }
        Ok(results) => DefaultMirRoute::Rejected(format!(
            "{}: scalar FFI MIR verifier returned an unsupported or inconclusive result: {results:?}",
            mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )),
        Err(error) => DefaultMirRoute::Rejected(format!(
            "{}: scalar FFI MIR verifier pass: {error}",
            mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
        )),
    }
}

/// Consumer preflight for direct-root nested copy-scalar callables. Their
/// checker receipt may describe an empty capture environment or a
/// declaration-time Copy i32 snapshot suffix. This route has no external FFI
/// receipt; the nested-callable scope receipt itself is the canonical
/// operation boundary.
fn select_scalar_nested_route(program: MirProgram) -> DefaultMirRoute {
    let island = mimi::core::mir::SCALAR_NESTED_ISLAND;
    if program.nested_callable_scopes().is_empty() {
        return DefaultMirRoute::Rejected(format!(
            "{island} route has no checker-owned nested-callable scope receipt"
        ));
    }
    if let Err(errors) = mimi::verifier::validate_mir_capabilities(&program) {
        return DefaultMirRoute::Rejected(format!("{island} MIR verifier capability: {errors:?}"));
    }
    let receipt = program.route_receipt(island);
    if let Err(errors) =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&program, &receipt)
    {
        return DefaultMirRoute::Rejected(format!("{island} MIR bytecode capability: {errors:?}"));
    }
    if let Err(errors) = mimi::codegen::mir::validate_mir_native(&program) {
        return DefaultMirRoute::Rejected(format!("{island} MIR native capability: {errors:?}"));
    }
    match mimi::verifier::verify_mir_with_route_receipt(&program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            DefaultMirRoute::Canonical(Box::new(program))
        }
        Ok(results) => DefaultMirRoute::Rejected(format!(
            "{island} MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => DefaultMirRoute::Rejected(format!("{island} MIR verifier pass: {error}")),
    }
}

/// Preflight the checked scalar C data exports across the canonical consumers.
/// This route is admitted only when no other checker profile already owns the
/// program; unsupported function bodies therefore fail closed here.
fn select_native_data_export_route(
    checked: &CheckedProgram,
    program: MirProgram,
) -> DefaultMirRoute {
    let route = NATIVE_DATA_EXPORT_ROUTE_PROFILE;
    let receipt = program.route_receipt(route);
    if let Err(error) = preflight_native_data_export_route(checked, &program, &receipt) {
        return DefaultMirRoute::Rejected(error);
    }
    DefaultMirRoute::Canonical(Box::new(program))
}

/// Shared L2 admission for the data-only native export profile. Default
/// selection and explicit `build --mir` call this same gate with the exact
/// CheckedProgram, MIR graph, and route receipt they later hand to codegen.
pub(crate) fn preflight_native_data_export_route(
    checked: &CheckedProgram,
    program: &MirProgram,
    receipt: &mimi::core::mir::CanonicalMirRouteReceipt,
) -> Result<(), String> {
    let route = NATIVE_DATA_EXPORT_ROUTE_PROFILE;
    if receipt.profile != route {
        return Err(format!(
            "{route} route received profile {:?}",
            receipt.profile
        ));
    }
    let table_receipt = mimi::core::native_data_export::checked_native_data_export_table_receipt(
        checked,
    )
    .map_err(|errors| {
        format!(
            "{}: checked native data export validation failed: {errors:?}",
            mimi::core::native_data_export::NATIVE_DATA_EXPORT_ERROR_CODE
        )
    })?;
    if table_receipt.export_count == 0 {
        return Err(format!(
            "{route} route has no checker-owned native data export descriptors"
        ));
    }
    receipt
        .validate_against_program(program)
        .map_err(|error| format!("{route} MIR route receipt rejected: {error}"))?;
    if let Err(errors) = mimi::verifier::validate_mir_capabilities(program) {
        return Err(format!("{route} MIR verifier capability: {errors:?}"));
    }
    if let Err(errors) =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(program, receipt)
    {
        return Err(format!("{route} MIR bytecode capability: {errors:?}"));
    }
    if let Err(errors) = mimi::codegen::mir::validate_mir_native(program) {
        return Err(format!("{route} MIR native capability: {errors:?}"));
    }
    match mimi::verifier::verify_mir_with_route_receipt(program, receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            Ok(())
        }
        Ok(results) => Err(format!(
            "{route} MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => Err(format!("{route} MIR verifier pass: {error}")),
    }
}

/// Consumer preflight for the checker-owned scalar C-export wrapper receipt.
/// The MIR body is executed by bytecode/reference/verifier; native emission is
/// the only consumer that adds the public C adapter from the same receipt.
fn select_scalar_export_route(program: MirProgram) -> DefaultMirRoute {
    let island = mimi::core::mir::SCALAR_EXPORT_ISLAND;
    if program.export_declarations().is_empty() {
        return DefaultMirRoute::Rejected(format!(
            "{island} route has no checker-owned export declaration"
        ));
    }
    if let Err(errors) = mimi::verifier::validate_mir_capabilities(&program) {
        return DefaultMirRoute::Rejected(format!("{island} MIR verifier capability: {errors:?}"));
    }
    let receipt = program.route_receipt(island);
    if let Err(errors) =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&program, &receipt)
    {
        return DefaultMirRoute::Rejected(format!("{island} MIR bytecode capability: {errors:?}"));
    }
    if let Err(errors) = mimi::codegen::mir::validate_mir_native(&program) {
        return DefaultMirRoute::Rejected(format!("{island} MIR native capability: {errors:?}"));
    }
    match mimi::verifier::verify_mir_with_route_receipt(&program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            DefaultMirRoute::Canonical(Box::new(program))
        }
        Ok(results) => DefaultMirRoute::Rejected(format!(
            "{island} MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => DefaultMirRoute::Rejected(format!("{island} MIR verifier pass: {error}")),
    }
}

/// Consumer preflight for the bounded scalar generic identity route. The
/// materializer has already checked its exact one-instance whole-program
/// envelope; this repeats the profile gate immediately before each consumer
/// capability check so the selected graph remains hard-bound to the profile.
fn select_scalar_generic_identity_route(
    program: MirProgram,
    island: &'static str,
    validate: fn(&MirProgram) -> Result<(), Vec<String>>,
) -> DefaultMirRoute {
    if let Err(errors) = validate(&program) {
        return DefaultMirRoute::Rejected(format!("{island} MIR island capability: {errors:?}"));
    }
    if let Err(errors) = mimi::verifier::validate_mir_capabilities(&program) {
        return DefaultMirRoute::Rejected(format!("{island} MIR verifier capability: {errors:?}"));
    }
    let receipt = program.route_receipt(island);
    if let Err(errors) =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&program, &receipt)
    {
        return DefaultMirRoute::Rejected(format!("{island} MIR bytecode capability: {errors:?}"));
    }
    if let Err(errors) = mimi::codegen::mir::validate_mir_native(&program) {
        return DefaultMirRoute::Rejected(format!("{island} MIR native capability: {errors:?}"));
    }
    match mimi::verifier::verify_mir_with_route_receipt(&program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            DefaultMirRoute::Canonical(Box::new(program))
        }
        Ok(results) => DefaultMirRoute::Rejected(format!(
            "{island} MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => {
            DefaultMirRoute::Rejected(format!("{island} MIR verifier pass failed: {error}"))
        }
    }
}

/// Consumer preflight for the bounded static generic-trait specialization
/// route.  The candidate predicate admits one checked protocol method with at
/// least two concrete, monomorphic impl owners for the same generic receiver
/// constructor; this gate then requires all four canonical consumers.
fn select_static_generic_trait_specialization_route(program: MirProgram) -> DefaultMirRoute {
    const ISLAND: &str = mimi::codegen::STATIC_GENERIC_TRAIT_SPECIALIZATION_ROUTE_PROFILE;
    if let Err(errors) = mimi::verifier::validate_mir_capabilities(&program) {
        return DefaultMirRoute::Rejected(format!("{ISLAND} MIR verifier capability: {errors:?}"));
    }
    let receipt = program.route_receipt(ISLAND);
    if let Err(errors) =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&program, &receipt)
    {
        return DefaultMirRoute::Rejected(format!("{ISLAND} MIR bytecode capability: {errors:?}"));
    }
    if let Err(errors) = mimi::codegen::mir::validate_mir_native(&program) {
        return DefaultMirRoute::Rejected(format!("{ISLAND} MIR native capability: {errors:?}"));
    }
    match mimi::verifier::verify_mir_with_route_receipt(&program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            DefaultMirRoute::Canonical(Box::new(program))
        }
        Ok(results) => DefaultMirRoute::Rejected(format!(
            "{ISLAND} MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => DefaultMirRoute::Rejected(format!("{ISLAND} MIR verifier pass: {error}")),
    }
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
    } else if reason.contains("nested Option tuple") {
        DefaultMirRoute::Rejected(format!(
            "non-Copy nested Option tuple variant candidate is not eligible for the default route: {}",
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CopyCandidateFlags {
    option_i32: bool,
    option_bool: bool,
    option_i64: bool,
    option_f64: bool,
    result_i32: bool,
}

fn reject_migrated_candidates_with_copy(
    flow_candidate: bool,
    collection_candidate: bool,
    record_candidate: bool,
    option_string_candidate: bool,
    copy_candidates: CopyCandidateFlags,
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
    } else if copy_candidates.option_i32 {
        DefaultMirRoute::Rejected(format!(
            "S114 Copy Option<i32> variant candidate is not eligible for the default route: {}",
            reason
        ))
    } else if copy_candidates.option_bool {
        DefaultMirRoute::Rejected(format!(
            "S115 Copy Option<bool> variant candidate is not eligible for the default route: {}",
            reason
        ))
    } else if copy_candidates.option_i64 {
        DefaultMirRoute::Rejected(format!(
            "S116 Copy Option<i64> variant candidate is not eligible for the default route: {}",
            reason
        ))
    } else if copy_candidates.option_f64 {
        DefaultMirRoute::Rejected(format!(
            "S117 Copy Option<f64> variant candidate is not eligible for the default route: {}",
            reason
        ))
    } else if copy_candidates.result_i32 {
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

/// Checker-level mirror of
/// [`mimi::core::mir::contains_multi_target_flow_union_candidate`]: whether
/// any transition declares multiple non-fallback targets.  The route uses it
/// to enter the union face checks instead of classifying the program as an
/// outside-profile input (R6-1040).
fn may_contain_multi_target_flow_union(checked: &CheckedProgram) -> bool {
    checked.transitions().values().any(|transition| {
        transition.targets.len() > 1 && !transition.is_fallback && !transition.is_ffi_pinned
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_contract_candidate_gate_failure_is_rejected() {
        assert_eq!(
            scalar_collection_gate_failure_disposition(true, false, false),
            ScalarCollectionGateFailureDisposition::RejectMigratedCandidate
        );
    }

    #[test]
    fn plain_scalar_stdout_gate_failure_keeps_legacy_compatibility() {
        assert_eq!(
            scalar_collection_gate_failure_disposition(false, false, false),
            ScalarCollectionGateFailureDisposition::PlainScalarStdoutCompatibility
        );
    }

    fn checked(source: &str) -> (CheckedProgram, File) {
        let tokens = mimi::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = mimi::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let checked = mimi::core::check_program(&file).expect("check");
        (checked, file)
    }

    #[test]
    fn pending_map_new_result_source_receipt_rejects_default_and_explicit_routes() {
        let map_new_result_source = r#"
            func observe() -> i32 {
                let outcome = map_new()
                match outcome {
                    Ok(root) => {
                        let size = map_size(root)
                        drop(root)
                        size
                    },
                    Err(status) => status
                }
            }
            func main() -> i32 { 0 }
        "#;
        let (checked_program, file) = checked(map_new_result_source);
        assert!(map_new_result_source_pending_reason(&checked_program).is_some());

        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked_program, &file)
        else {
            panic!("Checker NewResult source receipt must reject before any legacy fallback");
        };
        assert_eq!(reason, MAP_NEW_RESULT_SOURCE_PENDING_REASON);
        assert_eq!(
            build_canonical_program(&checked_program, &file).expect_err(
                "explicit canonical build must reject while the NewResult route receipt is pending"
            ),
            MAP_NEW_RESULT_SOURCE_PENDING_REASON
        );

        let near_misses = [
            (
                "missing root Drop",
                map_new_result_source.replace("                        drop(root)\n", ""),
            ),
            (
                "extra Map operation",
                map_new_result_source.replace(
                    "                        let size = map_size(root)\n                        drop(root)\n                        size",
                    "                        let size = map_size(root)\n                        let extra_size = map_size(root)\n                        drop(root)\n                        size + extra_size",
                ),
            ),
            (
                "rewritten Err payload",
                map_new_result_source.replace(
                    "Err(status) => status",
                    "Err(status) => status + 1",
                ),
            ),
        ];
        for (shape, source) in near_misses {
            let (near_miss, near_miss_file) = checked(&source);
            assert!(
                map_new_result_source_pending_reason(&near_miss).is_none(),
                "{shape} must not receive the exact NewResult receipt"
            );
            assert_eq!(
                mimi::core::mir::classify_canonical_mir_route_admission(&near_miss)
                    .ordinary_map_owned_any_v2,
                mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage,
                "the receipt-independent map_new Result census must claim {shape}"
            );
            let DefaultMirRoute::Rejected(default_reason) =
                select_default_route(&near_miss, &near_miss_file)
            else {
                panic!("unsupported {shape} must not fall through to Legacy");
            };
            assert!(
                default_reason.contains("incomplete ordinary Map/Any owner-v2 attempt"),
                "{shape}: {default_reason}"
            );
            let explicit_reason = build_canonical_program(&near_miss, &near_miss_file)
                .expect_err("explicit canonical build must reject unsupported {shape}");
            assert!(
                explicit_reason.contains("MIR-COVERAGE-001"),
                "{shape}: {explicit_reason}"
            );
        }

        let (plain_result, plain_result_file) = checked(include_str!(
            "../../tests/fixtures/mir_native_result_i32_unwrap.mimi"
        ));
        assert!(map_new_result_source_pending_reason(&plain_result).is_none());
        assert!(matches!(
            select_default_route(&plain_result, &plain_result_file),
            DefaultMirRoute::Canonical(_)
        ));
        assert!(build_canonical_program(&plain_result, &plain_result_file).is_ok());
    }

    /// MIR route integration tests use the host process's 8 MiB stack budget.
    /// Rust's test harness gives each worker a smaller stack than the
    /// production CLI entry thread uses during full graph lowering.
    fn on_process_sized_stack(test: impl FnOnce() + Send + 'static) {
        std::thread::Builder::new()
            .name("mimi-route-integration".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(test)
            .expect("spawn route integration test")
            .join()
            .expect("route integration test must not panic");
    }

    fn checked_get_source(
        set_key: &str,
        get_key: &str,
        set_value: &str,
        unpack_type: &str,
    ) -> String {
        let unpack_expression = checked_get_unpack_expression(unpack_type);
        format!(
            r#"
func main() -> bool {{
    let empty = map_new()
    let root = map_set(empty, {set_key}, {set_value})
    let pair = map_get(root, {get_key})
    drop(empty)
    drop(root)
    let found = pair.0
    let payload = {unpack_expression}
    drop(pair)
    found
}}
"#
        )
    }

    fn checked_get_unpack_expression(unpack_type: &str) -> &'static str {
        match unpack_type {
            "i32" => "unpack_i32(pair.1)",
            "string" => "unpack_string(pair.1)",
            "Label" => "pair.1 as Label",
            other => panic!("unsupported CheckedGet test payload type: {other}"),
        }
    }

    fn checked_get_observed_source(
        preamble: &str,
        set_key: &str,
        get_key: &str,
        set_value: &str,
        unpack_type: &str,
        result_type: &str,
        result_expression: &str,
        payload_cleanup: &str,
    ) -> String {
        let unpack_expression = checked_get_unpack_expression(unpack_type);
        format!(
            r#"
{preamble}
func main() -> {result_type} {{
    let empty = map_new()
    let root = map_set(empty, {set_key}, {set_value})
    let pair = map_get(root, {get_key})
    drop(empty)
    drop(root)
    let found = pair.0
    let payload = {unpack_expression}
    drop(pair)
    {payload_cleanup}
    {result_expression}
}}
"#
        )
    }

    #[test]
    fn ordinary_map_owner_observer_v1_uses_its_exact_receipt_for_default_and_explicit_routes() {
        let source = include_str!("../../tests/fixtures/mir_ordinary_map_owner_observer_v1.mimi");
        let (checked, file) = checked(source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
        assert!(admission
            .ordinary_map_owner_lifecycle_observer_v1
            .has_profile_signal());
        assert!(admission
            .ordinary_map_owner_lifecycle_observer_v1
            .is_complete());
        assert!(!admission.ordinary_map_owned_any_v2.has_profile_signal());
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);

        let profile = mimi::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE;
        for default_profile in [
            "run-canonical-v1",
            mimi::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE,
            "verify-canonical-v1",
        ] {
            assert_eq!(
                route_receipt_profile_for_checked(&checked, default_profile),
                profile,
                "Observer V1 identity must be shared by {default_profile}"
            );
        }

        let materialized = materialize_canonical_route(&checked, &file)
            .expect("complete Observer V1 source materializes Canonical MIR");
        assert!(ordinary_map_owner_lifecycle_observer_v1_materialization_is_exact(&materialized));
        let materialized_receipt = materialized
            .ordinary_map_owner_lifecycle_observer_v1_route_receipt
            .as_ref()
            .expect("materialization records the dedicated Observer V1 receipt");
        assert_eq!(materialized_receipt.profile, profile);
        materialized_receipt
            .validate_against_program(&materialized.program)
            .expect("Observer V1 receipt is bound to the materialized MIR graph");
        let validated =
            mimi::core::mir::validate_ordinary_map_owner_lifecycle_observer_v1_mir_route(
                &materialized.program,
            )
            .expect("independent Observer V1 MIR validator accepts the exact source census");
        let derived = materialized
            .program
            .ordinary_map_owner_lifecycle_observer_v1_receipts()
            .expect("MIR can independently derive Observer V1 receipts");
        assert!(!validated.is_empty());
        assert_eq!(validated, derived);

        for (label, route) in [
            ("execution", select_default_route(&checked, &file)),
            (
                "verification",
                select_default_verification_route(&checked, &file),
            ),
        ] {
            let DefaultMirRoute::Canonical(program) = route else {
                panic!("Observer V1 {label} route must select its dedicated canonical profile");
            };
            let receipt = program.route_receipt(profile);
            assert_eq!(receipt.profile, profile);
            receipt
                .validate_against_program(&program)
                .expect("default Observer V1 route receipt matches its MIR graph");
            assert!(
                mimi::core::mir::validate_ordinary_map_owner_lifecycle_observer_v1_mir_route(
                    &program
                )
                .is_ok()
            );
        }

        let explicit = build_canonical_program(&checked, &file)
            .expect("explicit --mir build uses Observer V1 preflight");
        let receipt = explicit.route_receipt(profile);
        assert_eq!(receipt.profile, profile);
        receipt
            .validate_against_program(&explicit)
            .expect("explicit Observer V1 route receipt matches its MIR graph");
    }

    #[test]
    fn observer_v1_readiness_census_emits_the_global_v2_constant_once() {
        let line = observer_v1_readiness_census_line();
        let readiness_fields = line
            .split_whitespace()
            .filter_map(|token| token.strip_prefix("global_map_any_v2_production_ready="))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(
            readiness_fields,
            vec![mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY.to_string()]
        );
    }

    #[test]
    fn incomplete_observer_signal_is_rejected_without_legacy_fallback() {
        let source = include_str!("../../tests/fixtures/mir_ordinary_map_owner_observer_v1.mimi");
        let source = source.replace(
            "    let snapshot = to_json(m2)\n",
            "    let snapshot = to_json(m2)\n    let unsupported = map_get(m2, \"a-number\")\n    drop(unsupported)\n",
        );
        assert_ne!(
            source,
            include_str!("../../tests/fixtures/mir_ordinary_map_owner_observer_v1.mimi"),
            "the test must inject an unsupported Map operation"
        );
        let (checked, file) = checked(&source);
        let observer_admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked)
            .ordinary_map_owner_lifecycle_observer_v1;
        assert!(observer_admission.has_profile_signal());
        assert!(!observer_admission.is_complete());

        for (label, route) in [
            ("execution", select_default_route(&checked, &file)),
            (
                "verification",
                select_default_verification_route(&checked, &file),
            ),
        ] {
            let DefaultMirRoute::Rejected(reason) = route else {
                panic!("incomplete Observer V1 {label} signal must be rejected, never fallback");
            };
            assert!(reason.contains("Observer V1"), "{label}: {reason}");
            assert!(!reason.contains("legacy"), "{label}: {reason}");
        }

        let explicit_error = build_canonical_program(&checked, &file)
            .expect_err("explicit --mir must reject incomplete Observer V1 coverage");
        assert!(explicit_error.contains("Observer V1"), "{explicit_error}");
    }

    #[test]
    fn observer_v1_candidate_cannot_be_composed_with_scalar_ffi() {
        let observer_source =
            include_str!("../../tests/fixtures/mir_ordinary_map_owner_observer_v1.mimi");
        let source = format!(
            r#"
extern "C" {{ func scalar_probe(value: i32) -> i32; }}
func unused_scalar_ffi_helper() -> i32 {{ scalar_probe(0) }}
{observer_source}
"#
        );
        let (checked, file) = checked(&source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
        assert!(admission.scalar_ffi);
        assert!(admission
            .ordinary_map_owner_lifecycle_observer_v1
            .is_complete());
        let reason = scalar_ffi_map_owner_composition_failure(admission)
            .expect("Observer V1 is in the Map-owner/scalar-FFI composition gate");
        assert!(reason.contains("cannot be combined"), "{reason}");
        assert!(reason.contains("scalar FFI"), "{reason}");

        for (label, route) in [
            ("execution", select_default_route(&checked, &file)),
            (
                "verification",
                select_default_verification_route(&checked, &file),
            ),
        ] {
            let DefaultMirRoute::Rejected(reason) = route else {
                panic!("Observer V1 plus scalar FFI must reject {label}, not choose a route");
            };
            assert!(reason.contains("cannot be combined"), "{label}: {reason}");
            assert!(reason.contains("scalar FFI"), "{label}: {reason}");
        }
        let explicit_error = build_canonical_program(&checked, &file)
            .expect_err("explicit --mir must share the Observer V1/scalar-FFI gate");
        assert!(
            explicit_error.contains("cannot be combined"),
            "{explicit_error}"
        );
    }

    #[test]
    fn direct_print_uses_its_exact_route_for_default_verification_and_explicit_mir() {
        let cases = [
            include_str!(
                "../../tests/fixtures/mir_ordinary_map_any_owner_v2_direct_print_hit_i32.mimi"
            ),
            include_str!(
                "../../tests/fixtures/mir_ordinary_map_any_owner_v2_direct_print_hit_string.mimi"
            ),
            include_str!(
                "../../tests/fixtures/mir_ordinary_map_any_owner_v2_direct_print_miss_i32.mimi"
            ),
            include_str!(
                "../../tests/fixtures/mir_ordinary_map_any_owner_v2_direct_print_same_root_map_size_i32.mimi"
            ),
        ];
        let profile = mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE;

        for source in cases {
            let (checked, file) = checked(source);
            let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2;
            assert_eq!(
                admission,
                mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::DirectPrint
            );
            assert!(!admission.is_complete());
            assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
            for default_profile in [
                "run-canonical-v1",
                mimi::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE,
                "verify-canonical-v1",
            ] {
                assert_eq!(
                    route_receipt_profile_for_checked(&checked, default_profile),
                    profile,
                    "the narrow route identity must be shared across CLI entries"
                );
            }

            for (label, route) in [
                ("execution", select_default_route(&checked, &file)),
                (
                    "verification",
                    select_default_verification_route(&checked, &file),
                ),
            ] {
                let DefaultMirRoute::Canonical(program) = route else {
                    panic!("DirectPrint {label} selector must use the narrow canonical route");
                };
                let receipt = program.route_receipt(profile);
                assert_eq!(receipt.profile, profile);
                receipt
                    .validate_against_program(&program)
                    .expect("the route receipt is bound to the exact MIR graph");
                mimi::core::mir::validate_ordinary_map_owned_any_v2_direct_print_mir_route(
                    &program,
                )
                .expect("the Checker-selected payload and print must be receipted");
            }

            let explicit = build_canonical_program(&checked, &file)
                .expect("explicit --mir must share the DirectPrint consumer preflight");
            let receipt = explicit.route_receipt(profile);
            assert_eq!(receipt.profile, profile);
            receipt
                .validate_against_program(&explicit)
                .expect("explicit --mir uses the same profile-bound receipt");
        }
    }

    #[test]
    fn partial_direct_print_candidates_are_rejected_without_legacy_fallback() {
        let source = include_str!(
            "../../tests/fixtures/mir_ordinary_map_any_owner_v2_direct_print_hit_i32.mimi"
        );
        let unsupported = [
            (
                "repeated print",
                source.replace(
                    "    println(pair.1)\n",
                    "    println(pair.1)\n    println(pair.1)\n",
                ),
            ),
            (
                "aliased payload",
                source.replace(
                    "    println(pair.1)\n",
                    "    let payload = pair.1\n    println(payload)\n",
                ),
            ),
        ];
        for (label, source) in unsupported {
            let (checked, file) = checked(&source);
            let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2;
            assert_ne!(
                admission,
                mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::DirectPrint,
                "{label} must not receive the direct-print admission"
            );
            match select_default_route(&checked, &file) {
                DefaultMirRoute::Rejected(reason) => assert!(
                    !reason.contains("legacy"),
                    "{label} must remain a hard canonical rejection: {reason}"
                ),
                DefaultMirRoute::Legacy(reason) => {
                    panic!("{label} escaped to legacy route {reason:?}")
                }
                DefaultMirRoute::Canonical(_) => {
                    panic!("{label} must not select a canonical execution route")
                }
            }
            assert!(
                build_canonical_program(&checked, &file).is_err(),
                "{label} must fail the explicit --mir route as well"
            );
        }
    }

    #[test]
    fn values_field_move_uses_its_exact_receipt_for_default_and_explicit_routes() {
        let source = include_str!(
            "../../tests/fixtures/mir_ordinary_map_any_owner_v2_values_field_move.mimi"
        );
        let (checked, file) = checked(source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked)
            .ordinary_map_owned_any_v2;
        assert_eq!(
            admission,
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesFieldMove
        );
        assert!(!admission.is_complete());
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        assert_eq!(
            route_receipt_profile_for_checked(&checked, "run-canonical-v1"),
            mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE
        );
        assert_eq!(
            route_receipt_profile_for_checked(
                &checked,
                mimi::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE,
            ),
            mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE
        );
        assert_eq!(
            route_receipt_profile_for_checked(&checked, "verify-canonical-v1"),
            mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE
        );

        for (label, route) in [
            ("execution", select_default_route(&checked, &file)),
            (
                "verification",
                select_default_verification_route(&checked, &file),
            ),
        ] {
            let DefaultMirRoute::Canonical(program) = route else {
                panic!("ValuesFieldMove {label} route must pass its narrow preflight");
            };
            let receipt = program.route_receipt(
                mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE,
            );
            assert_eq!(
                receipt.profile,
                mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE
            );
            receipt
                .validate_against_program(&program)
                .expect("dedicated ValuesFieldMove receipt is bound to this MIR graph");
        }

        let explicit = build_canonical_program(&checked, &file)
            .expect("explicit --mir builder must use the same ValuesFieldMove preflight");
        assert_eq!(
            explicit
                .route_receipt(route_receipt_profile_for_checked(
                    &checked,
                    "run-canonical-v1",
                ))
                .profile,
            mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE
        );
    }

    #[test]
    fn values_snapshot_replacement_uses_its_exact_receipt_for_default_and_explicit_routes() {
        let source = include_str!(
            "../../tests/fixtures/mir_ordinary_map_any_owner_v2_values_snapshot_replacement.mimi"
        );
        let (checked, file) = checked(source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked)
            .ordinary_map_owned_any_v2;
        assert_eq!(
            admission,
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesSnapshotReplacement
        );
        assert!(!admission.is_complete());
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        for default_profile in [
            "run-canonical-v1",
            mimi::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE,
            "verify-canonical-v1",
        ] {
            assert_eq!(
                route_receipt_profile_for_checked(&checked, default_profile),
                mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE
            );
        }

        for (label, route) in [
            ("execution", select_default_route(&checked, &file)),
            (
                "verification",
                select_default_verification_route(&checked, &file),
            ),
        ] {
            let DefaultMirRoute::Canonical(program) = route else {
                panic!("ValuesSnapshotReplacement {label} route must pass its narrow preflight");
            };
            let receipt = program.route_receipt(
                mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE,
            );
            assert_eq!(
                receipt.profile,
                mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE
            );
            receipt
                .validate_against_program(&program)
                .expect("snapshot replacement receipt is bound to this MIR graph");
            assert!(mimi::core::mir::validate_ordinary_map_owned_any_v2_values_snapshot_replacement_route(
                &checked, &program
            )
            .is_ok());
        }

        let explicit = build_canonical_program(&checked, &file)
            .expect("explicit --mir builder must use the same snapshot replacement preflight");
        assert_eq!(
            explicit
                .route_receipt(route_receipt_profile_for_checked(
                    &checked,
                    "run-canonical-v1",
                ))
                .profile,
            mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE
        );
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    }

    #[test]
    fn keys_snapshot_uses_its_exact_receipt_for_default_and_explicit_routes() {
        let source = r#"
func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "answer", 41)
    let snapshot = keys(root)
    drop(empty)
    drop(root)
    let key = snapshot[0]
    drop(snapshot)
    println(key)
    drop(key)
    0
}
"#;
        let (checked, file) = checked(source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked)
            .ordinary_map_owned_any_v2;
        assert_eq!(
            admission,
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::KeysSnapshot
        );
        assert!(!admission.is_complete());
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        let profile = mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE;
        for default_profile in [
            "run-canonical-v1",
            mimi::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE,
            "verify-canonical-v1",
        ] {
            assert_eq!(
                route_receipt_profile_for_checked(&checked, default_profile),
                profile,
                "KeysSnapshot identity must be shared by {default_profile}"
            );
        }

        let materialized = materialize_canonical_route(&checked, &file)
            .expect("exact KeysSnapshot materializes Canonical MIR");
        assert!(ordinary_map_owned_any_v2_keys_snapshot_materialization_is_exact(&materialized));
        let materialized_receipt = materialized
            .ordinary_map_owned_any_v2_keys_snapshot_route_receipt
            .as_ref()
            .expect("materialization carries the exact KeysSnapshot route receipt");
        assert_eq!(materialized_receipt.profile, profile);
        materialized_receipt
            .validate_against_program(&materialized.program)
            .expect("materialized receipt is bound to the exact MIR graph");
        preflight_ordinary_map_owned_any_v2_keys_snapshot(&checked, &materialized.program)
            .expect("all exact KeysSnapshot consumers pass the narrow preflight");

        for (label, route) in [
            ("default run/build", select_default_route(&checked, &file)),
            (
                "default verify",
                select_default_verification_route(&checked, &file),
            ),
        ] {
            let program = match route {
                DefaultMirRoute::Canonical(program) => program,
                DefaultMirRoute::Legacy(reason) => {
                    panic!("{label} must not succeed through Legacy ({reason:?})")
                }
                DefaultMirRoute::Rejected(reason) => {
                    panic!("{label} must select the exact KeysSnapshot route: {reason}")
                }
            };
            let receipt = program.route_receipt(profile);
            assert_eq!(receipt.profile, profile);
            assert!(receipt.same_semantic_identity(materialized_receipt));
            receipt
                .validate_against_program(&program)
                .expect("default route carries its exact profile receipt");
        }

        let explicit = build_canonical_program(&checked, &file)
            .expect("explicit --mir must use the same KeysSnapshot preflight");
        let explicit_receipt = explicit.route_receipt(profile);
        assert_eq!(explicit_receipt.profile, profile);
        assert!(explicit_receipt.same_semantic_identity(materialized_receipt));
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    }

    #[test]
    fn keys_snapshot_rejects_broad_or_mismatched_receipts_without_legacy_retry() {
        let source = r#"
func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "answer", 41)
    let snapshot = keys(root)
    drop(empty)
    drop(root)
    let key = snapshot[0]
    drop(snapshot)
    println(key)
    drop(key)
    0
}
"#;
        let (checked_program, file) = checked(source);
        let program = build_canonical_program(&checked_program, &file)
            .expect("valid Checker KeysSnapshot fixture materializes");
        let broad_receipt =
            program.route_receipt(mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        let broad_error = preflight_ordinary_map_owned_any_v2_keys_snapshot_with_receipt(
            &checked_program,
            &program,
            &broad_receipt,
        )
        .expect_err("broad V2 receipt cannot authorize the KeysSnapshot route");
        assert!(broad_error.contains("exact route profile"), "{broad_error}");

        let unrelated_source = "func main() -> i32 { 0 }";
        let (unrelated_checked, _) = checked(unrelated_source);
        let keys_receipt =
            program.route_receipt(mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE);
        let mismatch_error = preflight_ordinary_map_owned_any_v2_keys_snapshot_with_receipt(
            &unrelated_checked,
            &program,
            &keys_receipt,
        )
        .expect_err("unrelated CheckedProgram cannot authorize this Keys MIR");
        assert!(
            mismatch_error.contains("Checker did not admit"),
            "{mismatch_error}"
        );

        let incomplete_source =
            include_str!("../../tests/fixtures/mir_ordinary_map_any_owner_v2_incomplete_keys.mimi");
        let (incomplete_checked, incomplete_file) = checked(incomplete_source);
        assert_eq!(
            mimi::core::mir::classify_canonical_mir_route_admission(&incomplete_checked)
                .ordinary_map_owned_any_v2,
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage
        );
        for (label, route) in [
            (
                "default execution",
                select_default_route(&incomplete_checked, &incomplete_file),
            ),
            (
                "default verification",
                select_default_verification_route(&incomplete_checked, &incomplete_file),
            ),
        ] {
            match route {
                DefaultMirRoute::Rejected(reason) => {
                    assert!(
                        reason.contains(mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE),
                        "{label} must report MIR-COVERAGE-001: {reason}"
                    );
                    assert!(
                        reason.contains("cannot use legacy"),
                        "{label} must explain that rejected coverage cannot retry Legacy: {reason}"
                    );
                }
                DefaultMirRoute::Legacy(reason) => {
                    panic!("{label} must reject without Legacy retry ({reason:?})")
                }
                DefaultMirRoute::Canonical(_) => {
                    panic!("{label} must not route incomplete Keys canonically")
                }
            }
        }
        assert!(
            build_canonical_program(&incomplete_checked, &incomplete_file).is_err(),
            "explicit --mir must reject incomplete Keys coverage"
        );
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    }

    #[test]
    fn values_without_the_exact_field_move_stays_fail_closed() {
        let source =
            include_str!("../../tests/fixtures/mir_ordinary_map_any_owner_v2_values_release.mimi");
        let (checked, file) = checked(source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked)
            .ordinary_map_owned_any_v2;
        assert!(admission.has_profile_signal());
        assert!(!admission.is_values_field_move());
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);

        match select_default_route(&checked, &file) {
            DefaultMirRoute::Rejected(reason) => assert!(
                !reason.contains("legacy"),
                "unsupported Values shape must not expose a hidden fallback: {reason}"
            ),
            DefaultMirRoute::Legacy(reason) => {
                panic!("recognized owner-v2 Values attempt escaped to Legacy ({reason:?})")
            }
            DefaultMirRoute::Canonical(_) => {
                panic!("unsupported Values shape must not select a production route")
            }
        }
        let explicit_error = build_canonical_program(&checked, &file)
            .expect_err("explicit --mir must reject unsupported Values settlement");
        assert!(
            explicit_error.contains(mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                || explicit_error.contains("not production-ready"),
            "explicit route must report its fail-closed coverage boundary: {explicit_error}"
        );
    }

    fn assert_checked_get_rejected(checked: &CheckedProgram, file: &File, context: &str) {
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(checked)
            .ordinary_map_owned_any_v2;
        assert!(
            admission.has_profile_signal(),
            "{context}: fixture must remain visible as an owner-v2 candidate: {admission:?}"
        );
        assert_ne!(
            admission,
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::CheckedGet,
            "{context}: unsupported shape must not enter the dedicated CheckedGet profile"
        );
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);

        match select_default_route(checked, file) {
            DefaultMirRoute::Rejected(reason) => assert!(
                !reason.contains("legacy"),
                "{context}: fail-closed route must not report a hidden fallback: {reason}"
            ),
            DefaultMirRoute::Legacy(reason) => {
                panic!("{context}: recognized owner-v2 attempt escaped to Legacy ({reason:?})")
            }
            DefaultMirRoute::Canonical(_) => {
                panic!("{context}: unsupported owner-v2 shape selected an executable route")
            }
        }
        let explicit_error = build_canonical_program(checked, file)
            .expect_err("explicit --mir construction must reject unsupported owner-v2 shapes");
        assert!(
            explicit_error.contains(mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                || explicit_error.contains("CheckedGet"),
            "{context}: explicit --mir must expose its coverage rejection: {explicit_error}"
        );
    }

    fn assert_checked_get_source_rejected(source: &str, context: &str) {
        let tokens = mimi::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = mimi::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        match mimi::core::check_program(&file) {
            Ok(checked) => assert_checked_get_rejected(&checked, &file, context),
            Err(diagnostics) => assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code.as_deref() == Some("E0200")),
                "{context}: Checker rejected for an unexpected reason: {diagnostics:?}"
            ),
        }
    }

    fn assert_checker_or_route_rejects_exactly_once_shape(
        source: &str,
        expected_diagnostic: &str,
        context: &str,
    ) {
        let tokens = mimi::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = mimi::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        match mimi::core::check_program(&file) {
            Err(diagnostics) => assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code.as_deref() == Some(expected_diagnostic)),
                "{context}: Checker rejected for an unexpected reason: {diagnostics:?}"
            ),
            Ok(checked) => assert_checked_get_rejected(&checked, &file, context),
        }
    }

    #[test]
    fn checked_get_static_keys_and_i32_string_label_payloads_pass_default_and_explicit_preflight() {
        use mimi::core::mir::OrdinaryMapOwnedAnyV2UnpackPayloadSource as PayloadSource;

        let cases = [
            (
                "",
                "\"an\" + \"swer\"",
                "\"ans\" + \"wer\"",
                b"answer".as_slice(),
                "73",
                "i32",
                "bool",
                "found",
                "",
                true,
                PayloadSource::SelectedSetPayload,
            ),
            (
                "",
                "\"answer\"",
                "\"answer\"",
                b"answer".as_slice(),
                r#""owned\0 payload λ""#,
                "string",
                "bool",
                "found",
                "drop(payload)",
                true,
                PayloadSource::SelectedSetPayload,
            ),
            (
                "type Label { text: string }",
                "\"label\"",
                "\"label\"",
                b"label".as_slice(),
                r#"Label { text: "owned label λ" }"#,
                "Label",
                "bool",
                "found",
                "drop(payload)",
                true,
                PayloadSource::SelectedSetPayload,
            ),
            (
                "",
                "\"an\" + \"swer\"",
                "\"missing\"",
                b"missing".as_slice(),
                "17",
                "i32",
                "bool",
                "found",
                "",
                false,
                PayloadSource::CanonicalI32Miss,
            ),
        ];
        for (
            index,
            (
                preamble,
                set_key,
                get_key,
                expected_key,
                set_value,
                unpack_type,
                result_type,
                result_expression,
                payload_cleanup,
                expected_found,
                expected_source,
            ),
        ) in cases.into_iter().enumerate()
        {
            let source = checked_get_observed_source(
                preamble,
                set_key,
                get_key,
                set_value,
                unpack_type,
                result_type,
                result_expression,
                payload_cleanup,
            );
            let (checked, file) = checked(&source);
            let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2;
            assert_eq!(
                admission,
                mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::CheckedGet,
                "static literal/concat key case {index}"
            );
            assert_eq!(
                route_receipt_profile_for_checked(&checked, "run-canonical-v1"),
                mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE,
                "run must use the dedicated CheckedGet receipt profile"
            );
            for default_profile in [
                "run-canonical-v1",
                mimi::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE,
                "verify-canonical-v1",
            ] {
                assert_eq!(
                    route_receipt_profile_for_checked(&checked, default_profile),
                    mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE,
                    "CheckedGet must select one raw receipt profile for {default_profile}"
                );
            }
            assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);

            let expected_key_digest = blake3::hash(expected_key).to_hex().to_string();

            let default_program = match select_default_route(&checked, &file) {
                DefaultMirRoute::Canonical(program) => program,
                DefaultMirRoute::Legacy(reason) => panic!(
                    "CheckedGet static literal/concat case {index} unexpectedly selected Legacy: {reason:?}"
                ),
                DefaultMirRoute::Rejected(reason) => panic!(
                    "CheckedGet static literal/concat case {index} failed default-route consumer preflight: {reason}"
                ),
            };
            let route_profile = route_receipt_profile_for_checked(&checked, "run-canonical-v1");
            let route_receipt = default_program.route_receipt(route_profile);
            assert_eq!(
                route_receipt.profile,
                mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE
            );
            let get_receipt = default_program
                .ordinary_map_owned_any_v2_get_any_result_receipts()
                .expect("Checker-sealed GetAny key receipt")
                .into_iter()
                .next()
                .expect("one Checker GetAny result");
            assert_eq!(
                get_receipt.checker_key_static_digest.as_deref(),
                Some(expected_key_digest.as_str()),
                "GetAny receipt must preserve the exact Checker-derived static key digest"
            );
            assert!(get_receipt.checker_key_local.is_none());
            route_receipt
                .validate_against_program(&default_program)
                .expect("the dedicated route receipt is bound to this MIR graph");
            let unpack = default_program
                .ordinary_map_owned_any_v2_unpack_result_receipts()
                .expect("CheckedGet unpack receipt")
                .into_iter()
                .next()
                .expect("one checked pair.1 conversion");
            assert_eq!(unpack.payload_source, expected_source);
            let selection = default_program
                .ordinary_map_owned_any_v2_payload_selection_receipt()
                .expect("Checker payload selection receipt")
                .expect("one Checker-selected Set payload");
            let selected_set_type = &selection.entries[0].checker_type;
            let i32_type = default_program
                .type_catalog()
                .iter()
                .find_map(|(type_id, descriptor)| {
                    (descriptor.kind
                        == mimi::core::mir::types::MirTypeKind::Primitive(
                            mimi::core::PrimitiveType::I32,
                        ))
                    .then(|| type_id.clone())
                })
                .expect("the checked MIR type catalog contains canonical i32");
            let expected_unpack_type = if expected_key == b"missing" {
                i32_type
            } else {
                selected_set_type.clone()
            };
            assert_eq!(unpack.conversion_to, expected_unpack_type);

            let reference =
                mimi::core::mir::reference::MirReferenceInterpreter::new(&default_program)
                    .execute_with_output(&mimi::core::NodeId("function:main".into()), &[])
                    .expect("Reference observes the selected payload after root and pair Drops");
            assert_eq!(
                reference.value,
                mimi::core::mir::reference::MirRuntimeValue::Bool(expected_found),
                "the selected presence or payload observation must be exact"
            );

            let bytecode = mimi::interp::bytecode::compile_mir_program_with_route_receipt(
                &default_program,
                &route_receipt,
            )
            .expect("Bytecode capability accepts the CheckedGet profile receipt");
            assert!(
                bytecode.ast.is_none(),
                "canonical bytecode remains AST-free"
            );

            let explicit_program = build_canonical_program(&checked, &file)
                .expect("explicit --mir builder must use the same CheckedGet preflight");
            let explicit_route_profile = route_receipt_profile_for_checked(
                &checked,
                mimi::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE,
            );
            assert_eq!(
                explicit_program
                    .route_receipt(explicit_route_profile)
                    .profile,
                mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE
            );
            preflight_ordinary_map_owned_any_v2_checked_get(&checked, &explicit_program)
                .expect("MIR verifier, Bytecode, native capability, and verifier pass preflight");

            let verification_program =
                match select_default_verification_route(&checked, &file) {
                    DefaultMirRoute::Canonical(program) => program,
                    DefaultMirRoute::Legacy(reason) => panic!(
                        "CheckedGet static literal/concat case {index} unexpectedly selected Legacy for verify: {reason:?}"
                    ),
                    DefaultMirRoute::Rejected(reason) => panic!(
                        "CheckedGet static literal/concat case {index} failed verification-route preflight: {reason}"
                    ),
                };
            let verify_route_profile =
                route_receipt_profile_for_checked(&checked, "verify-canonical-v1");
            let verify_route_receipt = verification_program.route_receipt(verify_route_profile);
            assert_eq!(
                verify_route_receipt.profile,
                mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE,
                "verify must use the same raw CheckedGet receipt profile"
            );
            verify_route_receipt
                .validate_against_program(&verification_program)
                .expect("verification receipt is bound to its selected MIR graph");
        }
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    }

    #[test]
    fn checked_get_rejects_runtime_keys_type_mismatch_helpers_and_values() {
        let variable_key = r#"
func runtime_key() -> string { "answer" }
func main() -> bool {
    let empty = map_new()
    let root = map_set(empty, runtime_key(), 0)
    let pair = map_get(root, runtime_key())
    drop(empty)
    drop(root)
    let found = pair.0
    let payload = unpack_i32(pair.1)
    drop(pair)
    found
}
"#;
        assert_checked_get_source_rejected(&variable_key, "runtime variable key");

        let helper_key = r#"
func runtime_key() -> string { "answer" }
func main() -> bool {
    let empty = map_new()
    let root = map_set(empty, "answer", 0)
    let pair = map_get(root, runtime_key())
    drop(empty)
    drop(root)
    let found = pair.0
    let payload = unpack_i32(pair.1)
    drop(pair)
    found
}
"#;
        assert_checked_get_source_rejected(&helper_key, "helper-computed key");

        let wrong_payload = checked_get_source("\"answer\"", "\"answer\"", "\"text\"", "i32");
        assert_checked_get_source_rejected(
            &wrong_payload,
            "matching key with String-to-i32 payload mismatch",
        );

        let values = r#"
type Label { text: string }
func main() -> i32 {
    let label = Label { text: "owned values payload" }
    let empty = map_new()
    let root = map_set(empty, "answer", label)
    let snapshot = values(root)
    drop(snapshot)
    drop(empty)
    drop(root)
    drop(label)
    0
}
"#;
        assert_checked_get_source_rejected(&values, "Values owner shape");
    }

    #[test]
    fn checked_get_requires_exactly_one_explicit_pair_settlement() {
        let double_settlement = checked_get_source("\"answer\"", "\"answer\"", "0", "i32").replace(
            "    drop(pair)\n    found",
            "    drop(pair)\n    drop(pair)\n    found",
        );
        assert_checker_or_route_rejects_exactly_once_shape(
            &double_settlement,
            "E0200",
            "double pair settlement",
        );

        let missing_settlement = checked_get_source("\"answer\"", "\"answer\"", "0", "i32")
            .replace("    drop(pair)\n    found", "    found");
        assert_checker_or_route_rejects_exactly_once_shape(
            &missing_settlement,
            "E0200",
            "missing pair settlement",
        );
    }

    #[test]
    fn computed_key_get_uses_its_exact_route_receipt_profile_for_each_cli_entry() {
        let source = include_str!(
            "../../tests/fixtures/mir_ordinary_map_any_owner_v2_computed_key_get.mimi"
        );
        let (checked, _) = checked(source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked)
            .ordinary_map_owned_any_v2;
        assert_eq!(
            admission,
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ComputedKeyGet
        );
        assert!(admission.is_candidate());
        for default_profile in [
            "run-canonical-v1",
            mimi::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE,
            "verify-canonical-v1",
        ] {
            assert_eq!(
                route_receipt_profile_for_checked(&checked, default_profile),
                mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE,
                "ComputedKeyGet must select its dedicated receipt for {default_profile}"
            );
        }
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    }

    #[test]
    fn computed_string_key_owner_chain_uses_its_exact_route_receipt_profile_for_each_cli_entry() {
        let source = include_str!(
            "../../tests/fixtures/mir_ordinary_map_any_owner_v2_dynamic_string_keys.mimi"
        );
        let (checked, file) = checked(source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked)
            .ordinary_map_owned_any_v2;
        assert_eq!(
            admission,
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ComputedStringKeyOwnerChain
        );
        assert!(admission.is_candidate());
        for default_profile in [
            "run-canonical-v1",
            mimi::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE,
            "verify-canonical-v1",
        ] {
            assert_eq!(
                route_receipt_profile_for_checked(&checked, default_profile),
                mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE,
                "ComputedStringKeyOwnerChain must select its dedicated receipt for {default_profile}"
            );
        }
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Canonical(_)
        ));
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    }

    #[test]
    fn zero_miss_i32_uses_its_exact_profile_and_fails_closed_without_materialization_receipt() {
        let source =
            include_str!("../../tests/fixtures/mir_ordinary_map_any_owner_v2_zero_miss_i32.mimi");
        let (checked, file) = checked(source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
        assert!(admission.ordinary_map_owned_any_v2_zero_miss_i32());
        assert_eq!(
            admission.ordinary_map_owned_any_v2,
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ZeroMissI32
        );

        for default_profile in [
            "run-canonical-v1",
            mimi::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE,
            "verify-canonical-v1",
        ] {
            assert_eq!(
                route_receipt_profile_for_checked(&checked, default_profile),
                mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE,
                "ZeroMissI32 must select its dedicated route receipt for {default_profile}"
            );
        }

        let materialized = materialize_canonical_route(&checked, &file)
            .expect("ZeroMissI32 candidate materializes its canonical route");
        assert!(ordinary_map_owned_any_v2_zero_miss_i32_materialization_is_exact(&materialized));
        let expected_receipt = materialized
            .program
            .route_receipt(mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE);
        assert_eq!(
            materialized
                .ordinary_map_owned_any_v2_zero_miss_i32_route_receipt
                .as_ref(),
            Some(&expected_receipt)
        );

        let mut missing_receipt = materialized.clone();
        missing_receipt.ordinary_map_owned_any_v2_zero_miss_i32_route_receipt = None;
        assert!(
            !ordinary_map_owned_any_v2_zero_miss_i32_materialization_is_exact(&missing_receipt),
            "a missing dedicated receipt must not authorize fallback or a generic route"
        );
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    }

    #[test]
    fn zero_miss_i32_default_route_accepts_only_the_standard_library_prelude() {
        let source =
            include_str!("../../tests/fixtures/mir_ordinary_map_any_owner_v2_zero_miss_i32.mimi");
        let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mir_ordinary_map_any_owner_v2_zero_miss_i32.mimi");
        let load_with_prelude = |source: &str| {
            let tokens = mimi::lexer::Lexer::new(source).tokenize().expect("lex");
            let mut file = mimi::loader::parser_for_path(tokens, &fixture_path)
                .expect("fixture parser setup")
                .parse_file()
                .expect("parse");
            mimi::loader::merge_prelude_into(&mut file);
            let checked = mimi::core::check_program(&file).expect("check prelude graph");
            (checked, file)
        };

        let (checked, file) = load_with_prelude(source);
        assert_eq!(
            mimi::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ZeroMissI32
        );
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Canonical(_)
        ));

        let helper_source = format!("func helper() -> i32 {{ 1 }}\n{source}");
        let (checked, file) = load_with_prelude(&helper_source);
        assert_eq!(
            mimi::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::CompleteCoverage,
            "an additional user callable must not be hidden by excluding prelude bodies"
        );
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Rejected(reason)
                if reason.contains("shared production-readiness gate is closed")
        ));
    }

    #[test]
    fn scalar_ffi_composition_rejects_narrow_ordinary_map_any_v2_profiles() {
        let computed_key_get = include_str!(
            "../../tests/fixtures/mir_ordinary_map_any_owner_v2_computed_key_get.mimi"
        );
        let computed_key_source = format!(
            r#"
extern "C" {{ func scalar_probe(value: i32) -> i32; }}
func unused_scalar_ffi_helper() -> i32 {{ scalar_probe(0) }}
{computed_key_get}
"#
        );
        let checked_get_source = r#"
extern "C" { func scalar_probe(value: i32) -> i32; }
func unused_scalar_ffi_helper() -> i32 { scalar_probe(0) }
func main() -> bool {
    let empty = map_new()
    let root = map_set(empty, "answer", 41)
    let pair = map_get(root, "answer")
    drop(empty)
    drop(root)
    let found = pair.0
    let payload = unpack_i32(pair.1)
    drop(pair)
    found
}
"#;
        let cases = [
            (
                "ComputedKeyGet",
                computed_key_source.as_str(),
                mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ComputedKeyGet,
            ),
            (
                "CheckedGet",
                checked_get_source,
                mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::CheckedGet,
            ),
        ];

        for (label, source, expected) in cases {
            let (checked, file) = checked(source);
            let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
            assert!(admission.scalar_ffi, "{label} keeps the FFI signal");
            assert_eq!(admission.ordinary_map_owned_any_v2, expected);
            assert!(
                scalar_ffi_map_owner_composition_failure(admission).is_some(),
                "the semantic V2 candidate discriminator catches {label}"
            );

            let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
                panic!("{label} plus scalar FFI must not select an execution route");
            };
            assert!(reason.contains("cannot be combined"), "{label}: {reason}");
            assert!(reason.contains("scalar FFI"), "{label}: {reason}");

            let DefaultMirRoute::Rejected(reason) =
                select_default_verification_route(&checked, &file)
            else {
                panic!("{label} plus scalar FFI must not select a verification route");
            };
            assert!(reason.contains("Map/Any owner"), "{label}: {reason}");

            let explicit_error = build_canonical_program(&checked, &file)
                .expect_err("explicit canonical build shares the FFI/Map gate");
            assert!(
                explicit_error.contains("cannot be combined"),
                "{label}: {explicit_error}"
            );
        }
    }

    #[test]
    fn checked_get_validator_rejects_forged_key_and_payload_selection_receipts() {
        let source = checked_get_source("\"ans\" + \"wer\"", "\"an\" + \"swer\"", "0", "i32");
        let (checked, _file) = checked(&source);
        assert_eq!(
            mimi::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::CheckedGet
        );
        let program = MirProgram::from_checked_program(&checked)
            .expect("Checker CheckedGet source materializes independently receipted MIR");
        mimi::core::mir::validate_ordinary_map_owned_any_v2_checked_get_route(&program)
            .expect("untampered CheckedGet receipts pass their dedicated validator");

        let mut forged_get = program
            .ordinary_map_owned_any_v2_get_any_result_receipts()
            .expect("Core GetAny result receipt")
            .into_iter()
            .next()
            .expect("one GetAny result");
        forged_get.checker_key_static_digest =
            Some(blake3::hash(b"forged key").to_hex().to_string());
        forged_get
            .validate()
            .expect("forged digest remains locally well-formed");
        assert!(
            forged_get.validate_against_program(&program).is_err(),
            "GetAny validator must compare the key digest back to Checker and MIR"
        );

        let mut forged_selection = program
            .ordinary_map_owned_any_v2_payload_selection_receipt()
            .expect("payload selection receipt derives")
            .expect("one Set selection exists");
        let forged_type_digest = blake3::hash(b"forged payload TypeDesc")
            .to_hex()
            .to_string();
        forged_selection.entries[0].type_desc_digest = forged_type_digest.clone();
        forged_selection.selected_types[0].type_desc_digest = forged_type_digest;
        forged_selection.selection_digest =
            blake3::hash(forged_selection.canonical_text().as_bytes())
                .to_hex()
                .to_string();
        forged_selection
            .validate()
            .expect("self-consistent forged selection has a valid local digest");
        assert!(
            forged_selection.validate_against_program(&program).is_err(),
            "payload selection validator must recompute its Checker/MIR TypeDesc evidence"
        );
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    }

    fn checked_module_alias_graph(main_source: &str) -> (CheckedProgram, File, std::path::PathBuf) {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "mimi-canonical-route-module-alias-{}-{unique}",
            std::process::id()
        ));
        let apps = directory.join("apps");
        std::fs::create_dir_all(&apps).expect("create module alias directory");
        std::fs::write(
            apps.join("left.mimi"),
            "pub func parse(value: i32) -> i32 { value + 1 }\n",
        )
        .expect("write aliased module export");
        let main_path = directory.join("main.mimi");
        std::fs::write(&main_path, main_source).expect("write module alias entry");
        let tokens = mimi::lexer::Lexer::new(main_source)
            .tokenize()
            .expect("lex module alias entry");
        let main_file = mimi::loader::parser_for_path(tokens, &main_path)
            .expect("module alias parser setup")
            .parse_file()
            .expect("parse module alias entry");
        let mut loader = mimi::loader::ModuleLoader::new(directory.clone());
        loader
            .load_main_with_file(&main_path, main_file)
            .expect("load module alias graph");
        let mut merged_file = loader.merge_all().expect("merge module alias graph");
        mimi::loader::merge_prelude_into(&mut merged_file);
        let checked = mimi::core::check_program(&merged_file).expect("check module alias graph");
        (checked, merged_file, directory)
    }

    #[test]
    fn module_alias_with_actor_proof_boundary_rejects_instead_of_using_legacy() {
        let actor_prefix = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/real_world/actor_nested_func_capture.mimi"
        ))
        .split("\nfunc main() -> i32 {")
        .next()
        .expect("Actor fixture has its main entry");
        let main_source = format!(
            "use apps::left as left;\n{actor_prefix}\n\
             func guarded(value: i32) -> i32 {{\n\
                 requires: value >= 0\n\
                 ensures: result == value\n\
                 value\n\
             }}\n\
             func main() -> i32 {{\n\
                 let counter = Counter.spawn()\n\
                 let imported = left.parse(1)\n\
                 let actor_value = counter.bump(imported)\n\
                 guarded(actor_value)\n\
                 0\n\
             }}"
        );
        let (checked, file, directory) = checked_module_alias_graph(&main_source);
        assert!(
            checked.call_sites().values().any(|site| {
                matches!(site.kind, mimi::core::ResolvedCallKind::Function)
                    && site.target_function.is_some()
                    && site.callee.contains("::")
            }),
            "fixture must contain a resolved module alias call"
        );
        assert!(
            mimi::core::mir::scalar_actor_contract_boundary_reason(&checked).is_some(),
            "fixture must exercise the Actor proof-compatibility boundary"
        );
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("module alias plus Actor proof obligations must not enter Legacy");
        };
        assert!(reason.contains("MIR-COVERAGE-001"), "{reason}");
        assert!(reason.contains("module-qualified"), "{reason}");
        assert!(!reason.contains("legacy"), "{reason}");
        std::fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn module_alias_with_plain_scalar_native_preflight_rejects_instead_of_legacy() {
        let main_source = r#"
            use apps::left as left;

            func main() -> i32 {
                println(left.parse(1))
                left.parse(2) << 1
            }
        "#;
        let (checked, file, directory) = checked_module_alias_graph(main_source);
        let materialized = mimi::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("imported alias graph should materialize before consumer preflights");
        assert_eq!(
            materialized.admission.collection,
            mimi::core::mir::ScalarCollectionAdmission::MixedCoverage,
            "the imported source keeps the collection scan mixed"
        );
        assert!(!materialized.materialized_collection_candidate);
        assert!(!materialized.materialized_collection_operation_candidate);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!(
                "an aliased imported graph outside the scalar MIR preflight must not enter Legacy"
            );
        };
        assert!(reason.contains("native MIR preflight"), "{reason}");
        assert!(reason.contains("module-qualified"), "{reason}");
        assert!(!reason.contains("legacy"), "{reason}");
        std::fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn canonical_actor_default_and_explicit_share_original_fixture_graphs() {
        for source in [
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/real_world/actor_nested_func_capture.mimi"
            )),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/real_world/actor_nested_func_shadow.mimi"
            )),
        ] {
            let (program, mut file) = checked(source);
            drop(program);
            mimi::loader::merge_prelude_into(&mut file);
            let program = mimi::core::check_program(&file).expect("same production prelude graph");
            let DefaultMirRoute::Canonical(default) = select_default_route(&program, &file) else {
                panic!("original Actor fixture must have a complete default route");
            };
            let DefaultMirRoute::Canonical(verification) =
                select_default_verification_route(&program, &file)
            else {
                panic!("Actor NoContracts verify route must be canonical");
            };
            let direct = build_canonical_program(&program, &file).unwrap();
            let shared = materialize_canonical_route(&program, &file).unwrap();
            assert!(shared.admission.scalar_actor);
            assert_eq!(default.canonical_digest(), direct.canonical_digest());
            assert_eq!(default.canonical_digest(), verification.canonical_digest());
            assert_eq!(
                default.canonical_digest(),
                shared.program.canonical_digest()
            );
            assert_eq!(
                default.route_receipt(mimi::core::mir::SCALAR_ACTOR_ISLAND),
                direct.route_receipt(mimi::core::mir::SCALAR_ACTOR_ISLAND)
            );
        }
    }

    #[test]
    fn unsupported_nested_actor_candidate_rejects_before_legacy_route() {
        let source = r#"
actor Counter {
    base: i32 = 0;

    func bump(delta: i32) -> i32 {
        // The nested helper is deliberately outside the finite i32
        // environment ABI.  A checked Actor candidate must fail closed here;
        // it must never be handed to the legacy actor emitter.
        func unsupported(value: i64) -> i64 { value + 1 }
        self.base = self.base + delta
        self.base
    }
}

func main() -> i32 {
    let c = Counter.spawn()
    c.bump(1)
    0
}
"#;
        let (program, file) = checked(source);
        assert!(mimi::core::mir::has_scalar_actor_nested_callable_candidate(
            &program
        ));
        assert!(!mimi::core::mir::classify_canonical_mir_route_admission(&program).scalar_actor);
        let route = select_default_route(&program, &file);
        let DefaultMirRoute::Rejected(reason) = route else {
            panic!("unsupported nested Actor candidate must not retain a legacy route");
        };
        assert!(reason.contains(mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE));
        assert!(reason.contains("legacy Actor fallback is disabled"));
    }

    #[test]
    fn canonical_actor_dormant_contracts_use_full_default_verification_route() {
        for predicate in ["result == x", "result != x"] {
            let source = format!(
                "{}\nfunc guarded(x: i32) -> i32 {{\nrequires: x > 0\nensures: {predicate}\nx\n}}",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/real_world/actor_nested_func_capture.mimi"
                ))
            );
            let (program, file) = checked(&source);
            assert!(matches!(
                select_default_route(&program, &file),
                DefaultMirRoute::Legacy(LegacyRouteReason::ActorProofObligationsCompatibility)
            ));
            assert!(matches!(
                select_default_verification_route(&program, &file),
                DefaultMirRoute::Legacy(LegacyRouteReason::ActorProofObligationsCompatibility)
            ));
            assert!(build_canonical_program(&program, &file)
                .unwrap_err()
                .contains("cannot discard proof obligations"));
            let results = mimi::verifier::verify_checked_dual(
                &program,
                "default-actor-whole-proof-census".into(),
            )
            .unwrap();
            let guarded = results
                .iter()
                .find(|result| result.func_name == "guarded")
                .expect("real default contract result");
            if predicate == "result == x" {
                assert_eq!(guarded.status, mimi::verifier::VerifStatus::Proven);
            } else {
                assert_ne!(guarded.status, mimi::verifier::VerifStatus::Proven);
            }
        }
    }

    #[test]
    fn actor_contract_compatibility_cannot_bypass_map_ffi_execution_gate() {
        const FFI: &str = r#"
extern "C" {
    func scalar_probe(value: i32) -> i32
        requires: value >= 0
        ensures: result == value + 1;
}
"#;
        const USED_ACTOR_MAIN: &str = r#"
func main() -> i32 {
    let counter = Counter.spawn()
    let actor_value = counter.bump(5)
    let root = map_new()
    let updated = map_set(root, "answer", scalar_probe(actor_value))
    let size = map_size(updated)
    drop(updated)
    size
}
"#;
        const UNUSED_ACTOR_MAIN: &str = r#"
func main() -> i32 {
    let root = map_new()
    let updated = map_set(root, "answer", scalar_probe(41 as i32))
    let size = map_size(updated)
    drop(updated)
    size
}
"#;
        let actor_prefix = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/real_world/actor_nested_func_capture.mimi"
        ))
        .split("\nfunc main() -> i32 {")
        .next()
        .expect("original Actor fixture has its main entry");

        for (label, main) in [
            ("used-actor", USED_ACTOR_MAIN),
            ("unused-actor", UNUSED_ACTOR_MAIN),
        ] {
            let source = format!("{actor_prefix}\n{FFI}\n{main}");
            let (program, file) = checked(&source);
            let admission = mimi::core::mir::classify_canonical_mir_route_admission(&program);
            assert!(admission.scalar_ffi, "{label}: {admission:?}");
            assert!(
                admission.map_root_complete()
                    || admission.map_any_owner_complete()
                    || admission.ordinary_map_owner_lifecycle_i32_string_v1_complete()
                    || admission.ordinary_map_owned_get_i32_string_v1_complete(),
                "{label} must have a complete independent Map owner receipt: {admission:?}"
            );
            assert!(
                mimi::core::mir::scalar_actor_contract_boundary_reason(&program).is_some(),
                "{label} must exercise the Actor proof-compatibility boundary"
            );

            let DefaultMirRoute::Rejected(reason) = select_default_route(&program, &file) else {
                panic!("{label} Map+FFI composition must not fall through Actor compatibility");
            };
            assert!(reason.contains("cannot be combined"), "{label}: {reason}");
            assert!(reason.contains("scalar FFI"), "{label}: {reason}");

            let DefaultMirRoute::Rejected(reason) =
                select_default_verification_route(&program, &file)
            else {
                panic!("{label}: verification must not hide Map+FFI behind Actor compatibility");
            };
            assert!(reason.contains("Map/Any owner"), "{label}: {reason}");
        }
    }

    #[test]
    fn actor_contract_with_ordinary_map_cannot_hide_owner_behind_compatibility() {
        let actor_prefix = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/real_world/actor_nested_func_capture.mimi"
        ))
        .split("\nfunc main() -> i32 {")
        .next()
        .expect("original Actor fixture has its main entry");
        let source = format!(
            "{actor_prefix}\n\
                func guarded(value: i32) -> i32 {{\n\
                    requires: value >= 0\n\
                    ensures: result == value\n\
                    value\n\
                }}\n\
                func main() -> i32 {{\n\
                    let root = map_new()\n\
                    let updated = map_set(root, \"answer\", guarded(1))\n\
                    let size = map_size(updated)\n\
                    drop(updated)\n\
                    size\n\
                }}"
        );
        let (program, file) = checked(&source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&program);
        assert!(
            admission.map_any_owner.is_candidate()
                || admission.map_root.is_candidate()
                || admission
                    .ordinary_map_owner_lifecycle_i32_string_v1
                    .has_profile_signal()
                || admission
                    .ordinary_map_owned_get_i32_string_v1
                    .has_profile_signal(),
            "fixture must expose a checker-owned Map/Any candidate: {admission:?}"
        );
        assert!(
            mimi::core::mir::scalar_actor_contract_boundary_reason(&program).is_some(),
            "fixture must exercise the Actor proof-compatibility boundary"
        );
        for route in [
            select_default_route(&program, &file),
            select_default_verification_route(&program, &file),
        ] {
            let DefaultMirRoute::Rejected(reason) = route else {
                panic!("Actor contract + ordinary Map must fail closed: {route:?}");
            };
            assert!(reason.contains(mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE));
            assert!(reason.contains("Map/Any owner"));
            assert!(!reason.contains("legacy"));
        }
    }

    #[test]
    fn unsupported_actor_export_wrapper_rejects_before_legacy_route() {
        let source = format!(
            "{}\nextern \"C\" func exported_probe(value: i32) -> i32 {{ value + 1 }}",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/real_world/actor_nested_func_capture.mimi"
            ))
        );
        let (program, file) = checked(&source);
        assert!(!mimi::core::mir::classify_canonical_mir_route_admission(&program).scalar_actor);
        let route = select_default_route(&program, &file);
        assert!(
            matches!(route, DefaultMirRoute::Rejected(ref reason) if reason.contains("MIR-COVERAGE-001"))
        );
        let verification = select_default_verification_route(&program, &file);
        assert!(
            matches!(verification, DefaultMirRoute::Rejected(ref reason) if reason.contains("MIR-COVERAGE-001"))
        );
        assert!(
            build_canonical_program(&program, &file).is_err(),
            "explicit Actor MIR must not discard the unsupported exported wrapper"
        );
    }

    #[test]
    fn migrated_route_candidate_rejection_priority_is_stable() {
        let all_copy_candidates = CopyCandidateFlags {
            option_i32: true,
            option_bool: true,
            option_i64: true,
            option_f64: true,
            result_i32: true,
        };
        let rejected_reason = |flow, collection, record, option_string, copy, reason: &str| {
            match reject_migrated_candidates_with_copy(
                flow,
                collection,
                record,
                option_string,
                copy,
                reason,
            ) {
                DefaultMirRoute::Rejected(message) => message,
                route => panic!("candidate combination must reject, got {route:?}"),
            }
        };

        assert!(rejected_reason(
            true,
            true,
            true,
            true,
            all_copy_candidates,
            "generic variant predicate"
        )
        .starts_with("S8 Flow transition candidate"));
        assert!(
            rejected_reason(false, true, true, true, all_copy_candidates, "plain reason")
                .starts_with("S11 scalar collection candidate")
        );
        assert!(rejected_reason(
            false,
            false,
            true,
            true,
            all_copy_candidates,
            "generic variant predicate"
        )
        .starts_with("generic variant predicate candidate"));
        assert!(rejected_reason(
            false,
            false,
            true,
            true,
            all_copy_candidates,
            "plain reason"
        )
        .starts_with("S0 flat Copy record candidate"));
        assert!(rejected_reason(
            false,
            false,
            false,
            true,
            all_copy_candidates,
            "plain reason"
        )
        .starts_with("S30 non-Copy Option<string> variant candidate"));

        let copy_rejections = [
            (all_copy_candidates, "S114 Copy Option<i32>"),
            (
                CopyCandidateFlags {
                    option_i32: false,
                    ..all_copy_candidates
                },
                "S115 Copy Option<bool>",
            ),
            (
                CopyCandidateFlags {
                    option_i32: false,
                    option_bool: false,
                    ..all_copy_candidates
                },
                "S116 Copy Option<i64>",
            ),
            (
                CopyCandidateFlags {
                    option_i32: false,
                    option_bool: false,
                    option_i64: false,
                    ..all_copy_candidates
                },
                "S117 Copy Option<f64>",
            ),
            (
                CopyCandidateFlags {
                    option_i32: false,
                    option_bool: false,
                    option_i64: false,
                    option_f64: false,
                    ..all_copy_candidates
                },
                "S125 Copy Result<i32, i32>",
            ),
        ];
        for (copy_candidates, expected) in copy_rejections {
            assert!(
                rejected_reason(false, false, false, false, copy_candidates, "plain reason")
                    .starts_with(expected)
            );
        }

        assert!(matches!(
            reject_migrated_candidates_with_copy(
                false,
                false,
                false,
                false,
                CopyCandidateFlags::default(),
                "unrecognized candidate",
            ),
            DefaultMirRoute::Legacy(LegacyRouteReason::OutsideMigratedProfile)
        ));
    }

    #[test]
    fn scalar_ffi_default_route_covers_bindings_branches_and_helpers() {
        for body in [
            "foreign(42 as i64)",
            "let x = foreign(42 as i64); foreign(x)",
            "if true { foreign(42 as i64) } else { foreign(9 as i64) }",
            "let x = helper(42 as i64); println(x); foreign(x)",
            "let x = helper(42 as i64); let y = foreign(x); y",
            "if false { helper(9 as i64) } else { foreign(42 as i64) }",
        ] {
            let source = format!(
                r#"extern "C" {{ func foreign(x: i64) -> i64; }}
                func helper(x: i64) -> i64 {{ foreign(x) }}
                func main() -> i64 {{ {body} }}"#
            );
            let (checked, file) = checked(&source);
            let route = select_default_route(&checked, &file);
            assert!(
                matches!(route, DefaultMirRoute::Canonical(_)),
                "{body}: {route:?}"
            );
        }
    }

    #[test]
    fn map_any_owner_default_route_admits_no_contract_execution_with_stdout() {
        let source = r#"
            func main() -> i32 {
                let root = map_new()
                let updated = map_set(root, "answer", 42)
                let size = map_size(updated)
                println(size)
                drop(updated)
                0
            }
        "#;
        let (checked, file) = checked(source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
        assert_eq!(
            admission.map_any_owner,
            mimi::core::mir::MapAnyOwnerAdmission::CompleteCoverage
        );
        for default_profile in [
            "run-canonical-v1",
            mimi::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE,
            "verify-canonical-v1",
        ] {
            assert_eq!(
                route_receipt_profile_for_checked(&checked, default_profile),
                mimi::core::mir::MAP_ANY_OWNER_V1_ISLAND,
                "MapAnyOwner V1 must preserve its exact receipt profile for {default_profile}"
            );
        }
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("complete MapRoot receipt must enter the default canonical route");
        };
        assert!(
            program
                .functions()
                .values()
                .all(|function| function.contracts.is_empty()),
            "fixture must exercise execution with no source contracts"
        );
        assert_eq!(
            program
                .route_receipt(mimi::core::mir::MAP_ANY_OWNER_V1_ISLAND)
                .profile,
            mimi::core::mir::MAP_ANY_OWNER_V1_ISLAND
        );
        let main = mimi::core::NodeId("function:main".into());
        let reference = mimi::core::mir::reference::MirReferenceInterpreter::new(&program)
            .execute_with_output(&main, &[])
            .expect("reference no-contract execution");
        assert_eq!(reference.output, "1\n");
        assert_eq!(
            reference.value,
            mimi::core::mir::reference::MirRuntimeValue::Int(0)
        );
    }

    #[test]
    fn map_any_owner_default_route_admits_static_contains_and_retains_the_root() {
        let source = r#"
            func main() -> i32 {
                let root = map_new()
                let inserted = map_set(root, "answer", 42)
                let present = has_key(inserted, "answer")
                let absent = has_key(inserted, "missing")
                let size = map_size(inserted)
                println(present)
                println(absent)
                drop(inserted)
                size
            }
        "#;
        let (checked, file) = checked(source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
        assert_eq!(
            admission.map_any_owner,
            mimi::core::mir::MapAnyOwnerAdmission::CompleteCoverage
        );
        assert!(matches!(
            admission.collection,
            mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
        ));
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("static contains over a typed owner must enter the default canonical route");
        };
        let main = mimi::core::NodeId("function:main".into());
        let reference = mimi::core::mir::reference::MirReferenceInterpreter::new(&program)
            .execute_with_output(&main, &[])
            .expect("MapRoot remains usable after static contains queries");
        assert_eq!(reference.output, "true\nfalse\n");
        assert_eq!(
            reference.value,
            mimi::core::mir::reference::MirRuntimeValue::Int(1)
        );
    }

    #[test]
    fn ordinary_map_owned_get_export_body_cannot_escape_to_legacy_route() {
        let source = r#"
            extern "C" func exported_identity(value: i32) -> i32 { value }

            func main() -> i32 {
                let root = map_new()
                let result = map_get_i32(root, "answer")
                drop(root)
                match result {
                    Ok(value) => value,
                    Err(status) => status,
                }
            }
        "#;
        let (checked, file) = checked(source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
        assert!(
            admission
                .ordinary_map_owned_get_i32_string_v1
                .has_profile_signal(),
            "typed getter admission must remain visible alongside exported compatibility"
        );

        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("owned-get must not enter a legacy backend when an export body forces legacy");
        };
        assert!(reason.contains(mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE));
        assert!(reason.contains("owned-get"));
        assert!(!reason.contains("not yet implemented"));
    }

    #[test]
    fn ordinary_map_owner_route_rejects_incomplete_ownership_without_legacy_retry() {
        for (index, source) in [
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let value = map_get(root, "answer")
                    drop(value)
                    drop(root)
                    0
                }
            "#,
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let key = "answer"
                    let updated = map_set(root, key, 42)
                    drop(updated)
                    0
                }
            "#,
            r#"
                func branch_size(flag: bool) -> i32 {
                    let root = map_new()
                    if flag {
                        let size = map_size(root)
                    } else {
                        let size = map_size(root)
                    }
                    drop(root)
                    0
                }
                func main() -> i32 { branch_size(true) }
            "#,
            r#"
                func main() -> i32 {
                    loop {
                        let root = map_new()
                        drop(root)
                        break
                    }
                    0
                }
            "#,
            r#"
                func main() -> i32 {
                    let root = map_new()
                    0
                }
            "#,
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let values_snapshot = values(root)
                    let count = values_snapshot.len()
                    drop(values_snapshot)
                    drop(root)
                    count
                }
            "#,
            r#"
                type Point { x: i32 }
                func main() -> i32 {
                    let root = map_new()
                    let updated = map_set(root, "point", Point { x: 7 })
                    drop(updated)
                    0
                }
            "#,
            r#"
                func main() -> i32 {
                    let root = map_new()
                    let first = map_set(root, "answer", 42)
                    let mixed = map_set(first, "label", "forty-two")
                    drop(root)
                    drop(first)
                    drop(mixed)
                    0
                }
            "#,
        ]
        .into_iter()
        .enumerate()
        {
            let (checked, file) = checked(source);
            let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
            assert_eq!(
                admission.map_any_owner,
                mimi::core::mir::MapAnyOwnerAdmission::OutsideProfile,
                "ordinary Map/Any compatibility form must not become an incomplete typed-owner candidate: {source}"
            );
            assert_eq!(
                admission.map_root,
                mimi::core::mir::MapRootAdmission::OutsideProfile,
                "ordinary Map/Any compatibility form must stay outside MapRoot: {source}"
            );
            // The matrix keeps every independently receipted owner operation
            // on canonical MIR: untyped get, dynamic-key set, branch-local
            // read, loop cleanup, scope-drop root, Values snapshot, and the
            // mixed i32/String chain.  Record payloads remain an explicit
            // negative coverage probe until their operation receipt closes.
            let expected_canonical = matches!(index, 0 | 1 | 2 | 3 | 4 | 5 | 7);
            if expected_canonical {
                let route = select_default_route(&checked, &file);
                assert!(
                    matches!(&route, DefaultMirRoute::Canonical(_)),
                    "complete Map/Any owner slice must use its canonical route (case {index}): {source}; selected {route:?}"
                );
            } else {
                assert!(
                    admission
                        .ordinary_map_owner_lifecycle_i32_string_v1
                        .has_profile_signal()
                        || admission
                            .ordinary_map_owned_get_i32_string_v1
                            .has_profile_signal(),
                    "unhandled ordinary Map operation must remain visible to the route census: {source}; admission={admission:?}"
                );
                let route = select_default_route(&checked, &file);
                assert!(
                    matches!(&route, DefaultMirRoute::Rejected(reason) if reason.contains(mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)),
                    "unsupported ordinary Map shape must fail closed without a legacy retry: {source}; selected {route:?}"
                );
            }
        }
    }

    #[test]
    fn ordinary_map_owner_root_escape_rejects_before_any_legacy_owner() {
        // Returning a Map root crosses the callable boundary.  The Checker
        // deliberately records the New attempt but withholds its lifecycle
        // receipt, so the default execution and verification selectors must
        // expose one shared MIR coverage rejection instead of handing the
        // escaped owner to the AST compatibility emitter.
        let source = r#"
            func escape_root() -> Record {
                let root = map_new()
                root
            }
            func main() -> i32 {
                let escaped = escape_root()
                0
            }
        "#;
        let (checked, file) = checked(source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
        assert_eq!(
            admission.ordinary_map_owner_lifecycle_i32_string_v1,
            mimi::core::mir::OrdinaryMapOwnerLifecycleI32StringV1Admission::IncompleteCoverage,
            "root escape must remain visible as an incomplete ownership candidate"
        );
        for (label, route) in [
            ("execution", select_default_route(&checked, &file)),
            (
                "verification",
                select_default_verification_route(&checked, &file),
            ),
        ] {
            let DefaultMirRoute::Rejected(reason) = route else {
                panic!("{label} route must reject an escaped ordinary Map root");
            };
            assert!(
                reason.contains(mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE),
                "{label} route must report checker-owned MIR coverage: {reason}"
            );
            assert!(
                !reason.contains("legacy"),
                "{label} route must not expose a hidden legacy fallback: {reason}"
            );
        }
    }

    #[test]
    fn explicit_v2_execution_builder_rejects_while_read_only_inspection_remains_available() {
        let source = r#"
type Label { text: string }
func main() -> i32 {
    let label = Label { text: "owned label" }
    let empty = map_new()
    let root = map_set(empty, "label", label)
    drop(empty)
    drop(root)
    drop(label)
    0
}
"#;
        let (checked, file) = checked(source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked)
            .ordinary_map_owned_any_v2;
        assert_eq!(
            admission,
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::CompleteCoverage
        );
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);

        let error = build_canonical_program(&checked, &file)
            .expect_err("explicit execution/build/verify builder must keep V2 readiness closed");
        assert!(
            error.contains(mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE),
            "{error}"
        );
        assert!(error.contains("not production-ready"), "{error}");

        let inspected = build_canonical_program_for_sources_with_diagnostics(&checked, &file, None)
            .expect("read-only MIR inspection remains available for V2 evidence");
        assert!(inspected
            .ordinary_map_owned_any_v2_payload_selection_receipt()
            .is_ok_and(|receipt| receipt.is_some()));
    }

    #[test]
    fn ordinary_map_owned_any_v2_route_runs_whole_graph_preflight_before_readiness_gate() {
        let supported = checked(
            r#"
            type Label { text: string }
            func main() -> i32 {
                let label = Label { text: "route preflight" }
                let empty = map_new()
                let root = map_set(empty, "label", label)
                drop(empty)
                drop(root)
                drop(label)
                0
            }
            "#,
        )
        .0;
        let supported_mir = MirProgram::from_checked_program(&supported)
            .expect("supported V2 source lowers to MIR");
        assert!(mimi::core::mir::validate_ordinary_map_owned_any_v2_island(&supported_mir).is_ok());
        let DefaultMirRoute::Rejected(supported_reason) =
            select_ordinary_map_owned_any_v2_route(supported_mir)
        else {
            panic!("V2 execution must remain closed while production readiness is false");
        };
        assert!(
            supported_reason.contains("not production-ready"),
            "{supported_reason}"
        );
        assert!(
            !supported_reason.contains("whole-program island"),
            "{supported_reason}"
        );

        let unsupported = checked(
            r#"
            type Label { text: string }
            func main() -> i32 {
                let label = Label { text: "route preflight" }
                let empty = map_new()
                let root = map_set(empty, "label", label)
                drop(empty)
                drop(root)
                drop(label)
                0
            }
            func hidden() -> i32 { 1 * 2 }
            "#,
        )
        .0;
        let unsupported_mir = MirProgram::from_checked_program(&unsupported)
            .expect("unsupported helper still lowers to structurally valid MIR");
        let DefaultMirRoute::Rejected(unsupported_reason) =
            select_ordinary_map_owned_any_v2_route(unsupported_mir)
        else {
            panic!("whole-program V2 preflight must reject the hidden unsupported function");
        };
        assert!(
            unsupported_reason.contains("whole-program island")
                && !unsupported_reason.contains("not production-ready"),
            "the graph-closure gate must run before the closed readiness gate: {unsupported_reason}"
        );
    }

    #[test]
    fn empty_root_get_any_passes_direct_preflight_but_default_route_keeps_readiness_closed() {
        let (checked, file) = checked(
            r#"
func main() -> i32 {
    let root = map_new()
    let pair = map_get(root, "missing")
    let found = pair.0
    drop(pair)
    drop(root)
    if found { 1 } else { 0 }
}
"#,
        );
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
        assert_eq!(
            admission.ordinary_map_owned_any_v2,
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::CompleteCoverage
        );
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);

        let canonical = MirProgram::from_checked_program(&checked)
            .expect("direct empty-root GetAny must pass MIR construction and validation");
        assert!(mimi::core::mir::validate_ordinary_map_owned_any_v2_island(&canonical).is_ok());
        let DefaultMirRoute::Rejected(direct_reason) =
            select_ordinary_map_owned_any_v2_route(canonical)
        else {
            panic!("direct owner-v2 execution must remain closed while readiness is false");
        };
        assert!(
            direct_reason.contains("not production-ready"),
            "direct route must reach readiness gate after whole-program preflight: {direct_reason}"
        );
        assert!(
            !direct_reason.contains("whole-program island"),
            "{direct_reason}"
        );

        let DefaultMirRoute::Rejected(default_reason) = select_default_route(&checked, &file)
        else {
            panic!("default execution must reject the recognized empty-root V2 route");
        };
        assert!(
            default_reason.contains("not production-ready"),
            "default route must reach the same V2 readiness boundary: {default_reason}"
        );
        assert!(
            !default_reason.contains("legacy"),
            "no empty-root route may hide a fallback"
        );
    }
    #[test]
    fn ordinary_map_direct_mir_boundary_rejects_incomplete_operation_coverage() {
        let source = r#"
            func main() -> i32 {
                let root = map_new()
                let value = map_get(root, "answer")
                drop(root)
                0
            }
        "#;
        let (checked, file) = checked(source);
        let error = build_canonical_program_for_sources_with_diagnostics(&checked, &file, None)
            .expect_err("direct MIR inspection must reject ordinary Map without Get ownership");
        assert!(error.diagnostic_codes().contains(&Some("MIR-COVERAGE-001")));
    }

    #[test]
    fn ordinary_map_owned_get_receipt_profile_is_shared_across_cli_consumers() {
        let source = include_str!("../../tests/fixtures/mir_ordinary_map_owned_get_default.mimi");
        let (checked, file) = checked(source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
        assert!(admission.ordinary_map_owned_get_i32_string_v1_complete());
        let profile = mimi::core::mir::ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND;
        for default_profile in [
            "run-canonical-v1",
            mimi::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE,
            "verify-canonical-v1",
        ] {
            assert_eq!(
                route_receipt_profile_for_checked(&checked, default_profile),
                profile,
                "owned-get must preserve its exact receipt profile for {default_profile}"
            );
        }
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("complete owned-get fixture must pass default route preflight");
        };
        let receipt = program.route_receipt(profile);
        assert_eq!(receipt.profile, profile);
        receipt
            .validate_against_program(&program)
            .expect("owned-get receipt must bind the selected MIR graph");
    }

    #[test]
    fn ordinary_map_owned_get_direct_mir_boundary_rejects_incomplete_coverage() {
        let source = r#"
            func main() -> i32 {
                let root = map_new()
                let result = map_get_i32(root, "answer")
                drop(root)
                match result {
                    Ok(value) => value,
                    Err(code) => code,
                }
            }
        "#;
        let (checked, file) = checked(source);
        assert_eq!(
            mimi::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_get_i32_string_v1,
            mimi::core::mir::OrdinaryMapOwnedGetI32StringV1Admission::IncompleteCoverage
        );
        let error = build_canonical_program_for_sources_with_diagnostics(&checked, &file, None)
            .expect_err("direct MIR construction must reject an unreceipted typed getter");
        assert!(error.diagnostic_codes().contains(&Some("MIR-COVERAGE-001")));
        assert!(error
            .to_string()
            .contains("typed owned-get Map candidate without complete operation-attempt coverage"));
    }

    #[test]
    fn ordinary_map_owned_get_direct_mir_boundary_rejects_scope_only_result_consumers() {
        for (label, source) in [
            (
                "Values",
                r#"
                    func main() -> i32 {
                        let root = map_new()
                        let snapshot = values(root)
                        let count = snapshot.len()
                        drop(root)
                        count
                    }
                "#,
            ),
            (
                "GetAny",
                r#"
                    func main() -> i32 {
                        let root = map_new()
                        let pair = map_get(root, "answer")
                        let found = pair.0
                        drop(root)
                        if found { 1 } else { 0 }
                    }
                "#,
            ),
        ] {
            let (checked, file) = checked(source);
            assert_eq!(
                mimi::core::mir::classify_canonical_mir_route_admission(&checked)
                    .ordinary_map_owned_get_i32_string_v1,
                mimi::core::mir::OrdinaryMapOwnedGetI32StringV1Admission::CompleteCoverage,
                "{label} operation should have complete checker operation coverage"
            );
            let error = build_canonical_program_for_sources_with_diagnostics(&checked, &file, None)
                .expect_err(
                    "direct MIR construction must reject a result settled only by scope cleanup",
                );
            assert!(
                error.diagnostic_codes().contains(&Some("MIR-COVERAGE-001")),
                "{label}: {error}"
            );
            assert!(
                error.to_string().contains("not settled"),
                "{label}: {error}"
            );
        }
    }

    #[test]
    fn ordinary_map_complete_profile_materializes_its_own_mir_receipt() {
        let source = r#"
            func main() -> i32 {
                let root = map_new()
                let keyed = map_set(root, "answer", 0)
                let updated = map_set(keyed, "label", "")
                let size = map_size(updated)
                drop(root)
                drop(keyed)
                drop(updated)
                size
            }
        "#;
        let (checked, file) = checked(source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
        assert!(admission.ordinary_map_owner_lifecycle_i32_string_v1_complete());
        assert_eq!(
            admission.map_root,
            mimi::core::mir::MapRootAdmission::OutsideProfile
        );
        assert_eq!(
            admission.map_any_owner,
            mimi::core::mir::MapAnyOwnerAdmission::OutsideProfile
        );
        for default_profile in [
            "run-canonical-v1",
            mimi::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE,
            "verify-canonical-v1",
        ] {
            assert_eq!(
                route_receipt_profile_for_checked(&checked, default_profile),
                mimi::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_I32_STRING_V1_ISLAND,
                "ordinary Map lifecycle must preserve its exact receipt profile for {default_profile}"
            );
        }
        let program = build_canonical_program_for_sources_with_diagnostics(&checked, &file, None)
            .expect("complete ordinary Map owner graph should materialize");
        assert_eq!(
            program
                .route_receipt(mimi::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_I32_STRING_V1_ISLAND)
                .profile,
            mimi::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_I32_STRING_V1_ISLAND
        );
    }

    #[test]
    fn map_any_owner_default_route_rejects_extra_temporary_root_without_legacy_retry() {
        let source = r#"
            func main() -> i32 {
                let root = map_new()
                    let updated = map_set(root, "answer", 42)
                    let size = map_size(updated)
                    let temporary_size = map_size(map_new())
                    let unreceipted = map_get(map_new(), "missing")
                    drop(updated)
                    size + temporary_size
            }
        "#;
        let (checked, file) = checked(source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
        assert_eq!(
            admission.map_any_owner,
            mimi::core::mir::MapAnyOwnerAdmission::IncompleteCoverage,
            "an action-bearing typed lifecycle must not mask extra Map construction attempts"
        );
        assert_eq!(
            admission.map_root,
            mimi::core::mir::MapRootAdmission::OutsideProfile
        );
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("partial MapAnyOwner lifecycle must fail closed");
        };
        assert!(
            reason.contains(mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE),
            "{reason}"
        );
        assert!(!reason.contains("legacy"), "{reason}");
    }

    #[test]
    fn map_any_owner_default_route_receipts_temporary_roots_across_callables() {
        let source = r#"
            func temporary_size() -> i32 {
                map_size(map_new())
            }
            func main() -> i32 {
                let root = map_new()
                let updated = map_set(root, "answer", 42)
                let size = map_size(updated)
                drop(updated)
                size
            }
        "#;
        let (checked, file) = checked(source);
        assert_eq!(
            mimi::core::mir::classify_canonical_mir_route_admission(&checked).map_any_owner,
            mimi::core::mir::MapAnyOwnerAdmission::CompleteCoverage,
            "each temporary map owner in the program has an exact New/Size/Drop receipt"
        );
        assert!(
            matches!(
                select_default_route(&checked, &file),
                DefaultMirRoute::Canonical(_)
            ),
            "whole-program MapRoot receipts must select canonical MIR"
        );
    }

    #[test]
    fn scalar_ffi_default_route_rejects_unsupported_declaration_before_legacy() {
        for (source, expected) in [
            (
                r#"extern "Rust" { func foreign(value: i64) -> i64; }
                func main() -> i64 { foreign(42 as i64) }"#,
                "ABI 'Rust' is outside the canonical C ABI",
            ),
            (
                r#"#[no_panic]
                extern "C" { func foreign(value: i64) -> i64; }
                func main() -> i64 { foreign(42 as i64) }"#,
                "no_panic FFI protection",
            ),
            (
                r#"extern "C" { func foreign(value: i64 ...) -> i64; }
                func main() -> i64 { foreign(42 as i64) }"#,
                "variadic ABI",
            ),
            (
                r#"extern "C" { func foreign(value: string) -> i64; }
                func main() -> i64 { foreign("ok") }"#,
                "parameter type is outside canonical scalar FFI",
            ),
            (
                r#"extern "C" { func foreign(value: (i64, string)) -> i64; }
                func main() -> i64 { foreign((42 as i64, "ok")) }"#,
                "parameter type is outside canonical scalar FFI",
            ),
            (
                r#"extern "C" { func foreign(value: i64) -> string; }
                func main() -> string { foreign(42 as i64) }"#,
                "result type is outside canonical scalar FFI",
            ),
        ] {
            let (checked, file) = checked(source);
            let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
                panic!("unsupported FFI declaration must reject before legacy: {expected}");
            };
            assert!(
                reason.contains("canonical scalar FFI declaration boundary"),
                "{reason}"
            );
            assert!(reason.contains(expected), "{expected}: {reason}");
            assert!(!reason.contains("legacy"), "{reason}");
        }
    }

    #[test]
    fn scalar_ffi_default_route_rejects_uncovered_graph_without_fallback() {
        for (index, source) in [
            r#"extern "C" { func foreign(x: f64) -> f64 requires: x > 0.0; }
                func main() -> f64 { foreign(42.5) }"#,
            r#"extern "C" { func foreign(x: i64) -> i64; }
                func main() -> i64 { let xs = [1, 2]; println(len(xs)); foreign(42 as i64) }"#,
        ]
        .into_iter()
        .enumerate()
        {
            let (checked, file) = checked(source);
            assert!(mimi::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi);
            let route = select_default_route(&checked, &file);
            let DefaultMirRoute::Rejected(reason) = route else {
                panic!("uncovered scalar FFI graph must fail closed");
            };
            if index == 0 {
                assert!(
                    reason.contains(mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE),
                    "consumer capability rejection must preserve its registered code: {reason}"
                );
            }
        }
    }

    #[test]
    fn scalar_ffi_nested_root_helper_is_admitted_with_scope_receipt() {
        let source = r#"
            extern "C" { func llabs(value: i64) -> i64; }
            func main() -> i64 {
                func local_abs(value: i64) -> i64 { llabs(value) }
                local_abs(-17 as i64)
            }
        "#;
        let (checked, file) = checked(source);
        assert!(
            mimi::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi,
            "the nested callable's extern call must cross scalar FFI admission"
        );
        let DefaultMirRoute::Canonical(route) = select_default_route(&checked, &file) else {
            panic!("a checker-receipted root-scope helper must enter the canonical route");
        };
        assert_eq!(route.nested_callable_scopes().len(), 1);
        let scope = route
            .nested_callable_scopes()
            .values()
            .next()
            .expect("checker scope receipt");
        assert_eq!(scope.parent.0, "function:main");
        assert_eq!(scope.call_instructions.len(), 1);
        assert_eq!(
            scope.schema,
            mimi::core::mir::MIR_NESTED_CALLABLE_SCOPE_SCHEMA
        );
        assert_eq!(
            route.canonical_digest(),
            route.route_receipt("scalar-ffi-v1").mir_digest
        );
    }

    #[test]
    fn scalar_ffi_profile_admits_pure_root_scope_helper_from_default_route() {
        let source = r#"
            extern "C" { func llabs(value: i64) -> i64; }
            func main() -> i64 {
                func increment(value: i64) -> i64 { value + 1 as i64 }
                increment(llabs(-17 as i64))
            }
        "#;
        let (checked, file) = checked(source);
        assert!(
            mimi::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi,
            "the called scalar C ABI site must establish the closed graph profile"
        );
        let DefaultMirRoute::Canonical(route) = select_default_route(&checked, &file) else {
            panic!("a pure root-scope scalar helper in a scalar FFI graph must be canonical");
        };
        assert_eq!(route.ffi_calls().len(), 1);
        assert_eq!(route.nested_callable_scopes().len(), 1);
        let scope = route
            .nested_callable_scopes()
            .values()
            .next()
            .expect("checker scope receipt");
        assert!(route
            .ffi_calls()
            .values()
            .all(|ffi| ffi.caller != scope.callee));
        assert_eq!(scope.parent.0, "function:main");
        assert_eq!(scope.call_instructions.len(), 1);
        assert_eq!(
            route.canonical_digest(),
            route.route_receipt("scalar-ffi-v1").mir_digest
        );
    }

    #[test]
    fn scalar_nested_copy_helpers_route_without_an_ffi_receipt() {
        for source in [
            r#"
                func main() -> i32 {
                    func helper(value: i32) -> i32 { value + 1 }
                    helper(41)
                }
            "#,
            r#"
                func main() -> i64 {
                    func helper(value: i64) -> i64 { value + 1 as i64 }
                    helper(41 as i64)
                }
            "#,
            r#"
                func main() -> i32 {
                    func helper(value: bool) -> bool { value }
                    if helper(true) { 1 } else { 0 }
                }
            "#,
        ] {
            let (checked, file) = checked(source);
            let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
            assert!(
                admission.scalar_nested,
                "copy-scalar nested admission: {admission:?}"
            );
            assert!(
                !admission.scalar_ffi,
                "pure nested source must not claim FFI: {admission:?}"
            );
            let DefaultMirRoute::Canonical(route) = select_default_route(&checked, &file) else {
                panic!("direct-root copy-scalar helper must route canonical: {source}");
            };
            assert_eq!(route.ffi_calls().len(), 0);
            assert!(!route.nested_callable_scopes().is_empty());
            assert_eq!(
                route
                    .route_receipt(mimi::core::mir::SCALAR_NESTED_ISLAND)
                    .profile,
                mimi::core::mir::SCALAR_NESTED_ISLAND
            );
        }
    }

    #[test]
    fn unsupported_nested_shapes_reject_before_legacy_compatibility() {
        for source in [
            r#"
                func main() -> i32 {
                    let offset = 1
                    func helper(value: i32) -> i32 { value + offset }
                    helper(41)
                }
            "#,
            r#"
                func main() -> i32 {
                    if true {
                        func helper(value: i32) -> i32 { value + 1 }
                        helper(41)
                    } else { 0 }
                }
            "#,
        ] {
            let (checked, file) = checked(source);
            let reason =
                mimi::core::mir::unsupported_nested_callable_boundary_reason(&checked, None)
                    .expect("unsupported nested shape must carry a coverage boundary");
            assert!(reason.contains("outside the canonical scalar nested-callable slice"));
            let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
                panic!("unsupported nested shape must not re-enter legacy: {source}");
            };
            assert!(reason.contains(mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE));
            assert!(!reason.contains("legacy"));
        }
    }

    #[test]
    fn scalar_ffi_nested_helper_overflow_rejects_default_route_without_fallback() {
        let source = r#"
            extern "C" { func labs(value: i64) -> i64 requires: value < 0; }
            func main() -> i64 {
                func increment(value: i64) -> i64 { value + 1 as i64 }
                increment(labs(-17 as i64))
            }
        "#;
        let (checked, file) = checked(source);
        assert!(mimi::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi);
        let mir = mimi::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect("overflowing nested helper still lowers to validated MIR");
        let verifier_error = mimi::verifier::verify_ffi_mir(&mir)
            .expect_err("the contract verifier must expose the reachable checked overflow");
        assert!(
            verifier_error.contains("reachable trap 'E0802'"),
            "{verifier_error}"
        );
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("a reachable overflow in the nested helper must fail closed");
        };
        assert!(reason.contains("reachable trap 'E0802'"), "{reason}");
        assert!(!reason.contains("legacy"), "{reason}");
    }

    #[test]
    fn scalar_ffi_nested_unit_result_helper_rejects_without_legacy_fallback() {
        let source = r#"
            extern "C" { func notify(value: i32); }
            func main() {
                func helper(value: i32) { notify(value) }
                let done = helper(1)
            }
        "#;
        let (checked, file) = checked(source);
        assert!(
            mimi::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi,
            "the nested unit-result extern call remains within FFI candidate classification"
        );
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("unsupported unit-valued nested binding must fail before compatibility");
        };
        assert!(
            reason.contains("outside the canonical scalar nested-callable slice"),
            "{reason}"
        );
        assert!(!reason.contains("legacy"), "{reason}");
    }

    #[test]
    fn scalar_ffi_nested_recursive_effect_boundary_rejects_without_legacy() {
        let source = r#"
            extern "C" { func llabs(value: i64) -> i64; }
            func main() -> i32 {
                func helper(value: i64) -> i64 { llabs(main() as i64) }
                println(helper(1 as i64))
                0
            }
        "#;
        let (checked, file) = checked(source);
        assert!(
            mimi::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi,
            "the recursive nested helper must be recognized as the scalar FFI profile"
        );
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!(
                "recursive nested scalar FFI graph must fail closed without compatibility routing"
            );
        };
        assert!(reason.contains(mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE));
        assert!(reason.contains("unknown effect"), "{reason}");
        assert!(reason.contains("call site 'main'"), "{reason}");
        assert!(!reason.contains("legacy"), "{reason}");
    }

    #[test]
    fn source_scope_builder_preserves_typed_build_diagnostics_until_cli_boundary() {
        let source = r#"
            extern "C" { func foreign(value: f64) -> f64 requires: value > 0.0; }
            func main() -> f64 { foreign(42.5) }
        "#;
        let (checked, file) = checked(source);
        let error = build_canonical_program_for_sources_with_diagnostics(&checked, &file, None)
            .expect_err("unsupported scalar FFI graph must retain a typed build error");
        assert!(matches!(
            error,
            mimi::core::mir::reference::MirProgramBuildError::Validation(_)
        ));
        assert_eq!(error.diagnostic_codes(), vec![None]);
        let display = format!("MIR inspection input rejected: {error}");
        assert!(display.contains("MIR inspection input rejected"));
        assert!(display.contains("MIR validation failed"));
        assert!(!display.contains(mimi::core::mir::MIR_ROUTE_MATERIALIZATION_ERROR_CODE));
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
    fn concrete_flat_record_protocol_field_read_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_protocol_method.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_flat_copy_record_admission(&checked),
            mimi::core::mir::FlatCopyRecordAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("closed concrete ProtocolMethod field read must route canonical");
        };
        assert!(program
            .functions()
            .keys()
            .any(|owner| { owner.0.starts_with("function:Read:for:Counter::read:") }));
        assert!(program.functions().values().any(|function| {
            function
                .blocks
                .values()
                .flat_map(|block| block.instructions.iter())
                .any(|instruction| {
                    matches!(
                        &instruction.kind,
                        mimi::core::mir::MirInstructionKind::Call {
                            callee: mimi::core::ir::ResolvedCallee::ProtocolMethod { .. },
                            ..
                        }
                    )
                })
        }));
    }

    #[test]
    fn flat_record_protocol_admission_rejects_non_closed_method_shapes() {
        let cases = [
            (
                "computed field method",
                r#"
                    trait Read { func read() -> i32 }
                    type Counter { value: i32 }
                    impl Read for Counter { func read() -> i32 { self.value + 1 } }
                    func main() -> i32 { let counter = Counter { value: 41 }; counter.read() }
                "#,
            ),
            (
                "generic impl without a call site",
                r#"
                    trait Read { func read() -> i32 }
                    type Counter { value: i32 }
                    impl<T> Read for Counter { func read() -> i32 { self.value } }
                    func main() -> i32 { let counter = Counter { value: 41 }; counter.value }
                "#,
            ),
            (
                "trait with an additional method",
                r#"
                    trait Read { func read() -> i32; func read_again() -> i32 }
                    type Counter { value: i32 }
                    impl Read for Counter {
                        func read() -> i32 { self.value }
                        func read_again() -> i32 { self.value }
                    }
                    func main() -> i32 { let counter = Counter { value: 41 }; counter.read() }
                "#,
            ),
            (
                "method with stdout side effect",
                r#"
                    trait Read { func read() -> i32 }
                    type Counter { value: i32 }
                    impl Read for Counter {
                        func read() -> i32 { println(self.value); self.value }
                    }
                    func main() -> i32 { let counter = Counter { value: 41 }; counter.read() }
                "#,
            ),
            (
                "multiple implementations",
                r#"
                    trait Read { func read() -> i32 }
                    type Counter { value: i32 }
                    type OtherCounter { value: i32 }
                    impl Read for Counter { func read() -> i32 { self.value } }
                    impl Read for OtherCounter { func read() -> i32 { self.value } }
                    func main() -> i32 { let counter = Counter { value: 41 }; counter.read() }
                "#,
            ),
        ];
        for (label, source) in cases {
            let (checked, file) = checked(source);
            assert_eq!(
                mimi::core::mir::classify_flat_copy_record_admission(&checked),
                mimi::core::mir::FlatCopyRecordAdmission::MixedCoverage,
                "{label} admission must stay closed"
            );
            let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
                panic!("{label} must not enter a route or legacy fallback");
            };
            assert!(reason.contains("flat Copy record"), "{label}: {reason}");
        }
    }

    #[test]
    fn dynamic_trait_object_sibling_keeps_legacy_compatibility_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_protocol_method_dyn.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_flat_copy_record_admission(&checked),
            mimi::core::mir::FlatCopyRecordAdmission::MixedCoverage
        );
        assert!(matches!(
            select_default_route(&checked, &file),
            DefaultMirRoute::Legacy(LegacyRouteReason::MixedCoverageWithoutMaterializedCandidate)
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
    fn generic_option_f64_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_option_unwrap_f64.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_option_projection_admission(&checked),
            mimi::core::mir::GenericOptionProjectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic Option<f64> projection must select the canonical default route");
        };
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership
                            == mimi::core::mir::types::MirOwnership::Copy
                )
            })
            .expect("generic Option<f64> projection instance");
        assert_eq!(
            program
                .type_catalog()
                .get(&instance.arguments[0])
                .unwrap()
                .abi,
            mimi::core::mir::types::MirAbiClass::Float { bits: 64 }
        );
    }

    #[test]
    fn generic_option_f32_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_option_unwrap_f32.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_option_projection_admission(&checked),
            mimi::core::mir::GenericOptionProjectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic Option<f32> projection must select the canonical default route");
        };
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership
                            == mimi::core::mir::types::MirOwnership::Copy
                )
            })
            .expect("generic Option<f32> projection instance");
        assert_eq!(
            program
                .type_catalog()
                .get(&instance.arguments[0])
                .unwrap()
                .abi,
            mimi::core::mir::types::MirAbiClass::Float { bits: 32 }
        );
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
    fn generic_result_f64_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_unwrap_f64.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_result_projection_admission(&checked),
            mimi::core::mir::GenericResultProjectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic Result<T,i32> f64 projection must select the canonical route");
        };
        let instance = program.instances().values().find(|instance| {
            matches!(
                &instance.contract,
                mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                    contract
                } if contract.projection.nominal.as_str() == "builtin:type:Result"
            )
        });
        let Some(instance) = instance else {
            panic!("generic Result f64 projection instance is absent");
        };
        let mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract } =
            &instance.contract
        else {
            unreachable!("filtered above");
        };
        assert!(matches!(
            program
                .type_catalog()
                .get(&contract.result_ty)
                .map(|descriptor| descriptor.abi),
            Some(mimi::core::mir::types::MirAbiClass::Float { bits: 64 })
        ));
    }

    #[test]
    fn generic_result_homogeneous_f64_projection_is_rejected_before_legacy_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_unwrap_homogeneous_f64_rejected.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_result_projection_admission(&checked),
            mimi::core::mir::GenericResultProjectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("homogeneous Result<T,T> f64 projection must fail closed before legacy");
        };
        assert!(
            reason.contains("generic-result-projection-v1")
                || reason.contains("generic Result projection"),
            "{reason}"
        );
    }

    #[test]
    fn generic_result_bool_error_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_bool_error.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_result_projection_admission(&checked),
            mimi::core::mir::GenericResultProjectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic Result<T,bool> projection must select canonical MIR");
        };
        let instance = program.instances().values().find(|instance| {
            matches!(
                &instance.contract,
                mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                    contract
                } if contract.projection.nominal.as_str() == "builtin:type:Result"
            )
        });
        let Some(instance) = instance else {
            panic!("generic Result<T,bool> projection instance is absent");
        };
        let mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract } =
            &instance.contract
        else {
            unreachable!("filtered above");
        };
        let mimi::core::mir::types::MirLayout::Result { ok, error, .. } = &program
            .type_catalog()
            .get(&contract.source_ty)
            .expect("specialized Result<T,bool> TypeDesc")
            .layout
        else {
            panic!("specialized source must retain Result layout");
        };
        assert_ne!(ok, error);
        assert_eq!(
            program.type_catalog().get(error).map(|desc| &desc.kind),
            Some(&mimi::core::mir::types::MirTypeKind::Primitive(
                mimi::core::PrimitiveType::Bool
            ))
        );
        assert!(program.instances().values().any(|instance| matches!(
            &instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                contract
            } if contract.projection.nominal.as_str() == "builtin:type:Result"
        )));
    }

    #[test]
    fn generic_result_owned_string_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_unwrap_owned_string.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_result_projection_admission(&checked),
            mimi::core::mir::GenericResultProjectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic owned Result projection must select canonical MIR");
        };
        let instance = program.instances().values().find(|instance| {
            matches!(
                &instance.contract,
                mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                    contract
                } if contract.projection.nominal.as_str() == "builtin:type:Result"
                    && contract.projection.ownership == mimi::core::mir::types::MirOwnership::Move
                    && contract.projection.move_out_glue
                        == mimi::core::mir::types::MirGlueKind::OwnedString
            )
        });
        assert!(
            instance.is_some(),
            "owned generic Result projection receipt is absent"
        );
    }

    #[test]
    fn generic_result_bool_error_owned_string_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_bool_error_owned_string.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_result_projection_admission(&checked),
            mimi::core::mir::GenericResultProjectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic Result<T,bool> owned String projection must select canonical MIR");
        };
        let instance = program.instances().values().find(|instance| {
            matches!(
                &instance.contract,
                mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                    contract
                } if contract.projection.nominal.as_str() == "builtin:type:Result"
                    && contract.projection.ownership == mimi::core::mir::types::MirOwnership::Move
                    && contract.projection.move_out_glue
                        == mimi::core::mir::types::MirGlueKind::OwnedString
            )
        });
        assert!(
            instance.is_some(),
            "owned generic Result<T,bool> projection receipt is absent"
        );
    }

    #[test]
    fn generic_result_owned_list_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_unwrap_owned_list.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_result_projection_admission(&checked),
            mimi::core::mir::GenericResultProjectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic owned Result<List> projection must select canonical MIR");
        };
        let instance = program.instances().values().find(|instance| {
            matches!(
                &instance.contract,
                mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                    contract
                } if contract.projection.nominal.as_str() == "builtin:type:Result"
                    && contract.projection.ownership == mimi::core::mir::types::MirOwnership::Move
                    && contract.projection.move_out_glue
                        == mimi::core::mir::types::MirGlueKind::List
            )
        });
        assert!(
            instance.is_some(),
            "owned generic Result<List> projection receipt is absent"
        );
    }

    #[test]
    fn unsupported_generic_result_projection_cannot_reenter_legacy_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_unwrap_rejected.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_result_projection_admission(&checked),
            mimi::core::mir::GenericResultProjectionAdmission::OutsideProfile
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
    fn generic_result_string_error_projection_is_rejected_before_legacy_route() {
        let source = "func unwrap<T>(value: Result<T, string>) -> T { value.unwrap() }\nfunc main() -> i32 { unwrap(Ok(41)) }";
        let (checked, file) = checked(source);
        assert!(mimi::core::mir::has_unsupported_generic_result_projection_candidate(&checked));
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("unsupported Result<T,string> projection must fail closed before legacy");
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
    fn generic_result_f64_unwrap_or_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_unwrap_or_f64.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_result_projection_fallback_admission(&checked),
            mimi::core::mir::GenericResultProjectionFallbackAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic Result<T,i32> f64 unwrap_or must select canonical route");
        };
        let instance = program.instances().values().find(|instance| {
            matches!(
                &instance.contract,
                mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                    contract
                } if contract.projection.nominal.as_str() == "builtin:type:Result"
                    && contract.projection.ownership == mimi::core::mir::types::MirOwnership::Copy
            )
        });
        assert!(
            instance.is_some(),
            "generic Result f64 fallback receipt is absent"
        );
    }

    #[test]
    fn generic_result_homogeneous_f64_unwrap_or_is_rejected_before_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_unwrap_or_homogeneous_f64_rejected.mimi"
        ));
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("homogeneous Result<T,T> f64 unwrap_or must fail closed before legacy");
        };
        assert!(
            reason.contains("generic") && reason.contains("outside scalar contract"),
            "{reason}"
        );
    }

    #[test]
    fn generic_result_bool_f64_unwrap_or_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_result_bool_unwrap_or_f64.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_generic_result_projection_fallback_admission(&checked),
            mimi::core::mir::GenericResultProjectionFallbackAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic Result<T,bool> f64 unwrap_or must select canonical route");
        };
        assert!(program.instances().values().any(|instance| matches!(
            &instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                contract
            } if contract.projection.nominal.as_str() == "builtin:type:Result"
                && contract.projection.ownership == mimi::core::mir::types::MirOwnership::Copy
        )));
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
    fn three_field_owned_generic_record_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_record_owned_string_three_field_residual.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_flat_copy_record_admission(&checked),
            mimi::core::mir::FlatCopyRecordAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!(
                "three-field owned generic record residual projection must select canonical MIR"
            );
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                ref contract
            } if contract.projection.arity == 3
                && contract.projection.name == "value"
                && contract.residual.len() == 2
                && contract.residual[0].name == "tail"
                && contract.residual[1].name == "note"
        )));
    }

    #[test]
    fn heterogeneous_owned_generic_record_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_record_owned_string_heterogeneous_residual.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_flat_copy_record_admission(&checked),
            mimi::core::mir::FlatCopyRecordAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!(
                "heterogeneous owned generic record residual projection must select canonical MIR"
            );
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                ref contract
            } if contract.projection.arity == 2
                && contract.projection.name == "value"
                && contract.residual.len() == 1
                && contract.residual[0].name == "note"
                && contract.residual[0].glue == mimi::core::mir::types::MirGlueKind::OwnedString
        )));
    }

    #[test]
    fn three_field_heterogeneous_owned_generic_record_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_record_owned_string_three_field_heterogeneous_residual.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_flat_copy_record_admission(&checked),
            mimi::core::mir::FlatCopyRecordAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("three-field heterogeneous owned projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                ref contract
            } if contract.projection.arity == 3
                && contract.projection.name == "value"
                && contract.residual.len() == 2
                && contract.residual[0].name == "tail"
                && contract.residual[1].name == "note"
                && contract.residual.iter().all(|residual| {
                    residual.glue == mimi::core::mir::types::MirGlueKind::OwnedString
                })
        )));
    }

    #[test]
    fn four_field_heterogeneous_owned_generic_record_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_record_owned_string_four_field_heterogeneous_residual.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_flat_copy_record_admission(&checked),
            mimi::core::mir::FlatCopyRecordAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("four-field heterogeneous owned projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                ref contract
            } if contract.projection.arity == 4
                && contract.projection.name == "value"
                && contract.residual.len() == 3
                && contract.residual[0].name == "spare"
                && contract.residual[1].name == "tail"
                && contract.residual[2].name == "note"
                && contract.residual.iter().all(|residual| {
                    residual.glue == mimi::core::mir::types::MirGlueKind::OwnedString
                })
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
    fn three_field_generic_record_projection_enters_canonical_default_route() {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_projection_three_field.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("three-field generic record projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.arity == 3 && contract.name == "value"
        )));
    }

    #[test]
    fn three_field_generic_record_tail_projection_enters_canonical_default_route() {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_projection_three_field_tail.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("three-field tail projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.field_index == 2 && contract.arity == 3 && contract.name == "value"
        )));
    }

    #[test]
    fn four_field_generic_record_projection_enters_canonical_default_route() {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_projection_four_field.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("four-field generic record projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.arity == 4 && contract.name == "value"
        )));
    }

    #[test]
    fn five_field_generic_record_projection_enters_canonical_default_route() {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_projection_five_field.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("five-field generic record projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.arity == 5 && contract.name == "value"
        )));
    }

    #[test]
    fn six_field_generic_record_projection_enters_canonical_default_route() {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_projection_six_field.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("six-field generic record projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.arity == 6 && contract.name == "value"
        )));
    }

    #[test]
    fn seven_field_generic_record_projection_enters_canonical_default_route() {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_projection_seven_field.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("seven-field generic record projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.arity == 7 && contract.name == "value"
        )));
    }

    #[test]
    fn seven_field_generic_record_tail_projection_enters_canonical_default_route() {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_projection_seven_field_tail.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("seven-field tail projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.field_index == 6 && contract.arity == 7 && contract.name == "value"
        )));
    }

    #[test]
    fn seven_field_repeated_generic_record_projection_enters_canonical_default_route() {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_projection_seven_field_repeated_generic.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("repeated-generic projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.field_index == 0 && contract.arity == 7 && contract.name == "value"
        )));
    }

    #[test]
    fn seven_field_repeated_generic_nonzero_projection_enters_canonical_default_route() {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_projection_seven_field_repeated_generic_nonzero.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("repeated-generic nonzero projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.field_index == 2 && contract.arity == 7 && contract.name == "mirror"
        )));
    }

    #[test]
    fn repeated_generic_record_projection_with_managed_sibling_is_rejected_before_legacy_route() {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_projection_seven_field_repeated_generic_rejected.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("managed repeated-generic projection must fail closed");
        };
        assert!(reason.contains("S0 flat Copy record candidate"), "{reason}");
        assert!(
            reason.contains("canonical generic record projection candidate did not materialize"),
            "{reason}"
        );
    }

    #[test]
    fn generic_copy_record_update_enters_canonical_default_route() {
        let source = include_str!("../../tests/fixtures/mir_native_generic_record_update.mimi");
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic record update must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarRecordUpdate { ref contract }
                if contract.arity == 2 && contract.fields[0].name == "tag"
        )));
    }

    #[test]
    fn unsupported_generic_record_update_is_rejected_before_legacy_route() {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_update_five_field_rejected.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("five-field generic record update must fail closed");
        };
        assert!(reason.contains("S0 flat Copy record candidate"), "{reason}");
        assert!(
            reason.contains("canonical generic record update candidate did not materialize"),
            "{reason}"
        );
    }

    #[test]
    fn two_override_generic_record_update_enters_canonical_default_route() {
        let source =
            include_str!("../../tests/fixtures/mir_native_generic_record_update_multi.mimi");
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("two-override generic record update must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarRecordUpdate {
                ref contract
            } if contract.arity == 3 && contract.fields.len() == 2
                && contract.fields[0].name == "tag"
                && contract.fields[1].name == "enabled"
        )));
    }

    #[test]
    fn three_override_generic_record_update_is_rejected_before_legacy_route() {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_update_three_overrides_rejected.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("three-override generic record update must fail closed");
        };
        assert!(reason.contains("S0 flat Copy record candidate"), "{reason}");
        assert!(
            reason.contains("canonical generic record update candidate did not materialize"),
            "{reason}"
        );
    }

    #[test]
    fn owned_generic_record_update_enters_canonical_default_route() {
        let source =
            include_str!("../../tests/fixtures/mir_native_generic_record_update_owned.mimi");
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("owned generic record update must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::OwnedRecordUpdate { ref contract }
                if contract.arity == 2
                    && contract.updates.len() == 1
                    && contract.residual.len() == 1
                    && contract.updates[0].new_move
                        == mimi::core::mir::types::MirGlueKind::OwnedString
                    && contract.residual[0].glue
                        == mimi::core::mir::types::MirGlueKind::OwnedString
        )));
    }

    #[test]
    fn owned_generic_record_update_five_field_shape_is_rejected_before_legacy_route() {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_update_owned_three_field_rejected.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("five-field owned generic record update must fail closed");
        };
        assert!(
            reason.contains("canonical generic record update candidate did not materialize"),
            "{reason}"
        );
    }

    #[test]
    fn owned_generic_record_update_with_copy_residual_enters_canonical_default_route() {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_update_owned_copy_residual.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("owned generic record update with Copy residual must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::OwnedRecordUpdate { ref contract }
                if contract.updates.len() == 1
                    && contract.residual.len() == 1
                    && contract.residual[0].glue
                        == mimi::core::mir::types::MirGlueKind::Noop
        )));
    }

    #[test]
    fn owned_generic_record_update_f64_copy_residual_is_rejected_before_legacy_route() {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_update_owned_copy_residual_f64_rejected.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("f64 mixed owned generic record update must fail closed");
        };
        assert!(
            reason.contains("canonical generic record update candidate did not materialize"),
            "{reason}"
        );
    }

    #[test]
    fn owned_generic_record_update_with_two_residuals_enters_canonical_default_route() {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_update_owned_multi_residual.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("owned generic record update with two residuals must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::OwnedRecordUpdate { ref contract }
                if contract.arity == 3
                    && contract.updates.len() == 1
                    && contract.residual.len() == 2
                    && contract.residual[0].glue == mimi::core::mir::types::MirGlueKind::Noop
                    && contract.residual[1].glue
                        == mimi::core::mir::types::MirGlueKind::OwnedString
        )));
    }

    #[test]
    fn owned_generic_record_update_with_three_residuals_enters_canonical_default_route() {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_update_owned_three_residual.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("owned generic record update with three residuals must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::OwnedRecordUpdate { ref contract }
                if contract.arity == 4
                    && contract.updates.len() == 1
                    && contract.residual.len() == 3
                    && contract.residual[0].glue == mimi::core::mir::types::MirGlueKind::Noop
                    && contract.residual[1].glue
                        == mimi::core::mir::types::MirGlueKind::OwnedString
                    && contract.residual[2].glue
                        == mimi::core::mir::types::MirGlueKind::OwnedString
        )));
    }

    #[test]
    fn eight_field_generic_record_projection_is_rejected_before_legacy_route() {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_projection_eight_field_rejected.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("eight-field generic record projection must fail closed");
        };
        assert!(reason.contains("S0 flat Copy record candidate"), "{reason}");
        assert!(
            reason.contains("canonical generic record projection candidate did not materialize"),
            "{reason}"
        );
    }

    #[test]
    fn four_field_owned_generic_record_projection_is_rejected_before_legacy_route() {
        let source = "type Quad<T> { first: T, second: T, third: T, fourth: T }\nfunc get<T>(quad: Quad<T>) -> T { quad.first }\nfunc main() -> i32 { let quad = Quad { first: \"selected\", second: \"middle\", third: \"residual\", fourth: \"extra\" }; let picked = get(quad); drop(picked); 41 }";
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("four-field owned generic record projection must fail closed");
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
    fn mixed_owned_generic_record_projection_with_unsupported_noncopy_siblings_is_rejected_before_legacy_route(
    ) {
        let source = include_str!(
            "../../tests/fixtures/mir_native_generic_record_owned_string_five_field_heterogeneous_rejected.mimi"
        );
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("non-Copy sibling must fail closed before legacy route");
        };
        assert!(reason.contains("S0 flat Copy record candidate"), "{reason}");
    }

    #[test]
    fn unsupported_owned_generic_record_projection_is_rejected_before_legacy_route() {
        let source = "type Quad<T> { first: T, second: T, third: T, fourth: T }\nfunc get<T>(quad: Quad<T>) -> T { quad.first }\nfunc main() -> i32 { let quad = Quad { first: \"owned\", second: \"keep\", third: \"also\", fourth: \"last\" }; let picked = get(quad); drop(picked); 41 }";
        let (checked, file) = checked(source);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("four-field owned generic record projection must fail closed");
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
    fn multi_transition_silent_flow_routes_canonical_through_materialized_receipts() {
        // A two-self-loop Flow is outside the strict S8 candidate
        // (count == 1), so the flow hint does not fire.  The state record
        // hint still materializes the whole graph, and the FlowTransition
        // containment receipt carries both transitions through the shared
        // capability/preflight/verifier gates instead of the legacy route.
        let source = include_str!("../../tests/real_world/flow_multi_transition_selfloops.mimi");
        let (checked, file) = checked(source);
        assert!(
            !mimi::core::mir::is_s8_flow_transition_candidate(&checked),
            "two transitions must stay outside the strict S8 candidate"
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("multi-transition silent Flow must route canonical, not legacy");
        };
        assert_eq!(program.transitions().len(), 2);
        assert!(mimi::core::mir::contains_s8_flow_transition_candidate(
            &program
        ));
    }

    #[test]
    fn cross_state_flow_counter_rejects_nominal_glue_gap_before_legacy_route() {
        // The broad scalar Flow admission recognizes this real-world graph,
        // but its cross-state nominal payloads still lack the canonical
        // MoveOut/Clone/Drop receipt required by native MIR validation.  The
        // default dispatcher must surface that gap instead of turning the
        // materialization failure into a hidden legacy build.
        let source = include_str!("../../tests/real_world/flow_counter.mimi");
        let (checked, file) = checked(source);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
        assert_eq!(
            admission.flow,
            mimi::core::mir::S8FlowAdmission::CompleteCoverage,
            "the checker must classify the graph before the canonical glue gate"
        );
        let route = select_default_route(&checked, &file);
        let DefaultMirRoute::Rejected(reason) = route else {
            panic!("cross-state Flow with missing nominal glue must fail closed: {route:?}");
        };
        assert!(
            reason.contains("S8 Flow transition candidate")
                || reason.contains(mimi::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                || reason.contains(mimi::core::mir::MIR_ROUTE_MATERIALIZATION_ERROR_CODE),
            "unexpected Flow rejection: {reason}"
        );
    }

    #[test]
    fn mixed_recoverable_and_silent_flow_routes_canonical() {
        // A failing transition outside every recoverable shape (no extra
        // parameter, so the M3 local-retry profile does not recognize it)
        // plus a silent sibling: neither flow hint fires, but the state
        // record hint materializes the whole graph and the FlowTransition
        // containment receipt carries both transitions — including the
        // `?` failure path — through the shared consumer gates.
        let source = include_str!("../../tests/real_world/flow_retry_mixed_silent_transition.mimi");
        let (checked, file) = checked(source);
        assert!(
            !mimi::core::mir::is_flow_failure_retry_candidate(&checked),
            "a shapeless failing transition must stay outside the recoverable profile"
        );
        let route = select_default_route(&checked, &file);
        let DefaultMirRoute::Canonical(program) = route else {
            panic!(
                "failing + silent Flow transitions must route canonical via receipts: {route:?}"
            );
        };
        assert_eq!(program.transitions().len(), 2);
    }

    #[test]
    fn multi_target_union_transition_routes_canonical_with_receipts() {
        // R6-1035B flip (deliberately reversing the R6-1033 pin
        // multi_target_union_transition_stays_outside_the_migrated_profile):
        // the heterogeneous face — a variant payload outside the flat Copy
        // scalar contract — now closes onto the promoted multi-target union
        // tagged-union contract (Copy scalars + owned Strings, exactly one
        // field per variant), so the materialized receipts route the whole
        // graph Canonical instead of vetoing it to the compatibility route.
        let source = include_str!("../../tests/real_world/flow_multi_target_union_match.mimi");
        let (checked, file) = checked(source);
        assert!(!mimi::core::mir::is_s8_flow_transition_candidate(&checked));
        assert!(!mimi::core::mir::is_flow_failure_retry_candidate(&checked));
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!(
                "heterogeneous multi-target union transitions must route canonical via the promoted tagged-union contract"
            );
        };
        assert!(mimi::core::mir::contains_multi_target_flow_union_candidate(
            &program
        ));
        assert!(mimi::core::mir::multi_target_flow_union_face_closed(
            &program
        ));
        let union_transition = program
            .transitions()
            .values()
            .find(|contract| contract.targets.len() > 1)
            .expect("multi-target transition contract");
        assert_eq!(union_transition.targets.len(), 2);
    }

    #[test]
    fn flat_copy_multi_target_union_routes_canonical() {
        // R6-1034: a multi-target (`-> Hot | Cold`) transition whose union
        // face is flat Copy — one single-field variant payload of the same
        // Copy scalar type across targets — materializes the union receipt,
        // closes onto the flat Copy native contract, and passes all four
        // consumer gates on the default route.
        let source = include_str!("../../tests/real_world/flow_multi_target_union_copy_flat.mimi");
        let (checked, file) = checked(source);
        assert!(!mimi::core::mir::is_s8_flow_transition_candidate(&checked));
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("flat Copy multi-target union must route canonical");
        };
        assert!(mimi::core::mir::contains_multi_target_flow_union_candidate(
            &program
        ));
        assert!(mimi::core::mir::multi_target_flow_union_face_closed(
            &program
        ));
        let union_transition = program
            .transitions()
            .values()
            .find(|contract| contract.targets.len() > 1)
            .expect("multi-target transition contract");
        assert_eq!(union_transition.targets.len(), 2);
    }

    #[test]
    fn heterogeneous_union_face_routes_the_whole_graph_canonical() {
        // R6-1035B flip: the face-closed receipt stays whole-graph, but with
        // the promoted tagged-union contract the heterogeneous Bag union
        // closes onto it, so the mixed flat-Copy + heterogeneous graph routes
        // Canonical for every face instead of being vetoed.  A union payload
        // outside the contract (e.g. a List) keeps the whole-graph veto —
        // pinned by the lib negative test
        // union_outside_promoted_payload_contract_stays_fail_closed.
        let source = r#"
            flow Gauge {
                state Cold { v: i32 }
                state Hot { v: i32 }
                transition heat(Cold, delta: i32) -> Hot | Cold {
                    if self.v + delta > 50 {
                        return Hot { v: self.v + delta }
                    } else {
                        return Cold { v: self.v + delta }
                    }
                }
            }

            flow Bag {
                state Full { tag: string }
                state Empty { v: i32 }
                transition drop_one(Full) -> Full | Empty {
                    return Empty { v: 0 }
                }
            }

            func main() -> i32 {
                let g = Cold { v: 40 }
                let next = Gauge::heat(g, 20)
                let t = match next {
                    Hot { v } => v
                    Cold { v } => v
                }
                println(t)
                0
            }
        "#;
        let (checked, file) = checked(source);
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!(
                "the promoted union contract must close the whole mixed graph onto the canonical route"
            );
        };
        assert!(mimi::core::mir::multi_target_flow_union_face_closed(
            &program
        ));
        assert_eq!(program.transitions().len(), 2);
    }

    #[test]
    fn fault_absorption_union_routes_canonical() {
        // R6-1040 flip (deliberately reversing the R6-1039 pin
        // fault_absorption_union_keeps_explicit_legacy_route): the last
        // checker-legal union face — the compiler-owned Fault absorption
        // target (0.36.9 裁决 6) — now closes onto the promoted tagged-union
        // contract.  The Fault variant's glue-complete payloads
        // (StateId/EventId/String/SystemTrace) carry canonical MoveOut glue,
        // so the union route candidate fires and the whole graph routes
        // Canonical instead of stopping at the compatibility route.
        let absorption = r#"
            flow F {
                state S { v: i64 }
                transition go(S) -> S | Fault {
                    return S { v: self.v }
                }
            }

            func main() -> i64 {
                let s = S { v: 0 }
                let r = F::go(s)
                let v = match r {
                    S { v } => v
                    Fault { last_state: _, unexpected_event: _, snapshot: _, trace: _ } => 1 as i64
                }
                println(v)
                0
            }
        "#;
        // A call inside the transition body stays on the canonical route as
        // long as the argument carries the declared integer identity (a cast
        // or a typed binding).  A bare integer literal argument still trips
        // the pre-existing literal TypeDesc identity boundary at MIR
        // materialization and keeps the explicit legacy route — registered
        // as the remaining union-source-reachable legacy residual.
        let call_in_body = r#"
            func guarded(x: i64) -> i64 {
                x
            }

            flow F {
                state S { v: i64 }
                transition go(S) -> S | Fault {
                    let a = 1 as i64
                    let y = guarded(a)
                    return S { v: y }
                }
            }

            func main() -> i64 {
                let s = S { v: 0 }
                let r = F::go(s)
                let v = match r {
                    S { v } => v
                    Fault { last_state: _, unexpected_event: _, snapshot: _, trace: _ } => 1 as i64
                }
                println(v)
                0
            }
        "#;
        for (label, source) in [("absorption", absorption), ("call-in-body", call_in_body)] {
            let (checked, file) = checked(source);
            let route = select_default_route(&checked, &file);
            let DefaultMirRoute::Canonical(program) = route else {
                panic!(
                    "a Fault absorption union must route canonical via the promoted tagged-union contract ({label})"
                );
            };
            assert!(mimi::core::mir::contains_multi_target_flow_union_candidate(
                &program
            ));
            assert!(mimi::core::mir::multi_target_flow_union_face_closed(
                &program
            ));
            assert!(mimi::verifier::validate_mir_capabilities(&program).is_ok());
        }
    }

    #[test]
    fn bare_integer_literal_call_arguments_route_canonical() {
        // R6-1041: a bare integer literal argument (`guarded(1)`) is accepted
        // by the checker via NumericWiden, but MIR call validation used to
        // fail closed on the stale pre-coercion i32 identity and pushed the
        // whole graph — including the union absorption residual — onto the
        // explicit legacy route.  Once the conversion receipt is materialized,
        // every flow-carrying shape with a bare literal call argument routes
        // canonical.
        let single_target = r#"
            func guarded(x: i64) -> i64 {
                x
            }

            flow F {
                state S { v: i64 }
                transition go(S) -> S {
                    let y = guarded(1)
                    return S { v: y }
                }
            }

            func main() -> i64 {
                let s = S { v: 7 }
                let r = F::go(s)
                println(r.v)
                0
            }
        "#;
        let union = r#"
            func guarded(x: i64) -> i64 {
                x
            }

            flow F {
                state S { v: i64 }
                transition go(S) -> S | Fault {
                    let y = guarded(1)
                    return S { v: y }
                }
            }

            func main() -> i64 {
                let s = S { v: 7 }
                let r = F::go(s)
                let v = match r {
                    S { v } => v
                    Fault { last_state: _, unexpected_event: _, snapshot: _, trace: _ } => 1 as i64
                }
                println(v)
                0
            }
        "#;
        for (label, source) in [("single-target", single_target), ("union", union)] {
            let (checked, file) = checked(source);
            let route = select_default_route(&checked, &file);
            assert!(
                matches!(route, DefaultMirRoute::Canonical(_)),
                "a bare integer literal call argument must not force the legacy route ({label}): {route:?}"
            );
        }
    }

    #[test]
    fn three_target_union_mixed_width_operands_route_canonical() {
        // R6-1042: a >2-target union (S | Big | Fault) whose body mixes
        // scalar widths through the checker's numeric coercion (constant-
        // first comparison `100 < self.v`, arithmetic `self.v + 1`) used to
        // fail the whole-program capability gate on stale pre-coercion
        // operand identities and hard-reject the default route (exit 1 —
        // no legacy fallback once a union candidate exists).  The narrowed
        // operand must be materialized so the route stays canonical.
        let source = r#"
            flow F {
                state S { v: i64 }
                state Big { w: i64 }
                transition go(S) -> S | Big | Fault {
                    if 100 < self.v {
                        let bumped = self.v + 1
                        return Big { w: bumped }
                    }
                    return S { v: self.v }
                }
            }

            func main() -> i64 {
                let s = S { v: 150 }
                let r = F::go(s)
                let v = match r {
                    S { v } => v
                    Big { w } => w
                    Fault { last_state: _, unexpected_event: _, snapshot: _, trace: _ } => 0 as i64
                }
                println(v)
                0
            }
        "#;
        let (checked, file) = checked(source);
        let route = select_default_route(&checked, &file);
        let DefaultMirRoute::Canonical(program) = route else {
            panic!(
                "a three-target union with mixed-width binary operands must route canonical: {route:?}"
            );
        };
        assert!(mimi::core::mir::contains_multi_target_flow_union_candidate(
            &program
        ));
        assert!(mimi::core::mir::multi_target_flow_union_face_closed(
            &program
        ));
        assert!(mimi::verifier::validate_mir_capabilities(&program).is_ok());
    }

    #[test]
    fn binary_float_widening_routes_canonical() {
        // R6-1043: the checker's float-widening coercion pairs
        // ((f64,i32)/(f64,i64)) materialize as Int->Float64 Convert once the
        // canonical conversion contract admits them, so a float-widening
        // arithmetic operand inside a union transition must route canonical
        // instead of hard-rejecting the default entry (the R6-1042 known
        // boundary).  The float comparison face stays fail-closed and is
        // pinned separately in the capability suite.
        let source = r#"
            flow F {
                state S { v: i64 }
                transition go(S) -> S | Fault {
                    let widened = self.v + 0.5
                    return S { v: self.v }
                }
            }

            func main() -> i64 {
                let s = S { v: 7 }
                let r = F::go(s)
                let v = match r {
                    S { v } => v
                    Fault { last_state: _, unexpected_event: _, snapshot: _, trace: _ } => 0 as i64
                }
                println(v)
                0
            }
        "#;
        let (checked, file) = checked(source);
        let route = select_default_route(&checked, &file);
        let DefaultMirRoute::Canonical(program) = route else {
            panic!("a float-widening binary operand must route canonical: {route:?}");
        };
        assert!(mimi::core::mir::contains_multi_target_flow_union_candidate(
            &program
        ));
        assert!(mimi::core::mir::multi_target_flow_union_face_closed(
            &program
        ));
        assert!(mimi::verifier::validate_mir_capabilities(&program).is_ok());
    }

    #[test]
    fn out_of_contract_union_is_checker_rejected_before_route_selection() {
        // R6-1039: user-declared out-of-contract union payload shapes are
        // rejected by the checker (E0446) at the declaration site, so
        // `select_default_route` is never reached for them — the union
        // mixed-coverage veto remains as defense-in-depth for non-checker
        // producers only.
        let source = r#"
            flow P {
                state A { v: i32 }
                state B { xs: List<i32> }
                transition go(A, d: i32) -> A | B {
                    if d > 0 {
                        return B { xs: [d, d] }
                    } else {
                        return A { v: d }
                    }
                }
            }

            func main() -> i32 {
                let a = A { v: 10 }
                let r = P::go(a, 5)
                drop(r)
                println(6)
                0
            }
        "#;
        let tokens = mimi::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = mimi::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let diagnostics = mimi::core::check_program(&file)
            .err()
            .unwrap_or_else(|| panic!("the checker must reject the List payload union"));
        assert!(diagnostics
            .iter()
            .any(|d| d.code.as_deref() == Some("E0446")));
    }

    #[test]
    fn silent_sibling_keeps_the_local_retry_profile_canonical() {
        // The M3 local-retry arm collects only failing implemented
        // transitions, so adding a silent sibling transition preserves the
        // recoverable-Flow hint while the shared consumer gates cover the
        // whole two-transition graph.
        let source = include_str!("../../tests/real_world/flow_retry_m3_with_silent_sibling.mimi");
        let (checked, file) = checked(source);
        assert!(mimi::core::mir::is_flow_failure_retry_candidate(&checked));
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("local-retry Flow with a silent sibling must route canonical");
        };
        assert_eq!(program.transitions().len(), 2);
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
    fn scalar_collection_mixed_owned_string_graph_enters_canonical_default_route() {
        // R6-1094: the 5d08128c-era pin expected any managed value beside a
        // List.len candidate to reject.  The owned-String faces (R6-1049 /
        // R6-1050) deliberately admitted bound string literals with explicit
        // drop accounting, so this graph now materializes canonical MIR that
        // carries both the List glue and the OwnedString drop glue.  The pin
        // guards the deliberate widening: the route must be canonical with
        // the list operation and managed drops intact, never legacy.
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
        // The owned string keeps the checker scan Mixed, yet the graph is
        // fully constructible, so the route materializes canonical MIR
        // instead of the compatibility floor.
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::MixedCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("a constructible mixed collection graph must materialize canonical MIR");
        };
        assert!(
            program.functions().values().any(|function| {
                function.blocks.values().any(|block| {
                    block.instructions.iter().any(|instruction| {
                        matches!(
                            instruction.kind,
                            mimi::core::mir::MirInstructionKind::ListOp {
                                operation: mimi::core::mir::MirListOperation::Len,
                                ..
                            }
                        )
                    })
                })
            }),
            "the canonical MIR must retain the materialized List.len"
        );
        let drop_count = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| {
                matches!(
                    instruction.kind,
                    mimi::core::mir::MirInstructionKind::Drop { .. }
                )
            })
            .count();
        assert!(
            drop_count >= 2,
            "the list and the owned string must keep explicit drop accounting, got {drop_count}"
        );
    }

    #[test]
    fn scalar_collection_candidate_with_unsupported_managed_value_fails_closed() {
        // A managed value with no canonical glue (a mutex handle) inside an
        // otherwise-recognized collection candidate must fail closed at MIR
        // construction instead of re-entering the legacy route.
        let source = r#"
            func main() -> i32 {
                let values = [1, 2, 3]
                let count = len(values)
                drop(values)
                let lock = mutex_new(7 as i64)
                drop(lock)
                count
            }
        "#;
        let (checked, file) = checked(source);
        // R6-1094 boundary record, verdict closed in R6-1096: a glue-less
        // managed value beside a materialized List.len makes canonical
        // construction fail, the route envelope drops its materialized
        // candidate flags, and the dispatcher takes the explicit
        // MixedCoverageWithoutMaterializedCandidate compatibility route.
        // This is the documented R6-1052 floor for previously working
        // programs.  The absent `len` compatibility-arm tripwire (unlike
        // reverse/concat/generic facades) is a deliberate exemption, not a
        // hole: `has_unsupported_list_reverse_candidate` records "List len
        // retains the pre-existing compatibility policy here" (b58cfb79,
        // written after the len island had already closed), and the policy
        // line is coherent — len is a read-only borrow with no ownership
        // obligations crossing the island boundary, while reverse/concat
        // are transform operations whose unsupported boundary shapes carry
        // contract implications and therefore fail closed.  Reversing the
        // exemption would turn working programs into hard rejections and
        // needs its own island-closure verdict, not a silent pin change.
        let DefaultMirRoute::Legacy(reason) = select_default_route(&checked, &file) else {
            panic!("a glue-less managed value beside len must take the compatibility route");
        };
        assert!(
            matches!(
                reason,
                LegacyRouteReason::MixedCoverageWithoutMaterializedCandidate
            ),
            "unexpected route reason: {reason:?}"
        );
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
    fn nested_list_len_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_nested_list_len.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("nested List.len must select the canonical default route");
        };
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        mimi::core::mir::MirInstructionKind::ListOp {
                            operation: mimi::core::mir::MirListOperation::Len,
                            ..
                        }
                    )
                })
            })
        }));
    }

    #[test]
    fn nested_list_index_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_nested_list_index.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("nested List index must select the canonical default route");
        };
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        mimi::core::mir::MirInstructionKind::Project {
                            projection: mimi::core::mir::MirProjection::Index(_),
                            list_index_contract: Some(ref receipt),
                            ..
                        } if receipt.mode
                            == mimi::core::mir::types::MirListIndexProjectionMode::CloneNestedList
                    )
                })
            })
        }));
    }

    #[test]
    fn nested_list_reverse_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_nested_list_reverse.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("nested List.reverse must select the canonical default route");
        };
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        mimi::core::mir::MirInstructionKind::ListOp {
                            operation: mimi::core::mir::MirListOperation::Reverse,
                            list_operation_contract: Some(ref receipt),
                            ..
                        } if receipt.mode
                            == mimi::core::mir::types::MirListOperationMode::Nested
                    )
                })
            })
        }));
    }

    #[test]
    fn nested_list_concat_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_nested_list_concat.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_scalar_collection_admission(&checked),
            mimi::core::mir::ScalarCollectionAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("nested List.concat must select the canonical default route");
        };
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        mimi::core::mir::MirInstructionKind::ListOp {
                            operation: mimi::core::mir::MirListOperation::Concat,
                            list_operation_contract: Some(ref receipt),
                            ..
                        } if receipt.mode
                            == mimi::core::mir::types::MirListOperationMode::Nested
                    )
                })
            })
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
    fn nested_generic_list_construct_stays_explicit_mir_only_without_legacy_fallback() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_list_nested_owned.mimi"
        ));
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("nested generic List construction must stay explicit-MIR-only");
        };
        assert!(
            reason.contains("S11 scalar collection candidate"),
            "{reason}"
        );
        assert!(
            reason.contains("outside the canonical Copy scalar")
                || reason.contains("outside the Copy scalar contract"),
            "{reason}"
        );
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
        // R6-1052 restatement: the prelude is filtered out of mixed coverage,
        // so this fixture's all-island graph (list literal + reverse/len
        // builtins + drop + arithmetic) now classifies as a Complete
        // admission; the route assertion below is unchanged.
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
        // R6-1052 restatement: a single-argument String println is a migrated
        // plain-scalar face, so the standalone boundary pin uses the still
        // unsupported multi-argument println shape.
        let source = r#"
            func main() -> i32 {
                println("legacy", 1)
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
        // R6-1052 restatement: the unsupported println is the multi-argument
        // shape; a single String argument now prints through the migrated
        // owned-String stdout face instead of flooring the graph.
        let source = r#"
            func main() -> i32 {
                let values = {4, 1, 1}
                println(contains(values, 1))
                println("legacy", 1)
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
        assert_eq!(
            LegacyRouteReason::ExportedAbiBodyCompatibility.as_str(),
            "exported-abi-body-compatibility"
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
    fn nested_option_tuple_variant_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/real_world/mir_nested_tuple_option.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_option_nested_tuple_variant_admission(&checked),
            mimi::core::mir::OptionNestedTupleVariantAdmission::CompleteCoverage
        );
        let route = select_default_route(&checked, &file);
        let DefaultMirRoute::Canonical(program) = route else {
            panic!("nested Option tuple variant must select canonical MIR: {route:?}");
        };
        assert!(mimi::core::mir::contains_option_nested_tuple_variant_candidate(&program));
        mimi::core::mir::validate_option_nested_tuple_variant_island(&program)
            .expect("nested Option tuple route must pass its capability gate");
    }

    #[test]
    fn nested_option_tuple_variant_with_non_projecting_pattern_fails_closed() {
        let (checked, file) = checked(
            "func main() -> i32 { let value: Option<(string, i32)> = Some((\"owned\", 41)); match value { Some(_) => 0, None => 0 } }",
        );
        assert_eq!(
            mimi::core::mir::classify_option_nested_tuple_variant_admission(&checked),
            mimi::core::mir::OptionNestedTupleVariantAdmission::MixedCoverage
        );
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("non-projecting nested Option tuple patterns must fail closed");
        };
        assert!(reason.contains("nested Option tuple"), "{reason}");
        assert!(!reason.contains("legacy"), "{reason}");
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

    #[test]
    fn generic_trait_specializations_use_default_canonical_route() {
        on_process_sized_stack(|| {
            let (checked, file) = checked(
                r#"
                    trait Marker<T> { func read() -> i32; }
                    impl Marker<i32> for List<i32> { func read() -> i32 { 11 } }
                    impl Marker<i64> for List<i64> { func read() -> i32 { 22 } }
                    impl Marker<string> for List<string> { func read() -> i32 { 33 } }
                    func main() -> i32 {
                        let numbers: List<i32> = [1]
                        let wide: List<i64> = [2]
                        let words: List<string> = ["x"]
                        println(words.read())
                        println(wide.read())
                        println(numbers.read())
                        drop(words)
                        0
                    }
                "#,
            );
            assert!(mimi::codegen::has_static_generic_trait_specialization_candidate(&checked));
            let synthetic_main = checked.flow("Main").expect("parser-synthesized Main flow");
            assert!(matches!(
                &synthetic_main.origin,
                mimi::core::Origin::RuntimeSystem { parent, rule, .. }
                    if parent.0 == "function:main" && rule == "progressive.main"
            ));
            let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
                panic!("concrete generic trait specializations must select canonical MIR");
            };
            let receipt = program
                .route_receipt(mimi::codegen::STATIC_GENERIC_TRAIT_SPECIALIZATION_ROUTE_PROFILE);
            assert_eq!(
                receipt.profile,
                mimi::codegen::STATIC_GENERIC_TRAIT_SPECIALIZATION_ROUTE_PROFILE
            );
            let bytecode =
                mimi::interp::bytecode::compile_mir_program_with_route_receipt(&program, &receipt)
                    .expect("default route bytecode consumer accepts the selected graph");
            let mut vm = mimi::interp::bytecode::BytecodeVM::new(bytecode);
            vm.enable_stdout_capture();
            assert!(matches!(
                vm.run_value().expect("default canonical route execution"),
                mimi::interp::Value::Int(0)
            ));
            assert_eq!(vm.take_stdout(), "33\n22\n11\n");
        });
    }

    #[test]
    fn native_data_export_disposition_preserves_static_generic_trait_candidate() {
        on_process_sized_stack(|| {
            let source = format!(
                "extern \"C\" const DATA_I32: i32 = 41\n{}",
                include_str!("../../tests/fixtures/mir_static_generic_trait_specialization.mimi")
            );
            let tokens = mimi::lexer::Lexer::new(&source)
                .tokenize()
                .expect("lex source with native data export");
            let file = mimi::parser::Parser::new_memory(
                tokens,
                "canonical-dispatch-tests",
                "native-data-export-static-trait",
                &source,
            )
            .expect("register a stable test SourceKey")
            .parse_file()
            .expect("parse source with native data export");
            let checked =
                mimi::core::check_program(&file).expect("check source with native data export");
            let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
            assert!(mimi::codegen::has_static_generic_trait_specialization_candidate(&checked));
            assert!(
                admission.has_candidate(),
                "the collection-shaped static-trait fixture is an independent route candidate: {admission:?}"
            );
            assert_eq!(
                route_receipt_profile_for_checked(&checked, "default-route"),
                mimi::codegen::STATIC_GENERIC_TRAIT_SPECIALIZATION_ROUTE_PROFILE
            );

            let selected = select_default_route(&checked, &file);
            let DefaultMirRoute::Canonical(program) = selected else {
                panic!("the static-trait candidate must retain its canonical route owner: {selected:?}");
            };
            let receipt = program
                .route_receipt(mimi::codegen::STATIC_GENERIC_TRAIT_SPECIALIZATION_ROUTE_PROFILE);
            receipt
                .validate_against_program(&program)
                .expect("selected static-trait receipt binds to the complete graph");
            assert_eq!(
                route_receipt_profile_for_checked(&checked, "default-route"),
                receipt.profile,
                "canonical disposition must report the selected static-trait receipt"
            );
            let data_receipt =
                mimi::core::native_data_export::checked_native_data_export_table_receipt(&checked)
                    .expect("the exported data declaration retains its checked table receipt");
            assert_eq!(data_receipt.export_count, 1);

            let explicit_program = build_canonical_program(&checked, &file)
                .expect("explicit MIR construction accepts the same complete graph");
            let explicit_receipt = explicit_program.route_receipt(receipt.profile.clone());
            explicit_receipt
                .validate_against_program(&explicit_program)
                .expect("explicit MIR receipt binds to the same source graph");
            assert_eq!(explicit_receipt, receipt);
        });
    }

    #[test]
    fn scalar_export_disposition_precedes_competing_scalar_ffi_route() {
        let (checked, file) = checked(
            r#"
                extern "C" { func llabs(value: i64) -> i64; }
                extern "C" func internal_export(value: i64) -> i64 { value + 1 }
                func main() -> i64 { llabs(internal_export(-17 as i64)) }
            "#,
        );
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
        assert!(admission.scalar_export);
        assert!(admission.scalar_ffi);
        assert_eq!(
            route_receipt_profile_for_checked(&checked, "default-route"),
            mimi::core::mir::SCALAR_EXPORT_ISLAND
        );

        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("the scalar export wrapper route must win over the scalar FFI route");
        };
        let receipt = program.route_receipt(mimi::core::mir::SCALAR_EXPORT_ISLAND);
        assert_eq!(receipt.profile, mimi::core::mir::SCALAR_EXPORT_ISLAND);
        assert_eq!(
            route_receipt_profile_for_checked(&checked, "default-route"),
            receipt.profile,
            "canonical disposition must report the selected scalar-export receipt"
        );
    }

    #[test]
    fn scalar_ffi_disposition_reports_its_selected_receipt_profile() {
        let (checked, file) = checked(
            r#"
                extern "C" { func llabs(value: i64) -> i64; }
                func main() -> i64 { llabs(-17 as i64) }
            "#,
        );
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
        assert!(admission.scalar_ffi);
        assert!(!admission.scalar_export);
        assert_eq!(
            route_receipt_profile_for_checked(&checked, "default-route"),
            "scalar-ffi-v1"
        );

        let selected = select_default_route(&checked, &file);
        let DefaultMirRoute::Canonical(program) = selected else {
            panic!("the scalar FFI source must select its canonical receipt: {selected:?}");
        };
        let receipt =
            program.route_receipt(mimi::core::mir::CanonicalMirRouteProfile::ScalarFfi.as_str());
        receipt
            .validate_against_program(&program)
            .expect("selected scalar-FFI receipt binds to the complete graph");
        assert_eq!(
            route_receipt_profile_for_checked(&checked, "default-route"),
            receipt.profile,
            "canonical disposition must report the selected scalar-FFI receipt"
        );
    }

    #[test]
    fn scalar_ffi_effect_exception_rejects_unreceipted_dynamic_helpers() {
        let (checked, file) = checked(
            r#"
                extern "C" { func llabs(value: i64) -> i64; }
                func apply(f: func(i64) -> i64, value: i64) -> i64 { f(value) }
                func identity(value: i64) -> i64 { value }
                func main() -> i64 { apply(identity, llabs(-17 as i64)) }
            "#,
        );
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
        assert!(admission.scalar_ffi);
        let reason = mimi::core::mir::unknown_effect_boundary_reason(&checked)
            .expect("the dynamic helper remains outside the scalar-FFI effect proof");
        assert!(reason.contains("apply"), "{reason}");
        assert!(reason.contains("unknown"), "{reason}");

        let DefaultMirRoute::Rejected(default_reason) = select_default_route(&checked, &file)
        else {
            panic!("scalar FFI must not hide the dynamic helper: {reason}");
        };
        assert!(
            default_reason.contains("MIR-COVERAGE-001"),
            "{default_reason}"
        );
        assert!(default_reason.contains("apply"), "{default_reason}");
        let explicit_reason = build_canonical_program(&checked, &file)
            .expect_err("explicit MIR construction shares the unknown-effect boundary");
        assert!(
            explicit_reason.contains("MIR-COVERAGE-001"),
            "{explicit_reason}"
        );
        assert!(explicit_reason.contains("apply"), "{explicit_reason}");
    }

    #[test]
    fn generic_trait_specialization_does_not_admit_a_user_flow() {
        let (checked, _file) = checked(
            r#"
                trait Marker<T> { func read() -> i32; }
                impl Marker<i32> for List<i32> { func read() -> i32 { 11 } }
                impl Marker<string> for List<string> { func read() -> i32 { 22 } }
                flow Counter {
                    state Ready { value: i32 }
                    transition tick(Ready) -> Ready {
                        return Ready { value: self.value + 1 }
                    }
                }
                func main() -> i32 {
                    let numbers: List<i32> = [1]
                    let words: List<string> = ["x"]
                    println(words.read())
                    println(numbers.read())
                    0
                }
            "#,
        );
        assert!(
            checked.flow("Counter").is_some(),
            "fixture contains a real Flow"
        );
        assert!(
            !mimi::codegen::has_static_generic_trait_specialization_candidate(&checked),
            "the static trait route may admit only the parser's exact synthetic Main Flow"
        );
    }

    #[test]
    fn generic_record_f64_projection_enters_canonical_default_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_record_projection_f64.mimi"
        ));
        assert_eq!(
            mimi::core::mir::classify_flat_copy_record_admission(&checked),
            mimi::core::mir::FlatCopyRecordAdmission::CompleteCoverage
        );
        let DefaultMirRoute::Canonical(program) = select_default_route(&checked, &file) else {
            panic!("generic Record<f64> projection must select canonical MIR");
        };
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            mimi::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.arity == 1 && contract.name == "value"
        )));
    }

    #[test]
    fn generic_record_list_projection_rejects_before_legacy_route() {
        let (checked, file) = checked(include_str!(
            "../../tests/fixtures/mir_native_generic_record_projection_list_rejected.mimi"
        ));
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("generic Record<List<List<i32>>> projection must fail closed");
        };
        assert!(reason.contains("generic record projection"), "{reason}");
        assert!(!reason.contains("legacy"), "{reason}");
    }

    const BRANCH_LOCAL_SET_DIAMOND_SOURCE: &str = r#"
        type Label { text: string }
        func main() -> i32 {
            let payload = Label { text: "shared" }
            let base = map_new()
            let flag = true
            if flag {
                let left = map_set(base, "left", payload)
                let left_size = map_size(left)
                drop(left)
            } else {
                let right = map_set(base, "right", payload)
                let right_size = map_size(right)
                drop(right)
            }
            drop(base)
            0
        }
    "#;

    #[test]
    fn branch_local_set_diamond_default_and_explicit_routes_share_one_receipt() {
        let (checked, file) = checked(BRANCH_LOCAL_SET_DIAMOND_SOURCE);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
        assert!(admission.ordinary_map_owned_any_v2_branch_local_set_diamond());
        assert_eq!(
            admission.ordinary_map_owned_any_v2,
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::BranchLocalPersistentSetDiamond
        );
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        for consumer_profile in [
            "run-canonical-v1",
            mimi::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE,
            "verify-canonical-v1",
        ] {
            assert_eq!(
                route_receipt_profile_for_checked(&checked, consumer_profile),
                mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE,
                "every CLI consumer must use the same dedicated profile"
            );
        }

        let materialized = materialize_canonical_route(&checked, &file)
            .expect("materialize the exact branch-local Set diamond");
        let profile =
            mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2BranchLocalSetDiamond;
        assert!(profile.is_admitted(materialized.admission));
        assert!(profile.is_materialized(&materialized));
        assert!(
            materialized.materialized_ordinary_map_owned_any_v2_branch_local_set_diamond_candidate
        );
        let materialized_receipt = materialized
            .ordinary_map_owned_any_v2_branch_local_set_diamond_route_receipt
            .as_ref()
            .expect("independent branch-local route receipt");
        assert_eq!(
            materialized_receipt.profile,
            mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE
        );
        materialized_receipt
            .validate_against_program(&materialized.program)
            .expect("receipt is bound to this canonical MIR graph");
        mimi::core::mir::validate_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
            &checked,
            &materialized.program,
        )
        .expect("Checker and MIR branch-local ownership proofs agree");

        let DefaultMirRoute::Canonical(default) = select_default_route(&checked, &file) else {
            panic!("the complete branch-local Set diamond must pass default preflight");
        };
        let explicit = build_canonical_program(&checked, &file)
            .expect("explicit --mir uses the same branch-local route preflight");
        let profile = route_receipt_profile_for_checked(
            &checked,
            mimi::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE,
        );
        let default_receipt = default.route_receipt(profile);
        let explicit_receipt = explicit.route_receipt(profile);
        assert_eq!(default_receipt, explicit_receipt);
        assert_eq!(&default_receipt, materialized_receipt);
        assert_eq!(default.canonical_digest(), explicit.canonical_digest());
    }

    #[test]
    fn branch_local_set_diamond_near_miss_is_rejected_without_opening_global_readiness() {
        let near_miss = BRANCH_LOCAL_SET_DIAMOND_SOURCE.replace(
            "            drop(base)",
            "            let extra = map_set(base, \"extra\", 47)\n            drop(extra)\n            drop(base)",
        );
        let (checked, file) = checked(&near_miss);
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
        assert!(!admission.ordinary_map_owned_any_v2_branch_local_set_diamond());
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        let DefaultMirRoute::Rejected(reason) = select_default_route(&checked, &file) else {
            panic!("the extra Set must fail closed instead of taking a legacy route");
        };
        assert!(reason.contains("MIR-COVERAGE-001"), "{reason}");
        let explicit = build_canonical_program(&checked, &file)
            .expect_err("explicit --mir must also reject a route outside the narrow proof");
        assert!(explicit.contains("MIR-COVERAGE-001"), "{explicit}");
    }
}
