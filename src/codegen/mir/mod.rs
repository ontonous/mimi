//! AST-free native consumer for the closed scalar/owned-String,
//! recursive-tuple, non-Copy-record, flat-record/flat-variant, scalar-List
//! (including bounded nested List construction/clone/drop/outer-len/index/reverse/concat),
//! and the independently receipted owned `List<ManagedAny>` Values snapshots, and
//! local immutable-borrow Canonical MIR slices, plus the separately receipted
//! MapRoot, MapAnyOwner v1, and persistent ordinary-Map owner profiles.
//!
//! This module intentionally accepts only `MirProgram`.  It does not import
//! surface AST or `CheckedProgram`, and it never calls the legacy emitter.  A
//! small eligibility validator runs before LLVM declarations are created so a
//! shape is either covered by the scalar MIR contract or rejected explicitly.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use inkwell::basic_block::BasicBlock;
use inkwell::context::Context;
use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{
    AsValueRef, BasicMetadataValueEnum, BasicValue, BasicValueEnum, FunctionValue, PhiValue,
};
use inkwell::{FloatPredicate, IntPredicate};

use crate::codegen::{call_try_basic_value, CodeGenerator};
use crate::core::ir::{
    NativeExportScalarKind, ResolvedBinaryOp, ResolvedCallee, ResolvedLiteral, ResolvedUnaryOp,
};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{
    MirAbiClass, MirBuiltinContract, MirBuiltinKind, MirConversionKind, MirGlueContract,
    MirGlueKind, MirLayout, MirOwnership, MirTypeCatalog, MirTypeDesc, MirTypeKind,
};
use crate::core::mir::{
    MirAggregateKind, MirBlock, MirBlockId, MirFfiConversionKind, MirFunction, MirInstructionKind,
    MirListOperation, MirProjection, MirSetOperation, MirSwitchArm, MirSwitchCase, MirTerminator,
    MirValueId, MirVariantPredicate,
};
use crate::diagnostic::Diagnostic;
use crate::span::Span;

mod abi;
mod eligibility;
mod validate;

use abi::{
    native_basic_type, native_copy_variant_payload_type, native_list_kind,
    native_non_copy_variant_payload_type, native_non_copy_variant_payload_type_strict,
    native_ordinary_map_computed_string_key_owner_chain_set_any_result_llvm_type,
    native_ordinary_map_computed_string_key_owner_chain_set_any_result_variant_abi,
    native_ordinary_map_direct_tuple_get_l1_result_variant_abi,
    native_ordinary_map_new_result_llvm_type, native_ordinary_map_new_result_variant_abi,
    native_ordinary_map_set_drop_control_result_llvm_type,
    native_ordinary_map_set_drop_control_result_variant_abi,
    native_ordinary_map_set_get_any_i32_result_llvm_type,
    native_ordinary_map_set_get_any_i32_result_variant_abi,
    native_ordinary_map_set_get_any_result_i32_llvm_type,
    native_ordinary_map_set_get_any_result_i32_root_llvm_type,
    native_ordinary_map_set_get_any_result_i32_root_variant_abi,
    native_ordinary_map_set_get_any_result_i32_variant_abi,
    native_ordinary_map_set_get_string_result_llvm_type,
    native_ordinary_map_set_get_string_result_variant_abi,
    native_ordinary_map_set_result_llvm_type, native_ordinary_map_set_result_variant_abi,
    native_variant_abi, native_variant_abi_with_generic_result,
    validate_native_non_copy_record_type, validate_native_product_type,
    validate_native_recursive_tuple_type, NativeVariantAbi, NativeVariantPayloadSlot,
};
pub(crate) use eligibility::checked_managed_any_unpack_coverage;
use eligibility::{instruction_kind, mir_symbol, native_symbol_fragment, NativeMirError};
pub use eligibility::{
    validate_checked_mir_native_get_or_default_i32_route,
    validate_checked_mir_native_get_or_default_string_route,
    validate_checked_mir_native_map_root_get_any_caller_route,
    validate_checked_mir_native_map_root_producer_caller_route,
    validate_checked_mir_native_ordinary_map_new_result_route,
    validate_checked_mir_native_ordinary_map_owned_any_v2_direct_print_result_route,
    validate_checked_mir_native_ordinary_map_owned_any_v2_direct_tuple_get_unpack_l1_route,
    validate_checked_mir_native_ordinary_map_owned_any_v2_set_get_any_i32_route,
    validate_checked_mir_native_ordinary_map_owned_any_v2_set_get_any_result_i32_route,
    validate_checked_mir_native_ordinary_map_set_get_i32_route,
    validate_checked_mir_native_ordinary_map_set_get_string_route,
    validate_checked_mir_native_ordinary_map_set_result_route,
    validate_checked_mir_native_values_crosscall_string_caller_index1_route,
    validate_checked_mir_native_values_crosscall_string_caller_route,
    validate_checked_mir_native_values_producer_caller_route,
    validate_checked_mir_native_values_result_crosscall_string_caller_index1_route,
    validate_checked_mir_native_values_result_crosscall_string_caller_route, validate_mir_native,
    validate_mir_native_checked_get_route,
    validate_mir_native_computed_string_key_owner_chain_route,
    validate_mir_native_dynamic_keys_get_route,
    validate_mir_native_map_set_drop_control_result_route,
};
use ordinary_map_owned_any_v2::native_direct_print_result_root_result_abi;
use validate::NativeMirValidator;

/// Resolve one canonical MIR function owner to the exact LLVM symbol used by
/// the native MIR adapter.  Resolved callers that consume a materialized
/// generic instance must use this helper rather than reconstructing a symbol
/// from surface generic names or an AST mangle.
pub(crate) fn native_symbol_for_owner(owner: &crate::core::NodeId) -> Result<String, String> {
    mir_symbol(owner)
}
mod actor;
mod actor_status;
mod aggregate;
mod calls;
mod control_flow;
mod ffi_contract;
mod map_root;
mod ordinary_map;
mod ordinary_map_any_get;
mod ordinary_map_owned_any_v2;
mod ordinary_map_owned_get;
mod ownership;
mod runtime_glue;
mod scalar;
mod string_list;

#[cfg(test)]
#[path = "ffi_vararg_tests.rs"]
mod ffi_vararg_tests;

#[cfg(test)]
mod string_list_tests;

struct NativeMirEmitter<'a, 'ctx> {
    generator: &'a mut CodeGenerator<'ctx>,
    program: &'a MirProgram,
    report_canonical_mir_native_census: bool,
    register_map_runtime_shutdown: bool,
    dynamic_keys_get_authorized: bool,
    ordinary_map_new_result_l1_authorized: bool,
    ordinary_map_set_result_l1_authorized: bool,
    ordinary_map_set_get_i32_l1_authorized: bool,
    ordinary_map_set_get_any_i32_l1_authorized: bool,
    ordinary_map_set_get_any_result_i32_l1_authorized: bool,
    ordinary_map_set_get_string_l1_authorized: bool,
    ordinary_map_direct_tuple_get_unpack_l1_authorized: bool,
    ordinary_map_direct_print_result_authorized: bool,
    computed_string_key_owner_chain_l1_authorized: bool,
    map_set_drop_control_result_l1_authorized: bool,
    values_local_index_i32_result_branch_l1_authorized: bool,
    ordinary_map_owned_get_result_branch_v3_authorized: bool,
    functions: BTreeMap<crate::core::NodeId, FunctionValue<'ctx>>,
    ffi_functions: BTreeMap<String, FunctionValue<'ctx>>,
    actor_dispatchers: BTreeMap<crate::core::NodeId, FunctionValue<'ctx>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CanonicalMirNativeCensusEntry {
    owner: String,
    llvm_symbol: String,
    has_body: bool,
    verified: bool,
}

fn canonical_mir_native_census_enabled() -> bool {
    matches!(std::env::var("MIMI_ROUTE_CENSUS").as_deref(), Ok("1"))
}

fn canonical_mir_native_owner_event(owner: &str) -> String {
    format!("info: canonical MIR native emitted owner='{owner}'")
}

fn canonical_mir_native_summary_event(emitted: usize, expected: usize, mir_digest: &str) -> String {
    format!(
        "info: canonical MIR native compiled {emitted}/{expected} function(s), 0 fell back; mir_digest={mir_digest}"
    )
}

fn validate_canonical_mir_native_census_roster(
    expected_owners: &[String],
    entries: &[CanonicalMirNativeCensusEntry],
) -> Result<(), String> {
    let expected = expected_owners.iter().cloned().collect::<BTreeSet<_>>();
    if expected.len() != expected_owners.len() {
        return Err("canonical MIR census contains duplicate expected owners".into());
    }
    if entries.len() != expected_owners.len() {
        return Err(format!(
            "canonical MIR census has {} emitted entries for {} MIR functions",
            entries.len(),
            expected_owners.len()
        ));
    }

    let mut observed_owners = BTreeSet::new();
    let mut observed_symbols = BTreeSet::new();
    for entry in entries {
        if !observed_owners.insert(entry.owner.clone()) {
            return Err(format!(
                "canonical MIR census contains duplicate emitted owner '{}'",
                entry.owner
            ));
        }
        if !expected.contains(&entry.owner) {
            return Err(format!(
                "canonical MIR census emitted unexpected owner '{}'",
                entry.owner
            ));
        }
        if !observed_symbols.insert(entry.llvm_symbol.clone()) {
            return Err(format!(
                "canonical MIR census maps multiple owners to LLVM symbol '{}'",
                entry.llvm_symbol
            ));
        }
        if !entry.has_body {
            return Err(format!(
                "canonical MIR census owner '{}' is only an LLVM declaration",
                entry.owner
            ));
        }
        if !entry.verified {
            return Err(format!(
                "canonical MIR census owner '{}' failed LLVM function verification",
                entry.owner
            ));
        }
    }
    if observed_owners != expected {
        let missing = expected
            .difference(&observed_owners)
            .map(String::as_str)
            .collect::<Vec<_>>();
        return Err(format!(
            "canonical MIR census is missing emitted owners: {}",
            missing.join(", ")
        ));
    }
    Ok(())
}

impl<'a, 'ctx> NativeMirEmitter<'a, 'ctx> {
    fn new(
        generator: &'a mut CodeGenerator<'ctx>,
        program: &'a MirProgram,
        dynamic_keys_get_authorized: bool,
    ) -> Self {
        let report_canonical_mir_native_census = !generator.suppress_canonical_mir_native_census;
        // These checker-certified owner routes allocate into process-wide
        // registries. Register teardown for every ABI family, including V1
        // owned-get/lifecycle owners and the independent Map/Any owner V1.
        let register_map_runtime_shutdown = program.functions().values().any(|function| {
            let ownership = &function.ownership;
            ownership.has_checker_map_any_owner_actions()
                || ownership.has_checker_ordinary_map_owned_any_v2_actions()
                || ownership
                    .ordinary_map_direct_tuple_get_unpack_l1_receipt()
                    .is_some()
                || ownership.ordinary_map_new_result_receipt().is_some()
                || ownership.ordinary_map_set_lineage_receipt().is_some()
                || ownership
                    .ordinary_map_set_get_i32_lineage_receipt()
                    .is_some()
                || ownership
                    .ordinary_map_set_get_any_i32_lineage_receipt()
                    .is_some()
                || ownership
                    .ordinary_map_set_get_any_result_i32_lineage_receipt()
                    .is_some()
                || ownership
                    .ordinary_map_set_get_string_lineage_receipt()
                    .is_some()
                || ownership
                    .ordinary_map_owned_any_v2_direct_print_result_receipt()
                    .is_some()
                || ownership.has_checker_ordinary_map_owned_get_i32_string_v1_actions()
                || ownership.has_checker_ordinary_map_owner_lifecycle_v1_actions()
        });
        Self {
            generator,
            program,
            report_canonical_mir_native_census,
            register_map_runtime_shutdown,
            dynamic_keys_get_authorized,
            ordinary_map_new_result_l1_authorized: false,
            ordinary_map_set_result_l1_authorized: false,
            ordinary_map_set_get_i32_l1_authorized: false,
            ordinary_map_set_get_any_i32_l1_authorized: false,
            ordinary_map_set_get_any_result_i32_l1_authorized: false,
            ordinary_map_set_get_string_l1_authorized: false,
            ordinary_map_direct_tuple_get_unpack_l1_authorized: false,
            ordinary_map_direct_print_result_authorized: false,
            computed_string_key_owner_chain_l1_authorized: false,
            map_set_drop_control_result_l1_authorized: false,
            values_local_index_i32_result_branch_l1_authorized: false,
            ordinary_map_owned_get_result_branch_v3_authorized: false,
            functions: BTreeMap::new(),
            ffi_functions: BTreeMap::new(),
            actor_dispatchers: BTreeMap::new(),
        }
    }

    fn compile(mut self) -> Result<(), NativeMirError> {
        let has_owned_get_result_branch_v3 = self.program.functions().values().any(|function| {
            function
                .ownership
                .ordinary_map_owned_get_result_branch_v3_receipt()
                .is_some()
                || function.blocks.values().any(|block| {
                    block.instructions.iter().any(|instruction| {
                        matches!(
                            instruction.kind,
                            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1NewResult { .. }
                                | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1SetResult { .. }
                        )
                    })
                })
        });
        if has_owned_get_result_branch_v3
            && !self.ordinary_map_owned_get_result_branch_v3_authorized
        {
            return Err(NativeMirError::new(
                "ordinary Map owned-Get ResultBranchV3",
                "fallible New/Set native lowering requires its exact CheckedProgram-bound route",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE));
        }
        // This fused callsite operation has a native contract of its own.
        // Revalidate its typed MIR receipt before adding any LLVM declarations
        // so an incomplete, duplicated, or tampered profile cannot partially
        // mutate the destination module.
        ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_get_or_default_i32_native_contract(
            self.program,
        )
        .map_err(|messages| {
            NativeMirError::new(
                "OrdinaryMapOwnedAnyV2GetOrDefaultI32 native preflight",
                messages.join("; "),
            )
        })?;
        ordinary_map_owned_any_v2::validate_ordinary_map_owned_any_v2_get_or_default_string_native_contract(
            self.program,
        )
        .map_err(|messages| {
            NativeMirError::new(
                "OrdinaryMapOwnedAnyV2GetOrDefaultString native preflight",
                messages.join("; "),
            )
        })?;
        self.declare_canonical_runtime_helpers();
        self.declare_actor_runtime();
        self.declare_functions()?;
        self.declare_actor_dispatchers()?;
        self.declare_ffi_functions()?;
        let owners = self.program.functions().keys().cloned().collect::<Vec<_>>();
        for owner in owners {
            let function = self.program.functions().get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "function disappeared during native emission",
                )
            })?;
            let llvm_function = *self.functions.get(&owner).ok_or_else(|| {
                NativeMirError::new(owner.0.clone(), "LLVM function declaration is absent")
            })?;
            NativeMirFunctionEmitter::new(
                self.generator,
                self.program,
                &self.functions,
                &self.ffi_functions,
                &self.actor_dispatchers,
                self.register_map_runtime_shutdown,
                self.dynamic_keys_get_authorized,
                self.ordinary_map_new_result_l1_authorized,
                self.ordinary_map_set_result_l1_authorized,
                self.ordinary_map_set_get_i32_l1_authorized,
                self.ordinary_map_set_get_any_i32_l1_authorized,
                self.ordinary_map_set_get_any_result_i32_l1_authorized,
                self.ordinary_map_set_get_string_l1_authorized,
                self.ordinary_map_direct_tuple_get_unpack_l1_authorized,
                self.ordinary_map_direct_print_result_authorized,
                self.computed_string_key_owner_chain_l1_authorized,
                self.map_set_drop_control_result_l1_authorized,
                self.values_local_index_i32_result_branch_l1_authorized,
                function,
                llvm_function,
            )
            .emit()?;
        }
        self.emit_actor_dispatchers()?;
        self.emit_actor_root_wrapper()?;
        self.emit_scalar_export_wrappers()?;
        let census_roster =
            if self.report_canonical_mir_native_census && canonical_mir_native_census_enabled() {
                Some(self.validated_native_function_roster()?)
            } else {
                None
            };
        self.generator
            .module
            .verify()
            .map_err(|error| NativeMirError::new("LLVM module", error.to_string()))?;
        if let Some(roster) = census_roster {
            for entry in &roster {
                eprintln!("{}", canonical_mir_native_owner_event(&entry.owner));
            }
            eprintln!(
                "{}",
                canonical_mir_native_summary_event(
                    roster.len(),
                    self.program.functions().len(),
                    &self.program.canonical_digest(),
                )
            );
        }
        Ok(())
    }

    /// Emit the exact Checker/MIR NewResult graph after its production route
    /// receipt and native shape have been validated by the checked entrypoint.
    fn compile_ordinary_map_new_result_l1(mut self) -> Result<(), NativeMirError> {
        validate::validate_ordinary_map_new_result_l1_native_route(self.program)?;
        self.ordinary_map_new_result_l1_authorized = true;
        self.declare_canonical_runtime_helpers();
        self.declare_functions()?;
        let owners = self.program.functions().keys().cloned().collect::<Vec<_>>();
        for owner in owners {
            let function = self.program.functions().get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "function disappeared during native NewResult emission",
                )
            })?;
            let llvm_function = *self.functions.get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "native NewResult function declaration is absent",
                )
            })?;
            NativeMirFunctionEmitter::new(
                self.generator,
                self.program,
                &self.functions,
                &self.ffi_functions,
                &self.actor_dispatchers,
                self.register_map_runtime_shutdown,
                self.dynamic_keys_get_authorized,
                self.ordinary_map_new_result_l1_authorized,
                self.ordinary_map_set_result_l1_authorized,
                self.ordinary_map_set_get_i32_l1_authorized,
                self.ordinary_map_set_get_any_i32_l1_authorized,
                self.ordinary_map_set_get_any_result_i32_l1_authorized,
                self.ordinary_map_set_get_string_l1_authorized,
                self.ordinary_map_direct_tuple_get_unpack_l1_authorized,
                self.ordinary_map_direct_print_result_authorized,
                self.computed_string_key_owner_chain_l1_authorized,
                self.map_set_drop_control_result_l1_authorized,
                self.values_local_index_i32_result_branch_l1_authorized,
                function,
                llvm_function,
            )
            .emit()?;
        }
        self.generator
            .module
            .verify()
            .map_err(|error| NativeMirError::new("LLVM NewResult module", error.to_string()))?;
        Ok(())
    }

    /// Emit the exact Checker/MIR SetResult graph after its independent
    /// SetLineage route receipt and native shape have been validated by the
    /// checked entrypoint.
    fn compile_ordinary_map_set_result_l1(mut self) -> Result<(), NativeMirError> {
        validate::validate_ordinary_map_set_result_l1_native_route(self.program)?;
        self.ordinary_map_set_result_l1_authorized = true;
        self.declare_canonical_runtime_helpers();
        self.declare_functions()?;
        let owners = self.program.functions().keys().cloned().collect::<Vec<_>>();
        for owner in owners {
            let function = self.program.functions().get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "function disappeared during native SetResult emission",
                )
            })?;
            let llvm_function = *self.functions.get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "native SetResult function declaration is absent",
                )
            })?;
            NativeMirFunctionEmitter::new(
                self.generator,
                self.program,
                &self.functions,
                &self.ffi_functions,
                &self.actor_dispatchers,
                self.register_map_runtime_shutdown,
                self.dynamic_keys_get_authorized,
                self.ordinary_map_new_result_l1_authorized,
                self.ordinary_map_set_result_l1_authorized,
                self.ordinary_map_set_get_i32_l1_authorized,
                self.ordinary_map_set_get_any_i32_l1_authorized,
                self.ordinary_map_set_get_any_result_i32_l1_authorized,
                self.ordinary_map_set_get_string_l1_authorized,
                self.ordinary_map_direct_tuple_get_unpack_l1_authorized,
                self.ordinary_map_direct_print_result_authorized,
                self.computed_string_key_owner_chain_l1_authorized,
                self.map_set_drop_control_result_l1_authorized,
                self.values_local_index_i32_result_branch_l1_authorized,
                function,
                llvm_function,
            )
            .emit()?;
        }
        self.generator
            .module
            .verify()
            .map_err(|error| NativeMirError::new("LLVM SetResult module", error.to_string()))?;
        Ok(())
    }

    /// Emit the exact Checker-issued New/Set/Get-i32 graph after its own
    /// SetGet lineage route has been validated by the checked entrypoint.
    fn compile_ordinary_map_set_get_i32_l1(mut self) -> Result<(), NativeMirError> {
        validate::validate_ordinary_map_set_get_i32_l1_native_route(self.program)?;
        self.ordinary_map_set_get_i32_l1_authorized = true;
        self.declare_canonical_runtime_helpers();
        self.declare_functions()?;
        let owners = self.program.functions().keys().cloned().collect::<Vec<_>>();
        for owner in owners {
            let function = self.program.functions().get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "function disappeared during native Set/Get-i32 emission",
                )
            })?;
            let llvm_function = *self.functions.get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "native Set/Get-i32 function declaration is absent",
                )
            })?;
            NativeMirFunctionEmitter::new(
                self.generator,
                self.program,
                &self.functions,
                &self.ffi_functions,
                &self.actor_dispatchers,
                self.register_map_runtime_shutdown,
                self.dynamic_keys_get_authorized,
                self.ordinary_map_new_result_l1_authorized,
                self.ordinary_map_set_result_l1_authorized,
                self.ordinary_map_set_get_i32_l1_authorized,
                self.ordinary_map_set_get_any_i32_l1_authorized,
                self.ordinary_map_set_get_any_result_i32_l1_authorized,
                self.ordinary_map_set_get_string_l1_authorized,
                self.ordinary_map_direct_tuple_get_unpack_l1_authorized,
                self.ordinary_map_direct_print_result_authorized,
                self.computed_string_key_owner_chain_l1_authorized,
                self.map_set_drop_control_result_l1_authorized,
                self.values_local_index_i32_result_branch_l1_authorized,
                function,
                llvm_function,
            )
            .emit()?;
        }
        self.generator
            .module
            .verify()
            .map_err(|error| NativeMirError::new("LLVM Set/Get-i32 module", error.to_string()))?;
        Ok(())
    }

    /// Emit the exact Checker-issued New/Set/GetAny/unpack-i32 graph after its
    /// independent composite receipt and native L1 shape have been validated.
    /// This bit authorizes the Result-aware generic GetAny path only for this
    /// graph; the typed Get-i32 and CheckedGet capabilities stay independent.
    fn compile_ordinary_map_set_get_any_i32_l1(mut self) -> Result<(), NativeMirError> {
        validate::validate_ordinary_map_set_get_any_i32_l1_native_route(self.program)?;
        self.ordinary_map_set_get_any_i32_l1_authorized = true;
        self.declare_canonical_runtime_helpers();
        self.declare_functions()?;
        let owners = self.program.functions().keys().cloned().collect::<Vec<_>>();
        for owner in owners {
            let function = self.program.functions().get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "function disappeared during native Set/GetAny-i32 emission",
                )
            })?;
            let llvm_function = *self.functions.get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "native Set/GetAny-i32 function declaration is absent",
                )
            })?;
            NativeMirFunctionEmitter::new(
                self.generator,
                self.program,
                &self.functions,
                &self.ffi_functions,
                &self.actor_dispatchers,
                self.register_map_runtime_shutdown,
                self.dynamic_keys_get_authorized,
                self.ordinary_map_new_result_l1_authorized,
                self.ordinary_map_set_result_l1_authorized,
                self.ordinary_map_set_get_i32_l1_authorized,
                self.ordinary_map_set_get_any_i32_l1_authorized,
                self.ordinary_map_set_get_any_result_i32_l1_authorized,
                self.ordinary_map_set_get_string_l1_authorized,
                self.ordinary_map_direct_tuple_get_unpack_l1_authorized,
                self.ordinary_map_direct_print_result_authorized,
                self.computed_string_key_owner_chain_l1_authorized,
                self.map_set_drop_control_result_l1_authorized,
                self.values_local_index_i32_result_branch_l1_authorized,
                function,
                llvm_function,
            )
            .emit()?;
        }
        self.generator.module.verify().map_err(|error| {
            NativeMirError::new("LLVM Set/GetAny-i32 module", error.to_string())
        })?;
        Ok(())
    }

    /// Emit the exact Checker-certified direct tuple Get L1 graph after its
    /// independent source, route digest, executable MIR, and TypeDesc gates.
    /// This capability is disjoint from the composite i32 GetAny route.
    fn compile_ordinary_map_direct_tuple_get_unpack_l1(
        mut self,
        checked: &crate::core::CheckedProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), NativeMirError> {
        validate::validate_ordinary_map_direct_tuple_get_unpack_l1_native_route(
            checked,
            self.program,
            receipt,
        )?;
        self.ordinary_map_direct_tuple_get_unpack_l1_authorized = true;
        self.declare_canonical_runtime_helpers();
        self.declare_functions()?;
        let owners = self.program.functions().keys().cloned().collect::<Vec<_>>();
        for owner in owners {
            let function = self.program.functions().get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "function disappeared during direct tuple Get L1 emission",
                )
            })?;
            let llvm_function = *self.functions.get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "native direct tuple Get L1 function declaration is absent",
                )
            })?;
            NativeMirFunctionEmitter::new(
                self.generator,
                self.program,
                &self.functions,
                &self.ffi_functions,
                &self.actor_dispatchers,
                self.register_map_runtime_shutdown,
                self.dynamic_keys_get_authorized,
                self.ordinary_map_new_result_l1_authorized,
                self.ordinary_map_set_result_l1_authorized,
                self.ordinary_map_set_get_i32_l1_authorized,
                self.ordinary_map_set_get_any_i32_l1_authorized,
                self.ordinary_map_set_get_any_result_i32_l1_authorized,
                self.ordinary_map_set_get_string_l1_authorized,
                self.ordinary_map_direct_tuple_get_unpack_l1_authorized,
                self.ordinary_map_direct_print_result_authorized,
                self.computed_string_key_owner_chain_l1_authorized,
                self.map_set_drop_control_result_l1_authorized,
                self.values_local_index_i32_result_branch_l1_authorized,
                function,
                llvm_function,
            )
            .emit()?;
        }
        self.generator.module.verify().map_err(|error| {
            NativeMirError::new("LLVM direct tuple Get L1 module", error.to_string())
        })?;
        Ok(())
    }

    /// Emit the exact nested Result DirectPrint graph only after the checked
    /// route receipt, dedicated Core MIR graph, and native shape gate have
    /// been validated. This authorization is independent of both flat
    /// DirectPrint and ordinary owner-v2 readiness.
    fn compile_ordinary_map_direct_print_result_l1(mut self) -> Result<(), NativeMirError> {
        validate::validate_ordinary_map_direct_print_result_l1_native_route(self.program)?;
        self.ordinary_map_direct_print_result_authorized = true;
        self.declare_canonical_runtime_helpers();
        self.declare_functions()?;
        let owners = self.program.functions().keys().cloned().collect::<Vec<_>>();
        for owner in owners {
            let function = self.program.functions().get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "function disappeared during nested Result DirectPrint emission",
                )
            })?;
            let llvm_function = *self.functions.get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "native nested Result DirectPrint function declaration is absent",
                )
            })?;
            NativeMirFunctionEmitter::new(
                self.generator,
                self.program,
                &self.functions,
                &self.ffi_functions,
                &self.actor_dispatchers,
                self.register_map_runtime_shutdown,
                self.dynamic_keys_get_authorized,
                self.ordinary_map_new_result_l1_authorized,
                self.ordinary_map_set_result_l1_authorized,
                self.ordinary_map_set_get_i32_l1_authorized,
                self.ordinary_map_set_get_any_i32_l1_authorized,
                self.ordinary_map_set_get_any_result_i32_l1_authorized,
                self.ordinary_map_set_get_string_l1_authorized,
                self.ordinary_map_direct_tuple_get_unpack_l1_authorized,
                self.ordinary_map_direct_print_result_authorized,
                self.computed_string_key_owner_chain_l1_authorized,
                self.map_set_drop_control_result_l1_authorized,
                self.values_local_index_i32_result_branch_l1_authorized,
                function,
                llvm_function,
            )
            .emit()?;
        }
        let census_roster =
            if self.report_canonical_mir_native_census && canonical_mir_native_census_enabled() {
                Some(self.validated_native_function_roster()?)
            } else {
                None
            };
        self.generator.module.verify().map_err(|error| {
            NativeMirError::new("LLVM nested Result DirectPrint module", error.to_string())
        })?;
        if let Some(roster) = census_roster {
            for entry in &roster {
                eprintln!("{}", canonical_mir_native_owner_event(&entry.owner));
            }
            eprintln!(
                "{}",
                canonical_mir_native_summary_event(
                    roster.len(),
                    self.program.functions().len(),
                    &self.program.canonical_digest(),
                )
            );
        }
        Ok(())
    }

    /// Emit the exact Checker/MIR computed String-key owner chain. This
    /// private route is the only native authorization for the branch-aware
    /// Any-payload SetResult opcode; ordinary `compile()` keeps that opcode
    /// closed and continues to use the generic variant gate unchanged.
    fn compile_ordinary_map_computed_string_key_owner_chain_route(
        mut self,
        checked: &crate::core::CheckedProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), NativeMirError> {
        validate::validate_ordinary_map_owned_any_v2_computed_string_key_owner_chain_l1_native_route(
            checked,
            self.program,
            receipt,
        )?;
        NativeMirValidator::with_computed_string_key_owner_chain_authorization(self.program)
            .validate()
            .map_err(|errors| {
                NativeMirError::new(
                    "ComputedStringKeyOwnerChain native MIR validation",
                    errors
                        .into_iter()
                        .map(|error| format!("{}: {}", error.subject, error.message))
                        .collect::<Vec<_>>()
                        .join("; "),
                )
            })?;
        self.computed_string_key_owner_chain_l1_authorized = true;
        self.declare_canonical_runtime_helpers();
        self.declare_functions()?;
        let owners = self.program.functions().keys().cloned().collect::<Vec<_>>();
        for owner in owners {
            let function = self.program.functions().get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "function disappeared during computed String-key owner-chain emission",
                )
            })?;
            let llvm_function = *self.functions.get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "computed String-key owner-chain LLVM function declaration is absent",
                )
            })?;
            NativeMirFunctionEmitter::new(
                self.generator,
                self.program,
                &self.functions,
                &self.ffi_functions,
                &self.actor_dispatchers,
                self.register_map_runtime_shutdown,
                self.dynamic_keys_get_authorized,
                self.ordinary_map_new_result_l1_authorized,
                self.ordinary_map_set_result_l1_authorized,
                self.ordinary_map_set_get_i32_l1_authorized,
                self.ordinary_map_set_get_any_i32_l1_authorized,
                self.ordinary_map_set_get_any_result_i32_l1_authorized,
                self.ordinary_map_set_get_string_l1_authorized,
                self.ordinary_map_direct_tuple_get_unpack_l1_authorized,
                self.ordinary_map_direct_print_result_authorized,
                self.computed_string_key_owner_chain_l1_authorized,
                self.map_set_drop_control_result_l1_authorized,
                self.values_local_index_i32_result_branch_l1_authorized,
                function,
                llvm_function,
            )
            .emit()?;
        }
        let census_roster =
            if self.report_canonical_mir_native_census && canonical_mir_native_census_enabled() {
                Some(self.validated_native_function_roster()?)
            } else {
                None
            };
        self.generator.module.verify().map_err(|error| {
            NativeMirError::new(
                "LLVM computed String-key owner-chain module",
                error.to_string(),
            )
        })?;
        if let Some(roster) = census_roster {
            for entry in &roster {
                eprintln!("{}", canonical_mir_native_owner_event(&entry.owner));
            }
            eprintln!(
                "{}",
                canonical_mir_native_summary_event(
                    roster.len(),
                    self.program.functions().len(),
                    &self.program.canonical_digest(),
                )
            );
        }
        Ok(())
    }

    /// Emit the independently receipted MapSetDropControl Result branch.
    /// This path has a dedicated native capability and never borrows the
    /// computed-key owner-chain authorization.
    fn compile_ordinary_map_set_drop_control_result_route(
        mut self,
        checked: &crate::core::CheckedProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), NativeMirError> {
        validate::validate_ordinary_map_owned_any_v2_map_set_drop_control_result_l1_native_route(
            checked,
            self.program,
            receipt,
        )?;
        NativeMirValidator::with_map_set_drop_control_result_authorization(self.program)
            .validate()
            .map_err(|errors| {
                NativeMirError::new(
                    "MapSetDropControlResult native MIR validation",
                    errors
                        .into_iter()
                        .map(|error| format!("{}: {}", error.subject, error.message))
                        .collect::<Vec<_>>()
                        .join("; "),
                )
            })?;
        self.map_set_drop_control_result_l1_authorized = true;
        self.declare_canonical_runtime_helpers();
        self.declare_functions()?;
        let owners = self.program.functions().keys().cloned().collect::<Vec<_>>();
        for owner in owners {
            let function = self.program.functions().get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "function disappeared during MapSetDropControlResult emission",
                )
            })?;
            let llvm_function = *self.functions.get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "MapSetDropControlResult LLVM function declaration is absent",
                )
            })?;
            NativeMirFunctionEmitter::new(
                self.generator,
                self.program,
                &self.functions,
                &self.ffi_functions,
                &self.actor_dispatchers,
                self.register_map_runtime_shutdown,
                self.dynamic_keys_get_authorized,
                self.ordinary_map_new_result_l1_authorized,
                self.ordinary_map_set_result_l1_authorized,
                self.ordinary_map_set_get_i32_l1_authorized,
                self.ordinary_map_set_get_any_i32_l1_authorized,
                self.ordinary_map_set_get_any_result_i32_l1_authorized,
                self.ordinary_map_set_get_string_l1_authorized,
                self.ordinary_map_direct_tuple_get_unpack_l1_authorized,
                self.ordinary_map_direct_print_result_authorized,
                self.computed_string_key_owner_chain_l1_authorized,
                self.map_set_drop_control_result_l1_authorized,
                self.values_local_index_i32_result_branch_l1_authorized,
                function,
                llvm_function,
            )
            .emit()?;
        }
        self.generator.module.verify().map_err(|error| {
            NativeMirError::new("LLVM MapSetDropControlResult module", error.to_string())
        })?;
        Ok(())
    }

    /// Emit the independently receipted nested-Result Values profile. It has
    /// a dedicated authorization bit so the Label-only MapSetDropControl
    /// path and computed-key owner-chain route cannot authorize this i32 CFG.
    fn compile_ordinary_map_values_local_index_i32_result_branch_route(
        mut self,
        checked: &crate::core::CheckedProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), NativeMirError> {
        validate::validate_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_l1_native_route(
            checked,
            self.program,
            receipt,
        )?;
        NativeMirValidator::with_values_local_index_i32_result_branch_authorization(self.program)
            .validate()
            .map_err(|errors| {
                NativeMirError::new(
                    "ValuesLocalIndexI32ResultBranch native MIR validation",
                    errors
                        .into_iter()
                        .map(|error| format!("{}: {}", error.subject, error.message))
                        .collect::<Vec<_>>()
                        .join("; "),
                )
            })?;
        self.values_local_index_i32_result_branch_l1_authorized = true;
        self.declare_canonical_runtime_helpers();
        self.declare_functions()?;
        let owners = self.program.functions().keys().cloned().collect::<Vec<_>>();
        for owner in owners {
            let function = self.program.functions().get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "function disappeared during ValuesLocalIndexI32ResultBranch emission",
                )
            })?;
            let llvm_function = *self.functions.get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "ValuesLocalIndexI32ResultBranch LLVM function declaration is absent",
                )
            })?;
            NativeMirFunctionEmitter::new(
                self.generator,
                self.program,
                &self.functions,
                &self.ffi_functions,
                &self.actor_dispatchers,
                self.register_map_runtime_shutdown,
                self.dynamic_keys_get_authorized,
                self.ordinary_map_new_result_l1_authorized,
                self.ordinary_map_set_result_l1_authorized,
                self.ordinary_map_set_get_i32_l1_authorized,
                self.ordinary_map_set_get_any_i32_l1_authorized,
                self.ordinary_map_set_get_any_result_i32_l1_authorized,
                self.ordinary_map_set_get_string_l1_authorized,
                self.ordinary_map_direct_tuple_get_unpack_l1_authorized,
                self.ordinary_map_direct_print_result_authorized,
                self.computed_string_key_owner_chain_l1_authorized,
                self.map_set_drop_control_result_l1_authorized,
                self.values_local_index_i32_result_branch_l1_authorized,
                function,
                llvm_function,
            )
            .emit()?;
        }
        self.generator.module.verify().map_err(|error| {
            NativeMirError::new(
                "LLVM ValuesLocalIndexI32ResultBranch module",
                error.to_string(),
            )
        })?;
        Ok(())
    }

    /// Emit the exact Checker/MIR fallible `map_get_any` lineage after its
    /// independent Result-valued source receipt and native L1 shape gate have
    /// both been validated. This authorization is separate from the older
    /// tuple-valued GetAny and unpack-i32 route.
    fn compile_ordinary_map_set_get_any_result_i32_l1(mut self) -> Result<(), NativeMirError> {
        validate::validate_ordinary_map_set_get_any_result_i32_l1_native_route(self.program)?;
        self.ordinary_map_set_get_any_result_i32_l1_authorized = true;
        self.declare_canonical_runtime_helpers();
        self.declare_functions()?;
        let owners = self.program.functions().keys().cloned().collect::<Vec<_>>();
        for owner in owners {
            let function = self.program.functions().get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "function disappeared during native Set/GetAny-Result emission",
                )
            })?;
            let llvm_function = *self.functions.get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "native Set/GetAny-Result function declaration is absent",
                )
            })?;
            NativeMirFunctionEmitter::new(
                self.generator,
                self.program,
                &self.functions,
                &self.ffi_functions,
                &self.actor_dispatchers,
                self.register_map_runtime_shutdown,
                self.dynamic_keys_get_authorized,
                self.ordinary_map_new_result_l1_authorized,
                self.ordinary_map_set_result_l1_authorized,
                self.ordinary_map_set_get_i32_l1_authorized,
                self.ordinary_map_set_get_any_i32_l1_authorized,
                self.ordinary_map_set_get_any_result_i32_l1_authorized,
                self.ordinary_map_set_get_string_l1_authorized,
                self.ordinary_map_direct_tuple_get_unpack_l1_authorized,
                self.ordinary_map_direct_print_result_authorized,
                self.computed_string_key_owner_chain_l1_authorized,
                self.map_set_drop_control_result_l1_authorized,
                self.values_local_index_i32_result_branch_l1_authorized,
                function,
                llvm_function,
            )
            .emit()?;
        }
        self.generator.module.verify().map_err(|error| {
            NativeMirError::new("LLVM Set/GetAny-Result module", error.to_string())
        })?;
        Ok(())
    }

    /// Emit the isolated New/Set<String>/Get<String> graph after both the
    /// Checker-bound execution receipt and the dedicated native L1 shape gate
    /// have been validated. This authorization never opens the generic
    /// ManagedAny Set/Get path or the distinct i32 result profile.
    fn compile_ordinary_map_set_get_string_l1(mut self) -> Result<(), NativeMirError> {
        validate::validate_ordinary_map_set_get_string_l1_native_route(self.program)?;
        self.ordinary_map_set_get_string_l1_authorized = true;
        self.declare_canonical_runtime_helpers();
        self.declare_functions()?;
        let owners = self.program.functions().keys().cloned().collect::<Vec<_>>();
        for owner in owners {
            let function = self.program.functions().get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "function disappeared during native Set/Get-String emission",
                )
            })?;
            let llvm_function = *self.functions.get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "native Set/Get-String function declaration is absent",
                )
            })?;
            NativeMirFunctionEmitter::new(
                self.generator,
                self.program,
                &self.functions,
                &self.ffi_functions,
                &self.actor_dispatchers,
                self.register_map_runtime_shutdown,
                self.dynamic_keys_get_authorized,
                self.ordinary_map_new_result_l1_authorized,
                self.ordinary_map_set_result_l1_authorized,
                self.ordinary_map_set_get_i32_l1_authorized,
                self.ordinary_map_set_get_any_i32_l1_authorized,
                self.ordinary_map_set_get_any_result_i32_l1_authorized,
                self.ordinary_map_set_get_string_l1_authorized,
                self.ordinary_map_direct_tuple_get_unpack_l1_authorized,
                self.ordinary_map_direct_print_result_authorized,
                self.computed_string_key_owner_chain_l1_authorized,
                self.map_set_drop_control_result_l1_authorized,
                self.values_local_index_i32_result_branch_l1_authorized,
                function,
                llvm_function,
            )
            .emit()?;
        }
        self.generator.module.verify().map_err(|error| {
            NativeMirError::new("LLVM Set/Get-String module", error.to_string())
        })?;
        Ok(())
    }

    /// Build a complete MIR-function roster from the actual emitted LLVM
    /// entities. This is only used by the opt-in route census; a missing,
    /// colliding, body-less, or individually invalid function fails closed
    /// before any success event is printed.
    fn validated_native_function_roster(
        &self,
    ) -> Result<Vec<CanonicalMirNativeCensusEntry>, NativeMirError> {
        if self.functions.len() != self.program.functions().len() {
            return Err(NativeMirError::new(
                "canonical MIR census",
                format!(
                    "LLVM function map contains {} entries for {} MIR functions",
                    self.functions.len(),
                    self.program.functions().len()
                ),
            ));
        }

        let mut entries = Vec::with_capacity(self.program.functions().len());
        let mut expected_owners = Vec::with_capacity(self.program.functions().len());
        for (owner, _) in self.program.functions() {
            expected_owners.push(owner.0.clone());
            let registered = self.functions.get(owner).copied().ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "canonical MIR census has no emitted LLVM function",
                )
            })?;
            let symbol = if let Some(export) = self.program.export_declaration(owner) {
                export.body_symbol.clone()
            } else {
                actor_status::actor_native_symbol(self.program, owner)
                    .map_err(|message| NativeMirError::new(owner.0.clone(), message))?
            };
            let actual_symbol = registered.get_name().to_string_lossy().into_owned();
            if actual_symbol != symbol {
                return Err(NativeMirError::new(
                    owner.0.clone(),
                    format!(
                        "canonical MIR census expected LLVM symbol '{symbol}', found '{actual_symbol}'"
                    ),
                ));
            }
            let module_function = self.generator.module.get_function(&symbol).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    format!("canonical MIR census LLVM entity '{symbol}' is absent"),
                )
            })?;
            if module_function.as_value_ref() != registered.as_value_ref() {
                return Err(NativeMirError::new(
                    owner.0.clone(),
                    format!(
                        "canonical MIR census LLVM symbol '{symbol}' does not resolve to its registered function"
                    ),
                ));
            }
            entries.push(CanonicalMirNativeCensusEntry {
                owner: owner.0.clone(),
                llvm_symbol: symbol,
                has_body: !registered.get_basic_blocks().is_empty(),
                verified: registered.verify(false),
            });
        }
        validate_canonical_mir_native_census_roster(&expected_owners, &entries)
            .map_err(|message| NativeMirError::new("canonical MIR census", message))?;
        Ok(entries)
    }

    /// Canonical-only runtime declarations belong to the MIR adapter, not to
    /// the legacy builtin registry.  Keeping this declaration local prevents
    /// an otherwise unrelated legacy LLVM golden from changing merely because
    /// a new MIR glue operation was added.
    fn declare_canonical_runtime_helpers(&self) {
        if self.dynamic_keys_get_authorized
            && self
                .generator
                .module
                .get_function("mimi_args_init")
                .is_none()
        {
            let context = self.generator.context;
            let i8_ptr = context.ptr_type(inkwell::AddressSpace::default());
            self.generator.module.add_function(
                "mimi_args_init",
                context.void_type().fn_type(
                    &[
                        BasicMetadataTypeEnum::IntType(context.i32_type()),
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                    ],
                    false,
                ),
                Some(Linkage::External),
            );
        }
        if self.register_map_runtime_shutdown
            && self
                .generator
                .module
                .get_function("mimi_mir_ordinary_map_domain_register_shutdown")
                .is_none()
        {
            self.generator.module.add_function(
                "mimi_mir_ordinary_map_domain_register_shutdown",
                self.generator.context.void_type().fn_type(&[], false),
                Some(Linkage::External),
            );
        }
        let has_ordinary_map_owner_observer = self
            .program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| &block.instructions)
            .any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnerLifecycleObserverToJson { .. }
                        | MirInstructionKind::OrdinaryMapOwnerLifecycleObserverPrintln { .. }
                )
            });
        if self
            .generator
            .module
            .get_function("mimi_mir_assert_no_live_managed_any_v2_lists")
            .is_none()
        {
            let void = self.generator.context.void_type();
            self.generator.module.add_function(
                "mimi_mir_assert_no_live_managed_any_v2_lists",
                void.fn_type(&[], false),
                Some(Linkage::External),
            );
        }
        if self
            .generator
            .module
            .get_function("mimi_channel_drop")
            .is_none()
        {
            let i64 = self.generator.context.i64_type();
            self.generator.module.add_function(
                "mimi_channel_drop",
                self.generator
                    .context
                    .void_type()
                    .fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                Some(Linkage::External),
            );
        }
        if self
            .generator
            .module
            .get_function("mimi_channel_send")
            .is_none()
        {
            let i64 = self.generator.context.i64_type();
            self.generator.module.add_function(
                "mimi_channel_send",
                self.generator.context.void_type().fn_type(
                    &[
                        BasicMetadataTypeEnum::IntType(i64),
                        BasicMetadataTypeEnum::IntType(i64),
                    ],
                    false,
                ),
                Some(Linkage::External),
            );
        }
        if self
            .generator
            .module
            .get_function("mimi_session_pair")
            .is_none()
        {
            let i64 = self.generator.context.i64_type();
            self.generator.module.add_function(
                "mimi_session_pair",
                i64.fn_type(&[], false),
                Some(Linkage::External),
            );
        }
        if self
            .generator
            .module
            .get_function("mimi_session_lo")
            .is_none()
        {
            let i64 = self.generator.context.i64_type();
            self.generator.module.add_function(
                "mimi_session_lo",
                i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                Some(Linkage::External),
            );
        }
        if self
            .generator
            .module
            .get_function("mimi_session_hi")
            .is_none()
        {
            let i64 = self.generator.context.i64_type();
            self.generator.module.add_function(
                "mimi_session_hi",
                i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                Some(Linkage::External),
            );
        }
        if self
            .generator
            .module
            .get_function("mimi_trap_float_not_finite")
            .is_none()
        {
            let i8_ptr = self
                .generator
                .context
                .ptr_type(inkwell::AddressSpace::default());
            self.generator.module.add_function(
                "mimi_trap_float_not_finite",
                self.generator
                    .context
                    .void_type()
                    .fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
                Some(Linkage::External),
            );
        }
        if self
            .generator
            .module
            .get_function("mimi_set_clone_scalar")
            .is_none()
        {
            let i64 = self.generator.context.i64_type();
            self.generator.module.add_function(
                "mimi_set_clone_scalar",
                i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                Some(Linkage::External),
            );
        }

        if self
            .generator
            .module
            .get_function("mimi_mir_set_to_list_scalar")
            .is_none()
        {
            let i8 = self.generator.context.i8_type();
            let i64 = self.generator.context.i64_type();
            let ptr = self
                .generator
                .context
                .ptr_type(inkwell::AddressSpace::default());
            self.generator.module.add_function(
                "mimi_mir_set_to_list_scalar",
                ptr.fn_type(
                    &[
                        BasicMetadataTypeEnum::IntType(i64),
                        BasicMetadataTypeEnum::IntType(i8),
                    ],
                    false,
                ),
                Some(Linkage::External),
            );
        }

        if self
            .generator
            .module
            .get_function("mimi_mir_list_len_scalar")
            .is_none()
        {
            let i8 = self.generator.context.i8_type();
            let i32 = self.generator.context.i32_type();
            let ptr = self
                .generator
                .context
                .ptr_type(inkwell::AddressSpace::default());
            self.generator.module.add_function(
                "mimi_mir_list_len_scalar",
                i32.fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(ptr),
                        BasicMetadataTypeEnum::IntType(i8),
                    ],
                    false,
                ),
                Some(Linkage::External),
            );
        }

        if self
            .generator
            .module
            .get_function("mimi_mir_list_reverse_scalar")
            .is_none()
        {
            let i8 = self.generator.context.i8_type();
            let ptr = self
                .generator
                .context
                .ptr_type(inkwell::AddressSpace::default());
            self.generator.module.add_function(
                "mimi_mir_list_reverse_scalar",
                ptr.fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(ptr),
                        BasicMetadataTypeEnum::IntType(i8),
                    ],
                    false,
                ),
                Some(Linkage::External),
            );
        }

        let i8_ptr = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());
        let i64 = self.generator.context.i64_type();
        let i32 = self.generator.context.i32_type();
        let void = self.generator.context.void_type();
        if self
            .generator
            .module
            .get_function("mimi_mir_map_root_new")
            .is_none()
        {
            self.generator.module.add_function(
                "mimi_mir_map_root_new",
                i64.fn_type(&[], false),
                Some(Linkage::External),
            );
        }
        if self
            .generator
            .module
            .get_function("mimi_mir_map_root_set")
            .is_none()
        {
            self.generator.module.add_function(
                "mimi_mir_map_root_set",
                i64.fn_type(
                    &[
                        BasicMetadataTypeEnum::IntType(i64),
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::IntType(i64),
                        BasicMetadataTypeEnum::IntType(i32),
                    ],
                    false,
                ),
                Some(Linkage::External),
            );
        }
        if self
            .generator
            .module
            .get_function("mimi_mir_map_root_set_string")
            .is_none()
        {
            self.generator.module.add_function(
                "mimi_mir_map_root_set_string",
                i64.fn_type(
                    &[
                        BasicMetadataTypeEnum::IntType(i64),
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::IntType(i64),
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::IntType(i64),
                    ],
                    false,
                ),
                Some(Linkage::External),
            );
        }
        if self
            .generator
            .module
            .get_function("mimi_mir_map_root_remove")
            .is_none()
        {
            self.generator.module.add_function(
                "mimi_mir_map_root_remove",
                i64.fn_type(
                    &[
                        BasicMetadataTypeEnum::IntType(i64),
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::IntType(i64),
                    ],
                    false,
                ),
                Some(Linkage::External),
            );
        }
        if self
            .generator
            .module
            .get_function("mimi_mir_map_root_size")
            .is_none()
        {
            self.generator.module.add_function(
                "mimi_mir_map_root_size",
                i32.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                Some(Linkage::External),
            );
        }
        if self
            .generator
            .module
            .get_function("mimi_mir_map_root_contains")
            .is_none()
        {
            self.generator.module.add_function(
                "mimi_mir_map_root_contains",
                i32.fn_type(
                    &[
                        BasicMetadataTypeEnum::IntType(i64),
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::IntType(i64),
                    ],
                    false,
                ),
                Some(Linkage::External),
            );
        }
        if self
            .generator
            .module
            .get_function("mimi_mir_map_root_drop")
            .is_none()
        {
            self.generator.module.add_function(
                "mimi_mir_map_root_drop",
                void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                Some(Linkage::External),
            );
        }
        if self
            .program
            .type_catalog()
            .get(&crate::core::mir::types::map_any_owner_v1_type_id())
            .is_some()
        {
            let map_any_owner_v1_helpers = [
                ("mimi_mir_map_any_owner_v1_new", i64.fn_type(&[], false)),
                (
                    "mimi_mir_map_any_owner_v1_set_i32",
                    i64.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::IntType(i32),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_map_any_owner_v1_set_string",
                    i64.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_map_any_owner_v1_remove",
                    i64.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_map_any_owner_v1_size",
                    i32.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                ),
                (
                    "mimi_mir_map_any_owner_v1_contains",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_map_any_owner_v1_drop",
                    void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                ),
            ];
            for (name, function_type) in map_any_owner_v1_helpers {
                if self.generator.module.get_function(name).is_none() {
                    self.generator.module.add_function(
                        name,
                        function_type,
                        Some(Linkage::External),
                    );
                }
            }
        }

        if (self
            .program
            .type_catalog()
            .get(&crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id())
            .is_some()
            || self
                .program
                .type_catalog()
                .get(&crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id())
                .is_some()
            || self
                .program
                .type_catalog()
                .get(&crate::core::mir::types::managed_any_i32_string_v1_type_id())
                .is_some()
            || self
                .program
                .type_catalog()
                .get(&crate::core::mir::types::ordinary_map_owned_any_v2_type_id())
                .is_some())
            && self
                .generator
                .module
                .get_function("mimi_mir_ordinary_map_domain_own_string")
                .is_none()
        {
            self.generator.module.add_function(
                "mimi_mir_ordinary_map_domain_own_string",
                i32.fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::IntType(i64),
                    ],
                    false,
                ),
                Some(Linkage::External),
            );
        }

        if self
            .program
            .type_catalog()
            .get(&crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id())
            .is_some()
        {
            let ordinary_map_helpers = [
                (
                    "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_new",
                    i64.fn_type(&[], false),
                ),
                (
                    "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_clone",
                    i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                ),
                (
                    "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_set_i32",
                    i64.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::IntType(i32),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_set_string",
                    i64.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_remove",
                    i64.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_size",
                    i32.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                ),
                (
                    "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_size_status",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_contains",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_drop",
                    void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                ),
            ];
            for (name, function_type) in ordinary_map_helpers {
                if self.generator.module.get_function(name).is_none() {
                    self.generator.module.add_function(
                        name,
                        function_type,
                        Some(Linkage::External),
                    );
                }
            }

            if has_ordinary_map_owner_observer {
                // The JSON helper returns a detached NUL-terminated
                // allocation. Declare only the narrow C/runtime helpers
                // needed to recover its byte length, print it, and release
                // the scratch allocation. Map programs without observers do
                // not acquire these extra LLVM declarations.
                for (name, function_type) in [
                    (
                        "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_to_json",
                        i8_ptr.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                    ),
                    (
                        "strlen",
                        i64.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
                    ),
                    (
                        "mimi_print_bytes",
                        void.fn_type(
                            &[
                                BasicMetadataTypeEnum::PointerType(i8_ptr),
                                BasicMetadataTypeEnum::IntType(i64),
                            ],
                            false,
                        ),
                    ),
                    (
                        "mimi_string_free",
                        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
                    ),
                ] {
                    if self.generator.module.get_function(name).is_none() {
                        self.generator.module.add_function(
                            name,
                            function_type,
                            Some(Linkage::External),
                        );
                    }
                }
            }
        }

        if self
            .program
            .type_catalog()
            .get(&crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id())
            .is_some()
        {
            let i64_ptr = self
                .generator
                .context
                .ptr_type(inkwell::AddressSpace::default());
            let ordinary_map_owned_get_helpers = [
                (
                    "mimi_mir_ordinary_map_owned_get_i32_string_v1_new",
                    i64.fn_type(&[], false),
                ),
                (
                    "mimi_mir_ordinary_map_owned_get_i32_string_v1_new_result",
                    i32.fn_type(&[BasicMetadataTypeEnum::PointerType(i64_ptr)], false),
                ),
                (
                    "mimi_mir_ordinary_map_owned_get_i32_string_v1_clone",
                    i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                ),
                (
                    "mimi_mir_ordinary_map_owned_get_i32_string_v1_set_i32",
                    i64.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::IntType(i32),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_ordinary_map_owned_get_i32_string_v1_set_i32_result",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::IntType(i32),
                            BasicMetadataTypeEnum::PointerType(i64_ptr),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_ordinary_map_owned_get_i32_string_v1_set_string",
                    i64.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_ordinary_map_owned_get_i32_string_v1_set_string_result",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i64_ptr),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_ordinary_map_owned_get_i32_string_v1_remove",
                    i64.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_ordinary_map_owned_get_i32_string_v1_size",
                    i32.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                ),
                (
                    "mimi_mir_ordinary_map_owned_get_i32_string_v1_contains",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_ordinary_map_owned_get_i32_string_v1_get_i32",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(
                                self.generator
                                    .context
                                    .ptr_type(inkwell::AddressSpace::default()),
                            ),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_ordinary_map_owned_get_i32_string_v1_get_string",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::PointerType(
                                self.generator
                                    .context
                                    .ptr_type(inkwell::AddressSpace::default()),
                            ),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_ordinary_map_owned_get_i32_string_v1_values",
                    i8_ptr.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                ),
                (
                    "mimi_mir_ordinary_map_owned_get_i32_string_v1_drop",
                    void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                ),
            ];
            for (name, function_type) in ordinary_map_owned_get_helpers {
                if self.generator.module.get_function(name).is_none() {
                    self.generator.module.add_function(
                        name,
                        function_type,
                        Some(Linkage::External),
                    );
                }
            }
        }

        // The ordinary Map/Any owner-v2 island has a deliberately small ABI:
        // roots are opaque i64 handles, keys are borrowed `(ptr,len)` byte
        // ranges, and the one supported aggregate payload is passed as a
        // borrowed `Label { text: *const u8, text_len: i64 }` descriptor.
        // Keep these declarations conditional on the canonical V2 TypeDesc
        // so unrelated MIR/legacy modules do not acquire the reserved V2
        // runtime namespace merely because this adapter is linked.
        if self
            .program
            .type_catalog()
            .get(&crate::core::mir::types::ordinary_map_owned_any_v2_type_id())
            .is_some()
        {
            let i64_ptr = self
                .generator
                .context
                .ptr_type(inkwell::AddressSpace::default());
            let i32_ptr = self
                .generator
                .context
                .ptr_type(inkwell::AddressSpace::default());
            let opaque_ptr = self
                .generator
                .context
                .ptr_type(inkwell::AddressSpace::default());
            let ordinary_map_owned_any_v2_helpers = [
                (
                    "mimi_any_payload_v2_new",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i32),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            BasicMetadataTypeEnum::PointerType(i64_ptr),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_any_payload_v2_drop",
                    i32.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                ),
                (
                    "mimi_any_payload_v2_unpack",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::IntType(i32),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::IntType(i64),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_ordinary_map_owned_any_v2_new",
                    i32.fn_type(&[BasicMetadataTypeEnum::PointerType(i64_ptr)], false),
                ),
                (
                    "mimi_ordinary_map_owned_any_v2_clone",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i64_ptr),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_ordinary_map_owned_any_v2_remove",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i64_ptr),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_ordinary_map_owned_any_v2_size",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i32_ptr),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_ordinary_map_owned_any_v2_contains",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i32_ptr),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_ordinary_map_owned_any_v2_get",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i32_ptr),
                            BasicMetadataTypeEnum::PointerType(i64_ptr),
                            BasicMetadataTypeEnum::IntType(i32),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_ordinary_map_owned_any_v2_is_empty",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i32_ptr),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_ordinary_map_owned_any_v2_drop",
                    i32.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                ),
                (
                    "mimi_ordinary_map_owned_any_v2_set",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            BasicMetadataTypeEnum::PointerType(i64_ptr),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_ordinary_map_owned_any_v2_set_i32_result",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::IntType(i32),
                            BasicMetadataTypeEnum::PointerType(i64_ptr),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_ordinary_map_owned_any_v2_get_i32_result",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(i32_ptr),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_ordinary_map_owned_any_v2_unpack_copy_string",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_ordinary_map_owned_any_v2_values_unpack_copy_string",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_mir_ordinary_map_owned_any_v2_keys_unpack_copy_string",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                        ],
                        false,
                    ),
                ),
                (
                    "mimi_ordinary_map_owned_any_v2_keys_list",
                    i32.fn_type(
                        &[
                            BasicMetadataTypeEnum::IntType(i64),
                            BasicMetadataTypeEnum::PointerType(opaque_ptr),
                        ],
                        false,
                    ),
                ),
            ];
            for (name, function_type) in ordinary_map_owned_any_v2_helpers {
                if self.generator.module.get_function(name).is_none() {
                    self.generator.module.add_function(
                        name,
                        function_type,
                        Some(Linkage::External),
                    );
                }
            }

            if self.ordinary_map_set_get_string_l1_authorized
                || self.ordinary_map_direct_print_result_authorized
            {
                let string_result_helpers = [
                    (
                        "mimi_ordinary_map_owned_any_v2_set_string_result",
                        i32.fn_type(
                            &[
                                BasicMetadataTypeEnum::IntType(i64),
                                BasicMetadataTypeEnum::PointerType(i8_ptr),
                                BasicMetadataTypeEnum::IntType(i64),
                                BasicMetadataTypeEnum::PointerType(i8_ptr),
                                BasicMetadataTypeEnum::IntType(i64),
                                BasicMetadataTypeEnum::PointerType(i8_ptr),
                                BasicMetadataTypeEnum::IntType(i64),
                                BasicMetadataTypeEnum::PointerType(i8_ptr),
                                BasicMetadataTypeEnum::PointerType(opaque_ptr),
                                BasicMetadataTypeEnum::PointerType(opaque_ptr),
                                BasicMetadataTypeEnum::PointerType(i64_ptr),
                            ],
                            false,
                        ),
                    ),
                    (
                        "mimi_ordinary_map_owned_any_v2_get_string_result",
                        i32.fn_type(
                            &[
                                BasicMetadataTypeEnum::IntType(i64),
                                BasicMetadataTypeEnum::PointerType(i8_ptr),
                                BasicMetadataTypeEnum::IntType(i64),
                                BasicMetadataTypeEnum::PointerType(i8_ptr),
                                BasicMetadataTypeEnum::IntType(i64),
                                BasicMetadataTypeEnum::PointerType(i8_ptr),
                                // LLVM 18 uses opaque pointers, so `char **`
                                // has the same pointer TypeRef as `char *`.
                                BasicMetadataTypeEnum::PointerType(i8_ptr),
                                BasicMetadataTypeEnum::PointerType(i64_ptr),
                            ],
                            false,
                        ),
                    ),
                ];
                for (name, function_type) in string_result_helpers {
                    if self.generator.module.get_function(name).is_none() {
                        self.generator.module.add_function(
                            name,
                            function_type,
                            Some(Linkage::External),
                        );
                    }
                }
            }

            if self
                .program
                .type_catalog()
                .get(&crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id())
                .is_some()
            {
                let values_list_helpers = [
                    (
                        "mimi_ordinary_map_owned_any_v2_values_list",
                        i32.fn_type(
                            &[
                                BasicMetadataTypeEnum::IntType(i64),
                                BasicMetadataTypeEnum::PointerType(opaque_ptr),
                                BasicMetadataTypeEnum::IntType(i64),
                                BasicMetadataTypeEnum::PointerType(opaque_ptr),
                                BasicMetadataTypeEnum::PointerType(opaque_ptr),
                            ],
                            false,
                        ),
                    ),
                    (
                        "mimi_mir_list_len_managed_any_v2",
                        i32.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
                    ),
                    (
                        "mimi_mir_list_get_managed_any_v2",
                        i32.fn_type(
                            &[
                                BasicMetadataTypeEnum::PointerType(i8_ptr),
                                BasicMetadataTypeEnum::IntType(i64),
                                BasicMetadataTypeEnum::PointerType(i32_ptr),
                                BasicMetadataTypeEnum::PointerType(i64_ptr),
                            ],
                            false,
                        ),
                    ),
                    (
                        "mimi_mir_list_clone_managed_any_v2",
                        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
                    ),
                    (
                        "mimi_mir_list_drop_managed_any_v2",
                        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
                    ),
                ];
                for (name, function_type) in values_list_helpers {
                    if self.generator.module.get_function(name).is_none() {
                        self.generator.module.add_function(
                            name,
                            function_type,
                            Some(Linkage::External),
                        );
                    }
                }
            }
        }

        // V1 and V2 Values snapshots are move-owned lists of generation-
        // bearing owner tokens. Their list storage has dedicated glue; never
        // route these i64 token slots through the copy-scalar helpers. Keep
        // declarations local to the canonical MIR module so legacy LLVM
        // modules do not gain this ABI merely by construction.
        let managed_any_list_helpers = [
            ("mimi_mir_list_new_managed_any", i8_ptr.fn_type(&[], false)),
            (
                "mimi_mir_list_len_managed_any",
                i32.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
            ),
            (
                "mimi_mir_list_push_managed_any",
                self.generator.context.i8_type().fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::IntType(i64),
                    ],
                    false,
                ),
            ),
            (
                "mimi_mir_list_clone_managed_any",
                i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
            ),
            (
                "mimi_mir_list_drop_managed_any",
                void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
            ),
        ];
        for (name, function_type) in managed_any_list_helpers {
            if self.generator.module.get_function(name).is_none() {
                self.generator
                    .module
                    .add_function(name, function_type, Some(Linkage::External));
            }
        }
        self.declare_managed_any_helpers();
    }

    fn declare_functions(&mut self) -> Result<(), NativeMirError> {
        for (owner, function) in self.program.functions() {
            let export = self.program.export_declaration(owner);
            let symbol = match export {
                Some(export) => export.body_symbol.clone(),
                None => actor_status::actor_native_symbol(self.program, owner)
                    .map_err(|message| NativeMirError::new(owner.0.clone(), message))?,
            };
            let tagged = actor_status::is_actor_tagged(self.program, owner);
            let mut parameter_types = function
                .parameters
                .iter()
                .map(|parameter| {
                    let ty = function.values.get(parameter).ok_or_else(|| {
                        NativeMirError::new(owner.0.clone(), "parameter is absent from MIR values")
                    })?;
                    native_basic_type(self.generator.context, self.program.type_catalog(), &ty.ty)
                        .map(BasicMetadataTypeEnum::from)
                })
                .collect::<Result<Vec<_>, _>>()?;
            if self.dynamic_keys_get_authorized && owner.0 == "function:main" {
                parameter_types.push(self.generator.context.i32_type().into());
                parameter_types.push(
                    self.generator
                        .context
                        .ptr_type(inkwell::AddressSpace::default())
                        .into(),
                );
            }
            let result_desc = self
                .program
                .type_catalog()
                .get(&function.result)
                .ok_or_else(|| NativeMirError::new(owner.0.clone(), "result TypeDesc is absent"))?;
            if tagged {
                parameter_types.push(self.generator.context.i64_type().into());
            }
            let function_type = if tagged {
                actor_status::tagged_i32_type(self.generator.context)
                    .fn_type(&parameter_types, false)
            } else if result_desc.is_canonical_ffi_unit() {
                self.generator
                    .context
                    .void_type()
                    .fn_type(&parameter_types, false)
            } else {
                native_basic_type(
                    self.generator.context,
                    self.program.type_catalog(),
                    &function.result,
                )?
                .fn_type(&parameter_types, false)
            };
            let value = self.generator.module.add_function(
                &symbol,
                function_type,
                Some(if tagged {
                    Linkage::Internal
                } else if export.is_some() {
                    Linkage::Internal
                } else {
                    Linkage::External
                }),
            );
            self.functions.insert(owner.clone(), value);
        }
        Ok(())
    }

    fn emit_scalar_export_wrappers(&mut self) -> Result<(), NativeMirError> {
        for declaration in self.program.export_declarations().values() {
            let body = *self.functions.get(&declaration.owner).ok_or_else(|| {
                NativeMirError::new(
                    declaration.owner.0.clone(),
                    "scalar export body declaration is absent",
                )
            })?;
            if self
                .generator
                .module
                .get_function(&declaration.public_symbol)
                .is_some()
            {
                return Err(NativeMirError::new(
                    declaration.public_symbol.clone(),
                    "scalar export public symbol collides with an existing LLVM declaration",
                ));
            }
            let c_parameters = declaration
                .parameters
                .iter()
                .map(|(_, kind)| match kind {
                    NativeExportScalarKind::I32 => Ok(BasicMetadataTypeEnum::IntType(
                        self.generator.context.i32_type(),
                    )),
                    NativeExportScalarKind::I64 => Ok(BasicMetadataTypeEnum::IntType(
                        self.generator.context.i64_type(),
                    )),
                    NativeExportScalarKind::F32 => Ok(BasicMetadataTypeEnum::FloatType(
                        self.generator.context.f32_type(),
                    )),
                    NativeExportScalarKind::F64 => Ok(BasicMetadataTypeEnum::FloatType(
                        self.generator.context.f64_type(),
                    )),
                    NativeExportScalarKind::Bool => Ok(BasicMetadataTypeEnum::IntType(
                        self.generator.context.i8_type(),
                    )),
                    NativeExportScalarKind::Unit => Err(NativeMirError::new(
                        declaration.public_symbol.clone(),
                        "scalar export parameter has unsupported Unit ABI kind",
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?;
            let function_type = match declaration.result.1 {
                NativeExportScalarKind::Unit => self
                    .generator
                    .context
                    .void_type()
                    .fn_type(&c_parameters, false),
                NativeExportScalarKind::I32 => self
                    .generator
                    .context
                    .i32_type()
                    .fn_type(&c_parameters, false),
                NativeExportScalarKind::I64 => self
                    .generator
                    .context
                    .i64_type()
                    .fn_type(&c_parameters, false),
                NativeExportScalarKind::F32 => self
                    .generator
                    .context
                    .f32_type()
                    .fn_type(&c_parameters, false),
                NativeExportScalarKind::F64 => self
                    .generator
                    .context
                    .f64_type()
                    .fn_type(&c_parameters, false),
                NativeExportScalarKind::Bool => self
                    .generator
                    .context
                    .i8_type()
                    .fn_type(&c_parameters, false),
            };
            let wrapper = self.generator.module.add_function(
                &declaration.public_symbol,
                function_type,
                Some(Linkage::External),
            );
            wrapper.set_call_conventions(crate::ffi::abi_to_llvm_call_conv(&declaration.abi));
            let block = self.generator.context.append_basic_block(wrapper, "entry");
            self.generator.builder.position_at_end(block);
            let mut arguments = Vec::with_capacity(declaration.parameters.len());
            for (index, (_, kind)) in declaration.parameters.iter().enumerate() {
                let value = wrapper.get_nth_param(index as u32).ok_or_else(|| {
                    NativeMirError::new(
                        declaration.public_symbol.clone(),
                        format!("scalar export parameter {index} is absent"),
                    )
                })?;
                let value = if matches!(kind, NativeExportScalarKind::Bool) {
                    let value = value.into_int_value();
                    self.generator
                        .builder
                        .build_int_compare(
                            IntPredicate::NE,
                            value,
                            self.generator.context.i8_type().const_zero(),
                            "export_bool_arg",
                        )
                        .map_err(|error| {
                            NativeMirError::new(
                                declaration.public_symbol.clone(),
                                error.to_string(),
                            )
                        })?
                        .into()
                } else {
                    value
                };
                arguments.push(BasicMetadataValueEnum::from(value));
            }
            let call = self
                .generator
                .builder
                .build_call(body, &arguments, "export_body_call")
                .map_err(|error| {
                    NativeMirError::new(declaration.public_symbol.clone(), error.to_string())
                })?;
            match declaration.result.1 {
                NativeExportScalarKind::Unit => {
                    self.generator.builder.build_return(None).map_err(|error| {
                        NativeMirError::new(declaration.public_symbol.clone(), error.to_string())
                    })?;
                }
                NativeExportScalarKind::Bool => {
                    let value = call_try_basic_value(&call).ok_or_else(|| {
                        NativeMirError::new(
                            declaration.public_symbol.clone(),
                            "scalar export bool body returned void",
                        )
                    })?;
                    let value = self
                        .generator
                        .builder
                        .build_int_z_extend(
                            value.into_int_value(),
                            self.generator.context.i8_type(),
                            "export_bool_result",
                        )
                        .map_err(|error| {
                            NativeMirError::new(
                                declaration.public_symbol.clone(),
                                error.to_string(),
                            )
                        })?;
                    self.generator
                        .builder
                        .build_return(Some(&value))
                        .map_err(|error| {
                            NativeMirError::new(
                                declaration.public_symbol.clone(),
                                error.to_string(),
                            )
                        })?;
                }
                _ => {
                    let value = call_try_basic_value(&call).ok_or_else(|| {
                        NativeMirError::new(
                            declaration.public_symbol.clone(),
                            "scalar export body returned void",
                        )
                    })?;
                    self.generator
                        .builder
                        .build_return(Some(&value))
                        .map_err(|error| {
                            NativeMirError::new(
                                declaration.public_symbol.clone(),
                                error.to_string(),
                            )
                        })?;
                }
            }
        }
        Ok(())
    }

    /// Declare checker-owned scalar FFI symbols from the MIR call receipts.
    ///
    /// The legacy emitter obtains these declarations from `ExternFunc` AST
    /// nodes. Canonical native emission instead derives the complete LLVM
    /// signature from the declaration TypeDesc identities carried by
    /// `MirFfiCallContract`; symbol/ABI spelling also comes from that receipt.
    /// This keeps the external boundary AST-free while retaining the current
    /// fail-closed scalar-only island.
    fn declare_ffi_functions(&mut self) -> Result<(), NativeMirError> {
        let mut declarations = Vec::new();
        let mut declarations_by_symbol: BTreeMap<
            String,
            (
                String,
                crate::core::ResolvedTypeId,
                Vec<crate::core::ResolvedTypeId>,
                usize,
                bool,
            ),
        > = BTreeMap::new();
        // `compile_mir_native` has already run the shared receipt gate and
        // native admission validator.  Use the same canonical source-order
        // receipt view as the route manifest and bytecode descriptor table so
        // a symbol's representative span and declaration order are independent
        // of BTreeMap identity order or MIR block storage order.
        for (_, receipt) in self.program.ffi_call_entries_in_source_order() {
            if receipt.fixed_argument_count > receipt.parameter_types.len()
                || receipt.parameter_types.len() != receipt.arguments.len()
            {
                return Err(NativeMirError::new(
                    receipt.symbol.clone(),
                    "FFI call receipt has an invalid fixed-prefix or physical parameter count",
                )
                .with_span(receipt.span));
            }
            let tail_count = receipt.parameter_types.len() - receipt.fixed_argument_count;
            if (!receipt.variadic && tail_count != 0)
                || tail_count != receipt.variadic_arguments.len()
            {
                return Err(NativeMirError::new(
                    receipt.symbol.clone(),
                    "FFI variadic tail does not match its call-specific receipt",
                )
                .with_span(receipt.span));
            }
            for (tail_index, tail) in receipt.variadic_arguments.iter().enumerate() {
                let argument_index = receipt.fixed_argument_count + tail_index;
                if tail.argument_index != argument_index
                    || receipt.parameter_types.get(argument_index) != Some(&tail.promoted_type)
                {
                    return Err(NativeMirError::new(
                        receipt.symbol.clone(),
                        "FFI variadic receipt does not match its physical parameter type",
                    )
                    .with_span(receipt.span));
                }
            }

            let fixed_prefix = receipt.parameter_types[..receipt.fixed_argument_count].to_vec();
            let shape = (
                receipt.abi.clone(),
                receipt.result_type.clone(),
                fixed_prefix.clone(),
                receipt.fixed_argument_count,
                receipt.variadic,
            );
            if let Some(previous) = declarations_by_symbol.get(&receipt.symbol) {
                if previous != &shape {
                    return Err(NativeMirError::new(
                        receipt.symbol.clone(),
                        "FFI call receipts for one symbol disagree on ABI, result, fixed prefix, or variadic declaration",
                    )
                    .with_span(receipt.span));
                }
            } else {
                declarations_by_symbol.insert(receipt.symbol.clone(), shape);
                declarations.push((
                    receipt.symbol.clone(),
                    fixed_prefix,
                    receipt.result_type.clone(),
                    receipt.variadic,
                    receipt.span,
                ));
            }
        }

        for (symbol, parameter_type_ids, result_type, variadic, symbol_span) in declarations {
            let parameter_types = parameter_type_ids
                .iter()
                .map(|ty| {
                    native_ffi_scalar_type(
                        self.generator.context,
                        self.program.type_catalog(),
                        ty,
                        &symbol,
                    )
                    .map(BasicMetadataTypeEnum::from)
                    .map_err(|error| error.with_span(symbol_span))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let function_type = if self
                .program
                .type_catalog()
                .get(&result_type)
                .is_some_and(MirTypeDesc::is_canonical_ffi_unit)
            {
                self.generator
                    .context
                    .void_type()
                    .fn_type(&parameter_types, variadic)
            } else {
                native_ffi_scalar_type(
                    self.generator.context,
                    self.program.type_catalog(),
                    &result_type,
                    &symbol,
                )
                .map_err(|error| error.with_span(symbol_span))?
                .fn_type(&parameter_types, variadic)
            };
            if self.generator.module.get_function(&symbol).is_some() {
                return Err(NativeMirError::new(
                    symbol.clone(),
                    "FFI symbol collides with an already-declared native MIR function",
                )
                .with_span(symbol_span));
            }
            let function =
                self.generator
                    .module
                    .add_function(&symbol, function_type, Some(Linkage::External));
            self.ffi_functions.insert(symbol, function);
        }
        Ok(())
    }
}

fn native_ffi_scalar_type<'ctx>(
    context: &'ctx Context,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
    subject: &str,
) -> Result<BasicTypeEnum<'ctx>, NativeMirError> {
    let descriptor = catalog.get(ty).ok_or_else(|| {
        NativeMirError::new(subject, format!("FFI TypeDesc '{}' is absent", ty.as_str()))
    })?;
    if descriptor.layout != MirLayout::Scalar {
        return Err(NativeMirError::new(
            subject,
            format!(
                "FFI scalar TypeDesc has non-scalar layout {:?}",
                descriptor.layout
            ),
        ));
    }
    if !catalog.is_canonical_ffi_endpoint(ty, false) {
        return Err(NativeMirError::new(
            subject,
            format!(
                "FFI TypeDesc '{}' is outside the complete Copy scalar shape",
                ty.as_str()
            ),
        ));
    }
    let shape = native_ffi_scalar_shape(descriptor.abi).ok_or_else(|| {
        NativeMirError::new(
            subject,
            format!(
                "FFI scalar ABI {:?} is outside the canonical native FFI slice",
                descriptor.abi
            ),
        )
    })?;
    shape.llvm_type(context).ok_or_else(|| {
        NativeMirError::new(
            subject,
            "native FFI scalar shape has an unsupported physical width",
        )
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeFfiScalarShape {
    SignedInteger(u32),
    Bool,
    Float(u32),
}

impl NativeFfiScalarShape {
    fn from_mir_kind(kind: crate::core::mir::types::MirFfiScalarKind) -> Option<Self> {
        match kind {
            crate::core::mir::types::MirFfiScalarKind::I32 => Some(Self::SignedInteger(32)),
            crate::core::mir::types::MirFfiScalarKind::I64 => Some(Self::SignedInteger(64)),
            crate::core::mir::types::MirFfiScalarKind::Bool => Some(Self::Bool),
            crate::core::mir::types::MirFfiScalarKind::F32 => Some(Self::Float(32)),
            crate::core::mir::types::MirFfiScalarKind::F64 => Some(Self::Float(64)),
            crate::core::mir::types::MirFfiScalarKind::Unit => None,
        }
    }

    fn llvm_type<'ctx>(self, context: &'ctx Context) -> Option<BasicTypeEnum<'ctx>> {
        match self {
            Self::SignedInteger(32) => Some(context.i32_type().into()),
            Self::SignedInteger(64) => Some(context.i64_type().into()),
            Self::Bool => Some(context.bool_type().into()),
            Self::Float(64) => Some(context.f64_type().into()),
            Self::Float(32) => Some(context.f32_type().into()),
            // `native_ffi_scalar_shape` is the sole constructor and admits
            // only the widths above. Keep this total so a future shape-map
            // edit fails closed instead of inventing an LLVM ABI.
            Self::SignedInteger(_) | Self::Float(_) => None,
        }
    }
}

/// Return the one physical scalar shape admitted by the native FFI island.
/// All LLVM value, metadata, and declaration checks use this same shape map;
/// the checker-owned receipt remains responsible for deciding whether a call
/// is eligible for the island at all.
fn native_ffi_scalar_shape(abi: MirAbiClass) -> Option<NativeFfiScalarShape> {
    NativeFfiScalarShape::from_mir_kind(abi.canonical_ffi_scalar_kind()?)
}

struct NativeMirFunctionEmitter<'a, 'ctx> {
    generator: &'a mut CodeGenerator<'ctx>,
    program: &'a MirProgram,
    functions: &'a BTreeMap<crate::core::NodeId, FunctionValue<'ctx>>,
    ffi_functions: &'a BTreeMap<String, FunctionValue<'ctx>>,
    actor_dispatchers: &'a BTreeMap<crate::core::NodeId, FunctionValue<'ctx>>,
    actor_domain: Option<inkwell::values::IntValue<'ctx>>,
    register_map_runtime_shutdown: bool,
    dynamic_keys_get_authorized: bool,
    ordinary_map_new_result_l1_authorized: bool,
    ordinary_map_set_result_l1_authorized: bool,
    ordinary_map_set_get_i32_l1_authorized: bool,
    ordinary_map_set_get_any_i32_l1_authorized: bool,
    ordinary_map_set_get_any_result_i32_l1_authorized: bool,
    ordinary_map_set_get_string_l1_authorized: bool,
    ordinary_map_direct_tuple_get_unpack_l1_authorized: bool,
    ordinary_map_direct_print_result_authorized: bool,
    computed_string_key_owner_chain_l1_authorized: bool,
    map_set_drop_control_result_l1_authorized: bool,
    values_local_index_i32_result_branch_l1_authorized: bool,
    function: &'a MirFunction,
    llvm_function: FunctionValue<'ctx>,
    blocks: BTreeMap<MirBlockId, BasicBlock<'ctx>>,
    values: HashMap<MirValueId, BasicValueEnum<'ctx>>,
    phis: HashMap<MirValueId, PhiValue<'ctx>>,
    pending_incoming: Vec<(MirValueId, NativePhiSource<'ctx>, BasicBlock<'ctx>)>,
}

enum NativePhiSource<'ctx> {
    Mir(MirValueId),
    Value(BasicValueEnum<'ctx>),
}

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    fn new(
        generator: &'a mut CodeGenerator<'ctx>,
        program: &'a MirProgram,
        functions: &'a BTreeMap<crate::core::NodeId, FunctionValue<'ctx>>,
        ffi_functions: &'a BTreeMap<String, FunctionValue<'ctx>>,
        actor_dispatchers: &'a BTreeMap<crate::core::NodeId, FunctionValue<'ctx>>,
        register_map_runtime_shutdown: bool,
        dynamic_keys_get_authorized: bool,
        ordinary_map_new_result_l1_authorized: bool,
        ordinary_map_set_result_l1_authorized: bool,
        ordinary_map_set_get_i32_l1_authorized: bool,
        ordinary_map_set_get_any_i32_l1_authorized: bool,
        ordinary_map_set_get_any_result_i32_l1_authorized: bool,
        ordinary_map_set_get_string_l1_authorized: bool,
        ordinary_map_direct_tuple_get_unpack_l1_authorized: bool,
        ordinary_map_direct_print_result_authorized: bool,
        computed_string_key_owner_chain_l1_authorized: bool,
        map_set_drop_control_result_l1_authorized: bool,
        values_local_index_i32_result_branch_l1_authorized: bool,
        function: &'a MirFunction,
        llvm_function: FunctionValue<'ctx>,
    ) -> Self {
        Self {
            generator,
            program,
            functions,
            ffi_functions,
            actor_dispatchers,
            actor_domain: None,
            register_map_runtime_shutdown,
            dynamic_keys_get_authorized,
            ordinary_map_new_result_l1_authorized,
            ordinary_map_set_result_l1_authorized,
            ordinary_map_set_get_i32_l1_authorized,
            ordinary_map_set_get_any_i32_l1_authorized,
            ordinary_map_set_get_any_result_i32_l1_authorized,
            ordinary_map_set_get_string_l1_authorized,
            ordinary_map_direct_tuple_get_unpack_l1_authorized,
            ordinary_map_direct_print_result_authorized,
            computed_string_key_owner_chain_l1_authorized,
            map_set_drop_control_result_l1_authorized,
            values_local_index_i32_result_branch_l1_authorized,
            function,
            llvm_function,
            blocks: BTreeMap::new(),
            values: HashMap::new(),
            phis: HashMap::new(),
            pending_incoming: Vec::new(),
        }
    }

    fn emit(mut self) -> Result<(), NativeMirError> {
        self.create_blocks_and_parameters()?;
        let blocks = self.ordered_blocks();
        for block in &blocks {
            let llvm_block = *self
                .blocks
                .get(&block.id)
                .ok_or_else(|| NativeMirError::new(block.id.to_string(), "LLVM block is absent"))?;
            self.generator.builder.position_at_end(llvm_block);
            if self.dynamic_keys_get_authorized
                && self.function.owner.0 == "function:main"
                && block.id == self.function.entry
            {
                let argc = self.llvm_function.get_nth_param(0).ok_or_else(|| {
                    NativeMirError::new(
                        self.function.owner.0.clone(),
                        "DynamicKeysGet native main lacks host argc",
                    )
                })?;
                let argv = self.llvm_function.get_nth_param(1).ok_or_else(|| {
                    NativeMirError::new(
                        self.function.owner.0.clone(),
                        "DynamicKeysGet native main lacks host argv",
                    )
                })?;
                let args_init = self
                    .generator
                    .module
                    .get_function("mimi_args_init")
                    .ok_or_else(|| {
                        NativeMirError::new(
                            self.function.owner.0.clone(),
                            "DynamicKeysGet runtime argument initializer is absent",
                        )
                    })?;
                self.generator
                    .builder
                    .build_call(args_init, &[argc.into(), argv.into()], "mimi_args_init")
                    .map_err(|error| {
                        NativeMirError::new(self.function.owner.0.clone(), error.to_string())
                    })?;
            }
            if self.register_map_runtime_shutdown
                && self.function.owner.0 == "function:main"
                && block.id == self.function.entry
            {
                let shutdown_registration = self
                    .generator
                    .module
                    .get_function("mimi_mir_ordinary_map_domain_register_shutdown")
                    .ok_or_else(|| {
                        NativeMirError::new(
                            self.function.owner.0.clone(),
                            "ordinary Map shutdown registration declaration is absent",
                        )
                    })?;
                self.generator
                    .builder
                    .build_call(shutdown_registration, &[], "")
                    .map_err(|error| {
                        NativeMirError::new(self.function.owner.0.clone(), error.to_string())
                    })?;
            }
            for instruction in &block.instructions {
                if let Err(error) =
                    self.emit_instruction(&instruction.kind, instruction.id.as_str())
                {
                    let error_span = self
                        .program
                        .ffi_calls()
                        .get(&instruction.id)
                        .map(|receipt| receipt.span)
                        .unwrap_or(crate::span::Span::UNKNOWN);
                    return Err(error.with_span(error_span));
                }
            }
            self.emit_terminator(&block.terminator, &block.id)?;
        }
        self.add_phi_incomings()?;
        Ok(())
    }

    fn ordered_blocks(&self) -> Vec<MirBlock> {
        // LLVM treats the first basic block created for a function as its
        // entry block. MIR blocks live in a BTreeMap for stable IDs, so that
        // lexical order is not the CFG entry order. Keep native and bytecode
        // consumers on the same declared-entry-first layout contract.
        let mut blocks = Vec::with_capacity(self.function.blocks.len());
        if let Some(entry) = self.function.blocks.get(&self.function.entry) {
            blocks.push(entry.clone());
        }
        blocks.extend(
            self.function
                .blocks
                .values()
                .filter(|block| block.id != self.function.entry)
                .cloned(),
        );
        blocks
    }

    fn create_blocks_and_parameters(&mut self) -> Result<(), NativeMirError> {
        let blocks = self.ordered_blocks();
        for block in &blocks {
            let llvm_block = self
                .generator
                .context
                .append_basic_block(self.llvm_function, block.id.as_str());
            self.blocks.insert(block.id.clone(), llvm_block);
        }
        for (index, parameter) in self.function.parameters.iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| {
                NativeMirError::new(
                    parameter.to_string(),
                    format!("function parameter index {index} exceeds the native u32 ABI"),
                )
            })?;
            let value = self.llvm_function.get_nth_param(index).ok_or_else(|| {
                NativeMirError::new(parameter.to_string(), "LLVM function parameter is absent")
            })?;
            self.values.insert(parameter.clone(), value);
        }
        if actor_status::is_actor_tagged(self.program, &self.function.owner) {
            let index = self.u32_abi(
                self.function.parameters.len(),
                "Actor domain argument index",
                self.function.owner.0.as_str(),
            )?;
            let value = self.llvm_function.get_nth_param(index).ok_or_else(|| {
                NativeMirError::new(
                    self.function.owner.0.clone(),
                    "tagged Actor function lacks its hidden domain argument",
                )
            })?;
            self.actor_domain = Some(value.into_int_value());
        }
        for block in &blocks {
            let llvm_block = *self.blocks.get(&block.id).ok_or_else(|| {
                NativeMirError::new(block.id.to_string(), "LLVM block was not created")
            })?;
            self.generator.builder.position_at_end(llvm_block);
            for parameter in &block.parameters {
                let info = self.function.values.get(&parameter.value).ok_or_else(|| {
                    NativeMirError::new(parameter.value.to_string(), "block parameter is absent")
                })?;
                // Unit is a real MIR type but has no LLVM SSA value.  A
                // control-flow join may still carry a unit expression as a
                // semantic edge argument (for example, a `match` arm whose
                // only effect is `println`).  Keep that parameter in MIR for
                // validator/ownership identity, but do not fabricate an LLVM
                // phi node for it; queue_edge/queue_variant_edge apply the
                // same projection when wiring incoming values.
                if self
                    .program
                    .type_catalog()
                    .get(&info.ty)
                    .is_some_and(MirTypeDesc::is_canonical_ffi_unit)
                {
                    continue;
                }
                let ty = if self.ordinary_map_direct_print_result_authorized
                    && info.ty
                        == crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id()
                {
                    native_direct_print_result_root_result_abi(
                        self.generator.context,
                        self.function,
                        self.program.type_catalog(),
                        &info.ty,
                        parameter.value.as_str(),
                    )?
                    .1
                    .into()
                } else if self.ordinary_map_set_get_any_i32_l1_authorized
                    && info.ty
                        == crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id()
                {
                    native_ordinary_map_set_get_any_i32_result_llvm_type(
                        self.generator.context,
                        self.function,
                        self.program.type_catalog(),
                        &info.ty,
                    )?
                } else if self.ordinary_map_set_get_any_result_i32_l1_authorized
                    && info.ty
                        == crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id()
                {
                    native_ordinary_map_set_get_any_result_i32_root_llvm_type(
                        self.generator.context,
                        self.function,
                        self.program.type_catalog(),
                        &info.ty,
                    )?
                } else if self.ordinary_map_set_result_l1_authorized
                    && info.ty
                        == crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id()
                {
                    native_ordinary_map_set_result_llvm_type(
                        self.generator.context,
                        self.function,
                        self.program.type_catalog(),
                        &info.ty,
                    )?
                } else if self.ordinary_map_set_get_string_l1_authorized
                    && (info.ty
                        == crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id()
                        || self
                            .function
                            .ownership
                            .ordinary_map_set_get_string_lineage_receipt()
                            .is_some_and(|receipt| info.ty == receipt.get_result_type))
                {
                    native_ordinary_map_set_get_string_result_llvm_type(
                        self.generator.context,
                        self.function,
                        self.program.type_catalog(),
                        &info.ty,
                    )?
                } else if self.ordinary_map_set_get_any_result_i32_l1_authorized
                    && info.ty
                        == crate::core::mir::types::
                            ordinary_map_owned_any_v2_get_any_result_envelope_type_id()
                {
                    native_ordinary_map_set_get_any_result_i32_llvm_type(
                        self.generator.context,
                        self.function,
                        self.program.type_catalog(),
                        &info.ty,
                    )?
                } else if self.ordinary_map_set_get_i32_l1_authorized
                    && info.ty
                        == crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id()
                {
                    native_ordinary_map_set_result_llvm_type(
                        self.generator.context,
                        self.function,
                        self.program.type_catalog(),
                        &info.ty,
                    )?
                } else if self.ordinary_map_new_result_l1_authorized
                    && info.ty
                        == crate::core::mir::types::ordinary_map_owned_any_v2_root_result_type_id()
                {
                    native_ordinary_map_new_result_llvm_type(
                        self.generator.context,
                        self.function,
                        self.program.type_catalog(),
                        &info.ty,
                    )?
                } else {
                    native_basic_type(
                        self.generator.context,
                        self.program.type_catalog(),
                        &info.ty,
                    )?
                };
                let phi = self
                    .generator
                    .builder
                    .build_phi(ty, parameter.value.as_str())
                    .map_err(|error| {
                        NativeMirError::new(parameter.value.to_string(), error.to_string())
                    })?;
                self.values
                    .insert(parameter.value.clone(), phi.as_basic_value());
                self.phis.insert(parameter.value.clone(), phi);
            }
        }
        Ok(())
    }

    fn validate_nested_direct_print_result_drop(
        &self,
        value: &MirValueId,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        if !self.ordinary_map_direct_print_result_authorized {
            return Ok(());
        }
        let receipt = self
            .function
            .ownership
            .ordinary_map_owned_any_v2_direct_print_result_receipt()
            .ok_or_else(|| {
                NativeMirError::new(subject, "nested Result DirectPrint receipt disappeared")
            })?;
        let matching_set = receipt
            .operations
            .iter()
            .filter(|attempt| {
                attempt.kind == crate::core::ownership::OrdinaryMapOwnedAnyV2AttemptKind::Set
                    && attempt.point == receipt.set_operation
            })
            .collect::<Vec<_>>();
        let [set] = matching_set.as_slice() else {
            return Err(NativeMirError::new(
                subject,
                "nested Result DirectPrint receipt has no unique Set operation for Drop binding",
            ));
        };
        let [_, set_key, set_payload] = set.arguments.as_slice() else {
            return Err(NativeMirError::new(
                subject,
                "nested Result DirectPrint Set operation has no complete Drop operand census",
            ));
        };
        let key = MirValueId::new(format!("literal:{}", set_key.0))
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let get_key = MirValueId::new(format!("literal:{}", receipt.get_key.0))
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let pair = MirValueId::new(format!("local:{}", receipt.pair_local.0 .0))
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let mut allowed = vec![
            (
                format!(
                    "inst:ordinary_map_direct_print_result_l1:drop_set_key:{}",
                    set_key.0
                ),
                key,
            ),
            (
                format!(
                    "inst:ordinary_map_direct_print_result_l1:drop_get_key:{}",
                    receipt.get_key.0
                ),
                get_key,
            ),
            (
                format!(
                    "inst:ordinary_map_direct_print_result_l1:drop_pair:{}",
                    receipt.pair_drop.0
                ),
                pair,
            ),
        ];
        if matches!(
            receipt.shape,
            crate::core::ownership::OrdinaryMapOwnedAnyV2DirectPrintResultShape::HitString
        ) {
            allowed.push((
                format!(
                    "inst:ordinary_map_direct_print_result_l1:drop_set_payload:{}",
                    set_payload.0
                ),
                MirValueId::new(format!("literal:{}", set_payload.0))
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
            ));
        }
        if allowed.iter().any(|(expected_subject, expected_value)| {
            expected_subject == subject && expected_value == value
        }) {
            Ok(())
        } else {
            Err(NativeMirError::new(
                subject,
                "native Drop is not bound to a key, String payload, or pair settlement in the nested Result DirectPrint Checker receipt",
            ))
        }
    }

    fn emit_instruction(
        &mut self,
        instruction: &MirInstructionKind,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        match instruction {
            MirInstructionKind::Actor { operation } => {
                self.emit_actor_operation(operation, subject)?
            }
            MirInstructionKind::Const { result, literal } => {
                let value = self.emit_const(result, literal, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Copy { result, source } => {
                let value = self.value(source, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Move { result, source } => {
                let value = self.value(source, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Clone { result, source } => {
                let source_ty = self.value_type(source, subject)?;
                let is_list = self
                    .program
                    .type_catalog()
                    .get(&source_ty)
                    .is_some_and(|desc| matches!(desc.layout, MirLayout::List { .. }));
                let is_set = self
                    .program
                    .type_catalog()
                    .get(&source_ty)
                    .is_some_and(|desc| matches!(desc.layout, MirLayout::Set { .. }));
                let value = if source_ty
                    == crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id()
                {
                    self.emit_ordinary_map_clone(result, source, subject)?
                } else if source_ty
                    == crate::core::mir::types::ordinary_map_owned_get_i32_string_v1_type_id()
                {
                    self.emit_ordinary_map_owned_get_clone(result, source, subject)?
                } else if source_ty == crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
                {
                    self.emit_ordinary_map_owned_any_v2_clone(result, source, subject)?
                } else if is_list {
                    self.emit_list_clone(source, subject)?
                } else if is_set {
                    self.emit_set_clone(source, subject)?
                } else {
                    self.emit_clone_value(self.value(source, subject)?, &source_ty, subject)?
                };
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Convert { result, source } => {
                let source_value = self.value(source, subject)?;
                let source_ty = self.value_type(source, subject)?;
                let result_ty = self.value_type(result, subject)?;
                let contract = self
                    .program
                    .type_catalog()
                    .validate_conversion(&source_ty, &result_ty)
                    .map_err(|message| NativeMirError::new(subject, message))?;
                let value = match contract.kind {
                    MirConversionKind::ScalarIdentity => source_value,
                    MirConversionKind::SignedI32ToI64 => {
                        let source = source_value.into_int_value();
                        self.generator
                            .builder
                            .build_int_s_extend(
                                source,
                                self.generator.context.i64_type(),
                                "mir_i32_to_i64",
                            )
                            .map(BasicValueEnum::from)
                            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                    }
                    MirConversionKind::SignedI32ToFloat64
                    | MirConversionKind::SignedI64ToFloat64 => {
                        let source = source_value.into_int_value();
                        self.generator
                            .builder
                            .build_signed_int_to_float(
                                source,
                                self.generator.context.f64_type(),
                                "mir_int_to_f64",
                            )
                            .map(BasicValueEnum::from)
                            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                    }
                    MirConversionKind::Float64ToFloat32 => {
                        let source = source_value.into_float_value();
                        self.generator
                            .builder
                            .build_float_trunc(
                                source,
                                self.generator.context.f32_type(),
                                "mir_f64_to_f32",
                            )
                            .map(BasicValueEnum::from)
                            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                    }
                };
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Unary {
                result,
                op,
                operand,
            } => {
                let value = self.emit_unary(result, *op, operand, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Binary {
                result,
                op,
                left,
                right,
            } => {
                let value = self.emit_binary(result, *op, left, right, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Project {
                result,
                base,
                projection,
                list_index_contract,
            } => {
                let value = self.emit_project(
                    result,
                    base,
                    projection,
                    list_index_contract.as_ref(),
                    subject,
                )?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::MoveProject {
                result,
                base,
                projection,
            } => {
                let value = self.emit_move_project(result, base, projection, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::MoveProjectDrop {
                result,
                base,
                projection,
                contract,
            } => {
                let value = self.emit_move_project_drop(
                    result,
                    base,
                    projection,
                    contract.as_ref(),
                    subject,
                )?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::VariantProject {
                result,
                base,
                contract,
            } => {
                let value = self.emit_variant_project(result, base, contract.as_ref(), subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::VariantProjectOr {
                result,
                base,
                fallback,
                contract,
            } => {
                let value = self.emit_variant_project_or(
                    result,
                    base,
                    fallback,
                    contract.as_ref(),
                    subject,
                )?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::VariantProjectMove {
                result,
                base,
                contract,
            } => {
                let value =
                    self.emit_variant_project_move(result, base, contract.as_ref(), subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Borrow {
                result,
                source,
                mutable,
                ..
            } => {
                let value = self.emit_borrow(result, source, *mutable, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::EndBorrow { borrow } => {
                self.emit_end_borrow(borrow, subject)?;
            }
            MirInstructionKind::Construct {
                result,
                kind,
                fields,
            } => {
                let value = self.emit_construct(result, kind, fields, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::UpdateRecord {
                result,
                base,
                kind,
                fields,
                record_update_contract: _,
                record_update_move_contract,
            } => {
                let value = self.emit_update_record(
                    result,
                    base,
                    kind,
                    fields,
                    record_update_move_contract.as_ref(),
                    subject,
                )?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::ConstructVariant {
                result,
                nominal,
                variant,
                fields,
            } => {
                let value =
                    self.emit_construct_variant(result, nominal, variant, fields, subject, false)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::ConstructVariantMove {
                result,
                nominal,
                variant,
                fields,
            } => {
                let value =
                    self.emit_construct_variant(result, nominal, variant, fields, subject, true)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::ConstructList {
                result,
                elements,
                list_construct_contract,
            } => {
                let value = self.emit_list_construct(
                    result,
                    elements,
                    list_construct_contract.as_ref(),
                    subject,
                )?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::ListOp {
                result,
                operation,
                list,
                argument,
                list_operation_contract,
            } => {
                let value = if self.value_type(list, subject)?
                    == crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id()
                {
                    if *operation != MirListOperation::Len || argument.is_some() {
                        return Err(NativeMirError::new(
                            subject,
                            "ordinary Map/Any owner-v2 Values supports only receipted borrow-only Len",
                        ));
                    }
                    self.emit_ordinary_map_owned_any_v2_values_len(result, list, subject)?
                } else {
                    self.emit_list_op(
                        result,
                        *operation,
                        list,
                        argument.as_ref(),
                        list_operation_contract.as_ref(),
                        subject,
                    )?
                };
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::VariantPredicate {
                result,
                predicate,
                variant,
                contract,
            } => {
                let value = self.emit_variant_predicate(
                    result,
                    *predicate,
                    variant,
                    contract.as_ref(),
                    subject,
                )?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::ConstructSet { result, elements } => {
                let value = self.emit_set_construct(result, elements, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::SetOp {
                result,
                operation,
                set,
                argument,
            } => {
                let value =
                    self.emit_set_op(result, *operation, set, argument.as_ref(), subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::MapRootNew { result } => {
                let value = self.emit_map_root_new(result, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::MapRootSet {
                result,
                source,
                key,
                value,
            } => {
                let value = self.emit_map_root_set(result, source, key, value, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::MapRootRemove {
                result,
                source,
                key,
            } => {
                let value = self.emit_map_root_remove(result, source, key, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::MapRootSize { result, root } => {
                let value = self.emit_map_root_size(result, root, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::MapRootContains { result, root, key } => {
                let value = self.emit_map_root_contains(result, root, key, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::MapRootDrop { root } => {
                self.emit_map_root_drop(root, subject)?;
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleNew { result } => {
                let value = self.emit_ordinary_map_new(result, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleSet {
                result,
                source,
                key,
                value,
            } => {
                let value = self.emit_ordinary_map_set(result, source, key, value, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleRemove {
                result,
                source,
                key,
            } => {
                let value = self.emit_ordinary_map_remove(result, source, key, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleSize { result, root } => {
                let value = self.emit_ordinary_map_size(result, root, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleContains { result, root, key } => {
                let value = self.emit_ordinary_map_contains(result, root, key, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleObserverToJson {
                result,
                root,
                callsite,
            } => {
                let value =
                    self.emit_ordinary_map_observer_to_json(result, root, callsite, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleObserverPrintln {
                result,
                root,
                callsite,
            } => {
                self.emit_ordinary_map_observer_println(result, root, callsite, subject)?;
                // Unit is retained only in canonical MIR; it has no LLVM SSA
                // value and cannot be used as a fabricated runtime operand.
            }
            MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { root } => {
                self.emit_ordinary_map_drop(root, subject)?;
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1New { result } => {
                let value = self.emit_ordinary_map_owned_get_new(result, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1NewResult { result } => {
                let value = self.emit_ordinary_map_owned_get_new_result(result, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Set {
                result,
                source,
                key,
                value,
            } => {
                let value =
                    self.emit_ordinary_map_owned_get_set(result, source, key, value, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1SetResult {
                result,
                source,
                key,
                value,
            } => {
                let value = self
                    .emit_ordinary_map_owned_get_set_result(result, source, key, value, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Remove {
                result,
                source,
                key,
            } => {
                let value =
                    self.emit_ordinary_map_owned_get_remove(result, source, key, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Size { result, root } => {
                let value = self.emit_ordinary_map_owned_get_size(result, root, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Contains { result, root, key } => {
                let value =
                    self.emit_ordinary_map_owned_get_contains(result, root, key, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { result, root, key } => {
                let value = self.emit_ordinary_map_get_any(result, root, key, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { result, root } => {
                let value = self.emit_ordinary_map_owned_get_values(result, root, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::ManagedAny {
                result,
                source,
                operation,
                contract,
            } => {
                self.emit_managed_any_operation(
                    result,
                    source,
                    *operation,
                    contract.as_ref(),
                    subject,
                )?;
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetI32 { result, root, key } => {
                let value = self.emit_ordinary_map_owned_get_i32(result, root, key, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetString { result, root, key } => {
                let value = self.emit_ordinary_map_owned_get_string(result, root, key, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Drop { root } => {
                self.emit_ordinary_map_owned_get_drop(root, subject)?;
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2New { result } => {
                let value = if self.ordinary_map_direct_print_result_authorized
                    && self
                        .function
                        .ownership
                        .ordinary_map_owned_any_v2_direct_print_result_receipt()
                        .is_some()
                {
                    self.emit_ordinary_map_owned_any_v2_new_result(result, subject)?
                } else if self.ordinary_map_direct_tuple_get_unpack_l1_authorized
                    && self
                        .function
                        .ownership
                        .ordinary_map_direct_tuple_get_unpack_l1_receipt()
                        .is_some()
                {
                    self.emit_ordinary_map_owned_any_v2_new_result(result, subject)?
                } else if self.ordinary_map_set_get_any_result_i32_l1_authorized
                    && self
                        .function
                        .ownership
                        .ordinary_map_set_get_any_result_i32_lineage_receipt()
                        .is_some()
                {
                    self.emit_ordinary_map_owned_any_v2_new_result(result, subject)?
                } else if self.ordinary_map_set_get_any_i32_l1_authorized
                    && self
                        .function
                        .ownership
                        .ordinary_map_set_get_any_i32_lineage_receipt()
                        .is_some()
                {
                    self.emit_ordinary_map_owned_any_v2_new_result(result, subject)?
                } else if self.ordinary_map_set_get_string_l1_authorized
                    && self
                        .function
                        .ownership
                        .ordinary_map_set_get_string_lineage_receipt()
                        .is_some()
                {
                    self.emit_ordinary_map_owned_any_v2_new_result(result, subject)?
                } else if self.ordinary_map_set_get_i32_l1_authorized
                    && self
                        .function
                        .ownership
                        .ordinary_map_set_get_i32_lineage_receipt()
                        .is_some()
                {
                    self.emit_ordinary_map_owned_any_v2_new_result(result, subject)?
                } else if self.ordinary_map_set_result_l1_authorized
                    && self
                        .function
                        .ownership
                        .ordinary_map_set_lineage_receipt()
                        .is_some()
                {
                    self.emit_ordinary_map_owned_any_v2_new_result(result, subject)?
                } else if self.values_local_index_i32_result_branch_l1_authorized
                    && self
                        .function
                        .ownership
                        .ordinary_map_owned_any_v2_cfg_receipt()
                        .is_some()
                {
                    self.emit_ordinary_map_owned_any_v2_new_result(result, subject)?
                } else if self.computed_string_key_owner_chain_l1_authorized
                    && self
                        .function
                        .ownership
                        .ordinary_map_owned_any_v2_cfg_receipt()
                        .is_some()
                {
                    self.emit_ordinary_map_owned_any_v2_new_result(result, subject)?
                } else if self.ordinary_map_new_result_l1_authorized
                    && self
                        .function
                        .ownership
                        .ordinary_map_new_result_receipt()
                        .is_some()
                {
                    self.emit_ordinary_map_owned_any_v2_new_result(result, subject)?
                } else {
                    self.emit_ordinary_map_owned_any_v2_new(result, subject)?
                };
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Set {
                result,
                source,
                key,
                value,
            } => {
                let value =
                    self.emit_ordinary_map_owned_any_v2_set(result, source, key, value, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2SetResult {
                result,
                source,
                key,
                value,
            } => {
                let value = self.emit_ordinary_map_owned_any_v2_set_result(
                    result, source, key, value, subject,
                )?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2SetAnyResult {
                result,
                source,
                key,
                value,
            } => {
                if self.map_set_drop_control_result_l1_authorized {
                    let value = self.emit_ordinary_map_owned_any_v2_map_set_drop_control_result(
                        result, source, key, value, subject,
                    )?;
                    self.values.insert(result.clone(), value);
                } else if self.values_local_index_i32_result_branch_l1_authorized {
                    let value = self.emit_ordinary_map_owned_any_v2_map_set_drop_control_result(
                        result, source, key, value, subject,
                    )?;
                    self.values.insert(result.clone(), value);
                } else if self.computed_string_key_owner_chain_l1_authorized {
                    let value = self.emit_ordinary_map_owned_any_v2_set_any_result(
                        result, source, key, value, subject,
                    )?;
                    self.values.insert(result.clone(), value);
                } else {
                    return Err(NativeMirError::new(
                        subject,
                        "ordinary Map/Any SetAnyResult requires its exact dedicated native route",
                    ));
                }
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2GetI32Result { result, root, key } => {
                let value =
                    self.emit_ordinary_map_owned_any_v2_get_i32_result(result, root, key, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2SetStringResult {
                result,
                source,
                key,
                value,
            } => {
                let value = self.emit_ordinary_map_owned_any_v2_set_string_result(
                    result, source, key, value, subject,
                )?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2GetStringResult { result, root, key } => {
                let value = self
                    .emit_ordinary_map_owned_any_v2_get_string_result(result, root, key, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Remove {
                result,
                source,
                key,
            } => {
                let value =
                    self.emit_ordinary_map_owned_any_v2_remove(result, source, key, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Size { result, root } => {
                let value = self.emit_ordinary_map_owned_any_v2_size(result, root, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { result, root, key } => {
                let value =
                    self.emit_ordinary_map_owned_any_v2_contains(result, root, key, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { result, root, key } => {
                let value =
                    self.emit_ordinary_map_owned_any_v2_get_any(result, root, key, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2GetAnyResult { result, root, key } => {
                let value =
                    self.emit_ordinary_map_owned_any_v2_get_any_result(result, root, key, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultI32 {
                result,
                root,
                key,
                default,
                callsite,
                call_target,
                type_argument,
            } => {
                let value = self.emit_ordinary_map_owned_any_v2_get_or_default_i32(
                    result,
                    root,
                    key,
                    default,
                    callsite,
                    call_target,
                    type_argument,
                    subject,
                )?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2GetOrDefaultString {
                result,
                root,
                key,
                default,
                callsite,
                call_target,
                type_argument,
            } => {
                let value = self.emit_ordinary_map_owned_any_v2_get_or_default_string(
                    result,
                    root,
                    key,
                    default,
                    callsite,
                    call_target,
                    type_argument,
                    subject,
                )?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack {
                result,
                pair,
                projection,
            } => {
                let value =
                    self.emit_ordinary_map_owned_any_v2_unpack(result, pair, projection, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Println {
                result,
                pair,
                projection,
            } => {
                let value =
                    self.emit_ordinary_map_owned_any_v2_println(result, pair, projection, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack {
                result,
                values,
                index,
                projection,
            } => {
                let value = self.emit_ordinary_map_owned_any_v2_values_unpack(
                    result, values, index, projection, subject,
                )?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Values { result, root } => {
                let value = self.emit_ordinary_map_owned_any_v2_values(result, root, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Keys { result, root } => {
                let value = self.emit_ordinary_map_owned_any_v2_keys(result, root, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2KeysUnpack {
                result,
                keys,
                projection,
            } => {
                let value = self.emit_ordinary_map_owned_any_v2_keys_unpack(
                    result, keys, projection, subject,
                )?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2DynamicArg0 { result, projection } => {
                let value =
                    self.emit_ordinary_map_owned_any_v2_dynamic_arg0(result, projection, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { result, root } => {
                let value = self.emit_ordinary_map_owned_any_v2_is_empty(result, root, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root } => {
                self.emit_ordinary_map_owned_any_v2_drop(root, subject)?;
            }
            MirInstructionKind::Drop { value } => {
                self.validate_nested_direct_print_result_drop(value, subject)?;
                if self.map_set_drop_control_result_l1_authorized {
                    self.validate_map_set_drop_control_result_generic_drop(value, subject)?;
                } else if self.values_local_index_i32_result_branch_l1_authorized {
                    self.validate_values_local_index_i32_result_branch_generic_drop(
                        value, subject,
                    )?;
                }
                self.emit_drop(value, subject)?;
            }
            MirInstructionKind::BuiltinCall {
                result,
                kind,
                arguments,
                string_field_contract,
            } => {
                let value = self.emit_builtin(
                    result,
                    *kind,
                    arguments,
                    string_field_contract.as_ref(),
                    subject,
                )?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::SessionCall {
                result,
                operation,
                endpoint,
                payload,
                contract,
            } => {
                let value = self.emit_session_call(
                    result,
                    *operation,
                    endpoint,
                    payload.as_ref(),
                    contract.as_ref(),
                    subject,
                )?;
                // Unit has no physical LLVM value. Keep the receipt's result
                // at the MIR level only; any later use then fails closed in
                // `value()` instead of observing an ABI placeholder.
                if !self
                    .program
                    .type_catalog()
                    .get(&self.value_type(result, subject)?)
                    .is_some_and(MirTypeDesc::is_canonical_ffi_unit)
                {
                    self.values.insert(result.clone(), value);
                }
            }
            MirInstructionKind::SessionPairBind { lo, hi, contract } => {
                let (lo_value, hi_value) =
                    self.emit_session_pair_bind(lo, hi, contract.as_ref(), subject)?;
                self.values.insert(lo.clone(), lo_value);
                self.values.insert(hi.clone(), hi_value);
            }
            // No-capture closures have no runtime environment value. The
            // shared MIR receipt preflight has already proven this binding is
            // paired with each statically targeted invocation before LLVM
            // declarations are created.
            MirInstructionKind::ClosureBind { .. } => {}
            MirInstructionKind::ClosureInvoke {
                result,
                callee,
                type_arguments,
                arguments,
                ..
            } => self.emit_call(
                result.as_ref(),
                &ResolvedCallee::Function(callee.clone()),
                type_arguments,
                arguments,
                None,
                subject,
            )?,
            MirInstructionKind::Call {
                result,
                callee,
                variant_call_contract,
                arguments,
                type_arguments,
                ..
            } => self.emit_call(
                result.as_ref(),
                callee,
                type_arguments,
                arguments,
                variant_call_contract.as_ref(),
                subject,
            )?,
            MirInstructionKind::FlowTransition {
                result,
                transition,
                arguments,
                effect_receipt,
            } => {
                self.emit_flow_transition(
                    result,
                    transition,
                    arguments,
                    effect_receipt.as_ref(),
                    subject,
                )?;
            }
            MirInstructionKind::Nop => {}
            _ => {
                return Err(NativeMirError::new(
                    subject,
                    "unvalidated instruction reached native emitter",
                ));
            }
        }
        Ok(())
    }

    fn value(
        &self,
        id: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.values.get(id).copied().ok_or_else(|| {
            NativeMirError::new(
                subject,
                format!("value '{id}' is not available at native emission site"),
            )
        })
    }

    fn value_type(
        &self,
        id: &MirValueId,
        subject: &str,
    ) -> Result<crate::core::ResolvedTypeId, NativeMirError> {
        self.function
            .values
            .get(id)
            .map(|value| value.ty.clone())
            .ok_or_else(|| {
                NativeMirError::new(subject, format!("value '{id}' has no canonical type"))
            })
    }

    fn value_desc(
        &self,
        id: &MirValueId,
        subject: &str,
    ) -> Result<&crate::core::mir::types::MirTypeDesc, NativeMirError> {
        let ty = self.value_type(id, subject)?;
        self.program
            .type_catalog()
            .get(&ty)
            .ok_or_else(|| NativeMirError::new(subject, format!("value '{id}' has no TypeDesc")))
    }

    fn u32_abi(&self, value: usize, role: &str, subject: &str) -> Result<u32, NativeMirError> {
        u32::try_from(value).map_err(|_| {
            NativeMirError::new(
                subject,
                format!("{role} {value} exceeds the native u32 aggregate ABI"),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::CodeGenerator;
    use crate::core::ir::{ResolvedBinaryOp, ResolvedLiteral};
    use crate::core::mir::reference::{MirProgram, MirReferenceInterpreter, MirRuntimeValue};
    use crate::core::mir::types::{MirBuiltinKind, MirGlueKind, MirLayout, MirTypeKind};
    use crate::core::mir::{
        MirInstruction, MirInstructionId, MirInstructionKind, MirValue, MirValueId,
    };
    use crate::interp::bytecode::Op;
    use crate::interp::bytecode::{compile_mir_program, BytecodeVM};
    use crate::interp::Value;
    use crate::lexer::Lexer;
    use crate::parser::Parser;
    use inkwell::context::Context;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone, Copy, Debug)]
    enum Index1SetTemporaryTamper {
        RemoveKeyDrop,
        RetargetPayloadCloneDrop,
        AddKeyOwnerUse,
        ReorderTemporaryDrops,
    }

    fn tamper_index1_unselected_set_temporary(
        program: &mut MirProgram,
        tamper: Index1SetTemporaryTamper,
    ) {
        let proof = program
            .ordinary_map_owned_any_v2_values_crosscall_string_caller_receipts()
            .expect("derive the exact V2 Index1 bridge before tampering")
            .into_iter()
            .next()
            .expect("one V2 Index1 aggregate bridge");
        let entry = proof
            .producer_set_entries
            .iter()
            .find(|entry| entry.key == "a")
            .expect("sorted, non-selected Set entry a")
            .clone();
        let mut producer = program
            .functions()
            .get(&proof.producer_owner)
            .expect("producer MIR function")
            .clone();

        let key_drop = entry
            .key_temporary_drop_instruction
            .clone()
            .expect("V2 key temporary Drop identity");
        let payload_clone_drop = entry
            .payload_clone_drop_instruction
            .clone()
            .expect("V2 payload-clone temporary Drop identity");
        match tamper {
            Index1SetTemporaryTamper::RemoveKeyDrop => {
                let mut removed = 0;
                for block in producer.blocks.values_mut() {
                    let before = block.instructions.len();
                    block
                        .instructions
                        .retain(|instruction| instruction.id != key_drop);
                    removed += before - block.instructions.len();
                }
                assert_eq!(removed, 1, "remove exactly the receipted key Drop");
            }
            Index1SetTemporaryTamper::RetargetPayloadCloneDrop => {
                let drop = producer
                    .blocks
                    .values_mut()
                    .flat_map(|block| &mut block.instructions)
                    .find(|instruction| instruction.id == payload_clone_drop)
                    .expect("payload-clone temporary Drop");
                let MirInstructionKind::Drop { value } = &mut drop.kind else {
                    panic!("receipted payload-clone cleanup is not a generic Drop");
                };
                *value = entry.payload_value.clone();
            }
            Index1SetTemporaryTamper::AddKeyOwnerUse => {
                let key_type = producer
                    .values
                    .get(&entry.key_value)
                    .expect("receipted Set key value")
                    .ty
                    .clone();
                let extra_value = MirValueId::new("expr:index1_tamper_extra_key_clone")
                    .expect("fresh extra cloned key identity");
                producer.values.insert(
                    extra_value.clone(),
                    MirValue {
                        id: extra_value.clone(),
                        ty: key_type,
                    },
                );
                let clone_id = MirInstructionId::new("inst:index1_tamper_extra_key_clone")
                    .expect("fresh extra key Clone instruction identity");
                let drop_id = MirInstructionId::new("inst:index1_tamper_extra_key_drop")
                    .expect("fresh extra key Drop instruction identity");
                let block = producer
                    .blocks
                    .values_mut()
                    .find(|block| {
                        block
                            .instructions
                            .iter()
                            .any(|instruction| instruction.id == entry.set_instruction)
                    })
                    .expect("Set instruction block");
                let set_position = block
                    .instructions
                    .iter()
                    .position(|instruction| instruction.id == entry.set_instruction)
                    .expect("Set instruction position");
                block.instructions.splice(
                    set_position..set_position,
                    [
                        MirInstruction {
                            id: clone_id,
                            kind: MirInstructionKind::Clone {
                                result: extra_value.clone(),
                                source: entry.key_value.clone(),
                            },
                        },
                        MirInstruction {
                            id: drop_id,
                            kind: MirInstructionKind::Drop { value: extra_value },
                        },
                    ],
                );
            }
            Index1SetTemporaryTamper::ReorderTemporaryDrops => {
                let block = producer
                    .blocks
                    .values_mut()
                    .find(|block| {
                        block
                            .instructions
                            .iter()
                            .any(|instruction| instruction.id == key_drop)
                            && block
                                .instructions
                                .iter()
                                .any(|instruction| instruction.id == payload_clone_drop)
                    })
                    .expect("both Set temporary Drops share one block");
                let key_position = block
                    .instructions
                    .iter()
                    .position(|instruction| instruction.id == key_drop)
                    .expect("key temporary Drop position");
                let clone_position = block
                    .instructions
                    .iter()
                    .position(|instruction| instruction.id == payload_clone_drop)
                    .expect("payload-clone temporary Drop position");
                assert_eq!(
                    key_position + 1,
                    clone_position,
                    "canonical cleanup is adjacent"
                );
                block.instructions.swap(key_position, clone_position);
            }
        }

        program.replace_function_for_test_only(producer);
    }

    static NATIVE_STRING_CONCAT_FAILURE_FREE_CALLS: AtomicUsize = AtomicUsize::new(0);

    // SAFETY: the native MIR emitter calls this C ABI hook only with a live
    // String allocation. The runtime free function retains its normal owner
    // bookkeeping while this wrapper records the number of releases.
    unsafe extern "C" fn observe_native_string_concat_free(pointer: *mut std::ffi::c_char) {
        NATIVE_STRING_CONCAT_FAILURE_FREE_CALLS.fetch_add(1, Ordering::SeqCst);
        // SAFETY: forwarded unchanged from the generated native String drop.
        unsafe { crate::runtime::mimi_string_free(pointer) };
    }

    // SAFETY: runtime abort synchronously provides a live diagnostic snapshot;
    // this isolated child exits from the callback instead of unwinding through
    // the generated C ABI.
    unsafe extern "C" fn native_string_concat_failure_handler(message: *const std::ffi::c_char) {
        let typed_diagnostic = !message.is_null()
            // SAFETY: runtime abort keeps its diagnostic snapshot alive during
            // the callback.
            && unsafe { std::ffi::CStr::from_ptr(message) }
                .to_bytes()
                .windows(b"[E0800]".len())
                .any(|window| window == b"[E0800]");
        let allocation_attempts = crate::runtime::ordinary_map_string_concat_attempts_for_test(0);
        let publication_attempts = crate::runtime::ordinary_map_string_concat_attempts_for_test(1);
        let releases = NATIVE_STRING_CONCAT_FAILURE_FREE_CALLS.load(Ordering::SeqCst);
        let domain_settled = crate::runtime::ordinary_map_domain_live_counts_for_test()
            .is_ok_and(|counts| counts.owned_strings == 0 && counts.roots == 0);
        eprintln!(
            "native-mir-string-concat-publication injected={typed_diagnostic} allocation_attempts={allocation_attempts} publication_attempts={publication_attempts} releases={releases} domain_settled={domain_settled}"
        );
        let exit_code = if typed_diagnostic
            && allocation_attempts == 1
            && publication_attempts == 1
            && releases == 3
            && domain_settled
        {
            0
        } else {
            97
        };
        // SAFETY: this callback runs only in the test's isolated subprocess.
        unsafe { libc::_exit(exit_code) }
    }

    fn run_native_string_concat_program(program: &MirProgram, failure_stage: i32) -> i32 {
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_string_concat_checkpoint");
        generator
            .compile_mir_native(program)
            .expect("native String concatenation MIR emission");
        generator
            .module
            .verify()
            .expect("native String concatenation module verifies");
        let native_ir = generator.module.print_to_string().to_string();
        assert!(
            native_ir.contains("call i32 @mimi_mir_string_concat_checkpoint(i32 1)"),
            "native concat must emit the publication checkpoint"
        );
        assert!(
            native_ir.contains("[E0800] String concatenation publication failed"),
            "native publication failure must preserve the typed diagnostic"
        );
        assert!(
            native_ir.contains("call i32 @mimi_mir_ordinary_map_domain_own_string"),
            "the Map-bearing MIR must register the concat result before publication"
        );
        let engine = generator
            .module
            .create_jit_execution_engine(inkwell::OptimizationLevel::None)
            .expect("native String concatenation execution engine");
        for (symbol, address) in [
            (
                "mimi_str_clone",
                crate::runtime::mimi_str_clone as *const () as usize,
            ),
            (
                "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_new",
                crate::runtime::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_new as *const ()
                    as usize,
            ),
            (
                "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_set_i32",
                crate::runtime::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_set_i32
                    as *const () as usize,
            ),
            (
                "mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_drop",
                crate::runtime::mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_drop
                    as *const () as usize,
            ),
            (
                "mimi_mir_ordinary_map_domain_own_string",
                crate::runtime::mimi_mir_ordinary_map_domain_own_string as *const () as usize,
            ),
            (
                "mimi_mir_ordinary_map_domain_register_shutdown",
                crate::runtime::mimi_mir_ordinary_map_domain_register_shutdown as *const ()
                    as usize,
            ),
            (
                "mimi_str_concat_ll",
                crate::runtime::mimi_str_concat_ll as *const () as usize,
            ),
            (
                "mimi_mir_string_concat_checkpoint",
                crate::runtime::mimi_mir_string_concat_checkpoint as *const () as usize,
            ),
            (
                "mimi_string_free",
                observe_native_string_concat_free as *const () as usize,
            ),
            (
                "mimi_runtime_abort",
                crate::runtime::mimi_runtime_abort as *const () as usize,
            ),
            (
                "mimi_mir_assert_no_live_managed_any_v2_lists",
                crate::runtime::mimi_mir_assert_no_live_managed_any_v2_lists as *const () as usize,
            ),
        ] {
            assert_ne!(
                address, 0,
                "native String concatenation mapping for {symbol}"
            );
            let import = generator
                .module
                .get_function(symbol)
                .unwrap_or_else(|| panic!("native String concatenation is missing {symbol}"));
            assert_eq!(
                import.count_basic_blocks(),
                0,
                "native String concatenation {symbol} must be a runtime import"
            );
            engine.add_global_mapping(&import, address);
        }
        crate::codegen::assert_execution_engine_linked(
            &engine,
            "calling native String concatenation main",
        );
        let entry = generator.module.get_function("main").expect("native main");
        assert_eq!(entry.count_params(), 0);
        assert_eq!(
            entry.get_type().get_return_type(),
            Some(context.i32_type().into())
        );
        // SAFETY: the emitted entry has a checked zero-argument i32 ABI and
        // the JIT engine remains alive for the duration of this call.
        let run = unsafe { engine.get_function::<unsafe extern "C" fn() -> i32>("main") }
            .expect("native String concatenation main entry");
        NATIVE_STRING_CONCAT_FAILURE_FREE_CALLS.store(0, Ordering::SeqCst);
        if failure_stage == 1 {
            crate::runtime::mimi_runtime_set_error_handler(Some(
                native_string_concat_failure_handler,
            ));
        }
        let (result, allocation_attempts, publication_attempts) =
            crate::runtime::with_ordinary_map_string_concat_failure_for_test(failure_stage, || {
                // SAFETY: same verified entry and live engine as above.
                let result = unsafe { run.call() };
                (
                    result,
                    crate::runtime::ordinary_map_string_concat_attempts_for_test(0),
                    crate::runtime::ordinary_map_string_concat_attempts_for_test(1),
                )
            });
        assert_eq!(allocation_attempts, 1);
        assert_eq!(publication_attempts, 1);
        if failure_stage == 1 {
            crate::runtime::mimi_runtime_set_error_handler(None);
        }
        result
    }

    fn canonical_program(source: &str) -> MirProgram {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        MirProgram::from_checked_program(&checked).expect("canonical MIR")
    }

    fn assert_checker_rejects_wrong_v2_unpack_source(source: &str, context: &str) {
        let tokens = Lexer::new(source)
            .tokenize()
            .expect("lex wrong V2 unpack source");
        let file = Parser::new(tokens)
            .parse_file()
            .expect("parse wrong V2 unpack source");
        let diagnostics = crate::core::check_program(&file).expect_err(context);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_deref() == Some("E0200")),
            "wrong V2 unpack must fail in the source checker with E0200: {diagnostics:?}"
        );
    }

    #[test]
    fn native_mir_string_concat_publication_checkpoint_matches_consumers() {
        const CHILD_ENV: &str = "MIMI_NATIVE_MIR_STRING_CONCAT_FAILURE_CHILD";
        let program = canonical_program(
            r#"
            // Keep a supported ordinary-Map TypeDesc in the same canonical
            // program. The witness is compiled but not called, so the test
            // exercises Map-domain String registration without adding live
            // Map roots to the failure path.
            func map_type_witness() -> i32 {
                let empty = map_new()
                let root = map_set(empty, "seed", 7)
                drop(empty)
                drop(root)
                0
            }
            func main() -> i32 {
                let left = "left"
                let right = "right"
                let joined = left + right
                drop(joined)
                0
            }
            "#,
        );

        assert!(
            program
                .type_catalog()
                .get(&crate::core::mir::types::ordinary_map_owner_lifecycle_i32_string_v1_type_id())
                .is_some(),
            "the canonical MIR test fixture carries the ordinary Map TypeDesc"
        );

        if std::env::var(CHILD_ENV).as_deref() == Ok("1") {
            // The abort callback exits this process after observing the real
            // generated publication-failure edge and its three String frees.
            let _ = run_native_string_concat_program(&program, 1);
            panic!("injected native String publication failure returned normally");
        }

        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference String concatenation succeeds");
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("compile String concatenation bytecode"),
        )
        .run_value()
        .expect("bytecode String concatenation succeeds");
        assert_eq!(reference, MirRuntimeValue::Int(0));
        assert!(matches!(bytecode, Value::Int(0)));
        assert_eq!(
            run_native_string_concat_program(&program, -1),
            0,
            "native String concatenation success remains unchanged"
        );

        let test_module = module_path!()
            .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
            .expect("unit-test module path starts with the crate name");
        let test_name = format!(
            "{test_module}::native_mir_string_concat_publication_checkpoint_matches_consumers"
        );
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .arg("--exact")
            .arg(&test_name)
            .arg("--nocapture")
            .env(CHILD_ENV, "1")
            .output()
            .expect("run isolated native String publication failure");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "native publication-failure child failed: status={} stdout={stdout} stderr={stderr}",
            output.status
        );
        assert!(
            stderr.contains("[E0800] String concatenation publication failed"),
            "native publication failure must report E0800: {stderr}"
        );
        assert!(
            stderr.contains(
                "native-mir-string-concat-publication injected=true allocation_attempts=1 publication_attempts=1 releases=3 domain_settled=true"
            ),
            "native publication failure must stage both checkpoints, free the result plus inputs exactly once, and clear their Map-domain receipts: {stderr}"
        );
    }

    #[test]
    fn owner_v2_public_native_and_bytecode_execution_boundaries_require_readiness() {
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let source = r#"
type Label { text: string }
func main() -> i32 {
    println("must not execute before V2 readiness")
    let label = Label { text: "owned label" }
    let empty = map_new()
    let root = map_set(empty, "label", label)
    drop(empty)
    drop(root)
    drop(label)
    0
}
"#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        assert!(program.functions().values().any(|function| {
            function
                .ownership
                .has_checker_ordinary_map_owned_any_v2_actions()
        }));

        let receipt = program.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        let manifest = receipt.manifest_text().expect("V2 route manifest");
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "owner_v2_public_native_gate");
        let assert_native_gate = |result: Result<(), Vec<crate::diagnostic::Diagnostic>>| {
            let errors =
                result.expect_err("public native emission stays closed until V2 readiness");
            assert!(errors.iter().any(|diagnostic| {
                diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                    && diagnostic
                        .message
                        .contains("diagnostic-only while V2 production readiness is disabled")
            }));
        };
        assert_native_gate(generator.compile_mir_native(&program));
        assert_native_gate(generator.compile_mir_native_with_route_receipt(&program, &receipt));
        assert_native_gate(generator.compile_mir_native_with_route_manifest(&program, &manifest));
        let direct_receipt =
            program.route_receipt(crate::core::mir::MIR_NATIVE_DIRECT_ROUTE_PROFILE);
        assert_native_gate(
            generator.compile_mir_native_with_route_receipt(&program, &direct_receipt),
        );
        let build_receipt =
            program.route_receipt(crate::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE);
        assert_native_gate(
            generator.compile_mir_native_with_route_receipt(&program, &build_receipt),
        );
        assert_native_gate(
            generator.compile_checked_mir_native_with_route_receipt(&checked, &program, &receipt),
        );

        let narrow_receipt =
            program.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE);
        let narrow_errors = CodeGenerator::new(&context, "owner_v2_forged_checked_get_gate")
            .compile_mir_native_with_route_receipt(&program, &narrow_receipt)
            .expect_err("broad owner-v2 operations cannot borrow the CheckedGet native gate");
        assert!(narrow_errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic
                    .message
                    .contains("CheckedGet native route rejected")
        }));

        fn assert_vm_gate<T: std::fmt::Debug>(result: Result<T, crate::interp::InterpError>) {
            let error =
                result.expect_err("public bytecode execution stays closed until V2 readiness");
            assert_eq!(
                error.diagnostic_code(),
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            );
            assert!(error
                .to_string()
                .contains("diagnostic-only while V2 production readiness is disabled"));
        }

        let bytecode = compile_mir_program(&program).expect("L1/L2 bytecode artifact");
        let entry = bytecode.entry;
        let mut vm = BytecodeVM::new(bytecode.clone());
        assert_vm_gate(vm.run_value());
        assert!(
            vm.take_stdout().is_empty(),
            "gate precedes source side effects"
        );
        assert_vm_gate(vm.run());
        assert!(
            vm.take_stdout().is_empty(),
            "gate precedes source side effects"
        );
        assert_vm_gate(vm.call_named("main", Vec::new()));
        assert!(
            vm.take_stdout().is_empty(),
            "gate precedes source side effects"
        );
        assert_vm_gate(vm.call_function(entry, &[]));
        assert!(
            vm.take_stdout().is_empty(),
            "gate precedes source side effects"
        );
        assert_vm_gate(vm.call_function_wrap_ok(entry, &[], Value::Unit));
        assert!(
            vm.take_stdout().is_empty(),
            "gate precedes source side effects"
        );
    }

    #[test]
    fn exact_checked_get_receipt_authorizes_native_emission_without_opening_broad_v2() {
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let source = r#"
func main() -> bool {
    let empty = map_new()
    let root = map_set(empty, "answer", true)
    let pair = map_get(root, "answer")
    drop(empty)
    drop(root)
    let found = pair.0
    let payload = pair.1 as bool
    drop(pair)
    found && payload
}
"#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked)
            .ordinary_map_owned_any_v2;
        assert_eq!(
            admission,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::CheckedGet
        );
        let program = MirProgram::from_checked_program(&checked).expect("CheckedGet MIR");
        let generic_native_errors = crate::codegen::mir::validate_mir_native(&program)
            .expect_err("the generic native validator cannot authorize Bool CheckedGet");
        let generic_native_errors = format!("{generic_native_errors:?}");
        assert!(
            generic_native_errors.contains("Set value is outside the canonical i32/String/Label")
                && generic_native_errors.contains("unpack result is neither canonical i32"),
            "the generic native path must keep both Bool owner checks closed: {generic_native_errors}"
        );
        let generic_receipt = program.route_receipt("canonical-dispatch-v1");
        assert!(
            crate::codegen::mir::validate_mir_native_checked_get_route(&program, &generic_receipt)
                .is_err(),
            "a generic route receipt cannot authorize the Bool CheckedGet exception"
        );
        let checked_get_receipt =
            program.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE);
        crate::codegen::mir::validate_mir_native_checked_get_route(&program, &checked_get_receipt)
            .expect("the exact Checker-bound CheckedGet receipt admits native Bool");
        let context = Context::create();
        let digest = program.canonical_digest();
        for (module_name, profile) in [
            (
                "exact_checked_get_native_route",
                crate::core::mir::MIR_NATIVE_DIRECT_ROUTE_PROFILE,
            ),
            (
                "exact_checked_get_build_route",
                crate::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE,
            ),
            (
                "exact_checked_get_verifier_route",
                crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE,
            ),
        ] {
            let receipt = program.route_receipt(profile);
            let mut generator = CodeGenerator::new(&context, module_name);
            generator
                .compile_mir_native_with_route_receipt(&program, &receipt)
                .expect("an exact CheckedGet route receipt authorizes its native consumer");
            assert_eq!(
                generator.mir_native_compiled_digest.as_deref(),
                Some(digest.as_str())
            );
        }
        let mut checked_generator = CodeGenerator::new(&context, "checked_get_bool_checked_entry");
        checked_generator
            .compile_checked_mir_native_with_route_receipt(&checked, &program, &checked_get_receipt)
            .expect("CheckedProgram coverage admits only the exact Bool CheckedGet receipt");
        let values_receipt = program
            .route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE);
        let cross_profile_errors = CodeGenerator::new(&context, "checked_get_values_receipt")
            .compile_checked_mir_native_with_route_receipt(&checked, &program, &values_receipt)
            .expect_err("ValuesFieldMove cannot borrow a CheckedGet graph");
        assert!(cross_profile_errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic
                    .message
                    .contains("ValuesFieldMove native route rejected")
        }));
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
    }

    #[test]
    fn exact_computed_key_get_route_preflights_and_runs_natively_without_opening_broad_v2() {
        use crate::core::mir::{
            MIR_ROUTE_COVERAGE_ERROR_CODE, MIR_ROUTE_RECEIPT_ERROR_CODE,
            ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE,
            ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
        };

        const {
            assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let source = include_str!(
            "../../../tests/fixtures/mir_ordinary_map_any_owner_v2_computed_key_get.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked)
            .ordinary_map_owned_any_v2;
        assert_eq!(
            admission,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ComputedKeyGet
        );
        let program = MirProgram::from_checked_program(&checked).expect("ComputedKeyGet MIR");
        let receipt = program.route_receipt(ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE);
        let context = Context::create();

        let raw_errors = CodeGenerator::new(&context, "computed_key_get_raw_receipt")
            .compile_mir_native_with_route_receipt(&program, &receipt)
            .expect_err("raw MIR receipts cannot authorize ComputedKeyGet");
        assert!(raw_errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic
                    .message
                    .contains("requires CheckedProgram validation")
        }));

        let generic_errors = CodeGenerator::new(&context, "computed_key_get_generic_checked")
            .compile_checked_mir_native_with_route_receipt(&checked, &program, &receipt)
            .expect_err("the generic CheckedProgram route cannot authorize ComputedKeyGet");
        assert!(generic_errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic
                    .message
                    .contains("dedicated CheckedProgram native route entry")
        }));

        let broad_receipt =
            program.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        let wrong_profile_errors = CodeGenerator::new(&context, "computed_key_get_wrong_profile")
            .compile_checked_mir_native_with_computed_key_get_route(
                &checked,
                &program,
                &broad_receipt,
            )
            .expect_err("ComputedKeyGet requires its exact profile");
        assert!(wrong_profile_errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic.message.contains("requires profile")
        }));

        let mut tampered_receipt = receipt.clone();
        tampered_receipt.mir_digest = "0".repeat(64);
        let tampered_errors = CodeGenerator::new(&context, "computed_key_get_tampered_receipt")
            .compile_checked_mir_native_with_computed_key_get_route(
                &checked,
                &program,
                &tampered_receipt,
            )
            .expect_err("a receipt for another MIR digest is rejected");
        assert!(tampered_errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_RECEIPT_ERROR_CODE)
        }));

        let unrelated_tokens = Lexer::new("func main() -> i32 { 0 }")
            .tokenize()
            .expect("lex unrelated checker");
        let unrelated_file = Parser::new(unrelated_tokens)
            .parse_file()
            .expect("parse unrelated checker");
        let unrelated_checked =
            crate::core::check_program(&unrelated_file).expect("check unrelated program");
        let unrelated_checker_errors =
            CodeGenerator::new(&context, "computed_key_get_unrelated_checker")
                .compile_checked_mir_native_with_computed_key_get_route(
                    &unrelated_checked,
                    &program,
                    &receipt,
                )
                .expect_err("the receipt cannot be paired with unrelated Checker evidence");
        assert!(unrelated_checker_errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic
                    .message
                    .contains("ComputedKeyGet native route rejected")
        }));

        let mut native = CodeGenerator::new(&context, "computed_key_get_production_native");
        native
            .compile_checked_mir_native_with_computed_key_get_route(&checked, &program, &receipt)
            .expect("the exact Checker/MIR ComputedKeyGet profile passes native preflight");
        native.module.verify().expect("valid ComputedKeyGet LLVM");
        assert_eq!(
            native.mir_native_compiled_digest.as_deref(),
            Some(program.canonical_digest().as_str())
        );

        let engine = native
            .module
            .create_jit_execution_engine(inkwell::OptimizationLevel::None)
            .expect("ComputedKeyGet native execution engine");
        for (symbol, address) in [
            (
                "mimi_ordinary_map_owned_any_v2_new",
                crate::runtime::mimi_ordinary_map_owned_any_v2_new as *const () as usize,
            ),
            (
                "mimi_ordinary_map_owned_any_v2_set",
                crate::runtime::mimi_ordinary_map_owned_any_v2_set as *const () as usize,
            ),
            (
                "mimi_ordinary_map_owned_any_v2_get",
                crate::runtime::mimi_ordinary_map_owned_any_v2_get as *const () as usize,
            ),
            (
                "mimi_ordinary_map_owned_any_v2_drop",
                crate::runtime::mimi_ordinary_map_owned_any_v2_drop as *const () as usize,
            ),
            (
                "mimi_any_payload_v2_new",
                crate::runtime::mimi_any_payload_v2_new as *const () as usize,
            ),
            (
                "mimi_any_payload_v2_drop",
                crate::runtime::mimi_any_payload_v2_drop as *const () as usize,
            ),
            (
                "mimi_any_payload_v2_unpack",
                crate::runtime::mimi_any_payload_v2_unpack as *const () as usize,
            ),
            (
                "mimi_str_concat_ll",
                crate::runtime::mimi_str_concat_ll as *const () as usize,
            ),
            (
                "mimi_mir_string_concat_checkpoint",
                crate::runtime::mimi_mir_string_concat_checkpoint as *const () as usize,
            ),
            (
                "mimi_mir_ordinary_map_domain_own_string",
                crate::runtime::mimi_mir_ordinary_map_domain_own_string as *const () as usize,
            ),
            (
                "mimi_mir_ordinary_map_domain_register_shutdown",
                crate::runtime::mimi_mir_ordinary_map_domain_register_shutdown as *const ()
                    as usize,
            ),
            (
                "mimi_str_clone",
                crate::runtime::mimi_str_clone as *const () as usize,
            ),
            (
                "mimi_string_free",
                crate::runtime::mimi_string_free as *const () as usize,
            ),
            (
                "mimi_runtime_abort",
                crate::runtime::mimi_runtime_abort as *const () as usize,
            ),
            (
                "mimi_trap_overflow",
                crate::runtime::mimi_trap_overflow as *const () as usize,
            ),
            (
                "mimi_mir_assert_no_live_managed_any_v2_lists",
                crate::runtime::mimi_mir_assert_no_live_managed_any_v2_lists as *const () as usize,
            ),
        ] {
            assert_ne!(
                address, 0,
                "ComputedKeyGet runtime mapping for {symbol} must be non-empty"
            );
            let import = native.module.get_function(symbol).unwrap_or_else(|| {
                panic!("ComputedKeyGet native module is missing required runtime import {symbol}")
            });
            assert_eq!(
                import.count_basic_blocks(),
                0,
                "ComputedKeyGet {symbol} must be a body-less runtime import"
            );
            engine.add_global_mapping(&import, address);
        }
        let entry = native.module.get_function("main").expect("native main");
        assert_eq!(entry.count_params(), 0);
        assert_eq!(
            entry.get_type().get_return_type(),
            Some(context.i32_type().into())
        );
        // SAFETY: the generated entry has a checked zero-argument i32 ABI,
        // and every runtime import it uses is mapped above to its Mimi runtime.
        let run = unsafe { engine.get_function::<unsafe extern "C" fn() -> i32>("main") }
            .expect("verified native main entry");
        crate::codegen::assert_execution_engine_linked(
            &engine,
            "calling ComputedKeyGet native main",
        );
        // SAFETY: `run` is the checked zero-argument i32 entry and its engine
        // remains alive for this call; the native entry takes no pointers.
        assert_eq!(unsafe { run.call() }, 0);
        const {
            assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
    }

    #[test]
    fn exact_values_field_move_receipt_authorizes_only_checked_native_emission() {
        use crate::core::mir::{
            MIR_ROUTE_COVERAGE_ERROR_CODE, MIR_ROUTE_RECEIPT_ERROR_CODE,
            ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE, ORDINARY_MAP_OWNED_ANY_V2_ISLAND,
            ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE,
        };

        const {
            assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let source = include_str!(
            "../../../tests/fixtures/mir_ordinary_map_any_owner_v2_values_field_move.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked)
            .ordinary_map_owned_any_v2;
        assert_eq!(
            admission,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesFieldMove
        );
        let program = MirProgram::from_checked_program(&checked).expect("ValuesFieldMove MIR");
        super::checked_managed_any_unpack_coverage(&checked, &program)
            .expect("the direct Values conversion is joined to its field-move receipt");
        let receipt = program.route_receipt(ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE);
        let context = Context::create();

        let raw_error = CodeGenerator::new(&context, "values_field_move_raw_receipt")
            .compile_mir_native_with_route_receipt(&program, &receipt)
            .expect_err("raw MIR cannot authorize Checker-owned Values evidence");
        assert!(raw_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic
                    .message
                    .contains("requires CheckedProgram validation")
        }));

        let mut native = CodeGenerator::new(&context, "values_field_move_checked_route");
        native
            .compile_checked_mir_native_with_route_receipt(&checked, &program, &receipt)
            .expect("the exact CheckedProgram ValuesFieldMove receipt admits native emission");
        assert_eq!(
            native.mir_native_compiled_digest.as_deref(),
            Some(program.canonical_digest().as_str())
        );

        let mut missing = CodeGenerator::new(&context, "values_field_move_missing_receipt");
        let missing_receipt = program.route_receipt(ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        let missing_errors = missing
            .compile_checked_mir_native_with_route_receipt(&checked, &program, &missing_receipt)
            .expect_err("the broad V2 receipt cannot stand in for ValuesFieldMove");
        assert!(missing_errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));

        let mut mutated_receipt = receipt.clone();
        mutated_receipt.mir_digest = "0".repeat(64);
        let mutated_errors = CodeGenerator::new(&context, "values_field_move_mutated_receipt")
            .compile_checked_mir_native_with_route_receipt(&checked, &program, &mutated_receipt)
            .expect_err("a mutated ValuesFieldMove route receipt is rejected");
        assert!(mutated_errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_RECEIPT_ERROR_CODE)
        }));

        let checked_get_receipt =
            program.route_receipt(ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE);
        let cross_profile_errors = CodeGenerator::new(&context, "values_field_move_get_receipt")
            .compile_checked_mir_native_with_route_receipt(&checked, &program, &checked_get_receipt)
            .expect_err("CheckedGet cannot authorize a ValuesFieldMove graph");
        assert!(cross_profile_errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic
                    .message
                    .contains("CheckedGet native route rejected")
        }));

        let whole_drop_source = r#"
type Label { text: string }
func main() -> i32 {
    let label = Label { text: "whole drop" }
    let empty = map_new()
    let root = map_set(empty, "label", label)
    let snapshot = values(root)
    let item = snapshot[0] as Label
    drop(snapshot)
    drop(empty)
    drop(root)
    drop(label)
    drop(item)
    0
}
"#;
        let tokens = Lexer::new(whole_drop_source)
            .tokenize()
            .expect("lex whole-drop");
        let file = Parser::new(tokens).parse_file().expect("parse whole-drop");
        let whole_drop_checked = crate::core::check_program(&file).expect("check whole-drop");
        let whole_drop_program =
            MirProgram::from_checked_program(&whole_drop_checked).expect("whole-drop MIR");
        let whole_drop_receipt =
            whole_drop_program.route_receipt(ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE);
        let whole_drop_errors = CodeGenerator::new(&context, "values_field_move_whole_drop")
            .compile_checked_mir_native_with_route_receipt(
                &whole_drop_checked,
                &whole_drop_program,
                &whole_drop_receipt,
            )
            .expect_err("whole-record Drop cannot borrow the field-move route");
        assert!(whole_drop_errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic
                    .message
                    .contains("ValuesFieldMove native route rejected")
        }));

        let extra_field_source = r#"
type Label { text: string, number: i32 }
func main() -> i32 {
    let label = Label { text: "extra field", number: 7 }
    let empty = map_new()
    let root = map_set(empty, "label", label)
    let snapshot = values(root)
    let item = snapshot[0] as Label
    drop(snapshot)
    drop(empty)
    drop(root)
    drop(label)
    drop(item)
    0
}
"#;
        let tokens = Lexer::new(extra_field_source)
            .tokenize()
            .expect("lex extra-field");
        let file = Parser::new(tokens).parse_file().expect("parse extra-field");
        let extra_field_errors =
            crate::core::check_program(&file).expect_err("check extra-field must fail closed");
        assert!(
            extra_field_errors.iter().any(|diagnostic| {
                diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0200)
                    && diagnostic
                        .message
                        .contains("exact receipt-backed canonical Label")
            }),
            "multi-field nominal unpack must fail at the checked boundary: {extra_field_errors:?}"
        );
    }

    #[test]
    fn exact_values_snapshot_replacement_receipt_authorizes_only_its_checked_native_route() {
        use crate::core::mir::{
            CanonicalMirRouteProfile, MIR_ROUTE_COVERAGE_ERROR_CODE, MIR_ROUTE_RECEIPT_ERROR_CODE,
            ORDINARY_MAP_OWNED_ANY_V2_ISLAND, ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE,
        };

        const {
            assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let source = include_str!(
            "../../../tests/fixtures/mir_ordinary_map_any_owner_v2_values_snapshot_replacement.mimi"
        );
        let tokens = Lexer::new(source)
            .tokenize()
            .expect("lex snapshot replacement");
        let file = Parser::new(tokens)
            .parse_file()
            .expect("parse snapshot replacement");
        let checked = crate::core::check_program(&file).expect("check snapshot replacement");
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesSnapshotReplacement
        );

        let program = MirProgram::from_checked_program(&checked)
            .expect("snapshot replacement has canonical MIR");
        let profile = CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesSnapshotReplacement;
        assert_eq!(
            profile.as_str(),
            "map-any-owner-v2-values-snapshot-replacement-v1"
        );
        let receipt = program.route_receipt(profile.as_str());
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_snapshot_replacement_route(
            &checked, &program,
        )
        .expect("the checked replacement, snapshot and both indexed field moves agree");
        receipt
            .validate_against_program(&program)
            .expect("the exact replacement profile receipt binds this MIR graph");
        let generic_coverage_error = super::checked_managed_any_unpack_coverage(&checked, &program)
            .expect_err(
                "the generic CheckedGet/ValuesFieldMove gate cannot borrow snapshot authority",
            );
        assert!(generic_coverage_error.contains("exact Values field-move profile"));

        let context = Context::create();
        let raw_error = CodeGenerator::new(&context, "values_snapshot_raw_receipt")
            .compile_mir_native_with_route_receipt(&program, &receipt)
            .expect_err("raw MIR cannot mint snapshot-replacement Checker authority");
        assert!(raw_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic
                    .message
                    .contains("requires CheckedProgram validation")
        }));

        let generic_checked_error =
            CodeGenerator::new(&context, "values_snapshot_generic_checked_receipt")
                .compile_checked_mir_native_with_route_receipt(&checked, &program, &receipt)
                .expect_err("the generic checked receipt entry cannot mint this profile");
        assert!(generic_checked_error
            .iter()
            .any(|diagnostic| { diagnostic.code.as_deref() == Some("E0722") }));

        let mut native = CodeGenerator::new(&context, "values_snapshot_checked_route");
        native
            .compile_checked_mir_native_with_values_snapshot_replacement_route(
                &checked, &program, &receipt,
            )
            .expect("the exact CheckedProgram route authorizes native emission");
        assert_eq!(
            native.mir_native_compiled_digest.as_deref(),
            Some(program.canonical_digest().as_str())
        );

        let broad_receipt = program.route_receipt(ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        let broad_error = CodeGenerator::new(&context, "values_snapshot_broad_receipt")
            .compile_checked_mir_native_with_values_snapshot_replacement_route(
                &checked,
                &program,
                &broad_receipt,
            )
            .expect_err("the broad V2 receipt cannot authorize this narrow route");
        assert!(broad_error
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)));

        let field_move_receipt =
            program.route_receipt(ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE);
        let wrong_profile_error =
            CodeGenerator::new(&context, "values_snapshot_field_move_receipt")
                .compile_checked_mir_native_with_values_snapshot_replacement_route(
                    &checked,
                    &program,
                    &field_move_receipt,
                )
                .expect_err("ValuesFieldMove cannot authorize snapshot replacement");
        assert!(wrong_profile_error
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)));

        let mut mutated_receipt = receipt.clone();
        mutated_receipt.mir_digest = "0".repeat(64);
        let mutated_error = CodeGenerator::new(&context, "values_snapshot_mutated_receipt")
            .compile_checked_mir_native_with_values_snapshot_replacement_route(
                &checked,
                &program,
                &mutated_receipt,
            )
            .expect_err("a changed route digest cannot authorize native emission");
        assert!(mutated_error
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some(MIR_ROUTE_RECEIPT_ERROR_CODE)));

        let owner = crate::core::NodeId("function:main".into());
        let mut tampered_program = program.clone();
        let mut tampered_function = tampered_program
            .functions()
            .get(&owner)
            .expect("snapshot-replacement main MIR")
            .clone();
        let set = tampered_function
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2Set { .. }
                )
            })
            .expect("one receipted snapshot-replacement Set");
        set.id = crate::core::mir::MirInstructionId::new("inst:forged:snapshot-replacement-set")
            .expect("valid forged instruction identity");
        tampered_program.replace_function_for_test_only(tampered_function);
        let tampered_program_error = CodeGenerator::new(&context, "values_snapshot_mutated_mir")
            .compile_checked_mir_native_with_values_snapshot_replacement_route(
                &checked,
                &tampered_program,
                &receipt,
            )
            .expect_err("mutating a receipted MIR Set cannot retain its route authority");
        assert!(tampered_program_error
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some(MIR_ROUTE_RECEIPT_ERROR_CODE)));

        let unrelated_tokens = Lexer::new("func main() -> i32 { 0 }")
            .tokenize()
            .expect("lex unrelated source");
        let unrelated_file = Parser::new(unrelated_tokens)
            .parse_file()
            .expect("parse unrelated source");
        let unrelated_checked =
            crate::core::check_program(&unrelated_file).expect("check unrelated source");
        let unrelated_error = CodeGenerator::new(&context, "values_snapshot_wrong_checked_program")
            .compile_checked_mir_native_with_values_snapshot_replacement_route(
                &unrelated_checked,
                &program,
                &receipt,
            )
            .expect_err("unrelated CheckedProgram source cannot authorize this MIR");
        assert!(unrelated_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));
        const {
            assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
    }

    #[test]
    fn exact_values_local_index_i32_result_branch_receipt_authorizes_only_its_checked_native_route()
    {
        use crate::core::mir::{
            CanonicalMirRouteProfile, MIR_ROUTE_COVERAGE_ERROR_CODE, MIR_ROUTE_RECEIPT_ERROR_CODE,
            ORDINARY_MAP_OWNED_ANY_V2_ISLAND, ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE,
        };

        const {
            assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let source = include_str!("../../../tests/fixtures/mir_ordinary_map_any_owner_v2_values_local_index_i32_result_branch.mimi");
        let tokens = Lexer::new(source)
            .tokenize()
            .expect("lex local-index source");
        let file = Parser::new(tokens)
            .parse_file()
            .expect("parse local-index source");
        let checked = crate::core::check_program(&file).expect("check local-index source");
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesLocalIndexI32ResultBranch
        );

        let materialized = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("local-index Result-branch canonical route");
        let program = materialized.program;
        let profile =
            CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32ResultBranch;
        assert_eq!(
            profile.as_str(),
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_RESULT_BRANCH_PROFILE
        );
        let receipt = crate::core::mir::validate_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route(
            &checked, &program,
        )
        .expect("Checker and MIR receipts bind all three Result paths and the local i32 unpack");
        assert_eq!(receipt.profile, profile.as_str());
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_mir_route(
            &program,
        )
        .expect("MIR route binds New/Set Result branches, the local zero, and one dropped snapshot");
        receipt
            .validate_against_program(&program)
            .expect("route receipt binds this exact MIR graph");
        assert!(
            super::checked_managed_any_unpack_coverage(&checked, &program).is_err(),
            "generic Any coverage cannot borrow the local-index route authority"
        );

        let context = Context::create();
        let raw_error =
            CodeGenerator::new(&context, "values_local_index_result_branch_raw_receipt")
                .compile_mir_native_with_route_receipt(&program, &receipt)
                .expect_err("raw MIR cannot mint Result-branch Checker authority");
        assert!(raw_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic
                    .message
                    .contains("requires CheckedProgram validation")
        }));

        let generic_checked_error = CodeGenerator::new(
            &context,
            "values_local_index_result_branch_generic_checked_receipt",
        )
        .compile_checked_mir_native_with_route_receipt(&checked, &program, &receipt)
        .expect_err("generic checked receipt entry cannot mint the Result-branch profile");
        assert!(generic_checked_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));

        let mut native =
            CodeGenerator::new(&context, "values_local_index_result_branch_checked_route");
        native
            .compile_checked_mir_native_with_values_local_index_i32_result_branch_route(
                &checked, &program, &receipt,
            )
            .expect("exact CheckedProgram route authorizes native emission");
        native
            .module
            .verify()
            .expect("native Result-branch route LLVM module verifies");
        let execution = crate::tests::link_and_observe_canonical_mir(&native)
            .expect("run native Result-branch route executable");
        assert_eq!(execution.exit_code, Some(41), "{}", execution.stderr);
        assert!(execution.stdout.is_empty(), "{}", execution.stdout);
        assert!(execution.stderr.is_empty(), "{}", execution.stderr);

        let broad_receipt = program.route_receipt(ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        let broad_error =
            CodeGenerator::new(&context, "values_local_index_result_branch_broad_receipt")
                .compile_checked_mir_native_with_values_local_index_i32_result_branch_route(
                    &checked,
                    &program,
                    &broad_receipt,
                )
                .expect_err("the broad V2 receipt cannot authorize this narrow route");
        assert!(broad_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));

        let field_move_receipt =
            program.route_receipt(ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE);
        let wrong_profile_error = CodeGenerator::new(
            &context,
            "values_local_index_result_branch_field_move_receipt",
        )
        .compile_checked_mir_native_with_values_local_index_i32_result_branch_route(
            &checked,
            &program,
            &field_move_receipt,
        )
        .expect_err("the Label field-move profile cannot authorize local indexing");
        assert!(wrong_profile_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));

        let mut mutated_receipt = receipt.clone();
        mutated_receipt.mir_digest = "0".repeat(64);
        let mutated_error =
            CodeGenerator::new(&context, "values_local_index_result_branch_mutated_receipt")
                .compile_checked_mir_native_with_values_local_index_i32_result_branch_route(
                    &checked,
                    &program,
                    &mutated_receipt,
                )
                .expect_err("a changed route digest cannot authorize native emission");
        assert!(mutated_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_RECEIPT_ERROR_CODE)
        }));

        let owner = crate::core::NodeId("function:main".into());
        let mut tampered_program = program.clone();
        let mut tampered_function = tampered_program
            .functions()
            .get(&owner)
            .expect("local-index Result-branch main MIR")
            .clone();
        let set = tampered_function
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2SetAnyResult { .. }
                )
            })
            .expect("one receipted Result-branch local-index SetAnyResult");
        set.id =
            crate::core::mir::MirInstructionId::new("inst:forged:local-index-result-branch-set")
                .expect("valid forged instruction identity");
        tampered_program.replace_function_for_test_only(tampered_function);
        let tampered_error =
            CodeGenerator::new(&context, "values_local_index_result_branch_mutated_mir")
                .compile_checked_mir_native_with_values_local_index_i32_result_branch_route(
                    &checked,
                    &tampered_program,
                    &receipt,
                )
                .expect_err("mutating a receipted Map Set cannot retain route authority");
        assert!(tampered_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_RECEIPT_ERROR_CODE)
        }));

        let unrelated_tokens = Lexer::new("func main() -> i32 { 0 }")
            .tokenize()
            .expect("lex unrelated source");
        let unrelated_file = Parser::new(unrelated_tokens)
            .parse_file()
            .expect("parse unrelated source");
        let unrelated_checked =
            crate::core::check_program(&unrelated_file).expect("check unrelated source");
        let unrelated_error = CodeGenerator::new(
            &context,
            "values_local_index_result_branch_wrong_checked_program",
        )
        .compile_checked_mir_native_with_values_local_index_i32_result_branch_route(
            &unrelated_checked,
            &program,
            &receipt,
        )
        .expect_err("unrelated CheckedProgram cannot authorize this MIR");
        assert!(unrelated_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));
        const {
            assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
    }

    #[test]
    fn exact_keys_snapshot_receipt_authorizes_only_its_checked_native_route() {
        use crate::core::mir::{
            CanonicalMirRouteProfile, MIR_ROUTE_COVERAGE_ERROR_CODE, MIR_ROUTE_RECEIPT_ERROR_CODE,
            ORDINARY_MAP_OWNED_ANY_V2_ISLAND, ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
        };

        const {
            assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
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
        let tokens = Lexer::new(source)
            .tokenize()
            .expect("lex KeysSnapshot source");
        let file = Parser::new(tokens)
            .parse_file()
            .expect("parse KeysSnapshot source");
        let checked = crate::core::check_program(&file).expect("check KeysSnapshot source");
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::KeysSnapshot
        );
        let program =
            MirProgram::from_checked_program(&checked).expect("KeysSnapshot canonical MIR");
        let profile = CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2KeysSnapshot;
        assert_eq!(profile.as_str(), "map-any-owner-v2-keys-snapshot-v1");
        let receipt = crate::core::mir::validate_ordinary_map_owned_any_v2_keys_snapshot_route(
            &checked, &program,
        )
        .expect("Checker source and MIR witness bind the exact Keys snapshot and index zero");
        assert_eq!(receipt.profile, profile.as_str());
        receipt
            .validate_against_program(&program)
            .expect("exact route receipt binds the native MIR graph");
        crate::core::mir::validate_ordinary_map_owned_any_v2_keys_snapshot_mir_route(&program)
            .expect("MIR witness proves the copied key remains live after root/list drops");

        let context = Context::create();
        let raw_error = CodeGenerator::new(&context, "keys_snapshot_raw_receipt")
            .compile_mir_native_with_route_receipt(&program, &receipt)
            .expect_err("raw MIR receipt cannot mint Checker KeysSnapshot authority");
        assert!(raw_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic
                    .message
                    .contains("requires CheckedProgram validation")
        }));
        let generic_checked_error =
            CodeGenerator::new(&context, "keys_snapshot_generic_checked_receipt")
                .compile_checked_mir_native_with_route_receipt(&checked, &program, &receipt)
                .expect_err("generic checked route entry cannot mint KeysSnapshot authority");
        assert!(generic_checked_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));

        let mut native = CodeGenerator::new(&context, "keys_snapshot_checked_route");
        native
            .compile_checked_mir_native_with_keys_snapshot_route(&checked, &program, &receipt)
            .expect("exact Checker and MIR receipts authorize native Keys emission");
        native
            .module
            .verify()
            .expect("native KeysSnapshot LLVM verifies");
        let native_ir = native.module.print_to_string().to_string();
        assert!(native_ir.contains("@mimi_ordinary_map_owned_any_v2_keys_list"));
        assert!(native_ir.contains("@mimi_mir_ordinary_map_owned_any_v2_keys_unpack_copy_string"));
        let execution = crate::tests::link_and_observe_canonical_mir(&native)
            .expect("run native KeysSnapshot executable");
        assert_eq!(execution.exit_code, Some(0), "{}", execution.stderr);
        assert_eq!(execution.stdout, "answer\n");
        assert!(execution.stderr.is_empty(), "{}", execution.stderr);

        let broad_receipt = program.route_receipt(ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        let broad_error = CodeGenerator::new(&context, "keys_snapshot_broad_receipt")
            .compile_checked_mir_native_with_keys_snapshot_route(&checked, &program, &broad_receipt)
            .expect_err("the broad V2 receipt cannot authorize KeysSnapshot");
        assert!(broad_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));

        let values_receipt = program.route_receipt(
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE,
        );
        let non_keys_error = CodeGenerator::new(&context, "keys_snapshot_non_keys_receipt")
            .compile_checked_mir_native_with_keys_snapshot_route(
                &checked,
                &program,
                &values_receipt,
            )
            .expect_err("a non-Keys Values receipt cannot authorize Keys emission");
        assert!(non_keys_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));

        let mut mutated_receipt = receipt.clone();
        mutated_receipt.mir_digest = "0".repeat(64);
        let mutated_error = CodeGenerator::new(&context, "keys_snapshot_mutated_receipt")
            .compile_checked_mir_native_with_keys_snapshot_route(
                &checked,
                &program,
                &mutated_receipt,
            )
            .expect_err("a changed MIR digest cannot authorize KeysSnapshot");
        assert!(mutated_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_RECEIPT_ERROR_CODE)
        }));

        let unrelated_tokens = Lexer::new("func main() -> i32 { 0 }")
            .tokenize()
            .expect("lex unrelated source");
        let unrelated_file = Parser::new(unrelated_tokens)
            .parse_file()
            .expect("parse unrelated source");
        let unrelated_checked =
            crate::core::check_program(&unrelated_file).expect("check unrelated source");
        let unrelated_error = CodeGenerator::new(&context, "keys_snapshot_wrong_checked_program")
            .compile_checked_mir_native_with_keys_snapshot_route(
                &unrelated_checked,
                &program,
                &receipt,
            )
            .expect_err("unrelated CheckedProgram cannot authorize this MIR");
        assert!(unrelated_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));
        const {
            assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
    }

    #[test]
    fn exact_values_mixed_snapshot_receipt_authorizes_only_its_checked_native_route() {
        use crate::core::mir::{
            CanonicalMirRouteProfile, MIR_ROUTE_COVERAGE_ERROR_CODE, MIR_ROUTE_RECEIPT_ERROR_CODE,
            ORDINARY_MAP_OWNED_ANY_V2_ISLAND, ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE,
            ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE,
        };

        const {
            assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let source = include_str!(
            "../../../tests/fixtures/mir_ordinary_map_any_owner_v2_values_mixed_snapshot.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex mixed snapshot");
        let file = Parser::new(tokens)
            .parse_file()
            .expect("parse mixed snapshot");
        let checked = crate::core::check_program(&file).expect("check mixed snapshot");
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesMixedSnapshot
        );

        let program =
            MirProgram::from_checked_program(&checked).expect("mixed snapshot has canonical MIR");
        let profile = CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesMixedSnapshot;
        assert_eq!(
            profile.as_str(),
            "map-any-owner-v2-values-mixed-snapshot-v1"
        );
        let receipt = program.route_receipt(profile.as_str());
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_route(
            &checked, &program,
        )
        .expect("Checker and MIR receipts bind the exact sorted i32/String/Label snapshot");
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_mir_route(
            &program,
        )
        .expect("MIR-only route rederives the three indexed payloads and owner settlements");
        receipt
            .validate_against_program(&program)
            .expect("the mixed snapshot route receipt binds this exact MIR graph");
        assert!(
            super::checked_managed_any_unpack_coverage(&checked, &program).is_err(),
            "generic Values coverage cannot borrow the mixed-snapshot route authority"
        );

        let context = Context::create();
        let raw_error = CodeGenerator::new(&context, "values_mixed_snapshot_raw_receipt")
            .compile_mir_native_with_route_receipt(&program, &receipt)
            .expect_err("raw MIR cannot mint mixed-snapshot Checker authority");
        assert!(raw_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic
                    .message
                    .contains("requires CheckedProgram validation")
        }));

        let generic_checked_error =
            CodeGenerator::new(&context, "values_mixed_snapshot_generic_checked_receipt")
                .compile_checked_mir_native_with_route_receipt(&checked, &program, &receipt)
                .expect_err("the generic checked receipt entry cannot mint this profile");
        assert!(generic_checked_error
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("E0722")));

        let mut native = CodeGenerator::new(&context, "values_mixed_snapshot_checked_route");
        native
            .compile_checked_mir_native_with_values_mixed_snapshot_route(
                &checked, &program, &receipt,
            )
            .expect("the exact CheckedProgram route authorizes native emission");
        assert_eq!(
            native.mir_native_compiled_digest.as_deref(),
            Some(program.canonical_digest().as_str())
        );

        let broad_receipt = program.route_receipt(ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        let broad_error = CodeGenerator::new(&context, "values_mixed_snapshot_broad_receipt")
            .compile_checked_mir_native_with_values_mixed_snapshot_route(
                &checked,
                &program,
                &broad_receipt,
            )
            .expect_err("the broad V2 receipt cannot authorize this narrow route");
        assert!(broad_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));

        let snapshot_replacement_receipt =
            program.route_receipt(ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE);
        let wrong_profile_error =
            CodeGenerator::new(&context, "values_mixed_snapshot_replacement_receipt")
                .compile_checked_mir_native_with_values_mixed_snapshot_route(
                    &checked,
                    &program,
                    &snapshot_replacement_receipt,
                )
                .expect_err("snapshot replacement cannot authorize mixed snapshot");
        assert!(wrong_profile_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));

        let field_move_receipt =
            program.route_receipt(ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE);
        let field_move_error =
            CodeGenerator::new(&context, "values_mixed_snapshot_field_move_receipt")
                .compile_checked_mir_native_with_values_mixed_snapshot_route(
                    &checked,
                    &program,
                    &field_move_receipt,
                )
                .expect_err("the single-Label field-move profile cannot authorize mixed snapshot");
        assert!(field_move_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));

        let mut mutated_receipt = receipt.clone();
        mutated_receipt.mir_digest = "0".repeat(64);
        let mutated_error = CodeGenerator::new(&context, "values_mixed_snapshot_mutated_receipt")
            .compile_checked_mir_native_with_values_mixed_snapshot_route(
                &checked,
                &program,
                &mutated_receipt,
            )
            .expect_err("a changed route digest cannot authorize native emission");
        assert!(mutated_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_RECEIPT_ERROR_CODE)
        }));

        let owner = crate::core::NodeId("function:main".into());
        let mut tampered_program = program.clone();
        let mut tampered_function = tampered_program
            .functions()
            .get(&owner)
            .expect("mixed-snapshot main MIR")
            .clone();
        let set = tampered_function
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2Set { .. }
                )
            })
            .expect("one receipted mixed-snapshot Set");
        set.id = crate::core::mir::MirInstructionId::new("inst:forged:mixed-snapshot-set")
            .expect("valid forged instruction identity");
        tampered_program.replace_function_for_test_only(tampered_function);
        let tampered_error = CodeGenerator::new(&context, "values_mixed_snapshot_mutated_mir")
            .compile_checked_mir_native_with_values_mixed_snapshot_route(
                &checked,
                &tampered_program,
                &receipt,
            )
            .expect_err("mutating a receipted Set cannot retain route authority");
        assert!(tampered_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_RECEIPT_ERROR_CODE)
        }));

        let unrelated_tokens = Lexer::new("func main() -> i32 { 0 }")
            .tokenize()
            .expect("lex unrelated source");
        let unrelated_file = Parser::new(unrelated_tokens)
            .parse_file()
            .expect("parse unrelated source");
        let unrelated_checked =
            crate::core::check_program(&unrelated_file).expect("check unrelated source");
        let unrelated_error =
            CodeGenerator::new(&context, "values_mixed_snapshot_wrong_checked_program")
                .compile_checked_mir_native_with_values_mixed_snapshot_route(
                    &unrelated_checked,
                    &program,
                    &receipt,
                )
                .expect_err("unrelated CheckedProgram cannot authorize this MIR");
        assert!(unrelated_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));
        const {
            assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
    }

    #[test]
    fn exact_values_mixed_snapshot_remove_receipt_authorizes_only_its_checked_native_route() {
        use crate::core::mir::{
            CanonicalMirRouteProfile, MIR_ROUTE_COVERAGE_ERROR_CODE, MIR_ROUTE_RECEIPT_ERROR_CODE,
            ORDINARY_MAP_OWNED_ANY_V2_ISLAND, ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE,
        };

        const {
            assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let source = include_str!(
            "../../../tests/fixtures/mir_ordinary_map_any_owner_v2_values_snapshot_remove.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex snapshot remove");
        let file = Parser::new(tokens)
            .parse_file()
            .expect("parse snapshot remove");
        let checked = crate::core::check_program(&file).expect("check snapshot remove");
        assert_eq!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesMixedSnapshotRemove
        );

        let program =
            MirProgram::from_checked_program(&checked).expect("snapshot remove has canonical MIR");
        let profile = CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesMixedSnapshotRemove;
        assert_eq!(
            profile.as_str(),
            "map-any-owner-v2-values-mixed-snapshot-remove-v1"
        );
        let receipt = program.route_receipt(profile.as_str());
        crate::core::mir::validate_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_route(
            &checked, &program,
        )
        .expect("the dedicated route binds its nested snapshot and exact Remove receipts");
        super::ordinary_map_owned_any_v2::
            validate_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_native_contract(
                &program,
            )
            .expect("the nested snapshot has the common native Values/unpack contract");
        receipt
            .validate_against_program(&program)
            .expect("the exact snapshot-remove receipt binds this MIR graph");
        assert!(
            super::checked_managed_any_unpack_coverage(&checked, &program).is_err(),
            "generic Values coverage cannot borrow mixed-snapshot-remove authority"
        );

        let context = Context::create();
        let raw_error = CodeGenerator::new(&context, "values_snapshot_remove_raw_receipt")
            .compile_mir_native_with_route_receipt(&program, &receipt)
            .expect_err("raw MIR cannot mint snapshot-remove Checker authority");
        assert!(raw_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic
                    .message
                    .contains("requires CheckedProgram validation")
        }));

        let generic_checked_error =
            CodeGenerator::new(&context, "values_snapshot_remove_generic_checked_receipt")
                .compile_checked_mir_native_with_route_receipt(&checked, &program, &receipt)
                .expect_err("the generic checked-receipt entry cannot mint this profile");
        assert!(generic_checked_error
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("E0722")));

        let mut native = CodeGenerator::new(&context, "values_snapshot_remove_checked_route");
        native
            .compile_checked_mir_native_with_values_mixed_snapshot_remove_route(
                &checked, &program, &receipt,
            )
            .expect("the exact Checker/MIR profile authorizes native emission");
        let native_ir = native.module.print_to_string().to_string();
        for runtime in [
            "mimi_ordinary_map_owned_any_v2_remove",
            "mimi_ordinary_map_owned_any_v2_values_list",
            "mimi_mir_ordinary_map_owned_any_v2_values_unpack_copy_string",
        ] {
            assert!(native_ir.contains(runtime), "native IR omits {runtime}");
        }
        assert_eq!(
            native.mir_native_compiled_digest.as_deref(),
            Some(program.canonical_digest().as_str())
        );

        let broad_receipt = program.route_receipt(ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        let broad_error = CodeGenerator::new(&context, "values_snapshot_remove_broad_receipt")
            .compile_checked_mir_native_with_values_mixed_snapshot_remove_route(
                &checked,
                &program,
                &broad_receipt,
            )
            .expect_err("the broad V2 receipt cannot authorize this narrow route");
        assert!(broad_error
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)));

        let snapshot_receipt =
            program.route_receipt(ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE);
        let wrong_profile_error =
            CodeGenerator::new(&context, "values_snapshot_remove_snapshot_receipt")
                .compile_checked_mir_native_with_values_mixed_snapshot_remove_route(
                    &checked,
                    &program,
                    &snapshot_receipt,
                )
                .expect_err("mixed snapshot alone cannot authorize the additional Remove");
        assert!(wrong_profile_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));

        let mut mutated_receipt = receipt.clone();
        mutated_receipt.mir_digest = "0".repeat(64);
        let mutated_error = CodeGenerator::new(&context, "values_snapshot_remove_mutated_receipt")
            .compile_checked_mir_native_with_values_mixed_snapshot_remove_route(
                &checked,
                &program,
                &mutated_receipt,
            )
            .expect_err("a changed route digest cannot authorize native emission");
        assert!(mutated_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_RECEIPT_ERROR_CODE)
        }));

        let owner = crate::core::NodeId("function:main".into());
        let mut tampered_program = program.clone();
        let mut tampered_function = tampered_program
            .functions()
            .get(&owner)
            .expect("snapshot-remove main MIR")
            .clone();
        let remove = tampered_function
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::OrdinaryMapOwnedAnyV2Remove { .. }
                )
            })
            .expect("one receipted snapshot-remove Remove");
        remove.id = crate::core::mir::MirInstructionId::new("inst:forged:snapshot-remove")
            .expect("valid forged instruction identity");
        tampered_program.replace_function_for_test_only(tampered_function);
        let tampered_error = CodeGenerator::new(&context, "values_snapshot_remove_mutated_mir")
            .compile_checked_mir_native_with_values_mixed_snapshot_remove_route(
                &checked,
                &tampered_program,
                &receipt,
            )
            .expect_err("mutating the receipted Remove cannot retain route authority");
        assert!(tampered_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_RECEIPT_ERROR_CODE)
        }));

        let unrelated_tokens = Lexer::new("func main() -> i32 { 0 }")
            .tokenize()
            .expect("lex unrelated source");
        let unrelated_file = Parser::new(unrelated_tokens)
            .parse_file()
            .expect("parse unrelated source");
        let unrelated_checked =
            crate::core::check_program(&unrelated_file).expect("check unrelated source");
        let unrelated_error =
            CodeGenerator::new(&context, "values_snapshot_remove_wrong_checked_program")
                .compile_checked_mir_native_with_values_mixed_snapshot_remove_route(
                    &unrelated_checked,
                    &program,
                    &receipt,
                )
                .expect_err("unrelated CheckedProgram cannot authorize this MIR");
        assert!(unrelated_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));
        const {
            assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
    }

    #[test]
    fn checked_any_unpack_coverage_requires_an_exact_v2_receipt_for_label() {
        let source = include_str!(
            "../../../tests/fixtures/mir_ordinary_map_any_owner_v2_checked_get_hit_label.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");

        super::checked_managed_any_unpack_coverage(&checked, &program)
            .expect("the exact Checker-issued Label conversion and V2 unpack receipt match");
        assert_eq!(
            program
                .ordinary_map_owned_any_v2_unpack_result_receipts()
                .expect("derive exact V2 unpack receipts")
                .len(),
            1
        );

        let no_unpack_source = r#"
type Label { text: string }
func main() -> bool {
    let empty = map_new()
    let root = map_set(empty, "value", Label { text: "no unpack receipt" })
    drop(empty)
    drop(root)
    true
}
"#;
        let tokens = Lexer::new(no_unpack_source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked_no_unpack = crate::core::check_program(&file).expect("check no-unpack case");
        let without_mir_receipt =
            MirProgram::from_checked_program(&checked_no_unpack).expect("canonical no-unpack MIR");
        assert!(without_mir_receipt
            .ordinary_map_owned_any_v2_unpack_result_receipts()
            .expect("derive receipts from the no-unpack MIR")
            .is_empty());
        let error = super::checked_managed_any_unpack_coverage(&checked, &without_mir_receipt)
            .expect_err("a Checked Label conversion cannot be authorized without its MIR receipt");
        assert!(
            error.contains("complete canonical owner receipt coverage"),
            "missing V2 Label receipt must fail closed, got: {error}"
        );

        let other_record_source = r#"
type Other { text: string }
func main() -> bool {
    let empty = map_new()
    let root = map_set(empty, "value", Other { text: "not Label" })
    let pair = map_get(root, "value")
    drop(empty)
    drop(root)
    let payload = pair.1 as Other
    drop(pair)
    drop(payload)
    true
}
"#;
        let tokens = Lexer::new(other_record_source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked_other = crate::core::check_program(&file).expect("check non-Label record");
        let program_other =
            MirProgram::from_checked_program(&checked_other).expect("non-Label diagnostic MIR");
        let error = super::checked_managed_any_unpack_coverage(&checked_other, &program_other)
            .expect_err("same-layout nominal records do not acquire the CheckedGet Label profile");
        assert!(
            error.contains("exact V2 Label descriptor contracts"),
            "non-Label nominal target must fail closed, got: {error}"
        );
    }

    #[test]
    fn canonical_mir_native_census_events_have_stable_format_and_counts() {
        assert_eq!(
            super::canonical_mir_native_owner_event("function:main"),
            "info: canonical MIR native emitted owner='function:main'"
        );
        assert_eq!(
            super::canonical_mir_native_summary_event(2, 2, &"a".repeat(64)),
            format!(
                "info: canonical MIR native compiled 2/2 function(s), 0 fell back; mir_digest={}",
                "a".repeat(64)
            )
        );
    }

    #[test]
    fn canonical_mir_native_census_rejects_incomplete_duplicate_and_declared_rosters() {
        let expected = vec!["function:first".into(), "function:second".into()];
        let entry = |owner: &str, symbol: &str| super::CanonicalMirNativeCensusEntry {
            owner: owner.into(),
            llvm_symbol: symbol.into(),
            has_body: true,
            verified: true,
        };
        let complete = vec![
            entry("function:first", "first"),
            entry("function:second", "second"),
        ];
        assert!(super::validate_canonical_mir_native_census_roster(&expected, &complete).is_ok());

        let incomplete = vec![entry("function:first", "first")];
        assert!(
            super::validate_canonical_mir_native_census_roster(&expected, &incomplete)
                .unwrap_err()
                .contains("emitted entries")
        );

        let duplicate_owner = vec![
            entry("function:first", "first"),
            entry("function:first", "second"),
        ];
        assert!(
            super::validate_canonical_mir_native_census_roster(&expected, &duplicate_owner)
                .unwrap_err()
                .contains("duplicate emitted owner")
        );

        let duplicate_symbol = vec![
            entry("function:first", "same"),
            entry("function:second", "same"),
        ];
        assert!(
            super::validate_canonical_mir_native_census_roster(&expected, &duplicate_symbol)
                .unwrap_err()
                .contains("multiple owners")
        );

        let declared = vec![
            entry("function:first", "first"),
            super::CanonicalMirNativeCensusEntry {
                has_body: false,
                ..entry("function:second", "second")
            },
        ];
        assert!(
            super::validate_canonical_mir_native_census_roster(&expected, &declared)
                .unwrap_err()
                .contains("only an LLVM declaration")
        );

        let invalid = vec![
            entry("function:first", "first"),
            super::CanonicalMirNativeCensusEntry {
                verified: false,
                ..entry("function:second", "second")
            },
        ];
        assert!(
            super::validate_canonical_mir_native_census_roster(&expected, &invalid)
                .unwrap_err()
                .contains("failed LLVM function verification")
        );
    }

    const MAP_ANY_OWNER_V1_NATIVE_SOURCE: &str = r#"
func map_size_for(value: i32) -> i32 {
    ensures: result == 1
    let root = map_new()
    let updated = map_set(root, "answer", value)
    let size = map_size(updated)
    drop(updated)
    size
}
func checked_map_size(value: i32) -> i32 {
    ensures: result == 1
    map_size_for(value)
}
func wrong_map_size() -> i32 {
    ensures: result == 1
    let first = map_new()
    let alpha = map_set(first, "alpha", 1)
    let beta = map_set(alpha, "beta", 2)
    let size = map_size(beta)
    drop(beta)
    size
}
func main() -> i32 {
    ensures: result == 0
    let first = map_new()
    let inserted = map_set(first, "answer", 41)
    let overwritten = map_set(inserted, "answer", 42)
    let with_unicode = map_set(overwritten, "雪", 7)
    let size = map_size(with_unicode)
    println(size)
    drop(with_unicode)
    let checked = checked_map_size(73)
    println(checked)
    let temporary_size = map_size(map_new())
    println(temporary_size)
    let temporary_updated = map_set(map_new(), "temporary", 9)
    let temporary_updated_size = map_size(temporary_updated)
    println(temporary_updated_size)
    drop(temporary_updated)
    0
}
"#;

    const MAP_ANY_OWNER_V1_NATIVE_LIFECYCLE_SOURCE: &str = r#"
func main() -> i32 {
    let root = map_new()
    let updated = map_set(root, "reclaim-me", 17)
    drop(updated)
    let temporary = map_new()
    let temporary_size = map_size(temporary)
    println(temporary_size)
    drop(temporary)
    0
}
"#;

    const MAP_ANY_OWNER_V1_STRING_NATIVE_SOURCE: &str = r#"
func map_size_for_string(value: string) -> i32 {
    ensures: result == 1
    let root = map_new()
    let inserted = map_set(root, "payload", value)
    let size = map_size(inserted)
    drop(inserted)
    size
}
func wrong_string_map_size() -> i32 {
    ensures: result == 0
    let root = map_new()
    let inserted = map_set(root, "payload", "value")
    let size = map_size(inserted)
    drop(inserted)
    size
}
func map_remove_string_size(value: string) -> i32 {
    ensures: result == 1
    let root = map_new()
    let first = map_set(root, "remove", value)
    let second = map_set(first, "keep", "stay")
    let removed = map_remove(second, "remove")
    let size = map_size(removed)
    drop(removed)
    size
}
func wrong_map_remove_string_size() -> i32 {
    ensures: result == 0
    let root = map_new()
    let first = map_set(root, "remove", "gone")
    let second = map_set(first, "keep", "stay")
    let removed = map_remove(second, "remove")
    let size = map_size(removed)
    drop(removed)
    size
}
func main() -> i32 {
    let root = map_new()
    let inserted = map_set(root, "nul", "a\0b雪")
    let overwritten = map_set(inserted, "nul", "x\0y")
    let completed = map_set(overwritten, "second", "two")
    let removed = map_remove(completed, "nul")
    let size = map_size(removed)
    println(size)
    let missing = map_remove(removed, "absent")
    let missing_size = map_size(missing)
    println(missing_size)
    drop(missing)
    let helper_size = map_size_for_string("parameter\0payload")
    println(helper_size)
    0
}
"#;

    const MAP_ROOT_NATIVE_OVERFLOW_SOURCE: &str = r#"
extern "C" { func mimi_test_map_root_size_from_len(len: i64) -> i32; }
func main() -> i32 {
    let size = mimi_test_map_root_size_from_len(2147483648)
    println(size)
    0
}
"#;

    const MAP_ANY_OWNER_V1_PROOF_STRING_NATIVE_SOURCE: &str = r#"
func checked_string_owner_size(value: string) -> i32 {
    ensures: result == 1
    let root = map_new()
    let inserted = map_set(root, "payload", value)
    let size = map_size(inserted)
    drop(inserted)
    size
}
func false_string_owner_size() -> i32 {
    ensures: result == 0
    let root = map_new()
    let inserted = map_set(root, "payload", "owned")
    let size = map_size(inserted)
    drop(inserted)
    size
}
func main() -> i32 {
    let root = map_new()
    let inserted = map_set(root, "nul", "a\0b雪")
    let overwritten = map_set(inserted, "nul", "x\0y")
    let present = has_key(overwritten, "nul")
    let removed = map_remove(overwritten, "nul")
    let absent = has_key(removed, "nul")
    let size = map_size(removed)
    println(present)
    println(absent)
    println(size)
    drop(removed)
    0
}
"#;

    #[test]
    fn ordinary_map_owned_any_v2_borrows_locals_and_settles_computed_string_keys() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_ordinary_map_any_owner_v2_dynamic_string_keys.mimi"
        ));
        let main = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&main, &[])
            .expect("reference V2 dynamic String keys");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(reference.output, "true\ntrue\nfalse\ntrue\n");

        let mut vm = BytecodeVM::new(
            compile_mir_program(&program).expect("AST-free V2 dynamic String key bytecode"),
        );
        assert!(matches!(
            vm.run_value_for_component()
                .expect("bytecode V2 dynamic String keys"),
            Value::Int(0)
        ));
        assert_eq!(vm.take_stdout(), reference.output);

        let receipts = program
            .ordinary_map_owned_any_v2_get_any_result_receipts()
            .expect("Core derives exact V2 GetAny result receipt");
        let [get_receipt] = receipts.as_slice() else {
            panic!("expected one GetAny receipt, got {receipts:#?}");
        };
        assert!(get_receipt.checker_key_local.is_none());
        let computed_key =
            crate::core::mir::MirValueId::new(format!("expr:{}", get_receipt.key_node.0))
                .expect("MIR computed key identity");
        assert_eq!(get_receipt.key, computed_key);

        let main_function = program.functions().get(&main).expect("main MIR");
        let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        assert!(
            main_function
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
                .any(|instruction| matches!(
                    &instruction.kind,
                    MirInstructionKind::Const {
                        literal: ResolvedLiteral::String(value),
                        ..
                    } if value.as_bytes() == "nul\0雪".as_bytes()
                )),
            "fixture key seed contains both an embedded NUL and non-ASCII UTF-8"
        );
        let key_operations = main_function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter_map(|instruction| match &instruction.kind {
                MirInstructionKind::OrdinaryMapOwnedAnyV2Set { key, .. }
                | MirInstructionKind::OrdinaryMapOwnedAnyV2Remove { key, .. }
                | MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { key, .. }
                | MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { key, .. } => {
                    Some((instruction, key))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(key_operations.len(), 6);
        assert_eq!(
            key_operations
                .iter()
                .filter(|(_, key)| key.as_str().starts_with("local:"))
                .count(),
            2,
            "existing String values should remain borrowed"
        );
        let temporary_keys = key_operations
            .iter()
            .filter(|(_, key)| key.as_str().starts_with("expr:"))
            .collect::<Vec<_>>();
        assert_eq!(temporary_keys.len(), 4);
        for (block, instruction) in main_function.blocks.values().flat_map(|block| {
            block
                .instructions
                .iter()
                .map(move |instruction| (block, instruction))
        }) {
            if let MirInstructionKind::OrdinaryMapOwnedAnyV2Set { key, .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Remove { key, .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { key, .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { key, .. } = &instruction.kind
            {
                if key.as_str().starts_with("expr:") {
                    let operation_index = block
                        .instructions
                        .iter()
                        .position(|candidate| candidate.id == instruction.id)
                        .expect("key operation position");
                    assert!(matches!(
                        block.instructions.get(operation_index + 1).map(|next| &next.kind),
                        Some(MirInstructionKind::Drop { value }) if value == key
                    ));
                }
            }
            if let MirInstructionKind::Binary {
                result,
                op: ResolvedBinaryOp::Add,
                left,
                right,
            } = &instruction.kind
            {
                if main_function
                    .values
                    .get(result)
                    .is_some_and(|value| value.ty == string_type)
                {
                    let index = block
                        .instructions
                        .iter()
                        .position(|candidate| candidate.id == instruction.id)
                        .expect("String Add operation position");
                    assert!(matches!(
                        block.instructions.get(index + 1).map(|next| &next.kind),
                        Some(MirInstructionKind::Drop { value }) if value == left
                    ));
                    assert!(matches!(
                        block.instructions.get(index + 2).map(|next| &next.kind),
                        Some(MirInstructionKind::Drop { value }) if value == right
                    ));
                }
            }
        }

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_map_any_v2_dynamic_keys");
        generator
            .compile_mir_native_for_component(&program)
            .expect("native V2 dynamic String key lowering");
        generator.module.verify().expect("valid native LLVM module");
        let native_ir = generator.module.print_to_string().to_string();
        for runtime in [
            "mimi_ordinary_map_owned_any_v2_set",
            "mimi_ordinary_map_owned_any_v2_remove",
            "mimi_ordinary_map_owned_any_v2_contains",
            "mimi_ordinary_map_owned_any_v2_get",
            "mimi_str_concat_ll",
            "mimi_mir_ordinary_map_domain_own_string",
        ] {
            assert!(native_ir.contains(runtime), "native IR omits {runtime}");
        }
        let has_temporary_key_trap_cleanup = {
            let mut in_v2_invalid_block = false;
            let mut invalid_block_frees_key = false;
            let mut found_cleanup = false;
            for line in native_ir.lines() {
                let trimmed = line.trim();
                if line == trimmed && trimmed.contains(':') {
                    if in_v2_invalid_block && invalid_block_frees_key {
                        found_cleanup = true;
                        break;
                    }
                    let label = trimmed.split(':').next().unwrap_or_default();
                    in_v2_invalid_block = label.starts_with("mir_map_any_v2_invalid");
                    invalid_block_frees_key = false;
                } else if in_v2_invalid_block && trimmed.starts_with("call void @mimi_string_free(")
                {
                    // LLVM does not print an SSA name for void calls, so
                    // verify the free in the invalid edge block itself.
                    invalid_block_frees_key = true;
                }
            }
            found_cleanup || (in_v2_invalid_block && invalid_block_frees_key)
        };
        assert!(
            has_temporary_key_trap_cleanup,
            "temporary key is freed on a native Map/Any invalid edge"
        );
        assert!(
            native_ir.contains("call i32 @mimi_mir_ordinary_map_domain_own_string"),
            "native String concatenation registers its result for Map trap cleanup"
        );
    }

    fn ordinary_map_owned_any_v2_values_unpack_native_source(unpack_body: &str) -> String {
        format!(
            r#"
            type Label {{ text: string }}
            func main() -> i32 {{
                let label = Label {{ text: "Values Label payload" }}
                let text = "Values String payload"
                let empty = map_new()
                let numbers = map_set(empty, "number", 42)
                let labels = map_set(numbers, "label", label)
                let mixed = map_set(labels, "string", text)
                let snapshot = values(mixed)
                {unpack_body}
                drop(snapshot)
                drop(empty)
                drop(numbers)
                drop(labels)
                drop(mixed)
                drop(label)
                drop(text)
                0
            }}
            "#
        )
    }

    fn strict_map_any_memcheck_config() -> crate::tests::E2EConfig {
        crate::tests::E2EConfig {
            use_valgrind: true,
            valgrind_args: vec![
                "--tool=memcheck".into(),
                "--leak-check=full".into(),
                "--show-leak-kinds=all".into(),
                "--errors-for-leak-kinds=definite,indirect,possible".into(),
                "--error-exitcode=97".into(),
            ],
            retain_tmp_if_requested: std::env::var_os(
                "MIMI_MAP_V2_VALUES_STATIC_OOB_L3_ARTIFACT_DIR",
            )
            .is_some(),
            ..crate::tests::E2EConfig::default()
        }
    }

    fn memcheck_pid_payloads(stderr: &str) -> Result<Vec<String>, String> {
        let mut pid: Option<&str> = None;
        let mut payloads = Vec::new();
        for line in stderr.lines() {
            if let Some(prefixed) = line.strip_prefix("==") {
                let Some((line_pid, payload)) = prefixed.split_once("==") else {
                    return Err(format!("malformed Valgrind PID prefix: {line}"));
                };
                if line_pid.is_empty() || !line_pid.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(format!("invalid Valgrind PID prefix: {line}"));
                }
                if pid.is_some_and(|expected| expected != line_pid) {
                    return Err("Valgrind report contains more than one PID".into());
                }
                pid = Some(line_pid);
                payloads.push(payload.trim().to_owned());
            } else if [
                "Memcheck, a memory error detector",
                "Command:",
                "HEAP SUMMARY:",
                "LEAK SUMMARY:",
                "ERROR SUMMARY:",
                "in use at exit:",
                "total heap usage:",
                "All heap blocks were freed",
                "definitely lost:",
                "indirectly lost:",
                "possibly lost:",
                "still reachable:",
                "suppressed:",
            ]
            .iter()
            .any(|marker| line.contains(marker))
            {
                return Err(format!(
                    "Valgrind summary line lacks its PID prefix: {line}"
                ));
            }
        }
        if pid.is_none() {
            return Err("Valgrind report has no PID-prefixed lines".into());
        }
        Ok(payloads)
    }

    fn summary_line<'a>(payloads: &'a [String], prefix: &str) -> Result<&'a str, String> {
        let mut matches = payloads
            .iter()
            .filter(|line| line.starts_with(prefix))
            .map(String::as_str);
        let first = matches
            .next()
            .ok_or_else(|| format!("missing Valgrind summary line {prefix:?}"))?;
        if matches.next().is_some() {
            return Err(format!("duplicate Valgrind summary line {prefix:?}"));
        }
        Ok(first)
    }

    fn count_summary_lines(payloads: &[String], exact: &str) -> usize {
        payloads
            .iter()
            .filter(|line| line.as_str() == exact)
            .count()
    }

    fn parse_memcheck_bytes_blocks(line: &str, prefix: &str) -> Result<(u64, u64), String> {
        let body = line
            .strip_prefix(prefix)
            .ok_or_else(|| format!("unexpected Valgrind leak row: {line}"))?;
        let (bytes, blocks) = body
            .split_once(" bytes in ")
            .ok_or_else(|| format!("malformed Valgrind leak row: {line}"))?;
        let blocks = blocks
            .strip_suffix(" blocks")
            .ok_or_else(|| format!("malformed Valgrind block count: {line}"))?;
        let parse_count = |value: &str| {
            value
                .trim()
                .replace(',', "")
                .parse::<u64>()
                .map_err(|_| format!("malformed Valgrind count in row: {line}"))
        };
        Ok((parse_count(bytes)?, parse_count(blocks)?))
    }

    fn parse_memcheck_total_heap_usage(line: &str) -> Result<(u64, u64), String> {
        let body = line
            .strip_prefix("total heap usage: ")
            .ok_or_else(|| format!("malformed total heap usage row: {line}"))?;
        let (allocs, body) = body
            .split_once(" allocs, ")
            .ok_or_else(|| format!("malformed allocation count: {line}"))?;
        let (frees, bytes) = body
            .split_once(" frees, ")
            .ok_or_else(|| format!("malformed free count: {line}"))?;
        let bytes = bytes
            .strip_suffix(" bytes allocated")
            .ok_or_else(|| format!("malformed allocated byte count: {line}"))?;
        let parse_count = |value: &str| {
            value
                .trim()
                .replace(',', "")
                .parse::<u64>()
                .map_err(|_| format!("malformed total heap count: {line}"))
        };
        let allocs = parse_count(allocs)?;
        let frees = parse_count(frees)?;
        let _bytes_allocated = parse_count(bytes)?;
        if frees > allocs {
            return Err(format!(
                "Valgrind reports more frees than allocations: {line}"
            ));
        }
        Ok((allocs, frees))
    }

    fn validate_clean_map_any_memcheck_report(stderr: &str) -> Result<(), String> {
        let payloads = memcheck_pid_payloads(stderr)?;
        if count_summary_lines(&payloads, "Memcheck, a memory error detector") != 1 {
            return Err("expected exactly one complete Memcheck banner".into());
        }
        if count_summary_lines(&payloads, "HEAP SUMMARY:") != 1 {
            return Err("expected exactly one HEAP SUMMARY".into());
        }
        if summary_line(&payloads, "Command: ").is_err() {
            return Err("expected exactly one Valgrind Command line".into());
        }
        let total_heap = summary_line(&payloads, "total heap usage: ")?;
        let (allocs, frees) = parse_memcheck_total_heap_usage(total_heap)?;
        let in_use = summary_line(&payloads, "in use at exit: ")?;
        let (in_use_bytes, in_use_blocks) =
            parse_memcheck_bytes_blocks(in_use, "in use at exit: ")?;
        let all_free_marker = "All heap blocks were freed -- no leaks are possible";
        let all_free_count = count_summary_lines(&payloads, all_free_marker);
        let leak_header_count = count_summary_lines(&payloads, "LEAK SUMMARY:");
        let categories = [
            ("definitely lost:", true),
            ("indirectly lost:", true),
            ("possibly lost:", true),
            ("still reachable:", false),
            ("suppressed:", true),
        ];
        let mut category_rows = Vec::new();
        for (prefix, must_be_zero) in categories {
            let matching = payloads
                .iter()
                .filter(|line| line.starts_with(prefix))
                .collect::<Vec<_>>();
            if all_free_count == 1 && leak_header_count == 0 {
                if !matching.is_empty() {
                    return Err(format!(
                        "all-free report unexpectedly contains {prefix} rows"
                    ));
                }
                continue;
            }
            if matching.len() != 1 {
                return Err(format!(
                    "expected one {prefix} row in reachable-form report; observed {}",
                    matching.len()
                ));
            }
            let (bytes, blocks) = parse_memcheck_bytes_blocks(matching[0], prefix)?;
            if must_be_zero && (bytes != 0 || blocks != 0) {
                return Err(format!("nonzero Memcheck leak category: {}", matching[0]));
            }
            if !must_be_zero {
                category_rows.push((bytes, blocks));
            }
        }
        let total_leak_rows = payloads
            .iter()
            .filter(|line| {
                categories
                    .iter()
                    .any(|(prefix, _)| line.starts_with(prefix))
            })
            .count();
        if all_free_count == 1 {
            if leak_header_count != 0 || in_use_bytes != 0 || in_use_blocks != 0 || allocs != frees
            {
                return Err("all-free report has inconsistent leak/header/heap counters".into());
            }
        } else {
            if all_free_count != 0 || leak_header_count != 1 || total_leak_rows != 5 {
                return Err("expected one complete reachable-form LEAK SUMMARY".into());
            }
            if category_rows.as_slice() != [(in_use_bytes, in_use_blocks)] {
                return Err("heap-at-exit bytes/blocks differ from still-reachable summary".into());
            }
        }
        if count_summary_lines(
            &payloads,
            "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
        ) != 1
            || payloads
                .iter()
                .filter(|line| line.starts_with("ERROR SUMMARY:"))
                .count()
                != 1
        {
            return Err("expected one exact zero-error/zero-suppression summary".into());
        }
        Ok(())
    }

    fn assert_map_any_memcheck_parser_regressions() {
        let all_free = concat!(
            "==41== Memcheck, a memory error detector\n",
            "==41== Command: /tmp/map-any-test\n",
            "==41==\n",
            "==41== HEAP SUMMARY:\n",
            "==41==     in use at exit: 0 bytes in 0 blocks\n",
            "==41==   total heap usage: 73 allocs, 73 frees, 9,174 bytes allocated\n",
            "==41==\n",
            "==41== All heap blocks were freed -- no leaks are possible\n",
            "==41==\n",
            "==41== For lists of detected and suppressed errors, rerun with: -s\n",
            "==41== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)\n",
        );
        assert!(
            validate_clean_map_any_memcheck_report(all_free).is_ok(),
            "Valgrind's genuine three-row all-free form is accepted"
        );

        let reachable = concat!(
            "==42== Memcheck, a memory error detector\n",
            "==42== Command: /tmp/map-any-test\n",
            "==42==\n",
            "==42== HEAP SUMMARY:\n",
            "==42==     in use at exit: 192 bytes in 1 blocks\n",
            "==42==   total heap usage: 73 allocs, 72 frees, 9,174 bytes allocated\n",
            "==42==\n",
            "==42== LEAK SUMMARY:\n",
            "==42==    definitely lost: 0 bytes in 0 blocks\n",
            "==42==    indirectly lost: 0 bytes in 0 blocks\n",
            "==42==      possibly lost: 0 bytes in 0 blocks\n",
            "==42==    still reachable: 192 bytes in 1 blocks\n",
            "==42==         suppressed: 0 bytes in 0 blocks\n",
            "==42==\n",
            "==42== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)\n",
        );
        let reachable_result = validate_clean_map_any_memcheck_report(reachable);
        assert!(
            reachable_result.is_ok(),
            "the complete reachable-form summary is accepted: {reachable_result:?}"
        );

        let duplicate_loss = reachable.replace(
            "==42==    definitely lost: 0 bytes in 0 blocks\n",
            "==42==    definitely lost: 0 bytes in 0 blocks\n==42==    definitely lost: 1 bytes in 1 blocks\n",
        );
        assert!(
            validate_clean_map_any_memcheck_report(&duplicate_loss).is_err(),
            "a duplicate nonzero leak row cannot hide beside the zero row"
        );
        let truncated = reachable.replace("==42== Command: /tmp/map-any-test\n", "");
        assert!(
            validate_clean_map_any_memcheck_report(&truncated).is_err(),
            "a summary-only truncated report is rejected"
        );
        let mixed_pid = reachable.replace("==42== ERROR SUMMARY:", "==43== ERROR SUMMARY:");
        assert!(
            validate_clean_map_any_memcheck_report(&mixed_pid).is_err(),
            "summary lines from another process cannot be mixed in"
        );
        let invalid_counts = all_free.replace("73 allocs, 73 frees", "73 allocs, 74 frees");
        assert!(
            validate_clean_map_any_memcheck_report(&invalid_counts).is_err(),
            "all-free reports cannot claim more frees than allocations"
        );
    }

    fn assert_clean_map_any_memcheck(
        observed: &crate::tests::NativeRunObservation,
        expected_exit: i32,
        expected_stdout: &str,
        expected_trap: Option<&str>,
    ) {
        assert_eq!(
            observed.exit_code,
            Some(expected_exit),
            "{}",
            observed.stderr
        );
        assert_eq!(observed.stdout, expected_stdout);
        if let Some(code) = expected_trap {
            assert!(observed.stderr.contains(code), "{}", observed.stderr);
        }
        if let Err(reason) = validate_clean_map_any_memcheck_report(&observed.stderr) {
            panic!(
                "strict Map/Any Memcheck report rejected: {reason}: {}",
                observed.stderr
            );
        }
    }

    #[test]
    fn ordinary_map_owned_any_v2_raw_clone_mir_is_rejected_by_verifier_but_executes_on_consumers() {
        assert_map_any_memcheck_parser_regressions();
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_clone.mimi"
        ));
        crate::verifier::validate_mir_capabilities(&program)
            .expect("Clone and payload-selection receipts pass the whole-program capability gate");
        crate::codegen::mir::validate_mir_native(&program)
            .expect("native admission revalidates the same Clone and GetAny receipts");

        let verifier_error = crate::verifier::verify_mir(&program, String::new())
            .expect_err("a broad MIR receipt cannot authorize Map/Any CloneIsolation");
        assert!(
            verifier_error
                .contains("CloneIsolation requires the exact Checker-aware verifier route"),
            "the raw verifier must reject an unbound clone route: {verifier_error}"
        );

        let main = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&main, &[])
            .expect("Reference executes the same cloned-root MIR");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(reference.output, "true\ntrue\n");

        let mut vm = BytecodeVM::new(
            compile_mir_program(&program).expect("AST-free bytecode compiles the same Clone MIR"),
        );
        assert!(matches!(
            vm.run_value_for_component()
                .expect("bytecode executes the same Clone MIR"),
            Value::Int(0)
        ));
        assert_eq!(vm.take_stdout(), reference.output);

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_map_any_v2_clone");
        generator
            .compile_mir_native_for_component(&program)
            .expect("native compiler consumes the exact Clone MIR");
        generator
            .module
            .verify()
            .expect("native Clone module verifies");
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("native executes the same cloned-root lifecycle");
        assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
        assert_eq!(native.stdout, reference.output);
        assert_eq!(native.stderr, "");

        if std::process::Command::new("valgrind")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            let memcheck = crate::tests::link_and_observe_canonical_mir_with_config(
                &generator,
                &strict_map_any_memcheck_config(),
            )
            .expect("native cloned-root lifecycle runs under strict Memcheck");
            assert_clean_map_any_memcheck(&memcheck, 0, &reference.output, None);
        }
    }

    #[test]
    fn ordinary_map_owned_any_v2_values_len_deep_clone_runs_across_all_consumers() {
        let program = canonical_program(&ordinary_map_owned_any_v2_values_unpack_native_source(
            "let snapshot_count = snapshot.len()\nprintln(snapshot_count)",
        ));
        crate::verifier::validate_mir_capabilities(&program)
            .expect("Values snapshot and borrowed Clone receipts pass capability validation");
        crate::codegen::mir::validate_mir_native(&program)
            .expect("native admission revalidates the Values Clone and settlement receipts");

        let values_receipt = program
            .ordinary_map_owned_any_v2_values_result_receipts()
            .expect("canonical Values snapshot receipt")
            .into_iter()
            .next()
            .expect("one Values snapshot");
        assert!(values_receipt.validate_against_program(&program).is_ok());
        let values_type = crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id();
        let values_clones = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::Clone { source, .. }
                        if source.as_str() == values_receipt.binding.as_str()
                )
            })
            .count();
        assert_eq!(values_type, values_receipt.mir_result_type);
        assert_eq!(
            values_clones, 1,
            "Values.len borrows through one owned snapshot Clone"
        );

        let main = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&main, &[])
            .expect("Reference executes the same Values snapshot Clone");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(reference.output, "3\n");

        let mut vm = BytecodeVM::new(
            compile_mir_program(&program)
                .expect("AST-free bytecode compiles the Values snapshot Clone"),
        );
        assert!(matches!(
            vm.run_value_for_component()
                .expect("bytecode executes the same Values snapshot Clone"),
            Value::Int(0)
        ));
        assert_eq!(vm.take_stdout(), reference.output);

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_map_any_v2_values_len_clone");
        generator
            .compile_mir_native_for_component(&program)
            .expect("native compiler consumes the same Values snapshot Clone");
        generator
            .module
            .verify()
            .expect("native Values snapshot Clone module verifies");
        let native_ir = generator.module.print_to_string().to_string();
        assert!(
            native_ir.contains("@mimi_mir_list_clone_managed_any_v2("),
            "Values borrow Clone uses the V2 deep-copy runtime path"
        );
        assert!(
            native_ir.contains("[E0800] ordinary Map/Any owner-v2 Values List Clone failed"),
            "native null-clone abort retains the verifier's capacity/trap category"
        );
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("native executes and settles the same Values snapshot Clone");
        assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
        assert_eq!(native.stdout, reference.output);
        assert_eq!(native.stderr, "");

        if std::process::Command::new("valgrind")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            let memcheck = crate::tests::link_and_observe_canonical_mir_with_config(
                &generator,
                &strict_map_any_memcheck_config(),
            )
            .expect("native Values Clone lifecycle runs under strict Memcheck");
            assert_clean_map_any_memcheck(&memcheck, 0, &reference.output, None);
        }
    }

    #[test]
    fn values_producer_caller_checked_route_emits_native_pointer_abi() {
        use crate::core::mir::{
            CanonicalMirRouteProfile, MIR_ROUTE_COVERAGE_ERROR_CODE,
            ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
        };

        const {
            assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let source = include_str!(
            "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_values_producer_caller.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex Values bridge");
        let file = Parser::new(tokens)
            .parse_file()
            .expect("parse Values bridge");
        let checked = crate::core::check_program(&file).expect("check Values bridge");
        let program = MirProgram::from_checked_program(&checked)
            .expect("Values producer/caller has canonical MIR");
        let receipt =
            crate::core::mir::validate_ordinary_map_owned_any_v2_values_producer_caller_route(
                &checked, &program,
            )
            .expect("exact Checker/MIR producer-caller route");
        let bridge_receipts = program
            .ordinary_map_owned_any_v2_values_producer_caller_receipts()
            .expect("derive exact MIR producer-caller bridge receipt");
        let [bridge_receipt] = bridge_receipts.as_slice() else {
            panic!("expected one exact MIR producer-caller bridge receipt");
        };
        assert_eq!(
            bridge_receipt.producer_checker_snapshot_type,
            bridge_receipt.checker_producer.snapshot_type
        );
        assert_eq!(
            bridge_receipt.producer_return_source_type,
            bridge_receipt.producer_checker_snapshot_type
        );
        assert_eq!(
            bridge_receipt.producer_return_target_type,
            bridge_receipt.producer_surface_return_type
        );
        assert_eq!(
            bridge_receipt.call_surface_result_type,
            bridge_receipt.checker_caller.result_type
        );
        assert_eq!(
            bridge_receipt.producer_surface_return_type,
            bridge_receipt.call_surface_result_type
        );
        assert_ne!(
            bridge_receipt.checker_producer.snapshot_type, bridge_receipt.producer_result_type,
            "Checker List<DynamicAny> source and dedicated MIR Values TypeDesc are separate identities"
        );
        assert_ne!(
            bridge_receipt.checker_caller.result_type, bridge_receipt.call_result_type,
            "Checker surface List result and dedicated MIR Values call result are separate identities"
        );
        assert_ne!(
            bridge_receipt.checker_caller.owner_type, bridge_receipt.owner_type,
            "Checker owner obligation and dedicated MIR Values Drop TypeDesc are separate identities"
        );
        assert_eq!(
            receipt.profile,
            CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesProducerCaller.as_str()
        );
        crate::codegen::mir::validate_checked_mir_native_values_producer_caller_route(
            &checked, &program, &receipt,
        )
        .expect("native preflight revalidates exact Checker and MIR evidence");

        let context = Context::create();
        let raw_error = CodeGenerator::new(&context, "values_bridge_raw_route")
            .compile_mir_native_with_route_receipt(&program, &receipt)
            .expect_err("raw MIR receipt cannot authorize the producer return");
        assert!(raw_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic
                    .message
                    .contains("dedicated ValuesProducerCaller native route")
        }));

        let mut native = CodeGenerator::new(&context, "values_bridge_checked_route");
        native
            .compile_checked_mir_native_with_values_producer_caller_route(
                &checked, &program, &receipt,
            )
            .expect("only the dedicated Checker/MIR route authorizes native emission");
        native
            .module
            .verify()
            .expect("producer/caller native module verifies");
        let native_ir = native.module.print_to_string().to_string();
        assert!(native_ir.contains("mimi_mir_list_len_managed_any_v2"));
        assert!(native_ir.contains("mir_map_any_v2_values_list_out"));
        assert_eq!(
            native.mir_native_compiled_digest.as_deref(),
            Some(program.canonical_digest().as_str())
        );
    }

    #[test]
    fn values_crosscall_string_caller_index1_native_entry_is_exact_and_versioned() {
        use crate::core::mir::{
            CanonicalMirRouteProfile, MIR_ROUTE_COVERAGE_ERROR_CODE, MIR_ROUTE_RECEIPT_ERROR_CODE,
        };

        let checked_mir_from = |source: &str| {
            let tokens = Lexer::new(source).tokenize().expect("lex Values caller");
            let file = Parser::new(tokens)
                .parse_file()
                .expect("parse Values caller");
            let checked = crate::core::check_program(&file).expect("check Values caller");
            let program = MirProgram::from_checked_program(&checked)
                .expect("lower Values caller to canonical MIR");
            (checked, program)
        };

        let (checked, program) = checked_mir_from(include_str!(
            "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1.mimi"
        ));
        let receipt =
            crate::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route(
                &checked, &program,
            )
            .expect("exact Checker/MIR V2 index-one receipt");
        assert_eq!(
            receipt.profile,
            CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1
                .as_str()
        );
        crate::codegen::mir::validate_checked_mir_native_values_crosscall_string_caller_index1_route(
            &checked, &program, &receipt,
        )
        .expect("native V2 preflight revalidates exact Checker/MIR evidence");

        let context = Context::create();
        let generic_error = CodeGenerator::new(&context, "values_crosscall_index1_generic")
            .compile_checked_mir_native_with_route_receipt(&checked, &program, &receipt)
            .expect_err("the generic CheckedProgram route cannot authorize Index1");
        assert!(generic_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic
                    .message
                    .contains("ValuesCrosscallStringCallerIndex1 requires its dedicated")
        }));

        let mut native = CodeGenerator::new(&context, "values_crosscall_index1_checked");
        native
            .compile_checked_mir_native_with_values_crosscall_string_caller_index1_route(
                &checked, &program, &receipt,
            )
            .expect("exact V2 Checker/MIR route authorizes native emission");
        native
            .module
            .verify()
            .expect("cross-call Values index-one native module verifies");
        let native_ir = native.module.print_to_string().to_string();
        assert!(native_ir.contains("mir_map_any_v2_unpack_string_copy_status"));
        assert_eq!(
            native.mir_native_compiled_digest.as_deref(),
            Some(program.canonical_digest().as_str())
        );

        let mut wrong_profile = receipt.clone();
        wrong_profile.profile =
            CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller
                .as_str()
                .into();
        let profile_error =
            crate::codegen::mir::validate_checked_mir_native_values_crosscall_string_caller_index1_route(
                &checked,
                &program,
                &wrong_profile,
            )
            .expect_err("the preserved V1 profile cannot authorize the V2 native route");
        assert!(profile_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));

        let (v1_checked, v1_program) = checked_mir_from(include_str!(
            "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_values_crosscall_string_caller.mimi"
        ));
        let mut wrong_index =
            crate::core::mir::validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_route(
                &v1_checked,
                &v1_program,
            )
            .expect("the separate V1 index-zero route remains valid");
        wrong_index.profile =
            CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1
                .as_str()
                .into();
        let index_error =
            crate::codegen::mir::validate_checked_mir_native_values_crosscall_string_caller_index1_route(
                &v1_checked,
                &v1_program,
                &wrong_index,
            )
            .expect_err("a V1 index-zero Checker/MIR receipt cannot authorize V2 index one");
        assert!(index_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));

        let mixed_receipt_error =
            crate::codegen::mir::validate_checked_mir_native_values_crosscall_string_caller_index1_route(
                &checked,
                &program,
                &wrong_index,
            )
            .expect_err("a V1 MIR receipt mixed with V2 CheckedProgram evidence is rejected");
        assert!(mixed_receipt_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_RECEIPT_ERROR_CODE)
        }));
    }

    #[test]
    fn values_crosscall_string_caller_index1_native_entry_rejects_unselected_set_cleanup_tampering_before_emission(
    ) {
        use crate::core::mir::CanonicalMirRouteProfile;

        let source = include_str!(
            "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1.mimi"
        );
        let profile =
            CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1
                .as_str();
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let context = Context::create();

        for tamper in [
            Index1SetTemporaryTamper::RemoveKeyDrop,
            Index1SetTemporaryTamper::RetargetPayloadCloneDrop,
            Index1SetTemporaryTamper::AddKeyOwnerUse,
            Index1SetTemporaryTamper::ReorderTemporaryDrops,
        ] {
            let tokens = Lexer::new(source).tokenize().expect("lex Index1 fixture");
            let file = Parser::new(tokens)
                .parse_file()
                .expect("parse Index1 fixture");
            let checked = crate::core::check_program(&file).expect("check Index1 fixture");
            let mut program = MirProgram::from_checked_program(&checked)
                .expect("lower Index1 fixture to canonical MIR");
            tamper_index1_unselected_set_temporary(&mut program, tamper);
            let receipt = program.route_receipt(profile);
            let mut native = CodeGenerator::new(
                &context,
                &format!("values_crosscall_index1_tamper_{tamper:?}"),
            );
            native.shared = true;
            let native_user_symbols = program
                .functions()
                .keys()
                .map(|owner| {
                    crate::codegen::mir::native_symbol_for_owner(owner)
                        .expect("resolve native MIR user-function symbol")
                })
                .collect::<std::collections::BTreeSet<_>>();
            let defined_functions_before_route_rejection = native
                .module
                .get_functions()
                .filter(|function| function.count_basic_blocks() > 0)
                .map(|function| function.get_name().to_string_lossy().into_owned())
                .collect::<std::collections::BTreeSet<_>>();
            let errors = native
                .compile_checked_mir_native_with_values_crosscall_string_caller_index1_route(
                    &checked, &program, &receipt,
                )
                .expect_err("the checked native Index1 entry rejects malformed cleanup evidence");

            assert!(!errors.is_empty(), "{tamper:?} yields a route rejection");
            assert!(
                errors.iter().any(|error| {
                    error.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                        || error.code.as_deref()
                            == Some(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                }),
                "{tamper:?} is rejected by the Checker/MIR route gate: {errors:?}"
            );
            assert_eq!(
                native
                    .module
                    .get_functions()
                    .filter(|function| function.count_basic_blocks() > 0)
                    .map(|function| function.get_name().to_string_lossy().into_owned())
                    .collect::<std::collections::BTreeSet<_>>(),
                defined_functions_before_route_rejection,
                "{tamper:?} is rejected before any native function body is emitted"
            );
            assert!(
                native_user_symbols.iter().all(|symbol| native
                    .module
                    .get_function(symbol)
                    .is_none_or(|function| function.count_basic_blocks() == 0)),
                "{tamper:?} emits no MIR user-function bodies"
            );
            assert!(
                native.mir_native_compiled_digest.is_none(),
                "{tamper:?} produces no native MIR emission evidence"
            );
            assert!(
                !native.shared_dynamic_any_preflight_passed,
                "{tamper:?} produces no shared native preflight evidence"
            );
        }
    }

    #[test]
    fn map_root_producer_caller_checked_route_emits_handle_abi() {
        use crate::core::mir::{
            CanonicalMirRouteProfile, MIR_ROUTE_COVERAGE_ERROR_CODE,
            ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
        };

        const {
            assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let source = include_str!(
            "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_map_root_producer_caller.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex Map-root bridge");
        let file = Parser::new(tokens)
            .parse_file()
            .expect("parse Map-root bridge");
        let checked = crate::core::check_program(&file).expect("check Map-root bridge");
        let program = MirProgram::from_checked_program(&checked)
            .expect("Map-root producer/caller has canonical MIR");
        let receipt =
            crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_producer_caller_route(
                &checked, &program,
            )
            .expect("exact Checker/MIR Map-root producer-caller route");
        let bridge_receipts = program
            .ordinary_map_owned_any_v2_map_root_producer_caller_receipts()
            .expect("derive exact MIR Map-root producer-caller bridge receipt");
        let [bridge_receipt] = bridge_receipts.as_slice() else {
            panic!("expected one exact MIR Map-root producer-caller bridge receipt");
        };
        assert_eq!(
            bridge_receipt.root_type,
            crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
        );
        assert_eq!(
            receipt.profile,
            CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootProducerCaller.as_str()
        );
        crate::codegen::mir::validate_checked_mir_native_map_root_producer_caller_route(
            &checked, &program, &receipt,
        )
        .expect("native preflight revalidates the exact Checker and MIR evidence");

        let context = Context::create();
        let raw_error = CodeGenerator::new(&context, "map_root_bridge_raw_route")
            .compile_mir_native_with_route_receipt(&program, &receipt)
            .expect_err("raw MIR receipt cannot authorize the producer root return");
        assert!(raw_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic
                    .message
                    .contains("dedicated MapRootProducerCaller native route")
        }));

        let mut native = CodeGenerator::new(&context, "map_root_bridge_checked_route");
        native
            .compile_checked_mir_native_with_map_root_producer_caller_route(
                &checked, &program, &receipt,
            )
            .expect("only the dedicated Checker/MIR route authorizes native emission");
        native
            .module
            .verify()
            .expect("Map-root producer/caller module verifies");
        let producer_symbol =
            crate::codegen::mir::native_symbol_for_owner(&bridge_receipt.producer_owner)
                .expect("resolve exact producer symbol");
        let producer = native
            .module
            .get_function(&producer_symbol)
            .expect("native producer definition");
        assert_eq!(
            producer.get_type().get_return_type(),
            Some(context.i64_type().into()),
            "the receipted owner crosses the native call boundary as its i64 handle"
        );
        let native_ir = native.module.print_to_string().to_string();
        assert!(native_ir.contains("mimi_ordinary_map_owned_any_v2_new"));
        assert!(native_ir.contains("mimi_ordinary_map_owned_any_v2_set"));
        assert!(native_ir.contains("mimi_ordinary_map_owned_any_v2_size"));
        assert!(native_ir.contains("mimi_ordinary_map_owned_any_v2_drop"));
        assert_eq!(
            native.mir_native_compiled_digest.as_deref(),
            Some(program.canonical_digest().as_str())
        );
    }

    #[test]
    fn map_root_get_any_caller_checked_route_emits_owned_string_bridge() {
        use crate::core::mir::{
            CanonicalMirRouteProfile, MIR_ROUTE_COVERAGE_ERROR_CODE,
            ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
        };

        const {
            assert!(!ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let source = include_str!(
            "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_map_root_get_any_caller.mimi"
        );
        let tokens = Lexer::new(source)
            .tokenize()
            .expect("lex Map-root GetAny bridge");
        let file = Parser::new(tokens)
            .parse_file()
            .expect("parse Map-root GetAny bridge");
        let checked = crate::core::check_program(&file).expect("check Map-root GetAny bridge");
        let program = MirProgram::from_checked_program(&checked)
            .expect("Map-root GetAny caller has canonical MIR");
        let receipt =
            crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_get_any_caller_route(
                &checked, &program,
            )
            .expect("exact Checker/MIR Map-root GetAny caller route");
        let bridge_receipts = program
            .ordinary_map_owned_any_v2_map_root_get_any_caller_receipts()
            .expect("derive exact MIR Map-root GetAny bridge receipt");
        let [bridge_receipt] = bridge_receipts.as_slice() else {
            panic!("expected one exact MIR Map-root GetAny bridge receipt");
        };
        assert_eq!(
            bridge_receipt.root_type,
            crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
        );
        assert_eq!(
            bridge_receipt.string_type,
            crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String)
        );
        assert_eq!(
            receipt.profile,
            CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootGetAnyCaller.as_str()
        );
        assert_eq!(
            bridge_receipt.unpack_result.payload_source,
            crate::core::mir::OrdinaryMapOwnedAnyV2UnpackPayloadSource::SelectedSetPayload
        );
        assert_eq!(
            bridge_receipt.get_any_result.settlement_instruction,
            bridge_receipt.unpack_result.settlement_instruction,
            "the borrowed GetAny token settles with the pair Drop"
        );
        let caller = program
            .functions()
            .get(&bridge_receipt.caller_owner)
            .expect("receipted Map-root GetAny caller");
        assert_eq!(caller.blocks.len(), 1, "Map-root caller is straight-line");
        let caller_block = caller
            .blocks
            .values()
            .next()
            .expect("Map-root caller block exists");
        let instruction_position = |id: &crate::core::mir::MirInstructionId| {
            caller_block
                .instructions
                .iter()
                .position(|instruction| &instruction.id == id)
                .expect("receipt-bound Drop instruction exists")
        };
        assert!(
            instruction_position(&bridge_receipt.caller_root_drop_instruction)
                < instruction_position(&bridge_receipt.get_any_result.settlement_instruction)
        );
        assert!(
            instruction_position(&bridge_receipt.get_any_result.settlement_instruction)
                < instruction_position(&bridge_receipt.caller_string_drop_instruction)
        );
        crate::codegen::mir::validate_checked_mir_native_map_root_get_any_caller_route(
            &checked, &program, &receipt,
        )
        .expect("native preflight revalidates exact Checker, unpack and Drop evidence");

        let context = Context::create();
        let raw_error = CodeGenerator::new(&context, "map_root_get_any_raw_route")
            .compile_mir_native_with_route_receipt(&program, &receipt)
            .expect_err("raw MIR receipt cannot authorize the GetAny/String route");
        assert!(raw_error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic
                    .message
                    .contains("dedicated MapRootGetAnyCaller native route")
        }));

        let mut native = CodeGenerator::new(&context, "map_root_get_any_checked_route");
        native
            .compile_checked_mir_native_with_map_root_get_any_caller_route(
                &checked, &program, &receipt,
            )
            .expect("only the dedicated Checker/MIR route authorizes native emission");
        native
            .module
            .verify()
            .expect("Map-root GetAny caller module verifies");
        let producer_symbol =
            crate::codegen::mir::native_symbol_for_owner(&bridge_receipt.producer_owner)
                .expect("resolve exact producer symbol");
        let producer = native
            .module
            .get_function(&producer_symbol)
            .expect("native producer definition");
        assert_eq!(
            producer.get_type().get_return_type(),
            Some(context.i64_type().into()),
            "the receipted Map root crosses the native call boundary as its i64 handle"
        );
        let native_ir = native.module.print_to_string().to_string();
        assert!(native_ir.contains("mimi_ordinary_map_owned_any_v2_new"));
        assert!(native_ir.contains("mimi_ordinary_map_owned_any_v2_set"));
        assert!(native_ir.contains("mimi_ordinary_map_owned_any_v2_get"));
        assert!(native_ir.contains("mimi_any_payload_v2_unpack"));
        assert!(native_ir.contains("mimi_mir_ordinary_map_owned_any_v2_unpack_copy_string"));
        assert!(native_ir.contains("mimi_ordinary_map_owned_any_v2_drop"));
        assert_eq!(
            native.mir_native_compiled_digest.as_deref(),
            Some(program.canonical_digest().as_str())
        );
    }

    #[test]
    fn map_root_producer_caller_native_gate_rejects_mismatched_call_result_type() {
        use crate::core::mir::{MIR_ROUTE_COVERAGE_ERROR_CODE, MIR_ROUTE_RECEIPT_ERROR_CODE};

        let source = include_str!(
            "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_map_root_producer_caller.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex Map-root bridge");
        let file = Parser::new(tokens)
            .parse_file()
            .expect("parse Map-root bridge");
        let checked = crate::core::check_program(&file).expect("check Map-root bridge");
        let program = MirProgram::from_checked_program(&checked)
            .expect("Map-root producer/caller has canonical MIR");
        let receipt =
            crate::core::mir::validate_ordinary_map_owned_any_v2_map_root_producer_caller_route(
                &checked, &program,
            )
            .expect("exact Checker/MIR Map-root producer-caller route");
        let bridge_receipts = program
            .ordinary_map_owned_any_v2_map_root_producer_caller_receipts()
            .expect("derive exact MIR Map-root producer-caller bridge receipt");
        let [bridge_receipt] = bridge_receipts.as_slice() else {
            panic!("expected one exact MIR Map-root producer-caller bridge receipt");
        };

        let mut forged = program.clone();
        let mut caller = forged
            .functions()
            .get(&bridge_receipt.caller_owner)
            .expect("receipted caller")
            .clone();
        caller
            .values
            .get_mut(&bridge_receipt.caller_call_result)
            .expect("producer Call result")
            .ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        forged.replace_function_for_test_only(caller);

        let native_error = crate::codegen::mir::validate_mir_native(&forged)
            .expect_err("native MIR validation rejects a mismatched producer Call result TypeDesc");
        assert!(native_error.iter().any(|diagnostic| {
            diagnostic.message.contains("MapRoot")
                || diagnostic.message.contains("Map-root")
                || diagnostic.code.as_deref() == Some(MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));
        let checked_error =
            crate::codegen::mir::validate_checked_mir_native_map_root_producer_caller_route(
                &checked, &forged, &receipt,
            )
            .expect_err("checked native route rejects the post-receipt TypeDesc mutation");
        assert!(checked_error.iter().any(|diagnostic| {
            matches!(
                diagnostic.code.as_deref(),
                Some(MIR_ROUTE_COVERAGE_ERROR_CODE | MIR_ROUTE_RECEIPT_ERROR_CODE)
            )
        }));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "map_root_bridge_mismatched_result");
        let defined_functions_before_emission = generator
            .module
            .get_functions()
            .filter(|function| function.count_basic_blocks() > 0)
            .map(|function| function.get_name().to_string_lossy().into_owned())
            .collect::<std::collections::BTreeSet<_>>();
        let emission_error = generator
            .compile_checked_mir_native_with_map_root_producer_caller_route(
                &checked, &forged, &receipt,
            )
            .expect_err("mismatched Call result is rejected before native emission");
        assert!(emission_error.iter().any(|diagnostic| {
            matches!(
                diagnostic.code.as_deref(),
                Some(MIR_ROUTE_COVERAGE_ERROR_CODE | MIR_ROUTE_RECEIPT_ERROR_CODE)
            )
        }));
        assert_eq!(
            generator
                .module
                .get_functions()
                .filter(|function| function.count_basic_blocks() > 0)
                .map(|function| function.get_name().to_string_lossy().into_owned())
                .collect::<std::collections::BTreeSet<_>>(),
            defined_functions_before_emission,
            "route rejection must precede MIR function-body emission"
        );
    }

    #[test]
    fn values_producer_caller_native_gate_rejects_mismatched_call_result_type() {
        use crate::core::mir::{MIR_ROUTE_COVERAGE_ERROR_CODE, MIR_ROUTE_RECEIPT_ERROR_CODE};

        let source = include_str!(
            "../../../tests/fixtures/mir_ordinary_map_owned_any_v2_values_producer_caller.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex Values bridge");
        let file = Parser::new(tokens)
            .parse_file()
            .expect("parse Values bridge");
        let checked = crate::core::check_program(&file).expect("check Values bridge");
        let program = MirProgram::from_checked_program(&checked)
            .expect("Values producer/caller has canonical MIR");
        let receipt =
            crate::core::mir::validate_ordinary_map_owned_any_v2_values_producer_caller_route(
                &checked, &program,
            )
            .expect("exact Checker/MIR producer-caller route");
        let bridge_receipts = program
            .ordinary_map_owned_any_v2_values_producer_caller_receipts()
            .expect("derive exact MIR producer-caller bridge receipt");
        let [bridge_receipt] = bridge_receipts.as_slice() else {
            panic!("expected one exact MIR producer-caller bridge receipt");
        };

        let mut forged = program.clone();
        let mut caller = forged
            .functions()
            .get(&bridge_receipt.caller_owner)
            .expect("receipted caller")
            .clone();
        caller
            .values
            .get_mut(&bridge_receipt.call_result)
            .expect("producer Call result")
            .ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        forged.replace_function_for_test_only(caller);

        let native_error = crate::codegen::mir::validate_mir_native(&forged)
            .expect_err("native MIR validation rejects a mismatched producer Call result TypeDesc");
        assert!(native_error.iter().any(|diagnostic| {
            diagnostic.message.contains("Values")
                || diagnostic
                    .message
                    .to_ascii_lowercase()
                    .contains("call result")
        }));
        let checked_error =
            crate::codegen::mir::validate_checked_mir_native_values_producer_caller_route(
                &checked, &forged, &receipt,
            )
            .expect_err("the checked native route rejects the same post-receipt type mutation");
        assert!(checked_error.iter().any(|diagnostic| {
            matches!(
                diagnostic.code.as_deref(),
                Some(MIR_ROUTE_COVERAGE_ERROR_CODE | MIR_ROUTE_RECEIPT_ERROR_CODE)
            )
        }));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "values_bridge_mismatched_result");
        let defined_functions_before_emission = generator
            .module
            .get_functions()
            .filter(|function| function.count_basic_blocks() > 0)
            .map(|function| function.get_name().to_string_lossy().into_owned())
            .collect::<std::collections::BTreeSet<_>>();
        let emission_error = generator
            .compile_checked_mir_native_with_values_producer_caller_route(
                &checked, &forged, &receipt,
            )
            .expect_err("mismatched Call result is rejected before native emission");
        assert!(emission_error.iter().any(|diagnostic| {
            matches!(
                diagnostic.code.as_deref(),
                Some(MIR_ROUTE_COVERAGE_ERROR_CODE | MIR_ROUTE_RECEIPT_ERROR_CODE)
            )
        }));
        assert!(
            generator
                .module
                .get_functions()
                .filter(|function| function.count_basic_blocks() > 0)
                .map(|function| function.get_name().to_string_lossy().into_owned())
                .collect::<std::collections::BTreeSet<_>>()
                == defined_functions_before_emission,
            "route rejection must precede MIR function-body emission"
        );
    }

    #[test]
    fn ordinary_map_owned_any_v2_values_unpack_clones_selected_payloads_and_traps_cleanly() {
        let hit = canonical_program(&ordinary_map_owned_any_v2_values_unpack_native_source(
            r#"
            let first_from_end: i64 = -3
            let picked_label = snapshot[first_from_end] as Label
            let middle = 1
            let picked_number = snapshot[middle] as i32
            let picked_text = snapshot[2] as string
            let text_matches = picked_text == "Values String payload"
            println(text_matches)
            println(picked_number)
            drop(picked_label)
            "#,
        ));
        crate::verifier::validate_mir_capabilities(&hit)
            .expect("all Values unpack and snapshot Drop receipts pass capability validation");
        crate::codegen::mir::validate_mir_native(&hit)
            .expect("native admission validates every Values unpack receipt before emission");
        let receipts = hit
            .ordinary_map_owned_any_v2_values_unpack_result_receipts()
            .expect("exact indexed Values unpack receipts");
        assert_eq!(receipts.len(), 3);
        assert!(receipts.iter().all(|receipt| {
            receipt.validate_against_program(&hit).is_ok()
                && receipt.conversion_to == receipt.result_type
        }));

        let main = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&hit)
            .execute_with_output(&main, &[])
            .expect("Reference Values unpack execution");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(reference.output, "true\n42\n");
        let mut vm = BytecodeVM::new(
            compile_mir_program(&hit).expect("AST-free bytecode Values unpack program"),
        );
        assert!(matches!(
            vm.run_value_for_component()
                .expect("bytecode Values unpack"),
            Value::Int(0)
        ));
        assert_eq!(vm.take_stdout(), "true\n42\n");

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_map_any_v2_values_unpack");
        generator
            .compile_mir_native_for_component(&hit)
            .expect("native V2 Values unpack emitter");
        generator
            .module
            .verify()
            .expect("native V2 Values unpack LLVM module verifies");
        let native_ir = generator.module.print_to_string().to_string();
        assert_eq!(
            native_ir
                .matches("call i32 @mimi_any_payload_v2_unpack")
                .count(),
            3,
            "each index read uses the typed V2 payload unpack ABI"
        );
        assert_eq!(
            native_ir
                .matches("call i32 @mimi_mir_list_get_managed_any_v2")
                .count(),
            3,
            "each index read retains its own selected element token"
        );
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("run native V2 Values unpack executable");
        assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
        assert_eq!(native.stdout, "true\n42\n");
        assert_eq!(native.stderr, "");

        if std::process::Command::new("valgrind")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            let memcheck = crate::tests::link_and_observe_canonical_mir_with_config(
                &generator,
                &strict_map_any_memcheck_config(),
            )
            .expect("run successful native Values unpack under strict Memcheck");
            assert_clean_map_any_memcheck(&memcheck, 0, "true\n42\n", None);
        }

        let wrong_tag_source = ordinary_map_owned_any_v2_values_unpack_native_source(
            "let rejected = snapshot[1] as string\nprintln(rejected)",
        );
        let wrong_tag_tokens = Lexer::new(&wrong_tag_source)
            .tokenize()
            .expect("lex statically mismatched Values payload");
        let wrong_tag_file = Parser::new(wrong_tag_tokens)
            .parse_file()
            .expect("parse statically mismatched Values payload");
        let wrong_tag_diagnostics = crate::core::check_program(&wrong_tag_file)
            .expect_err("a statically wrong key-sorted payload type must fail in Checker");
        assert!(
            wrong_tag_diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_deref() == Some("E0200")),
            "wrong-index payload cast must fail closed: {wrong_tag_diagnostics:?}"
        );

        {
            let (name, body, code) = (
                "out-of-bounds",
                "let rejected = snapshot[3] as i32\nprintln(rejected)",
                "E0803",
            );
            let rejected =
                canonical_program(&ordinary_map_owned_any_v2_values_unpack_native_source(body));
            let reference_error = MirReferenceInterpreter::new(&rejected)
                .execute_with_output(&main, &[])
                .expect_err("Reference rejects an invalid Values index/type selection");
            assert!(
                reference_error.message.contains(code),
                "{reference_error:?}"
            );
            let mut rejected_vm = BytecodeVM::new(
                compile_mir_program(&rejected).expect("AST-free bytecode invalid Values case"),
            );
            let bytecode_error = rejected_vm
                .run_value_for_component()
                .expect_err("bytecode rejects an invalid Values index/type selection");
            assert_eq!(bytecode_error.code(), code);

            let context = Context::create();
            let mut rejected_generator = CodeGenerator::new(
                &context,
                &format!("mir_native_map_any_v2_values_unpack_{name}"),
            );
            rejected_generator
                .compile_mir_native_for_component(&rejected)
                .expect("native invalid Values case emits a checked trap");
            let native = crate::tests::link_and_observe_canonical_mir(&rejected_generator)
                .expect("run native invalid Values case");
            assert_eq!(native.exit_code, Some(1), "{}", native.stderr);
            assert!(native.stderr.contains(code), "{}", native.stderr);
            assert!(native.stdout.is_empty(), "{}", native.stdout);

            if std::process::Command::new("valgrind")
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
            {
                let memcheck = crate::tests::link_and_observe_canonical_mir_with_config(
                    &rejected_generator,
                    &strict_map_any_memcheck_config(),
                )
                .expect("run invalid native Values case under strict Memcheck");
                assert_clean_map_any_memcheck(&memcheck, 1, "", Some(code));
            }
        }
    }

    fn persist_static_values_oob_l3_memcheck(
        index: i64,
        observed: &crate::tests::NativeRunObservation,
    ) {
        use std::path::PathBuf;

        let Some(root) = std::env::var_os("MIMI_MAP_V2_VALUES_STATIC_OOB_L3_ARTIFACT_DIR") else {
            return;
        };
        let case_name = match index {
            1 => "raw-index-1",
            -2 => "raw-index-minus-2",
            _ => panic!("unexpected static Values OOB index for L3 evidence: {index}"),
        };
        let root = PathBuf::from(root);
        let case_dir = root.join(case_name);
        std::fs::create_dir(&case_dir).expect("fresh static Values OOB case evidence directory");
        std::fs::write(case_dir.join("valgrind.stderr.bin"), &observed.stderr_bytes)
            .expect("preserve raw Valgrind stderr");
        std::fs::write(case_dir.join("stdout.bin"), &observed.stdout_bytes)
            .expect("preserve raw native stdout");
        std::fs::write(
            case_dir.join("run.status.txt"),
            format!(
                "raw_index={index}\nexit_code={}\nexpected_exit_code=1\nexpected_trap=E0803\n",
                observed
                    .exit_code
                    .map_or_else(|| "signal".into(), |code| code.to_string())
            ),
        )
        .expect("preserve static Values OOB process status");

        let write_argv = |path: &std::path::Path, argv: &[String]| {
            let mut bytes = Vec::new();
            for argument in argv {
                bytes.extend_from_slice(argument.as_bytes());
                bytes.push(0);
            }
            std::fs::write(path, bytes).expect("preserve exact native command argv");
        };
        let write_command = |path: &std::path::Path, argv: &[String]| {
            std::fs::write(
                path,
                argv.iter()
                    .map(|argument| format!("{argument:?}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .expect("preserve readable native command");
        };

        let run_command = observed
            .actual_run_command
            .as_ref()
            .expect("strict Memcheck observation has exact Valgrind argv");
        write_argv(&case_dir.join("valgrind.command.argv.nul"), run_command);
        write_command(&case_dir.join("valgrind.command.txt"), run_command);
        let mut replay_run_command = run_command.clone();
        *replay_run_command
            .last_mut()
            .expect("Valgrind command ends with the native program") = "./relinked-program".into();
        write_argv(
            &case_dir.join("valgrind.replay.argv.nul"),
            &replay_run_command,
        );
        write_command(
            &case_dir.join("valgrind.replay.command.txt"),
            &replay_run_command,
        );
        std::fs::write(case_dir.join("valgrind.replay.cwd.txt"), ".\n")
            .expect("record Valgrind replay working directory");

        write_argv(
            &case_dir.join("link.command.argv.nul"),
            &observed.actual_link_command,
        );
        write_command(
            &case_dir.join("link.command.txt"),
            &observed.actual_link_command,
        );

        let mut artifact_records = Vec::new();
        let mut replay_link_command = observed.actual_link_command.clone();
        for artifact in &observed.artifacts {
            let (persisted_path, replay_path) = match artifact.role {
                "native-binary" => (case_dir.join("program"), "./relinked-program"),
                "native-object" => (case_dir.join("native-object"), "./native-object"),
                "runtime-archive" => (case_dir.join("runtime-archive"), "./runtime-archive"),
                _ => continue,
            };
            std::fs::copy(&artifact.path, &persisted_path)
                .expect("preserve strict Memcheck native input/output artifact");
            let bytes = std::fs::read(&persisted_path).expect("read copied native artifact");
            let copied_hash = blake3::hash(&bytes).to_hex().to_string();
            let source_path = artifact.path.to_string_lossy().into_owned();
            for argument in &mut replay_link_command {
                if argument == &source_path {
                    *argument = replay_path.into();
                }
            }
            artifact_records.push(format!(
                "role={}\nsource_path={}\npersisted_path={}\nblake3={}\nhash_before_link={}\nhash_before_run={}\nhash_after_run={}\nstable_during_execution={}\n",
                artifact.role,
                source_path,
                persisted_path.display(),
                copied_hash,
                artifact.hash_before_link.as_deref().unwrap_or("<not-retained>"),
                artifact.hash_before_run.as_deref().unwrap_or("<not-retained>"),
                artifact.hash_after_run.as_deref().unwrap_or("<not-retained>"),
                artifact
                    .stable_during_execution
                    .map_or_else(|| "<not-retained>".to_owned(), |stable| stable.to_string()),
            ));
        }
        let output_flag = replay_link_command
            .iter()
            .position(|argument| argument == "-o")
            .expect("native linker argv has an output flag");
        *replay_link_command
            .get_mut(output_flag + 1)
            .expect("native linker output flag has a path") = "./relinked-program".into();
        std::fs::write(
            case_dir.join("native-artifacts.txt"),
            artifact_records.join("\n"),
        )
        .expect("preserve native artifact hashes and paths");
        write_argv(&case_dir.join("link.replay.argv.nul"), &replay_link_command);
        write_command(
            &case_dir.join("link.replay.command.txt"),
            &replay_link_command,
        );
        std::fs::write(case_dir.join("link.replay.cwd.txt"), ".\n")
            .expect("record native link replay working directory");

        let kept_tmp_dir = observed
            .kept_tmp_dir
            .as_ref()
            .expect("evidence mode retains the native temporary directory for copying");
        std::fs::remove_dir_all(kept_tmp_dir)
            .expect("clean temporary directory after preserving required evidence");
        std::fs::write(case_dir.join("native-tmp-cleaned.txt"), "true\n")
            .expect("record cleanup of temporary native artifacts");
    }

    #[test]
    fn ordinary_map_owned_any_v2_values_static_oob_without_selected_target_traps_before_get() {
        ordinary_map_owned_any_v2_values_static_oob_without_selected_target_traps_before_get_at(1);
    }

    #[test]
    fn ordinary_map_owned_any_v2_values_static_negative_oob_without_selected_target_traps_before_get(
    ) {
        ordinary_map_owned_any_v2_values_static_oob_without_selected_target_traps_before_get_at(-2);
    }

    fn ordinary_map_owned_any_v2_values_static_oob_without_selected_target_traps_before_get_at(
        index: i64,
    ) {
        let source = r#"
            func main() -> i32 {
                let text = "only selected String payload"
                let empty = map_new()
                let root = map_set(empty, "string", text)
                let snapshot = values(root)
                let rejected = snapshot[__INDEX__] as i32
                println(rejected)
                drop(snapshot)
                drop(empty)
                drop(root)
                drop(text)
                0
            }
            "#
        .replace("__INDEX__", &index.to_string());
        let program = canonical_program(&source);

        let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        let selection = program
            .ordinary_map_owned_any_v2_payload_selection_receipt()
            .expect("Map payload selection receipt")
            .expect("one Map payload selection");
        selection
            .validate_against_program(&program)
            .expect("selection receipt is bound to the exact Map payload TypeDesc set");
        assert_eq!(selection.selected_types.len(), 1);
        assert_eq!(selection.selected_types[0].type_id, string_type);
        assert!(
            selection
                .selected_types
                .iter()
                .all(|selected| selected.type_id != i32_type),
            "i32 is not present in any selected Map payload TypeDesc"
        );

        let receipts = program
            .ordinary_map_owned_any_v2_values_unpack_result_receipts()
            .expect("exact indexed Values unpack receipt");
        let [receipt] = receipts.as_slice() else {
            panic!("one receipt-bound Values unpack");
        };
        receipt
            .validate_against_program(&program)
            .expect("the Values receipt independently validates its OOB index and snapshot");
        assert_eq!(receipt.result_type, i32_type);
        assert_eq!(
            receipt.index_disposition,
            crate::core::OrdinaryMapOwnedAnyV2ValuesIndexDisposition::AlwaysOutOfBounds {
                raw_index: index,
                snapshot_len: 1,
            }
        );
        crate::verifier::validate_mir_capabilities(&program)
            .expect("native L1/L2 capability checks accept the receipt-bound OOB operation");
        crate::codegen::mir::validate_mir_native(&program)
            .expect("native route validation accepts OOB with an unselected target TypeDesc");

        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_values_oob_target_absent_from_selection",
        );
        generator
            .compile_mir_native_for_component(&program)
            .expect("native emitter compiles the receipt-bound OOB branch");
        generator
            .module
            .verify()
            .expect("native LLVM module verifies");
        let native_ir = generator.module.print_to_string().to_string();
        let main = crate::core::NodeId("function:main".into());
        let reference_error = MirReferenceInterpreter::new(&program)
            .execute_with_output(&main, &[])
            .expect_err("Reference reports the receipt-bound static OOB");
        assert!(
            reference_error.message.contains("E0803"),
            "{reference_error:?}"
        );

        let mut vm = BytecodeVM::new(
            compile_mir_program(&program).expect("AST-free static OOB bytecode program"),
        );
        let bytecode_error = vm
            .run_value_for_component()
            .expect_err("bytecode reports the receipt-bound static OOB");
        assert_eq!(bytecode_error.code(), "E0803");
        assert!(vm.take_stdout().is_empty());

        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("run native receipt-bound static OOB");
        assert_eq!(native.exit_code, Some(1), "{}", native.stderr);
        assert!(native.stderr.contains("E0803"), "{}", native.stderr);
        assert!(native.stdout.is_empty(), "{}", native.stdout);
        let l3_evidence_requested =
            std::env::var_os("MIMI_MAP_V2_VALUES_STATIC_OOB_L3_ARTIFACT_DIR").is_some();
        let valgrind_available = std::process::Command::new("valgrind")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success());
        assert!(
            !l3_evidence_requested || valgrind_available,
            "static Values OOB strict L3 requires Valgrind; refusing a skipped Memcheck run"
        );
        if valgrind_available {
            let memcheck = crate::tests::link_and_observe_canonical_mir_with_config(
                &generator,
                &strict_map_any_memcheck_config(),
            )
            .expect("run static OOB trap under strict Memcheck");
            if l3_evidence_requested {
                persist_static_values_oob_l3_memcheck(index, &memcheck);
            }
            assert_clean_map_any_memcheck(&memcheck, 1, "", Some("E0803"));
        }

        assert!(
            native_ir.contains("call void @mimi_runtime_abort("),
            "native code emits a runtime abort call for the receipt-bound E0803 path"
        );
        let e0803_message_symbol = native_ir
            .lines()
            .find(|line| line.contains("c\"[E0803] list index out of bounds"))
            .and_then(|line| line.split_once(" = ").map(|(symbol, _)| symbol))
            .expect("static OOB trap has its canonical E0803 message global");
        let e0803_abort_call = format!("call void @mimi_runtime_abort(ptr {e0803_message_symbol})");
        let main_start = native_ir
            .find("define i32 @main() {")
            .expect("canonical native module defines main");
        let main_end = native_ir[main_start..]
            .find("\n}")
            .map(|offset| main_start + offset + 2)
            .expect("canonical native main has a closing brace");
        let mut current_block = String::new();
        let mut static_oob_abort_block = None;
        for line in native_ir[main_start..main_end].lines() {
            if !line.starts_with(' ') && line.contains(':') && !line.starts_with("define") {
                if current_block.contains(&e0803_abort_call) {
                    static_oob_abort_block = Some(current_block.clone());
                }
                current_block.clear();
            }
            current_block.push_str(line);
            current_block.push('\n');
        }
        if current_block.contains(&e0803_abort_call) {
            static_oob_abort_block = Some(current_block);
        }
        let static_oob_abort_block =
            static_oob_abort_block.expect("the E0803 abort belongs to a native basic block");
        assert!(
            static_oob_abort_block.contains(&e0803_abort_call),
            "the receipt-bound static OOB block emits its canonical E0803 abort"
        );
        assert!(
            !static_oob_abort_block.contains("mir_ordinary_map_trap_drop"),
            "static OOB must not clean Map roots before reporting the receipt-bound E0803"
        );
        for runtime_call in [
            "call i32 @mimi_mir_list_get_managed_any_v2(",
            "call i32 @mimi_any_payload_v2_unpack(",
        ] {
            assert!(
                !native_ir.contains(runtime_call),
                "static OOB must trap without emitting `{runtime_call}`"
            );
        }

        let i32_llvm_suffix = context
            .i32_type()
            .print_to_string()
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        for target_glue in [
            format!("mimi_any_payload_v2_clone_glue__scalar__llvm_{i32_llvm_suffix}"),
            format!("mimi_any_payload_v2_drop_glue__scalar__llvm_{i32_llvm_suffix}"),
        ] {
            assert!(
                !native_ir.contains(&target_glue),
                "static OOB must not emit selected-target glue `{target_glue}`"
            );
        }

        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("run native receipt-bound Values OOB case");
        assert_eq!(native.exit_code, Some(1), "{}", native.stderr);
        assert!(native.stderr.contains("E0803"), "{}", native.stderr);
        assert!(native.stdout.is_empty(), "{}", native.stdout);
    }

    #[test]
    fn ordinary_map_owned_any_v2_values_label_field_move_runs_across_all_consumers() {
        use crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind as OwnerSettlement;

        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_ordinary_map_any_owner_v2_values_field_move.mimi"
        ));
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        crate::verifier::validate_mir_capabilities(&program)
            .expect("the full source/MIR receipt is a verifier capability");
        crate::codegen::mir::validate_mir_native(&program)
            .expect("native preflight rechecks the field and String-read receipts");

        let receipts = program
            .ordinary_map_owned_any_v2_values_unpack_result_receipts()
            .expect("same MIR carries direct field-move and borrowed-read receipts");
        let [receipt] = receipts.as_slice() else {
            panic!("one direct Values Label unpack");
        };
        assert_eq!(
            receipt.result_owner_settlement,
            OwnerSettlement::DirectStringFieldMove
        );
        let field_move = receipt
            .result_owner_field_move
            .as_ref()
            .expect("one whole-Label consuming projection");
        assert_eq!(field_move.source.text_read_uses.len(), 2);
        assert_eq!(field_move.text_read_uses.len(), 2);
        assert!(field_move.source.has_exact_source_use_sets());
        let [eq_read, ne_read] = field_move.text_read_uses.as_slice() else {
            panic!("MIR carries one equality and one inequality read")
        };
        assert_eq!(eq_read.source.peer_literal, "expected");
        assert_eq!(eq_read.op, ResolvedBinaryOp::Equal);
        assert_eq!(ne_read.source.peer_literal, "unexpected");
        assert_eq!(ne_read.op, ResolvedBinaryOp::NotEqual);

        let main = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&main, &[])
            .expect("reference executes the selected child String after parent cleanup");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(reference.output, "true\ntrue\n");

        let bytecode = compile_mir_program(&program)
            .expect("AST-free bytecode emits the same MIR source-use receipt");
        let proto = &bytecode.functions[bytecode.entry as usize];
        let (unpack_pc, _) = proto
            .code
            .iter()
            .enumerate()
            .find(|(_, op)| matches!(op, Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { .. }))
            .expect("one bytecode Values unpack");
        let bytecode_binding = bytecode
            .canonical_map_any_owner_v1_receipt
            .as_ref()
            .and_then(|receipt| {
                receipt.ordinary_map_owned_any_v2_values_unpack_binding_at_pc(
                    bytecode.entry,
                    u32::try_from(unpack_pc).expect("bytecode PC fits the receipt ABI"),
                )
            })
            .expect("exact bytecode unpack binding");
        let bytecode_field_move = bytecode_binding
            .result_owner_field_move
            .as_ref()
            .expect("bytecode keeps the field-move receipt");
        assert_eq!(bytecode_field_move.receipt, *field_move);
        let [bytecode_eq_read, bytecode_ne_read] = bytecode_field_move.text_read_uses.as_slice()
        else {
            panic!("bytecode carries exact equality and inequality bindings");
        };
        for (bytecode_read, mir_read, not_equal) in [
            (bytecode_eq_read, eq_read, false),
            (bytecode_ne_read, ne_read, true),
        ] {
            assert_eq!(bytecode_read.receipt, *mir_read);
            let operands_match = match proto.code.get(bytecode_read.pc as usize) {
                Some(Op::Eq { rd, ra, rb }) if !not_equal => {
                    *rd == bytecode_read.rd
                        && *ra == bytecode_read.left_reg
                        && *rb == bytecode_read.right_reg
                }
                Some(Op::Ne { rd, ra, rb }) if not_equal => {
                    *rd == bytecode_read.rd
                        && *ra == bytecode_read.left_reg
                        && *rb == bytecode_read.right_reg
                }
                _ => false,
            };
            assert!(
                operands_match,
                "Bytecode operator/register receipt must be exact"
            );
            assert!(bytecode_read.pc > bytecode_field_move.text_binding_pc);
            assert!(bytecode_read.pc < bytecode_field_move.text_drop_pc);
        }
        let mut vm = BytecodeVM::new(bytecode);
        assert!(matches!(
            vm.run_value_for_component()
                .expect("bytecode executes the same lifecycle"),
            Value::Int(0)
        ));
        assert_eq!(vm.take_stdout(), reference.output);

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_values_label_field_move");
        generator
            .compile_mir_native_for_component(&program)
            .expect("test-only native consumer accepts the same admitted MIR receipt");
        generator
            .module
            .verify()
            .expect("native LLVM module verifies");
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("test-only native execution uses the direct field-move fixture");
        assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
        assert_eq!(native.stdout, reference.output);
        assert_eq!(native.stderr, "");

        if std::process::Command::new("valgrind")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            let memcheck = crate::tests::link_and_observe_canonical_mir_with_config(
                &generator,
                &strict_map_any_memcheck_config(),
            )
            .expect("test-only native field move runs under strict Memcheck");
            assert_clean_map_any_memcheck(&memcheck, 0, &reference.output, None);
        }
    }

    fn ordinary_map_owned_any_v2_unpack_native_source(key: &str) -> String {
        format!(
            r#"
            type Label {{ text: string }}
            func main() -> i32 {{
                let label = Label {{ text: "native unpack fixture" }}
                let empty = map_new()
                let numbers = map_set(empty, "number", 42)
                let mixed = map_set(numbers, "label", label)
                let pair = map_get(mixed, "{key}")
                let value = unpack_i32(pair.1)
                drop(pair)
                drop(empty)
                drop(numbers)
                drop(mixed)
                drop(label)
                println(value)
                0
            }}
            "#
        )
    }

    fn ordinary_map_owned_any_v2_print_native_source(value: &str, get_key: &str) -> String {
        format!(
            r#"
            func main() -> i32 {{
                let empty = map_new()
                let root = map_set(empty, "value", {value})
                let pair = map_get(root, "{get_key}")
                drop(empty)
                drop(root)
                println(pair.1)
                drop(pair)
                0
            }}
            "#
        )
    }

    #[test]
    fn ordinary_map_owned_any_v2_direct_print_runs_native_for_i32_and_string_hits() {
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let fixtures = [
            (
                ordinary_map_owned_any_v2_print_native_source("negative", "value").replace(
                    "let empty = map_new()",
                    "let negative: i32 = -137\n                let empty = map_new()",
                ),
                "-137\n",
                false,
            ),
            (
                ordinary_map_owned_any_v2_print_native_source("\"owned\\0🧪 值\"", "value"),
                "owned\0🧪 值\n",
                true,
            ),
            (
                ordinary_map_owned_any_v2_print_native_source("\"present payload\"", "missing"),
                "0\n",
                true,
            ),
        ];
        for (source, expected, has_string) in fixtures {
            let program = canonical_program(&source);
            crate::codegen::mir::validate_mir_native(&program)
                .expect("native preflight accepts only the exact direct Any print receipt");
            let receipts = program
                .ordinary_map_owned_any_v2_println_receipts()
                .expect("Core derives the source-bound V2 Any println receipt");
            assert_eq!(receipts.len(), 1);
            receipts[0]
                .validate_against_program(&program)
                .expect("print receipt binds the GetAny root, key, projection and call");
            let main = program
                .functions()
                .get(&crate::core::NodeId("function:main".into()))
                .expect("main MIR");
            let print_instructions = main
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::OrdinaryMapOwnedAnyV2Println { .. }
                    )
                })
                .count();
            assert_eq!(print_instructions, 1);

            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, "mir_native_map_any_v2_print");
            generator
                .compile_mir_native_for_component(&program)
                .expect("native V2 direct Any println emitter");
            generator
                .module
                .verify()
                .expect("native V2 direct Any println module verifies");
            let ir = generator.module.print_to_string().to_string();
            assert!(ir.contains("mimi_any_payload_v2_unpack"));
            if has_string {
                assert!(ir.contains("mir_map_any_v2_print_string_fallback_allowed"));
                assert!(ir.contains("mimi_print_bytes"));
            }
            assert!(ir.contains("printf"));
            assert!(!ir.contains("mimi_mir_managed_any_i32_string_v1"));

            let native = crate::tests::link_and_observe_canonical_mir(&generator)
                .expect("native V2 direct Any println executable");
            assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
            assert_eq!(native.stdout.as_bytes(), expected.as_bytes());
            assert_eq!(native.stderr, "");
        }
    }

    #[test]
    fn ordinary_map_owned_any_v2_direct_print_requires_its_exact_native_route_receipt() {
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let source = include_str!(
            "../../../tests/fixtures/mir_ordinary_map_any_owner_v2_direct_print_hit_i32.mimi"
        );
        let program = canonical_program(source);
        crate::core::mir::validate_ordinary_map_owned_any_v2_direct_print_mir_route(&program)
            .expect("the fixture has the exact Checker-sealed DirectPrint shape");
        let receipt =
            program.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE);
        receipt
            .validate_against_program(&program)
            .expect("the dedicated route receipt binds this immutable MIR");

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "direct_print_route_receipt");
        generator
            .compile_mir_native_with_route_receipt(&program, &receipt)
            .expect("the exact DirectPrint receipt authorizes the narrow native slice");
        generator
            .module
            .verify()
            .expect("DirectPrint native module verifies");
        assert_eq!(generator.test_mir_native_route_receipt(), Some(&receipt));

        let broad_receipt =
            program.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
        let error = CodeGenerator::new(&context, "direct_print_broad_receipt")
            .compile_mir_native_with_route_receipt(&program, &broad_receipt)
            .expect_err("the broad V2 receipt does not authorize DirectPrint");
        assert!(error.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        }));

        let mut forged = program.clone();
        let (owner, mut function, instruction_id) = forged
            .functions()
            .iter()
            .find_map(|(owner, function)| {
                function
                    .blocks
                    .values()
                    .flat_map(|block| &block.instructions)
                    .find(|instruction| {
                        matches!(
                            instruction.kind,
                            MirInstructionKind::OrdinaryMapOwnedAnyV2Println { .. }
                        )
                    })
                    .map(|instruction| (owner.clone(), function.clone(), instruction.id.clone()))
            })
            .expect("Checker-lowered DirectPrint instruction");
        let instruction = function
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| instruction.id == instruction_id)
            .expect("same DirectPrint instruction in owner");
        instruction.kind = MirInstructionKind::Nop;
        forged.replace_function_for_test_only(function);
        let forged_receipt =
            forged.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE);
        let mut forged_generator = CodeGenerator::new(&context, "direct_print_forged_receipt");
        let errors = forged_generator
            .compile_mir_native_with_route_receipt(&forged, &forged_receipt)
            .expect_err("a resealed MIR digest cannot replace the DirectPrint operation receipt");
        assert!(errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic
                    .message
                    .contains("DirectPrint native route rejected")
        }));
        assert!(forged_generator.test_mir_native_route_receipt().is_none());
        assert_eq!(owner.0, "function:main");
    }

    #[test]
    fn ordinary_map_owned_any_v2_print_rejects_mismatched_projection_before_llvm_emission() {
        let mut program = canonical_program(&ordinary_map_owned_any_v2_print_native_source(
            "17", "value",
        ));
        let receipt =
            program.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE);
        receipt
            .validate_against_program(&program)
            .expect("the original MIR has the exact DirectPrint receipt");
        let owner = crate::core::NodeId("function:main".into());
        let mut main = program.functions().get(&owner).expect("main MIR").clone();
        let projection = main
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find_map(|instruction| match &mut instruction.kind {
                MirInstructionKind::OrdinaryMapOwnedAnyV2Println { projection, .. } => {
                    Some(projection)
                }
                _ => None,
            })
            .expect("exact V2 print operation");
        *projection = crate::core::NodeId("forged:other-source-projection".into());
        program.replace_function_for_test_only(main);

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_map_any_v2_print_forgery");
        let declarations_before = generator
            .module
            .get_functions()
            .map(|function| function.get_name().to_string_lossy().into_owned())
            .collect::<std::collections::BTreeSet<_>>();
        let error = generator
            .compile_mir_native_with_route_receipt(&program, &receipt)
            .expect_err("a forged source projection must fail native preflight");
        assert!(error.iter().any(|diagnostic| {
            diagnostic.message.contains("println")
                || diagnostic.message.contains("projection")
                || diagnostic.message.contains("receipt")
        }));
        for function in ["main", "mimi_any_payload_v2_unpack"] {
            assert!(
                generator.module.get_function(function).is_none(),
                "the mismatched projection must be rejected before adding LLVM function {function}"
            );
        }
        let declarations_after = generator
            .module
            .get_functions()
            .map(|function| function.get_name().to_string_lossy().into_owned())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            declarations_after, declarations_before,
            "preflight rejection must not add LLVM declarations to CodeGenerator's runtime baseline"
        );
    }

    #[test]
    fn ordinary_map_owned_any_v2_unpack_executes_in_native_and_rejects_wrong_query_type() {
        for (key, expected_output) in [("number", "42\n"), ("absent", "0\n")] {
            let program = canonical_program(&ordinary_map_owned_any_v2_unpack_native_source(key));
            crate::verifier::validate_mir_capabilities(&program)
                .expect("exact V2 GetAny/unpack/Drop receipts pass capability validation");
            crate::codegen::mir::validate_mir_native(&program)
                .expect("native V2 unpack consumes the same exact Checker receipt");
            let main = crate::core::NodeId("function:main".into());
            let reference = MirReferenceInterpreter::new(&program)
                .execute_with_output(&main, &[])
                .expect("Reference V2 unpack");
            assert_eq!(reference.value, MirRuntimeValue::Int(0));
            assert_eq!(reference.output, expected_output);
            let mut vm = BytecodeVM::new(
                compile_mir_program(&program).expect("AST-free bytecode V2 unpack"),
            );
            assert!(matches!(
                vm.run_value_for_component().expect("bytecode V2 unpack"),
                Value::Int(0)
            ));
            assert_eq!(vm.take_stdout(), expected_output);

            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, "mir_native_map_any_v2_unpack");
            generator
                .compile_mir_native_for_component(&program)
                .expect("native V2 unpack emitter");
            generator
                .module
                .verify()
                .expect("native V2 unpack LLVM module verifies");
            let native_ir = generator.module.print_to_string().to_string();
            assert!(
                native_ir.contains("mimi_any_payload_v2_unpack"),
                "native MIR must call the typed V2 payload unpack ABI"
            );
            let native = crate::tests::link_and_observe_canonical_mir(&generator)
                .expect("run the native V2 unpack executable");
            assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
            assert_eq!(native.stdout, expected_output);
            assert_eq!(native.stderr, "");

            if std::process::Command::new("valgrind")
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
            {
                let memcheck = crate::tests::link_and_observe_canonical_mir_with_config(
                    &generator,
                    &crate::tests::E2EConfig {
                        use_valgrind: true,
                        valgrind_args: vec![
                            "--tool=memcheck".into(),
                            "--leak-check=full".into(),
                            "--show-leak-kinds=all".into(),
                            "--errors-for-leak-kinds=definite,indirect,possible".into(),
                            "--error-exitcode=97".into(),
                        ],
                        ..crate::tests::E2EConfig::default()
                    },
                )
                .expect("strict Memcheck for the native V2 unpack success path");
                assert_clean_map_any_memcheck(&memcheck, 0, expected_output, None);
            }
        }

        assert_checker_rejects_wrong_v2_unpack_source(
            &ordinary_map_owned_any_v2_unpack_native_source("label"),
            "a queried Label payload cannot be unpacked as i32",
        );
    }

    fn ordinary_map_owned_any_v2_string_unpack_native_source(key: &str) -> String {
        format!(
            r#"
            type Label {{ text: string }}
            func main() -> i32 {{
                let source = "detached String payload"
                let label = Label {{ text: "record marker" }}
                let empty = map_new()
                let stored = map_set(empty, "present", source)
                drop(source)
                let with_label = map_set(stored, "label", label)
                let mixed = map_set(with_label, "number", 42)
                let pair = map_get(mixed, "{key}")
                let value = unpack_string(pair.1)
                drop(pair)
                drop(empty)
                drop(stored)
                drop(with_label)
                drop(mixed)
                let matches = value == "detached String payload"
                println(value)
                println(matches)
                drop(value)
                drop(label)
                0
            }}
            "#
        )
    }

    fn ordinary_map_owned_any_v2_record_unpack_native_source(key: &str) -> String {
        format!(
            r#"
            type Label {{ text: string }}
            func main() -> i32 {{
                let label = Label {{ text: "native record unpack fixture" }}
                let empty = map_new()
                let labels = map_set(empty, "label", label)
                let mixed = map_set(labels, "number", 73)
                let pair = map_get(mixed, "{key}")
                let value = pair.1 as Label
                drop(pair)
                drop(empty)
                drop(labels)
                drop(mixed)
                drop(label)
                let matches = value.text == "native record unpack fixture"
                println(matches)
                0
            }}
            "#
        )
    }

    #[test]
    fn ordinary_map_owned_any_v2_record_unpack_returns_owned_label_and_rejects_wrong_query() {
        let hit = canonical_program(&ordinary_map_owned_any_v2_record_unpack_native_source(
            "label",
        ));
        crate::codegen::mir::validate_mir_native(&hit)
            .expect("native Label unpack consumes the exact Core result and selection receipts");

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_map_any_v2_record_unpack");
        generator
            .compile_mir_native_for_component(&hit)
            .expect("native V2 Label unpack emitter");
        generator
            .module
            .verify()
            .expect("native V2 Label unpack LLVM module verifies");
        let native_ir = generator.module.print_to_string().to_string();
        assert!(native_ir.contains("mimi_any_payload_v2_unpack"));
        let copy_string_call = native_ir
            .find("call i32 @mimi_mir_ordinary_map_owned_any_v2_unpack_copy_string")
            .expect("native V2 Label unpack uses the cap-free owned String bridge");
        // LLVM omits the requested instruction name for a void-returning call,
        // so identify the generated cleanup by its drop-glue callee instead.
        let temporary_drop = native_ir[copy_string_call..]
            .find("call void @mimi_any_payload_v2_drop_glue__")
            .map(|offset| copy_string_call + offset)
            .expect("native V2 Label callback payload is released through its exact drop glue");
        let status_branch = native_ir[temporary_drop..]
            .find("mir_map_any_v2_unpack_string_copy_ok")
            .map(|offset| temporary_drop + offset)
            .expect("native V2 Label unpack checks copy status only after releasing the callback payload");
        assert!(
            copy_string_call < temporary_drop && temporary_drop < status_branch,
            "String copy result is published only after the callback payload is dropped on both status paths"
        );
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("run the native V2 Label unpack executable");
        assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
        assert_eq!(native.stdout, "true\n");
        assert_eq!(native.stderr, "");

        let hit_memcheck = crate::tests::link_and_observe_canonical_mir_with_config(
            &generator,
            &crate::tests::E2EConfig {
                use_valgrind: true,
                valgrind_args: vec![
                    "--tool=memcheck".into(),
                    "--leak-check=full".into(),
                    "--show-leak-kinds=all".into(),
                    "--errors-for-leak-kinds=definite,indirect,possible".into(),
                    "--error-exitcode=97".into(),
                ],
                ..crate::tests::E2EConfig::default()
            },
        )
        .expect("run native V2 Label unpack under strict Memcheck");
        assert_clean_map_any_memcheck(&hit_memcheck, 0, "true\n", None);

        let _main = crate::core::NodeId("function:main".into());
        for key in ["absent", "number"] {
            assert_checker_rejects_wrong_v2_unpack_source(
                &ordinary_map_owned_any_v2_record_unpack_native_source(key),
                "a Label unpack requires the queried key to select a Label payload",
            );
        }
    }

    #[test]
    fn ordinary_map_owned_any_v2_string_unpack_clones_on_selected_key_and_rejects_wrong_query() {
        let hit = canonical_program(&ordinary_map_owned_any_v2_string_unpack_native_source(
            "present",
        ));
        crate::verifier::validate_mir_capabilities(&hit)
            .expect("exact String V2 GetAny/unpack/Drop receipts pass capability validation");
        crate::codegen::mir::validate_mir_native(&hit)
            .expect("native V2 String unpack consumes the exact Checker receipt");

        let main = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&hit)
            .execute_with_output(&main, &[])
            .expect("Reference V2 String unpack");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(reference.output, "detached String payload\ntrue\n");

        let mut vm =
            BytecodeVM::new(compile_mir_program(&hit).expect("AST-free bytecode V2 String unpack"));
        assert!(matches!(
            vm.run_value_for_component()
                .expect("bytecode V2 String unpack"),
            Value::Int(0)
        ));
        assert_eq!(vm.take_stdout(), "detached String payload\ntrue\n");

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_map_any_v2_string_unpack");
        generator
            .compile_mir_native_for_component(&hit)
            .expect("native V2 String unpack emitter");
        generator
            .module
            .verify()
            .expect("native V2 String unpack LLVM module verifies");
        let native_ir = generator.module.print_to_string().to_string();
        assert!(native_ir.contains("mimi_any_payload_v2_unpack"));
        assert!(native_ir.contains("mimi_mir_ordinary_map_domain_own_string"));
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("run native V2 String unpack executable");
        assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
        assert_eq!(native.stdout, "detached String payload\ntrue\n");
        assert_eq!(native.stderr, "");

        let hit_memcheck = crate::tests::link_and_observe_canonical_mir_with_config(
            &generator,
            &crate::tests::E2EConfig {
                use_valgrind: true,
                valgrind_args: vec![
                    "--tool=memcheck".into(),
                    "--leak-check=full".into(),
                    "--show-leak-kinds=all".into(),
                    "--errors-for-leak-kinds=definite,indirect,possible".into(),
                    "--error-exitcode=97".into(),
                ],
                ..crate::tests::E2EConfig::default()
            },
        )
        .expect("strict Memcheck for native V2 String unpack success");
        assert_clean_map_any_memcheck(&hit_memcheck, 0, "detached String payload\ntrue\n", None);

        for key in ["absent", "number"] {
            assert_checker_rejects_wrong_v2_unpack_source(
                &ordinary_map_owned_any_v2_string_unpack_native_source(key),
                "a String unpack requires the queried key to select a String payload",
            );
        }
    }

    #[test]
    fn receipt_backed_map_any_owner_v1_is_one_mir_across_reference_bytecode_native_and_verifier() {
        let program = canonical_program(MAP_ANY_OWNER_V1_NATIVE_SOURCE);
        crate::verifier::validate_mir_capabilities(&program)
            .expect("whole-program verifier admits the exact MapAnyOwner v1 profile");
        crate::codegen::mir::validate_mir_native(&program)
            .expect("native admission revalidates the independent MapAnyOwner v1 receipt");

        let main = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&main, &[])
            .expect("reference MapAnyOwner v1 execution");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(reference.output, "2\n1\n0\n1\n");

        let mut vm = BytecodeVM::new(
            compile_mir_program(&program).expect("AST-free MapAnyOwner v1 bytecode compilation"),
        );
        assert!(matches!(
            vm.run_value().expect("bytecode MapAnyOwner v1 execution"),
            Value::Int(0)
        ));
        assert_eq!(vm.take_stdout(), "2\n1\n0\n1\n");

        let verification = crate::verifier::verify_mir(&program, "map-any-owner-v1-native".into())
            .expect("public MIR verifier consumes the same MapAnyOwner v1 program");
        for name in ["main", "map_size_for", "checked_map_size"] {
            let result = verification
                .iter()
                .find(|result| result.func_name.ends_with(name))
                .expect("positive MapAnyOwner v1 contract result");
            assert_eq!(
                result.status,
                crate::verifier::VerifStatus::Proven,
                "{}: {}",
                name,
                result.message
            );
        }
        let wrong = verification
            .iter()
            .find(|result| result.func_name.ends_with("wrong_map_size"))
            .expect("negative MapAnyOwner v1 contract result");
        assert_eq!(wrong.status, crate::verifier::VerifStatus::Disproven);

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_map_any_owner_v1_i32");
        generator
            .compile_mir_native(&program)
            .expect("receipt-backed MapAnyOwner v1 native lowering");
        generator
            .module
            .verify()
            .expect("native MapAnyOwner v1 module verifies");
        let native_ir = generator.module.print_to_string().to_string();
        assert!(
            native_ir.contains("mir_map_root_new_nonzero = icmp ne i64"),
            "opaque MapAnyOwner handles may be negative when their generation high bit is set"
        );
        assert!(
            native_ir.contains("mir_map_root_handle_nonzero = icmp ne i64"),
            "nonzero is the MapAnyOwner handle validity sentinel; sign is not"
        );
        for runtime in [
            "mimi_mir_map_any_owner_v1_new",
            "mimi_mir_map_any_owner_v1_set_i32",
            "mimi_mir_map_any_owner_v1_remove",
            "mimi_mir_map_any_owner_v1_size",
            "mimi_mir_map_any_owner_v1_drop",
        ] {
            assert!(
                generator.module.get_function(runtime).is_some(),
                "native MapAnyOwner v1 lowering must declare {runtime}"
            );
        }
        assert!(native_ir.contains("call i64 @mimi_mir_map_any_owner_v1_set_i32"));
        assert!(!native_ir.contains("call i64 @mimi_mir_map_root_set"));
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("same MIR native MapAnyOwner v1 process");
        assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
        assert_eq!(native.stdout, "2\n1\n0\n1\n");
        assert_eq!(native.stderr, "");

        if std::process::Command::new("valgrind")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            let memcheck = crate::tests::link_and_observe_canonical_mir_with_config(
                &generator,
                &crate::tests::E2EConfig {
                    use_valgrind: true,
                    valgrind_args: vec![
                        "--tool=memcheck".into(),
                        "--leak-check=full".into(),
                        "--show-leak-kinds=all".into(),
                        // The process-global AnyOwner registry retains its
                        // descriptor table until process exit. That reachable
                        // bookkeeping is not lost memory; the map roots and
                        // payloads themselves must still be fully reclaimed.
                        "--errors-for-leak-kinds=definite,indirect,possible".into(),
                        "--error-exitcode=99".into(),
                    ],
                    ..crate::tests::E2EConfig::default()
                },
            )
            .expect("production runtime link under Valgrind Memcheck");
            if std::env::var_os("MIMI_MAP_ANY_OWNER_V1_VALGRIND_TRACE").is_some() {
                eprintln!(
                    "Checker-receipted MapAnyOwner v1 lifecycle Memcheck report:\n{}",
                    memcheck.stderr
                );
            }
            assert_clean_map_any_memcheck(&memcheck, 0, "2\n1\n0\n1\n", None);
        }
    }

    #[test]
    fn ordinary_map_owner_v1_lifecycle_is_one_mir_across_reference_bytecode_native_and_verifier() {
        let program = canonical_program(
            r#"
func key_from_value(key: string) -> string { key }

func checked_source_preserving(key: string) -> i32 {
    ensures: result == 1
    let source = map_new()
    let inserted = map_set(source, key, 1)
    let source_size = map_size(source)
    let inserted_size = map_size(inserted)
    drop(source)
    drop(inserted)
    source_size + inserted_size
}

func checked_remove_preserving(key: string) -> i32 {
    ensures: result == 1
    let source = map_new()
    let inserted = map_set(source, key, 1)
    let removed = map_remove(inserted, key)
    let inserted_size = map_size(inserted)
    let removed_size = map_size(removed)
    drop(source)
    drop(inserted)
    drop(removed)
    inserted_size + removed_size
}

func checked_dynamic_contains(key: string) -> bool {
    ensures: result == true
    let source = map_new()
    let inserted = map_set(source, key, 1)
    let present = has_key(inserted, key)
    drop(source)
    drop(inserted)
    present
}

func checked_mixed_payload() -> i32 {
    ensures: result == 2
    let source = map_new()
    let numbers = map_set(source, "number", 42)
    let mixed = map_set(numbers, "label", "forty-two")
    let size = map_size(mixed)
    drop(source)
    drop(numbers)
    drop(mixed)
    size
}

func main() -> i32 {
    let source = map_new()
    let number_key = key_from_value("number")
    let numbers = map_set(source, number_key, 42)
    let label_key = key_from_value("label")
    let mixed = map_set(numbers, label_key, "forty-two")

    let source_size = map_size(source)
    let numbers_has_number = has_key(numbers, number_key)
    let numbers_has_label = has_key(numbers, label_key)
    let mixed_has_number = has_key(mixed, number_key)
    let mixed_has_label = has_key(mixed, label_key)
    let mixed_size = map_size(mixed)

    let removed = map_remove(mixed, number_key)
    let mixed_after_remove_has_number = has_key(mixed, number_key)
    let removed_has_number = has_key(removed, number_key)
    let removed_has_label = has_key(removed, label_key)
    let removed_size = map_size(removed)

    println(source_size)
    println(numbers_has_number)
    println(numbers_has_label)
    println(mixed_has_number)
    println(mixed_has_label)
    println(mixed_size)
    println(mixed_after_remove_has_number)
    println(removed_has_number)
    println(removed_has_label)
    println(removed_size)

    drop(source)
    drop(numbers)
    drop(mixed)
    drop(removed)
    0
}
"#,
        );
        assert_eq!(
            program
                .route_receipt(crate::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_I32_STRING_V1_ISLAND)
                .profile,
            crate::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_I32_STRING_V1_ISLAND
        );
        crate::verifier::validate_mir_capabilities(&program)
            .expect("verifier admits the independent ordinary Map lifecycle receipt");
        crate::codegen::mir::validate_mir_native(&program)
            .expect("native admission accepts the ordinary Map lifecycle profile");

        let main = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&main, &[])
            .expect("reference ordinary Map lifecycle execution");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(
            reference.output,
            "0\ntrue\nfalse\ntrue\ntrue\n2\ntrue\nfalse\ntrue\n1\n"
        );

        let mut vm = BytecodeVM::new(
            compile_mir_program(&program).expect("AST-free ordinary Map lifecycle bytecode"),
        );
        assert_eq!(
            vm.run_value()
                .expect("bytecode ordinary Map lifecycle execution"),
            Value::Int(0)
        );
        assert_eq!(vm.take_stdout(), reference.output);

        let verification =
            crate::verifier::verify_mir(&program, "ordinary-map-owner-v1-lifecycle".into())
                .expect("public verifier consumes the same ordinary Map lifecycle MIR");
        for name in [
            "checked_source_preserving",
            "checked_remove_preserving",
            "checked_dynamic_contains",
            "checked_mixed_payload",
        ] {
            let result = verification
                .iter()
                .find(|result| result.func_name.ends_with(name))
                .unwrap_or_else(|| panic!("missing ordinary Map lifecycle proof for {name}"));
            assert_eq!(
                result.status,
                crate::verifier::VerifStatus::Disproven,
                "{}: {}",
                name,
                result.message
            );
            assert!(
                result
                    .message
                    .contains("can reach trap 'E0800' under requires"),
                "{name}: native Map capacity failure must remain a reachable trap: {}",
                result.message
            );
            assert!(
                result.diagnostic.as_ref().is_some_and(|diagnostic| {
                    diagnostic
                        .message
                        .contains("can reach trap 'E0800' under requires")
                }),
                "{name}: the E0800 trap must retain its diagnostic"
            );
        }
        assert!(!verification.iter().any(|result| {
            [
                "checked_source_preserving",
                "checked_remove_preserving",
                "checked_dynamic_contains",
                "checked_mixed_payload",
            ]
            .iter()
            .any(|name| result.func_name.ends_with(name))
                && result.status == crate::verifier::VerifStatus::Proven
        }));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_ordinary_map_owner_v1");
        generator
            .compile_mir_native(&program)
            .expect("receipt-backed ordinary Map lifecycle native lowering");
        generator
            .module
            .verify()
            .expect("ordinary Map lifecycle native module verifies");
        let native_ir = generator.module.print_to_string().to_string();
        let final_drop_calls = native_ir
            .matches("call void @mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_drop")
            .count();
        let normal_drop_operations = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                matches!(
                    instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { .. }
                )
            })
            .count();
        assert_eq!(
            normal_drop_operations, 14,
            "the normal MIR graph settles every persistent root once"
        );
        assert!(
            final_drop_calls >= normal_drop_operations,
            "native lowering includes every normal root Drop and additional trap-only cleanup calls"
        );
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("same MIR native ordinary Map lifecycle process");
        assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
        assert_eq!(native.stdout, reference.output);
        assert_eq!(native.stderr, "");

        if std::process::Command::new("valgrind")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            let memcheck = crate::tests::link_and_observe_canonical_mir_with_config(
                &generator,
                &crate::tests::E2EConfig {
                    use_valgrind: true,
                    valgrind_args: vec![
                        "--tool=memcheck".into(),
                        "--leak-check=full".into(),
                        "--show-leak-kinds=all".into(),
                        // Shared owner registries remain reachable until process exit.
                        // Fail on invalid access and actual loss, while reporting
                        // reachable bookkeeping separately.
                        "--errors-for-leak-kinds=definite,indirect,possible".into(),
                        "--error-exitcode=99".into(),
                    ],
                    ..crate::tests::E2EConfig::default()
                },
            )
            .expect("production ordinary Map lifecycle under strict Memcheck");
            assert_clean_map_any_memcheck(&memcheck, 0, &reference.output, None);
        }
    }

    #[test]
    fn receipt_backed_static_map_any_owner_v1_contains_matches_same_mir_consumers() {
        let program = canonical_program(include_str!(
            "../../../tests/real_world/map_root_static_contains.mimi"
        ));
        crate::verifier::validate_mir_capabilities(&program)
            .expect("whole-program verifier admits static MapAnyOwner v1 Contains receipts");
        crate::codegen::mir::validate_mir_native(&program)
            .expect("native validator admits static MapAnyOwner v1 Contains receipts");

        let main = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&main, &[])
            .expect("reference executes the same MapAnyOwner v1 MIR");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(
            reference.output,
            "true\nfalse\ntrue\nfalse\ntrue\n1\ntrue\n"
        );

        let mut vm = BytecodeVM::new(
            compile_mir_program(&program).expect("AST-free bytecode consumes the same MIR"),
        );
        assert_eq!(vm.run_value().expect("bytecode execution"), Value::Int(0));
        assert_eq!(vm.take_stdout(), reference.output);

        let verification =
            crate::verifier::verify_mir(&program, "map-any-owner-v1-static-contains".into())
                .expect("public verifier consumes the same receipted MIR");
        for (name, expected) in [
            (
                "contains_inserted_key",
                crate::verifier::VerifStatus::Proven,
            ),
            ("contains_missing_key", crate::verifier::VerifStatus::Proven),
            (
                "contains_string_payload",
                crate::verifier::VerifStatus::Proven,
            ),
            (
                "wrong_contains_contract",
                crate::verifier::VerifStatus::Disproven,
            ),
        ] {
            let result = verification
                .iter()
                .find(|result| result.func_name.ends_with(name))
                .expect("MapAnyOwner v1 Contains contract result");
            assert_eq!(result.status, expected, "{}: {}", name, result.message);
        }

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_static_map_any_owner_v1_contains");
        generator
            .compile_mir_native(&program)
            .expect("native emitter consumes the same independently receipted MIR");
        generator
            .module
            .verify()
            .expect("native contains module verifies");
        assert!(generator
            .module
            .get_function("mimi_mir_map_any_owner_v1_contains")
            .is_some());
        let native_ir = generator.module.print_to_string().to_string();
        assert!(native_ir.contains("call i32 @mimi_mir_map_any_owner_v1_contains"));
        assert!(!native_ir.contains("call i32 @mimi_mir_map_root_contains"));
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("native execution of the same MapAnyOwner v1 MIR");
        assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
        assert_eq!(native.stdout, reference.output);
        assert_eq!(native.stderr, "");

        if std::process::Command::new("valgrind")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            let memcheck = crate::tests::link_and_observe_canonical_mir_with_config(
                &generator,
                &crate::tests::E2EConfig {
                    use_valgrind: true,
                    valgrind_args: vec![
                        "--tool=memcheck".into(),
                        "--leak-check=full".into(),
                        "--show-leak-kinds=all".into(),
                        "--errors-for-leak-kinds=definite,indirect,possible".into(),
                        "--error-exitcode=99".into(),
                    ],
                    ..crate::tests::E2EConfig::default()
                },
            )
            .expect("production MapAnyOwner v1 Contains path under strict Memcheck");
            assert_clean_map_any_memcheck(&memcheck, 0, &reference.output, None);
        }

        let (_owner, function) = program
            .functions()
            .iter()
            .find(|(owner, _)| owner.0.ends_with("main"))
            .expect("main MIR");
        let canonical = function.canonical_text();
        assert!(canonical.contains("checker_map_any_owner["));
        assert!(canonical.contains("map_root_contains"));
    }

    #[test]
    fn receipt_backed_string_map_any_owner_v1_matches_all_consumers_and_reclaims_payloads() {
        let program = canonical_program(MAP_ANY_OWNER_V1_STRING_NATIVE_SOURCE);
        let verifier_receipt = program.route_receipt(crate::core::mir::MAP_ANY_OWNER_V1_ISLAND);
        crate::verifier::validate_mir_capabilities(&program)
            .expect("verifier admits the exact typed String MapAnyOwner v1 profile");
        crate::codegen::mir::validate_mir_native(&program)
            .expect("native validator admits Checker-receipted i32/String MapAnyOwner payloads");

        let main = program
            .functions()
            .keys()
            .find(|owner| owner.0.ends_with("main"))
            .expect("String MapAnyOwner v1 main")
            .clone();
        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&main, &[])
            .expect("reference String MapAnyOwner v1 execution");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(reference.output, "1\n1\n1\n");

        let mut vm = BytecodeVM::new(
            compile_mir_program(&program).expect("AST-free String MapAnyOwner v1 bytecode"),
        );
        assert_eq!(
            vm.run_value()
                .expect("bytecode String MapAnyOwner v1 execution"),
            Value::Int(0)
        );
        assert_eq!(vm.take_stdout(), reference.output);

        let verification = crate::verifier::verify_mir(&program, "map-any-owner-v1-string".into())
            .expect("public MIR verifier consumes the same String MapAnyOwner v1 graph");
        let proven = verification
            .iter()
            .find(|result| result.func_name.ends_with("map_size_for_string"))
            .expect("positive String MapAnyOwner v1 contract");
        assert_eq!(
            proven.status,
            crate::verifier::VerifStatus::Proven,
            "{}",
            proven.message
        );
        let remove_proven = verification
            .iter()
            .find(|result| result.func_name.ends_with("map_remove_string_size"))
            .expect("positive String MapAnyOwner v1 Remove contract");
        assert_eq!(
            remove_proven.status,
            crate::verifier::VerifStatus::Proven,
            "{}",
            remove_proven.message
        );
        let receipt_verification = crate::verifier::verify_mir_with_route_receipt(
            &program,
            &verifier_receipt,
            "map-any-owner-v1-string-route-receipt".into(),
        )
        .expect("public receipt-bound verifier consumes the same String MapAnyOwner v1 graph");
        assert!(receipt_verification.iter().any(|result| {
            result.func_name.ends_with("map_size_for_string")
                && result.status == crate::verifier::VerifStatus::Proven
        }));
        let disproven = verification
            .iter()
            .find(|result| result.func_name.ends_with("wrong_string_map_size"))
            .expect("negative String MapAnyOwner v1 contract");
        assert_eq!(disproven.status, crate::verifier::VerifStatus::Disproven);
        let remove_disproven = verification
            .iter()
            .find(|result| result.func_name.ends_with("wrong_map_remove_string_size"))
            .expect("negative String MapAnyOwner v1 Remove contract");
        assert_eq!(
            remove_disproven.status,
            crate::verifier::VerifStatus::Disproven,
            "{}",
            remove_disproven.message
        );

        let mut missing_checker_receipt = program.clone();
        let mut main_without_checker_receipt = missing_checker_receipt
            .functions()
            .get(&main)
            .expect("String MapAnyOwner v1 main")
            .clone();
        main_without_checker_receipt
            .ownership
            .clear_checker_map_any_owner_actions_for_test_only();
        missing_checker_receipt.replace_function_for_test_only(main_without_checker_receipt);
        let missing_receipt_construction =
            crate::core::mir::reference::MirProgram::with_type_catalog(
                missing_checker_receipt.functions().clone(),
                missing_checker_receipt.type_catalog().clone(),
            )
            .expect_err("MapAnyOwner v1 TypeDesc must not outlive its independent Checker receipt");
        assert!(missing_receipt_construction.iter().any(|error| {
            error
                .message
                .contains("MapAnyOwner v1 TypeDesc has no independent Checker receipt")
        }));
        assert!(
            crate::codegen::mir::validate_mir_native(&missing_checker_receipt)
                .expect_err("native admission must reject a missing MapAnyOwner v1 receipt")
                .iter()
                .any(|error| error.to_string().contains("MapAnyOwner v1"))
        );
        assert!(
            crate::verifier::validate_mir_capabilities(&missing_checker_receipt)
                .expect_err("verifier admission must reject a missing MapAnyOwner v1 receipt")
                .iter()
                .any(|error| error.contains("MapAnyOwner v1"))
        );
        assert!(crate::verifier::verify_mir_with_route_receipt(
            &missing_checker_receipt,
            &verifier_receipt,
            "map-any-owner-v1-string-missing-checker-receipt".into(),
        )
        .expect_err("a valid v1 route receipt must not replay without its Checker ledger")
        .contains("canonical route receipt rejected"));

        let mut forged_remove = program.clone();
        let mut main_with_forged_remove = forged_remove
            .functions()
            .get(&main)
            .expect("String MapAnyOwner v1 main")
            .clone();
        let remove = main_with_forged_remove
            .blocks
            .values_mut()
            .flat_map(|block| block.instructions.iter_mut())
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::MapRootRemove { .. }
                )
            })
            .expect("MapAnyOwner v1 Remove instruction");
        let crate::core::mir::MirInstructionKind::MapRootRemove { key, .. } = &mut remove.kind
        else {
            unreachable!("located Remove instruction")
        };
        *key = "forged-key".into();
        forged_remove.replace_function_for_test_only(main_with_forged_remove);
        let forged_remove_construction =
            crate::core::mir::reference::MirProgram::with_type_catalog(
                forged_remove.functions().clone(),
                forged_remove.type_catalog().clone(),
            )
            .expect_err("MIR constructor must reject a forged MapAnyOwner v1 Remove key");
        assert!(forged_remove_construction
            .iter()
            .any(|error| error.message.contains("MapRoot operation disagrees")));
        let forged_remove_native_errors = crate::codegen::mir::validate_mir_native(&forged_remove)
            .expect_err("native admission must reject the forged MapAnyOwner v1 Remove key");
        assert!(
            forged_remove_native_errors
                .iter()
                .any(|error| error.to_string().contains("MapAnyOwner v1")),
            "native validator errors: {forged_remove_native_errors:#?}"
        );
        let forged_remove_verifier_errors =
            crate::verifier::validate_mir_capabilities(&forged_remove)
                .expect_err("verifier capability gate must reject the forged MapAnyOwner v1 key");
        assert!(
            forged_remove_verifier_errors.iter().any(|error| {
                error.contains("canonical MIR structural preflight rejected")
                    && error.contains(
                        "MapRoot operation disagrees with its private Checker root/type/key receipt",
                    )
            }),
            "verifier must reject the key/receipt disagreement before capability admission: {forged_remove_verifier_errors:#?}"
        );
        assert!(crate::verifier::verify_mir_with_route_receipt(
            &forged_remove,
            &verifier_receipt,
            "map-any-owner-v1-string-forged-remove-key".into(),
        )
        .expect_err("valid v1 route receipt cannot authorize a forged Remove key")
        .contains("canonical route receipt rejected"));

        let mut missing_string_drop = program.clone();
        let mut main_without_string_drop = missing_string_drop
            .functions()
            .get(&main)
            .expect("String MapAnyOwner v1 main")
            .clone();
        let block = main_without_string_drop
            .blocks
            .values_mut()
            .find(|block| {
                block.instructions.windows(2).any(|pair| {
                    matches!(
                        &pair[0].kind,
                        crate::core::mir::MirInstructionKind::MapRootSet { value, .. }
                            if matches!(
                                &pair[1].kind,
                                crate::core::mir::MirInstructionKind::Drop { value: dropped }
                                    if dropped == value
                            )
                    )
                })
            })
            .expect("MapAnyOwner v1 Set and its owned String temporary Drop");
        let set_index = block
            .instructions
            .windows(2)
            .position(|pair| {
                matches!(
                    &pair[0].kind,
                    crate::core::mir::MirInstructionKind::MapRootSet { value, .. }
                        if matches!(
                            &pair[1].kind,
                            crate::core::mir::MirInstructionKind::Drop { value: dropped }
                                if dropped == value
                        )
                )
            })
            .expect("MapAnyOwner v1 Set temporary Drop pair");
        block.instructions.remove(set_index + 1);
        missing_string_drop.replace_function_for_test_only(main_without_string_drop);
        let missing_drop_construction = crate::core::mir::reference::MirProgram::with_type_catalog(
            missing_string_drop.functions().clone(),
            missing_string_drop.type_catalog().clone(),
        )
        .expect_err("the public MIR constructor must reject a missing MapAnyOwner v1 String Drop");
        assert!(missing_drop_construction.iter().any(|error| {
            error
                .message
                .contains("MapRoot operation disagrees with its private Checker")
        }));
        let missing_drop_route = crate::verifier::verify_mir_with_route_receipt(
            &missing_string_drop,
            &verifier_receipt,
            "map-any-owner-v1-string-missing-drop".into(),
        )
        .expect_err("a valid v1 route receipt must not replay against MIR missing String Drop");
        assert!(missing_drop_route.contains("canonical route receipt rejected"));
        assert!(
            crate::codegen::mir::validate_mir_native(&missing_string_drop)
                .expect_err("omitted MapAnyOwner v1 String Drop must fail native admission")
                .iter()
                .any(|error| error.to_string().contains("MapAnyOwner v1"))
        );
        let missing_drop_verifier_errors =
            crate::verifier::validate_mir_capabilities(&missing_string_drop)
                .expect_err("omitted MapAnyOwner v1 String Drop must fail verifier admission");
        assert!(
            missing_drop_verifier_errors.iter().any(|error| {
                error.contains("canonical MIR structural preflight rejected")
                    && error.contains(
                        "MapRoot operation disagrees with its private Checker root/type/key receipt",
                    )
            }),
            "verifier must reject the missing String payload Drop through its Checker receipt: {missing_drop_verifier_errors:#?}"
        );

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_string_map_any_owner_v1");
        generator
            .compile_mir_native(&program)
            .expect("native emitter consumes the same receipt-backed String MapAnyOwner v1 MIR");
        generator
            .module
            .verify()
            .expect("native String MapAnyOwner v1 module verifies");
        assert!(generator
            .module
            .get_function("mimi_mir_map_any_owner_v1_set_string")
            .is_some());
        let native_ir = generator.module.print_to_string().to_string();
        assert!(native_ir.contains("call i64 @mimi_mir_map_any_owner_v1_set_string"));
        assert!(!native_ir.contains("call i64 @mimi_mir_map_root_set_string"));
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("production native String MapAnyOwner v1 process");
        assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
        assert_eq!(native.stdout, reference.output);
        assert_eq!(native.stderr, "");

        if std::process::Command::new("valgrind")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            let memcheck = crate::tests::link_and_observe_canonical_mir_with_config(
                &generator,
                &crate::tests::E2EConfig {
                    use_valgrind: true,
                    valgrind_args: vec![
                        "--tool=memcheck".into(),
                        "--leak-check=full".into(),
                        "--show-leak-kinds=all".into(),
                        "--errors-for-leak-kinds=definite,indirect,possible".into(),
                        "--error-exitcode=99".into(),
                    ],
                    ..crate::tests::E2EConfig::default()
                },
            )
            .expect("production String MapAnyOwner v1 lifecycle under Memcheck");
            assert_clean_map_any_memcheck(&memcheck, 0, &reference.output, None);
        }
    }

    #[test]
    fn map_any_owner_v1_is_one_mir_across_reference_bytecode_native_verifier_and_memcheck() {
        let program = canonical_program(MAP_ANY_OWNER_V1_PROOF_STRING_NATIVE_SOURCE);
        let verifier_receipt = program.route_receipt(crate::core::mir::MAP_ANY_OWNER_V1_ISLAND);
        crate::verifier::validate_mir_capabilities(&program)
            .expect("whole-program verifier admits the exact MapAnyOwner v1 profile");
        crate::codegen::mir::validate_mir_native(&program)
            .expect("native validator admits the receipted String owner profile");

        let main = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&main, &[])
            .expect("reference executes the same MapAnyOwner v1 MIR");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(reference.output, "true\nfalse\n0\n");

        let mut vm = BytecodeVM::new(
            compile_mir_program(&program).expect("AST-free bytecode consumes the same MIR"),
        );
        assert_eq!(vm.run_value().expect("bytecode execution"), Value::Int(0));
        assert_eq!(vm.take_stdout(), reference.output);

        let verification = crate::verifier::verify_mir(&program, "map-any-owner-v1-string".into())
            .expect("public verifier consumes the same typed-owner MIR");
        for (name, expected) in [
            (
                "checked_string_owner_size",
                crate::verifier::VerifStatus::Proven,
            ),
            (
                "false_string_owner_size",
                crate::verifier::VerifStatus::Disproven,
            ),
        ] {
            let result = verification
                .iter()
                .find(|result| result.func_name.ends_with(name))
                .unwrap_or_else(|| panic!("missing MapAnyOwner v1 proof for {name}"));
            assert_eq!(result.status, expected, "{}: {}", name, result.message);
            assert_eq!(
                result
                    .artifact
                    .as_ref()
                    .map(|artifact| artifact.engine.as_str()),
                Some(crate::verifier::ProofArtifact::ENGINE_MIR)
            );
        }
        let receipt_verification = crate::verifier::verify_mir_with_route_receipt(
            &program,
            &verifier_receipt,
            "map-any-owner-v1-string-route-receipt".into(),
        )
        .expect("route-bound verifier consumes the same Checker receipt");
        assert!(receipt_verification.iter().any(|result| {
            result.func_name.ends_with("checked_string_owner_size")
                && result.status == crate::verifier::VerifStatus::Proven
        }));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_map_any_owner_v1_string");
        generator
            .compile_mir_native(&program)
            .expect("native emitter consumes the same receipted MIR");
        generator
            .module
            .verify()
            .expect("native MapAnyOwner v1 module verifies");
        for runtime in [
            "mimi_mir_map_any_owner_v1_new",
            "mimi_mir_map_any_owner_v1_set_string",
            "mimi_mir_map_any_owner_v1_remove",
            "mimi_mir_map_any_owner_v1_size",
            "mimi_mir_map_any_owner_v1_contains",
            "mimi_mir_map_any_owner_v1_drop",
        ] {
            assert!(
                generator.module.get_function(runtime).is_some(),
                "native V1 lowering must declare {runtime}"
            );
        }
        let native_ir = generator.module.print_to_string().to_string();
        assert!(native_ir.contains("call i64 @mimi_mir_map_any_owner_v1_set_string"));
        assert!(!native_ir.contains("call i64 @mimi_mir_map_root_set_string"));
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("same MIR native MapAnyOwner v1 process");
        assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
        assert_eq!(native.stdout, reference.output);
        assert_eq!(native.stderr, "");

        if std::process::Command::new("valgrind")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            let memcheck = crate::tests::link_and_observe_canonical_mir_with_config(
                &generator,
                &crate::tests::E2EConfig {
                    use_valgrind: true,
                    valgrind_args: vec![
                        "--tool=memcheck".into(),
                        "--leak-check=full".into(),
                        "--show-leak-kinds=all".into(),
                        // The process-global owner registry and its descriptor
                        // table remain reachable until process exit. Treat
                        // actual losses as errors and report reachable state
                        // separately; runtime lifecycle tests assert that all
                        // Map-owned tokens return to their live-count baseline.
                        "--errors-for-leak-kinds=definite,indirect,possible".into(),
                        "--error-exitcode=99".into(),
                    ],
                    ..crate::tests::E2EConfig::default()
                },
            )
            .expect("production MapAnyOwner v1 runtime under strict Memcheck");
            assert_clean_map_any_memcheck(&memcheck, 0, &reference.output, None);
        }
    }

    #[test]
    fn map_any_owner_v1_mir_rejects_a_missing_checker_receipt() {
        let program = canonical_program(MAP_ANY_OWNER_V1_STRING_NATIVE_SOURCE);
        let main = crate::core::NodeId("function:main".into());
        let mut function = program.functions().get(&main).expect("main MIR").clone();
        function
            .ownership
            .clear_checker_map_any_owner_actions_for_test_only();
        let errors = function
            .validate()
            .expect_err("v1 synthetic owner types cannot outlive their private Checker ledger");
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("MapAnyOwner v1 TypeDesc has no independent Checker receipt")
        }));
    }

    #[test]
    fn native_map_any_owner_v1_admission_rejects_forged_and_missing_receipts_before_llvm() {
        let program = canonical_program(MAP_ANY_OWNER_V1_NATIVE_SOURCE);
        let owner = crate::core::NodeId("function:main".into());

        let mut forged = program.clone();
        let mut function = forged
            .functions()
            .get(&owner)
            .expect("MapAnyOwner v1 main")
            .clone();
        let set = function
            .blocks
            .values_mut()
            .flat_map(|block| block.instructions.iter_mut())
            .find(|instruction| matches!(&instruction.kind, MirInstructionKind::MapRootSet { .. }))
            .expect("MapAnyOwner v1 Set instruction");
        let MirInstructionKind::MapRootSet { key, .. } = &mut set.kind else {
            unreachable!("located Set instruction")
        };
        *key = "forged".into();
        forged.replace_function_for_test_only(function);
        let errors = crate::codegen::mir::validate_mir_native(&forged)
            .expect_err("native admission must reject MIR that disagrees with its v1 receipt");
        assert!(
            errors
                .iter()
                .any(|error| { error.to_string().contains("MapAnyOwner v1") }),
            "native validator errors: {errors:#?}"
        );
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_map_any_owner_v1_forged");
        assert!(generator.compile_mir_native(&forged).is_err());
        assert!(generator
            .module
            .get_function("mimi_mir_map_any_owner_v1_new")
            .is_none());

        let mut missing = program.clone();
        let mut function = missing
            .functions()
            .get(&owner)
            .expect("MapAnyOwner v1 main")
            .clone();
        function
            .ownership
            .clear_checker_map_any_owner_actions_for_test_only();
        missing.replace_function_for_test_only(function);
        let errors = crate::codegen::mir::validate_mir_native(&missing)
            .expect_err("native admission must reject missing independent v1 receipts");
        assert!(errors
            .iter()
            .any(|error| { error.to_string().contains("MapAnyOwner v1") }));

        let mut out_of_range = program.clone();
        let mut function = out_of_range
            .functions()
            .get(&owner)
            .expect("MapAnyOwner v1 main")
            .clone();
        let set_value = function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::MapRootSet { value, .. } => Some(value.clone()),
                _ => None,
            })
            .expect("MapAnyOwner v1 Set scalar");
        let constant = function
            .blocks
            .values_mut()
            .flat_map(|block| block.instructions.iter_mut())
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Const { result, .. } if result == &set_value
                )
            })
            .expect("MapAnyOwner v1 Set scalar constant");
        let MirInstructionKind::Const {
            literal: crate::core::ir::ResolvedLiteral::Int(literal),
            ..
        } = &mut constant.kind
        else {
            panic!("MapAnyOwner v1 Set scalar must be an integer literal");
        };
        *literal = i64::from(i32::MAX) + 1;
        out_of_range.replace_function_for_test_only(function);
        let errors = crate::codegen::mir::validate_mir_native(&out_of_range)
            .expect_err("native admission must reject an i32 literal outside its TypeDesc range");
        assert!(errors.iter().any(|error| {
            error
                .to_string()
                .contains("does not match native TypeDesc ABI")
        }));
    }

    #[test]
    fn emitted_map_any_owner_v1_drop_reclaims_the_production_runtime_entry() {
        let program = canonical_program(MAP_ANY_OWNER_V1_NATIVE_LIFECYCLE_SOURCE);
        crate::codegen::mir::validate_mir_native(&program)
            .expect("native MapAnyOwner v1 lifecycle probe is admitted");
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_map_any_owner_v1_lifecycle");
        generator
            .compile_mir_native(&program)
            .expect("receipt-backed MapAnyOwner v1 lifecycle lowering");
        let native_ir = generator.module.print_to_string().to_string();
        assert!(native_ir.contains("call void @mimi_mir_map_any_owner_v1_drop"));
        assert!(!native_ir.contains("call void @mimi_mir_map_root_drop"));
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("production runtime link observes the emitted Drop");
        assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
        assert_eq!(
            native.stdout, "0\n",
            "emitted v1 Drop must leave the temporary owner empty and reclaimable"
        );

        if std::process::Command::new("valgrind")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            let memcheck = crate::tests::link_and_observe_canonical_mir_with_config(
                &generator,
                &crate::tests::E2EConfig {
                    use_valgrind: true,
                    valgrind_args: vec![
                        "--tool=memcheck".into(),
                        "--leak-check=full".into(),
                        "--show-leak-kinds=all".into(),
                        // The process-global AnyOwner registry and descriptor
                        // table stay reachable until process exit. Treat real
                        // losses as errors and report reachable bookkeeping.
                        "--errors-for-leak-kinds=definite,indirect,possible".into(),
                        "--error-exitcode=99".into(),
                    ],
                    ..crate::tests::E2EConfig::default()
                },
            )
            .expect("production runtime MapAnyOwner v1 lifecycle under Memcheck");
            assert_clean_map_any_memcheck(&memcheck, 0, "0\n", None);
        }
    }

    #[test]
    fn native_map_root_checked_size_overflow_uses_the_e0802_abort_path() {
        let program = canonical_program(MAP_ROOT_NATIVE_OVERFLOW_SOURCE);
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_map_root_size_overflow");
        generator
            .compile_mir_native(&program)
            .expect("test-only checked-size runtime boundary compiles through scalar FFI");
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("overflow child process links against the production runtime");
        assert_ne!(
            native.exit_code,
            Some(0),
            "overflow must abort the child process"
        );
        assert!(native.stderr.contains("E0802"), "{}", native.stderr);
        assert!(native.stderr.contains("MapRoot.size result overflows i32"));
        assert!(native.stdout.is_empty());
    }

    #[test]
    fn silent_local_flow_transition_is_one_four_consumer_production_island() {
        let program = canonical_program(
            "flow Counter { state Zero { n: i32 } state Active { n: i32 } transition inc(Zero, amount: i32) -> Active { return Active { n: self.n + amount } } } func main() -> i32 { let c = Zero { n: 41 } let active = Counter::inc(c, 1) active.n }",
        );
        let transition = crate::core::NodeId("transition:Counter::inc::Zero".into());
        let contract = program
            .transitions()
            .get(&transition)
            .expect("implemented transition contract");
        assert_eq!(contract.targets.len(), 1);
        assert_eq!(
            contract.effect,
            crate::core::mir::MirTransitionEffect::SilentLocal
        );
        assert_eq!(contract.source, contract.parameters[0]);
        assert_eq!(contract.parameters.len(), 2, "source state + i32 event arg");
        assert_ne!(contract.source, contract.result, "transition changes state");
        let state_desc = program
            .type_catalog()
            .get(&contract.source)
            .expect("Flow state TypeDesc");
        assert_eq!(
            state_desc.abi,
            crate::core::mir::types::MirAbiClass::Aggregate
        );
        assert_eq!(
            state_desc.ownership,
            crate::core::mir::types::MirOwnership::Linear
        );
        assert_eq!(
            state_desc.glue.move_out,
            crate::core::mir::types::MirGlueKind::Aggregate
        );
        assert_eq!(
            state_desc.glue.clone,
            crate::core::mir::types::MirGlueKind::Aggregate
        );
        assert_eq!(
            state_desc.glue.drop,
            crate::core::mir::types::MirGlueKind::Aggregate
        );
        assert!(state_desc.drop_plan.is_some());
        assert!(program.functions().contains_key(&transition));
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        &instruction.kind,
                        crate::core::mir::MirInstructionKind::FlowTransition {
                            transition: owner,
                            arguments,
                            ..
                        } if owner == &transition && arguments.len() == 2
                    )
                })
            })
        }));

        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Flow transition execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));

        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("Flow transition MIR bytecode"))
                .run_value()
                .expect("bytecode Flow transition execution");
        assert!(matches!(bytecode, Value::Int(42)));

        crate::verifier::validate_mir_capabilities(&program)
            .expect("verifier capability for Flow transition island");
        crate::verifier::verify_mir(&program, String::new())
            .expect("verifier consumes Flow transition MIR");

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_flow_transition_test");
        generator
            .compile_mir_native(&program)
            .expect("native Flow transition lowering");
        generator
            .module
            .verify()
            .expect("native Flow transition module verifies");
        assert!(generator.module.get_function("main").is_some());
        assert!(generator
            .module
            .get_function("__mimi_transition_Counter__inc__Zero")
            .is_some());
    }

    #[test]
    fn record_list_family_composes_across_reference_bytecode_native_and_verifier() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_m1_record_list_chain.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Record/List composition");
        assert_eq!(reference, MirRuntimeValue::Int(6));

        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("Record/List composition MIR bytecode"),
        )
        .run_value()
        .expect("bytecode Record/List composition");
        assert!(matches!(bytecode, Value::Int(6)));

        crate::verifier::validate_mir_capabilities(&program)
            .expect("verifier capability for Record/List composition");
        let verification = crate::verifier::verify_mir(&program, String::new())
            .expect("verifier consumes Record/List composition MIR");
        let main_verification = verification
            .iter()
            .find(|result| result.func_name == owner.0)
            .expect("M1 must have a non-empty contract result");
        assert_eq!(
            main_verification.status,
            crate::verifier::VerifStatus::Proven
        );
        let artifact = main_verification
            .artifact
            .as_ref()
            .expect("M1 Proven result must carry a proof artifact");
        assert_eq!(artifact.engine, crate::verifier::ProofArtifact::ENGINE_MIR);
        assert_eq!(artifact.mir_hash, program.canonical_digest());

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_m1_record_list_chain_test");
        generator
            .compile_mir_native(&program)
            .expect("native Record/List composition lowering");
        generator
            .module
            .verify()
            .expect("native Record/List composition module verifies");
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("native Record/List composition execution");
        assert_eq!(native.stdout, "");
        assert_eq!(native.stderr, "");
        assert_eq!(native.exit_code, Some(6));
    }

    #[test]
    fn mir_verifier_receipt_distinguishes_disproven_contract_with_mir_identity() {
        let program = canonical_program(
            "func disproven(value: i32) -> i32 { requires: value == 41 ensures: result == 42 value }\nfunc main() -> i32 { 0 }",
        );
        let results = crate::verifier::verify_mir(&program, "m3-disproven-source".into())
            .expect("MIR verifier must return a counterexample result");
        let result = results
            .iter()
            .find(|result| result.func_name.ends_with("disproven"))
            .expect("disproven contract result");
        assert_eq!(result.status, crate::verifier::VerifStatus::Disproven);
        assert!(result.message.contains("ensures contract is disproven"));
        let artifact = result
            .artifact
            .as_ref()
            .expect("definitive counterexample must carry a MIR artifact");
        assert_eq!(artifact.engine, crate::verifier::ProofArtifact::ENGINE_MIR);
        assert_eq!(artifact.mir_hash, program.canonical_digest());
    }

    #[test]
    fn generated_scalar_list_chains_share_one_mir_across_three_execution_consumers() {
        // A small deterministic generator keeps the differential corpus
        // reproducible while varying the list shape that crosses the same
        // reverse/concat/len production boundary. Each iteration constructs
        // one MirProgram and passes that exact object to all three consumers.
        for values in [vec![], vec![4], vec![1, 2], vec![7, 8, 9], vec![-3, 0, 12]] {
            let literals = values
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            let source = format!(
                "func main() -> i32 {{\n    let values: List<i32> = [{literals}]\n    let reversed = values.reverse()\n    let joined = reversed.concat([11])\n    let count = len(joined)\n    drop(values)\n    drop(joined)\n    count\n}}"
            );
            let program = canonical_program(&source);
            let owner = crate::core::NodeId("function:main".into());
            let expected = MirRuntimeValue::Int(values.len() as i64 + 1);

            let reference = MirReferenceInterpreter::new(&program)
                .execute(&owner, &[])
                .expect("generated list reference execution");
            assert_eq!(reference, expected);

            let bytecode = BytecodeVM::new(
                compile_mir_program(&program).expect("generated list MIR bytecode"),
            )
            .run_value()
            .expect("generated list bytecode execution");
            assert!(matches!(bytecode, Value::Int(value) if value == values.len() as i64 + 1));

            crate::verifier::validate_mir_capabilities(&program)
                .expect("generated list verifier capability");
            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, "mir_generated_list_chain_test");
            generator
                .compile_mir_native(&program)
                .expect("generated list native lowering");
            generator
                .module
                .verify()
                .expect("generated list native module verifies");
            let native = crate::tests::link_and_observe_canonical_mir(&generator)
                .expect("generated list native execution");
            assert_eq!(native.stdout, "");
            assert_eq!(native.stderr, "");
            assert_eq!(
                native.exit_code,
                Some(values.len() as i32 + 1),
                "native result diverged for generated values {values:?}"
            );
        }
    }

    #[test]
    fn generated_cross_state_flow_cases_share_one_mir_across_three_consumers() {
        // Keep the generator deterministic and centered on the R6 Flow
        // contract: one input takes the recoverable Err((source, error))
        // branch, while the remaining inputs take the Ok(target) branch.
        // Every iteration constructs exactly one MirProgram and gives that
        // same object to reference, MIR bytecode, and native LLVM. There is
        // deliberately no legacy compile_file comparison here: a canonical
        // rejection or consumer mismatch must remain visible.
        let template = r#"
flow F {
    state A { n: i32 }
    state B { n: i32 }

    transition go(A) -> B fails string {
        let checked: Result<i32, string> = if self.n == 1 {
            Err("bad")
        } else {
            Ok(self.n + 1)
        }
        let next = checked?
        return B { n: next }
    }
}

func main() -> i32 {
    let result = F::go(A { n: INPUT })
    match result {
        Ok(B { n }) => {
            println(n)
            99
        },
        Err((source, error)) => {
            println(source.n)
            drop(source)
            drop(error)
            0
        },
    }
}
"#;

        for input in [-7_i32, 0, 1, 2, 41] {
            let source = template.replace("INPUT", &input.to_string());
            let program = canonical_program(&source);
            let owner = crate::core::NodeId("function:main".into());
            let mir_digest = program.canonical_digest();
            assert!(
                crate::core::mir::contains_flow_failure_retry_candidate(&program),
                "generated input {input} must materialize a recoverable Flow boundary"
            );
            let (expected_int, expected_output, expected_exit) = if input == 1 {
                (0_i64, "1\n".to_owned(), 0)
            } else {
                (99_i64, format!("{}\n", input + 1), 99)
            };
            let expected_value = MirRuntimeValue::Int(expected_int);

            let reference = MirReferenceInterpreter::new(&program)
                .execute_with_output(&owner, &[])
                .expect("generated cross-state Flow reference execution");
            assert_eq!(reference.value, expected_value, "input {input}");
            assert_eq!(reference.output, expected_output, "input {input}");

            let mut bytecode = BytecodeVM::new(
                compile_mir_program(&program)
                    .expect("generated cross-state Flow MIR bytecode compilation"),
            );
            let bytecode_value = bytecode
                .run_value()
                .expect("generated cross-state Flow bytecode execution");
            assert!(
                matches!(bytecode_value, Value::Int(value) if value == expected_int),
                "input {input}"
            );
            assert_eq!(bytecode.take_stdout(), expected_output, "input {input}");

            crate::verifier::validate_mir_capabilities(&program)
                .expect("generated cross-state Flow verifier capability");

            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, "mir_generated_cross_state_flow");
            generator
                .compile_mir_native(&program)
                .expect("generated cross-state Flow native lowering");
            generator
                .module
                .verify()
                .expect("generated cross-state Flow native module verifies");
            let native = crate::tests::link_and_observe_canonical_mir(&generator)
                .expect("generated cross-state Flow native execution");
            assert_eq!(native.stdout, expected_output, "input {input}");
            assert_eq!(native.stderr, "", "input {input}");
            assert_eq!(native.exit_code, Some(expected_exit), "input {input}");
            assert_eq!(
                program.canonical_digest(),
                mir_digest,
                "a consumer mutated the shared MIR for input {input}"
            );
        }
    }

    #[test]
    fn recoverable_flow_failure_returns_source_and_retries_across_four_consumers() {
        let source = include_str!("../../../tests/fixtures/mir_m3_flow_retry.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("recoverable Flow transition must lower to canonical MIR");
        let transition = crate::core::NodeId("transition:Account::withdraw::Active".into());
        let contract = program
            .transitions()
            .get(&transition)
            .expect("recoverable transition contract");
        assert_eq!(
            contract.effect,
            crate::core::mir::MirTransitionEffect::RecoverableLocal
        );
        assert_eq!(contract.targets.len(), 1);
        assert!(contract.failure.is_some());
        assert_eq!(
            program
                .functions()
                .get(&crate::core::NodeId("function:main".into()))
                .expect("main MIR")
                .blocks
                .values()
                .flat_map(|block| block.instructions.iter())
                .filter(|instruction| {
                    matches!(
                        instruction.kind,
                        crate::core::mir::MirInstructionKind::FlowTransition {
                            transition: ref owner,
                            ..
                        } if owner == &transition
                    )
                })
                .count(),
            2,
            "the returned source must be consumed by exactly one retry"
        );
        let transition_body = program
            .functions()
            .get(&transition)
            .expect("transition MIR");
        assert!(transition_body.blocks.values().any(|block| matches!(
            block.terminator,
            crate::core::mir::MirTerminator::SwitchMove { .. }
        )));
        assert!(transition_body.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::Construct {
                        kind: crate::core::mir::MirAggregateKind::Tuple,
                        ..
                    }
                )
            })
        }));

        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&owner, &[])
            .expect("reference recoverable Flow execution");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(reference.output, "100\n95\n");

        let mut bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("recoverable Flow MIR bytecode"));
        let bytecode_value = bytecode
            .run_value()
            .expect("bytecode recoverable Flow execution");
        assert!(matches!(bytecode_value, Value::Int(0)));
        assert_eq!(bytecode.take_stdout(), reference.output);

        crate::verifier::validate_mir_capabilities(&program)
            .expect("verifier capability for recoverable Flow");
        let verification = crate::verifier::verify_mir(&program, String::new())
            .expect("verifier consumes recoverable Flow MIR");
        let proof = verification
            .iter()
            .find(|result| result.func_name.ends_with("retry_balance_contract"))
            .expect("M3 must contain a non-empty positive contract result");
        assert_eq!(proof.status, crate::verifier::VerifStatus::Proven);
        let artifact = proof
            .artifact
            .as_ref()
            .expect("M3 Proven result must carry a proof artifact");
        assert_eq!(artifact.engine, crate::verifier::ProofArtifact::ENGINE_MIR);
        assert_eq!(artifact.mir_hash, program.canonical_digest());

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_m3_flow_retry_test");
        generator
            .compile_mir_native(&program)
            .expect("native recoverable Flow lowering");
        generator
            .module
            .verify()
            .expect("native recoverable Flow module verifies");
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("native recoverable Flow execution");
        assert_eq!(native.stdout, "100\n95\n");
        assert_eq!(native.stderr, "");
        assert_eq!(native.exit_code, Some(0));
    }

    #[test]
    fn recoverable_flow_helper_result_shares_one_mir_across_four_consumers() {
        let source = include_str!("../../../tests/fixtures/mir_m3_flow_retry_helper_result.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("recoverable Flow helper Result must lower to canonical MIR");

        let transition = crate::core::NodeId("transition:Account::withdraw::Active".into());
        let transition_body = program
            .functions()
            .get(&transition)
            .expect("recoverable transition MIR");
        let helper_receipts = transition_body
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::Call {
                    variant_call_contract: Some(receipt),
                    ..
                } => Some(receipt),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(helper_receipts.len(), 1);
        let receipt = helper_receipts[0];
        assert_eq!(receipt.callee.0, "function:validate_amount");
        assert_eq!(
            receipt.mode,
            crate::core::mir::types::MirVariantCallAbiMode::RecoverableAggregate
        );
        assert_eq!(
            receipt.return_mode,
            crate::core::mir::types::MirVariantCallReturnMode::AggregateEnvelopeMerge
        );
        assert_eq!(receipt.payload_types.len(), 2);
        program
            .type_catalog()
            .validate_variant_call_abi_receipt(
                &receipt.callee,
                &receipt.type_arguments,
                &receipt.parameter_types,
                &receipt.result_ty,
                receipt,
            )
            .expect("helper Result receipt must be TypeDesc-derived");

        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&owner, &[])
            .expect("reference helper Result execution");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(reference.output, "100\n100\n95\n");

        let mut bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("helper Result MIR bytecode"));
        let bytecode_value = bytecode
            .run_value()
            .expect("bytecode helper Result execution");
        assert!(matches!(bytecode_value, Value::Int(0)));
        assert_eq!(bytecode.take_stdout(), "100\n100\n95\n");

        crate::verifier::validate_mir_capabilities(&program)
            .expect("verifier capability for helper Result Flow");
        let verification = crate::verifier::verify_mir(&program, String::new())
            .expect("verifier consumes helper Result Flow MIR");
        let proof = verification
            .iter()
            .find(|result| result.func_name.ends_with("retry_balance_contract"))
            .expect("helper Result fixture must contain a contract result");
        assert_eq!(proof.status, crate::verifier::VerifStatus::Proven);
        assert!(proof.constraint_count > 0);
        let artifact = proof
            .artifact
            .as_ref()
            .expect("helper Result Proven result must carry a proof artifact");
        assert_eq!(artifact.engine, crate::verifier::ProofArtifact::ENGINE_MIR);
        assert_eq!(artifact.mir_hash, program.canonical_digest());

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_m3_flow_retry_helper_result_test");
        generator
            .compile_mir_native(&program)
            .expect("native helper Result Flow lowering");
        generator
            .module
            .verify()
            .expect("native helper Result Flow module verifies");
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("native helper Result Flow execution");
        assert_eq!(native.stdout, "100\n100\n95\n");
        assert_eq!(native.stderr, "");
        assert_eq!(native.exit_code, Some(0));
    }

    #[test]
    fn recoverable_flow_string_state_shares_one_mir_across_four_consumers() {
        let source = include_str!("../../../tests/fixtures/mir_m3_flow_retry_string_state.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("recoverable Flow String state must lower to canonical MIR");

        let transition = crate::core::NodeId("transition:Account::withdraw::Active".into());
        let contract = program
            .transitions()
            .get(&transition)
            .expect("recoverable String transition contract");
        assert_eq!(
            contract.effect,
            crate::core::mir::MirTransitionEffect::RecoverableLocal
        );
        let state_desc = program
            .type_catalog()
            .get(&contract.source)
            .expect("String-bearing Flow state TypeDesc");
        assert!(matches!(state_desc.layout, MirLayout::Record { .. }));
        assert_eq!(state_desc.glue.move_out, MirGlueKind::Aggregate);
        assert!(state_desc.drop_plan.as_ref().is_some_and(|plan| {
            plan.fields
                .iter()
                .any(|field| field.glue == MirGlueKind::OwnedString)
        }));
        let println_string_calls = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::BuiltinCall {
                        kind: MirBuiltinKind::PrintlnString,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(println_string_calls, 2);

        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&owner, &[])
            .expect("reference recoverable String Flow execution");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(reference.output, "invalid amount\ncredit\n");

        let mut bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("String state MIR bytecode"));
        let bytecode_value = bytecode
            .run_value()
            .expect("bytecode recoverable String Flow execution");
        assert!(matches!(bytecode_value, Value::Int(0)));
        assert_eq!(bytecode.take_stdout(), "invalid amount\ncredit\n");

        crate::verifier::validate_mir_capabilities(&program)
            .expect("verifier capability for String state Flow");
        let verification = crate::verifier::verify_mir(&program, String::new())
            .expect("verifier consumes String state Flow MIR");
        let proof = verification
            .iter()
            .find(|result| result.func_name.ends_with("retry_string_state_contract"))
            .expect("String state fixture must contain a contract result");
        assert_eq!(proof.status, crate::verifier::VerifStatus::Proven);
        assert!(proof.constraint_count > 0);
        let artifact = proof
            .artifact
            .as_ref()
            .expect("String state Proven result must carry a proof artifact");
        assert_eq!(artifact.engine, crate::verifier::ProofArtifact::ENGINE_MIR);
        assert_eq!(artifact.mir_hash, program.canonical_digest());

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_m3_flow_retry_string_state_test");
        generator
            .compile_mir_native(&program)
            .expect("native String state Flow lowering");
        generator
            .module
            .verify()
            .expect("native String state Flow module verifies");
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("native String state Flow execution");
        assert_eq!(native.stdout, "invalid amount\ncredit\n");
        assert_eq!(native.stderr, "");
        assert_eq!(native.exit_code, Some(0));
    }

    #[test]
    fn canonical_mir_rejects_forged_string_println_abi_before_consumers() {
        let source = include_str!("../../../tests/fixtures/mir_m3_flow_retry_string_state.mimi");
        let program = canonical_program(source);
        let mut functions = program.functions().clone();
        let mut replaced = false;
        for function in functions.values_mut() {
            for block in function.blocks.values_mut() {
                for instruction in &mut block.instructions {
                    if let crate::core::mir::MirInstructionKind::BuiltinCall { kind, .. } =
                        &mut instruction.kind
                    {
                        if *kind == MirBuiltinKind::PrintlnString {
                            *kind = MirBuiltinKind::PrintlnInt;
                            replaced = true;
                            break;
                        }
                    }
                }
                if replaced {
                    break;
                }
            }
            if replaced {
                break;
            }
        }
        assert!(
            replaced,
            "String state fixture must contain a String println"
        );
        let errors = MirProgram::with_type_catalog_and_instances_and_transitions(
            functions,
            program.type_catalog().clone(),
            program.instances().clone(),
            program.transitions().clone(),
        )
        .expect_err("forged StringHandle -> integer println must fail closed");
        let messages = errors
            .iter()
            .map(|error| error.message.as_str())
            .collect::<Vec<_>>();
        assert!(
            messages.iter().any(|message| {
                message.contains("does not support argument")
                    || message.contains("requires Copy arguments")
            }),
            "unexpected forged builtin diagnostics: {messages:?}"
        );
    }

    #[test]
    fn cross_state_result_match_uses_one_mir_across_reference_bytecode_and_native() {
        let program = canonical_program(include_str!(
            "../../../tests/real_world/flow_state_match_fail_result_dual_backend.mimi"
        ));
        let transition = crate::core::NodeId("transition:F::go::A".into());
        let contract = program
            .transitions()
            .get(&transition)
            .expect("cross-state recoverable transition contract");
        assert_eq!(
            contract.effect,
            crate::core::mir::MirTransitionEffect::RecoverableBoundary
        );
        assert_eq!(contract.targets.len(), 1);
        assert!(contract.failure.is_some());
        assert!(crate::core::mir::contains_flow_failure_retry_candidate(
            &program
        ));

        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&owner, &[])
            .expect("reference cross-state Result match execution");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(reference.output, "2\n");

        let mut bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("cross-state Result match MIR bytecode"),
        );
        let bytecode_value = bytecode
            .run_value()
            .expect("bytecode cross-state Result match execution");
        assert!(matches!(bytecode_value, Value::Int(0)));
        assert_eq!(bytecode.take_stdout(), "2\n");

        crate::verifier::validate_mir_capabilities(&program)
            .expect("verifier capability for cross-state recoverable Result");

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_cross_state_result_match_test");
        generator
            .compile_mir_native(&program)
            .expect("native cross-state Result match lowering");
        generator
            .module
            .verify()
            .expect("native cross-state Result match module verifies");
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("native cross-state Result match execution");
        assert_eq!(native.stdout, "2\n");
        assert_eq!(native.stderr, "");
        assert_eq!(native.exit_code, Some(0));
    }

    #[test]
    fn boundary_flow_effect_receipt_preserves_order_state_across_three_consumers() {
        let program = canonical_program(include_str!(
            "../../../tests/real_world/flow_order_system.mimi"
        ));
        let ship = crate::core::NodeId("transition:Order::ship::Paid".into());
        let deliver = crate::core::NodeId("transition:Order::deliver::Shipped".into());
        for transition in [&ship, &deliver] {
            let contract = program
                .transitions()
                .get(transition)
                .expect("boundary transition contract");
            assert_eq!(
                contract.effect,
                crate::core::mir::MirTransitionEffect::Boundary
            );
            assert_eq!(contract.targets.len(), 1);
        }
        let main = program
            .functions()
            .get(&crate::core::NodeId("function:main".into()))
            .expect("order main MIR");
        let receipts = main
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter_map(|instruction| match &instruction.kind {
                MirInstructionKind::FlowTransition {
                    transition,
                    effect_receipt: Some(receipt),
                    ..
                } if transition == &ship || transition == &deliver => Some(receipt),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(receipts.len(), 2, "both cross-state calls need receipts");
        assert!(receipts
            .iter()
            .all(|receipt| { receipt.transition == ship || receipt.transition == deliver }));

        let pay = program
            .functions()
            .get(&crate::core::NodeId(
                "transition:Order::pay::Pending".into(),
            ))
            .expect("pay transition MIR");
        let pay_parameter = pay.parameters.get(1).expect("pay transaction parameter");
        let pay_parameter_drops = pay
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Drop { value } if value == pay_parameter
                )
            })
            .count();
        assert_eq!(
            pay_parameter_drops, 2,
            "the extra linear transaction parameter must be discharged on both ? paths"
        );

        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&owner, &[])
            .expect("reference order Flow execution");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(
            reference.output,
            "TXN-42\nTRK-001\nbook\ninvalid price\n0\n"
        );

        let mut bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("order Flow MIR bytecode"));
        let bytecode_value = bytecode.run_value().expect("bytecode order Flow execution");
        assert!(matches!(bytecode_value, Value::Int(0)));
        assert_eq!(bytecode.take_stdout(), reference.output);

        crate::verifier::validate_mir_capabilities(&program)
            .expect("verifier capability for Boundary Flow receipt");

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_boundary_flow_order_test");
        generator
            .compile_mir_native(&program)
            .expect("native order Flow lowering");
        generator
            .module
            .verify()
            .expect("native order Flow module verifies");
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("native order Flow execution");
        assert_eq!(native.stdout, reference.output);
        assert_eq!(native.stderr, "");
        assert_eq!(native.exit_code, Some(0));
    }

    #[test]
    fn boundary_flow_effect_receipt_missing_or_forged_is_rejected_before_consumers() {
        let program = canonical_program(include_str!(
            "../../../tests/real_world/flow_order_system.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let transition = crate::core::NodeId("transition:Order::ship::Paid".into());

        let mut missing = program.functions().clone();
        let instruction = missing
            .get_mut(&owner)
            .expect("order main")
            .blocks
            .values_mut()
            .flat_map(|block| block.instructions.iter_mut())
            .find(|instruction| {
                matches!(
                    instruction.kind,
                    MirInstructionKind::FlowTransition {
                        transition: ref actual,
                        ..
                    } if actual == &transition
                )
            })
            .expect("ship FlowTransition");
        let MirInstructionKind::FlowTransition { effect_receipt, .. } = &mut instruction.kind
        else {
            unreachable!();
        };
        *effect_receipt = None;
        let errors = MirProgram::with_type_catalog_and_instances_and_transitions(
            missing,
            program.type_catalog().clone(),
            program.instances().clone(),
            program.transitions().clone(),
        )
        .expect_err("missing Boundary receipt must fail before consumers");
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("Boundary transition without an effect receipt")
        }));

        let mut forged = program.functions().clone();
        let instruction = forged
            .get_mut(&owner)
            .expect("order main")
            .blocks
            .values_mut()
            .flat_map(|block| block.instructions.iter_mut())
            .find(|instruction| {
                matches!(
                    instruction.kind,
                    MirInstructionKind::FlowTransition {
                        transition: ref actual,
                        ..
                    } if actual == &transition
                )
            })
            .expect("ship FlowTransition");
        let MirInstructionKind::FlowTransition {
            effect_receipt: Some(receipt),
            ..
        } = &mut instruction.kind
        else {
            unreachable!();
        };
        receipt.parameters.clear();
        let errors = MirProgram::with_type_catalog_and_instances_and_transitions(
            forged,
            program.type_catalog().clone(),
            program.instances().clone(),
            program.transitions().clone(),
        )
        .expect_err("forged Boundary receipt must fail before consumers");
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("Flow Boundary effect receipt parameter TypeDesc identities disagree")
        }));
    }

    #[test]
    fn recoverable_flow_parameter_cleanup_does_not_trust_sibling_branch() {
        let source = r#"
func validate_price(price: i32) -> Result<i32, string> {
    if price <= 0 { Err("invalid") } else { Ok(price) }
}

flow Order {
    state Pending { price: i32 }
    state Paid { price: i32 }

        transition pay(Pending, txn_id: string) -> Paid fails string {
            if self.price > 0 {
                drop(txn_id)
            } else {
                let untouched = self.price
            }
        let valid_price = validate_price(self.price)?
        return Paid { price: valid_price }
    }
}

func main() -> i32 {
    let pending = Pending { price: 1 }
    match Order::pay(pending, "TXN") {
        Ok(paid) => { println(paid.price) drop(paid) 0 },
        Err((source, error)) => { drop(error) drop(source) 0 },
    }
}
"#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("mixed sibling consumption must stay outside canonical MIR");
        match error {
            crate::core::mir::reference::MirProgramBuildError::Lowering(errors) => {
                assert!(errors.iter().any(|error| {
                    error
                        .message
                        .contains("path-dependent consumption before `?`")
                }));
            }
            other => panic!("expected fail-closed lowering diagnostic, got {other:?}"),
        }
    }

    #[test]
    fn cross_state_failure_returns_source_and_drops_one_failure_tuple_across_consumers() {
        let program = canonical_program(include_str!(
            "../../../tests/real_world/flow_state_match_fail_result_failure_dual_backend.mimi"
        ));
        let transition = crate::core::NodeId("transition:F::go::A".into());
        let contract = program
            .transitions()
            .get(&transition)
            .expect("cross-state failing transition contract");
        assert_eq!(
            contract.effect,
            crate::core::mir::MirTransitionEffect::RecoverableBoundary
        );
        assert_eq!(contract.targets.len(), 1);
        assert!(contract.failure.is_some());
        let main = program
            .functions()
            .get(&crate::core::NodeId("function:main".into()))
            .expect("main MIR");
        let read_paths = main
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::Project {
                    projection: crate::core::mir::MirProjection::ReadPath(receipt),
                    ..
                } => Some(receipt),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            read_paths.is_empty(),
            "nested source binding must not synthesize a read path"
        );
        let nested_receipts = main
            .blocks
            .values()
            .flat_map(|block| match &block.terminator {
                crate::core::mir::MirTerminator::SwitchMove { arms, .. } => arms.as_slice(),
                _ => &[] as &[crate::core::mir::MirSwitchArm],
            })
            .flat_map(|arm| arm.bindings.iter())
            .filter(|binding| binding.nested_tuple.is_some())
            .count();
        assert_eq!(nested_receipts, 2);
        assert!(main.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::Drop { ref value }
                        if value.as_str().contains("pattern.variable")
                )
            })
        }));

        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&owner, &[])
            .expect("reference cross-state failure execution");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(reference.output, "1\n");

        let mut bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("cross-state failure MIR bytecode"),
        );
        let bytecode_value = bytecode
            .run_value()
            .expect("bytecode cross-state failure execution");
        assert!(matches!(bytecode_value, Value::Int(0)));
        assert_eq!(bytecode.take_stdout(), "1\n");

        crate::verifier::validate_mir_capabilities(&program)
            .expect("verifier capability for cross-state failure Result");

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_cross_state_result_failure_test");
        generator
            .compile_mir_native(&program)
            .expect("native cross-state failure lowering");
        generator
            .module
            .verify()
            .expect("native cross-state failure module verifies");
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("native cross-state failure execution");
        assert_eq!(native.stdout, "1\n");
        assert_eq!(native.stderr, "");
        assert_eq!(native.exit_code, Some(0));
    }

    #[test]
    fn recoverable_flow_negative_matrix_rejects_source_loss_identity_and_stale_state() {
        let cases = [
            (
                "mir_m3_flow_source_loss_rejected.mimi",
                "does not consume the returned source at tuple index 0",
            ),
            (
                "mir_m3_flow_branch_join_rejected.mimi",
                "does not consume the returned source at tuple index 0",
            ),
        ];
        for (fixture, expected) in cases {
            let source = match fixture {
                "mir_m3_flow_source_loss_rejected.mimi" => {
                    include_str!("../../../tests/fixtures/mir_m3_flow_source_loss_rejected.mimi")
                }
                "mir_m3_flow_branch_join_rejected.mimi" => {
                    include_str!("../../../tests/fixtures/mir_m3_flow_branch_join_rejected.mimi")
                }
                _ => unreachable!("fixture is fixed above"),
            };
            let tokens = Lexer::new(source).tokenize().expect("lex");
            let file = Parser::new(tokens).parse_file().expect("parse");
            let checked = crate::core::check_program(&file).expect("negative remains well-typed");
            let error = MirProgram::from_checked_program(&checked)
                .expect_err("invalid recoverable failure receipt must fail Canonical MIR");
            let text = format!("{error:?}");
            assert!(
                text.contains(expected),
                "{fixture} lost the source-residual diagnostic: {text}"
            );
        }

        for (fixture, expected) in [
            (
                include_str!("../../../tests/fixtures/mir_m3_flow_wrong_identity_rejected.mimi"),
                "E0211",
            ),
            (
                include_str!(
                    "../../../tests/fixtures/mir_m3_flow_success_stale_state_rejected.mimi"
                ),
                "E0423",
            ),
        ] {
            let tokens = Lexer::new(fixture).tokenize().expect("lex");
            let file = Parser::new(tokens).parse_file().expect("parse");
            let errors = crate::core::check_program(&file)
                .expect_err("invalid Flow identity fixture must fail checker");
            let text = format!("{errors:?}");
            assert!(
                text.contains(expected),
                "expected {expected} in Flow negative diagnostic: {text}"
            );
        }
    }

    #[test]
    fn recoverable_flow_contract_rejects_silent_effect_forgery_before_consumers() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_m3_flow_retry.mimi"
        ));
        let mut transitions = program.transitions().clone();
        transitions
            .values_mut()
            .next()
            .expect("recoverable transition")
            .effect = crate::core::mir::MirTransitionEffect::SilentLocal;
        let errors = crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            program.functions().clone(),
            program.type_catalog().clone(),
            program.instances().clone(),
            transitions,
        )
        .expect_err("forged silent failure contract must be rejected before consumers");
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("silent-local transition must be one target, non-failing")
        }));
    }

    #[test]
    fn native_validator_admits_f64_comparisons_with_llvm_declarations() {
        // R6-1068 restatement: the f64 comparison face this pin used to hold
        // closed now compiles natively, so the pin flips to positive
        // admission — main is declared and the module verifies.  The
        // fail-before-LLVM-emission property stays pinned by
        // `native_validator_rejects_record_with_unsupported_child_before_llvm_declarations`.
        let program = canonical_program("func main() -> i32 { if 0.5 < 1.5 { println(1) } 0 }");
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_validator_test");

        generator
            .compile_mir_native(&program)
            .expect("R6-1068 admits f64 comparisons into the native emitter");
        assert!(
            generator.module.get_function("main").is_some(),
            "the admitted comparison face declares main"
        );
        generator
            .module
            .verify()
            .expect("the comparison face emits a verifiable module");
    }

    #[test]
    fn native_validator_rejects_record_with_unsupported_child_before_llvm_declarations() {
        let program = canonical_program(
            "type Box { text: string, values: Option<string> }\nfunc main() -> i32 { let value = Box { text: \"x\", values: Some(\"y\") }; drop(value); 0 }",
        );
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_record_validator_test");

        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("record with an unsupported child must fail closed in native slice");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(
                    "outside the scalar/String/List<Copy scalar>/Set<Copy scalar>/tuple ABI"
                )),
            "missing unsupported-child rejection: {diagnostics:?}"
        );
        assert!(
            generator.module.get_function("main").is_none(),
            "non-Copy aggregate must be rejected before LLVM declarations"
        );
    }

    #[test]
    fn native_emitter_materializes_non_copy_option_string_clone_move_and_drop() {
        let program = canonical_program(
            "func make_some() -> Option<string> { Some(\"owned\") }\nfunc make_none() -> Option<string> { None }\nfunc main() -> i32 { let some = make_some(); let cloned = some; drop(cloned); drop(some); let none = make_none(); drop(none); 42 }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Option<string> execution");
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("Option<string> MIR bytecode"))
                .run_value()
                .expect("bytecode Option<string> execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(bytecode, Value::Int(42)));

        let make_some = program
            .functions()
            .get(&crate::core::NodeId("function:make_some".into()))
            .expect("make_some MIR");
        assert!(make_some.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::ConstructVariantMove { .. }
                )
            })
        }));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_option_string_test");
        generator
            .compile_mir_native(&program)
            .expect("Option<string> MIR should have a native glue contract");
        generator
            .module
            .verify()
            .expect("native Option<string> module verifies");
        assert!(generator.module.get_function("make_some").is_some());
        assert!(generator.module.get_function("make_none").is_some());
        assert!(generator.module.get_function("mimi_str_clone").is_some());
        assert!(generator.module.get_function("mimi_string_free").is_some());
    }

    #[test]
    fn native_emitter_materializes_option_string_unwrap_move_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_option_string_unwrap.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Option<string>.unwrap execution");
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("Option<string>.unwrap MIR bytecode"),
        )
        .run_value()
        .expect("bytecode Option<string>.unwrap execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        assert!(matches!(bytecode, Value::Int(41)));

        let unwrap_owned = program
            .functions()
            .get(&crate::core::NodeId("function:unwrap_owned".into()))
            .expect("unwrap_owned MIR");
        assert!(unwrap_owned.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::VariantProjectMove {
                        contract: Some(_),
                        ..
                    }
                )
            })
        }));

        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_option_string_unwrap_projection_test");
        generator
            .compile_mir_native(&program)
            .expect("Option<string>.unwrap MIR should have a native contract");
        generator
            .module
            .verify()
            .expect("native Option<string>.unwrap module verifies");
        assert!(generator.module.get_function("unwrap_owned").is_some());
        assert!(generator.module.get_function("mimi_string_free").is_some());
    }

    #[test]
    fn native_emitter_materializes_option_i32_unwrap_copy_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_option_i32_unwrap.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Option<i32>.unwrap execution");
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("Option<i32>.unwrap MIR bytecode"),
        )
        .run_value()
        .expect("bytecode Option<i32>.unwrap execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        assert!(matches!(bytecode, Value::Int(41)));

        let unwrap_copy = program
            .functions()
            .get(&crate::core::NodeId("function:unwrap_copy".into()))
            .expect("unwrap_copy MIR");
        let receipt = unwrap_copy
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::VariantProject {
                    contract: Some(receipt),
                    ..
                } => Some(receipt),
                _ => None,
            })
            .expect("Copy unwrap must carry a read-only projection receipt");
        assert_eq!(receipt.variant_name, "Some");
        assert_eq!(
            receipt.projection.ownership,
            crate::core::mir::types::MirOwnership::Copy
        );
        assert_eq!(
            receipt.projection.move_out_glue,
            crate::core::mir::types::MirGlueKind::Noop
        );

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_option_i32_unwrap_test");
        generator
            .compile_mir_native(&program)
            .expect("Option<i32>.unwrap MIR should have a native contract");
        generator
            .module
            .verify()
            .expect("native Option<i32>.unwrap module verifies");
        assert!(generator.module.get_function("unwrap_copy").is_some());
    }

    #[test]
    fn native_emitter_materializes_result_i32_i32_unwrap_copy_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_result_i32_unwrap.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Result<i32, i32>.unwrap execution");
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("Result<i32, i32>.unwrap MIR bytecode"),
        )
        .run_value()
        .expect("bytecode Result<i32, i32>.unwrap execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        assert!(matches!(bytecode, Value::Int(41)));

        let unwrap_copy = program
            .functions()
            .get(&crate::core::NodeId("function:unwrap_copy".into()))
            .expect("unwrap_copy MIR");
        let receipt = unwrap_copy
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::VariantProject {
                    contract: Some(receipt),
                    ..
                } => Some(receipt),
                _ => None,
            })
            .expect("Result unwrap must carry a read-only projection receipt");
        assert_eq!(receipt.variant_name, "Ok");
        assert_eq!(
            receipt.projection.ownership,
            crate::core::mir::types::MirOwnership::Copy
        );
        assert_eq!(
            receipt.projection.move_out_glue,
            crate::core::mir::types::MirGlueKind::Noop
        );
        program
            .type_catalog()
            .validate_copy_result_i32_variant(&receipt.source_ty)
            .expect("Result<i32, i32> TypeDesc contract");
        crate::verifier::validate_mir_capabilities(&program)
            .expect("verifier capability for Result<i32, i32>.unwrap MIR");

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_result_i32_unwrap_test");
        generator
            .compile_mir_native(&program)
            .expect("Result<i32, i32>.unwrap MIR should have a native contract");
        generator
            .module
            .verify()
            .expect("native Result<i32, i32>.unwrap module verifies");
        assert!(generator.module.get_function("unwrap_copy").is_some());
    }

    #[test]
    fn native_emitter_materializes_result_i32_i32_unwrap_or_copy_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_result_i32_unwrap_or.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Result<i32, i32>.unwrap_or execution");
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("Result<i32, i32>.unwrap_or MIR bytecode"),
        )
        .run_value()
        .expect("bytecode Result<i32, i32>.unwrap_or execution");
        assert_eq!(reference, MirRuntimeValue::Int(14));
        assert!(matches!(bytecode, Value::Int(14)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_result_i32_unwrap_or_test");
        generator
            .compile_mir_native(&program)
            .expect("Result<i32, i32>.unwrap_or MIR should have a native contract");
        generator
            .module
            .verify()
            .expect("native Result<i32, i32>.unwrap_or module verifies");
        assert!(generator.module.get_function("unwrap_ok").is_some());
        assert!(generator.module.get_function("unwrap_err").is_some());
    }

    #[test]
    fn native_emitter_materializes_option_i32_unwrap_or_copy_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_option_i32_unwrap_or.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Option<i32>.unwrap_or execution");
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("Option<i32>.unwrap_or MIR bytecode"),
        )
        .run_value()
        .expect("bytecode Option<i32>.unwrap_or execution");
        assert_eq!(reference, MirRuntimeValue::Int(14));
        assert!(matches!(bytecode, Value::Int(14)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_option_i32_unwrap_or_test");
        generator
            .compile_mir_native(&program)
            .expect("Option<i32>.unwrap_or MIR should have a native contract");
        generator
            .module
            .verify()
            .expect("native Option<i32>.unwrap_or module verifies");
        assert!(generator.module.get_function("some_value").is_some());
        assert!(generator.module.get_function("none_value").is_some());
    }

    #[test]
    fn native_emitter_materializes_option_bool_unwrap_copy_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_option_bool_unwrap.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Option<bool>.unwrap execution");
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("Option<bool>.unwrap MIR bytecode"),
        )
        .run_value()
        .expect("bytecode Option<bool>.unwrap execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(bytecode, Value::Int(42)));

        let unwrap_copy = program
            .functions()
            .get(&crate::core::NodeId("function:unwrap_copy".into()))
            .expect("unwrap_copy MIR");
        let receipt = unwrap_copy
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::VariantProject {
                    contract: Some(receipt),
                    ..
                } => Some(receipt),
                _ => None,
            })
            .expect("Copy bool unwrap must carry a read-only projection receipt");
        assert_eq!(receipt.variant_name, "Some");
        assert_eq!(
            receipt.projection.ownership,
            crate::core::mir::types::MirOwnership::Copy
        );
        assert_eq!(
            receipt.projection.move_out_glue,
            crate::core::mir::types::MirGlueKind::Noop
        );

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_option_bool_unwrap_test");
        generator
            .compile_mir_native(&program)
            .expect("Option<bool>.unwrap MIR should have a native contract");
        generator
            .module
            .verify()
            .expect("native Option<bool>.unwrap module verifies");
        assert!(generator.module.get_function("unwrap_copy").is_some());
    }

    #[test]
    fn native_emitter_materializes_option_i64_unwrap_copy_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_option_i64_unwrap.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Option<i64>.unwrap execution");
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("Option<i64>.unwrap MIR bytecode"),
        )
        .run_value()
        .expect("bytecode Option<i64>.unwrap execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        assert!(matches!(bytecode, Value::Int(41)));

        let unwrap_copy = program
            .functions()
            .get(&crate::core::NodeId("function:unwrap_copy".into()))
            .expect("unwrap_copy MIR");
        let receipt = unwrap_copy
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::VariantProject {
                    contract: Some(receipt),
                    ..
                } => Some(receipt),
                _ => None,
            })
            .expect("Copy i64 unwrap must carry a read-only projection receipt");
        assert_eq!(
            receipt.projection.ownership,
            crate::core::mir::types::MirOwnership::Copy
        );
        assert_eq!(
            receipt.projection.move_out_glue,
            crate::core::mir::types::MirGlueKind::Noop
        );

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_option_i64_unwrap_test");
        generator
            .compile_mir_native(&program)
            .expect("Option<i64>.unwrap MIR should have a native contract");
        generator
            .module
            .verify()
            .expect("native Option<i64>.unwrap module verifies");
        assert!(generator.module.get_function("unwrap_copy").is_some());
    }

    #[test]
    fn native_emitter_materializes_option_f64_unwrap_copy_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_option_f64_unwrap.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Option<f64>.unwrap execution");
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("Option<f64>.unwrap MIR bytecode"),
        )
        .run_value()
        .expect("bytecode Option<f64>.unwrap execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(bytecode, Value::Int(42)));

        let unwrap_copy = program
            .functions()
            .get(&crate::core::NodeId("function:unwrap_copy".into()))
            .expect("unwrap_copy MIR");
        let receipt = unwrap_copy
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::VariantProject {
                    contract: Some(receipt),
                    ..
                } => Some(receipt),
                _ => None,
            })
            .expect("Copy f64 unwrap must carry a read-only projection receipt");
        assert_eq!(
            receipt.projection.ownership,
            crate::core::mir::types::MirOwnership::Copy
        );
        assert_eq!(
            receipt.projection.move_out_glue,
            crate::core::mir::types::MirGlueKind::Noop
        );

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_option_f64_unwrap_test");
        generator
            .compile_mir_native(&program)
            .expect("Option<f64>.unwrap MIR should have a native contract");
        generator
            .module
            .verify()
            .expect("native Option<f64>.unwrap module verifies");
        assert!(generator.module.get_function("unwrap_copy").is_some());
    }

    #[test]
    fn native_emitter_materializes_f64_unary_negate_copy_contract() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_f64_negate.mimi"
        ));
        let negate = crate::core::NodeId("function:negate".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&negate, &[MirRuntimeValue::FloatBits(2.5f64.to_bits())])
            .expect("reference f64 negate execution");
        assert_eq!(reference, MirRuntimeValue::FloatBits((-2.5f64).to_bits()));
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("f64 negate MIR bytecode"))
                .run_value()
                .expect("bytecode f64 negate execution");
        assert!(matches!(bytecode, Value::Int(42)));
        crate::verifier::validate_mir_capabilities(&program)
            .expect("verifier capability for f64 negate MIR");

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_f64_negate_test");
        generator
            .compile_mir_native(&program)
            .expect("f64 unary negate MIR should have a native contract");
        generator
            .module
            .verify()
            .expect("native f64 negate module verifies");
        assert!(generator.module.get_function("negate").is_some());
    }

    #[test]
    fn native_f32_const_decodes_shared_f64_literal_bits() {
        // `ResolvedLiteral::FloatBits` is always the shared semantic f64 slot.
        // Forge the direct f32-constant shape that surface lowering normally
        // spells as f64 Const + Float64ToFloat32 Convert, then require all
        // three MIR consumers to preserve the same value. This protects the
        // physical f32 ABI path used by canonical scalar FFI arguments.
        let program = canonical_program(
            "func consume(value: f32) -> i32 { 42 }\nfunc main() -> i32 { let value = 1.5 as f32; consume(value) }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let mut functions = program.functions().clone();
        let main = functions.get_mut(&owner).expect("main MIR");
        let mut replaced = false;
        for block in main.blocks.values_mut() {
            for instruction in &mut block.instructions {
                let MirInstructionKind::Convert { result, .. } = &instruction.kind else {
                    continue;
                };
                let result = result.clone();
                instruction.kind = MirInstructionKind::Const {
                    result,
                    literal: ResolvedLiteral::float(1.5),
                };
                replaced = true;
                break;
            }
            if replaced {
                break;
            }
        }
        assert!(replaced, "f32 fixture must contain a narrowing conversion");
        let forged = MirProgram::with_type_catalog(functions, program.type_catalog().clone())
            .expect("direct f32 constant remains a valid canonical MIR shape");

        let reference = MirReferenceInterpreter::new(&forged)
            .execute(&owner, &[])
            .expect("reference direct f32 constant execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));

        let bytecode = BytecodeVM::new(
            compile_mir_program(&forged).expect("bytecode direct f32 constant lowering"),
        )
        .run_value()
        .expect("bytecode direct f32 constant execution");
        assert!(matches!(bytecode, Value::Int(42)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_f32_const_bits_test");
        generator
            .compile_mir_native(&forged)
            .expect("native direct f32 constant lowering");
        generator
            .module
            .verify()
            .expect("native direct f32 constant module verifies");
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("native direct f32 constant execution");
        assert_eq!(native.exit_code, Some(42));
        assert_eq!(native.stdout, "");
        assert_eq!(native.stderr, "");
    }

    #[test]
    fn native_emitter_materializes_f64_add_finite_only_contract() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_f64_add.mimi"
        ));
        let add = crate::core::NodeId("function:add".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(
                &add,
                &[
                    MirRuntimeValue::FloatBits(1.25f64.to_bits()),
                    MirRuntimeValue::FloatBits(2.75f64.to_bits()),
                ],
            )
            .expect("reference f64 add execution");
        assert_eq!(reference, MirRuntimeValue::FloatBits(4.0f64.to_bits()));
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("f64 add MIR bytecode"))
                .run_value()
                .expect("bytecode f64 add execution");
        assert!(matches!(bytecode, Value::Int(42)));
        crate::verifier::validate_mir_capabilities(&program)
            .expect("verifier capability for f64 add MIR");

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_f64_add_test");
        generator
            .compile_mir_native(&program)
            .expect("f64 add MIR should have a native contract");
        generator
            .module
            .verify()
            .expect("native f64 add module verifies");
        assert!(generator.module.get_function("add").is_some());
    }

    #[test]
    fn native_emitter_materializes_f64_subtract_finite_only_contract() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_f64_subtract.mimi"
        ));
        let subtract = crate::core::NodeId("function:subtract".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(
                &subtract,
                &[
                    MirRuntimeValue::FloatBits(5.5f64.to_bits()),
                    MirRuntimeValue::FloatBits(2.25f64.to_bits()),
                ],
            )
            .expect("reference f64 subtract execution");
        assert_eq!(reference, MirRuntimeValue::FloatBits(3.25f64.to_bits()));
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("f64 subtract MIR bytecode"))
                .run_value()
                .expect("bytecode f64 subtract execution");
        assert!(matches!(bytecode, Value::Int(42)));
        crate::verifier::validate_mir_capabilities(&program)
            .expect("verifier capability for f64 subtract MIR");

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_f64_subtract_test");
        generator
            .compile_mir_native(&program)
            .expect("f64 subtract MIR should have a native contract");
        generator
            .module
            .verify()
            .expect("native f64 subtract module verifies");
        assert!(generator.module.get_function("subtract").is_some());
    }

    #[test]
    fn native_validator_rejects_f64_binary_shapes_outside_finite_only_contract() {
        // R6-1067 restatement: f64 Multiply/Divide compile natively now;
        // the surface cannot express float remainder (E0202), so the
        // validator-level Remainder rejection is pinned directly on the
        // TypeDesc catalog in core::mir::tests instead.  This test keeps
        // the positive admission face: the emitter accepts the multiply
        // program and declares its named helper.
        let program =
            canonical_program("func multiply_f64() -> f64 { 1.0 * 2.0 }\nfunc main() -> i32 { 0 }");
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_f64_binary_rejected_test");
        generator
            .compile_mir_native(&program)
            .expect("R6-1067 admits f64 Multiply into the native emitter");
        assert!(generator.module.get_function("multiply_f64").is_some());
    }

    #[test]
    fn native_scalar_consumers_require_complete_shape() {
        let program = canonical_program(
            "func id(value: i64) -> i64 { value }\nfunc main() -> i64 { id(41 as i64) }",
        );
        let id_owner = crate::core::NodeId("function:id".into());
        let id_function = program.functions().get(&id_owner).expect("id function");
        let scalar_id = id_function
            .parameters
            .first()
            .and_then(|value| id_function.values.get(value))
            .map(|value| value.ty.clone())
            .expect("id scalar parameter TypeDesc");
        let descriptor = program
            .type_catalog()
            .get(&scalar_id)
            .expect("canonical scalar TypeDesc")
            .clone();
        let mut forged_descriptor = descriptor;
        forged_descriptor.kind = MirTypeKind::Nominal;
        let mut catalog = program.type_catalog().clone();
        catalog.replace_for_test_only(scalar_id.clone(), forged_descriptor);
        let forged_program = MirProgram::with_type_catalog(program.functions().clone(), catalog)
            .expect("forged native catalog remains structurally valid");
        let context = Context::create();
        let direct_error =
            super::native_basic_type(&context, forged_program.type_catalog(), &scalar_id)
                .expect_err("malformed Copy scalar must fail native ABI materialization");
        assert!(
            format!("{direct_error:?}").contains("complete Copy scalar TypeDesc contract"),
            "{direct_error:?}"
        );
        let mut generator = CodeGenerator::new(&context, "mir_native_scalar_shape_rejected_test");
        let diagnostics = generator
            .compile_mir_native(&forged_program)
            .expect_err("malformed Copy scalar must fail before LLVM emission");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic
                    .message
                    .contains("complete Copy scalar TypeDesc contract")
            }),
            "{diagnostics:?}"
        );
        assert!(generator.module.get_function("id").is_none());
    }

    #[test]
    fn native_aggregate_materialization_requires_complete_copy_metadata() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_record_copy.mimi"
        ));
        let record_id = program
            .type_catalog()
            .iter()
            .find_map(|(ty, descriptor)| {
                (matches!(descriptor.layout, MirLayout::Record { .. })
                    && descriptor.ownership == crate::core::mir::types::MirOwnership::Copy)
                    .then(|| ty.clone())
            })
            .expect("Copy record TypeDesc");
        let descriptor = program
            .type_catalog()
            .get(&record_id)
            .expect("Copy record descriptor")
            .clone();
        let context = Context::create();
        super::native_basic_type(&context, program.type_catalog(), &record_id)
            .expect("canonical Copy record should materialize as a native aggregate");

        for mutation in 0..4 {
            let mut catalog = program.type_catalog().clone();
            let mut forged = descriptor.clone();
            match mutation {
                0 => forged.session_protocol = Some(record_id.clone()),
                1 => forged.needs_drop_glue = true,
                2 => {
                    forged.drop_plan =
                        Some(crate::core::mir::types::MirDropGluePlan { fields: Vec::new() })
                }
                3 => forged.variant_drop_plan = Some(Vec::new()),
                _ => unreachable!(),
            }
            catalog.replace_for_test_only(record_id.clone(), forged);
            let error = super::native_basic_type(&context, &catalog, &record_id)
                .expect_err("forged Copy aggregate metadata must fail native materialization");
            assert!(
                format!("{error:?}").contains("complete no-op metadata contract"),
                "unexpected native aggregate error: {error:?}"
            );
        }
    }

    #[test]
    fn native_generic_variant_abi_requires_complete_copy_metadata() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_distinct_unwrap_or.mimi"
        ));
        let instance = program
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
            .expect("generic heterogeneous Result fallback projection instance");
        let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
            contract,
        } = &instance.contract
        else {
            unreachable!("filtered above");
        };
        let source_ty = contract.source_ty.clone();
        let descriptor = program
            .type_catalog()
            .get(&source_ty)
            .expect("generic Result TypeDesc")
            .clone();
        assert_eq!(
            descriptor.ownership,
            crate::core::mir::types::MirOwnership::Copy
        );
        super::native_variant_abi_with_generic_result(
            program.type_catalog(),
            &source_ty,
            false,
            true,
        )
        .expect("canonical generic Result variant ABI");

        for mutation in 0..4 {
            let mut catalog = program.type_catalog().clone();
            let mut forged = descriptor.clone();
            match mutation {
                0 => forged.session_protocol = Some(source_ty.clone()),
                1 => forged.needs_drop_glue = true,
                2 => {
                    forged.drop_plan =
                        Some(crate::core::mir::types::MirDropGluePlan { fields: Vec::new() })
                }
                3 => forged.variant_drop_plan = Some(Vec::new()),
                _ => unreachable!(),
            }
            catalog.replace_for_test_only(source_ty.clone(), forged);
            let error =
                super::native_variant_abi_with_generic_result(&catalog, &source_ty, false, true)
                    .expect_err("forged generic Result metadata must fail native variant ABI");
            assert!(
                format!("{error:?}").contains("complete no-op metadata contract"),
                "unexpected native variant ABI error: {error:?}"
            );
        }
    }

    #[test]
    fn bytecode_option_i32_unwrap_none_matches_reference_trap() {
        let program = canonical_program(
            "func main() -> i32 { let value: Option<i32> = None; value.unwrap() }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect_err("reference Option<i32>.unwrap(None) must trap");
        let bytecode = compile_mir_program(&program).expect("Option<i32> unwrap(None) bytecode");
        let bytecode_error = BytecodeVM::new(bytecode)
            .run_value()
            .expect_err("bytecode Option<i32>.unwrap(None) must trap");
        assert!(reference.to_string().contains("E0800"));
        assert_eq!(bytecode_error.code(), "E0800");
    }

    #[test]
    fn bytecode_option_bool_unwrap_none_matches_reference_trap() {
        let program = canonical_program(
            "func main() -> i32 { let value: Option<bool> = None; let flag = value.unwrap(); if flag { 42 } else { 0 } }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect_err("reference Option<bool>.unwrap(None) must trap");
        let bytecode = compile_mir_program(&program).expect("Option<bool> unwrap(None) bytecode");
        let bytecode_error = BytecodeVM::new(bytecode)
            .run_value()
            .expect_err("bytecode Option<bool>.unwrap(None) must trap");
        assert!(reference.to_string().contains("E0800"));
        assert_eq!(bytecode_error.code(), "E0800");
    }

    #[test]
    fn bytecode_option_i64_unwrap_none_matches_reference_trap() {
        let program = canonical_program(
            "func main() -> i64 { let value: Option<i64> = None; value.unwrap() }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect_err("reference Option<i64>.unwrap(None) must trap");
        let bytecode = compile_mir_program(&program).expect("Option<i64> unwrap(None) bytecode");
        let bytecode_error = BytecodeVM::new(bytecode)
            .run_value()
            .expect_err("bytecode Option<i64>.unwrap(None) must trap");
        assert!(reference.to_string().contains("E0800"));
        assert_eq!(bytecode_error.code(), "E0800");
    }

    #[test]
    fn bytecode_option_f64_unwrap_none_matches_reference_trap() {
        let program = canonical_program(
            "func unwrap_none_f64() -> f64 { let value: Option<f64> = None; value.unwrap() }\nfunc main() -> i32 { 0 }",
        );
        let owner = crate::core::NodeId("function:unwrap_none_f64".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect_err("reference Option<f64>.unwrap(None) must trap");
        let mut bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("Option<f64> unwrap(None) bytecode"),
        );
        let bytecode_error = bytecode
            .call_named("unwrap_none_f64", vec![])
            .expect_err("bytecode Option<f64>.unwrap(None) must trap");
        assert!(reference.to_string().contains("E0800"));
        assert_eq!(bytecode_error.code(), "E0800");
    }

    #[test]
    fn bytecode_option_string_unwrap_none_matches_reference_trap() {
        let program = canonical_program(
            "func main() -> i32 { let value: Option<string> = None; let text = value.unwrap(); drop(text); 41 }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect_err("reference unwrap(None) must trap");
        let bytecode = compile_mir_program(&program).expect("unwrap(None) MIR bytecode");
        let bytecode_error = BytecodeVM::new(bytecode)
            .run_value()
            .expect_err("bytecode unwrap(None) must trap");
        assert!(reference.to_string().contains("E0800"));
        assert_eq!(bytecode_error.code(), "E0800");
    }

    #[test]
    fn native_emitter_consumes_shared_flat_copy_variant_contract() {
        for (source, expected, module_name) in [
            (
                include_str!("../../../tests/fixtures/mir_native_option_copy.mimi"),
                42,
                "mir_native_flat_copy_option_test",
            ),
            (
                include_str!("../../../tests/fixtures/mir_native_result_copy.mimi"),
                8,
                "mir_native_flat_copy_result_test",
            ),
        ] {
            let program = canonical_program(source);
            for ty in program
                .type_catalog()
                .iter()
                .filter_map(|(ty, descriptor)| {
                    matches!(
                        descriptor.layout,
                        crate::core::mir::types::MirLayout::Option { .. }
                            | crate::core::mir::types::MirLayout::Result { .. }
                    )
                    .then_some(ty.clone())
                })
            {
                program
                    .type_catalog()
                    .validate_flat_copy_variant(&ty)
                    .expect("shared flat Copy variant TypeDesc contract");
            }
            let owner = crate::core::NodeId("function:main".into());
            let reference = MirReferenceInterpreter::new(&program)
                .execute(&owner, &[])
                .expect("reference flat Copy variant execution");
            assert_eq!(reference, MirRuntimeValue::Int(expected));
            let bytecode = BytecodeVM::new(
                compile_mir_program(&program).expect("flat Copy variant MIR bytecode"),
            )
            .run_value()
            .expect("bytecode flat Copy variant execution");
            assert!(matches!(bytecode, Value::Int(value) if value == expected));

            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, module_name);
            generator
                .compile_mir_native(&program)
                .expect("native flat Copy variant lowering");
            generator
                .module
                .verify()
                .expect("native flat Copy variant module verifies");
            assert!(generator.module.get_function("main").is_some());
        }
    }

    #[test]
    fn native_emitter_materializes_flat_copy_variant_predicates() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_variant_predicate.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference variant predicate execution");
        assert_eq!(reference, MirRuntimeValue::Int(4));
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("variant predicate MIR bytecode"))
                .run_value()
                .expect("bytecode variant predicate execution");
        assert!(matches!(bytecode, Value::Int(4)));

        let predicate_count = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::VariantPredicate {
                        contract: Some(_),
                        ..
                    }
                )
            })
            .count();
        assert_eq!(predicate_count, 4);

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_variant_predicate_test");
        generator
            .compile_mir_native(&program)
            .expect("native variant predicate lowering");
        generator
            .module
            .verify()
            .expect("native variant predicate module verifies");
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_emitter_materializes_flat_copy_variant_call_abi() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_variant_call.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference direct variant call execution");
        assert_eq!(reference, MirRuntimeValue::Int(4));
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("variant call MIR bytecode"))
                .run_value()
                .expect("bytecode direct variant call execution");
        assert!(matches!(bytecode, Value::Int(4)));

        let call_count = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::Call {
                        variant_call_contract: Some(_),
                        ..
                    }
                )
            })
            .count();
        assert_eq!(call_count, 2);

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_variant_call_test");
        generator
            .compile_mir_native(&program)
            .expect("native direct variant call lowering");
        generator
            .module
            .verify()
            .expect("native direct variant call module verifies");
        assert!(generator.module.get_function("choose").is_some());
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_emitter_materializes_option_string_switch_move_and_matches_oracles() {
        let program = canonical_program(
            "func consume_text(text: string) -> i32 { drop(text); 41 }\nfunc consume(value: Option<string>) -> i32 { match value { Some(text) => consume_text(text), None => 0 } }\nfunc discard(value: Option<string>) -> i32 { match value { Some(_) => 7, None => 8 } }\nfunc main() -> i32 { let first: Option<string> = Some(\"owned\"); let second: Option<string> = Some(\"discard\"); let a = consume(first); let b = discard(second); a + b }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Option<string> switch-move execution");
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("Option<string> MIR bytecode"))
                .run_value()
                .expect("bytecode Option<string> switch-move execution");
        assert_eq!(reference, MirRuntimeValue::Int(48));
        assert!(matches!(bytecode, Value::Int(48)));
        let switch_move_count = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .filter(|block| {
                matches!(
                    block.terminator,
                    crate::core::mir::MirTerminator::SwitchMove { .. }
                )
            })
            .count();
        assert_eq!(switch_move_count, 2);

        let (option_string_ty, string_ty) = program
            .type_catalog()
            .iter()
            .find_map(|(id, descriptor)| match &descriptor.layout {
                crate::core::mir::types::MirLayout::Option { inner, .. }
                    if descriptor.ownership == crate::core::mir::types::MirOwnership::Move =>
                {
                    Some((id.clone(), inner.clone()))
                }
                _ => None,
            })
            .expect("canonical Option<string> TypeDesc");
        let (variant_abi, payload_ty) =
            super::native_variant_abi(program.type_catalog(), &option_string_ty, true)
                .expect("native variant ABI contract");
        assert_eq!(variant_abi.tag_field, 0);
        assert_eq!(variant_abi.payload_field, 1);
        assert_eq!(payload_ty, string_ty);
        let unsupported = super::native_variant_abi(program.type_catalog(), &string_ty, true)
            .expect_err("non-variant must not enter native variant ABI");
        assert!(unsupported
            .message
            .contains("native non-Copy Option<string> variant contract"));

        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_option_string_switch_move_test");
        generator
            .compile_mir_native(&program)
            .expect("Option<string> SwitchMove should have a native contract");
        generator
            .module
            .verify()
            .expect("native Option<string> SwitchMove module verifies");
        assert!(generator.module.get_function("consume").is_some());
        assert!(generator.module.get_function("discard").is_some());
    }

    #[test]
    fn native_emitter_materializes_result_string_i32_switch_and_matches_oracles() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_verifier_result_string_i32_switch_move.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Result<string, i32> SwitchMove execution");
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("Result<string, i32> MIR bytecode"),
        )
        .run_value()
        .expect("Result<string, i32> MIR-bytecode execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(bytecode, Value::Int(42)));

        let result_ty = program
            .type_catalog()
            .iter()
            .find_map(|(id, descriptor)| {
                matches!(
                    descriptor.layout,
                    crate::core::mir::types::MirLayout::Result { .. }
                )
                .then_some(id.clone())
            })
            .expect("canonical Result TypeDesc");
        program
            .type_catalog()
            .validate_result_string_i32_variant(&result_ty)
            .expect("Result native TypeDesc contract");
        let (variant_abi, first_payload) =
            super::native_variant_abi(program.type_catalog(), &result_ty, true)
                .expect("Result native ABI contract");
        assert_eq!(variant_abi.tag_field, 0);
        assert_eq!(variant_abi.payload_types.len(), 2);
        assert_eq!(variant_abi.payload_field, 1);
        assert_eq!(first_payload, variant_abi.payload_types[0]);
        assert_eq!(variant_abi.payload_fields.len(), 2);
        assert_eq!(
            variant_abi
                .payload_slot(&crate::core::NodeId("builtin:variant:Result::Ok".into()))
                .expect("Result Ok payload slot")
                .physical_field,
            1
        );
        assert_eq!(
            variant_abi
                .payload_slot(&crate::core::NodeId("builtin:variant:Result::Err".into()))
                .expect("Result Err payload slot")
                .physical_field,
            2
        );

        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_result_string_i32_switch_test");
        generator
            .compile_mir_native(&program)
            .expect("Result<string, i32> SwitchMove should have a native contract");
        generator
            .module
            .verify()
            .expect("native Result<string, i32> module verifies");
        assert!(generator.module.get_function("consume").is_some());
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_emitter_materializes_result_string_i32_clone_and_drop_glue() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_result_string_i32_glue.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Result<string, i32> glue execution");
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("Result<string, i32> glue MIR bytecode"),
        )
        .run_value()
        .expect("Result<string, i32> glue MIR-bytecode execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(bytecode, Value::Int(42)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_result_string_i32_glue_test");
        generator
            .compile_mir_native(&program)
            .expect("Result<string, i32> clone/drop should have a native contract");
        generator
            .module
            .verify()
            .expect("native Result<string, i32> glue module verifies");
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_emitter_materializes_move_owned_result_call_and_matches_oracles() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_result_string_i32_call_return.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference move-owned Result call execution");
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("move-owned Result call MIR bytecode"),
        )
        .run_value()
        .expect("move-owned Result call MIR-bytecode execution");
        assert_eq!(reference, MirRuntimeValue::Int(48));
        assert!(matches!(bytecode, Value::Int(48)));

        let calls = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .filter_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::Call {
                    variant_call_contract: Some(receipt),
                    ..
                } => Some(receipt),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|receipt| {
            receipt.mode == crate::core::mir::types::MirVariantCallAbiMode::MoveOwned
                && receipt.payload_types.len() == 2
        }));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_move_owned_result_call_test");
        generator
            .compile_mir_native(&program)
            .expect("native move-owned Result call must consume its ABI receipt");
        generator
            .module
            .verify()
            .expect("native move-owned Result call module verifies");
        for function in ["make_ok", "make_err", "use_ok", "use_err"] {
            assert!(
                generator.module.get_function(function).is_some(),
                "{function}"
            );
        }
    }

    #[test]
    fn native_emitter_materializes_move_owned_result_list_call_and_matches_oracles() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_result_list_i32_call_return.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference move-owned Result<List<i32>, i32> call execution");
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("move-owned Result<List<i32>, i32> MIR bytecode"),
        )
        .run_value()
        .expect("move-owned Result<List<i32>, i32> MIR-bytecode execution");
        assert_eq!(reference, MirRuntimeValue::Int(48));
        assert!(matches!(bytecode, Value::Int(48)));

        let calls = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .filter_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::Call {
                    variant_call_contract: Some(receipt),
                    ..
                } => Some(receipt),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|receipt| {
            receipt.mode == crate::core::mir::types::MirVariantCallAbiMode::MoveOwned
                && receipt.return_mode
                    == crate::core::mir::types::MirVariantCallReturnMode::OwnershipPathExclusiveMerge
                && receipt
                    .payload_types
                    .first()
                    .and_then(|ty| program.type_catalog().get(ty))
                    .is_some_and(|desc| matches!(desc.kind, crate::core::mir::types::MirTypeKind::List))
        }));

        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_move_owned_result_list_call_test");
        generator
            .compile_mir_native(&program)
            .expect("native move-owned Result<List<i32>, i32> call must consume its ABI receipt");
        generator
            .module
            .verify()
            .expect("native move-owned Result<List<i32>, i32> call module verifies");
        assert!(generator
            .module
            .get_function("mimi_mir_list_drop_scalar")
            .is_some());
        for function in ["make_ok", "make_err", "use_ok", "use_err"] {
            assert!(
                generator.module.get_function(function).is_some(),
                "{function}"
            );
        }
    }

    #[test]
    fn native_emitter_materializes_move_owned_result_i64_bool_list_calls() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_result_list_i64_bool_call_return.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Result<List<i64|bool>, i32> call execution");
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program)
                .expect("move-owned Result<List<i64|bool>, i32> MIR bytecode"),
        )
        .run_value()
        .expect("move-owned Result<List<i64|bool>, i32> MIR-bytecode execution");
        assert_eq!(reference, MirRuntimeValue::Int(48));
        assert!(matches!(bytecode, Value::Int(48)));

        let calls = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .filter_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::Call {
                    variant_call_contract: Some(receipt),
                    ..
                } => Some(receipt),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|receipt| {
            receipt.mode == crate::core::mir::types::MirVariantCallAbiMode::MoveOwned
                && receipt.return_mode
                    == crate::core::mir::types::MirVariantCallReturnMode::OwnershipPathExclusiveMerge
        }));

        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_move_owned_result_i64_bool_list_call_test",
        );
        generator
            .compile_mir_native(&program)
            .expect("native Result<List<i64|bool>, i32> call must consume its ABI receipt");
        generator
            .module
            .verify()
            .expect("native Result<List<i64|bool>, i32> call module verifies");
        for function in ["make_i64", "make_bool", "use_i64", "use_bool", "main"] {
            assert!(
                generator.module.get_function(function).is_some(),
                "{function}"
            );
        }
    }

    #[test]
    fn native_emitter_materializes_move_owned_result_i64_bool_err_calls() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_result_list_i64_bool_err_call_return.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Result<List<i64|bool>, i32> Err-path execution");
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("move-owned Result Err-path MIR bytecode"),
        )
        .run_value()
        .expect("move-owned Result Err-path MIR-bytecode execution");
        assert_eq!(reference, MirRuntimeValue::Int(56));
        assert!(matches!(bytecode, Value::Int(56)));

        let calls = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .filter_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::Call {
                    variant_call_contract: Some(receipt),
                    ..
                } => Some(receipt),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 4);
        assert!(calls.iter().all(|receipt| {
            receipt.mode == crate::core::mir::types::MirVariantCallAbiMode::MoveOwned
                && receipt.return_mode
                    == crate::core::mir::types::MirVariantCallReturnMode::OwnershipPathExclusiveMerge
        }));

        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_move_owned_result_i64_bool_err_call_test",
        );
        generator
            .compile_mir_native(&program)
            .expect("native Err-path Result call must consume its ABI receipt");
        generator
            .module
            .verify()
            .expect("native Err-path Result call module verifies");
        for function in [
            "make_i64_ok",
            "make_i64_err",
            "make_bool_ok",
            "make_bool_err",
            "use_i64_ok",
            "use_i64_err",
            "use_bool_ok",
            "use_bool_err",
            "main",
        ] {
            assert!(
                generator.module.get_function(function).is_some(),
                "{function}"
            );
        }
    }

    #[test]
    fn native_emitter_merges_move_owned_result_call_return_paths() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_result_string_i32_call_return_multipath.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference move-owned Result multi-path execution");
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("move-owned Result multi-path MIR bytecode"),
        )
        .run_value()
        .expect("move-owned Result multi-path MIR-bytecode execution");
        assert_eq!(reference, MirRuntimeValue::Int(48));
        assert!(matches!(bytecode, Value::Int(48)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_move_owned_result_multipath");
        generator
            .compile_mir_native(&program)
            .expect("native move-owned Result multi-path call must use its MIR contract");
        generator
            .module
            .verify()
            .expect("native move-owned Result multi-path module verifies");
        for function in ["choose", "checked", "main"] {
            assert!(
                generator.module.get_function(function).is_some(),
                "{function}"
            );
        }
    }

    #[test]
    fn native_validator_rejects_result_string_string_before_llvm_declarations() {
        let program = canonical_program(
            "func main() -> i32 { let value: Result<string, string> = Ok(\"owned\"); drop(value); 42 }",
        );
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_result_string_string_test");
        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("unsupported Result payload must remain fail-closed");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("native non-Copy Result<string, i32> variant contract")
        }));
        assert!(
            generator.module.get_function("main").is_none(),
            "unsupported Result must be rejected before LLVM declarations"
        );
    }

    #[test]
    fn native_validator_rejects_non_copy_switch_move_default_before_llvm_declarations() {
        let program = canonical_program(
            "func match_option_string() -> string { let value: Option<string> = Some(\"owned\"); match value { Some(text) => text, _ => \"fallback\" } }\nfunc main() -> i32 { 0 }",
        );
        let owner = crate::core::NodeId("function:match_option_string".into());
        assert!(program.functions().get(&owner).is_some_and(|function| {
            function.blocks.values().any(|block| {
                matches!(
                    block.terminator,
                    crate::core::mir::MirTerminator::SwitchMove { .. }
                )
            })
        }));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_option_string_switch_default_test");

        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("default consuming switch must remain fail-closed in native slice");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("requires explicit variant arms; default/literal cases are not covered")
        }));
        assert!(
            generator.module.get_function("main").is_none(),
            "unsupported consuming switch must be rejected before LLVM declarations"
        );
    }

    #[test]
    fn native_emitter_materializes_move_project_before_llvm_declarations() {
        let program = canonical_program(
            "type Named { name: string, count: i32 }\nfunc main() -> i32 { let value = Named { name: \"x\", count: 41 }; let name = value.name; drop(name); 42 }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference MoveProject execution");
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("MoveProject MIR bytecode"))
                .run_value()
                .expect("MIR bytecode MoveProject execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(bytecode, Value::Int(42)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_move_project_test");

        generator
            .compile_mir_native(&program)
            .expect("MoveProject should have a native transfer contract");
        assert!(
            generator.module.get_function("main").is_some(),
            "MoveProject native function should be declared"
        );
        assert!(generator.module.get_function("mimi_string_free").is_some());
        generator
            .module
            .verify()
            .expect("native MoveProject module verifies");
    }

    #[test]
    fn native_validator_rejects_non_copy_record_update_before_llvm_declarations() {
        let program = canonical_program(
            "type Box { text: string, count: i32 }\nfunc main() -> i32 { let value = Box { count: 1, text: \"x\" }; let updated = Box { count: 2, ..value }; drop(updated); 0 }",
        );
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_record_update_validator_test");

        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("non-Copy record update must fail closed in native slice");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("non-Copy record update requires an explicit transfer/update contract")
        }));
        assert!(
            generator.module.get_function("main").is_none(),
            "non-Copy record update must be rejected before LLVM declarations"
        );
    }

    #[test]
    fn native_record_update_matches_reference_and_mir_bytecode() {
        let source = "type Point { x: i32, enabled: bool }\nfunc main() -> i32 { let point = Point { enabled: true, x: 40 }; let updated = Point { x: 42, ..point }; if updated.enabled { updated.x } else { 0 } }";
        let program = canonical_program(source);
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference record update execution");
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("record update MIR bytecode"))
                .run_value()
                .expect("MIR bytecode record update execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(bytecode, Value::Int(42)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_record_update_test");
        generator
            .compile_mir_native(&program)
            .expect("flat Copy record update should have a native contract");
        assert!(generator.module.get_function("main").is_some());
        generator
            .module
            .verify()
            .expect("native record update module verifies");
        let update_count = program
            .functions()
            .get(&owner)
            .into_iter()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::UpdateRecord { .. }
                )
            })
            .count();
        assert_eq!(update_count, 1, "fixture must exercise UpdateRecord");
    }

    #[test]
    fn canonical_mir_rejects_nested_string_list_before_any_backend() {
        let tokens = Lexer::new("func main() -> i32 { let values = [[\"x\"]]; drop(values); 0 }")
            .tokenize()
            .expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("nested String List must fail before any backend");
        let crate::core::mir::reference::MirProgramBuildError::Validation(errors) = error else {
            panic!("unexpected canonical List rejection: {error:?}");
        };
        assert!(errors
            .iter()
            .any(|error| error.message.contains("outside the canonical Copy scalar")));
    }

    #[test]
    fn native_emitter_accepts_scalar_list_projection_contract() {
        let program =
            canonical_program("func main() -> i32 { let values = [10, 20, 30]; values[1] }");
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_list_emitter_test");

        generator
            .compile_mir_native(&program)
            .expect("scalar List MIR should have a native contract");
        assert!(generator.module.get_function("main").is_some());
        assert!(generator
            .module
            .get_function("mimi_mir_list_get_scalar")
            .is_some());
        generator
            .module
            .verify()
            .expect("native List module verifies");
    }

    #[test]
    fn native_emitter_materializes_scalar_list_len_adapter() {
        let program = canonical_program(
            "func main() -> i32 { let values = [10, 20, 30]; let count = len(values); drop(values); count }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let function = program.functions().get(&owner).expect("List.len main MIR");
        assert!(function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::ListOp {
                        operation: crate::core::mir::MirListOperation::Len,
                        list_operation_contract: Some(_),
                        ..
                    }
                )
            })
        }));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_scalar_list_len_test");
        generator
            .compile_mir_native(&program)
            .expect("scalar List.len MIR should have a canonical ABI adapter");
        generator
            .module
            .verify()
            .expect("native List.len module verifies");
        assert!(generator.module.get_function("main").is_some());
        assert!(generator
            .module
            .get_function("mimi_mir_list_len_scalar")
            .is_some());
    }

    #[test]
    fn native_emitter_materializes_clone_based_scalar_list_reverse_adapter() {
        let program = canonical_program(
            "func main() -> i32 { let values = [1, 2, 3]; let reversed = reverse(values); let count = len(reversed); drop(reversed); drop(values); count }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let function = program
            .functions()
            .get(&owner)
            .expect("List.reverse main MIR");
        assert!(function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::ListOp {
                        operation: crate::core::mir::MirListOperation::Reverse,
                        list_operation_contract: Some(_),
                        ..
                    }
                )
            })
        }));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_scalar_list_reverse_test");
        generator
            .compile_mir_native(&program)
            .expect("scalar List.reverse MIR should have a canonical ABI adapter");
        generator
            .module
            .verify()
            .expect("native List.reverse module verifies");
        assert!(generator
            .module
            .get_function("mimi_mir_list_reverse_scalar")
            .is_some());
    }

    #[test]
    fn native_emitter_materializes_scalar_list_reverse_method_as_the_same_mir_op() {
        let program = canonical_program(
            "func main() -> i32 { let values = [1, 2, 3]; let reversed = values.reverse(); let count = len(reversed); drop(reversed); drop(values); count }",
        );
        let function = program
            .functions()
            .get(&crate::core::NodeId("function:main".into()))
            .expect("List.reverse method main MIR");
        assert!(function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::ListOp {
                        operation: crate::core::mir::MirListOperation::Reverse,
                        list_operation_contract: Some(_),
                        ..
                    }
                )
            })
        }));

        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_scalar_list_reverse_method_test");
        generator
            .compile_mir_native(&program)
            .expect("scalar List.reverse method should use the canonical ABI adapter");
        generator
            .module
            .verify()
            .expect("native List.reverse method module verifies");
    }

    #[test]
    fn native_emitter_materializes_scalar_list_concat_method_with_two_input_move_abi() {
        let program = canonical_program(
            "func main() -> i32 { let left = [1, 2]; let right = [3, 4]; let joined = left.concat(right); let count = len(joined); drop(joined); count }",
        );
        let function = program
            .functions()
            .get(&crate::core::NodeId("function:main".into()))
            .expect("List.concat method main MIR");
        assert!(function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::ListOp {
                        operation: crate::core::mir::MirListOperation::Concat,
                        argument: Some(_),
                        list_operation_contract: Some(_),
                        ..
                    }
                )
            })
        }));

        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_scalar_list_concat_method_test");
        generator
            .compile_mir_native(&program)
            .expect("scalar List.concat method should use the canonical two-input ABI adapter");
        generator
            .module
            .verify()
            .expect("native List.concat method module verifies");
        assert!(generator
            .module
            .get_function("mimi_mir_list_concat_scalar")
            .is_some());
    }

    #[test]
    fn native_emitter_materializes_owned_string_clone_move_and_drop() {
        let program = canonical_program(
            "func make_text() -> string { \"canonical\" }\nfunc consume_text(text: string) -> i32 { drop(text); 41 }\nfunc main() -> i32 { let text = make_text(); let cloned = text; drop(cloned); consume_text(text) }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference owned string execution");
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("owned string MIR bytecode"))
                .run_value()
                .expect("MIR bytecode owned string execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        assert!(matches!(bytecode, Value::Int(41)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_owned_string_test");
        generator
            .compile_mir_native(&program)
            .expect("owned String MIR should have a native glue contract");
        generator
            .module
            .verify()
            .expect("native owned String module verifies");
        assert!(generator.module.get_function("make_text").is_some());
        assert!(generator.module.get_function("consume_text").is_some());
        assert!(generator.module.get_function("mimi_str_clone").is_some());
        assert!(generator.module.get_function("mimi_string_free").is_some());
    }

    #[test]
    fn native_emitter_materializes_owned_string_move_return_contract() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_owned_string_return.mimi"
        ));
        let echo = program
            .functions()
            .get(&crate::core::NodeId("function:echo".into()))
            .expect("echo MIR function");
        assert!(echo
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .any(|instruction| matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::Move { .. }
            )));

        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference owned String return execution");
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("owned String return bytecode"))
                .run_value()
                .expect("owned String return bytecode execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(bytecode, Value::Int(42)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_owned_string_return");
        generator
            .compile_mir_native(&program)
            .expect("native owned String return must consume canonical Move");
        generator
            .module
            .verify()
            .expect("native owned String return module verifies");
        assert!(generator.module.get_function("echo").is_some());
        assert!(generator.module.get_function("mimi_string_free").is_some());
    }

    #[test]
    fn native_emitter_materializes_direct_owned_string_calls() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_verifier_owned_string_call_return.mimi"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference direct owned String call execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_owned_string_calls");
        generator
            .compile_mir_native(&program)
            .expect("direct owned String calls must use canonical ABI declarations");
        generator
            .module
            .verify()
            .expect("native direct owned String call module verifies");
        for function in ["inner", "relay", "echo", "forward"] {
            assert!(
                generator.module.get_function(function).is_some(),
                "{function}"
            );
        }
        assert!(generator.module.get_function("mimi_string_free").is_some());
    }

    #[test]
    fn native_emitter_materializes_recursive_owned_tuple_clone_move_and_drop() {
        let program = canonical_program(
            "func make_nested() -> ((string, i32), bool) { ((\"inner\", 41), true) }\nfunc consume_nested(value: ((string, i32), bool)) -> i32 { drop(value); 42 }\nfunc main() -> i32 { let value = make_nested(); let cloned = value; drop(cloned); consume_nested(value) }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference recursive tuple execution");
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("recursive tuple MIR bytecode"))
                .run_value()
                .expect("MIR bytecode recursive tuple execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(bytecode, Value::Int(42)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_recursive_tuple_test");
        generator
            .compile_mir_native(&program)
            .expect("recursive tuple MIR should have a native glue contract");
        generator
            .module
            .verify()
            .expect("native recursive tuple module verifies");
        assert!(generator.module.get_function("make_nested").is_some());
        assert!(generator.module.get_function("consume_nested").is_some());
        assert!(generator.module.get_function("mimi_str_clone").is_some());
        assert!(generator.module.get_function("mimi_string_free").is_some());
    }

    #[test]
    fn native_emitter_materializes_non_copy_record_clone_move_and_drop() {
        let program = canonical_program(
            "type Named { name: string, count: i32 }\nfunc make_named() -> Named { Named { count: 41, name: \"owned\" } }\nfunc consume_named(value: Named) -> i32 { drop(value); 42 }\nfunc main() -> i32 { let value = make_named(); let cloned = value; drop(cloned); consume_named(value) }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference non-Copy record execution");
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("non-Copy record MIR bytecode"))
                .run_value()
                .expect("MIR bytecode non-Copy record execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(bytecode, Value::Int(42)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_non_copy_record_test");
        generator
            .compile_mir_native(&program)
            .expect("non-Copy record MIR should have a native glue contract");
        generator
            .module
            .verify()
            .expect("native non-Copy record module verifies");
        assert!(generator.module.get_function("make_named").is_some());
        assert!(generator.module.get_function("consume_named").is_some());
        assert!(generator.module.get_function("mimi_str_clone").is_some());
        assert!(generator.module.get_function("mimi_string_free").is_some());
    }

    #[test]
    fn native_emitter_materializes_scalar_set_handle_island() {
        let program = canonical_program(
            "func make_values() -> Set<i32> { let values: Set<i32> = {1, 2, 1}; values }\nfunc main() -> i32 { let values = make_values(); let inserted = values.insert(3); let present = inserted.contains(2); let nonempty = !inserted.is_empty(); let removed = inserted.remove(1); let size = removed.size(); let _present = present; let _nonempty = nonempty; size }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference scalar Set execution");
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("scalar Set MIR bytecode"))
                .run_value()
                .expect("bytecode scalar Set execution");
        assert_eq!(reference, MirRuntimeValue::Int(2));
        assert!(matches!(bytecode, Value::Int(2)));

        let set_ops = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::ConstructSet { .. }
                        | crate::core::mir::MirInstructionKind::SetOp { .. }
                )
            })
            .count();
        assert!(
            set_ops >= 5,
            "fixture must exercise construct and Set operations"
        );

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_scalar_set_test");
        generator
            .compile_mir_native(&program)
            .expect("scalar Set MIR should have a native handle contract");
        generator
            .module
            .verify()
            .expect("native scalar Set module verifies");
        for runtime in [
            "mimi_set_new",
            "mimi_set_clone_scalar",
            "mimi_set_insert",
            "mimi_set_remove",
            "mimi_set_size",
            "mimi_set_contains",
            "mimi_set_destroy",
        ] {
            assert!(
                generator.module.get_function(runtime).is_some(),
                "native Set island must declare {runtime}"
            );
        }
    }

    #[test]
    fn generic_scalar_set_facade_shares_one_mir_across_reference_bytecode_native() {
        let program = canonical_program(
            "func set_size<T>(s: Set<T>) -> i32 { s.size() }\nfunc set_contains<T>(s: Set<T>, value: T) -> bool { s.contains(value) }\nfunc set_insert<T>(s: Set<T>, value: T) -> Set<T> { s.insert(value) }\nfunc set_remove<T>(s: Set<T>, value: T) -> Set<T> { s.remove(value) }\nfunc set_to_list<T>(s: Set<T>) -> List<T> { s.to_list() }\nfunc main() -> i32 { let values: Set<i32> = {1, 2, 1}; let inserted = set_insert(values, 3); if set_size(inserted) != 3 { return 1 } if !set_contains(inserted, 2) { return 2 } let removed = set_remove(inserted, 1); let list = set_to_list(removed); if len(list) != 2 { return 3 } 0 }",
        );
        assert_eq!(
            program.instances().len(),
            5,
            "all generic Set facade calls must materialize in the shared MIR"
        );
        assert!(program.instances().values().all(|instance| matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarSetFacade { .. }
        )));

        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic Set facade execution");
        assert_eq!(reference, MirRuntimeValue::Int(0));

        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("generic Set facade MIR bytecode"),
        )
        .run_value()
        .expect("bytecode generic Set facade execution");
        assert!(matches!(bytecode, Value::Int(0)));

        crate::verifier::validate_mir_capabilities(&program)
            .expect("verifier capability for generic Set facade");

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_set_facade_test");
        generator
            .compile_mir_native(&program)
            .expect("native generic Set facade lowering");
        generator
            .module
            .verify()
            .expect("native generic Set facade module verifies");
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("native generic Set facade execution");
        assert_eq!(native.stdout, "");
        assert_eq!(native.stderr, "");
        assert_eq!(native.exit_code, Some(0));
    }

    #[test]
    fn native_emitter_materializes_scalar_set_to_list_adapter() {
        let program = canonical_program(
            "func set_to_list_value() -> List<i32> { let values: Set<i32> = {3, 1, 2, 1}; values.to_list() }\nfunc main() -> i32 { 0 }",
        );
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_scalar_set_to_list_test");
        assert!(
            generator
                .module
                .get_function("mimi_mir_set_to_list_scalar")
                .is_none(),
            "canonical Set.to_list adapter must not leak into legacy runtime registration"
        );
        generator
            .compile_mir_native(&program)
            .expect("scalar Set.to_list MIR should have a canonical ABI adapter");
        generator
            .module
            .verify()
            .expect("native scalar Set.to_list module verifies");
        assert!(generator.module.get_function("set_to_list_value").is_some());
        assert!(generator
            .module
            .get_function("mimi_mir_set_to_list_scalar")
            .is_some());
        assert!(generator
            .module
            .get_function("mimi_mir_list_drop_scalar")
            .is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_scalar_generic_identity() {
        let program = canonical_program(
            "func identity<T>(value: T) -> T { value }\nfunc main() -> i32 { identity(41) }",
        );
        assert_eq!(program.instances().len(), 1);
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic identity execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_identity_test");
        generator
            .compile_mir_native(&program)
            .expect("native backend must consume the specialized MIR target");
        generator
            .module
            .verify()
            .expect("native generic identity module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_scalar_generic_tuple_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_tuple_projection.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarTupleProjection { .. }
                )
            })
            .expect("generic tuple projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarTupleProjection {
                ref contract
            } if contract.arity == 2 && contract.field_index == 0
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic tuple projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_tuple_projection");
        generator
            .compile_mir_native(&program)
            .expect("native generic tuple projection must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic tuple projection module verifies");
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_emitter_consumes_typed_session_pair_send_recv_roundtrip() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_typed_session_pair_send_recv.mimi"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference SessionPair send/recv execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_typed_session_pair_send_recv");
        generator
            .compile_mir_native(&program)
            .expect("native SessionPair send/recv must consume canonical MIR");
        generator
            .module
            .verify()
            .expect("native SessionPair send/recv module verifies");
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_scalar_generic_list_len() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_list_len.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("generic List.len instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarListFacade {
                operation: crate::core::mir::MirListOperation::Len
            }
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic List.len execution");
        assert_eq!(reference, MirRuntimeValue::Int(3));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_list_len");
        generator
            .compile_mir_native(&program)
            .expect("native generic List.len must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic List.len module verifies");
        assert!(generator
            .module
            .get_function("mimi_mir_list_len_scalar")
            .is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_option_predicate() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_predicate.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantPredicate { .. }
                )
            })
            .expect("generic Option predicate instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantPredicate { .. }
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic Option predicate execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let bytecode_program =
            compile_mir_program(&program).expect("generic Option predicate MIR bytecode");
        assert!(bytecode_program.ast.is_none());
        let bytecode = BytecodeVM::new(bytecode_program)
            .run_value()
            .expect("bytecode generic Option predicate execution");
        assert!(matches!(bytecode, Value::Int(41)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_option_predicate");
        generator
            .compile_mir_native(&program)
            .expect("native generic Option predicate must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic Option predicate module verifies");
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_option_unwrap_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { .. }
                )
            })
            .expect("generic Option projection instance");
        let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract } =
            &instance.contract
        else {
            unreachable!("filtered above");
        };
        assert_eq!(contract.variant_name, "Some");
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic Option unwrap execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_option_unwrap");
        generator
            .compile_mir_native(&program)
            .expect("native generic Option unwrap must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic Option unwrap module verifies");
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_option_unwrap_f64_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_f64.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership
                            == crate::core::mir::types::MirOwnership::Copy
                )
            })
            .expect("generic Option<f64> projection instance");
        assert_eq!(
            program
                .type_catalog()
                .get(&instance.arguments[0])
                .unwrap()
                .abi,
            crate::core::mir::types::MirAbiClass::Float { bits: 64 }
        );
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_option_unwrap_f64");
        generator
            .compile_mir_native(&program)
            .expect("native generic Option<f64> unwrap must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic Option<f64> unwrap module verifies");
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_generic_option_unwrap_none_keeps_the_receipt_trap() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_none.mimi"
        ));
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_option_unwrap_none");
        generator
            .compile_mir_native(&program)
            .expect("native generic Option unwrap None must consume MIR");
        generator
            .module
            .verify()
            .expect("native generic Option unwrap None module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("[E0800] canonical MIR direct variant projection"));
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_option_unwrap_owned_string() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_owned_string.mimi"
        ));
        let instance = program
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
        let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract } =
            &instance.contract
        else {
            unreachable!("filtered above");
        };
        assert_eq!(
            contract.projection.move_out_glue,
            crate::core::mir::types::MirGlueKind::OwnedString
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic owned Option unwrap execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("generic owned Option unwrap MIR bytecode"),
        )
        .run_value()
        .expect("generic owned Option unwrap bytecode execution");
        assert!(matches!(bytecode, Value::Int(41)));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_option_unwrap_owned_string");
        generator
            .compile_mir_native(&program)
            .expect("native generic owned Option unwrap must consume Move MIR");
        generator
            .module
            .verify()
            .expect("native generic owned Option unwrap module verifies");
        assert!(generator.module.get_function("main").is_some());
        assert!(generator.module.get_function("mimi_string_free").is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_option_unwrap_owned_list() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_owned_list.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership == crate::core::mir::types::MirOwnership::Move
                        && contract.projection.move_out_glue == crate::core::mir::types::MirGlueKind::List
                )
            })
            .expect("owned generic Option<List> projection instance");
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic owned Option<List> unwrap execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("generic owned Option<List> MIR bytecode"),
        )
        .run_value()
        .expect("generic owned Option<List> bytecode execution");
        assert!(matches!(bytecode, Value::Int(41)));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_option_unwrap_owned_list");
        generator
            .compile_mir_native(&program)
            .expect("native generic owned Option<List> must consume Move MIR");
        generator
            .module
            .verify()
            .expect("native generic owned Option<List> module verifies");
        assert!(generator.module.get_function("main").is_some());
        assert!(generator
            .module
            .get_function("mimi_mir_list_drop_scalar")
            .is_some());
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { .. }
        ));
    }

    #[test]
    fn native_emitter_consumes_generic_option_unwrap_owned_list_i64_and_bool() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_owned_list_scalars.mimi"
        ));
        let instances = program
            .instances()
            .values()
            .filter(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership == crate::core::mir::types::MirOwnership::Move
                        && contract.projection.move_out_glue == crate::core::mir::types::MirGlueKind::List
                )
            })
            .count();
        assert_eq!(instances, 2);
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic Option<List<i64|bool>> execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("generic Option<List<i64|bool>> MIR bytecode"),
        )
        .run_value()
        .expect("generic Option<List<i64|bool>> bytecode execution");
        assert!(matches!(bytecode, Value::Int(41)));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_option_unwrap_owned_list_scalars",
        );
        generator
            .compile_mir_native(&program)
            .expect("native generic Option<List<i64|bool>> must consume Move MIR");
        generator
            .module
            .verify()
            .expect("native generic Option<List<i64|bool>> module verifies");
        assert!(generator.module.get_function("main").is_some());
        assert!(generator
            .module
            .get_function("mimi_mir_list_drop_scalar")
            .is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_option_unwrap_or_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_or.mimi"
        ));
        let instance =
            program
                .instances()
                .values()
                .find(|instance| {
                    matches!(
                    instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                        ..
                    }
                )
                })
                .expect("generic Option fallback projection instance");
        let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
            contract,
        } = &instance.contract
        else {
            unreachable!("filtered above");
        };
        assert_eq!(contract.variant_name, "Some");
        assert_eq!(contract.fallback_variant_name, "None");
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic Option unwrap_or execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_option_unwrap_or");
        generator
            .compile_mir_native(&program)
            .expect("native generic Option unwrap_or must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic Option unwrap_or module verifies");
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_generic_option_unwrap_or_none_selects_fallback() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_or_none.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic Option unwrap_or None execution");
        assert_eq!(reference, MirRuntimeValue::Int(7));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_option_unwrap_or_none");
        generator
            .compile_mir_native(&program)
            .expect("native generic Option unwrap_or None must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic Option unwrap_or None module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_option_unwrap_or_f64_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_or_f64.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership == crate::core::mir::types::MirOwnership::Copy
                )
            })
            .expect("generic Option<f64> fallback projection instance");
        assert_eq!(
            program
                .type_catalog()
                .get(&instance.arguments[0])
                .expect("generic Option<f64> TypeDesc")
                .abi,
            crate::core::mir::types::MirAbiClass::Float { bits: 64 }
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic Option<f64> unwrap_or execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_option_unwrap_or_f64");
        generator
            .compile_mir_native(&program)
            .expect("native generic Option<f64> unwrap_or must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic Option<f64> unwrap_or module verifies");
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_generic_option_unwrap_or_f64_none_selects_fallback() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_or_f64_none.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic Option<f64> unwrap_or None execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_option_unwrap_or_f64_none");
        generator
            .compile_mir_native(&program)
            .expect("native generic Option<f64> unwrap_or None must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic Option<f64> unwrap_or None module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_option_unwrap_or_owned_string() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_string.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                        contract
                    } if contract.projection.ownership
                        == crate::core::mir::types::MirOwnership::Move
                )
            })
            .expect("managed generic Option fallback projection instance");
        let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
            contract,
        } = &instance.contract
        else {
            unreachable!("filtered above");
        };
        assert_eq!(
            contract.projection.move_out_glue,
            crate::core::mir::types::MirGlueKind::OwnedString
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference managed Option fallback execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let bytecode = BytecodeVM::new(
            crate::interp::bytecode::mir::compile_mir_program(&program)
                .expect("managed Option fallback MIR bytecode"),
        )
        .run_value()
        .expect("managed Option fallback bytecode execution");
        assert!(matches!(bytecode, Value::Int(41)));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_option_unwrap_or_owned_string");
        generator
            .compile_mir_native(&program)
            .expect("native managed Option fallback must consume Move MIR");
        generator
            .module
            .verify()
            .expect("native managed Option fallback module verifies");
        assert!(generator.module.get_function("main").is_some());
        assert!(generator.module.get_function("mimi_string_free").is_some());
    }

    #[test]
    fn native_generic_option_unwrap_or_owned_string_none_selects_fallback() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_string_none.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference managed Option fallback None execution");
        assert_eq!(reference, MirRuntimeValue::Int(7));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_option_unwrap_or_owned_string_none",
        );
        generator
            .compile_mir_native(&program)
            .expect("native managed Option fallback None must consume Move MIR");
        generator
            .module
            .verify()
            .expect("native managed Option fallback None module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_option_unwrap_or_owned_list() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_list.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                        contract
                    } if contract.projection.ownership
                        == crate::core::mir::types::MirOwnership::Move
                        && contract.projection.move_out_glue
                            == crate::core::mir::types::MirGlueKind::List
                )
            })
            .expect("managed generic Option<List> fallback projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback { .. }
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference managed Option<List> fallback execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let bytecode = BytecodeVM::new(
            crate::interp::bytecode::mir::compile_mir_program(&program)
                .expect("managed Option<List> fallback MIR bytecode"),
        )
        .run_value()
        .expect("managed Option<List> fallback bytecode execution");
        assert!(matches!(bytecode, Value::Int(41)));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_option_unwrap_or_owned_list");
        generator
            .compile_mir_native(&program)
            .expect("native managed Option<List> fallback must consume Move MIR");
        generator
            .module
            .verify()
            .expect("native managed Option<List> fallback module verifies");
        assert!(generator.module.get_function("main").is_some());
        assert!(generator
            .module
            .get_function("mimi_mir_list_drop_scalar")
            .is_some());
    }

    #[test]
    fn native_generic_option_unwrap_or_owned_list_none_selects_fallback() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_list_none.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference managed Option<List> fallback None execution");
        assert_eq!(reference, MirRuntimeValue::Int(7));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_option_unwrap_or_owned_list_none",
        );
        generator
            .compile_mir_native(&program)
            .expect("native managed Option<List> fallback None must consume Move MIR");
        generator
            .module
            .verify()
            .expect("native managed Option<List> fallback None module verifies");
    }

    #[test]
    fn native_generic_option_unwrap_or_bool_selects_fallback() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_or_bool.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic Option unwrap_or bool execution");
        assert_eq!(reference, MirRuntimeValue::Int(0));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_option_unwrap_or_bool");
        generator
            .compile_mir_native(&program)
            .expect("native generic Option unwrap_or bool must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic Option unwrap_or bool module verifies");
    }

    #[test]
    fn native_generic_option_unwrap_or_i64_selects_fallback() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_option_unwrap_or_i64.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic Option unwrap_or i64 execution");
        assert_eq!(reference, MirRuntimeValue::Int(7));
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_option_unwrap_or_i64");
        generator
            .compile_mir_native(&program)
            .expect("native generic Option unwrap_or i64 must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic Option unwrap_or i64 module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_result_unwrap_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_unwrap.mimi"
        ));
        let instance = program
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
            .expect("generic Result projection instance");
        let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract } =
            &instance.contract
        else {
            unreachable!("filtered above");
        };
        assert_eq!(contract.variant_name, "Ok");
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic Result unwrap execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let bytecode_program =
            compile_mir_program(&program).expect("generic Result unwrap MIR bytecode");
        assert!(bytecode_program.ast.is_none());
        let bytecode = BytecodeVM::new(bytecode_program)
            .run_value()
            .expect("bytecode generic Result unwrap execution");
        assert!(matches!(bytecode, Value::Int(41)));
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_result_unwrap");
        generator
            .compile_mir_native(&program)
            .expect("native generic Result unwrap must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic Result unwrap module verifies");
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_result_f64_unwrap_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_unwrap_f64.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Result"
                        && contract.projection.ownership
                            == crate::core::mir::types::MirOwnership::Copy
                )
            })
            .expect("generic Result f64 projection instance");
        let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract } =
            &instance.contract
        else {
            unreachable!("filtered above");
        };
        assert!(matches!(
            program
                .type_catalog()
                .get(&contract.result_ty)
                .map(|descriptor| descriptor.abi),
            Some(crate::core::mir::types::MirAbiClass::Float { bits: 64 })
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic Result f64 unwrap execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("generic Result f64 MIR bytecode"),
        )
        .run_value()
        .expect("bytecode generic Result f64 unwrap execution");
        assert!(matches!(bytecode, Value::Int(42)));
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_result_unwrap_f64");
        generator
            .compile_mir_native(&program)
            .expect("native generic Result f64 unwrap must consume MIR");
        generator
            .module
            .verify()
            .expect("native generic Result f64 unwrap module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_result_unwrap_owned_string() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_unwrap_owned_string.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Result"
                        && contract.projection.ownership
                            == crate::core::mir::types::MirOwnership::Move
                )
            })
            .expect("owned generic Result projection instance");
        let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract } =
            &instance.contract
        else {
            unreachable!("filtered above");
        };
        assert_eq!(
            contract.projection.move_out_glue,
            crate::core::mir::types::MirGlueKind::OwnedString
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference owned generic Result projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("generic owned Result projection bytecode"),
        )
        .run_value()
        .expect("bytecode owned generic Result projection execution");
        assert!(matches!(bytecode, Value::Int(41)));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_result_unwrap_owned_string");
        generator
            .compile_mir_native(&program)
            .expect("native generic owned Result projection must consume Move MIR");
        generator
            .module
            .verify()
            .expect("native generic owned Result projection module verifies");
        assert!(generator.module.get_function("main").is_some());
        assert!(generator.module.get_function("mimi_string_free").is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_result_bool_error_owned_string() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_bool_error_owned_string.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Result"
                        && contract.projection.ownership
                            == crate::core::mir::types::MirOwnership::Move
                        && contract.projection.move_out_glue
                            == crate::core::mir::types::MirGlueKind::OwnedString
                )
            })
            .expect("owned generic Result<T,bool> projection instance");
        let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract } =
            &instance.contract
        else {
            unreachable!("filtered above");
        };
        let crate::core::mir::types::MirLayout::Result { error, .. } = &program
            .type_catalog()
            .get(&contract.source_ty)
            .expect("specialized Result<T,bool> TypeDesc")
            .layout
        else {
            panic!("specialized source must retain a Result layout");
        };
        assert_eq!(
            program.type_catalog().get(error).map(|desc| &desc.kind),
            Some(&crate::core::mir::types::MirTypeKind::Primitive(
                crate::core::PrimitiveType::Bool
            ))
        );
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic Result<T,bool> owned String projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_result_bool_error_owned_string",
        );
        generator
            .compile_mir_native(&program)
            .expect("native generic Result<T,bool> owned String projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native generic Result<T,bool> owned String module verifies");
        assert!(generator.module.get_function("main").is_some());
        assert!(generator.module.get_function("mimi_string_free").is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_result_unwrap_owned_list() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_unwrap_owned_list.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection {
                        contract
                    } if contract.projection.nominal.as_str() == "builtin:type:Result"
                        && contract.projection.ownership
                            == crate::core::mir::types::MirOwnership::Move
                        && contract.projection.move_out_glue
                            == crate::core::mir::types::MirGlueKind::List
                )
            })
            .expect("owned generic Result<List> projection instance");
        let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract } =
            &instance.contract
        else {
            unreachable!("filtered above");
        };
        assert_eq!(contract.projection.field_index, 0);
        assert_eq!(contract.projection.arity, 1);
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference owned generic Result<List> projection execution");
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("generic owned Result<List> projection bytecode"),
        )
        .run_value()
        .expect("bytecode owned generic Result<List> projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        assert!(matches!(bytecode, Value::Int(41)));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_result_unwrap_owned_list");
        generator
            .compile_mir_native(&program)
            .expect("native generic owned Result<List> projection must consume Move MIR");
        generator
            .module
            .verify()
            .expect("native generic owned Result<List> projection module verifies");
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_result_distinct_unwrap_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_distinct_unwrap.mimi"
        ));
        let instance = program
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
            .expect("generic distinct Result projection instance");
        let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract } =
            &instance.contract
        else {
            unreachable!("filtered above");
        };
        let crate::core::mir::types::MirLayout::Result { ok, error, .. } = &program
            .type_catalog()
            .get(&contract.source_ty)
            .expect("specialized Result TypeDesc")
            .layout
        else {
            panic!("specialized source must retain a Result layout");
        };
        assert_ne!(ok, error);
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference distinct Result unwrap execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("generic distinct Result MIR bytecode"),
        )
        .run_value()
        .expect("bytecode distinct Result unwrap execution");
        assert!(matches!(bytecode, Value::Int(41)));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_result_distinct_unwrap");
        generator
            .compile_mir_native(&program)
            .expect("native generic distinct Result unwrap must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic distinct Result unwrap module verifies");
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_result_bool_error_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_bool_error.mimi"
        ));
        let instance = program
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
            .expect("generic Result<T,bool> projection instance");
        let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjection { contract } =
            &instance.contract
        else {
            unreachable!("filtered above");
        };
        let crate::core::mir::types::MirLayout::Result { ok, error, .. } = &program
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
            Some(&crate::core::mir::types::MirTypeKind::Primitive(
                crate::core::PrimitiveType::Bool
            ))
        );
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic Result<bool-error> execution");
        assert_eq!(reference, MirRuntimeValue::Int(81));
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_result_bool_error");
        generator
            .compile_mir_native(&program)
            .expect("native generic Result<T,bool> must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic Result<T,bool> module verifies");
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_generic_result_bool_error_unwrap_keeps_the_receipt_trap() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_bool_error_trap.mimi"
        ));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_result_bool_error_trap");
        generator
            .compile_mir_native(&program)
            .expect("native generic Result<T,bool> Err must consume MIR");
        generator
            .module
            .verify()
            .expect("native generic Result<T,bool> Err module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("[E0800] canonical MIR direct variant projection"));
    }

    #[test]
    fn native_generic_result_distinct_unwrap_err_keeps_the_receipt_trap() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_distinct_unwrap_err.mimi"
        ));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_result_distinct_unwrap_err");
        generator
            .compile_mir_native(&program)
            .expect("native generic distinct Result Err must consume MIR");
        generator
            .module
            .verify()
            .expect("native generic distinct Result Err module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("[E0800] canonical MIR direct variant projection"));
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_result_unwrap_or_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_unwrap_or.mimi"
        ));
        let instance = program
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
            .expect("generic Result fallback projection instance");
        let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
            contract,
        } = &instance.contract
        else {
            unreachable!("filtered above");
        };
        assert_eq!(contract.variant_name, "Ok");
        assert_eq!(contract.fallback_variant_name, "Err");
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic Result unwrap_or execution");
        assert_eq!(reference, MirRuntimeValue::Int(48));
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_result_unwrap_or");
        generator
            .compile_mir_native(&program)
            .expect("native generic Result unwrap_or must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic Result unwrap_or module verifies");
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_generic_result_unwrap_or_i64_and_bool_select_fallback() {
        for (source, expected, module_name) in [
            (
                include_str!(
                    "../../../tests/fixtures/mir_native_generic_result_unwrap_or_i64.mimi"
                ),
                7,
                "mir_native_generic_result_unwrap_or_i64",
            ),
            (
                include_str!(
                    "../../../tests/fixtures/mir_native_generic_result_unwrap_or_bool.mimi"
                ),
                0,
                "mir_native_generic_result_unwrap_or_bool",
            ),
        ] {
            let program = canonical_program(source);
            let owner = crate::core::NodeId("function:main".into());
            let reference = MirReferenceInterpreter::new(&program)
                .execute(&owner, &[])
                .expect("reference generic Result scalar unwrap_or execution");
            assert_eq!(reference, MirRuntimeValue::Int(expected));
            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, module_name);
            generator
                .compile_mir_native(&program)
                .expect("native generic Result scalar unwrap_or must consume MIR");
            generator
                .module
                .verify()
                .expect("native generic Result scalar unwrap_or module verifies");
        }
    }

    #[test]
    fn native_generic_result_f64_unwrap_or_selects_fallback() {
        for (source, module_name) in [
            (
                include_str!(
                    "../../../tests/fixtures/mir_native_generic_result_unwrap_or_f64.mimi"
                ),
                "mir_native_generic_result_unwrap_or_f64",
            ),
            (
                include_str!(
                    "../../../tests/fixtures/mir_native_generic_result_unwrap_or_f64_err.mimi"
                ),
                "mir_native_generic_result_unwrap_or_f64_err",
            ),
        ] {
            let program = canonical_program(source);
            let owner = crate::core::NodeId("function:main".into());
            let reference = MirReferenceInterpreter::new(&program)
                .execute(&owner, &[])
                .expect("reference generic Result f64 unwrap_or execution");
            assert_eq!(reference, MirRuntimeValue::Int(42));
            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, module_name);
            generator
                .compile_mir_native(&program)
                .expect("native generic Result f64 unwrap_or must consume MIR");
            generator
                .module
                .verify()
                .expect("native generic Result f64 unwrap_or module verifies");
            assert!(generator.module.get_function("main").is_some());
        }
    }

    #[test]
    fn native_generic_result_bool_f64_unwrap_or_selects_fallback() {
        for (source, module_name) in [
            (
                include_str!(
                    "../../../tests/fixtures/mir_native_generic_result_bool_unwrap_or_f64.mimi"
                ),
                "mir_native_generic_result_bool_unwrap_or_f64",
            ),
            (
                include_str!(
                    "../../../tests/fixtures/mir_native_generic_result_bool_unwrap_or_f64_err.mimi"
                ),
                "mir_native_generic_result_bool_unwrap_or_f64_err",
            ),
        ] {
            let program = canonical_program(source);
            let reference = MirReferenceInterpreter::new(&program)
                .execute(&crate::core::NodeId("function:main".into()), &[])
                .expect("reference generic Result bool/f64 unwrap_or execution");
            assert_eq!(reference, MirRuntimeValue::Int(42));
            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, module_name);
            generator
                .compile_mir_native(&program)
                .expect("native generic Result bool/f64 unwrap_or must consume MIR");
            generator
                .module
                .verify()
                .expect("native generic Result bool/f64 unwrap_or module verifies");
        }
    }

    #[test]
    fn native_generic_distinct_result_unwrap_or_consumes_two_slot_receipt() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_distinct_unwrap_or.mimi"
        ));
        let instance = program
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
            .expect("generic heterogeneous Result fallback projection instance");
        let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
            contract,
        } = &instance.contract
        else {
            unreachable!("filtered above");
        };
        let crate::core::mir::types::MirLayout::Result { ok, error, .. } = &program
            .type_catalog()
            .get(&contract.source_ty)
            .expect("specialized heterogeneous Result TypeDesc")
            .layout
        else {
            panic!("specialized source must retain a Result layout");
        };
        assert_ne!(ok, error);
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference heterogeneous Result unwrap_or execution");
        assert_eq!(reference, MirRuntimeValue::Int(50));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_result_distinct_unwrap_or");
        generator
            .compile_mir_native(&program)
            .expect("native heterogeneous Result unwrap_or must consume MIR");
        generator
            .module
            .verify()
            .expect("native heterogeneous Result unwrap_or module verifies");
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_managed_generic_result_unwrap_or_consumes_move_receipt() {
        for (source, expected, module_name) in [
            (
                include_str!(
                    "../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_string.mimi"
                ),
                41,
                "mir_native_generic_result_unwrap_or_owned_string",
            ),
            (
                include_str!(
                    "../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_string_err.mimi"
                ),
                7,
                "mir_native_generic_result_unwrap_or_owned_string_err",
            ),
        ] {
            let program = canonical_program(source);
            let instance = program
                .instances()
                .values()
                .find(|instance| {
                    matches!(
                        &instance.contract,
                        crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                            contract
                        } if contract.projection.nominal.as_str() == "builtin:type:Result"
                            && contract.projection.ownership
                                == crate::core::mir::types::MirOwnership::Move
                    )
                })
                .expect("managed Result fallback projection instance");
            let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                contract,
            } = &instance.contract
            else {
                unreachable!("filtered above");
            };
            assert_eq!(
                contract.projection.move_out_glue,
                crate::core::mir::types::MirGlueKind::OwnedString
            );
            let owner = crate::core::NodeId("function:main".into());
            let reference = MirReferenceInterpreter::new(&program)
                .execute(&owner, &[])
                .expect("reference managed Result unwrap_or execution");
            assert_eq!(reference, MirRuntimeValue::Int(expected));
            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, module_name);
            generator
                .compile_mir_native(&program)
                .expect("native managed Result unwrap_or must consume MIR");
            generator
                .module
                .verify()
                .expect("native managed Result unwrap_or module verifies");
            assert!(generator.module.get_function("main").is_some());
        }
    }

    #[test]
    fn native_managed_generic_bool_result_unwrap_or_consumes_move_receipt() {
        let source = include_str!(
            "../../../tests/fixtures/mir_native_generic_result_bool_unwrap_or_owned_string.mimi"
        );
        let program = canonical_program(source);
        let contracts = program
            .instances()
            .values()
            .filter_map(|instance| match &instance.contract {
                crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                    contract,
                } if contract.projection.nominal.as_str() == "builtin:type:Result" => {
                    Some(contract)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            contracts.len(),
            1,
            "one deduplicated String specialization serves Ok and Err"
        );
        assert!(contracts.iter().all(|contract| {
            contract.projection.ownership == crate::core::mir::types::MirOwnership::Move
                && contract.projection.move_out_glue
                    == crate::core::mir::types::MirGlueKind::OwnedString
        }));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference managed Result<bool> unwrap_or execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_bool_result_unwrap_or_owned_string",
        );
        generator
            .compile_mir_native(&program)
            .expect("native managed Result<bool> unwrap_or must consume MIR");
        generator
            .module
            .verify()
            .expect("native managed Result<bool> unwrap_or module verifies");
    }

    #[test]
    fn native_managed_generic_result_unwrap_or_owned_list_consumes_move_receipt() {
        for (source, expected, module_name) in [
            (
                include_str!(
                    "../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_list.mimi"
                ),
                41,
                "mir_native_generic_result_unwrap_or_owned_list",
            ),
            (
                include_str!(
                    "../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_list_err.mimi"
                ),
                7,
                "mir_native_generic_result_unwrap_or_owned_list_err",
            ),
        ] {
            let program = canonical_program(source);
            let instance = program
                .instances()
                .values()
                .find(|instance| {
                    matches!(
                        &instance.contract,
                        crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                            contract
                        } if contract.projection.nominal.as_str() == "builtin:type:Result"
                            && contract.projection.ownership
                                == crate::core::mir::types::MirOwnership::Move
                    )
                })
                .expect("Result<List> fallback projection instance");
            let crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                contract,
            } = &instance.contract
            else {
                unreachable!("filtered above");
            };
            assert_eq!(
                contract.projection.move_out_glue,
                crate::core::mir::types::MirGlueKind::List
            );
            let owner = crate::core::NodeId("function:main".into());
            let reference = MirReferenceInterpreter::new(&program)
                .execute(&owner, &[])
                .expect("reference Result<List> unwrap_or execution");
            assert_eq!(reference, MirRuntimeValue::Int(expected));
            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, module_name);
            generator
                .compile_mir_native(&program)
                .expect("native Result<List> unwrap_or must consume MIR");
            generator
                .module
                .verify()
                .expect("native Result<List> unwrap_or module verifies");
            assert!(generator.module.get_function("main").is_some());
        }
    }

    #[test]
    fn native_managed_generic_result_unwrap_or_owned_list_copy_scalar_family() {
        let source = include_str!(
            "../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_list_scalars.mimi"
        );
        let program = canonical_program(source);
        let contracts = program
            .instances()
            .values()
            .filter_map(|instance| match &instance.contract {
                crate::core::mir::MirGenericInstanceContract::ScalarVariantProjectionFallback {
                    contract,
                } if contract.projection.nominal.as_str() == "builtin:type:Result" => {
                    Some(contract)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(contracts.len(), 2, "i64 and bool specializations");
        assert!(contracts.iter().all(|contract| {
            contract.projection.ownership == crate::core::mir::types::MirOwnership::Move
                && contract.projection.move_out_glue == crate::core::mir::types::MirGlueKind::List
        }));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Result List scalar fallback execution");
        assert_eq!(reference, MirRuntimeValue::Int(49));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_result_unwrap_or_owned_list_scalars",
        );
        generator
            .compile_mir_native(&program)
            .expect("native Result List scalar fallback must consume MIR");
        generator
            .module
            .verify()
            .expect("native Result List scalar fallback module verifies");
    }

    #[test]
    fn native_generic_result_unwrap_err_keeps_the_receipt_trap() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_unwrap_none.mimi"
        ));
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_result_unwrap_none");
        generator
            .compile_mir_native(&program)
            .expect("native generic Result unwrap Err must consume MIR");
        generator
            .module
            .verify()
            .expect("native generic Result unwrap Err module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("[E0800] canonical MIR direct variant projection"));
    }

    #[test]
    fn native_generic_result_f64_unwrap_err_keeps_the_receipt_trap() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_unwrap_f64_err.mimi"
        ));
        let helper = crate::core::NodeId("function:unwrap_f64_err".into());
        assert!(
            program.functions().contains_key(&helper),
            "the f64 Result trap belongs to the exact helper owner"
        );
        let helper_symbol = super::native_symbol_for_owner(&helper)
            .expect("resolve the exact native MIR helper owner");
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_result_unwrap_f64_err");
        generator
            .compile_mir_native(&program)
            .expect("native generic Result f64 unwrap Err must consume MIR");
        generator
            .module
            .verify()
            .expect("native generic Result f64 unwrap Err module verifies");
        let helper_function = generator
            .module
            .get_function(&helper_symbol)
            .expect("native adapter emits the exact f64 helper owner");
        assert!(
            !helper_function.get_basic_blocks().is_empty(),
            "the f64 helper has an emitted native body"
        );
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("[E0800] canonical MIR direct variant projection"));
        assert!(ir.contains(&format!("define double @{helper_symbol}(")));
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_result_predicate() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_predicate.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarVariantPredicate {
                        contract: crate::core::mir::types::MirVariantPredicateContract {
                            predicate: crate::core::mir::MirVariantPredicate::IsOk,
                            ..
                        }
                    }
                )
            })
            .expect("generic Result predicate instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantPredicate { .. }
        ));
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference generic Result predicate execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let bytecode_program =
            compile_mir_program(&program).expect("generic Result predicate MIR bytecode");
        assert!(bytecode_program.ast.is_none());
        let bytecode = BytecodeVM::new(bytecode_program)
            .run_value()
            .expect("bytecode generic Result predicate execution");
        assert!(matches!(bytecode, Value::Int(41)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_result_predicate");
        generator
            .compile_mir_native(&program)
            .expect("native generic Result predicate must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic Result predicate module verifies");
        assert!(generator.module.get_function("main").is_some());
    }

    #[test]
    fn native_emitter_consumes_generic_result_error_slot_predicate() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_result_error_slot.mimi"
        ));
        assert!(program.instances().values().any(|instance| matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarVariantPredicate {
                contract: crate::core::mir::types::MirVariantPredicateContract {
                    predicate: crate::core::mir::MirVariantPredicate::IsErr,
                    ..
                }
            }
        )));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference Result error-slot predicate execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let bytecode_program =
            compile_mir_program(&program).expect("Result error-slot predicate MIR bytecode");
        assert!(bytecode_program.ast.is_none());
        let bytecode = BytecodeVM::new(bytecode_program)
            .run_value()
            .expect("bytecode Result error-slot predicate execution");
        assert!(matches!(bytecode, Value::Int(41)));
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_result_error_slot");
        generator
            .compile_mir_native(&program)
            .expect("native Result error-slot predicate must consume canonical MIR");
        generator
            .module
            .verify()
            .expect("native Result error-slot predicate module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_scalar_generic_list_reverse() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_list_reverse.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("generic List.reverse instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarListFacade {
                operation: crate::core::mir::MirListOperation::Reverse
            }
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic List.reverse execution");
        assert_eq!(reference, MirRuntimeValue::Int(3));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_list_reverse");
        generator
            .compile_mir_native(&program)
            .expect("native generic List.reverse must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic List.reverse module verifies");
        assert!(generator
            .module
            .get_function("mimi_mir_list_reverse_scalar")
            .is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_scalar_generic_list_concat() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_list_concat.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("generic List.concat instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarListFacade {
                operation: crate::core::mir::MirListOperation::Concat
            }
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic List.concat execution");
        assert_eq!(reference, MirRuntimeValue::Int(5));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_list_concat");
        generator
            .compile_mir_native(&program)
            .expect("native generic List.concat must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic List.concat module verifies");
        assert!(generator
            .module
            .get_function("mimi_mir_list_concat_scalar")
            .is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_scalar_generic_list_construct() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_list_construct.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("generic List construction instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarListConstruct { .. }
        ));
        let target = program
            .functions()
            .get(&instance.function)
            .expect("materialized List construction target");
        assert!(target.canonical_text().contains("list_construct_contract"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic List construction execution");
        assert_eq!(reference, MirRuntimeValue::Int(1));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_list_construct");
        generator
            .compile_mir_native(&program)
            .expect("native generic List construction must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic List construction module verifies");
        assert!(generator
            .module
            .get_function("mimi_mir_list_new_scalar")
            .is_some());
        assert!(generator
            .module
            .get_function("mimi_mir_list_push_scalar")
            .is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_nested_generic_list_construct() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_list_nested_owned.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    instance.contract,
                    crate::core::mir::MirGenericInstanceContract::ScalarListConstruct { .. }
                ) && instance.arguments.len() == 1
            })
            .expect("nested List construction instance");
        let argument_desc = program
            .type_catalog()
            .get(&instance.arguments[0])
            .expect("nested List argument TypeDesc");
        assert!(matches!(argument_desc.layout, MirLayout::List { .. }));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference nested List construction execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));

        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_list_nested_construct");
        generator
            .compile_mir_native(&program)
            .expect("native nested List construction must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native nested List construction module verifies");
        assert!(generator
            .module
            .get_function("mimi_mir_list_new_nested")
            .is_some());
        assert!(generator
            .module
            .get_function("mimi_mir_list_push_nested")
            .is_some());
        assert!(generator
            .module
            .get_function("mimi_mir_list_drop_nested")
            .is_some());
    }

    #[test]
    fn native_emitter_consumes_nested_list_len_with_borrow_only_abi() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_nested_list_len.mimi"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference nested List.len execution");
        assert_eq!(reference, MirRuntimeValue::Int(2));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_nested_list_len");
        generator
            .compile_mir_native(&program)
            .expect("native nested List.len must consume canonical MIR");
        generator
            .module
            .verify()
            .expect("native nested List.len module verifies");
        assert!(generator
            .module
            .get_function("mimi_mir_list_len_nested")
            .is_some());
        assert!(generator
            .module
            .get_function("mimi_mir_list_drop_nested")
            .is_some());
    }

    #[test]
    fn native_emitter_consumes_nested_list_index_with_clone_abi() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_nested_list_index.mimi"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference nested List index execution");
        assert_eq!(reference, MirRuntimeValue::Int(3));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_nested_list_index");
        generator
            .compile_mir_native(&program)
            .expect("native nested List index must consume canonical MIR");
        generator
            .module
            .verify()
            .expect("native nested List index module verifies");
        assert!(generator
            .module
            .get_function("mimi_mir_list_get_nested")
            .is_some());
        assert!(generator
            .module
            .get_function("mimi_mir_list_drop_nested")
            .is_some());
    }

    #[test]
    fn native_emitter_consumes_nested_list_reverse_with_recursive_clone_abi() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_nested_list_reverse.mimi"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference nested List.reverse execution");
        assert_eq!(reference, MirRuntimeValue::Int(3));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_nested_list_reverse");
        generator
            .compile_mir_native(&program)
            .expect("native nested List.reverse must consume canonical MIR");
        generator
            .module
            .verify()
            .expect("native nested List.reverse module verifies");
        assert!(generator
            .module
            .get_function("mimi_mir_list_reverse_nested")
            .is_some());
        assert!(generator
            .module
            .get_function("mimi_mir_list_drop_nested")
            .is_some());
    }

    #[test]
    fn native_emitter_consumes_nested_list_concat_with_child_move_abi() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_nested_list_concat.mimi"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference nested List.concat execution");
        assert_eq!(reference, MirRuntimeValue::Int(3));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_nested_list_concat");
        generator
            .compile_mir_native(&program)
            .expect("native nested List.concat must consume canonical MIR");
        generator
            .module
            .verify()
            .expect("native nested List.concat module verifies");
        assert!(generator
            .module
            .get_function("mimi_mir_list_concat_nested")
            .is_some());
        assert!(generator
            .module
            .get_function("mimi_mir_list_drop_nested")
            .is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_scalar_generic_list_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_list_projection.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("generic List projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarListProjection {
                index_value: 0,
                ..
            }
        ));
        let target = program
            .functions()
            .get(&instance.function)
            .expect("materialized List projection target");
        assert!(target.canonical_text().contains("list_index"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic List projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_list_projection");
        generator
            .compile_mir_native(&program)
            .expect("native generic List projection must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic List projection module verifies");
        assert!(generator
            .module
            .get_function("mimi_mir_list_get_scalar")
            .is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_scalar_generic_list_index_one_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_list_projection_index_one.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("generic List index-one projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarListProjection {
                index_value: 1,
                ..
            }
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic List index-one projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));

        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_list_projection_index_one");
        generator
            .compile_mir_native(&program)
            .expect("native generic List index-one projection must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic List index-one projection module verifies");
        assert!(generator
            .module
            .get_function("mimi_mir_list_get_scalar")
            .is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_scalar_generic_record_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_projection.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("generic record projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection { .. }
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic record projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_record_projection");
        generator
            .compile_mir_native(&program)
            .expect("native generic record projection must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic record projection module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_two_field_generic_record_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_projection_pair.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("two-field generic record projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.arity == 2 && contract.name == "left"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference two-field generic record projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_record_projection_pair");
        generator
            .compile_mir_native(&program)
            .expect("native two-field generic record projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native two-field generic record projection module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_three_field_generic_record_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_projection_three_field.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("three-field generic record projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.arity == 3 && contract.name == "value"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference three-field generic record projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_record_projection_three_field");
        generator
            .compile_mir_native(&program)
            .expect("native three-field generic record projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native three-field generic record projection module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_four_field_generic_record_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_projection_four_field.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("four-field generic record projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.arity == 4 && contract.name == "value"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference four-field generic record projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));

        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_record_projection_four_field");
        generator
            .compile_mir_native(&program)
            .expect("native four-field generic record projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native four-field generic record projection module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_five_field_generic_record_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_projection_five_field.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("five-field generic record projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.arity == 5 && contract.name == "value"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference five-field generic record projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));

        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_record_projection_five_field");
        generator
            .compile_mir_native(&program)
            .expect("native five-field generic record projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native five-field generic record projection module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_six_field_generic_record_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_projection_six_field.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("six-field generic record projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.arity == 6 && contract.name == "value"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference six-field generic record projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_record_projection_six_field");
        generator
            .compile_mir_native(&program)
            .expect("native six-field generic record projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native six-field generic record projection module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_seven_field_generic_record_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_projection_seven_field.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("seven-field generic record projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.arity == 7 && contract.name == "value"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference seven-field generic record projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_record_projection_seven_field");
        generator
            .compile_mir_native(&program)
            .expect("native seven-field generic record projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native seven-field generic record projection module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_seven_field_generic_record_tail_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_projection_seven_field_tail.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("seven-field tail projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.field_index == 6 && contract.arity == 7 && contract.name == "value"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference seven-field tail projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_record_projection_seven_field_tail",
        );
        generator
            .compile_mir_native(&program)
            .expect("native seven-field tail projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native seven-field tail projection module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_seven_field_repeated_generic_record_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_projection_seven_field_repeated_generic.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("repeated-generic projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.field_index == 0 && contract.arity == 7 && contract.name == "value"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference repeated-generic projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_record_projection_seven_field_repeated_generic",
        );
        generator
            .compile_mir_native(&program)
            .expect("native repeated-generic projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native repeated-generic projection module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_seven_field_repeated_generic_nonzero_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_projection_seven_field_repeated_generic_nonzero.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("repeated-generic nonzero projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.field_index == 2 && contract.arity == 7 && contract.name == "mirror"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference repeated-generic nonzero projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(99));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_record_projection_seven_field_repeated_generic_nonzero",
        );
        generator
            .compile_mir_native(&program)
            .expect("native repeated-generic nonzero projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native repeated-generic nonzero projection module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_record_update() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_update.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("generic record update instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarRecordUpdate {
                ref contract
            } if contract.arity == 2 && contract.fields[0].name == "tag"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic record update execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_record_update");
        generator
            .compile_mir_native(&program)
            .expect("native generic record update must consume MIR");
        generator
            .module
            .verify()
            .expect("native generic record update module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_two_override_generic_record_update() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_update_multi.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("two-override generic record update instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarRecordUpdate {
                ref contract
            } if contract.arity == 3 && contract.fields.len() == 2
                && contract.fields[0].name == "tag"
                && contract.fields[1].name == "enabled"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference two-override record update execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_record_update_multi");
        generator
            .compile_mir_native(&program)
            .expect("native two-override generic record update must consume MIR");
        generator
            .module
            .verify()
            .expect("native two-override generic record update module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_owned_generic_record_update() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_update_owned.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    instance.contract,
                    crate::core::mir::MirGenericInstanceContract::OwnedRecordUpdate { .. }
                )
            })
            .expect("owned generic record update instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::OwnedRecordUpdate { ref contract }
                if contract.arity == 2
                    && contract.updates.len() == 1
                    && contract.residual.len() == 1
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference owned generic record update execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_record_update_owned");
        generator
            .compile_mir_native(&program)
            .expect("native owned generic record update must consume MIR");
        generator
            .module
            .verify()
            .expect("native owned generic record update module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_owned_generic_record_update_with_copy_residual() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_update_owned_copy_residual.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    instance.contract,
                    crate::core::mir::MirGenericInstanceContract::OwnedRecordUpdate { .. }
                )
            })
            .expect("mixed owned generic record update instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::OwnedRecordUpdate { ref contract }
                if contract.residual.len() == 1
                    && contract.residual[0].glue
                        == crate::core::mir::types::MirGlueKind::Noop
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference mixed owned update execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_record_update_owned_copy_residual",
        );
        generator
            .compile_mir_native(&program)
            .expect("native mixed owned update must consume MIR");
        generator
            .module
            .verify()
            .expect("native mixed owned update module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_owned_generic_record_update_with_two_residuals() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_update_owned_multi_residual.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    instance.contract,
                    crate::core::mir::MirGenericInstanceContract::OwnedRecordUpdate { .. }
                )
            })
            .expect("multi-residual owned generic record update instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::OwnedRecordUpdate { ref contract }
                if contract.arity == 3
                    && contract.residual.len() == 2
                    && contract.residual[0].glue == crate::core::mir::types::MirGlueKind::Noop
                    && contract.residual[1].glue
                        == crate::core::mir::types::MirGlueKind::OwnedString
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference multi-residual owned update execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_record_update_owned_multi_residual",
        );
        generator
            .compile_mir_native(&program)
            .expect("native multi-residual owned update must consume MIR");
        generator
            .module
            .verify()
            .expect("native multi-residual owned update module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_owned_generic_record_update_with_three_residuals() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_update_owned_three_residual.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    instance.contract,
                    crate::core::mir::MirGenericInstanceContract::OwnedRecordUpdate { .. }
                )
            })
            .expect("three-residual owned generic record update instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::OwnedRecordUpdate { ref contract }
                if contract.arity == 4
                    && contract.residual.len() == 3
                    && contract.residual[0].glue
                        == crate::core::mir::types::MirGlueKind::Noop
                    && contract.residual[1].glue
                        == crate::core::mir::types::MirGlueKind::OwnedString
                    && contract.residual[2].glue
                        == crate::core::mir::types::MirGlueKind::OwnedString
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference three-residual owned update execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_record_update_owned_three_residual",
        );
        generator
            .compile_mir_native(&program)
            .expect("native three-residual owned update must consume MIR");
        generator
            .module
            .verify()
            .expect("native three-residual owned update module verifies");
    }

    #[test]
    fn native_emitter_consumes_three_field_generic_record_tail_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_projection_three_field_tail.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("three-field tail projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.field_index == 2 && contract.arity == 3 && contract.name == "value"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference non-zero-index generic record projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_record_projection_three_field_tail",
        );
        generator
            .compile_mir_native(&program)
            .expect("native three-field tail projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native three-field tail projection module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_mixed_generic_record_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_projection_mixed.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("mixed generic record projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection {
                ref contract
            } if contract.arity == 2 && contract.name == "value"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference mixed generic record projection execution");
        assert_eq!(reference, MirRuntimeValue::Bool(true));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_record_projection_mixed");
        generator
            .compile_mir_native(&program)
            .expect("native mixed generic record projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native mixed generic record projection module verifies");
    }

    #[test]
    fn native_emitter_consumes_scalar_generic_record_projection_rvalue() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_projection_rvalue.mimi"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic record Copy rvalue execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_record_projection_rvalue");
        generator
            .compile_mir_native(&program)
            .expect("native generic record Copy rvalue must consume MIR");
        generator
            .module
            .verify()
            .expect("native generic record Copy rvalue module verifies");
    }

    #[test]
    fn native_emitter_consumes_scalar_generic_record_projection_i64_and_bool() {
        for (module_name, source, expected) in [
            (
                "mir_native_generic_record_projection_i64",
                include_str!(
                    "../../../tests/fixtures/mir_native_generic_record_projection_i64.mimi"
                ),
                MirRuntimeValue::Int(41),
            ),
            (
                "mir_native_generic_record_projection_bool",
                include_str!(
                    "../../../tests/fixtures/mir_native_generic_record_projection_bool.mimi"
                ),
                MirRuntimeValue::Bool(true),
            ),
        ] {
            let program = canonical_program(source);
            let instance = program
                .instances()
                .values()
                .next()
                .expect("generic record projection instance");
            assert!(matches!(
                instance.contract,
                crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection { .. }
            ));
            let reference = MirReferenceInterpreter::new(&program)
                .execute(&crate::core::NodeId("function:main".into()), &[])
                .expect("reference scalar generic record projection execution");
            assert_eq!(reference, expected);

            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, module_name);
            generator
                .compile_mir_native(&program)
                .expect("native scalar generic record projection must consume MIR");
            generator
                .module
                .verify()
                .expect("native scalar generic record projection module verifies");
        }
    }

    #[test]
    fn native_emitter_consumes_owned_generic_record_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_string_projection.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("owned generic record projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::OwnedRecordProjection { .. }
        ));
        let target = program
            .functions()
            .get(&instance.function)
            .expect("owned generic record projection target");
        assert!(target.canonical_text().contains("move_project"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference owned generic record projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_record_owned_string");
        generator
            .compile_mir_native(&program)
            .expect("native owned generic record projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native owned generic record projection module verifies");
    }

    #[test]
    fn native_emitter_consumes_owned_generic_record_list_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_list_projection.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    instance.contract,
                    crate::core::mir::MirGenericInstanceContract::OwnedRecordProjection { .. }
                )
            })
            .expect("owned generic Record<List<i32>> projection instance");
        let target = program
            .functions()
            .get(&instance.function)
            .expect("owned generic Record<List<i32>> projection target");
        assert!(target.canonical_text().contains("move_project"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference owned generic Record<List<i32>> projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));

        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_record_owned_list_projection");
        generator
            .compile_mir_native(&program)
            .expect("native owned generic Record<List<i32>> projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native owned generic Record<List<i32>> projection module verifies");
    }

    #[test]
    fn native_emitter_consumes_owned_generic_record_list_projection_with_string_residual() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_list_projection_residual.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                        contract
                    } if contract.projection.arity == 2
                        && contract.projection.name == "value"
                        && contract.residual.len() == 1
                        && contract.residual[0].name == "note"
                )
            })
            .expect("owned generic Record<List<i32>> residual instance");
        let target = program
            .functions()
            .get(&instance.function)
            .expect("owned generic Record<List<i32>> residual target");
        assert!(target.canonical_text().contains("move_project_drop"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference owned generic residual execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_record_owned_list_projection_residual",
        );
        generator
            .compile_mir_native(&program)
            .expect("native owned generic List residual projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native owned generic List residual module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("mir_record_move_drop_project"));
        assert!(ir.contains("mir_record_move_drop_residual"));
    }

    #[test]
    fn native_emitter_consumes_owned_generic_record_list_projection_with_list_residual() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_list_projection_list_residual.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                        contract
                    } if contract.projection.arity == 2
                        && contract.projection.name == "value"
                        && contract.residual.len() == 1
                        && contract.residual[0].name == "tail"
                        && contract.residual[0].glue
                            == crate::core::mir::types::MirGlueKind::List
                )
            })
            .expect("owned generic Record<List<i32>> List residual instance");
        let target = program
            .functions()
            .get(&instance.function)
            .expect("owned generic Record<List<i32>> List residual target");
        assert!(target.canonical_text().contains("move_project_drop"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference owned generic List residual execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_record_owned_list_projection_list_residual",
        );
        generator
            .compile_mir_native(&program)
            .expect("native owned generic List/List residual projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native owned generic List/List residual module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("mir_record_move_drop_project"));
        assert!(ir.contains("mir_record_move_drop_residual"));
    }

    #[test]
    fn native_emitter_consumes_owned_generic_record_list_projection_with_two_list_residual() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_list_projection_two_list_residual.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                        contract
                    } if contract.projection.arity == 3
                        && contract.projection.name == "value"
                        && contract.residual.len() == 2
                        && contract.residual[0].name == "spare"
                        && contract.residual[1].name == "tail"
                        && contract.residual.iter().all(|residual| {
                            residual.glue == crate::core::mir::types::MirGlueKind::List
                        })
                )
            })
            .expect("owned generic two-List residual instance");
        let target = program
            .functions()
            .get(&instance.function)
            .expect("owned generic two-List residual target");
        assert!(target.canonical_text().contains("move_project_drop"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference owned generic two-List residual execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_record_owned_list_projection_two_list_residual",
        );
        generator
            .compile_mir_native(&program)
            .expect("native owned generic two-List residual projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native owned generic two-List residual module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("mir_record_move_drop_project"));
        assert!(ir.contains("mir_record_move_drop_residual"));
    }

    #[test]
    fn native_emitter_consumes_owned_generic_record_list_string_residual() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_list_string_residual.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                        contract
                    } if contract.projection.arity == 3
                        && contract.projection.name == "value"
                        && contract.residual.len() == 2
                        && contract.residual[0].name == "note"
                        && contract.residual[0].glue
                            == crate::core::mir::types::MirGlueKind::OwnedString
                        && contract.residual[1].name == "tail"
                        && contract.residual[1].glue
                            == crate::core::mir::types::MirGlueKind::List
                )
            })
            .expect("owned generic List/String residual instance");
        let target = program
            .functions()
            .get(&instance.function)
            .expect("owned generic List/String residual target");
        assert!(target.canonical_text().contains("move_project_drop"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference owned generic List/String residual execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_record_owned_list_string_residual",
        );
        generator
            .compile_mir_native(&program)
            .expect("native owned generic List/String residual must consume MIR");
        generator
            .module
            .verify()
            .expect("native owned generic List/String residual module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("mir_record_move_drop_project"));
        assert!(ir.contains("mir_record_move_drop_residual"));
    }

    #[test]
    fn native_emitter_consumes_owned_generic_record_list_two_string_residual() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_list_two_string_residual.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                        contract
                    } if contract.projection.arity == 4
                        && contract.projection.name == "value"
                        && contract.residual.len() == 3
                        && contract.residual[0].name == "spare"
                        && contract.residual[0].glue
                            == crate::core::mir::types::MirGlueKind::OwnedString
                        && contract.residual[1].name == "note"
                        && contract.residual[1].glue
                            == crate::core::mir::types::MirGlueKind::OwnedString
                        && contract.residual[2].name == "tail"
                        && contract.residual[2].glue == crate::core::mir::types::MirGlueKind::List
                )
            })
            .expect("owned generic List/two-String residual instance");
        let target = program
            .functions()
            .get(&instance.function)
            .expect("owned generic List/two-String residual target");
        assert!(target.canonical_text().contains("move_project_drop"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference owned generic List/two-String residual execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_record_owned_list_two_string_residual",
        );
        generator
            .compile_mir_native(&program)
            .expect("native owned generic List/two-String residual must consume MIR");
        generator
            .module
            .verify()
            .expect("native owned generic List/two-String residual module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("mir_record_move_drop_project"));
        assert!(ir.contains("mir_record_move_drop_residual"));
    }

    #[test]
    fn native_emitter_consumes_owned_generic_record_bool_list_two_string_residual() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_bool_list_two_string_residual.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                        contract
                    } if contract.projection.arity == 4
                        && contract.projection.name == "value"
                        && contract.residual.len() == 3
                        && contract.residual[0].name == "spare"
                        && contract.residual[0].glue
                            == crate::core::mir::types::MirGlueKind::OwnedString
                        && contract.residual[1].name == "note"
                        && contract.residual[1].glue
                            == crate::core::mir::types::MirGlueKind::OwnedString
                        && contract.residual[2].name == "tail"
                        && contract.residual[2].glue == crate::core::mir::types::MirGlueKind::List
                )
            })
            .expect("owned generic Bool List/two-String residual instance");
        let target = program
            .functions()
            .get(&instance.function)
            .expect("owned generic Bool List/two-String residual target");
        assert!(target.canonical_text().contains("move_project_drop"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference owned generic Bool List/two-String residual execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_record_owned_bool_list_two_string_residual",
        );
        generator
            .compile_mir_native(&program)
            .expect("native owned generic Bool List/two-String residual must consume MIR");
        generator
            .module
            .verify()
            .expect("native owned generic Bool List/two-String residual module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("mir_record_move_drop_project"));
        assert!(ir.contains("mir_record_move_drop_residual"));
    }

    #[test]
    fn native_emitter_consumes_owned_generic_record_set_residual() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_set_residual.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .find(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                        contract
                    } if contract.projection.arity == 2
                        && contract.projection.name == "value"
                        && contract.residual.len() == 1
                        && contract.residual[0].name == "tail"
                        && contract.residual[0].glue == crate::core::mir::types::MirGlueKind::Set
                )
            })
            .expect("owned generic Set residual instance");
        let target = program
            .functions()
            .get(&instance.function)
            .expect("owned generic Set residual target");
        assert!(target.canonical_text().contains("move_project_drop"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference owned generic Set residual execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_record_owned_set_residual");
        generator
            .compile_mir_native(&program)
            .expect("native owned generic Set residual must consume MIR");
        generator
            .module
            .verify()
            .expect("native owned generic Set residual module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("mir_record_move_drop_project"));
        assert!(ir.contains("mir_record_move_drop_residual"));
    }

    #[test]
    fn native_emitter_consumes_owned_generic_record_set_scalar_family() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_set_scalar_family.mimi"
        ));
        let instances = program
            .instances()
            .values()
            .filter(|instance| {
                matches!(
                    &instance.contract,
                    crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                        contract
                    } if contract.projection.arity == 2
                        && contract.projection.name == "value"
                        && contract.residual.len() == 1
                        && contract.residual[0].name == "tail"
                        && contract.residual[0].glue == crate::core::mir::types::MirGlueKind::Set
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            instances.len(),
            2,
            "both Set element instances must materialize"
        );
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference owned generic Set scalar-family execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_record_owned_set_scalar_family",
        );
        generator
            .compile_mir_native(&program)
            .expect("native owned generic Set scalar-family must consume MIR");
        generator
            .module
            .verify()
            .expect("native owned generic Set scalar-family module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("mir_record_move_drop_project"));
        assert!(ir.contains("mir_record_move_drop_residual"));
    }

    #[test]
    fn native_emitter_consumes_owned_mixed_generic_record_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_string_mixed.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("mixed owned generic record projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::OwnedRecordProjection { ref contract }
                if contract.arity == 2 && contract.name == "value"
        ));
        let target = program
            .functions()
            .get(&instance.function)
            .expect("mixed owned generic record projection target");
        assert!(target.canonical_text().contains("move_project"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference mixed owned generic projection execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));

        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_record_owned_string_mixed");
        generator
            .compile_mir_native(&program)
            .expect("native mixed owned generic record projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native mixed owned generic record projection module verifies");
    }

    #[test]
    fn native_emitter_consumes_owned_generic_record_projection_with_residual_drop() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_string_residual.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("owned generic record residual projection instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                ref contract
            } if contract.projection.arity == 2
                && contract.projection.name == "value"
                && contract.residual.len() == 1
                && contract.residual[0].name == "note"
        ));
        let target = program
            .functions()
            .get(&instance.function)
            .expect("owned generic record residual projection target");
        assert!(target.canonical_text().contains("move_project_drop"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference owned generic record residual execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_record_owned_string_residual");
        generator
            .compile_mir_native(&program)
            .expect("native owned generic record residual projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native owned generic record residual projection module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("mir_record_move_drop_project"));
        assert!(ir.contains("mir_record_move_drop_residual"));
    }

    #[test]
    fn native_emitter_consumes_three_field_owned_generic_record_projection_with_two_residuals() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_string_three_field_residual.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("three-field owned generic record residual instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                ref contract
            } if contract.projection.arity == 3
                && contract.residual.len() == 2
                && contract.residual[0].name == "tail"
                && contract.residual[1].name == "note"
        ));
        let target = program
            .functions()
            .get(&instance.function)
            .expect("three-field owned generic record target");
        assert!(target.canonical_text().contains("move_project_drop"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference three-field owned generic residual execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_record_owned_string_three_field_residual",
        );
        generator
            .compile_mir_native(&program)
            .expect("native three-field owned generic residual projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native three-field owned generic residual module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("mir_record_move_drop_project"));
        assert!(ir.contains("mir_record_move_drop_residual"));
    }

    #[test]
    fn native_emitter_consumes_heterogeneous_owned_generic_record_projection_with_residual_drop() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_string_heterogeneous_residual.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("heterogeneous owned generic residual instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                ref contract
            } if contract.projection.arity == 2
                && contract.projection.name == "value"
                && contract.residual.len() == 1
                && contract.residual[0].name == "note"
                && contract.residual[0].glue == crate::core::mir::types::MirGlueKind::OwnedString
        ));
        let target = program
            .functions()
            .get(&instance.function)
            .expect("heterogeneous owned generic residual target");
        assert!(target.canonical_text().contains("move_project_drop"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference heterogeneous owned generic residual execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_record_owned_string_heterogeneous_residual",
        );
        generator
            .compile_mir_native(&program)
            .expect("native heterogeneous owned generic residual projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native heterogeneous owned generic residual module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("mir_record_move_drop_project"));
        assert!(ir.contains("mir_record_move_drop_residual"));
    }

    #[test]
    fn native_emitter_consumes_three_field_heterogeneous_owned_generic_record_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_string_three_field_heterogeneous_residual.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("three-field heterogeneous owned residual instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                ref contract
            } if contract.projection.arity == 3
                && contract.projection.name == "value"
                && contract.residual.len() == 2
                && contract.residual[0].name == "tail"
                && contract.residual[1].name == "note"
                && contract.residual.iter().all(|residual| {
                    residual.glue == crate::core::mir::types::MirGlueKind::OwnedString
                })
        ));
        let target = program
            .functions()
            .get(&instance.function)
            .expect("three-field heterogeneous owned residual target");
        assert!(target.canonical_text().contains("move_project_drop"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference three-field heterogeneous owned residual execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_record_owned_string_three_field_heterogeneous_residual",
        );
        generator
            .compile_mir_native(&program)
            .expect("native three-field heterogeneous owned projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native three-field heterogeneous owned residual module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("mir_record_move_drop_project"));
        assert!(ir.contains("mir_record_move_drop_residual"));
    }

    #[test]
    fn native_emitter_consumes_four_field_heterogeneous_owned_generic_record_projection() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_string_four_field_heterogeneous_residual.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("four-field heterogeneous owned residual instance");
        assert!(matches!(
            instance.contract,
            crate::core::mir::MirGenericInstanceContract::OwnedRecordProjectionDrop {
                ref contract
            } if contract.projection.arity == 4
                && contract.projection.name == "value"
                && contract.residual.len() == 3
                && contract.residual[0].name == "spare"
                && contract.residual[1].name == "tail"
                && contract.residual[2].name == "note"
                && contract.residual.iter().all(|residual| {
                    residual.glue == crate::core::mir::types::MirGlueKind::OwnedString
                })
        ));
        let target = program
            .functions()
            .get(&instance.function)
            .expect("four-field heterogeneous owned residual target");
        assert!(target.canonical_text().contains("move_project_drop"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference four-field heterogeneous owned residual execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator = CodeGenerator::new(
            &context,
            "mir_native_generic_record_owned_string_four_field_heterogeneous_residual",
        );
        generator
            .compile_mir_native(&program)
            .expect("native four-field heterogeneous owned projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native four-field heterogeneous owned residual module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("mir_record_move_drop_project"));
        assert!(ir.contains("mir_record_move_drop_residual"));
    }

    #[test]
    fn native_emitter_consumes_owned_generic_record_projection_rvalue() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_string_rvalue_call.mimi"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference owned generic record rvalue execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_record_owned_string_rvalue");
        generator
            .compile_mir_native(&program)
            .expect("native owned generic record rvalue must consume MIR");
        generator
            .module
            .verify()
            .expect("native owned generic record rvalue module verifies");
    }

    #[test]
    fn native_emitter_consumes_owned_string_generic_identity_with_explicit_drop() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_owned_string_identity.mimi"
        ));
        let instance = program
            .instances()
            .values()
            .next()
            .expect("owned String identity instance");
        let target = program
            .functions()
            .get(&instance.function)
            .expect("owned String identity target");
        assert!(target.canonical_text().contains("clone"));
        assert!(target.canonical_text().contains("drop"));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference owned String generic identity execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));

        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_owned_string_identity");
        generator
            .compile_mir_native(&program)
            .expect("native owned String generic identity must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native owned String generic identity module verifies");
        assert!(generator.module.get_function("mimi_str_clone").is_some());
        assert!(generator.module.get_function("mimi_string_free").is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_variant_identity() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_variant_identity.mimi"
        ));
        assert_eq!(program.instances().len(), 2);
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic variant identity execution");
        assert_eq!(reference, MirRuntimeValue::Int(18));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_variant_identity");
        generator
            .compile_mir_native(&program)
            .expect("native generic variant identity must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic variant identity module verifies");
    }

    #[test]
    fn native_emitter_consumes_materialized_generic_variant_identity_branch_paths() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_variant_identity_multipath.mimi"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic branch identity execution");
        assert_eq!(reference, MirRuntimeValue::Int(7));

        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_variant_identity_multipath");
        generator
            .compile_mir_native(&program)
            .expect("native generic branch identity must consume specialized MIR");
        generator
            .module
            .verify()
            .expect("native generic branch identity module verifies");
    }

    #[test]
    fn native_emitter_consumes_total_direct_variant_call_paths() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_variant_call_multipath.mimi"
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference multipath direct variant execution");
        assert_eq!(reference, MirRuntimeValue::Int(4));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_variant_call_multipath");
        generator
            .compile_mir_native(&program)
            .expect("native multipath direct variant call must consume MIR");
        generator
            .module
            .verify()
            .expect("native multipath direct variant call module verifies");
    }

    #[test]
    fn native_emitter_consumes_tuple_projection_receipt() {
        let program = canonical_program("func main() -> i32 { let pair = (40, 2); pair.0 }");
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_tuple_projection_test");
        generator
            .compile_mir_native(&program)
            .expect("native tuple projection should use the canonical receipt");
        generator
            .module
            .verify()
            .expect("native tuple projection module verifies");
    }

    #[test]
    fn native_emitter_materializes_direct_variant_project_active_tag_guard() {
        let fixture = crate::core::mir::test_support::direct_variant_projection_fixture();
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_variant_project_test");
        generator
            .compile_mir_native(&fixture.program)
            .expect("native direct variant projection lowering");
        generator
            .module
            .verify()
            .expect("native direct variant projection module verifies");

        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("mir_variant_project_active"));
        assert!(ir.contains("[E0800] canonical MIR direct variant projection"));
        assert!(generator.module.get_function("project").is_some());
    }

    #[test]
    fn native_emitter_materializes_consuming_variant_project_active_tag_guard() {
        let fixture = crate::core::mir::test_support::direct_variant_move_projection_fixture();
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_variant_move_project_test");
        generator
            .compile_mir_native(&fixture.program)
            .expect("native consuming variant projection lowering");
        generator
            .module
            .verify()
            .expect("native consuming variant projection module verifies");

        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("mir_variant_move_project_active"));
        assert!(ir.contains("[E0800] canonical MIR consuming direct variant projection"));
        assert!(generator.module.get_function("project").is_some());
    }

    #[test]
    fn native_emitter_materializes_record_move_drop_residual_glue() {
        let fixture = crate::core::mir::test_support::direct_record_move_drop_fixture();
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_record_move_drop_test");
        generator
            .compile_mir_native(&fixture.program)
            .expect("native record move/drop projection lowering");
        generator
            .module
            .verify()
            .expect("native record move/drop projection module verifies");

        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("mir_record_move_drop_residual"));
        assert!(ir.contains("mir_record_move_drop_project"));
        assert!(generator.module.get_function("project").is_some());
    }

    #[test]
    fn ordinary_record_field_move_drop_shares_one_mir_across_consumers() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_record_move_drop_field.mimi"
        ));
        let main = program
            .functions()
            .get(&crate::core::NodeId("function:main".into()))
            .expect("main MIR");
        let move_drop = main
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::MoveProjectDrop {
                    contract: Some(contract),
                    ..
                } => Some(contract),
                _ => None,
            })
            .expect("ordinary record field must use MoveProjectDrop");
        assert_eq!(move_drop.projection.name, "title");
        assert_eq!(move_drop.residual.len(), 1);
        assert_eq!(move_drop.residual[0].name, "note");
        assert_eq!(move_drop.residual[0].glue, MirGlueKind::OwnedString);

        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&owner, &[])
            .expect("reference ordinary MoveProjectDrop execution");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(reference.output, "");

        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("ordinary MoveProjectDrop MIR bytecode"),
        )
        .run_value()
        .expect("bytecode ordinary MoveProjectDrop execution");
        assert!(matches!(bytecode, Value::Int(0)));

        crate::verifier::validate_mir_capabilities(&program)
            .expect("verifier capability for ordinary MoveProjectDrop");

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_ordinary_record_move_drop_test");
        generator
            .compile_mir_native(&program)
            .expect("native ordinary MoveProjectDrop lowering");
        generator
            .module
            .verify()
            .expect("native ordinary MoveProjectDrop module verifies");
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("native ordinary MoveProjectDrop execution");
        assert_eq!(native.stdout, "");
        assert_eq!(native.stderr, "");
        assert_eq!(native.exit_code, Some(0));
    }

    #[test]
    fn multifield_flow_source_receipt_shares_one_mir_across_consumers() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_m3_flow_multifield_string_source_receipt.mimi"
        ));
        let owner = crate::core::NodeId("function:main".into());
        let main = program.functions().get(&owner).expect("main MIR");
        assert!(main.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::BuiltinCall {
                        kind: MirBuiltinKind::PrintlnString,
                        string_field_contract: Some(_),
                        ..
                    }
                )
            })
        }));
        let transition = program
            .functions()
            .get(&crate::core::NodeId(
                "transition:Account::close::Active".into(),
            ))
            .expect("close transition MIR");
        assert!(transition.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(instruction.kind, MirInstructionKind::MoveProjectDrop { .. })
            })
        }));

        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&owner, &[])
            .expect("reference multifield Flow source receipt execution");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(reference.output, "source\n");

        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("multifield Flow source receipt MIR bytecode"),
        )
        .run_value()
        .expect("bytecode multifield Flow source receipt execution");
        assert!(matches!(bytecode, Value::Int(0)));

        crate::verifier::validate_mir_capabilities(&program)
            .expect("verifier capability for multifield Flow source receipt");

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_multifield_flow_source_receipt");
        generator
            .compile_mir_native(&program)
            .expect("native multifield Flow source receipt lowering");
        generator
            .module
            .verify()
            .expect("native multifield Flow source receipt module verifies");
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("native multifield Flow source receipt execution");
        assert_eq!(native.stdout, "source\n");
        assert_eq!(native.stderr, "");
        assert_eq!(native.exit_code, Some(0));
    }

    #[test]
    fn f64_cross_state_flow_receipt_shares_one_mir_and_proves_through_the_verifier() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_r6_flow_f64_cross_state_receipt.mimi"
        ));
        let digest = program.canonical_digest();
        let transition = program
            .transitions()
            .get(&crate::core::NodeId(
                "transition:Account::settle::Active".into(),
            ))
            .expect("f64 recoverable transition MIR");
        assert_eq!(
            transition.effect,
            crate::core::mir::MirTransitionEffect::RecoverableBoundary
        );
        assert!(program.type_catalog().iter().any(|(_, descriptor)| {
            descriptor.abi == crate::core::mir::types::MirAbiClass::Float { bits: 64 }
        }));

        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute_with_output(&owner, &[])
            .expect("reference f64 recoverable Flow execution");
        assert_eq!(reference.value, MirRuntimeValue::Int(0));
        assert_eq!(reference.output, "");

        let bytecode = BytecodeVM::new(
            compile_mir_program(&program).expect("f64 recoverable Flow MIR bytecode"),
        )
        .run_value()
        .expect("bytecode f64 recoverable Flow execution");
        assert!(matches!(bytecode, Value::Int(0)));

        crate::verifier::validate_mir_capabilities(&program)
            .expect("f64 recoverable Flow canonical capability gate");
        let verifier = crate::verifier::verify_mir(&program, "f64-flow-test".into())
            .expect("f64 recoverable Flow verifier result");
        let main_result = verifier
            .iter()
            .find(|result| result.func_name == owner.0)
            .expect("f64 recoverable Flow verifier result for main");
        // R6-1061 restatement: the transition's `self.balance + 1.0` now
        // carries the Z3 IEEE Float symbolic domain, so the receipt proves
        // on the canonical engine instead of reporting the float boundary.
        assert_eq!(main_result.status, crate::verifier::VerifStatus::Proven);
        assert!(main_result.constraint_count > 0);
        let artifact = main_result
            .artifact
            .as_ref()
            .expect("f64 recoverable Flow proof artifact");
        assert_eq!(artifact.engine, crate::verifier::ProofArtifact::ENGINE_MIR);

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_f64_cross_state_flow_receipt");
        generator
            .compile_mir_native(&program)
            .expect("native f64 recoverable Flow lowering");
        generator
            .module
            .verify()
            .expect("native f64 recoverable Flow module verifies");
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("native f64 recoverable Flow execution");
        assert_eq!(native.stdout, "");
        assert_eq!(native.stderr, "");
        assert_eq!(native.exit_code, Some(0));
        assert_eq!(program.canonical_digest(), digest);
    }

    #[test]
    fn native_validator_rejects_checker_materialized_enum_before_llvm() {
        let fixture = crate::core::mir::test_support::direct_enum_switch_move_fixture();
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_user_enum_validator_test");
        let diagnostics = generator
            .compile_mir_native(&fixture.program)
            .expect_err("user enum must remain outside the native union ABI slice");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("checker-materialized user enum has no native tagged-union ABI contract")
        }));
        assert!(
            generator.module.get_function("take").is_none(),
            "L2 requires an unsupported user enum to be rejected before LLVM declarations"
        );
    }

    #[test]
    fn native_emitter_consumes_flat_copy_user_enum_abi() {
        let fixture = crate::core::mir::test_support::direct_flat_copy_enum_switch_fixture();
        let descriptor = fixture
            .program
            .type_catalog()
            .get(&fixture.source_ty)
            .expect("Signal TypeDesc");
        assert_eq!(
            descriptor.abi,
            crate::core::mir::types::MirAbiClass::Aggregate
        );
        assert_eq!(
            descriptor.ownership,
            crate::core::mir::types::MirOwnership::Copy
        );
        let (variant_abi, payload_ty) =
            super::native_variant_abi(fixture.program.type_catalog(), &fixture.source_ty, false)
                .expect("flat Copy user-enum ABI");
        assert_eq!(variant_abi.tag_field, 0);
        assert_eq!(variant_abi.payload_field, 1);
        assert_eq!(variant_abi.payload_types, vec![payload_ty.clone()]);
        let payload_slot = variant_abi
            .payload_slot(&fixture.number)
            .expect("Number payload ABI slot");
        assert_eq!(payload_slot.physical_field, 1);
        assert_eq!(payload_slot.ty, payload_ty);

        let reference = MirReferenceInterpreter::new(&fixture.program)
            .execute(
                &fixture.function,
                &[MirRuntimeValue::Variant {
                    nominal: fixture.nominal.clone(),
                    variant: fixture.number.clone(),
                    payload: vec![MirRuntimeValue::Int(7)],
                }],
            )
            .expect("reference flat Copy user enum switch");
        let bytecode = BytecodeVM::new(
            compile_mir_program(&fixture.program).expect("flat Copy user enum bytecode"),
        )
        .call_named(
            fixture.function.0.as_str(),
            vec![Value::CanonicalVariant {
                nominal: fixture.nominal.clone(),
                variant: fixture.number.clone(),
                tag: "Number".into(),
                payload: vec![Value::Int(7)],
            }],
        )
        .expect("bytecode flat Copy user enum switch");
        assert_eq!(reference, MirRuntimeValue::Int(7));
        assert!(matches!(bytecode, Value::Int(7)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_flat_copy_user_enum_test");
        generator
            .compile_mir_native(&fixture.program)
            .expect("flat Copy user enum should have a native ABI contract");
        generator
            .module
            .verify()
            .expect("native flat Copy user enum module verifies");
        assert!(generator.module.get_function("take_signal").is_some());
    }

    #[test]
    fn native_emitter_consumes_flat_copy_user_enum_construction_abi() {
        let fixture = crate::core::mir::test_support::direct_flat_copy_enum_construct_fixture();
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_flat_copy_enum_construct");
        generator
            .compile_mir_native(&fixture.program)
            .expect("flat Copy user-enum construction should have a native ABI contract");
        generator
            .module
            .verify()
            .expect("native flat Copy user-enum construction module verifies");
        assert!(generator.module.get_function("construct_signal").is_some());
    }

    #[test]
    fn native_emitter_consumes_surface_flat_copy_user_enum_constructor() {
        let source = include_str!("../../../tests/fixtures/mir_custom_enum_flat_copy.mimi");
        let program = canonical_program(source);
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_surface_flat_copy_enum");
        generator
            .compile_mir_native(&program)
            .expect("surface flat Copy user-enum constructor should use canonical ABI");
        generator
            .module
            .verify()
            .expect("surface flat Copy user-enum constructor module verifies");
        assert!(generator.module.get_function("make_signal").is_some());
    }

    #[test]
    fn native_emitter_consumes_surface_flat_copy_user_enum_match() {
        let source = include_str!("../../../tests/fixtures/mir_custom_enum_flat_copy.mimi");
        let program = canonical_program(source);
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_surface_flat_copy_match");
        generator
            .compile_mir_native(&program)
            .expect("surface flat Copy user-enum match should use canonical ABI");
        generator
            .module
            .verify()
            .expect("surface flat Copy user-enum match module verifies");
        assert!(generator.module.get_function("read_signal").is_some());
    }

    #[test]
    fn native_validator_rejects_mixed_copy_user_enum_before_llvm() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_custom_enum_mixed_copy_rejected.mimi"
        ));
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_mixed_user_enum_test");
        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("mixed Copy user enum must fail the single-slot ABI gate");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("mixed payload ABI is outside the flat Copy variant contract")
        }));
        assert!(
            generator.module.get_function("inspect").is_none(),
            "L2 requires mixed user-enum ABI rejection before LLVM declarations"
        );
    }

    #[test]
    fn native_validator_rejects_tuple_with_unsupported_child_before_llvm_declarations() {
        let program =
            canonical_program("func main() -> i32 { let value = (\"x\", [1]); drop(value); 0 }");
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_tuple_validator_test");

        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("tuple with List child must fail closed in native tuple slice");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("outside the scalar/String/tuple ABI")
        }));
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("recursive tuple contract")));
        assert!(
            generator.module.get_function("main").is_none(),
            "L2 requires unsupported tuple ABI to be rejected before LLVM declarations"
        );
    }

    #[test]
    fn native_validator_rejects_mixed_variant_payload_before_llvm_declarations() {
        let program = canonical_program(
            "func main() -> i64 { let value: Result<i64, bool> = Ok(41); match value { Ok(v) => v, Err(_) => (0 as i64) } }",
        );
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_variant_validator_test");

        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("mixed variant payloads must fail before native emission");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.message.contains("flat Copy variant contract")
                && diagnostic.message.contains("mixed payload ABI")
        }));
        assert!(
            generator.module.get_function("main").is_none(),
            "L2 requires variant validation before LLVM function declarations"
        );
    }

    #[test]
    fn native_generic_record_f64_projection_consumes_float_record_receipt() {
        let program = canonical_program(include_str!(
            "../../../tests/fixtures/mir_native_generic_record_projection_f64.mimi"
        ));
        let instance = program
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
            .expect("generic Record<f64> projection instance");
        let crate::core::mir::MirGenericInstanceContract::ScalarRecordProjection { contract } =
            &instance.contract
        else {
            unreachable!("filtered above");
        };
        assert!(matches!(
            program
                .type_catalog()
                .get(&contract.field_ty)
                .map(|descriptor| descriptor.abi),
            Some(crate::core::mir::types::MirAbiClass::Float { bits: 64 })
        ));
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic Record<f64> projection");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_generic_record_projection_f64");
        generator
            .compile_mir_native(&program)
            .expect("native generic Record<f64> projection must consume MIR");
        generator
            .module
            .verify()
            .expect("native generic Record<f64> projection module verifies");
    }

    #[test]
    fn native_emitter_consumes_checker_owned_scalar_ffi_receipt() {
        let program = canonical_program(
            r#"
extern "C" {
    func getpid() -> i64;
}
func main() -> i64 {
    getpid()
}
"#,
        );
        let receipt = program.ffi_calls().values().next().expect("FFI receipt");
        assert_eq!(receipt.symbol, "getpid");
        assert_eq!(receipt.abi, "C");
        assert!(receipt.result.is_some());

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_scalar_ffi");
        generator
            .compile_mir_native(&program)
            .expect("scalar FFI should use the canonical native declaration");
        generator
            .module
            .verify()
            .expect("scalar FFI native module verifies");
        assert!(generator.module.get_function("getpid").is_some());
        let ir = generator.emit_ir();
        assert!(ir.contains("declare i64 @getpid()"), "{ir}");
        assert!(ir.contains("call i64 @getpid()"), "{ir}");
    }

    #[test]
    fn native_emitter_rejects_string_ffi_before_llvm_declarations() {
        let program = canonical_program(
            r#"
extern "C" {
    func strlen(s: string) -> i64;
}
func main() -> i64 {
    strlen("mimi")
}
"#,
        );
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_string_ffi");
        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("string FFI remains outside scalar native island");
        assert!(diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message.contains("unsupported canonical ABI") }));
        assert!(
            generator.module.get_function("main").is_none(),
            "unsupported FFI must fail before native function declarations"
        );
    }

    #[test]
    fn scalar_ffi_single_and_multiple_calls_share_one_mir_across_three_consumers() {
        use crate::core::mir::reference::MirReferenceFfiResolver;
        use std::cell::RefCell;

        struct LabsOracle(RefCell<Vec<i64>>);
        impl MirReferenceFfiResolver for LabsOracle {
            crate::mir_test_total_ffi_resolver!();
            fn call(
                &self,
                receipt: &crate::core::mir::MirFfiCallContract,
                arguments: &[MirRuntimeValue],
            ) -> Result<MirRuntimeValue, String> {
                if receipt.symbol != "labs" || receipt.abi != "C" {
                    return Err("unexpected foreign symbol or ABI".into());
                }
                let [MirRuntimeValue::Int(n)] = arguments else {
                    return Err("labs requires exactly one i64 argument".into());
                };
                self.0.borrow_mut().push(*n);
                n.checked_abs()
                    .map(MirRuntimeValue::Int)
                    .ok_or_else(|| "labs oracle excludes LONG_MIN".into())
            }
        }

        // Serialize against the compatibility FFI tests' library overrides.
        let _guard = crate::tests::FfiEnvLock::lock();
        for inputs in [vec![42_i64], vec![-17, -25], vec![0, -42, 42]] {
            let calls = inputs
                .iter()
                .map(|n| format!("println(labs({n} as i64))"))
                .collect::<Vec<_>>()
                .join("\n");
            let source = format!(
                "extern \"C\" {{ func labs(n: i64) -> i64; }}\nfunc main() -> i64 {{ {calls}\n 0 }}"
            );
            let expected = inputs
                .iter()
                .map(|n| format!("{}\n", n.abs()))
                .collect::<String>();
            let program = canonical_program(&source);
            let digest = program.canonical_digest();
            assert_eq!(program.ffi_calls().len(), inputs.len());

            let oracle = LabsOracle(RefCell::new(Vec::new()));
            let reference = MirReferenceInterpreter::new(&program)
                .with_ffi_resolver(&oracle)
                .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
                .expect("reference FFI with explicit oracle");
            assert_eq!(*oracle.0.borrow(), inputs);
            assert_eq!(reference.value, MirRuntimeValue::Int(0));
            assert_eq!(reference.output, expected);

            let bytecode = compile_mir_program(&program).expect("AST-free FFI bytecode");
            assert!(bytecode.ast.is_none());
            assert!(bytecode.extern_names.is_empty());
            let mut vm = BytecodeVM::new(bytecode);
            assert!(matches!(
                vm.run_value().expect("libc FFI VM execution"),
                Value::Int(0)
            ));
            assert_eq!(vm.stdout(), expected);

            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, "mir_ffi_three_consumers");
            generator
                .compile_mir_native(&program)
                .expect("FFI native lowering");
            generator.module.verify().expect("valid FFI LLVM module");
            let native = crate::tests::link_and_observe_canonical_mir(&generator)
                .expect("real native FFI execution");
            assert_eq!(native.exit_code, Some(0));
            assert_eq!(native.stdout, expected);
            assert_eq!(native.stderr, "");
            assert_eq!(program.canonical_digest(), digest);
        }
    }
}
