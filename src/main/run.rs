use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

#[cfg(test)]
thread_local! {
    static LEGACY_AST_COMPILER_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

use crate::{is_production, is_sketch, resolve_path};
use mimi::diagnostic::format::{
    colors_enabled, format_diagnostic, format_diagnostic_with_registry, strip_ansi,
};
use mimi::{lexer, loader};

/// Runtime traps cross the CLI boundary by ownership transfer. Ordinary
/// command errors retain their existing textual diagnostic path.
#[derive(Debug)]
pub(crate) enum RunError {
    Ordinary(String),
    Trap(mimi::interp::InterpError),
}

impl From<String> for RunError {
    fn from(message: String) -> Self {
        Self::Ordinary(message)
    }
}

impl From<&str> for RunError {
    fn from(message: &str) -> Self {
        Self::Ordinary(message.into())
    }
}

fn canonical_runtime_error(error: mimi::interp::InterpError) -> RunError {
    if error.is_trap() {
        RunError::Trap(error)
    } else {
        RunError::Ordinary(format!("canonical MIR runtime error: {error}"))
    }
}

fn compatibility_runtime_error(error: mimi::interp::InterpError) -> RunError {
    if error.is_trap() {
        RunError::Trap(error)
    } else {
        eprintln!("bytecode runtime error: {error}");
        RunError::Ordinary("runtime error".into())
    }
}

const RUN_LEGACY_REMAINDER_SHIFT_RETURN_PROFILE: &str = "run-legacy-remainder-shift-return-v1";

/// The scalar native/verifier intersection intentionally keeps shifts on the
/// compatibility route.  The bytecode VM already has a typed MIR lowering for
/// this exact pure shape, so `run` can retire its AST compiler for the narrow
/// return-only profile below without widening the native or verifier island.
/// The complete program is first lowered from CheckedProgram; this predicate
/// only admits two i32 `Const` instructions, one i32 `ShiftLeft`, and a return
/// of that result. Any other MIR operation stays on the named compatibility
/// route.
fn is_exact_legacy_shift_return_mir(program: &mimi::core::mir::reference::MirProgram) -> bool {
    use mimi::core::ir::ResolvedBinaryOp;
    use mimi::core::mir::types::MirAbiClass;
    use mimi::core::mir::{MirInstructionKind, MirTerminator};
    use std::collections::BTreeSet;

    if program.functions().len() != 1
        || !program.ffi_calls().is_empty()
        || !program.transitions().is_empty()
    {
        return false;
    }
    let main = mimi::core::resolved::NodeId("function:main".into());
    let Some(function) = program.functions().get(&main) else {
        return false;
    };
    let is_i32 = |ty: &mimi::core::ir::ResolvedTypeId| {
        program.type_catalog().get(ty).is_some_and(|desc| {
            desc.abi
                == (MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                })
        })
    };
    if !function.parameters.is_empty()
        || !function
            .parameter_permissions
            .as_ref()
            .is_some_and(|permissions| permissions.is_empty())
        || !is_i32(&function.result)
        || !function.contracts.is_empty()
        || function.proof_worklist.is_some()
        || function.blocks.len() != 1
        || function.values.is_empty()
        || function.values.values().any(|value| !is_i32(&value.ty))
    {
        return false;
    }
    let Some(block) = function.blocks.get(&function.entry) else {
        return false;
    };
    if !block.parameters.is_empty() || block.instructions.len() != 3 {
        return false;
    }

    let mut constants = BTreeSet::new();
    let mut shift_result = None;
    for instruction in &block.instructions {
        match &instruction.kind {
            MirInstructionKind::Const {
                result,
                literal: mimi::core::ir::ResolvedLiteral::Int(_),
            } if constants.insert(result.clone()) => {}
            MirInstructionKind::Binary {
                result,
                op: ResolvedBinaryOp::ShiftLeft,
                left,
                right,
            } if shift_result.is_none() => {
                shift_result = Some((result.clone(), left.clone(), right.clone()));
            }
            _ => return false,
        }
    }
    let Some((result, left, right)) = shift_result else {
        return false;
    };
    if constants.len() != 2 || !constants.contains(&left) || !constants.contains(&right) {
        return false;
    }
    matches!(
        &block.terminator,
        MirTerminator::Return { value: Some(returned) } if returned == &result
    )
}

fn item_meta(item: &mimi::ast::Item) -> mimi::ast::AstNodeMeta {
    match item {
        mimi::ast::Item::Func(item) => item.meta,
        mimi::ast::Item::Type(item) => item.meta,
        mimi::ast::Item::Actor(item) => item.meta,
        mimi::ast::Item::Cap(item) => item.meta,
        mimi::ast::Item::Trait(item) => item.meta,
        mimi::ast::Item::Impl(item) => item.meta,
        mimi::ast::Item::ExternBlock(item) => item.meta,
        mimi::ast::Item::Const { meta, .. } => *meta,
        mimi::ast::Item::Flow(item) => item.meta,
        mimi::ast::Item::Session(item) => item.meta,
    }
}

/// This surface preflight lets the caller fail closed if canonical lowering
/// fails for the one intended source shape. Other legacy programs are not
/// speculatively lowered and keep their existing compatibility path.
fn is_exact_legacy_shift_return_source(file: &mimi::ast::File) -> bool {
    use mimi::ast::{BinOp, Expr, Item, Lit, Stmt, Type};

    if !file.imports.is_empty() {
        return false;
    }
    let mut main = None;
    for item in &file.items {
        let meta = item_meta(item);
        let is_prelude = file
            .sources
            .key(meta.span.source_id)
            .is_some_and(|key| key.as_str() == "stdlib:prelude.mimi");
        if is_prelude {
            continue;
        }
        match item {
            Item::Func(function) if function.name == "main" && main.is_none() => {
                main = Some(function);
            }
            _ => return false,
        }
    }
    let Some(main) = main else {
        return false;
    };
    let [statement] = main.body.as_slice() else {
        return false;
    };
    if !main.params.is_empty()
        || !main.generics.is_empty()
        || !main.where_clause.is_empty()
        || !main.effects.is_empty()
        || main.is_comptime
        || main.is_async
        || main.extern_abi.is_some()
        || main.has_requires
        || main.has_ensures
        || !main.ret.as_ref().is_some_and(
            |ty| matches!(ty.unlocated(), Type::Name(name, args) if name == "i32" && args.is_empty()),
        )
    {
        return false;
    }
    let Stmt::Expr(expression) = statement.unlocated() else {
        return false;
    };
    matches!(
        expression.unlocated(),
        Expr::Binary(BinOp::Shl, left, right)
            if matches!(left.unlocated(), Expr::Literal(Lit::Int(_)))
                && matches!(right.unlocated(), Expr::Literal(Lit::Int(_)))
    )
}

/// Attempt the return-only shift migration after default dispatch has chosen
/// a compatibility disposition.  Canonical lowering, the finite MIR shape
/// check, the bound route receipt, and AST-free bytecode compilation all have
/// to succeed before the caller changes routes.  A nonmatching source keeps
/// its existing explicitly reported compatibility behavior; an error after
/// exact admission is returned and never retried through the AST compiler.
fn compile_legacy_shift_return_route(
    checked: &mimi::core::resolved::CheckedProgram,
    merged_file: &mimi::ast::File,
) -> Result<
    Option<(
        mimi::core::mir::reference::MirProgram,
        mimi::core::mir::CanonicalMirRouteReceipt,
        std::sync::Arc<mimi::interp::bytecode::BytecodeProgram>,
    )>,
    String,
> {
    if !is_exact_legacy_shift_return_source(merged_file) {
        return Ok(None);
    }
    let canonical = crate::canonical_dispatch::build_canonical_program(checked, merged_file)
        .map_err(|error| {
            format!("exact shift-return candidate failed canonical MIR lowering: {error}")
        })?;
    if !is_exact_legacy_shift_return_mir(&canonical) {
        return Err(
            "exact shift-return source candidate did not lower to its required canonical MIR shape"
                .into(),
        );
    }
    let receipt = canonical.route_receipt(RUN_LEGACY_REMAINDER_SHIFT_RETURN_PROFILE);
    let bytecode =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&canonical, &receipt)
            .map_err(|errors| {
                format!(
            "checked MIR bytecode route `{RUN_LEGACY_REMAINDER_SHIFT_RETURN_PROFILE}` failed: {}",
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        )
            })?;
    if bytecode.ast.is_some() {
        return Err(format!(
            "checked MIR bytecode route `{RUN_LEGACY_REMAINDER_SHIFT_RETURN_PROFILE}` unexpectedly retained an AST"
        ));
    }
    Ok(Some((canonical, receipt, bytecode)))
}

fn is_map_set_result_route_profile(profile: &str) -> bool {
    profile == mimi::core::mir::ORDINARY_MAP_SET_RESULT_EXECUTION_PROFILE
}

fn handle_watch_result(
    result: Result<i32, RunError>,
    report: impl FnOnce(&str),
) -> Result<(), RunError> {
    match result {
        Ok(_) => Ok(()),
        Err(error @ RunError::Trap(_)) => Err(error),
        Err(RunError::Ordinary(message)) => {
            report(&message);
            Ok(())
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run(
    path: Option<&Path>,
    verify_contracts: bool,
    verify_ffi: bool,
    allocator: &str,
    mir: bool,
    watch: bool,
    profile: bool,
    extra_args: &[String],
) -> Result<i32, RunError> {
    let path = resolve_path(path)?;
    if profile {
        mimi::runtime::profiler::profiler_init();
    }
    let result = if watch {
        run_watch(
            &path,
            verify_contracts,
            verify_ffi,
            allocator,
            mir,
            extra_args,
        )?;
        0
    } else {
        run_once(
            &path,
            verify_contracts,
            verify_ffi,
            allocator,
            mir,
            extra_args,
        )?
    };
    if profile {
        mimi::runtime::profiler::profiler_report();
    }
    Ok(result)
}

fn run_once(
    path: &Path,
    verify_contracts: bool,
    verify_ffi: bool,
    allocator: &str,
    mir: bool,
    extra_args: &[String],
) -> Result<i32, RunError> {
    // §13-#67 (audit 2026-08-05, closed 2026-08-07): --allocator was a dead
    // flag on every backend. Fail loud instead of silently accepting a
    // selection no backend implements; the default "system" passes.
    if allocator != "system" {
        return Err(format!(
            "--allocator '{allocator}' is not implemented (arena/bump allocation is not wired into any backend); use 'system' or omit the flag"
        ).into());
    }
    // CL-H1: size-capped source load (shared with other CLI entry points).
    let source = mimi::path_safety::read_source_capped(path)?;
    if is_sketch(path) {
        return Err(
            "MimiSpec .mms sketches are not executable; port the program to a .mimi source file"
                .into(),
        );
    }
    if !is_production(path) {
        return Err(format!("expected .mimi production file, got {}", path.display()).into());
    }
    let tokens = lexer::Lexer::new(&source)
        .tokenize()
        .map_err(String::from)?;
    let (file, parse_errors) = loader::parser_for_path(tokens, path)?.parse_file_with_recovery();
    if !parse_errors.is_empty() {
        let use_color = colors_enabled();
        let src_ref = Some(source.as_str());
        let filename = &path.display().to_string();
        for e in &parse_errors {
            let formatted = format_diagnostic(&e.to_diagnostic(), src_ref, filename);
            if use_color {
                eprint!("{}", formatted);
            } else {
                eprint!("{}", strip_ansi(&formatted));
            }
        }
        return Err(format!("{} parse error(s) found", parse_errors.len()).into());
    }

    // Load imports if any
    let mut merged_file = if !file.imports.is_empty() {
        let base_dir = path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        let mut loader = loader::ModuleLoader::new(base_dir);
        loader.load_main_with_file(path, file)?;
        loader.merge_all()?
    } else {
        file
    };

    // Auto-merge standard library prelude (identity, clamp, is_even, etc.)
    loader::merge_prelude_into(&mut merged_file);

    let checked_program = mimi::core::check_program(&merged_file);
    let checked_program = match checked_program {
        Ok(program) => program,
        Err(diagnostics) => {
            eprintln!(
                "{} has {} type error(s):",
                path.display(),
                diagnostics.len()
            );
            let use_color = colors_enabled();
            let src = mimi::path_safety::read_source_capped(path).ok();
            let src_ref = src.as_deref();
            for d in &diagnostics {
                let formatted = format_diagnostic_with_registry(
                    d,
                    &merged_file.sources,
                    src_ref,
                    &path.display().to_string(),
                );
                if use_color {
                    eprint!("{}", formatted);
                } else {
                    eprint!("{}", strip_ansi(&formatted));
                }
            }
            return Err("type checking failed".into());
        }
    };

    // Canonical MIR bytecode path. Explicit --mir always requests it and fails
    // closed on an unsupported graph. The default path enters the same route
    // after whole-program preflight, or for the one exact run-only shift
    // return receipt below. It never falls back after canonical execution
    // starts.
    let mut legacy_bytecode_override = None;
    let canonical = if mir {
        if let Some(reason) =
            crate::canonical_dispatch::scalar_ffi_declaration_boundary_diagnostic(&checked_program)
        {
            return Err(format!("canonical MIR run rejected: {reason}").into());
        }
        Some(crate::canonical_dispatch::build_canonical_program(
            &checked_program,
            &merged_file,
        )?)
    } else {
        match crate::canonical_dispatch::select_default_route(&checked_program, &merged_file) {
            crate::canonical_dispatch::DefaultMirRoute::Canonical(canonical) => Some(*canonical),
            crate::canonical_dispatch::DefaultMirRoute::Legacy(reason) => {
                let can_try_legacy_shift_return = matches!(
                    reason,
                    crate::canonical_dispatch::LegacyRouteReason::OutsideMigratedProfile
                        | crate::canonical_dispatch::LegacyRouteReason::PlainScalarIslandPreflightCompatibility
                );
                if can_try_legacy_shift_return {
                    match compile_legacy_shift_return_route(&checked_program, &merged_file) {
                        Ok(Some((canonical, receipt, bytecode))) => {
                            if std::env::var_os("MIMI_VERBOSE").is_some()
                                || std::env::var_os("MIMI_ROUTE_CENSUS").is_some()
                            {
                                eprintln!(
                                    "canonical route disposition: canonical ({})",
                                    receipt.profile
                                );
                            }
                            legacy_bytecode_override = Some((receipt, bytecode));
                            Some(canonical)
                        }
                        Ok(None) => {
                            crate::canonical_dispatch::report_legacy_route(reason);
                            None
                        }
                        Err(message) => return Err(message.into()),
                    }
                } else {
                    crate::canonical_dispatch::report_legacy_route(reason);
                    None
                }
            }
            crate::canonical_dispatch::DefaultMirRoute::Rejected(reason) => {
                return Err(format!("default Canonical MIR route rejected: {reason}").into());
            }
            crate::canonical_dispatch::DefaultMirRoute::VerifierOnlyFfiStringRecordOwnerSlice => {
                return Err(
                    "verifier-only FFI owner slice cannot be used by the run bytecode route".into(),
                );
            }
            crate::canonical_dispatch::DefaultMirRoute::VerifierOnlyDirectTupleGetUnpackL2 => {
                return Err(
                    "VerifierOnly direct tuple Get L2 semantic projection cannot be used by the run bytecode route".into(),
                );
            }
        }
    };
    if let Some(canonical) = canonical {
        let receipt_profile = legacy_bytecode_override
            .as_ref()
            .map(|(receipt, _)| receipt.profile.clone())
            .unwrap_or_else(|| {
                crate::canonical_dispatch::route_receipt_profile_for_checked(
                    &checked_program,
                    "run-canonical-v1",
                )
                .to_owned()
            });
        let receipt = legacy_bytecode_override
            .as_ref()
            .map(|(receipt, _)| receipt.clone())
            .unwrap_or_else(|| canonical.route_receipt(receipt_profile.clone()));
        if std::env::var_os("MIMI_ROUTE_CENSUS").is_some() {
            let admission_mode = if mir { "explicit-mir" } else { "default" };
            eprintln!(
                "MIMI_ROUTE_CENSUS route-admission consumer=run mode={} checker_profile={} mir_digest={} receipt_identity_digest={}",
                admission_mode,
                receipt_profile,
                canonical.canonical_digest(),
                receipt.route_identity_digest(),
            );
        }
        if mir && std::env::var_os("MIMI_ROUTE_CENSUS").is_some() {
            eprintln!(
                "canonical route disposition: explicit canonical ({}) mir_digest={}",
                receipt.profile, receipt.mir_digest
            );
        }
        let has_owned_get_result_branch_v3 = checked_program
            .resource_analyses()
            .values()
            .any(|analysis| analysis.has_ordinary_map_owned_get_result_branch_v3_receipt());
        let bytecode_result = if let Some((bound_receipt, bytecode)) =
            legacy_bytecode_override.take()
        {
            if bound_receipt != receipt || !is_exact_legacy_shift_return_mir(&canonical) {
                return Err(format!(
                    "checked MIR bytecode route `{RUN_LEGACY_REMAINDER_SHIFT_RETURN_PROFILE}` lost its exact route binding"
                )
                .into());
            }
            Ok(bytecode)
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_RESULT_BRANCH_PROFILE
        {
            // The nested Result Values route has a separate CheckedProgram /
            // MIR bytecode adapter; its duplicate owned-Get receipt cannot
            // authorize the legacy ResultBranchV3 adapter.
            mimi::interp::bytecode::compile_mir_program_with_values_local_index_i32_result_branch_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND
            && has_owned_get_result_branch_v3
        {
            // New/Set ResultBranchV3 ops require the exact Checker/MIR
            // binding to mint the bytecode VM anchor; a profile receipt alone
            // cannot authorize these instructions.
            mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_get_result_branch_v3_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_RESULT_PROFILE
        {
            mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_direct_print_result_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_DIRECT_TUPLE_GET_UNPACK_L1_CORE_PROFILE
        {
            mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_direct_tuple_get_unpack_l1_route(
                &checked_program,
                &canonical,
                &receipt,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_I32_EXECUTION_PROFILE
        {
            mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_set_get_any_i32_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_PROFILE
        {
            mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_get_or_default_string_route(
                &checked_program,
                &canonical,
                &receipt,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_I32_PROFILE
        {
            // The cross-call Map borrow, selected payload tag, miss default,
            // and pair settlement share one Checker/MIR route receipt.
            mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_get_or_default_i32_route(
                &checked_program,
                &canonical,
                &receipt,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE
        {
            // The zero-miss route binds both the Boolean hit result and a
            // fresh owned i32 zero for the miss branch to the Checker/MIR
            // pair and its dedicated route receipt.
            mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_zero_miss_i32_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE
        {
            // The checked miss marker is authorized only by the exact
            // Checker/MIR Remove-successor receipt and its private VM anchor.
            mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_removed_key_checked_miss_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE
        {
            // KeysSnapshot bytecode must validate the same Checker/MIR source
            // pair and install its private exact-profile VM anchor.
            mimi::interp::bytecode::compile_mir_program_with_keys_snapshot_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesProducerCaller
                .as_str()
        {
            // The direct Values producer/caller bridge has a private
            // bytecode anchor bound to this CheckedProgram/MIR pair.
            mimi::interp::bytecode::compile_mir_program_with_values_producer_caller_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::CanonicalMirRouteProfile::
                OrdinaryMapOwnedAnyV2ValuesResultCrosscallStringCallerIndex1
                .as_str()
        {
            // V4 Result-branch index-one String unpack has its own CheckedProgram-bound
            // bytecode anchor distinct from V3 and straight-line V1/V2.
            mimi::interp::bytecode::compile_mir_program_with_values_result_crosscall_string_caller_index1_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::CanonicalMirRouteProfile::
                OrdinaryMapOwnedAnyV2ValuesResultCrosscallStringCaller
                .as_str()
        {
            // V3 Result-branch index-zero String unpack has its own CheckedProgram-bound
            // bytecode anchor distinct from V4 and straight-line V1/V2.
            mimi::interp::bytecode::compile_mir_program_with_values_result_crosscall_string_caller_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::CanonicalMirRouteProfile::
                OrdinaryMapOwnedAnyV2ValuesResultProducerCaller
                .as_str()
        {
            // Result-branch one-length Values producer/caller has its own CheckedProgram-bound
            // bytecode anchor distinct from V3/V4 unpack and straight-line V1/V2.
            mimi::interp::bytecode::compile_mir_program_with_values_result_producer_caller_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::CanonicalMirRouteProfile::
                OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1
                .as_str()
        {
            // Literal index one uses the distinct V2 bytecode adapter so its
            // sorted two-entry snapshot and owned String proof stay exact.
            mimi::interp::bytecode::compile_mir_program_with_values_crosscall_string_caller_index1_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::CanonicalMirRouteProfile::
                OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller
                .as_str()
        {
            // The cross-call Values String-unpack profile has its own private
            // bytecode anchor for the producer, copied String, and snapshot
            // obligations. Generic route-receipt compilation cannot authorize
            // those indexed operations.
            mimi::interp::bytecode::compile_mir_program_with_values_crosscall_string_caller_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootProducerCaller
                .as_str()
        {
            // Root ownership crosses one exact producer/caller edge. Keep
            // the VM anchor bound to that Checker/MIR receipt pair.
            mimi::interp::bytecode::compile_mir_program_with_map_root_producer_caller_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootGetAnyCaller
                .as_str()
        {
            // The checked GetAny/String-unpack caller consumes a root returned
            // across a producer boundary and carries its own VM anchor.
            mimi::interp::bytecode::compile_mir_program_with_map_root_get_any_caller_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE
        {
            // CloneIsolation binds bytecode authorization to the exact
            // Checker/MIR pair and its independent owner-v2 receipt.
            mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_clone_isolation_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE
        {
            // PersistentRootAliasReplacement has a separate Checker/MIR
            // validator and a dedicated bytecode anchor for aliased roots.
            mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE {
            // IsEmpty has its own Checker-sealed profile and bytecode
            // anchor. The adapter revalidates this exact CheckedProgram /
            // MIR pair and rejects a missing or substituted receipt.
            mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_is_empty_route(
                    &checked_program,
                    &canonical,
                )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE
        {
            // RecordUnit binds the exact mixed payload, Label glue, roots,
            // cleanup, and borrowed output before minting its VM anchor.
            mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_record_unit_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_RESULT_BRANCH_PROFILE
        {
            // Result branches have their own Checker/MIR receipt and private
            // AST-free Bytecode anchor; the legacy MapSetDropControl adapter
            // cannot authorize this graph.
            mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE
        {
            // The no-query control has an independent Checker-sealed bytecode
            // anchor and cannot borrow IsEmpty or generic owner-v2 authority.
            mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_map_set_drop_control_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE
        {
            mimi::interp::bytecode::compile_mir_program_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile == mimi::core::mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE {
            mimi::interp::bytecode::compile_mir_program_with_empty_root_root_queries_v1_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE
        {
            mimi::interp::bytecode::compile_mir_program_with_computed_key_get_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE
        {
            mimi::interp::bytecode::compile_mir_program_with_computed_string_key_owner_chain_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE
        {
            // argv[0], the NUL/UTF-8 key, snapshot copy, and both GetAny
            // outcomes are authorized only by their exact CheckedProgram/MIR
            // receipt and the dedicated AST-free VM anchor.
            mimi::interp::bytecode::compile_mir_program_with_dynamic_keys_get_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE
        {
            // This adapter validates the same checker/MIR pair and seals its
            // exact snapshot-replacement receipt into the bytecode artifact.
            mimi::interp::bytecode::compile_mir_program_with_values_snapshot_replacement_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE
        {
            // This adapter validates the same checker/MIR pair and seals its
            // exact mixed-snapshot receipt into the bytecode artifact.
            mimi::interp::bytecode::compile_mir_program_with_values_mixed_snapshot_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE
        {
            mimi::interp::bytecode::compile_mir_program_with_values_mixed_snapshot_remove_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE
        {
            mimi::interp::bytecode::compile_mir_program_with_values_field_move_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_PROFILE
        {
            mimi::interp::bytecode::compile_mir_program_with_values_local_index_i32_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_NEW_RESULT_EXECUTION_PROFILE
        {
            // The recoverable NewResult slice binds its isolated L1 graph and
            // its private VM anchor to the exact Checker receipt.
            mimi::interp::bytecode::compile_mir_program_with_map_new_result_route(
                &checked_program,
                &canonical,
            )
        } else if is_map_set_result_route_profile(&receipt_profile)
        {
            // SetResult has an independent lineage receipt and VM anchor for
            // the two persistent-root Result dispatches.
            mimi::interp::bytecode::compile_mir_program_with_map_set_result_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_SET_GET_STRING_EXECUTION_PROFILE
        {
            // The String payload and Map ownership lineage are authorized by
            // the exact Checker receipt, with a String-specific VM anchor.
            mimi::interp::bytecode::compile_mir_program_with_map_set_get_string_route(
                &checked_program,
                &canonical,
            )
        } else if receipt_profile
            == mimi::core::mir::ORDINARY_MAP_SET_GET_I32_EXECUTION_PROFILE
        {
            mimi::interp::bytecode::compile_mir_program_with_map_set_get_i32_route(
                &checked_program,
                &canonical,
            )
        } else {
            mimi::interp::bytecode::compile_mir_program_with_route_receipt(&canonical, &receipt)
        };
        let prog = bytecode_result.map_err(|errors| {
            let details = errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            format!("canonical MIR bytecode is not eligible:\n{details}")
        })?;
        let mut vm =
            mimi::interp::bytecode::BytecodeVM::new(prog).with_cli_args(extra_args.to_vec());
        vm.verify_contracts = verify_contracts;
        vm.set_verify_ffi(verify_ffi);
        return vm
            .run()
            .map(|exit_code| exit_code as i32)
            .map_err(canonical_runtime_error);
    }

    // Legacy-compatible Bytecode VM path (sole default interpreter since
    // v0.33).  It remains the default until the canonical MIR eligibility
    // surface covers aggregate values, builtins, Flow, and ownership glue.
    {
        use mimi::interp::bytecode::{BytecodeCompiler, BytecodeVM};
        let mut compiler = BytecodeCompiler::new();
        // G1: install type information from CheckedProgram for type-directed
        // method resolution and parameter type tracking.
        compiler.install_checked_program(&checked_program);
        #[cfg(test)]
        LEGACY_AST_COMPILER_CALLS.with(|calls| calls.set(calls.get() + 1));
        let prog = compiler
            .compile_file(&merged_file)
            .map_err(|e| format!("bytecode compile error: {}", e))?;
        let mut vm = BytecodeVM::new(prog.clone()).with_cli_args(extra_args.to_vec());
        // §13-#67: --verify-contracts was silently ignored. Wire it: the CLI
        // flag is opt-in (default false), matching the documented semantics
        // ("Enable runtime contract verification"). The VM's internal default
        // (true) is overridden so the flag actually controls behavior.
        vm.verify_contracts = verify_contracts;
        // Legacy-compatible programs may still contain FFI declarations that
        // are outside the canonical scalar-C profile. Keep the warning only
        // on this old consumer path; admitted scalar FFI programs return
        // above after the AST-free MIR VM has installed the receipt check.
        if verify_ffi
            && merged_file
                .items
                .iter()
                .any(|item| matches!(item, mimi::ast::Item::ExternBlock(_)))
        {
            eprintln!(
                "warning: --verify-ffi is not yet supported by the bytecode VM; \
                 FFI contract verification is disabled (pass --skip-verify-ffi to silence)"
            );
        }
        match vm.run() {
            Ok(exit_code) => {
                // 0.39.136: no stdout echo of a nonzero result. Native
                // binaries print nothing here, and stdout is part of the
                // L1-observable contract (`run_source == compile_and_run`);
                // the exit code itself still propagates via process status.
                Ok(exit_code as i32)
            }
            Err(error) => Err(compatibility_runtime_error(error)),
        }
    }
}

fn debounce_mtime(path: &Path, last: SystemTime) -> Option<SystemTime> {
    // Wait 150ms then re-check: debounces rapid save events
    std::thread::sleep(Duration::from_millis(150));
    get_mtime(path).ok().filter(|&m| m != last)
}

fn run_watch(
    path: &Path,
    verify_contracts: bool,
    verify_ffi: bool,
    allocator: &str,
    mir: bool,
    extra_args: &[String],
) -> Result<(), RunError> {
    println!("Watching {} for changes...", path.display());
    // CL-H16 (deep audit): the watch loop had no termination condition and
    // could only be killed with SIGKILL. Install a SIGINT (Ctrl-C) handler
    // that flips a flag so the loop exits cleanly and the process can be
    // stopped normally.
    WATCH_RUNNING.store(true, Ordering::SeqCst);
    unsafe {
        // SAFETY: libc::signal 标准调用；watch_sigint_handler 为 extern "C" fn，SIGINT 仅置位原子标志。
        libc::signal(
            libc::SIGINT,
            watch_sigint_handler as *const () as libc::sighandler_t,
        );
    }
    let mut last_modified = get_mtime(path)?;
    // Run once first
    handle_watch_result(
        run_once(
            path,
            verify_contracts,
            verify_ffi,
            allocator,
            mir,
            extra_args,
        ),
        |message| eprintln!("{message}"),
    )?;
    while WATCH_RUNNING.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(500));
        match get_mtime(path) {
            Ok(mtime) if mtime != last_modified => {
                // Debounce: wait briefly and re-check for stable mtime
                let stable = debounce_mtime(path, last_modified).unwrap_or(mtime);
                last_modified = stable;
                println!("\n--- file changed, re-running ---");
                print!("\x1B[2J\x1B[H");
                handle_watch_result(
                    run_once(
                        path,
                        verify_contracts,
                        verify_ffi,
                        allocator,
                        mir,
                        extra_args,
                    ),
                    |message| eprintln!("{message}"),
                )?;
            }
            Err(e) => {
                eprintln!("watch error: {}", e);
            }
            _ => {}
        }
    }
    Ok(())
}

fn get_mtime(path: &Path) -> Result<SystemTime, String> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map_err(|e| format!("failed to get file modification time: {}", e))
}

/// CL-H16: shared flag toggled by the SIGINT handler so `run_watch`'s loop
/// can terminate on Ctrl-C instead of running forever.
static WATCH_RUNNING: AtomicBool = AtomicBool::new(true);

extern "C" fn watch_sigint_handler(_sig: libc::c_int) {
    WATCH_RUNNING.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod typed_trap_tests {
    use super::*;
    use mimi::interp::{error::ErrorContext, InterpError};

    fn allocation_failure() -> InterpError {
        InterpError::AllocationFailure(Box::new(ErrorContext {
            msg: "String allocation failed".into(),
            function: None,
            operation: None,
            help: None,
            call_stack: Vec::new(),
            line: None,
        }))
    }

    #[test]
    fn terminal_runtime_traps_move_through_both_cli_handoffs() {
        for handoff in [canonical_runtime_error, compatibility_runtime_error] {
            for error in [
                InterpError::new("generic"),
                InterpError::div_by_zero(),
                InterpError::integer_overflow("overflow"),
                allocation_failure(),
            ] {
                let code = error.code();
                let original_context = error.ctx() as *const ErrorContext;
                let RunError::Trap(error) = handoff(error) else {
                    panic!("terminal trap must remain typed");
                };
                assert_eq!(error.code(), code);
                assert_eq!(
                    error.ctx() as *const ErrorContext,
                    original_context,
                    "handoff must move the owned context without cloning or stringification"
                );
            }
        }
    }

    #[test]
    fn ordinary_runtime_and_file_errors_keep_textual_handoffs() {
        let error = canonical_runtime_error(InterpError::index_out_of_bounds("oob"));
        assert!(
            matches!(error, RunError::Ordinary(message) if message == "canonical MIR runtime error: [E0803] oob")
        );
        let error = compatibility_runtime_error(InterpError::index_out_of_bounds("oob"));
        assert!(matches!(error, RunError::Ordinary(message) if message == "runtime error"));
        assert!(
            matches!(RunError::from("file missing"), RunError::Ordinary(message) if message == "file missing")
        );
    }

    #[test]
    fn watch_stops_on_typed_traps_and_continues_after_ordinary_errors() {
        let mut reports = Vec::new();
        for error in [
            InterpError::new("generic"),
            InterpError::div_by_zero(),
            InterpError::integer_overflow("overflow"),
            allocation_failure(),
        ] {
            let outcome = handle_watch_result(Err(RunError::Trap(error)), |message| {
                reports.push(message.to_owned())
            });
            assert!(matches!(outcome, Err(RunError::Trap(_))));
        }
        assert!(
            reports.is_empty(),
            "watch must not stringify or report terminal traps before returning them"
        );
        handle_watch_result(Err(RunError::Ordinary("file missing".into())), |message| {
            reports.push(message.to_owned())
        })
        .expect("ordinary file error keeps watch running");
        handle_watch_result(Ok(0), |_| {
            panic!("successful execution needs no error report")
        })
        .expect("successful execution keeps watch running");
        assert_eq!(reports, ["file missing"]);
    }
}

#[cfg(test)]
mod legacy_shift_return_route_tests {
    use super::*;
    use mimi::core::mir::reference::{MirReferenceInterpreter, MirRuntimeValue};
    use mimi::core::resolved::NodeId;
    use mimi::interp::bytecode::BytecodeVM;

    fn checked_file(source: &str) -> (mimi::core::resolved::CheckedProgram, mimi::ast::File) {
        let tokens = mimi::lexer::Lexer::new(source).tokenize().expect("lex");
        let mut file = mimi::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        mimi::loader::merge_prelude_into(&mut file);
        let checked = mimi::core::check_program(&file)
            .unwrap_or_else(|diagnostics| panic!("check failed: {diagnostics:?}"));
        (checked, file)
    }

    fn temp_source_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "mimi-legacy-shift-return-{}-{}.mimi",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ))
    }

    #[test]
    fn exact_default_shift_return_runs_ast_free_with_checker_mir_receipt() {
        let source = "func main() -> i32 { 3 << 2 }";
        let (checked, file) = checked_file(source);
        assert!(is_exact_legacy_shift_return_source(&file));
        assert!(matches!(
            crate::canonical_dispatch::select_default_route(&checked, &file),
            crate::canonical_dispatch::DefaultMirRoute::Legacy(
                crate::canonical_dispatch::LegacyRouteReason::OutsideMigratedProfile
                    | crate::canonical_dispatch::LegacyRouteReason::PlainScalarIslandPreflightCompatibility
            )
        ));

        let (canonical, receipt, bytecode) = compile_legacy_shift_return_route(&checked, &file)
            .expect("exact route compilation")
            .expect("the exact shift-return MIR shape is admitted");
        assert_eq!(receipt.profile, RUN_LEGACY_REMAINDER_SHIFT_RETURN_PROFILE);
        assert_eq!(receipt.mir_digest, canonical.canonical_digest());
        assert_eq!(
            receipt,
            canonical.route_receipt(RUN_LEGACY_REMAINDER_SHIFT_RETURN_PROFILE)
        );
        assert_eq!(receipt.route_identity_digest().len(), 64);
        assert!(
            bytecode.ast.is_none(),
            "MIR bytecode must not retain the source AST"
        );

        let reference = MirReferenceInterpreter::new(&canonical)
            .execute_with_output(&NodeId("function:main".into()), &[])
            .expect("MIR reference execution");
        assert_eq!(reference.value, MirRuntimeValue::Int(12));
        assert_eq!(reference.output, "");
        assert_eq!(
            BytecodeVM::new(bytecode).run().expect("MIR bytecode run"),
            12
        );

        let path = temp_source_path();
        fs::write(&path, source).expect("write production source");
        let before = LEGACY_AST_COMPILER_CALLS.with(std::cell::Cell::get);
        assert_eq!(before, 0, "legacy AST compiler calls must start empty");
        let run_result = run_once(&path, false, false, "system", false, &[])
            .expect("default run should use checked MIR bytecode");
        let after = LEGACY_AST_COMPILER_CALLS.with(std::cell::Cell::get);
        let _ = fs::remove_file(path);
        assert_eq!(run_result, 12);
        assert_eq!(
            after, before,
            "the legacy AST compiler call count must stay empty"
        );
    }

    #[test]
    fn shift_return_profile_is_narrow_and_set_result_profile_is_exact() {
        let (nonmatching_checked, nonmatching_file) =
            checked_file("func helper() -> i32 {\n    1\n}\nfunc main() -> i32 {\n    3 << 2\n}");
        assert!(!is_exact_legacy_shift_return_source(&nonmatching_file));
        assert!(
            compile_legacy_shift_return_route(&nonmatching_checked, &nonmatching_file)
                .expect("nonmatching source preflight")
                .is_none()
        );

        let (existing_legacy_checked, existing_legacy_file) = checked_file(
            "func main() -> i32 {\n    let base = 3\n    let shifted = base << 2\n    println(shifted)\n    0\n}",
        );
        assert!(!is_exact_legacy_shift_return_source(&existing_legacy_file));
        assert!(
            compile_legacy_shift_return_route(&existing_legacy_checked, &existing_legacy_file)
                .expect("existing shift compatibility preflight")
                .is_none()
        );

        assert!(is_map_set_result_route_profile(
            mimi::core::mir::ORDINARY_MAP_SET_RESULT_EXECUTION_PROFILE
        ));
        assert!(!is_map_set_result_route_profile(
            mimi::core::mir::ORDINARY_MAP_NEW_RESULT_EXECUTION_PROFILE
        ));
        assert!(!is_map_set_result_route_profile(
            mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_RESULT_PROFILE
        ));
        assert!(!is_map_set_result_route_profile(
            "ordinary-map-set-result-v1+other"
        ));
    }
}
