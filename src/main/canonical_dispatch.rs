//! Program-level Canonical MIR route selection.
//!
//! The default entry points must make one capability decision for the whole
//! checked program.  A canonical backend is never attempted and then replaced
//! by legacy code on failure: programs either pass this preflight and use the
//! canonical route, or remain on the legacy route because their capability
//! set is not yet migrated. Once the bounded collection or exact non-Copy
//! `Option<string>` or managed direct-Result-call island is recognized, its
//! failure is an explicit rejection rather than compatibility fallback.

use std::collections::HashSet;

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

pub(crate) fn report_legacy_route(reason: LegacyRouteReason) {
    if std::env::var_os("MIMI_VERBOSE").is_some() || std::env::var_os("MIMI_ROUTE_CENSUS").is_some()
    {
        eprintln!("canonical route disposition: legacy ({})", reason.as_str());
    }
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
    let v2_admission =
        mimi::core::mir::classify_canonical_mir_route_admission(checked).ordinary_map_owned_any_v2;
    if v2_admission.has_profile_signal()
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
                    || is_ordinary_map_owner
                    || is_ordinary_map_owned_get
                    || is_scalar_export
                    || is_scalar_nested))
        {
            let profile = if is_map_any_owner {
                mimi::core::mir::MAP_ANY_OWNER_V1_ISLAND
            } else if is_ordinary_map_owned_any_v2 {
                mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND
            } else if is_ordinary_map_owned_get {
                mimi::core::mir::ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND
            } else if is_ordinary_map_owner {
                mimi::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_I32_STRING_V1_ISLAND
            } else if is_map_root {
                mimi::core::mir::MAP_ROOT_ISLAND
            } else if admission.scalar_actor {
                mimi::core::mir::SCALAR_ACTOR_ISLAND
            } else if is_scalar_export {
                mimi::core::mir::SCALAR_EXPORT_ISLAND
            } else if is_scalar_nested {
                mimi::core::mir::SCALAR_NESTED_ISLAND
            } else {
                "default-route"
            };
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
    // A source-qualified module call is itself a production boundary: once
    // the checker resolved `alias::function` to a concrete exported owner,
    // it must use that owner in the default route. Leaving this shape
    // outside every migration island would send the caller to bytecode even
    // though the same checked graph can be materialized as Canonical MIR.
    let module_alias_call_hint = checked.call_sites().values().any(|site| {
        matches!(site.kind, mimi::core::ResolvedCallKind::Function)
            && matches!(
                site.resolved_callee.as_ref(),
                Some(mimi::core::ir::ResolvedCallee::Function(_))
            )
            && site.callee.contains("::")
    });
    // Map owner receipts and scalar FFI currently have no combined route.
    // Check the same shared composition contract as MIR materialization
    // before Actor proof compatibility can route execution/build to legacy.
    // Verification retains the complete compatibility verifier so its
    // call-site FFI adapter can return named, untrusted results.
    if purpose == RoutePurpose::Execution {
        if let Some(error) = mimi::core::mir::scalar_ffi_map_owner_composition_failure(admission) {
            return DefaultMirRoute::Rejected(error.to_diagnostic().message);
        }
    }
    if admission.scalar_export {
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
            if !route.materialized_collection_operation_candidate && !flow_route_candidate {
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

/// Consumer preflight for the concrete-record ordinary Map/Any owner-v2
/// profile.  The profile owns its TypeDesc and receipt; no older MapRoot or
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
    // diagnostics; other Values uses remain staged. Empty Maps,
    // cross-function roots, general payload/key coverage, automatic cleanup,
    // and complete L1/L2/L3 evidence remain open. Runtime symbols and partial
    // consumers do not imply production readiness.
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
            "scalar FFI MIR verifier capability: {errors:?}"
        ));
    }
    let receipt = program.route_receipt("scalar-ffi-v1");
    if let Err(errors) =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&program, &receipt)
    {
        return DefaultMirRoute::Rejected(format!(
            "scalar FFI MIR bytecode capability: {errors:?}"
        ));
    }
    if let Err(errors) = mimi::codegen::mir::validate_mir_native(&program) {
        return DefaultMirRoute::Rejected(format!("scalar FFI MIR native capability: {errors:?}"));
    }
    match mimi::verifier::verify_mir_with_route_receipt(&program, &receipt, String::new()) {
        Ok(results)
            if mimi::verifier::canonical_execution_route_verifier_ready(&results, false, false) =>
        {
            DefaultMirRoute::Canonical(Box::new(program))
        }
        Ok(results) => DefaultMirRoute::Rejected(format!(
            "scalar FFI MIR verifier returned an unsupported or inconclusive result: {results:?}"
        )),
        Err(error) => DefaultMirRoute::Rejected(format!("scalar FFI MIR verifier pass: {error}")),
    }
}

/// Consumer preflight for direct-root, capture-free nested copy-scalar
/// callables.  This route has no external FFI receipt; the nested-callable
/// scope receipt itself is the canonical operation boundary.
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

    fn checked(source: &str) -> (CheckedProgram, File) {
        let tokens = mimi::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = mimi::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let checked = mimi::core::check_program(&file).expect("check");
        (checked, file)
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
                    && matches!(
                        site.resolved_callee.as_ref(),
                        Some(mimi::core::ir::ResolvedCallee::Function(_))
                    )
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
                    reason.contains(mimi::core::mir::MIR_ROUTE_MATERIALIZATION_ERROR_CODE),
                    "materialization rejection must preserve its registered code: {reason}"
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
            reason.contains("outside the canonical root-scope MIR slice"),
            "{reason}"
        );
        assert!(!reason.contains("legacy"), "{reason}");
    }

    #[test]
    fn scalar_ffi_nested_recursive_call_graph_is_rejected_without_legacy() {
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
        assert!(reason.contains("recursive MIR call graph"), "{reason}");
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
}
