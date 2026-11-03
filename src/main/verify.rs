use std::path::Path;

use crate::resolve_path;
use mimi::diagnostic::format::{colors_enabled, format_diagnostic_with_registry, strip_ansi};
use mimi::verifier::{TrustedSubsetDomain, VerifStatus};
use mimi::{lexer, loader};

fn verification_blocks_success(
    status: &VerifStatus,
    constraint_count: usize,
    message: &str,
    domain: Option<TrustedSubsetDomain>,
) -> bool {
    // NoObligations never blocks — no proof was attempted.
    if *status == VerifStatus::NoObligations {
        return false;
    }
    let no_contracts = status.is_inconclusive()
        && constraint_count == 0
        && matches!(message, "no contracts" | "no contracts to verify");
    // v0.31.25 验证域隔离：
    // - Contract-level NotInTrustedSubset → hard error (blocks)
    // - Body-level NotInTrustedSubset → doesn't block (treated as SolverUnknown)
    let body_level_not_in_subset = *status == VerifStatus::NotInTrustedSubset
        && matches!(domain, Some(TrustedSubsetDomain::Body));
    *status == VerifStatus::Disproven
        || (status.is_inconclusive() && !no_contracts && !body_level_not_in_subset)
}

pub(crate) fn verify(
    path: Option<&Path>,
    show_stats: bool,
    dump_z3: bool,
    mir: bool,
) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = mimi::path_safety::read_source_capped(&path)?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = loader::parser_for_path(tokens, &path)?.parse_file()?;

    let mut merged_file = if !file.imports.is_empty() {
        let base_dir = path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        let mut loader = loader::ModuleLoader::new(base_dir);
        loader.load_main_with_file(&path, file)?;
        loader.merge_all()?
    } else {
        file
    };

    // Keep verifier route construction identical to `run`, `build`, and
    // `mir` for programs that actually cross an extern boundary: the
    // checker-owned prelude declarations are part of that shared source graph
    // and are excluded only by the canonical MIR materializer.  Compatibility
    // programs without extern declarations retain their historical verifier
    // input, so prelude-only helper contracts do not turn a legacy request
    // into a new inconclusive proof batch.
    let has_extern_block = merged_file
        .items
        .iter()
        .any(|item| matches!(item, mimi::ast::Item::ExternBlock(_)));
    if has_extern_block {
        loader::merge_prelude_into(&mut merged_file);
    }

    // V-H8: typecheck before Z3 so ill-typed sources cannot produce
    // meaningless positive verification results.
    let mut checked_program = match mimi::core::check_program(&merged_file) {
        Ok(program) => program,
        Err(diags) => {
            let use_color = colors_enabled();
            let src_ref = Some(source.as_str());
            let filename = path.display().to_string();
            for d in &diags {
                let formatted =
                    format_diagnostic_with_registry(d, &merged_file.sources, src_ref, &filename);
                if use_color {
                    eprint!("{}", formatted);
                } else {
                    eprint!("{}", strip_ansi(&formatted));
                }
            }
            return Err(format!(
                "typecheck failed before verify ({} diagnostic(s))",
                diags.len()
            ));
        }
    };

    // `run`, `build`, and `mimi mir` include the standard prelude before
    // constructing their checked graph. Legacy `verify` historically omits
    // it when no extern block is present, but the Actor materializer's MIR
    // TypeCatalog is derived from the complete CheckedProgram type table
    // even while prelude callables themselves are excluded by source ID.
    // Give default Actor and MapRoot verifier routes the same checked graph
    // (and receipt digest) as those shared consumers; compatibility verifier
    // inputs keep the historical prelude boundary.
    let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked_program);
    if !has_extern_block && admission.has_candidate() {
        loader::merge_prelude_into(&mut merged_file);
        checked_program = mimi::core::check_program(&merged_file).map_err(|diags| {
            format!(
                "typecheck failed after loading the canonical prelude ({} diagnostics): {diags:?}",
                diags.len()
            )
        })?;
    }

    // P1-24: compute source hash for ProofArtifact tamper detection.
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let source_hash_for_provenance = source_hash.clone();

    let mut default_legacy_reason = None;
    let canonical = if mir {
        if dump_z3 {
            return Err("--dump-z3 is not available with --mir".into());
        }
        if let Some(reason) =
            crate::canonical_dispatch::scalar_ffi_declaration_boundary_diagnostic(&checked_program)
        {
            return Err(format!("canonical MIR verifier input rejected: {reason}"));
        }
        Some(
            crate::canonical_dispatch::build_canonical_program(&checked_program, &merged_file)
                .map_err(|error| format!("canonical MIR verifier input rejected: {error}"))?,
        )
    } else if dump_z3 {
        None
    } else {
        match crate::canonical_dispatch::select_default_verification_route(
            &checked_program,
            &merged_file,
        ) {
            crate::canonical_dispatch::DefaultMirRoute::Canonical(canonical) => Some(*canonical),
            crate::canonical_dispatch::DefaultMirRoute::Legacy(reason) => {
                crate::canonical_dispatch::report_legacy_route(reason);
                default_legacy_reason = Some(reason);
                None
            }
            crate::canonical_dispatch::DefaultMirRoute::Rejected(reason) => {
                return Err(format!("default Canonical MIR route rejected: {reason}"));
            }
        }
    };

    // Keep the checker-owned route receipt available for the optional CLI
    // provenance line below.  The verifier consumes the same receipt object;
    // this is only a read-only view after verification, never a second route
    // construction.
    // A selected MIR route still goes through the checked library boundary
    // when Z3 is unavailable. That boundary reports owner-scoped
    // InfrastructureError/NITS rows without constructing a solver; direct
    // MIR consumption here would otherwise turn the same request into a
    // top-level SolverSession initialization error.
    let canonical_mir_available = canonical
        .as_ref()
        .is_some_and(|_| mimi::verifier::is_z3_available());
    let canonical_mir_selected = canonical.is_some();
    let canonical_receipt_profile = crate::canonical_dispatch::route_receipt_profile_for_checked(
        &checked_program,
        "verify-canonical-v1",
    );
    let get_or_default_string_admitted =
        mimi::core::mir::classify_canonical_mir_route_admission(&checked_program)
            .ordinary_map_owned_any_v2_get_or_default_string();
    let get_or_default_string_receipt_profile = canonical_receipt_profile
        == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_PROFILE;
    if get_or_default_string_admitted != get_or_default_string_receipt_profile {
        return Err(format!(
            "canonical GetOrDefault<String> verifier route rejected: Checker admission/profile mismatch (admitted={get_or_default_string_admitted}, profile={canonical_receipt_profile})"
        ));
    }
    if get_or_default_string_receipt_profile {
        let program = canonical.as_ref().ok_or_else(|| {
            "canonical GetOrDefault<String> verifier route rejected: Checker admission has no canonical MIR graph to validate".to_string()
        })?;
        crate::canonical_dispatch::preflight_ordinary_map_owned_any_v2_get_or_default_string(
            &checked_program,
            program,
        )
        .map_err(|reason| {
            format!("canonical GetOrDefault<String> verifier route rejected: {reason}")
        })?;
    }
    let checked_get_admitted =
        mimi::core::mir::classify_canonical_mir_route_admission(&checked_program)
            .ordinary_map_owned_any_v2
            .is_checked_get();
    let checked_get_receipt_profile =
        canonical_receipt_profile == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE;
    if checked_get_admitted != checked_get_receipt_profile {
        return Err(format!(
            "canonical CheckedGet verifier route rejected: Checker admission/profile mismatch (admitted={checked_get_admitted}, profile={canonical_receipt_profile})"
        ));
    }
    if checked_get_receipt_profile {
        let program = canonical.as_ref().ok_or_else(|| {
            "canonical CheckedGet verifier route rejected: Checker admission has no canonical MIR receipt to validate".to_string()
        })?;
        crate::canonical_dispatch::preflight_ordinary_map_owned_any_v2_checked_get(
            &checked_program,
            program,
        )
        .map_err(|reason| format!("canonical CheckedGet verifier route rejected: {reason}"))?;
    }
    let zero_miss_i32_admitted =
        mimi::core::mir::classify_canonical_mir_route_admission(&checked_program)
            .ordinary_map_owned_any_v2_zero_miss_i32();
    let zero_miss_i32_receipt_profile = canonical_receipt_profile
        == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE;
    if zero_miss_i32_admitted != zero_miss_i32_receipt_profile {
        return Err(format!(
            "canonical ZeroMissI32 verifier route rejected: Checker admission/profile mismatch (admitted={zero_miss_i32_admitted}, profile={canonical_receipt_profile})"
        ));
    }
    if zero_miss_i32_receipt_profile {
        let program = canonical.as_ref().ok_or_else(|| {
            "canonical ZeroMissI32 verifier route rejected: Checker admission has no canonical MIR receipt to validate".to_string()
        })?;
        crate::canonical_dispatch::preflight_ordinary_map_owned_any_v2_zero_miss_i32(
            &checked_program,
            program,
        )
        .map_err(|reason| format!("canonical ZeroMissI32 verifier route rejected: {reason}"))?;
    }
    let removed_key_checked_miss_admitted =
        mimi::core::mir::classify_canonical_mir_route_admission(&checked_program)
            .ordinary_map_owned_any_v2_removed_key_checked_miss();
    let removed_key_checked_miss_receipt_profile = canonical_receipt_profile
        == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE;
    if removed_key_checked_miss_admitted != removed_key_checked_miss_receipt_profile {
        return Err(format!(
            "canonical RemovedKeyCheckedMiss verifier route rejected: Checker admission/profile mismatch (admitted={removed_key_checked_miss_admitted}, profile={canonical_receipt_profile})"
        ));
    }
    if removed_key_checked_miss_receipt_profile {
        let program = canonical.as_ref().ok_or_else(|| {
            "canonical RemovedKeyCheckedMiss verifier route rejected: Checker admission has no canonical MIR receipt to validate".to_string()
        })?;
        crate::canonical_dispatch::preflight_ordinary_map_owned_any_v2_removed_key_checked_miss(
            &checked_program,
            program,
        )
        .map_err(|reason| {
            format!("canonical RemovedKeyCheckedMiss verifier route rejected: {reason}")
        })?;
    }
    let keys_snapshot_admitted =
        mimi::core::mir::classify_canonical_mir_route_admission(&checked_program)
            .ordinary_map_owned_any_v2
            == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::KeysSnapshot;
    let keys_snapshot_receipt_profile = canonical_receipt_profile
        == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE;
    if keys_snapshot_admitted != keys_snapshot_receipt_profile {
        return Err(format!(
            "canonical KeysSnapshot verifier route rejected: Checker admission/profile mismatch (admitted={keys_snapshot_admitted}, profile={canonical_receipt_profile})"
        ));
    }
    if keys_snapshot_receipt_profile {
        let program = canonical.as_ref().ok_or_else(|| {
            "canonical KeysSnapshot verifier route rejected: Checker admission has no canonical MIR receipt to validate".to_string()
        })?;
        crate::canonical_dispatch::preflight_ordinary_map_owned_any_v2_keys_snapshot(
            &checked_program,
            program,
        )
        .map_err(|reason| format!("canonical KeysSnapshot verifier route rejected: {reason}"))?;
    }
    let values_producer_caller_admitted =
        mimi::core::mir::classify_canonical_mir_route_admission(&checked_program)
            .ordinary_map_owned_any_v2_values_producer_caller();
    let values_producer_caller_receipt_profile = canonical_receipt_profile
        == mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesProducerCaller
            .as_str();
    if values_producer_caller_admitted != values_producer_caller_receipt_profile {
        return Err(format!(
            "canonical ValuesProducerCaller verifier route rejected: Checker admission/profile mismatch (admitted={values_producer_caller_admitted}, profile={canonical_receipt_profile})"
        ));
    }
    if values_producer_caller_receipt_profile {
        let program = canonical.as_ref().ok_or_else(|| {
            "canonical ValuesProducerCaller verifier route rejected: Checker admission has no canonical MIR receipt to validate".to_string()
        })?;
        crate::canonical_dispatch::preflight_ordinary_map_owned_any_v2_values_producer_caller(
            &checked_program,
            program,
        )
        .map_err(|reason| {
            format!("canonical ValuesProducerCaller verifier route rejected: {reason}")
        })?;
    }
    let values_crosscall_string_caller_admitted =
        mimi::core::mir::classify_canonical_mir_route_admission(&checked_program)
            .ordinary_map_owned_any_v2_values_crosscall_string_caller_v1();
    let values_crosscall_string_caller_receipt_profile = canonical_receipt_profile
        == mimi::core::mir::CanonicalMirRouteProfile::
            OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller
            .as_str();
    if values_crosscall_string_caller_admitted != values_crosscall_string_caller_receipt_profile {
        return Err(format!(
            "canonical ValuesCrosscallStringCaller verifier route rejected: Checker admission/profile mismatch (admitted={values_crosscall_string_caller_admitted}, profile={canonical_receipt_profile})"
        ));
    }
    if values_crosscall_string_caller_receipt_profile {
        let program = canonical.as_ref().ok_or_else(|| {
            "canonical ValuesCrosscallStringCaller verifier route rejected: Checker admission has no canonical MIR receipt to validate".to_string()
        })?;
        crate::canonical_dispatch::preflight_ordinary_map_owned_any_v2_values_crosscall_string_caller(
            &checked_program,
            program,
        )
        .map_err(|reason| {
            format!("canonical ValuesCrosscallStringCaller verifier route rejected: {reason}")
        })?;
    }
    let values_crosscall_string_caller_index1_admitted =
        mimi::core::mir::classify_canonical_mir_route_admission(&checked_program)
            .ordinary_map_owned_any_v2_values_crosscall_string_caller_index1();
    let values_crosscall_string_caller_index1_receipt_profile = canonical_receipt_profile
        == mimi::core::mir::CanonicalMirRouteProfile::
            OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1
            .as_str();
    if values_crosscall_string_caller_index1_admitted
        != values_crosscall_string_caller_index1_receipt_profile
    {
        return Err(format!(
            "canonical ValuesCrosscallStringCallerIndex1 verifier route rejected: Checker admission/profile mismatch (admitted={values_crosscall_string_caller_index1_admitted}, profile={canonical_receipt_profile})"
        ));
    }
    if values_crosscall_string_caller_index1_receipt_profile {
        let program = canonical.as_ref().ok_or_else(|| {
            "canonical ValuesCrosscallStringCallerIndex1 verifier route rejected: Checker admission has no canonical MIR receipt to validate".to_string()
        })?;
        crate::canonical_dispatch::preflight_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1(
            &checked_program,
            program,
        )
        .map_err(|reason| {
            format!(
                "canonical ValuesCrosscallStringCallerIndex1 verifier route rejected: {reason}"
            )
        })?;
    }
    let map_root_producer_caller_admitted =
        mimi::core::mir::classify_canonical_mir_route_admission(&checked_program)
            .ordinary_map_owned_any_v2_map_root_producer_caller();
    let map_root_producer_caller_receipt_profile = canonical_receipt_profile
        == mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootProducerCaller
            .as_str();
    if map_root_producer_caller_admitted != map_root_producer_caller_receipt_profile {
        return Err(format!(
            "canonical MapRootProducerCaller verifier route rejected: Checker admission/profile mismatch (admitted={map_root_producer_caller_admitted}, profile={canonical_receipt_profile})"
        ));
    }
    if map_root_producer_caller_receipt_profile {
        let program = canonical.as_ref().ok_or_else(|| {
            "canonical MapRootProducerCaller verifier route rejected: Checker admission has no canonical MIR receipt to validate".to_string()
        })?;
        crate::canonical_dispatch::preflight_ordinary_map_owned_any_v2_map_root_producer_caller(
            &checked_program,
            program,
        )
        .map_err(|reason| {
            format!("canonical MapRootProducerCaller verifier route rejected: {reason}")
        })?;
    }
    let map_root_get_any_caller_admitted =
        mimi::core::mir::classify_canonical_mir_route_admission(&checked_program)
            .ordinary_map_owned_any_v2_map_root_get_any_caller();
    let map_root_get_any_caller_receipt_profile = canonical_receipt_profile
        == mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootGetAnyCaller
            .as_str();
    if map_root_get_any_caller_admitted != map_root_get_any_caller_receipt_profile {
        return Err(format!(
            "canonical MapRootGetAnyCaller verifier route rejected: Checker admission/profile mismatch (admitted={map_root_get_any_caller_admitted}, profile={canonical_receipt_profile})"
        ));
    }
    if map_root_get_any_caller_receipt_profile {
        let program = canonical.as_ref().ok_or_else(|| {
            "canonical MapRootGetAnyCaller verifier route rejected: Checker admission has no canonical MIR receipt to validate".to_string()
        })?;
        crate::canonical_dispatch::preflight_ordinary_map_owned_any_v2_map_root_get_any_caller(
            &checked_program,
            program,
        )
        .map_err(|reason| {
            format!("canonical MapRootGetAnyCaller verifier route rejected: {reason}")
        })?;
    }
    let is_empty_admitted =
        mimi::core::mir::classify_canonical_mir_route_admission(&checked_program)
            .ordinary_map_owned_any_v2
            == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::IsEmpty;
    let is_empty_receipt_profile =
        canonical_receipt_profile == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE;
    if is_empty_admitted != is_empty_receipt_profile {
        return Err(format!(
            "canonical IsEmpty verifier route rejected: Checker admission/profile mismatch (admitted={is_empty_admitted}, profile={canonical_receipt_profile})"
        ));
    }
    if is_empty_receipt_profile {
        let program = canonical.as_ref().ok_or_else(|| {
            "canonical IsEmpty verifier route rejected: Checker admission has no canonical MIR receipt to validate".to_string()
        })?;
        crate::canonical_dispatch::preflight_ordinary_map_owned_any_v2_is_empty(
            &checked_program,
            program,
        )
        .map_err(|reason| format!("canonical IsEmpty verifier route rejected: {reason}"))?;
    }
    let record_unit_admitted =
        mimi::core::mir::classify_canonical_mir_route_admission(&checked_program)
            .ordinary_map_owned_any_v2
            == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::RecordUnit;
    let record_unit_receipt_profile =
        canonical_receipt_profile == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE;
    if record_unit_admitted != record_unit_receipt_profile {
        return Err(format!(
            "canonical RecordUnit verifier route rejected: Checker admission/profile mismatch (admitted={record_unit_admitted}, profile={canonical_receipt_profile})"
        ));
    }
    if record_unit_receipt_profile {
        let program = canonical.as_ref().ok_or_else(|| {
            "canonical RecordUnit verifier route rejected: Checker admission has no canonical MIR receipt to validate".to_string()
        })?;
        crate::canonical_dispatch::preflight_ordinary_map_owned_any_v2_record_unit(
            &checked_program,
            program,
        )
        .map_err(|reason| format!("canonical RecordUnit verifier route rejected: {reason}"))?;
    }
    let map_set_drop_control_admitted =
        mimi::core::mir::classify_canonical_mir_route_admission(&checked_program)
            .ordinary_map_owned_any_v2
            == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControl;
    let map_set_drop_control_receipt_profile = canonical_receipt_profile
        == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE;
    if map_set_drop_control_admitted != map_set_drop_control_receipt_profile {
        return Err(format!(
            "canonical MapSetDropControl verifier route rejected: Checker admission/profile mismatch (admitted={map_set_drop_control_admitted}, profile={canonical_receipt_profile})"
        ));
    }
    if map_set_drop_control_receipt_profile {
        let program = canonical.as_ref().ok_or_else(|| {
            "canonical MapSetDropControl verifier route rejected: Checker admission has no canonical MIR receipt to validate".to_string()
        })?;
        crate::canonical_dispatch::preflight_ordinary_map_owned_any_v2_map_set_drop_control(
            &checked_program,
            program,
        )
        .map_err(|reason| {
            format!("canonical MapSetDropControl verifier route rejected: {reason}")
        })?;
    }
    let branch_local_set_diamond_admitted =
        mimi::core::mir::classify_canonical_mir_route_admission(&checked_program)
            .ordinary_map_owned_any_v2_branch_local_set_diamond();
    let branch_local_set_diamond_receipt_profile = canonical_receipt_profile
        == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE;
    if branch_local_set_diamond_admitted != branch_local_set_diamond_receipt_profile {
        return Err(format!(
            "canonical BranchLocalSetDiamond verifier route rejected: Checker admission/profile mismatch (admitted={branch_local_set_diamond_admitted}, profile={canonical_receipt_profile})"
        ));
    }
    if branch_local_set_diamond_receipt_profile {
        let program = canonical.as_ref().ok_or_else(|| {
            "canonical BranchLocalSetDiamond verifier route rejected: Checker admission has no canonical MIR receipt to validate".to_string()
        })?;
        crate::canonical_dispatch::preflight_ordinary_map_owned_any_v2_branch_local_set_diamond(
            &checked_program,
            program,
        )
        .map_err(|reason| {
            format!("canonical BranchLocalSetDiamond verifier route rejected: {reason}")
        })?;
    }
    let clone_isolation_admitted =
        mimi::core::mir::classify_canonical_mir_route_admission(&checked_program)
            .ordinary_map_owned_any_v2
            == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::CloneIsolation;
    let clone_isolation_receipt_profile = canonical_receipt_profile
        == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE;
    if clone_isolation_admitted != clone_isolation_receipt_profile {
        return Err(format!(
            "canonical CloneIsolation verifier route rejected: Checker admission/profile mismatch (admitted={clone_isolation_admitted}, profile={canonical_receipt_profile})"
        ));
    }
    if clone_isolation_receipt_profile {
        let program = canonical.as_ref().ok_or_else(|| {
            "canonical CloneIsolation verifier route rejected: Checker admission has no canonical MIR receipt to validate".to_string()
        })?;
        crate::canonical_dispatch::preflight_ordinary_map_owned_any_v2_clone_isolation(
            &checked_program,
            program,
        )
        .map_err(|reason| format!("canonical CloneIsolation verifier route rejected: {reason}"))?;
    }
    let persistent_root_alias_replacement_admitted =
        mimi::core::mir::classify_canonical_mir_route_admission(&checked_program)
            .ordinary_map_owned_any_v2_persistent_root_alias_replacement();
    let persistent_root_alias_replacement_receipt_profile = canonical_receipt_profile
        == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE;
    if persistent_root_alias_replacement_admitted
        != persistent_root_alias_replacement_receipt_profile
    {
        return Err(format!(
            "canonical PersistentRootAliasReplacement verifier route rejected: Checker admission/profile mismatch (admitted={persistent_root_alias_replacement_admitted}, profile={canonical_receipt_profile})"
        ));
    }
    if persistent_root_alias_replacement_receipt_profile {
        let program = canonical.as_ref().ok_or_else(|| {
            "canonical PersistentRootAliasReplacement verifier route rejected: Checker admission has no canonical MIR receipt to validate".to_string()
        })?;
        crate::canonical_dispatch::preflight_ordinary_map_owned_any_v2_persistent_root_alias_replacement(
            &checked_program,
            program,
        )
        .map_err(|reason| {
            format!("canonical PersistentRootAliasReplacement verifier route rejected: {reason}")
        })?;
    }
    let values_local_index_i32_admitted =
        mimi::core::mir::classify_canonical_mir_route_admission(&checked_program)
            .ordinary_map_owned_any_v2_values_local_index_i32();
    let values_local_index_i32_receipt_profile = canonical_receipt_profile
        == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_PROFILE;
    if values_local_index_i32_admitted != values_local_index_i32_receipt_profile {
        return Err(format!(
            "canonical ValuesLocalIndexI32 verifier route rejected: Checker admission/profile mismatch (admitted={values_local_index_i32_admitted}, profile={canonical_receipt_profile})"
        ));
    }
    if values_local_index_i32_receipt_profile {
        let program = canonical.as_ref().ok_or_else(|| {
            "canonical ValuesLocalIndexI32 verifier route rejected: Checker admission has no canonical MIR receipt to validate".to_string()
        })?;
        mimi::core::mir::validate_ordinary_map_owned_any_v2_values_local_index_i32_route(
            &checked_program,
            program,
        )
        .map_err(|reason| {
            format!("canonical ValuesLocalIndexI32 verifier route rejected: {reason}")
        })?;
    }
    let values_heterogeneous_string_index1_admitted =
        mimi::core::mir::classify_canonical_mir_route_admission(&checked_program)
            .ordinary_map_owned_any_v2_values_heterogeneous_string_index1();
    let values_heterogeneous_string_index1_receipt_profile = canonical_receipt_profile
        == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_HETEROGENEOUS_STRING_INDEX1_PROFILE;
    if values_heterogeneous_string_index1_admitted
        != values_heterogeneous_string_index1_receipt_profile
    {
        return Err(format!(
            "canonical ValuesHeterogeneousStringIndex1 verifier route rejected: Checker admission/profile mismatch (admitted={values_heterogeneous_string_index1_admitted}, profile={canonical_receipt_profile})"
        ));
    }
    if values_heterogeneous_string_index1_receipt_profile {
        let program = canonical.as_ref().ok_or_else(|| {
            "canonical ValuesHeterogeneousStringIndex1 verifier route rejected: Checker admission has no canonical MIR receipt to validate".to_string()
        })?;
        mimi::core::mir::validate_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route(
            &checked_program,
            program,
        )
        .map_err(|reason| {
            format!(
                "canonical ValuesHeterogeneousStringIndex1 verifier route rejected: {reason}"
            )
        })?;
    }
    if canonical_receipt_profile == mimi::core::mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE {
        if let Some(program) = canonical.as_ref() {
            mimi::core::mir::validate_map_empty_root_root_queries_v1_route(
                &checked_program,
                program,
            )?;
        }
    }
    if canonical_receipt_profile
        == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE
    {
        if let Some(program) = canonical.as_ref() {
            mimi::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_route(
                &checked_program,
                program,
            )
            .map_err(|reason| {
                format!("canonical ValuesMixedSnapshotRemove verifier route rejected: {reason}")
            })?;
        }
    }
    if canonical_receipt_profile
        == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE
    {
        if let Some(program) = canonical.as_ref() {
            mimi::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_route(
                &checked_program,
                program,
            )
            .map_err(|reason| {
                format!("canonical ValuesMixedSnapshot verifier route rejected: {reason}")
            })?;
        }
    }
    if std::env::var_os("MIMI_ROUTE_CENSUS").is_some() {
        if let Some(program) = canonical.as_ref() {
            let admission_mode = if mir { "explicit-mir" } else { "default" };
            eprintln!(
                "MIMI_ROUTE_CENSUS route-admission consumer=verify mode={} checker_profile={} mir_digest={} receipt_identity_digest={}",
                admission_mode,
                canonical_receipt_profile,
                program.canonical_digest(),
                program
                    .route_receipt(canonical_receipt_profile)
                    .route_identity_digest(),
            );
        }
    }
    let canonical_receipt = canonical
        .as_ref()
        .filter(|_| canonical_mir_available)
        .map(|program| program.route_receipt(canonical_receipt_profile));

    let mut results = if let Some(canonical) = canonical {
        // The shared dispatcher checks the complete graph. Executable inputs
        // also preflight execution consumers; contract libraries need no main.
        // With a solver, MIR validates the final boundary. Without one, the
        // checked route preserves the owner-scoped infrastructure results.
        if canonical_mir_available {
            let receipt = canonical.route_receipt(canonical_receipt_profile);
            if canonical_receipt_profile
                == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_checked_get_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_PROFILE
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_get_or_default_string_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_I32_PROFILE
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_get_or_default_i32_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_keys_snapshot_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_dynamic_keys_get_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::CanonicalMirRouteProfile::
                    OrdinaryMapOwnedAnyV2ValuesProducerCaller
                    .as_str()
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_values_producer_caller_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::CanonicalMirRouteProfile::
                    OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1
                    .as_str()
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::CanonicalMirRouteProfile::
                    OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller
                    .as_str()
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::CanonicalMirRouteProfile::
                    OrdinaryMapOwnedAnyV2MapRootProducerCaller
                    .as_str()
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_map_root_producer_caller_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::CanonicalMirRouteProfile::
                    OrdinaryMapOwnedAnyV2MapRootGetAnyCaller
                    .as_str()
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
                    &checked_program,
                    &canonical,
                    source_hash,
                    &receipt,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_HETEROGENEOUS_STRING_INDEX1_PROFILE
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_values_heterogeneous_string_index1_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_clone_isolation_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE
            {
                mimi::verifier::verify_mir_with_map_empty_root_root_queries_v1_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_is_empty_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_map_set_drop_control_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE
            {
                mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
                    &checked_program,
                    &canonical,
                    &receipt,
                    source_hash,
                )?
            } else if canonical_receipt_profile
                == mimi::core::mir::ORDINARY_MAP_NEW_RESULT_EXECUTION_PROFILE
            {
                // The execution graph is the contract-free L1 projection; the
                // verifier re-derives its own L2 projection (contracts, proof
                // worklist, source census) from the same CheckedProgram and
                // binds its receipts to that graph.
                let l2 = mimi::core::mir::materialize_ordinary_map_new_result_verifier_route(
                    &checked_program,
                )
                .map_err(|reason| {
                    format!("canonical Map NewResult verifier route rejected: {reason}")
                })?;
                let l2_receipt = l2
                    .route_receipt(mimi::core::mir::ORDINARY_MAP_NEW_RESULT_L2_VERIFIER_PROFILE);
                mimi::verifier::verify_mir_with_ordinary_map_new_result_l2_verifier_route(
                    &checked_program,
                    &l2,
                    &l2_receipt,
                    source_hash,
                )?
            } else {
                mimi::verifier::verify_mir_with_route_receipt(&canonical, &receipt, source_hash)?
            }
        } else {
            mimi::verifier::verify_checked(&checked_program, source_hash)?
        }
    } else if dump_z3 {
        // --dump-z3 needs access to Verifier::dump_smt2 after verification,
        // which the Flow state machine doesn't expose. Keep direct for this case.
        let mut verifier = mimi::verifier::Verifier::new()?;
        verifier.set_source_hash(source_hash);
        eprintln!("; Z3 SMT-LIB2 dump for {}", path.display());
        eprintln!("; (verification will proceed after dump)");
        let results = verifier.verify_checked(&checked_program);
        if let Some(smt2) = verifier.dump_smt2() {
            eprintln!("{}", smt2);
        } else {
            eprintln!("; (no Z3 assertions)");
        }
        results
    } else {
        // 0.34.44 (ADR-008 §3): the CLI main judgment is DUAL-engine —
        // resolved (primary) + flow/VIR (demoted math: channel) with
        // fail-closed divergence (E0439). Neither engine is trusted alone
        // when their verdict classes disagree.
        mimi::verifier::verify_checked_dual(&checked_program, source_hash)?
    };

    if canonical_mir_selected
        && !canonical_mir_available
        && !results.is_empty()
        && results.iter().all(|result| result.artifact.is_none())
        && results.iter().all(|result| !result.status.is_definitive())
        && (std::env::var_os("MIMI_VERBOSE").is_some()
            || std::env::var_os("MIMI_ROUTE_CENSUS").is_some())
    {
        eprintln!(
            "canonical verifier result: MIR candidate retained; Z3 unavailable; no proof artifacts or proof verdicts emitted; owner-scoped results follow"
        );
    }

    // The canonical scalar FFI verifier proves call-site preconditions from
    // shared MIR receipts. A program containing an exported C body stays on
    // the explicit compatibility route because MIR has no export-wrapper ABI
    // receipt. Keep the general verifier's results while reporting the FFI
    // obligation's separate fail-closed boundary.
    if default_legacy_reason
        == Some(crate::canonical_dispatch::LegacyRouteReason::ExportedAbiBodyCompatibility)
        || (dump_z3
            && mimi::core::mir::exported_abi_body_boundary_reason(&checked_program).is_some())
    {
        match mimi::verifier::verify_ffi_checked_with_source_hash(
            &checked_program,
            blake3::hash(source.as_bytes()).to_hex().to_string(),
        ) {
            Ok(ffi_results) => results.extend(ffi_results),
            Err(message) => results.push(mimi::verifier::VerificationResult {
                func_name: "extern call-site obligations".into(),
                status: if message.starts_with("TOOL-RESOLUTION-001:") {
                    mimi::verifier::VerifStatus::InfrastructureError
                } else {
                    mimi::verifier::VerifStatus::NotInTrustedSubset
                },
                message: format!("FFI call-site verification failed closed: {message}"),
                diagnostic: None,
                duration_us: 0,
                constraint_count: 0,
                artifact: None,
                trusted_subset_domain: Some(mimi::verifier::TrustedSubsetDomain::Contract),
            }),
        }
    }

    // MIR proof results keep canonical subject identities. Source positions
    // are presentation metadata from the checked source registry, and do not
    // participate in the proof. Preserve existing precise FFI diagnostics.
    if canonical_receipt.is_some() {
        for result in &mut results {
            let Some(diagnostic) = result.diagnostic.as_mut() else {
                continue;
            };
            if diagnostic.span != mimi::span::Span::UNKNOWN {
                continue;
            }
            let subject = diagnostic
                .origin
                .as_ref()
                .and_then(|origin| origin.parent_node_id.as_ref());
            if let Some(meta) = subject
                .and_then(|subject| {
                    checked_program
                        .node_meta()
                        .get(&mimi::core::NodeId(subject.clone()))
                })
                .or_else(|| {
                    checked_program
                        .node_meta()
                        .get(&mimi::core::NodeId(result.func_name.clone()))
                })
            {
                diagnostic.span = meta.origin.user_span();
            }
        }
    }

    // `verify` historically exposed only human verdict text, which made it
    // impossible for a CLI caller to compare the proof artifact with a route
    // receipt emitted by `mimi mir --receipt`.  Under the existing verbose
    // opt-in, expose the immutable MIR/ABI/FFI identity and source provenance
    // carried by the actual result artifact.  Do not synthesize a receipt or
    // print anything for the compatibility verifier path.
    if std::env::var_os("MIMI_VERBOSE").is_some() {
        if let Some(receipt) = canonical_receipt.as_ref() {
            // A no-contract canonical program legitimately has no proof
            // artifact, but it still crossed the same Checker/MIR route and
            // must expose that receipt for CLI evidence and backend parity.
            // When an artifact exists, retain its immutable source hash;
            // otherwise the already computed source hash is the only valid
            // provenance value (never synthesize a second route identity).
            let artifact_source_hash = results.iter().find_map(|result| {
                result
                    .artifact
                    .as_ref()
                    .filter(|artifact| artifact.engine == mimi::verifier::ProofArtifact::ENGINE_MIR)
                    .map(|artifact| artifact.source_hash.clone())
            });
            let provenance_source_hash = artifact_source_hash
                .as_deref()
                .unwrap_or(&source_hash_for_provenance);
            {
                eprintln!(
                    "canonical MIR verifier provenance: profile={} mir_digest={} type_desc_digest={} abi_digest={} ffi_digest={} ownership_digest={} flow_transition_digest={} source_hash={} route_identity_digest={}",
                    receipt.profile,
                    receipt.mir_digest,
                    receipt.type_desc_digest,
                    receipt.abi_digest,
                    receipt.ffi_digest,
                    receipt.ownership_digest,
                    receipt.flow_transition_digest,
                    provenance_source_hash,
                    receipt.route_identity_digest(),
                );
            }
        }
    }

    if results.is_empty() {
        println!("No contracts to verify in {}", path.display());
    } else {
        let use_color = colors_enabled();
        let src_ref = Some(source.as_str());
        let filename = &path.display().to_string();
        let mut all_passed = true;
        let mut total_duration_us: u64 = 0;
        let mut total_constraints: usize = 0;

        // Show per-function stats table if --stats is set
        if show_stats {
            println!(
                "{:30} {:>10} {:>12} {:>8}  {}",
                "Function", "Status", "Constraints", "Time", "Proof profile / scope"
            );
            println!("{}", "-".repeat(110));
        }

        for r in &results {
            let icon = if r.status == VerifStatus::Proven {
                "\x1b[32m✓\x1b[0m"
            } else if r.status == VerifStatus::Disproven {
                "\x1b[31m✗\x1b[0m"
            } else {
                "\x1b[33m?\x1b[0m"
            };
            total_duration_us += r.duration_us;
            total_constraints += r.constraint_count;

            if show_stats {
                let time_str = if r.duration_us > 1000 {
                    format!("{:.1}ms", r.duration_us as f64 / 1000.0)
                } else {
                    format!("{}µs", r.duration_us)
                };
                let status_str = if r.status == VerifStatus::Proven {
                    "✓ pass"
                } else if r.status == VerifStatus::Disproven {
                    "✗ fail"
                } else {
                    "? unknown"
                };
                let identity = r
                    .artifact
                    .as_ref()
                    .map(|artifact| artifact.identity_label())
                    .unwrap_or_else(|| "—".to_string());
                println!(
                    "{:30} {:>10} {:>12} {:>8}  {}",
                    r.func_name, status_str, r.constraint_count, time_str, identity
                );
            }

            if let Some(diag) = &r.diagnostic {
                let formatted =
                    format_diagnostic_with_registry(diag, &merged_file.sources, src_ref, filename);
                // A call-site-owned diagnostic can otherwise hide the result
                // label entirely: the source span and message identify the
                // declaration, but not the caller/obligation row that owns
                // the verdict. Keep the named FFI Ensures observation visible
                // in the normal (non-stats) CLI transcript while preserving
                // the structured diagnostic and its exact source span.
                if !show_stats {
                    if let Some(artifact) = &r.artifact {
                        eprint!("  [{}] ", artifact.identity_label());
                    }
                    if r.func_name.contains(" -> extern ") {
                        eprint!("{}: ", r.func_name);
                    }
                }
                if use_color {
                    eprint!("{}", formatted);
                } else {
                    eprint!("{}", strip_ansi(&formatted));
                }
            } else if !show_stats {
                let time_str = if r.duration_us > 1000 {
                    format!(" ({:.1}ms)", r.duration_us as f64 / 1000.0)
                } else {
                    format!(" ({}µs)", r.duration_us)
                };
                let identity = r
                    .artifact
                    .as_ref()
                    .map(|artifact| format!(" [{}]", artifact.identity_label()))
                    .unwrap_or_default();
                println!(
                    "  {} {}: {} [{} constraints]{}{}",
                    icon, r.func_name, r.message, r.constraint_count, time_str, identity
                );
            }

            if verification_blocks_success(
                &r.status,
                r.constraint_count,
                &r.message,
                r.trusted_subset_domain,
            ) {
                all_passed = false;
            }
        }

        let verified = results
            .iter()
            .filter(|r| r.status == VerifStatus::Verified)
            .count();
        let total_time_ms = total_duration_us as f64 / 1000.0;
        println!(
            "\n{}/{} verified in {:.1}ms ({} total constraints)",
            verified,
            results.len(),
            total_time_ms,
            total_constraints
        );

        if show_stats && !results.is_empty() {
            let max_constraints = results
                .iter()
                .map(|r| r.constraint_count)
                .max()
                .unwrap_or(0);
            let min_constraints = results
                .iter()
                .map(|r| r.constraint_count)
                .min()
                .unwrap_or(0);
            let avg_time = total_duration_us as f64 / results.len() as f64;
            println!(
                "  (constraint range: {}-{}, avg time: {:.1}µs)",
                min_constraints, max_constraints, avg_time
            );
        }

        if !all_passed {
            return Err("verification failed or was inconclusive".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::verification_blocks_success;
    use mimi::verifier::{TrustedSubsetDomain, VerifStatus};

    #[test]
    fn genuine_unknown_blocks_cli_success() {
        assert!(verification_blocks_success(
            &VerifStatus::SolverUnknown,
            1,
            "could not encode ensures",
            None,
        ));
        assert!(verification_blocks_success(
            &VerifStatus::InfrastructureError,
            0,
            "Z3 solver not available",
            None,
        ));
    }

    #[test]
    fn no_contract_result_is_neutral() {
        assert!(!verification_blocks_success(
            &VerifStatus::NoObligations,
            0,
            "no contracts to verify",
            None,
        ));
    }

    #[test]
    fn contract_level_not_in_trusted_subset_blocks() {
        // v0.31.25: contract-level NotInTrustedSubset is a hard error.
        assert!(verification_blocks_success(
            &VerifStatus::NotInTrustedSubset,
            1,
            "could not encode extern requires for Z3",
            Some(TrustedSubsetDomain::Contract),
        ));
    }

    #[test]
    fn body_level_not_in_trusted_subset_does_not_block() {
        // v0.31.25: body-level NotInTrustedSubset doesn't block mimi verify.
        assert!(!verification_blocks_success(
            &VerifStatus::NotInTrustedSubset,
            1,
            "body contains unsupported constructs",
            Some(TrustedSubsetDomain::Body),
        ));
    }
}
