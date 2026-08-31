//! Native lowering directly from checker-owned Typed Resolved IR.
//!
//! This module is a capability boundary: it accepts `CheckedProgram` and
//! canonical identities only. Surface `File`/`FuncDef`/`Stmt`/`Expr` are not
//! imported here, and unsupported nodes fail closed instead of falling back to
//! the legacy emitter.

mod eligibility;
mod types;

use crate::codegen::mono_recover::infer_type_args_from_call_site;
use std::collections::BTreeMap;

use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, StructType};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, PointerValue};

use crate::ast::BinOp;
use crate::codegen::{CallSiteValueExt, CodeGenerator};
use crate::core::ir::{ContractKind, ResolvedFStringPart};
use crate::core::ir::{ResolvedBinaryOp, ResolvedUnaryOp};
use crate::core::{
    CheckedConversion, CheckedConversionKind, CheckedProgram, FunctionTypeAbi, MethodId, NodeId,
    PrimitiveType, ResolvedBlock, ResolvedBody, ResolvedCall, ResolvedCallee, ResolvedConstValue,
    ResolvedExpr, ResolvedExprKind, ResolvedLiteral, ResolvedLocalId, ResolvedPattern,
    ResolvedPatternKind, ResolvedPlace, ResolvedStmtKind, ResolvedType, ResolvedTypeId,
};
use crate::diagnostic::Diagnostic;
use crate::error::CompileError;

use self::eligibility::{
    eligible_function_ids_with_stats, is_core_kernel_function, require_resolved_native_program,
    DispatchStats, UnsupportedResolvedNode,
};
use self::types::{llvm_type_for_resolved, llvm_type_for_resolved_with};

/// Mailbox blob capacity in bytes. MUST stay in sync with
/// `MIMI_ACTOR_BLOB_SIZE` in `src/runtime/actor.rs`.
const RESOLVED_ACTOR_BLOB_CAPACITY: u64 = 256;

/// Future data region offset; must stay in sync with `src/runtime/future.rs`.
const RESOLVED_FUTURE_DATA_OFFSET: u64 = 16;

pub(super) fn supports_resolved_native(program: &CheckedProgram) -> bool {
    require_resolved_native_program(program).is_ok()
}

/// Returns the set of function NodeIds eligible for resolved native compilation.
/// Returns None if program-level blockers prevent any resolved compilation.
/// Also returns structured dispatch stats (0.34.40, MIMI_STAT=1).
/// `verify_contracts` (0.34.41): gate contract-bearing functions (erased when false).
pub(super) fn resolved_eligible_functions(
    program: &CheckedProgram,
    verify_contracts: bool,
) -> Option<std::collections::BTreeSet<NodeId>> {
    match eligible_function_ids_with_stats(program, verify_contracts) {
        Ok((set, stats)) if !set.is_empty() => {
            emit_dispatch_stats(&stats);
            Some(set)
        }
        Ok((_set, stats)) => {
            // Emit stats even when nothing is eligible (fallback rate = 1.0).
            emit_dispatch_stats(&stats);
            if std::env::var("MIMI_VERBOSE").is_ok() {
                eprintln!(
                    "info: resolved dispatch: 0 eligible functions (all filtered per-function)"
                );
            }
            None
        }
        Err(blocker) => {
            if std::env::var("MIMI_VERBOSE").is_ok() {
                eprintln!("info: resolved dispatch blocked: {}", blocker.reason);
            }
            None
        }
    }
}

/// When `MIMI_STAT=1`, write the structured dispatch report as JSON to
/// `MIMI_STAT_OUT` (default: `target/mimi-stat/<program>.json`).
pub(super) fn emit_dispatch_stats(stats: &DispatchStats) {
    if std::env::var("MIMI_STAT").map_or(false, |v| v == "1") {
        let out_dir = std::env::var("MIMI_STAT_OUT").unwrap_or_else(|_| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("mimi-stat")
                .to_string_lossy()
                .to_string()
        });
        let dir = std::path::Path::new(&out_dir);
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("warning: mimi-stat: cannot create {}: {}", out_dir, e);
            return;
        }
        let file_name = format!("{}.json", stats.program);
        let path = dir.join(file_name);
        match serde_json::to_string_pretty(stats) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    eprintln!("warning: mimi-stat: cannot write {}: {}", path.display(), e);
                }
            }
            Err(e) => eprintln!("warning: mimi-stat: serialize: {}", e),
        }
    }
}

#[derive(Clone, Copy)]
struct ResolvedVarEntry<'ctx> {
    storage: PointerValue<'ctx>,
    llvm_type: BasicTypeEnum<'ctx>,
}

struct ResolvedFrame<'ctx> {
    owner: NodeId,
    locals: BTreeMap<ResolvedLocalId, ResolvedVarEntry<'ctx>>,
    /// 0.34.41 第二档: entry snapshots for `old(x)` occurrences in ensures
    /// conditions, keyed by the NodeId of the `Old` expression occurrence.
    /// Empty unless `verify_contracts` is on and the callable has ensures.
    old_snapshots: BTreeMap<NodeId, ResolvedVarEntry<'ctx>>,
}

/// Loop context for `break`/`continue` lowering.
#[derive(Clone, Copy)]
struct LoopContext<'ctx> {
    header: inkwell::basic_block::BasicBlock<'ctx>,
    exit: inkwell::basic_block::BasicBlock<'ctx>,
}

/// Enumeration used by `emit_try` to distinguish builtin Result/Option from
/// custom Ok/Err enum values.
#[derive(Clone, Copy)]
enum TryInnerKind {
    ResolvedBuiltinResult,
    ResolvedBuiltinOption,
    CustomEnum,
}

struct NativeResolvedEmitter<'program, 'generator, 'ctx> {
    program: &'program CheckedProgram,
    generator: &'generator mut CodeGenerator<'ctx>,
    loop_stack: Vec<LoopContext<'ctx>>,
    /// Per-callable place inputs (dynamic index expressions). Set before
    /// emitting each function body, cleared after.
    place_inputs: BTreeMap<NodeId, crate::core::ResolvedExpr>,
    /// 0.36.15 (Phase D 预研 L1 修复): scope-guard semantics on the resolved
    /// (production, `mimi build`/compile_checked) emitter. The legacy
    /// func.rs/block.rs machinery registers `defer`/`on failure` blocks at
    /// their statement position and emits them at scope exit; the resolved
    /// emitter previously inlined both kinds at their statement position, so
    /// `defer` bodies ran BEFORE the body statements and `on failure` fired on
    /// NORMAL exits (the dual harness uses legacy compile_file and stayed
    /// green — the CLI path miscompiled). These stacks mirror the
    /// register-emit model: blocks are recorded here at statement position and
    /// emitted LIFO at function exits (defer always; on-failure only on
    /// fault/exit(...) paths, discarded on normal return).
    defer_scopes: Vec<ResolvedBlock>,
    comp_scopes: Vec<ResolvedBlock>,
    /// 0.39.x E0722 根治 scaffold: composite-T / cap generic call sites that
    /// currently route to the legacy monomorphizer (route-a, Call arm) are
    /// recorded here, keyed by (callee qualified name, concrete type args). A
    /// later round can emit resolved monomorphized instances for these instead
    /// of falling back to legacy. Populated without changing emission behavior.
    pending_generic_instances: Vec<(String, Vec<ResolvedTypeId>)>,
}

impl<'ctx> CodeGenerator<'ctx> {
    /// Migration entry point for the resolved native Typed IR emitter.
    ///
    /// Unlike `compile_checked`, this function has no surface-AST fallback.
    /// It currently accepts primitive scalar callables with control flow
    /// (if/while/for/break/continue); extending the accepted schema is done
    /// slice by slice in `codegen::resolved`.
    pub fn compile_resolved_native(
        &mut self,
        program: &CheckedProgram,
    ) -> Result<(), Vec<Diagnostic>> {
        program.validate_backend(crate::core::BackendProfile::Native)?;
        if let Err(error) = require_resolved_native_program(program) {
            return Err(vec![unsupported_diagnostic(program, error)]);
        }
        NativeResolvedEmitter {
            program,
            generator: self,
            loop_stack: Vec::new(),
            place_inputs: BTreeMap::new(),
            defer_scopes: Vec::new(),
            comp_scopes: Vec::new(),
            pending_generic_instances: Vec::new(),
        }
        .compile_program()
        .map_err(|error| {
            let mut diagnostic = error.to_diagnostic();
            if let Some(span) = program.entry_span() {
                diagnostic = diagnostic.with_span(span);
            }
            vec![diagnostic]
        })
    }

    /// Compile only the eligible subset of functions through the resolved
    /// emitter. Ineligible functions are left for the legacy emitter.
    /// Returns the number of functions compiled.
    pub fn compile_resolved_subset(
        &mut self,
        program: &CheckedProgram,
        eligible: &std::collections::BTreeSet<NodeId>,
    ) -> Result<(usize, Vec<(String, Vec<ResolvedTypeId>)>), Vec<Diagnostic>> {
        program.validate_backend(crate::core::BackendProfile::Native)?;
        NativeResolvedEmitter {
            program,
            generator: self,
            loop_stack: Vec::new(),
            place_inputs: BTreeMap::new(),
            defer_scopes: Vec::new(),
            comp_scopes: Vec::new(),
            pending_generic_instances: Vec::new(),
        }
        .compile_subset(eligible)
        .map_err(|error| {
            let mut diagnostic = error.to_diagnostic();
            if let Some(span) = program.entry_span() {
                diagnostic = diagnostic.with_span(span);
            }
            vec![diagnostic]
        })
    }
}

fn unsupported_diagnostic(program: &CheckedProgram, error: UnsupportedResolvedNode) -> Diagnostic {
    let mut diagnostic = CompileError::Unsupported(format!(
        "resolved native slice rejected owner '{}' node '{}': {}",
        error.owner.0, error.node.0, error.reason
    ))
    .to_diagnostic();
    if let Some(span) = program
        .node_meta()
        .get(&error.node)
        .map(|meta| meta.origin.user_span())
        .or_else(|| program.entry_span())
    {
        diagnostic = diagnostic.with_span(span);
    }
    diagnostic
}

/// Convert a generic callee's resolved type arguments into the AST type map
/// expected by the legacy monomorphizer (`compile_generic_func`). This is used
/// when the resolved native slice emits a call to a generic function whose type
/// variable cannot be substituted inline (e.g. `List<T> -> T` returning a heap
/// type), so the monomorphized instance is built via the legacy path with the
/// concrete args taken from the resolved IR's `call.type_arguments`.
fn resolved_type_args_to_ast(
    generics: &[crate::ast::GenericParam],
    type_args: &[crate::core::ResolvedTypeId],
    table: &crate::core::ResolvedTypeTable,
) -> std::collections::HashMap<String, crate::ast::Type> {
    let mut map = std::collections::HashMap::new();
    for (gp, tid) in generics.iter().zip(type_args.iter()) {
        if let Some(rt) = table.get(tid) {
            if let Some(ast_ty) = resolved_type_to_ast(rt, table) {
                map.insert(gp.name.clone(), ast_ty);
            }
        }
    }
    map
}

/// Best-effort conversion of a resolved type to an AST type for monomorphization.
pub(super) fn resolved_type_to_ast(
    rt: &crate::core::ResolvedType,
    table: &crate::core::ResolvedTypeTable,
) -> Option<crate::ast::Type> {
    use crate::core::ResolvedType::*;
    match rt {
        Primitive(p) => {
            let name = match p {
                crate::core::PrimitiveType::I8 => "i8",
                crate::core::PrimitiveType::I16 => "i16",
                crate::core::PrimitiveType::I32 => "i32",
                crate::core::PrimitiveType::I64 => "i64",
                crate::core::PrimitiveType::I128 => "i128",
                crate::core::PrimitiveType::U8 => "u8",
                crate::core::PrimitiveType::U16 => "u16",
                crate::core::PrimitiveType::U32 => "u32",
                crate::core::PrimitiveType::U64 => "u64",
                crate::core::PrimitiveType::U128 => "u128",
                crate::core::PrimitiveType::Isize => "isize",
                crate::core::PrimitiveType::Usize => "usize",
                crate::core::PrimitiveType::F32 => "f32",
                crate::core::PrimitiveType::F64 => "f64",
                crate::core::PrimitiveType::Bool => "bool",
                crate::core::PrimitiveType::Char => "char",
                crate::core::PrimitiveType::String => "string",
                _ => return None,
            };
            Some(crate::ast::Type::Name(name.to_string(), vec![]))
        }
        Nominal {
            item, arguments, ..
        } => {
            let item_str = item.as_str();
            let name = item_str
                .strip_prefix("type:")
                .or_else(|| item_str.strip_prefix("builtin:type:"))
                .unwrap_or(item_str);
            let args: Vec<crate::ast::Type> = arguments
                .iter()
                .filter_map(|tid| table.get(tid).and_then(|t| resolved_type_to_ast(t, table)))
                .collect();
            Some(crate::ast::Type::Name(name.to_string(), args))
        }
        // 0.1.9 (bare-T return ABI, L1): monomorphized generic instances whose
        // bare type parameter `T` is instantiated to a composite (tuple / Option
        // / Result) must lower that composite into the legacy `type_map` so the
        // instance's parameter and return layouts match the resolved caller.
        // Without these arms `resolved_type_to_ast` returned `None` for the
        // composite, leaving `type_map` empty and erasing the bare-T slot to the
        // i64 skeleton — which then disagrees with the caller's real `{i64,i64}`
        // (or `{i1, …}`) value and crashes codegen (E0700 / numeric-convert).
        Tuple(elements) => {
            let elems: Vec<crate::ast::Type> = elements
                .iter()
                .filter_map(|tid| table.get(tid).and_then(|t| resolved_type_to_ast(t, table)))
                .collect();
            if elems.len() == elements.len() {
                Some(crate::ast::Type::Tuple(elems))
            } else {
                None
            }
        }
        Option(payload) => table
            .get(payload)
            .and_then(|t| resolved_type_to_ast(t, table))
            .map(|p| crate::ast::Type::Name("Option".to_string(), vec![p])),
        Result { ok, error } => {
            let ok_t = table.get(ok).and_then(|t| resolved_type_to_ast(t, table));
            let err_t = table
                .get(error)
                .and_then(|t| resolved_type_to_ast(t, table));
            match (ok_t, err_t) {
                (Some(o), Some(e)) => {
                    Some(crate::ast::Type::Name("Result".to_string(), vec![o, e]))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// A3 compatibility gate over the A1 canonical ownership shape.
///
/// `Set`/`Map` and the proven unsafe nested-list shapes remain fail-closed with
/// E0723 until A2 can derive their return-transfer glue.  Keeping this thin
/// wrapper lets the top-level native gate name the policy without rebuilding a
/// parallel AST or LLVM-layout classifier.
pub(crate) fn native_return_owns_unclaimed_heap(
    program: &crate::core::CheckedProgram,
    ty: &crate::core::ResolvedTypeId,
) -> bool {
    crate::codegen::abi::ownership::classify_resolved(program, ty).has_unclaimed_return_heap()
}

impl<'program, 'generator, 'ctx> NativeResolvedEmitter<'program, 'generator, 'ctx> {
    fn compile_program(&mut self) -> Result<(), CompileError> {
        let mut functions: Vec<_> = self
            .program
            .functions()
            .values()
            .filter(|function| !function.is_comptime)
            .collect();
        functions.sort_by(|left, right| left.node_id.cmp(&right.node_id));

        for function in &functions {
            let callable = self.program.callable(&function.node_id).ok_or_else(|| {
                CompileError::Unsupported(format!(
                    "resolved callable '{}' is absent",
                    function.node_id.0
                ))
            })?;
            self.declare_callable(callable)?;
        }
        for function in functions {
            let callable = self.program.callable(&function.node_id).ok_or_else(|| {
                CompileError::Unsupported(format!(
                    "resolved callable '{}' is absent",
                    function.node_id.0
                ))
            })?;
            self.emit_callable(callable)?;
        }
        self.generator.module.verify().map_err(|message| {
            CompileError::LlvmError(format!(
                "resolved native module verification failed: {message}"
            ))
        })
    }

    /// Compile only the functions in the eligible set. Does NOT verify the
    /// module (the legacy emitter will add remaining functions afterwards).
    fn compile_subset(
        &mut self,
        eligible: &std::collections::BTreeSet<NodeId>,
    ) -> Result<(usize, Vec<(String, Vec<ResolvedTypeId>)>), CompileError> {
        let mut functions: Vec<_> = self
            .program
            .functions()
            .values()
            .filter(|function| !function.is_comptime && eligible.contains(&function.node_id))
            .collect();
        functions.sort_by(|left, right| left.node_id.cmp(&right.node_id));

        // Declare all eligible functions first (for mutual recursion).
        for function in &functions {
            let callable = self.program.callable(&function.node_id).ok_or_else(|| {
                CompileError::Unsupported(format!(
                    "resolved callable '{}' is absent",
                    function.node_id.0
                ))
            })?;
            self.declare_callable(callable)?;
        }
        // Emit eligible functions. If a function fails (e.g., calls an
        // unimplemented builtin), skip it — the legacy emitter will handle it.
        // The compile_func_legacy skip guard (count_basic_blocks != 0) ensures the
        // legacy emitter won't re-emit successfully compiled functions.
        // Set MIMI_VERBOSE=1 to see per-function fallback details.
        let mut count = 0;
        let mut failed = 0;
        for function in functions {
            let callable = self.program.callable(&function.node_id).ok_or_else(|| {
                CompileError::Unsupported(format!(
                    "resolved callable '{}' is absent",
                    function.node_id.0
                ))
            })?;
            match self.emit_callable(callable) {
                Ok(()) => {
                    // P1-4 fix: verify each emitted function individually.
                    // compile_subset() cannot call module.verify() because the
                    // legacy emitter will add remaining functions later. But
                    // each function we emit must be valid LLVM IR on its own.
                    let symbol = function.qualified_name.clone();
                    if let Some(llvm_fn) = self.generator.module.get_function(&symbol) {
                        let verbose = std::env::var("MIMI_VERBOSE").is_ok();
                        if !llvm_fn.verify(verbose) {
                            if verbose {
                                eprintln!(
                                    "warning: resolved emitter verification failed for '{}'",
                                    symbol
                                );
                                // Dump the function IR for diagnosis.
                                let ir_str = llvm_fn.to_string();
                                // Print first 30 lines to avoid flooding.
                                for (i, line) in ir_str.lines().enumerate() {
                                    if i >= 30 {
                                        eprintln!("  ... (truncated)");
                                        break;
                                    }
                                    eprintln!("  | {}", line);
                                }
                            }
                            // Track failed functions so the legacy emitter
                            // can re-compile them (clearing the partial body
                            // before re-emitting).
                            self.generator
                                .resolved_failed_functions
                                .insert(symbol.clone());
                            // 0.34.42: clear the partial body HERE. The legacy
                            // skip guard is keyed on surface func.name which
                            // can differ from the LLVM symbol (impl method
                            // mangling); a leftover terminator-less stub
                            // segfaults LLVM's pass pipeline.
                            self.clear_partial_body(llvm_fn);
                            if is_core_kernel_function(self.program, function) {
                                return Err(CompileError::Unsupported(format!(
                                    "resolved emitter hard-error for core callee '{}': LLVM verification failed",
                                    symbol
                                )));
                            }
                            failed += 1;
                            continue;
                        }
                    }
                    count += 1;
                }
                Err(e) => {
                    // F-004 (0.40.1.8) / A3 E0723: a fail-closed ownership error
                    // (code E0723 — e.g. returning a closure that captures a heap
                    // value) must NOT be silently downgraded to the legacy emitter;
                    // that would re-emit broken IR that aliases freed heap. Escalate
                    // it as a hard error instead of falling back.
                    if e.code() == "E0723" {
                        return Err(e);
                    }
                    // Function failed to emit through resolved path.
                    // Record in failed set — the legacy emitter's skip check
                    // will handle it by deleting the partial body and
                    // re-compiling from scratch.
                    let symbol = function.qualified_name.clone();
                    self.generator
                        .resolved_failed_functions
                        .insert(symbol.clone());
                    // 0.34.42: same clear-on-failure as the verify-fail arm —
                    // never leave a terminator-less stub in the module.
                    if let Some(llvm_fn) = self.generator.module.get_function(&symbol) {
                        self.clear_partial_body(llvm_fn);
                    }
                    if is_core_kernel_function(self.program, function) {
                        return Err(CompileError::Unsupported(format!(
                            "resolved emitter hard-error for core callee '{}': {}",
                            function.qualified_name, e
                        )));
                    }
                    if std::env::var("MIMI_VERBOSE").is_ok() {
                        eprintln!(
                            "warning: resolved emitter fallback for '{}': {}",
                            function.qualified_name, e
                        );
                    }
                    failed += 1;
                }
            }
        }
        if std::env::var("MIMI_VERBOSE").is_ok() && (count + failed) > 0 {
            eprintln!(
                "info: resolved emitter compiled {}/{} function(s), {} fell back to legacy",
                count,
                count + failed,
                failed
            );
        }
        Ok((count, std::mem::take(&mut self.pending_generic_instances)))
    }

    /// 0.34.42: delete every basic block of a partially-emitted function,
    /// restoring it to a pure declaration. Keeps the symbol alive (callers
    /// compiled by the resolved emitter hold value references) while making
    /// the body slot reusable by whichever emitter recompiles it. Mirrors the
    /// clear loop in func.rs compile_func_legacy_inner.
    /// 0.34.42: delete every basic block of a partially-emitted function,
    /// restoring it to a pure declaration. Keeps the symbol alive (callers
    /// compiled by the resolved emitter hold value references) while making
    /// the body slot reusable by whichever emitter recompiles it.
    ///
    /// 0.39.x matrix sweep: delegates to the shared three-pass teardown on
    /// CodeGenerator (valgrind-pinned: appearance-order deletion corrupted the
    /// heap; see the long comment there for the use-before-def analysis).
    fn clear_partial_body(&self, function: inkwell::values::FunctionValue<'ctx>) {
        self.generator.clear_partial_body(function);
    }

    fn callable_symbol(&self, owner: &NodeId) -> Result<&str, CompileError> {
        let function = self.program.functions().get(owner).ok_or_else(|| {
            CompileError::Unsupported(format!("function catalog has no owner '{}'", owner.0))
        })?;
        if function.qualified_name.contains("::") {
            return Err(CompileError::Unsupported(format!(
                "qualified symbol '{}' is not in the scalar-leaf slice",
                function.qualified_name
            )));
        }
        Ok(&function.qualified_name)
    }

    /// Lower a ResolvedTypeId to an LLVM type, with fallback for
    /// user-defined record Nominal types that types.rs doesn't handle.
    /// 0.36.35: nominal-resolution hook for Flow-state record layouts —
    /// resolves 'state:Flow::State' against the legacy type_defs record
    /// ("flow::Flow::State"), so container payloads (Result/Option slots)
    /// get the SAME struct as top-level state values.
    fn state_nominal_llvm_type(&self, id: &ResolvedTypeId) -> Option<BasicTypeEnum<'ctx>> {
        let ResolvedType::Nominal { item, .. } = self.program.resolved_types().get(id)? else {
            return None;
        };
        let item_str = item.as_str();
        let state_path = item_str.strip_prefix("state:")?;
        let flow_type_name = format!("flow::{state_path}");
        let td = self.generator.type_defs.get(&flow_type_name).or_else(|| {
            state_path
                .rsplit("::")
                .next()
                .and_then(|short| self.generator.type_defs.get(short))
        })?;
        let crate::ast::TypeDefKind::Record(fields) = &td.kind else {
            return None;
        };
        let mut field_types = Vec::with_capacity(fields.len());
        for field in fields {
            field_types.push(self.generator.llvm_type_for(&field.ty)?);
        }
        Some(BasicTypeEnum::StructType(
            self.generator.context.struct_type(&field_types, false),
        ))
    }

    fn lower_type(&self, id: &ResolvedTypeId) -> Result<BasicTypeEnum<'ctx>, CompileError> {
        let mut nominal_hook = |id: &ResolvedTypeId| self.state_nominal_llvm_type(id);
        match llvm_type_for_resolved_with(
            self.generator.context,
            self.program.resolved_types(),
            id,
            &mut nominal_hook,
        ) {
            Ok(ty) => Ok(ty),
            Err(_) => {
                // 0.32.14: Newtype is transparent — lower to the inner type.
                if let Some(ResolvedType::Newtype { inner, .. }) =
                    self.program.resolved_types().get(id)
                {
                    return self.lower_type(inner);
                }
                // Check if this is a user-defined Nominal type (record or enum).
                if let Some(ResolvedType::Nominal { item, .. }) =
                    self.program.resolved_types().get(id)
                {
                    let item_str = item.as_str();
                    // 0.32.20: Flow state types (state:FlowName::StateName).
                    // The legacy emitter registers them as TypeDefs with
                    // qualified name "flow::FlowName::StateName". Look up
                    // the legacy type_defs to build the LLVM struct type.
                    if let Some(state_path) = item_str.strip_prefix("state:") {
                        let flow_type_name = format!("flow::{state_path}");
                        let td = self
                            .generator
                            .type_defs
                            .get(&flow_type_name)
                            .or_else(|| {
                                // Fallback: try unqualified state name.
                                state_path
                                    .rsplit("::")
                                    .next()
                                    .and_then(|short| self.generator.type_defs.get(short))
                            })
                            .ok_or_else(|| {
                                CompileError::Unsupported(format!(
                                    "flow state type '{item_str}' not found in legacy type_defs"
                                ))
                            })?;
                        if let crate::ast::TypeDefKind::Record(fields) = &td.kind {
                            let mut field_types = Vec::with_capacity(fields.len());
                            for field in fields {
                                let ft =
                                    self.generator.llvm_type_for(&field.ty).ok_or_else(|| {
                                        CompileError::Unsupported(format!(
                                            "flow state field '{}' type not lowerable",
                                            field.name
                                        ))
                                    })?;
                                field_types.push(ft);
                            }
                            return Ok(BasicTypeEnum::StructType(
                                self.generator.context.struct_type(&field_types, false),
                            ));
                        }
                        return Err(CompileError::Unsupported(format!(
                            "flow state type '{item_str}' is not a record"
                        )));
                    }
                    // 0.32.12: Enum types lower to {i32 tag, i64 payload}.
                    let type_name = item_str.strip_prefix("type:").unwrap_or(item_str);
                    let is_enum = self.program.type_defs().values().any(|td| {
                        (td.qualified_name == type_name || td.qualified_name == item_str)
                            && matches!(td.kind, crate::core::resolved::ResolvedTypeKind::Enum)
                    });
                    if is_enum {
                        let i32_ty = self.generator.context.i32_type();
                        let i64_ty = self.generator.context.i64_type();
                        return Ok(BasicTypeEnum::StructType(
                            self.generator.context.struct_type(
                                &[
                                    BasicTypeEnum::IntType(i32_ty),
                                    BasicTypeEnum::IntType(i64_ty),
                                ],
                                false,
                            ),
                        ));
                    }
                    // 0.35.23 deep-eval: builtin containers
                    // (builtin:type:List / Map / Set) have NO record type def —
                    // record_llvm_type would fail "type definition not found"
                    // (mimi-log main fell back to legacy, then hit the legacy
                    // List<record> for-loop gap). Only identities with an
                    // actual record def reach the record path; everything else
                    // falls back to the pure lookup, which knows the builtin
                    // container layouts ({i64 len, ptr} for List, handles for
                    // Map/Set).
                    let has_record_def = self.program.type_defs().values().any(|td| {
                        (td.qualified_name == type_name || td.qualified_name == item_str)
                            && matches!(td.kind, crate::core::resolved::ResolvedTypeKind::Record)
                    });
                    if has_record_def {
                        let sty = self.record_llvm_type(item)?;
                        Ok(BasicTypeEnum::StructType(sty))
                    } else {
                        llvm_type_for_resolved(
                            self.generator.context,
                            self.program.resolved_types(),
                            id,
                        )
                    }
                } else {
                    // Re-propagate the original error.
                    llvm_type_for_resolved(
                        self.generator.context,
                        self.program.resolved_types(),
                        id,
                    )
                }
            }
        }
    }

    fn declare_callable(
        &mut self,
        callable: &crate::core::ResolvedCallable,
    ) -> Result<(), CompileError> {
        let symbol = self.callable_symbol(&callable.owner)?.to_string();
        let result = self.lower_type(&callable.signature.result)?;
        let mut parameters = callable
            .signature
            .parameters
            .iter()
            .map(|parameter| {
                // 0.34.43: non-self view/mutate borrow parameters use the
                // pointer ABI — identical to legacy declare_func, which
                // declares borrowed params as ptr so callee stores are
                // visible to the caller (true reference semantics).
                if parameter.name != "self"
                    && matches!(
                        parameter.permission,
                        Some(crate::core::ir::Permission::View)
                            | Some(crate::core::ir::Permission::Mutate)
                    )
                {
                    return Ok(BasicMetadataTypeEnum::from(
                        self.generator
                            .context
                            .ptr_type(inkwell::AddressSpace::default()),
                    ));
                }
                self.lower_type(&parameter.ty)
                    .map(BasicMetadataTypeEnum::from)
            })
            .collect::<Result<Vec<_>, CompileError>>()?;
        // 0.35.23 deep-eval: main carries the C (argc, argv) pair (legacy
        // declare_func parity) so the native entry seeds mimi_args_init.
        if symbol == "main" {
            parameters.insert(
                0,
                BasicMetadataTypeEnum::IntType(self.generator.context.i32_type()),
            );
            parameters.insert(
                1,
                BasicMetadataTypeEnum::PointerType(
                    self.generator
                        .context
                        .ptr_type(inkwell::AddressSpace::default()),
                ),
            );
        }
        let function_type = match result {
            BasicTypeEnum::IntType(ty) => ty.fn_type(&parameters, false),
            BasicTypeEnum::FloatType(ty) => ty.fn_type(&parameters, false),
            BasicTypeEnum::PointerType(ty) => ty.fn_type(&parameters, false),
            BasicTypeEnum::StructType(ty) => ty.fn_type(&parameters, false),
            BasicTypeEnum::ArrayType(ty) => ty.fn_type(&parameters, false),
            other => {
                return Err(CompileError::Unsupported(format!(
                    "resolved callable '{}' has unsupported LLVM result {other:?}",
                    callable.owner.0
                )))
            }
        };
        if self.generator.module.get_function(&symbol).is_none() {
            self.generator
                .module
                .add_function(&symbol, function_type, None);
        } else {
            // 0.39.136: cross-emitter ABI guard. The symbol was pre-declared by
            // the legacy pass; if its LLVM signature disagrees with the
            // resolved lowering (historically: `Result<(), E>` lowered to None
            // → legacy i64 fallback while the resolved body emitted a struct
            // return), emitting the body would produce INVALID IR (signature
            // says i64, terminator says struct) that segfaults callers.
            // Fail loud here so the function is left to the legacy emitter.
            let existing = self
                .generator
                .module
                .get_function(&symbol)
                .expect("checked above");
            let existing_ty = existing.get_type();
            let resolved_ty = function_type;
            let ret_compatible = existing_ty.get_return_type() == resolved_ty.get_return_type();
            let param_compatible = existing_ty.get_param_types() == resolved_ty.get_param_types()
                && existing_ty.is_var_arg() == resolved_ty.is_var_arg();
            if !ret_compatible || !param_compatible {
                return Err(CompileError::Unsupported(format!(
                    "resolved callable '{symbol}' ABI mismatch with legacy declaration \
                     (resolved ret {:?} vs declared {:?}) — refusing to emit mismatched body",
                    resolved_ty.get_return_type(),
                    existing_ty.get_return_type(),
                )));
            }
        }
        Ok(())
    }

    fn emit_callable(
        &mut self,
        callable: &crate::core::ResolvedCallable,
    ) -> Result<(), CompileError> {
        // Install per-callable place inputs (dynamic index expressions).
        self.place_inputs = callable.body.place_inputs.clone();
        let symbol = self.callable_symbol(&callable.owner)?.to_string();
        let function = self.generator.module.get_function(&symbol).ok_or_else(|| {
            CompileError::LlvmError(format!("resolved declaration '{symbol}' is absent"))
        })?;
        if function.count_basic_blocks() != 0 {
            return Err(CompileError::LlvmError(format!(
                "resolved callable '{symbol}' was emitted more than once"
            )));
        }
        let entry = self.generator.context.append_basic_block(function, "entry");
        self.generator.builder.position_at_end(entry);
        // 0.35.23 deep-eval: resolved native entry — seed mimi_args_init
        // (declare_callable added the (argc: i32, argv: ptr) pair).
        if symbol == "main" {
            if let Some(args_init_fn) = self.generator.module.get_function("mimi_args_init") {
                if let (Some(argc), Some(argv)) =
                    (function.get_nth_param(0), function.get_nth_param(1))
                {
                    let argc_i32 = argc.into_int_value();
                    self.generator.build_call(
                        args_init_fn,
                        &[argc_i32.into(), argv.into()],
                        "mimi_args_init",
                    )?;
                }
            }
        }
        // Function-level heap scope with a boundary marker (legacy B9 shape,
        // func.rs begin_function_heap_scope): every return path emits its own
        // frees via flush_heap_scopes_to_boundary (frees without popping), and
        // the scope bookkeeping is popped here exactly once by
        // end_function_heap_scope — no matter how many returns the body has,
        // and never after a terminator (roadmap Wave-2 #1d: the old
        // free-after-ret shape left dangling instructions that the
        // per-function verify() rejected, silently demoting functions to
        // legacy).
        self.generator.begin_function_heap_scope();
        let mut frame = ResolvedFrame {
            owner: callable.owner.clone(),
            locals: BTreeMap::new(),
            old_snapshots: BTreeMap::new(),
        };
        self.bind_parameters(callable, function, &mut frame)?;
        // 0.36.15 L1: guard stacks are per-function — clear residue from the
        // previous callable and bind the enclosing body for deferred emission.
        self.defer_scopes.clear();
        self.comp_scopes.clear();
        // 0.34.41 第二档: contract guard emission (--verify-contracts).
        // Mirrors legacy func.rs ordering: requires asserts run at entry after
        // parameter binding, before the body; old() snapshots are captured at
        // entry so postconditions observe pre-call values.
        if self.generator.verify_contracts && !callable.contracts.is_empty() {
            self.emit_contract_prologue(callable, &mut frame)?;
        }
        let value = self.emit_block(&callable.body, &callable.body.root, &mut frame)?;
        if self.current_block_terminated() {
            // An early Return statement already emitted its path-specific
            // heap flush BEFORE the ret (emit_statement Return arm). Only
            // the bookkeeping needs balancing here; emitting anything after
            // the terminator would dangle.
            self.generator.end_function_heap_scope();
            return Ok(());
        }
        // 0.36.15 L1: fallthrough exit — deferred blocks run LIFO before the
        // implicit return; on-failure compensations are discarded (normal
        // exit).
        let pending_defers = std::mem::take(&mut self.defer_scopes);
        self.emit_guard_stack(&callable.body, pending_defers, &mut frame)?;
        self.comp_scopes.clear();
        let result_type = self.lower_type(&callable.signature.result)?;
        let value = match value {
            Some(value) => value,
            None if matches!(
                self.program
                    .resolved_types()
                    .get(&callable.signature.result),
                Some(ResolvedType::Primitive(crate::core::PrimitiveType::Unit))
            ) =>
            {
                result_type.const_zero()
            }
            None => {
                return Err(CompileError::Unsupported(format!(
                    "resolved callable '{}' has no value for its non-unit result",
                    callable.owner.0
                )))
            }
        };
        let value = self.coerce_to(value, result_type)?;
        // 0.34.41 第二档: ensures guards before any return cleanup (legacy
        // emit_return checks ensures before heap teardown).
        self.emit_ensures_checks(callable, Some(value), &mut frame)?;
        let ownership = crate::codegen::abi::ownership::classify_resolved(
            self.program,
            &callable.signature.result,
        );
        // A2 (0.40.3.2): an adopted StringBox/tuple/record return is cloned as
        // one canonical ownership value.  This replaces both the top-level
        // string probe and the record-field recursion for that return, so no
        // fresh child can be cloned twice and orphaned. Unsupported shapes
        // keep the proven old path without widening the acceptance surface.
        let glue_value = self
            .generator
            .clone_return_with_derived_glue(&ownership, value)?;
        let glue_return_is_independent = glue_value.is_some();
        let value = if let Some(value) = glue_value {
            value
        } else {
            // Deep-eval 2026-08-09: enforce the string-return ownership
            // contract. Resolved returns may hand back `.rodata` literals, so
            // probe live heap registrations and heap-copy anything not owned.
            let value = self.generator.claim_resolved_string_return(value)?;
            // Heap-field records may contain String leaves pointing at .rodata
            // literals. Transform those leaves to owned heap copies so the
            // caller's scope-exit free is always safe.
            let return_type_id = self
                .program
                .callable(&callable.owner)
                .map(|c| c.signature.result.clone());
            // NOTE (0.40.1.3, A3): the Set/Map return ownership fail-closed is
            // enforced as a fatal top-level gate in `compile_checked` before
            // any emission, so it cannot be downgraded to the legacy emitter.
            self.ensure_returned_heap_strings_owned(value, result_type, return_type_id)?
        };
        // Deep-eval 2026-08-09 (demos/07 custom Res segv): same claim for
        // custom-enum-shaped returns ({i32 tag, i64 payload}): the payload
        // box of boxed variants must survive the callee's scope-exit free —
        // ownership transfers to the caller (mirrors the legacy
        // claim_returned_enum_box in Stmt::Return / emit_return).
        self.generator.claim_returned_enum_box(value, result_type)?;
        // Determine whether the return type transitively owns heap data.
        // If so, drain the heap scope (caller takes ownership) instead of
        // freeing — otherwise the returned pointer(s) dangle.
        //
        // Strings:         {ptr, i64}     — ptr is the string data pointer.
        // Lists:           {i64, ptr}     — ptr is the element data pointer.
        // Nested records:  any struct with at least one pointer field.
        //
        // The resolved emitter's `register_heap_slot` only tracks the data
        // pointer of list/string allocas.  Any struct field whose LLVM type
        // is a PointerType *may* reference such a tracked allocation.
        // 0.34.36 (audit §6.1): the old check was SHALLOW — it only looked at
        // the top-level fields, so a nested record (`Outer { inner: Inner }`
        // where `Inner` holds a string/list) whose direct fields were all
        // StructType fell through to free_heap_allocs and could free the
        // inner string's data out from under the caller. Recurse into
        // nested struct fields: any transitively-reachable pointer means the
        // return owns heap data.
        let return_owns_heap = ownership.requires_scope_drain();
        if return_owns_heap && !glue_return_is_independent {
            self.generator.drain_heap_scope();
        } else {
            self.generator.free_heap_allocs()?;
        }
        self.generator.build_return(Some(&value))
    }

    fn bind_parameters(
        &mut self,
        callable: &crate::core::ResolvedCallable,
        function: inkwell::values::FunctionValue<'ctx>,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<(), CompileError> {
        for (index, local_id) in callable.body.parameters.iter().enumerate() {
            let local = callable.body.locals.get(local_id).ok_or_else(|| {
                CompileError::Unsupported(format!(
                    "resolved parameter local '{}' is absent",
                    local_id.0 .0
                ))
            })?;
            let llvm_type = self.lower_type(&local.ty)?;
            let value = function.get_nth_param(index as u32).ok_or_else(|| {
                CompileError::LlvmError(format!(
                    "resolved parameter {index} is absent from '{}'",
                    callable.owner.0
                ))
            })?;
            // 0.34.43: borrowed (view/mutate) parameters arrive as a pointer
            // to the CALLER's storage and are used directly — no fresh
            // alloca — so callee stores are the caller's stores (legacy
            // func.rs bind shape; the reference ABI promised by ParamBorrow).
            let parameter = callable.signature.parameters.get(index);
            let borrowed = parameter.is_some_and(|p| {
                p.name != "self"
                    && matches!(
                        p.permission,
                        Some(crate::core::ir::Permission::View)
                            | Some(crate::core::ir::Permission::Mutate)
                    )
            });
            let storage = if borrowed {
                let BasicValueEnum::PointerValue(ptr) = value else {
                    return Err(CompileError::LlvmError(format!(
                        "resolved borrowed parameter {index} of '{}' is not a pointer",
                        callable.owner.0
                    )));
                };
                ptr
            } else {
                let storage = self
                    .generator
                    .build_alloca(llvm_type, &local.display_name)?;
                self.generator.build_store(storage, value)?;
                storage
            };
            frame
                .locals
                .insert(local_id.clone(), ResolvedVarEntry { storage, llvm_type });
            // 0.39.136 (L1): same var_type_names seeding as Bind — parameters
            // participate in typed dispatch (to_json/println/method fallback)
            // exactly like locals.
            let param_display = resolved_type_display_name(self.program, &local.ty);
            self.generator
                .var_type_names
                .insert(local.display_name.clone(), param_display);
        }
        Ok(())
    }

    // ── 0.34.41 第二档: contract guard emission (--verify-contracts) ──────
    //
    // Parity target: legacy func.rs/scope.rs. Requires asserts at entry after
    // parameter binding; ensures asserts at every return point with a `result`
    // binding; `old(x)` loads entry snapshots. Violation aborts with an E0808
    // message (same shape as legacy; condition text degrades to span
    // coordinates — the resolved IR has no surface renderer, and the VM's own
    // message prints the evaluated condition value, so cross-backend text
    // equality was never a contract).

    /// Emit requires guards and capture old() snapshots at function entry.
    fn emit_contract_prologue(
        &mut self,
        callable: &crate::core::ResolvedCallable,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<(), CompileError> {
        let owner = self.callable_symbol(&callable.owner)?.to_string();
        // 1. Snapshot every `old(...)` occurrence referenced by an ensures
        //    condition. Legacy only snapshots idents actually referenced via
        //    old() (CG-H10); here each Old occurrence snapshots its inner
        //    expression evaluated AT ENTRY (parameters only are bound so far).
        let mut old_nodes: Vec<&crate::core::ResolvedExpr> = Vec::new();
        for contract in &callable.contracts {
            if matches!(contract.kind, ContractKind::Ensures) {
                Self::collect_old_nodes(&contract.condition, &mut old_nodes);
            }
        }
        for old_node in old_nodes {
            let crate::core::ResolvedExprKind::Old(inner) = &old_node.kind else {
                continue;
            };
            let value = self.emit_expr(inner, frame)?;
            let llvm_type = self.lower_type(&old_node.ty)?;
            let storage = self.generator.build_alloca(llvm_type, "old_snapshot")?;
            self.generator.build_store(storage, value)?;
            frame.old_snapshots.insert(
                old_node.node_id.clone(),
                ResolvedVarEntry { storage, llvm_type },
            );
        }
        // 2. Requires guards in declaration order.
        for contract in &callable.contracts {
            if matches!(contract.kind, ContractKind::Requires) {
                self.emit_contract_assert(&contract.condition, "requires", &owner, frame)?;
            }
        }
        Ok(())
    }

    /// Emit ensures guards for one return point. `value` is the (already
    /// coerced) return value; `None` for a bare `return;` (unit stores zero,
    /// matching legacy).
    fn emit_ensures_checks(
        &mut self,
        callable: &crate::core::ResolvedCallable,
        value: Option<BasicValueEnum<'ctx>>,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<(), CompileError> {
        if !self.generator.verify_contracts {
            return Ok(());
        }
        let ensures: Vec<&crate::core::ResolvedContract> = callable
            .contracts
            .iter()
            .filter(|contract| matches!(contract.kind, ContractKind::Ensures))
            .collect();
        if ensures.is_empty() {
            return Ok(());
        }
        let owner = self.callable_symbol(&callable.owner)?.to_string();
        // Bind the desugared `result` pseudo-local (lower.rs ensures lowering
        // registers `{owner}/contract-result/local` with the signature result
        // type) to the actual return value.
        let result_type = self.lower_type(&callable.signature.result)?;
        let result_alloca = self.generator.build_alloca(result_type, "result")?;
        let stored = value.unwrap_or_else(|| result_type.const_zero());
        self.generator.build_store(result_alloca, stored)?;
        let result_local = ResolvedLocalId(NodeId(format!(
            "{}/contract-result/local",
            callable.owner.0
        )));
        let prior = frame.locals.insert(
            result_local.clone(),
            ResolvedVarEntry {
                storage: result_alloca,
                llvm_type: result_type,
            },
        );
        let outcome = (|| {
            for contract in &ensures {
                self.emit_contract_assert(&contract.condition, "ensures", &owner, frame)?;
            }
            Ok(())
        })();
        // Restore the frame: the pseudo-local must not leak past the check
        // (the body never binds it).
        match prior {
            Some(entry) => {
                frame.locals.insert(result_local, entry);
            }
            None => {
                frame.locals.remove(&result_local);
            }
        }
        outcome
    }

    /// Compile one contract condition as a runtime assert: cond → pass BB /
    /// fail BB (E0808 message + mimi_runtime_abort + unreachable).
    fn emit_contract_assert(
        &mut self,
        condition: &crate::core::ResolvedExpr,
        phase: &str,
        owner: &str,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<(), CompileError> {
        let cond = self.emit_expr(condition, frame)?;
        let cond_bool = match cond {
            BasicValueEnum::IntValue(int_value) => {
                if int_value.get_type().get_bit_width() == 1 {
                    int_value
                } else {
                    self.generator
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::NE,
                            int_value,
                            int_value.get_type().const_zero(),
                            "contract_cond_ne",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("contract cond: {e}")))?
                }
            }
            other => {
                return Err(CompileError::Unsupported(format!(
                    "contract condition is not boolean ({:?})",
                    other.get_type()
                )))
            }
        };
        let function = self
            .generator
            .builder
            .get_insert_block()
            .and_then(|block| block.get_parent())
            .ok_or_else(|| {
                CompileError::LlvmError("no current function for contract assert".into())
            })?;
        // NodeId-suffixed BB names: unique per condition occurrence, no
        // counter state (legacy needed contract_bb_counter; the resolved IR
        // carries stable identity).
        let suffix = condition.node_id.0.replace(['/', ':', ' '], "_");
        let pass_bb = self
            .generator
            .context
            .append_basic_block(function, &format!("contract_pass_{suffix}"));
        let fail_bb = self
            .generator
            .context
            .append_basic_block(function, &format!("contract_fail_{suffix}"));
        self.generator.build_cond_br(cond_bool, pass_bb, fail_bb)?;
        self.generator.builder.position_at_end(fail_bb);
        let message = self.contract_violation_message(condition, phase, owner);
        let msg_ptr = self
            .generator
            .builder
            .build_global_string_ptr(&message, "contract_msg")
            .map_err(|e| CompileError::LlvmError(format!("contract msg: {e}")))?;
        let abort_fn = self.generator.get_or_declare_abort_fn();
        self.generator.build_call(
            abort_fn,
            &[BasicMetadataValueEnum::PointerValue(
                msg_ptr.as_pointer_value(),
            )],
            "contract_abort",
        )?;
        // SAFETY: mimi_runtime_abort is noreturn; this block is unreachable.
        self.generator
            .builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("contract unreachable: {e}")))?;
        self.generator.builder.position_at_end(pass_bb);
        Ok(())
    }

    /// Build the embedded E0808 violation message. Shape mirrors legacy
    /// scope.rs build_contract_violation_message; the resolved IR has no
    /// surface renderer, so the condition text slot carries the span
    /// coordinates directly (still machine-first, still exact).
    fn contract_violation_message(
        &self,
        condition: &crate::core::ResolvedExpr,
        phase: &str,
        owner: &str,
    ) -> String {
        let pretty_owner = match owner.strip_suffix("__method") {
            Some(stripped) => stripped.replace("__", "::"),
            None => owner.to_string(),
        };
        let mut message = format!("[E0808] {phase} condition failed for '{pretty_owner}'");
        let span = condition.origin.user_span();
        if span.start_line > 0 {
            let label = self.generator.contract_location_label(span.source_id);
            let columns = if span.start_col > 0 {
                if span.end_line == span.start_line && span.end_col > span.start_col {
                    format!("{}-{}", span.start_col, span.end_col)
                } else {
                    format!("{}", span.start_col)
                }
            } else {
                String::new()
            };
            message.push_str(&format!(" @ {}:{}:{}", label, span.start_line, columns));
        }
        message
            .push_str(" | hint: rebuild without --verify-contracts to disable contract checking.");
        message
    }

    /// Collect every `Old` occurrence in a contract condition tree.
    fn collect_old_nodes<'a>(
        expression: &'a crate::core::ResolvedExpr,
        out: &mut Vec<&'a crate::core::ResolvedExpr>,
    ) {
        use crate::core::ResolvedExprKind as K;
        match &expression.kind {
            K::Old(_) => {
                out.push(expression);
            }
            K::Literal(_)
            | K::Constant(_)
            | K::Callable(_)
            | K::DefaultArgument { .. }
            | K::Load(_)
            | K::ComptimeValue(_)
            | K::TypeValue(_) => {}
            K::FString(parts) => {
                for part in parts {
                    if let crate::core::ir::ResolvedFStringPart::Interpolation(inner) = part {
                        Self::collect_old_nodes(inner, out);
                    }
                }
            }
            K::Project { value, .. }
            | K::TypeOf(value)
            | K::Spawn(value)
            | K::Await(value)
            | K::Try { value, .. } => Self::collect_old_nodes(value, out),
            K::Binary { left, right, .. } => {
                Self::collect_old_nodes(left, out);
                Self::collect_old_nodes(right, out);
            }
            K::Unary { operand, .. } => Self::collect_old_nodes(operand, out),
            K::Call(call) => {
                for argument in &call.arguments {
                    Self::collect_old_nodes(&argument.value, out);
                }
            }
            K::Tuple(items) | K::List(items) | K::Set(items) => {
                for item in items {
                    Self::collect_old_nodes(item, out);
                }
            }
            K::Map(entries) => {
                for (key, value) in entries {
                    Self::collect_old_nodes(key, out);
                    Self::collect_old_nodes(value, out);
                }
            }
            K::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                Self::collect_old_nodes(value, out);
                Self::collect_old_nodes(iterable, out);
                if let Some(guard) = guard {
                    Self::collect_old_nodes(guard, out);
                }
            }
            K::OptionalChain { .. } => {}
            K::Record { fields, .. } => {
                for field in fields {
                    Self::collect_old_nodes(&field.value, out);
                }
            }
            K::Block(block) => Self::collect_old_block(block, out),
            K::Scope { body, .. } | K::Comptime(body) | K::Quote(body) => {
                Self::collect_old_block(body, out)
            }
            K::If {
                condition,
                then_block,
                else_block,
            } => {
                Self::collect_old_nodes(condition, out);
                Self::collect_old_block(then_block, out);
                Self::collect_old_block(else_block, out);
            }
            K::Match { scrutinee, arms } => {
                Self::collect_old_nodes(scrutinee, out);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        Self::collect_old_nodes(guard, out);
                    }
                    Self::collect_old_nodes(&arm.body, out);
                }
            }
            K::Range { start, end } => {
                Self::collect_old_nodes(start, out);
                Self::collect_old_nodes(end, out);
            }
            K::Slice { target, start, end } => {
                Self::collect_old_nodes(target, out);
                if let Some(start) = start {
                    Self::collect_old_nodes(start, out);
                }
                if let Some(end) = end {
                    Self::collect_old_nodes(end, out);
                }
            }
            K::Cast { value, .. } => Self::collect_old_nodes(value, out),
            K::Lambda(_) => {
                // Lambda bodies bind their own parameters; an old() inside one
                // cannot reference this callable's entry snapshot. Skipping is
                // the fail-closed shape: such a condition would evaluate the
                // inner at check time instead of the snapshot — the checker
                // does not admit old() inside contract lambdas today.
            }
        }
    }

    /// Block companion of collect_old_nodes (statements + trailing result).
    fn collect_old_block<'a>(
        block: &'a crate::core::ResolvedBlock,
        out: &mut Vec<&'a crate::core::ResolvedExpr>,
    ) {
        for statement in &block.statements {
            match &statement.kind {
                crate::core::ResolvedStmtKind::Bind {
                    initializer: Some(value),
                    ..
                }
                | crate::core::ResolvedStmtKind::Assign { value, .. }
                | crate::core::ResolvedStmtKind::Expr(value)
                | crate::core::ResolvedStmtKind::Return {
                    value: Some(value), ..
                }
                | crate::core::ResolvedStmtKind::Break(Some(value)) => {
                    Self::collect_old_nodes(value, out)
                }
                crate::core::ResolvedStmtKind::Contract { condition, .. } => {
                    Self::collect_old_nodes(condition, out)
                }
                crate::core::ResolvedStmtKind::Math(expressions) => {
                    for expression in expressions {
                        Self::collect_old_nodes(expression, out);
                    }
                }
                crate::core::ResolvedStmtKind::IfLet {
                    initializer,
                    then_block,
                    else_block,
                    ..
                } => {
                    Self::collect_old_nodes(initializer, out);
                    Self::collect_old_block(then_block, out);
                    if let Some(else_block) = else_block {
                        Self::collect_old_block(else_block, out);
                    }
                }
                crate::core::ResolvedStmtKind::While { condition, body } => {
                    Self::collect_old_nodes(condition, out);
                    Self::collect_old_block(body, out);
                }
                crate::core::ResolvedStmtKind::WhileLet {
                    initializer, body, ..
                }
                | crate::core::ResolvedStmtKind::For {
                    iterable: initializer,
                    body,
                    ..
                } => {
                    Self::collect_old_nodes(initializer, out);
                    Self::collect_old_block(body, out);
                }
                crate::core::ResolvedStmtKind::Loop(body)
                | crate::core::ResolvedStmtKind::Scope { body, .. } => {
                    Self::collect_old_block(body, out)
                }
                crate::core::ResolvedStmtKind::Pinned { value, body, .. } => {
                    Self::collect_old_nodes(value, out);
                    Self::collect_old_block(body, out);
                }
                _ => {}
            }
        }
        if let Some(result) = &block.result {
            Self::collect_old_nodes(result, out);
        }
    }

    /// 0.36.15 L1: emit registered guard blocks (defer / on-failure
    /// compensation) in LIFO order, then drop the stack. `pending` is taken
    /// out of the emitter so the &mut self emit below does not alias a field.
    fn emit_guard_stack(
        &mut self,
        body: &ResolvedBody,
        pending: Vec<ResolvedBlock>,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<(), CompileError> {
        for block in pending.iter().rev() {
            self.emit_block(body, block, frame)?;
        }
        Ok(())
    }

    fn emit_block(
        &mut self,
        body: &ResolvedBody,
        block: &ResolvedBlock,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
        let mut last = None;
        for statement in &block.statements {
            last = self.emit_statement(body, statement, frame)?;
            if self.current_block_terminated() {
                return Ok(last);
            }
        }
        if let Some(result) = &block.result {
            last = Some(self.emit_expr(result, frame)?);
        }
        Ok(last)
    }

    /// F-004 (0.40.1.8): does a resolved `Lambda` capture any heap-typed local?
    /// Used to fail-closed returning such a closure on the native backend — its
    /// captured data array is freed when the enclosing scope exits, leaving the
    /// escaped closure with a dangling pointer (use-after-free).
    /// F-004 (0.40.1.8): does the returned `lambda` capture a heap-collection-typed
    /// free variable (List/Set/Map, possibly nested inside Option/Result/Tuple)?
    /// Used to fail-closed returning such a closure on the native backend — the
    /// captured data array is freed when the enclosing scope exits, leaving the
    /// escaped closure with a dangling pointer. The check uses the precise Mimi
    /// type of each captured local (not the LLVM layout, which cannot distinguish a
    /// `List` handle from a scalar local's alloca / a captured closure struct under
    /// LLVM 18 opaque pointers).
    fn lambda_captures_heap_resolved(
        &self,
        lambda: &crate::core::ir::ResolvedLambda,
        body: &crate::core::ir::ResolvedBody,
    ) -> Result<bool, CompileError> {
        for cap in &lambda.captures {
            if let Some(local) = body.locals.get(cap) {
                if self.resolved_type_owns_heap_collection(&local.ty) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// F-004 (0.40.1.8): precise Mimi-type test for whether a type transitively
    /// owns a heap collection (List/Set/Map). Recurses through Option/Result/Tuple/
    /// Newtype/Array/Slice/CBuffer so `Option<List<…>>`, `List<List<…>>`, tuples of
    /// collections, etc. are all caught; scalars, strings, closures and records are
    /// not (records-with-heap are the separate F-005 / E0700 boundary).
    fn resolved_type_owns_heap_collection(&self, id: &crate::core::ir::ResolvedTypeId) -> bool {
        crate::codegen::abi::ownership::classify_resolved(self.program, id)
            .contains_heap_collection()
    }

    fn emit_statement(
        &mut self,
        body: &ResolvedBody,
        statement: &crate::core::ResolvedStmt,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
        match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer: Some(initializer),
            } => {
                // 0.35.11-fix (O0 double free): list literal bound directly
                // to a simple local — construct into the local's storage so
                // the registered heap owner is the slot push/pop reallocs
                // update. The generic path below would construct a temp,
                // register the temp, then value-copy into the local; after a
                // realloc the temp slot holds the stale pointer and the
                // scope-exit free double-frees it (see emit_list_literal).
                if let (
                    crate::core::ResolvedExprKind::List(elements),
                    crate::core::ResolvedPatternKind::Binding {
                        local,
                        by_reference: None,
                    },
                ) = (&initializer.kind, &pattern.kind)
                {
                    let metadata = body.locals.get(local).ok_or_else(|| {
                        CompileError::Unsupported(format!(
                            "resolved binding local '{}' is absent",
                            local.0 .0
                        ))
                    })?;
                    let llvm_type = self.lower_type(&metadata.ty)?;
                    if matches!(llvm_type, BasicTypeEnum::StructType(_)) {
                        let storage = self
                            .generator
                            .build_alloca(llvm_type, &metadata.display_name)?;
                        self.emit_list_literal(elements, frame, Some(storage))?;
                        frame
                            .locals
                            .insert(local.clone(), ResolvedVarEntry { storage, llvm_type });
                        return Ok(None);
                    }
                }
                let value = self.emit_expr(initializer, frame)?;
                self.bind_pattern(body, pattern, value, frame)?;
                // 0.37.x: transfer string-temp ownership into the local slot for
                // simple bindings too. Without this, `let ch = str_char_at(...)`
                // inside a loop left the heap allocation in the per-iteration
                // scope and `free_heap_allocs` freed it before the next
                // statement could safely use the local.
                let is_string_temp_bind = matches!(
                    initializer.kind,
                    ResolvedExprKind::Binary {
                        op: ResolvedBinaryOp::Add,
                        ..
                    } | ResolvedExprKind::FString(_)
                        | ResolvedExprKind::Call(_)
                );
                if is_string_temp_bind {
                    if let ResolvedPatternKind::Binding {
                        local,
                        by_reference: None,
                    } = &pattern.kind
                    {
                        if let Some(entry) = frame.locals.get(local) {
                            if let BasicTypeEnum::StructType(st) = entry.llvm_type {
                                let fields = st.get_field_types();
                                let is_plain_string = fields.len() == 2
                                    && matches!(fields[0], BasicTypeEnum::PointerType(_))
                                    && matches!(fields[1], BasicTypeEnum::IntType(_));
                                if is_plain_string && self.generator.pop_last_heap_ptr().is_some() {
                                    self.generator.register_heap_slot(entry.storage, st, 0);
                                }
                            }
                        }
                    }
                }
                // 0.39.x matrix sweep (LOOP-REBIND-HEAP-001): a Call result of
                // LIST shape bound inside a loop must also leave the
                // per-iteration heap scope — otherwise each iteration freed the
                // buffer the variable still referenced (flatten's
                // `result = concat(result, xs)`; shuffle's
                // `sh_rest = random_remove_ith(...)`). Root-scope registration
                // gives the value the binding's lifetime.
                if matches!(initializer.kind, ResolvedExprKind::Call(_)) {
                    if let ResolvedPatternKind::Binding {
                        local,
                        by_reference: None,
                    } = &pattern.kind
                    {
                        if let Some(entry) = frame.locals.get(local) {
                            if let BasicTypeEnum::StructType(st) = entry.llvm_type {
                                let fields = st.get_field_types();
                                let is_list_struct = fields.len() == 2
                                    && matches!(
                                        fields[0],
                                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                                    )
                                    && matches!(fields[1], BasicTypeEnum::PointerType(_));
                                if is_list_struct && self.generator.pop_last_heap_ptr().is_some() {
                                    // Transfer ownership to the variable's own
                                    // slot — but only when it does not already
                                    // own one (first binding registers it via
                                    // emit_list_literal); a second entry for the
                                    // same storage would free the final buffer
                                    // twice at function exit.
                                    if !self.generator.has_heap_slot(entry.storage) {
                                        self.generator.register_heap_slot_root(
                                            entry.storage,
                                            st,
                                            1,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(None)
            }
            ResolvedStmtKind::Assign {
                target,
                value,
                conversion,
            } => {
                let rhs_expr = value;
                let value = self.emit_expr(value, frame)?;
                // SD-7 (0.34.34): narrowing assign into an i32 variable traps
                // out of range (VM assign-guard parity). Range-check BEFORE
                // apply_conversion truncates; explicit casts keep wrap.
                if matches!(
                    conversion.kind,
                    crate::core::CheckedConversionKind::NumericNarrowChecked
                ) {
                    let conv_target = self.lower_type(&conversion.to)?;
                    if let (BasicValueEnum::IntValue(iv), BasicTypeEnum::IntType(it)) =
                        (value, conv_target)
                    {
                        if it.get_bit_width() == 32 && iv.get_type().get_bit_width() > 32 {
                            self.generator.emit_i32_range_guard(iv, "assign")?;
                        }
                    }
                }
                let value = self.apply_conversion(value, conversion)?;
                // Assignment targets an index WRITE (negative indices trap,
                // VM ListSet parity) — see emit_checked_list_index (H-15).
                let target_is_root_local = target.projections.is_empty();
                // Capture the place's projections before `root_place` consumes the
                // `ResolvedPlace` (it returns a `ResolvedVarEntry` without them) —
                // the index-write boxing at the store site needs the final
                // projection to detect a direct List-element write (F-016).
                let target_projections = target.projections.clone();
                let target = self.root_place(frame, target, false)?;
                // 0.37.x (dogfood: build string by `w = w + ch` in loops):
                // resolved string-temp assignments must transfer ownership out
                // of the per-iteration heap scope. The old code left the concat
                // result in `heap_allocs`, so `free_heap_allocs` at the end of
                // each loop body freed the string that was just stored into the
                // variable. Register the variable slot in the function root
                // scope instead, mirroring the legacy emitter's
                // `compile_assign_stmt` string-temp transfer.
                let is_string_temp_assign = matches!(
                    rhs_expr.kind,
                    ResolvedExprKind::Binary {
                        op: ResolvedBinaryOp::Add,
                        ..
                    } | ResolvedExprKind::FString(_)
                        | ResolvedExprKind::Call(_)
                );
                if target_is_root_local {
                    if let BasicTypeEnum::StructType(st) = target.llvm_type {
                        let fields = st.get_field_types();
                        let is_plain_string = fields.len() == 2
                            && matches!(fields[0], BasicTypeEnum::PointerType(_))
                            && matches!(fields[1], BasicTypeEnum::IntType(_));
                        if is_plain_string {
                            if is_string_temp_assign {
                                // Only claim when the expression really registered a
                                // heap allocation. User string-returning calls may
                                // already have claimed their own result; literals
                                // and other non-heap strings must not be popped.
                                if self.generator.pop_last_heap_ptr().is_some() {
                                    self.generator
                                        .register_heap_slot_root(target.storage, st, 0);
                                }
                            } else if matches!(rhs_expr.kind, ResolvedExprKind::Load(_)) {
                                // A string variable assigned from another string
                                // variable must not alias a per-iteration heap slot
                                // (e.g. `w = ch` inside a loop, where `ch` is freed
                                // at the end of the iteration). Heap-copy the data
                                // so the target owns an independent buffer.
                                let value = self.generator.heap_copy_string_value(value)?;
                                self.generator
                                    .register_heap_slot_root(target.storage, st, 0);
                                let value = self.coerce_to(value, target.llvm_type)?;
                                self.generator.build_store(target.storage, value)?;
                                return Ok(None);
                            }
                        }
                        // 0.39.x matrix sweep (LOOP-REBIND-HEAP-001): LIST-shaped
                        // rebindings (`sh_rest = random_remove_ith(sh_rest, i)`,
                        // `result = concat(result, xs)`) have the same hazard as
                        // string temps: the Call's returned buffer registered in
                        // the per-iteration scope was freed at iteration end while
                        // the variable still referenced it. Transfer that
                        // registration to the function root scope so the buffer
                        // lives as long as the binding.
                        if is_string_temp_assign {
                            let fields = st.get_field_types();
                            let is_list_struct = fields.len() == 2
                                && matches!(
                                    fields[0],
                                    BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                                )
                                && matches!(fields[1], BasicTypeEnum::PointerType(_));
                            if is_list_struct && self.generator.pop_last_heap_ptr().is_some() {
                                // Same reasoning as the Bind arm: transfer to
                                // the existing owner slot when present, root-
                                // register otherwise.
                                if !self.generator.has_heap_slot(target.storage) {
                                    self.generator
                                        .register_heap_slot_root(target.storage, st, 1);
                                }
                            }
                        }
                    }
                }
                // 0.40.1.20 (F-016): writing into a List element slot stores an
                // `i64` handle (the data array is `i64[]`). Scalars widen via
                // `coerce_to_i64`; string elements must be boxed through
                // `mimi_str_box` (`coerce_string_to_i64`) — the exact same
                // primitives the List-literal emitter uses (line 2358), so the
                // written slot matches what readers/`contains`/zip expect. The
                // resolved Index-projection place already returns the element GEP
                // for a final-index write; here we convert the RHS to the stored
                // handle form. Element string-ness comes from the resolved Index
                // projection type (mirrors the literal path) — no new heuristic.
                let is_elem_write = !target_is_root_local
                    && matches!(
                        target_projections.last(),
                        Some(crate::core::ir::ResolvedProjection::Index { .. })
                    );
                let value = if is_elem_write {
                    let elem_is_string = target_projections
                        .iter()
                        .rev()
                        .find_map(|p| match p {
                            crate::core::ir::ResolvedProjection::Index { ty, .. } => {
                                Some(resolved_type_display_name(self.program, ty) == "string")
                            }
                            _ => None,
                        })
                        .unwrap_or(false);
                    if elem_is_string {
                        BasicValueEnum::IntValue(self.coerce_string_to_i64(value)?)
                    } else {
                        BasicValueEnum::IntValue(self.coerce_to_i64(value)?)
                    }
                } else {
                    self.coerce_to(value, target.llvm_type)?
                };
                self.generator.build_store(target.storage, value)?;
                Ok(None)
            }
            ResolvedStmtKind::Return { value, conversion } => {
                // F-004 (0.40.1.8): fail-closed returning a closure that captures a
                // heap value on the native backend — the captured data array is freed
                // when the enclosing scope exits, leaving the escaped closure with a
                // dangling pointer (use-after-free). Mirror the E0723 ownership gate
                // instead of silently corrupting memory (mimi run / VM backend is unaffected).
                if let Some(ret_expr) = value.as_ref() {
                    if let ResolvedExprKind::Lambda(lambda) = &ret_expr.kind {
                        if self.lambda_captures_heap_resolved(lambda, body)? {
                            return Err(CompileError::UnsupportedReturn(
                                "returning a closure that captures a heap collection (List/Set/Map, or a \
                                 container such as Option/Result/Tuple holding one) from a native (LLVM) \
                                 function is not yet supported: the captured data array is freed when the \
                                 enclosing scope exits, leaving the returned closure with a dangling pointer \
                                 (use-after-free). Use `mimi run` (VM backend), or restructure so the closure \
                                 does not escape with captured heap data. Tracked as 0.1.10 A2 \
                                 ownership-glue work (E0723)."
                                    .to_string(),
                            ));
                        }
                    }
                }
                let value = value
                    .as_ref()
                    .map(|value| self.emit_expr(value, frame))
                    .transpose()?;
                let value = match (value, conversion) {
                    (Some(value), Some(conversion)) => {
                        Some(self.apply_conversion(value, conversion)?)
                    }
                    (value, None) => value,
                    (None, Some(_)) => {
                        return Err(CompileError::Unsupported(format!(
                            "resolved return '{}' has a conversion without a value",
                            statement.node_id.0
                        )))
                    }
                };
                let function = self
                    .generator
                    .builder
                    .get_insert_block()
                    .and_then(|block| block.get_parent())
                    .ok_or_else(|| CompileError::LlvmError("return outside function".into()))?;
                let result_type = function.get_type().get_return_type().ok_or_else(|| {
                    CompileError::LlvmError("resolved function has void return".into())
                })?;
                let value = value
                    .map(|value| self.coerce_to(value, result_type))
                    .transpose()?;
                // 0.35.23 parity: a bare `return` in a unit function must
                // `ret i64 0` — the unit signature is i64, so `ret void`
                // would be invalid IR for the resolved emitter too.
                let value = match value {
                    Some(value) => Some(value),
                    None => Some(self.generator.zero_value_for(result_type)),
                };
                // 0.36.15 L1: deferred blocks run LIFO before every return
                // (legacy: pop_defer_scope before emit_return); pending
                // on-failure compensations are discarded — normal exit.
                let pending_defers = std::mem::take(&mut self.defer_scopes);
                self.emit_guard_stack(body, pending_defers, frame)?;
                self.comp_scopes.clear();
                // 0.34.41 第二档: ensures guards on early return paths too
                // (legacy emit_return is the single funnel there; here every
                // Return statement funnels its own check before the ret).
                self.emit_ensures_checks(
                    self.program.callable(&frame.owner).ok_or_else(|| {
                        CompileError::Unsupported("callable absent for return ensures".into())
                    })?,
                    value,
                    frame,
                )?;
                let return_type_id = self
                    .program
                    .callable(&frame.owner)
                    .map(|c| c.signature.result.clone());
                let ownership = return_type_id
                    .as_ref()
                    .map(|type_id| {
                        crate::codegen::abi::ownership::classify_resolved(self.program, type_id)
                    })
                    .unwrap_or(crate::codegen::abi::ownership::OwnershipClass::Unknown);
                // A2 (0.40.3.2): early and implicit returns share the same
                // adoption contract. Clone an admitted StringBox/product once
                // after semantic guards, then let the callee flush every
                // original allocation. Unsupported shapes retain the proven
                // probe/recursive-claim path without widening the matrix.
                let (value, glue_return_is_independent) = if let Some(value) = value {
                    if let Some(cloned) = self
                        .generator
                        .clone_return_with_derived_glue(&ownership, value)?
                    {
                        (Some(cloned), true)
                    } else {
                        let value = self.generator.claim_resolved_string_return(value)?;
                        // Heap-field records may contain String leaves pointing
                        // at .rodata literals. Make them owned before claiming
                        // pointer leaves for the deterministic drop.
                        // NOTE (0.40.1.3, A3): the Set/Map return ownership
                        // fail-closed is enforced before native emission.
                        (
                            Some(self.ensure_returned_heap_strings_owned(
                                value,
                                result_type,
                                return_type_id.clone(),
                            )?),
                            false,
                        )
                    }
                } else {
                    (None, false)
                };
                // Claim the heap pointers embedded in the returned value so
                // the deterministic drop below frees only non-escaping local
                // allocations. This prevents freeing a list/string data
                // buffer that the caller is about to read.
                if let (false, Some(value)) = (glue_return_is_independent, value) {
                    self.claim_returned_heap_pointers(value, result_type, return_type_id)?;
                }
                // Deterministic drop on every early return: emit path-specific
                // frees for all function-local heap scopes before the ret.
                // The scopes are not popped here; end_function_heap_scope
                // balances bookkeeping after the function body finishes.
                //
                // 0.39.x L1 (E0722 family): the returned value's heap pointers
                // were claimed above via `claim_returned_heap_pointers` — this
                // now includes generic `List<T>` (see `claim_returned_generic_list`
                // + `emit_generic_list_contains`), so its data-array buffer is
                // excluded from the flush and ownership transfers to the caller,
                // matching the bytecode VM. `List<string>` / `List<List<string>>`
                // are claimed by their own dedicated paths. The flush below frees
                // every other function-local allocation.
                self.generator.flush_heap_scopes_to_boundary()?;
                self.generator.build_return(
                    value
                        .as_ref()
                        .map(|value| value as &dyn inkwell::values::BasicValue<'ctx>),
                )?;
                Ok(value)
            }
            ResolvedStmtKind::Expr(expression) => {
                // 0.35.23 deep-eval: statement-position if runs in statement
                // mode — branch values are discarded instead of coerced into
                // the unit result (mimi-log collect_latencies: `if e.latency
                // > 0.0 { push(lats, e.latency) }` — push returns a list
                // pointer, the if's type is unit(i64), ptr→i64 coerce failed
                // and the whole function fell back to legacy E0700).
                if let ResolvedExprKind::If {
                    condition,
                    then_block,
                    else_block,
                } = &expression.kind
                {
                    let value =
                        self.emit_if(expression, condition, then_block, else_block, frame, true)?;
                    return Ok(Some(value));
                }
                self.emit_expr(expression, frame).map(Some)
            }
            ResolvedStmtKind::Bind {
                pattern,
                initializer: None,
            } => {
                self.bind_pattern_uninitialized(body, pattern, frame)?;
                Ok(None)
            }
            ResolvedStmtKind::While {
                condition,
                body: loop_body,
            } => {
                self.emit_while(body, condition, loop_body, frame)?;
                Ok(None)
            }
            ResolvedStmtKind::For {
                pattern,
                iterable,
                body: loop_body,
            } => {
                self.emit_for(body, pattern, iterable, loop_body, frame)?;
                Ok(None)
            }
            ResolvedStmtKind::Break(_) => {
                let loop_ctx = self
                    .loop_stack
                    .last()
                    .ok_or_else(|| CompileError::Unsupported("break outside loop".into()))?;
                // Deterministic drop on break: free the current loop body's
                // heap allocations before leaving the iteration.
                self.generator.emit_frees_for_top_scope()?;
                self.generator.build_br(loop_ctx.exit)?;
                Ok(None)
            }
            ResolvedStmtKind::Continue => {
                let loop_ctx = self
                    .loop_stack
                    .last()
                    .ok_or_else(|| CompileError::Unsupported("continue outside loop".into()))?;
                // Deterministic drop on continue: free the current loop body's
                // heap allocations before jumping back to the loop header.
                self.generator.emit_frees_for_top_scope()?;
                self.generator.build_br(loop_ctx.header)?;
                Ok(None)
            }
            ResolvedStmtKind::Scope {
                body: scope_block,
                kind: scope_kind,
            } => {
                // H-11 (2026-08-06): `ResolvedScopeKind::IeeeFloat` must mirror
                // legacy func.rs/block.rs and bump `generator.ieee_depth` so
                // check_float_finite suspends the finiteness trap inside. The
                // old `..` unconditionally inline-emitted the block with
                // ieee_depth stuck at 0, so `(-1.0) ** 0.5` (NaN) trapped
                // E0813 inside `ieee_float { }` on the resolved path while the
                // legacy path honored it — an L1/codegen divergence.
                if matches!(scope_kind, crate::core::ir::ResolvedScopeKind::IeeeFloat) {
                    self.generator.ieee_depth += 1;
                    let r = self.emit_block(body, scope_block, frame);
                    self.generator.ieee_depth -= 1;
                    r?;
                } else {
                    // 0.36.15 L1: `defer` / `on failure` are scope GUARDS, not
                    // inline blocks — the resolved emitter previously inlined
                    // their bodies at the statement position, so `defer` ran
                    // before the body and `on failure` fired on normal exits
                    // (CLI `mimi build`/compile_checked path; the dual harness
                    // uses legacy compile_file and stayed green). Record the
                    // block here and emit it at function exits: defer LIFO on
                    // every exit, on-failure only before fault propagation
                    // (the `exit(...)` call), discarded on normal return —
                    // mirroring legacy func.rs/block.rs register_defer /
                    // register_comp.
                    match scope_kind {
                        crate::core::ir::ResolvedScopeKind::Defer => {
                            self.defer_scopes.push(scope_block.clone());
                        }
                        crate::core::ir::ResolvedScopeKind::FailureGuard => {
                            self.comp_scopes.push(scope_block.clone());
                        }
                        _ => {
                            // Lexical / unsafe / arena / allocator / parallel
                            // scopes: emit the inner block inline.
                            self.emit_block(body, scope_block, frame)?;
                        }
                    }
                }
                Ok(None)
            }
            ResolvedStmtKind::Loop(loop_body) => {
                self.emit_loop(body, loop_body, frame)?;
                Ok(None)
            }
            // Specification-level statements: no native code output.
            ResolvedStmtKind::Drop(_) => Ok(None),
            ResolvedStmtKind::Contract { .. } => Ok(None),
            ResolvedStmtKind::Math(_) => Ok(None),
            // NestedCallable: no-op (nested function compiled separately).
            ResolvedStmtKind::NestedCallable(_) => Ok(None),
            other => Err(CompileError::Unsupported(format!(
                "resolved statement {other:?} escaped resolved native eligibility for '{}'",
                frame.owner.0
            ))),
        }
    }

    /// 0.35.11-fix (O0 double free): emit a list literal, either into a fresh
    /// construction temp (`target: None`, inline uses — the temp keeps
    /// ownership and is freed at scope exit) or DIRECTLY into an existing
    /// storage slot (`target: Some`, used by the Bind fast path below).
    ///
    /// Why the direct mode exists: `let mut ys = [1, 2, 3]; push(ys, 4)`
    /// used to construct the literal into a temp, register the temp as the
    /// buffer owner, then copy the struct VALUE into the local. push/pop
    /// realloc through the LOCAL, leaving the registered temp slot holding
    /// the stale pre-realloc pointer — freed again at scope exit (realloc
    /// already released it when it moved the chunk) → tcache double free.
    /// O1 only survived because SROA merged the two allocas. Constructing
    /// into the local makes the registered owner the very slot the
    /// mutators update.
    fn emit_list_literal(
        &mut self,
        elements: &[crate::core::ResolvedExpr],
        frame: &mut ResolvedFrame<'ctx>,
        target: Option<inkwell::values::PointerValue<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let count = elements.len() as u64;
        let i64_ty = self.generator.context.i64_type();
        let len_val = i64_ty.const_int(count, false);
        // Allocate data buffer: count * 8 bytes.
        let sizeof_i64 = i64_ty.const_int(8, false);
        let alloc_size = self
            .generator
            .builder
            .build_int_mul(len_val, sizeof_i64, "list_alloc_size")
            .map_err(|e| CompileError::LlvmError(format!("list alloc mul: {e}")))?;
        let data_ptr = self.generator.malloc_or_abort(alloc_size, "list_malloc")?;
        // Store each element as i64. Since 0.38.26 `List<string>`
        // elements are fat `MimiStr` boxes (`{ptr,len}`), not raw C-string
        // pointers, so resolved list literals must box strings to keep
        // readers/contains/zip/enumerate consistent with the runtime.
        for (i, element) in elements.iter().enumerate() {
            let value = self.emit_expr(element, frame)?;
            let iv = if resolved_type_display_name(self.program, &element.ty) == "string" {
                self.coerce_string_to_i64(value)?
            } else {
                self.coerce_to_i64(value)?
            };
            let idx = i64_ty.const_int(i as u64, false);
            let elem_ptr =
                self.generator
                    .build_in_bounds_gep(i64_ty, data_ptr, &[idx], "list_elem")?;
            self.generator.build_store(elem_ptr, iv)?;
        }
        match target {
            None => self.generator.build_list_struct(len_val, data_ptr),
            Some(storage) => {
                let list_ty = self.generator.list_struct_type();
                let len_gep = self
                    .generator
                    .builder
                    .build_struct_gep(list_ty, storage, 0, "list_len")
                    .map_err(|e| CompileError::LlvmError(format!("list len gep: {e}")))?;
                self.generator.build_store(len_gep, len_val)?;
                let data_gep = self
                    .generator
                    .builder
                    .build_struct_gep(list_ty, storage, 1, "list_data")
                    .map_err(|e| CompileError::LlvmError(format!("list data gep: {e}")))?;
                let data_void_ptr = self.generator.build_bit_cast(
                    data_ptr.into(),
                    self.generator
                        .context
                        .ptr_type(inkwell::AddressSpace::default())
                        .into(),
                    "data_void",
                )?;
                self.generator.build_store(data_gep, data_void_ptr)?;
                // The LOCAL is the buffer owner: push/pop reallocs write this
                // slot, so the scope-exit free must read this slot.
                self.generator.register_heap_slot(storage, list_ty, 1);
                Ok(storage.into())
            }
        }
    }

    fn bind_pattern(
        &mut self,
        body: &ResolvedBody,
        pattern: &ResolvedPattern,
        value: BasicValueEnum<'ctx>,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<(), CompileError> {
        match &pattern.kind {
            ResolvedPatternKind::Wildcard => Ok(()),
            ResolvedPatternKind::Binding { local, .. } => {
                let metadata = body.locals.get(local).ok_or_else(|| {
                    CompileError::Unsupported(format!(
                        "resolved binding local '{}' is absent",
                        local.0 .0
                    ))
                })?;
                let llvm_type = self.lower_type(&metadata.ty)?;
                // For i64 (ptrtoint) → struct conversion, inttoptr + load.
                // This is used when the error tuple in Result<E> stores the
                // payload as a ptrtoint-encoded heap pointer (e.g., source
                // state or string in the Err tuple).
                let value = if matches!(value, BasicValueEnum::IntValue(_))
                    && matches!(llvm_type, BasicTypeEnum::StructType(_))
                {
                    let ptr = self
                        .generator
                        .builder
                        .build_int_to_ptr(
                            value.into_int_value(),
                            self.generator
                                .context
                                .ptr_type(inkwell::AddressSpace::default()),
                            "bind_struct_ptr",
                        )
                        .map_err(|e| {
                            CompileError::LlvmError(format!("inttoptr for struct bind: {e}"))
                        })?;
                    self.generator
                        .builder
                        .build_load(llvm_type, ptr, "bind_struct_loaded")
                        .map_err(|e| {
                            CompileError::LlvmError(format!("load for struct bind: {e}"))
                        })?
                } else {
                    // SD-7 (0.34.34): narrowing bind into an i32 slot
                    // range-checks before the silent truncate — mirrors the
                    // VM CheckI32 let-guard. Cast narrows via
                    // ResolvedExprKind::Cast and keeps wrap semantics.
                    if let (BasicValueEnum::IntValue(iv), BasicTypeEnum::IntType(it)) =
                        (value, llvm_type)
                    {
                        if it.get_bit_width() == 32 && iv.get_type().get_bit_width() > 32 {
                            self.generator.emit_i32_range_guard(iv, "let-bind")?;
                        }
                    }
                    self.coerce_to(value, llvm_type)?
                };
                let storage = self
                    .generator
                    .build_alloca(llvm_type, &metadata.display_name)?;
                self.generator.build_store(storage, value)?;
                frame
                    .locals
                    .insert(local.clone(), ResolvedVarEntry { storage, llvm_type });
                // 0.39.136 (L1): register the checker-canonical type name for
                // this binding so legacy-shared typed-dispatch helpers
                // (infer_object_type → to_json/println/map-set dispatch) see
                // the same var_type_names entries the legacy emitter seeds.
                // The resolved pipeline previously left it empty, so opaque
                // i64 handles (builtin maps/sets) mis-dispatched — e.g.
                // `to_json(m)` on a map printed the raw handle integer
                // natively while the VM serialized real JSON. Display names
                // use the same convention as resolved_type_display_name
                // elsewhere in this emitter ("Map", "Record", "List<string>",
                // user record names…).
                let type_display = resolved_type_display_name(self.program, &metadata.ty);
                self.generator
                    .var_type_names
                    .insert(metadata.display_name.clone(), type_display);
                Ok(())
            }
            ResolvedPatternKind::Constructor { variant, fields } => {
                // 0.37.3: newtype constructor bindings (`let UserId(v) = u`)
                // bind the scrutinee directly — same semantics as newtype
                // constructor patterns in match arms.
                if self.is_newtype_variant(variant) {
                    for (_, sub_pattern) in fields {
                        self.bind_pattern(body, sub_pattern, value, frame)?;
                    }
                    return Ok(());
                }
                // 0.40.x (L1): flow-state constructor sub-patterns
                // (`state:F::B`). These are plain flow-state records with no
                // discriminant tag, e.g. the inner `B` of an `Ok(B { n })`
                // Result arm for a `fails` transition. Destructure the record
                // by field name/index, recursively binding each sub-pattern —
                // mirroring bind_flow_arm_variables.
                if variant.0.starts_with("state:") {
                    let sv = match value {
                        BasicValueEnum::StructValue(sv) => sv,
                        BasicValueEnum::PointerValue(pv) => {
                            let sty = self.lower_type(&pattern.ty)?;
                            self.generator
                                .build_load(sty, pv, "flow_state_bind_scrutinee")?
                                .into_struct_value()
                        }
                        _ => {
                            return Err(CompileError::Unsupported(
                                "flow-state constructor pattern bound to non-struct value".into(),
                            ))
                        }
                    };
                    for (field_id, sub_pattern) in fields {
                        let field_name = self.lookup_field_name(field_id)?;
                        let field_idx = self.lookup_field_index(field_id, &field_name)?;
                        let field_val = self
                            .generator
                            .builder
                            .build_extract_value(sv, field_idx, "flow_state_bind_field")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("flow state field extract: {e}"))
                            })?;
                        self.bind_pattern(body, sub_pattern, field_val, frame)?;
                    }
                    return Ok(());
                }
                return Err(CompileError::Unsupported(format!(
                    "resolved pattern '{}' escaped resolved native eligibility",
                    pattern.node_id.0
                )));
            }
            ResolvedPatternKind::Tuple(sub_patterns) => {
                let BasicValueEnum::StructValue(struct_val) = value else {
                    return Err(CompileError::Unsupported(
                        "tuple pattern bound to non-struct value".into(),
                    ));
                };
                // Store to alloca + GEP + load instead of extract_value.
                // extract_value misorders fields on struct-returning function
                // call results under LLVM target lowering (cross-emitter ABI).
                // GEP+load is consistent across all code paths.
                let sty = struct_val.get_type();
                let alloca = self.generator.build_alloca(sty, "tuple_pat")?;
                self.generator.build_store(alloca, struct_val)?;
                for (index, sub_pattern) in sub_patterns.iter().enumerate() {
                    let field_ptr = self
                        .generator
                        .builder
                        .build_struct_gep(sty, alloca, index as u32, "pat_gep")
                        .map_err(|e| CompileError::LlvmError(format!("pat gep: {e}")))?;
                    let field_ty = sty.get_field_type_at_index(index as u32).ok_or_else(|| {
                        CompileError::LlvmError(format!(
                            "tuple field type {index} absent in pattern"
                        ))
                    })?;
                    let field = self
                        .generator
                        .build_load(field_ty, field_ptr, "pat_field")
                        .map_err(|e| CompileError::LlvmError(format!("pat load: {e}")))?;
                    self.bind_pattern(body, sub_pattern, field, frame)?;
                }
                Ok(())
            }
            _ => Err(CompileError::Unsupported(format!(
                "resolved pattern '{}' escaped resolved native eligibility",
                pattern.node_id.0
            ))),
        }
    }

    fn emit_expr(
        &mut self,
        expression: &ResolvedExpr,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match &expression.kind {
            ResolvedExprKind::Literal(literal) => self.emit_literal(&expression.ty, literal),
            ResolvedExprKind::Constant(node_id) => {
                // Builtin constants (None, true, false) are not in the
                // program's constants catalog. Handle them directly.
                if node_id.0 == "builtin:value:None" {
                    // 0.34.35: build None in the *resolved* layout ({bool,
                    // payload}) from the expression type. Previously routed to
                    // legacy compile_constructor, which hard-codes the narrow
                    // {i1,i64} — an if/else with Some(string) (wide {i1,{ptr,i64}})
                    // vs None then failed the branch coerce ({i1,i64} → {i1,{ptr,i64}})
                    // and the whole callable fell back to legacy, where the phi
                    // merge crashed LLVM's CVP pass (v1: SIGSEGV in visitPHINode)
                    // or mis-dispatched the print arg (v2/v3).
                    return self.emit_resolved_optional_ctor("None", vec![], &expression.ty);
                }
                // 0.32.12: Enum unit variants (e.g., `Green` in
                // `type Color { Red | Green | Blue }`) are constants
                // whose NodeId is a variant declaration. Construct the
                // enum struct {i32 tag, i64 0}.
                if let Some(variant) = self.program.resolved_variant(node_id) {
                    return self.emit_enum_unit_variant(variant);
                }
                let constant = self.program.constants().get(node_id).ok_or_else(|| {
                    CompileError::Unsupported(format!(
                        "resolved constant '{}' is absent from catalog",
                        node_id.0
                    ))
                })?;
                self.emit_const_value(&expression.ty, &constant.value)
            }
            ResolvedExprKind::Load(place) => {
                let entry = self.root_place(frame, place, true)?;
                self.generator
                    .build_load(entry.llvm_type, entry.storage, "resolved_load")
            }
            ResolvedExprKind::Tuple(elements) => {
                let llvm_type = self.lower_type(&expression.ty)?;
                let BasicTypeEnum::StructType(struct_type) = llvm_type else {
                    return Err(CompileError::Unsupported(
                        "tuple type did not lower to LLVM struct".into(),
                    ));
                };
                let alloca = self.generator.build_alloca(struct_type, "tuple_alloc")?;
                for (index, element) in elements.iter().enumerate() {
                    let value = self.emit_expr(element, frame)?;
                    let field_ptr = self
                        .generator
                        .builder
                        .build_struct_gep(struct_type, alloca, index as u32, "tuple_field")
                        .map_err(|e| CompileError::LlvmError(format!("tuple gep: {e}")))?;
                    let field_type = struct_type
                        .get_field_type_at_index(index as u32)
                        .ok_or_else(|| {
                            CompileError::LlvmError(format!("tuple field {index} type absent"))
                        })?;
                    let value = self.numeric_convert(value, field_type)?;
                    self.generator.build_store(field_ptr, value)?;
                }
                self.generator.build_load(struct_type, alloca, "tuple_val")
            }
            // 0.32.2: List literal construction. Mirrors the legacy
            // compile_list_expr: malloc data buffer, store elements as i64,
            // build {i64 len, ptr data} struct.
            ResolvedExprKind::List(elements) => {
                let list_ptr = self.emit_list_literal(elements, frame, None)?;
                // build_list_struct returns a pointer to the alloca'd struct.
                // Load the struct value so the resolved emitter can store it
                // in local variables (matching tuple semantics).
                let list_ty = self.generator.list_struct_type();
                self.generator.build_load(
                    BasicTypeEnum::StructType(list_ty),
                    list_ptr.into_pointer_value(),
                    "list_val",
                )
            }
            // 0.32.3: Map literal. Call mimi_map_new() then mimi_map_set()
            // for each entry, same as legacy compile_map_literal.
            // Returns an i64 opaque handle — the same type used in types.rs.
            ResolvedExprKind::Map(entries) => {
                let map_new = self.generator.get_runtime_fn("mimi_map_new")?;
                let result = self.generator.build_call(map_new, &[], "map_new_call")?;
                let map_handle = result
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("mimi_map_new returned void".into()))?
                    .into_int_value();
                if !entries.is_empty() {
                    let map_set = self.generator.get_runtime_fn("mimi_map_set")?;
                    for (key_expr, val_expr) in entries {
                        let key_val = self.emit_expr(key_expr, frame)?;
                        let val_val = self.emit_expr(val_expr, frame)?;
                        // Key must be a string — extract the data pointer.
                        let key_ptr = match key_val {
                            BasicValueEnum::PointerValue(pv) => pv,
                            BasicValueEnum::StructValue(sv) => self
                                .generator
                                .build_extract_value(sv.into(), 0, "map_key_ptr")?
                                .into_pointer_value(),
                            _ => {
                                return Err(CompileError::Unsupported(
                                    "map literal key must be a string".into(),
                                ))
                            }
                        };
                        // Value is cast to i64 (ValueHandle) for storage.
                        let val_i64 = self.generator.any_value_to_handle(val_val)?;
                        self.generator.build_call(
                            map_set,
                            &[
                                BasicMetadataValueEnum::IntValue(map_handle),
                                BasicMetadataValueEnum::PointerValue(key_ptr),
                                BasicMetadataValueEnum::IntValue(val_i64),
                            ],
                            "map_set_call",
                        )?;
                    }
                }
                Ok(BasicValueEnum::IntValue(map_handle))
            }
            // 0.32.3: Set literal. Call mimi_set_new() then mimi_set_insert()
            // for each element, same as legacy compile_set_literal.
            // Returns an i64 opaque handle.
            ResolvedExprKind::Set(elements) => {
                let set_new = self.generator.get_runtime_fn("mimi_set_new")?;
                let result = self.generator.build_call(set_new, &[], "set_new_call")?;
                let set_handle = result
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("mimi_set_new returned void".into()))?
                    .into_int_value();
                if !elements.is_empty() {
                    let set_insert = self.generator.get_runtime_fn("mimi_set_insert")?;
                    for elem in elements {
                        let val = self.emit_expr(elem, frame)?;
                        if resolved_type_display_name(self.program, &elem.ty) == "string" {
                            self.generator.compile_set_insert_string(set_handle, val)?;
                        } else {
                            let val_i64 = self.generator.any_value_to_handle(val)?;
                            self.generator.build_call(
                                set_insert,
                                &[
                                    BasicMetadataValueEnum::IntValue(set_handle),
                                    BasicMetadataValueEnum::IntValue(val_i64),
                                ],
                                "set_insert_call",
                            )?;
                        }
                    }
                }
                Ok(BasicValueEnum::IntValue(set_handle))
            }
            // 0.32.5: Record construction. Build LLVM struct from field
            // value types, allocate, store each field. 0.1.8 Phase F:
            // `..rest` starts from the rest record and overrides explicit
            // fields by declared position.
            ResolvedExprKind::Record {
                nominal: _,
                fields,
                rest,
            } => {
                let struct_ty = match self.lower_type(&expression.ty)? {
                    BasicTypeEnum::StructType(sty) => sty,
                    _ => {
                        return Err(CompileError::Unsupported(
                            "record construction on non-struct nominal type".into(),
                        ))
                    }
                };
                let alloca = self.generator.build_alloca(struct_ty, "record_alloc")?;
                if let Some(rest_expr) = rest {
                    let rest_val = self.emit_expr(rest_expr, frame)?;
                    match rest_val {
                        BasicValueEnum::StructValue(sv) => {
                            self.generator.build_store(alloca, sv)?;
                        }
                        BasicValueEnum::PointerValue(pv) => {
                            let loaded = self.generator.build_load(
                                BasicTypeEnum::StructType(struct_ty),
                                pv,
                                "rest_record",
                            )?;
                            self.generator.build_store(alloca, loaded)?;
                        }
                        other => {
                            return Err(CompileError::Unsupported(format!(
                                "record update `..rest` expected struct, got {}",
                                other.get_type()
                            )));
                        }
                    }
                }
                for field in fields {
                    let value = self.emit_expr(&field.value, frame)?;
                    let field_name = self.lookup_field_name(&field.field)?;
                    let field_idx = self.lookup_field_index(&field.field, &field_name)?;
                    let field_ptr = self
                        .generator
                        .builder
                        .build_struct_gep(struct_ty, alloca, field_idx, &field_name)
                        .map_err(|e| CompileError::LlvmError(format!("record gep: {e}")))?;
                    let field_ty =
                        struct_ty
                            .get_field_type_at_index(field_idx)
                            .ok_or_else(|| {
                                CompileError::LlvmError(format!(
                                    "record field {} type absent",
                                    field_name
                                ))
                            })?;
                    let value = self.numeric_convert(value, field_ty)?;
                    self.generator.build_store(field_ptr, value)?;
                }
                self.generator.build_load(
                    BasicTypeEnum::StructType(struct_ty),
                    alloca,
                    "record_val",
                )
            }
            ResolvedExprKind::Project { value, projection } => {
                let agg = self.emit_expr(value, frame)?;
                match projection {
                    crate::core::ir::ResolvedValueProjection::Tuple(index) => {
                        let BasicValueEnum::StructValue(struct_val) = agg else {
                            return Err(CompileError::Unsupported(
                                "tuple projection on non-struct value".into(),
                            ));
                        };
                        // 0.39.136: extractvalue via the builder — the const-only
                        // StructValue::get_field_at_index returns garbage for
                        // runtime SSA aggregates (e.g. str_parse_int(s).0 produced
                        // a bogus pointer where field 0 was an i1).
                        self.generator
                            .builder
                            .build_extract_value(struct_val, *index as u32, "tuple_proj")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("tuple field {index}: {e}"))
                            })
                    }
                    // 0.32.2: Index value projection for List rvalue access.
                    crate::core::ir::ResolvedValueProjection::Index(index_expr) => {
                        let BasicValueEnum::StructValue(struct_val) = agg else {
                            return Err(CompileError::Unsupported(
                                "index projection on non-struct (list) value".into(),
                            ));
                        };
                        // F-018 (0.40.1.22): extract len (field 0) and data pointer
                        // (field 1) with the *builder* (`build_extract_value`), not
                        // `StructValue::get_field_at_index`. The latter is a const-
                        // only helper that returns garbage for runtime SSA
                        // aggregates — the 0.39.136 note on the Tuple arm above
                        // records exactly this trap (`str_parse_int(s).0` yielded a
                        // bogus pointer). For a `List<T>` returned from a function
                        // the projected value is a call result (`%resolved_call`),
                        // an SSA aggregate: `get_field_at_index(0)` handed back the
                        // callee function pointer, and `.into_int_value()` then
                        // panicked — a codegen panic, forbidden by kernel §13.15.
                        // `build_extract_value` is correct for both const and SSA
                        // struct values, so the index read now mirrors the Tuple
                        // arm and the panic is gone.
                        let len_val = self
                            .generator
                            .builder
                            .build_extract_value(struct_val, 0, "list_len")
                            .map_err(|e| CompileError::LlvmError(format!("list len field: {e}")))?
                            .into_int_value();
                        let data_ptr = self
                            .generator
                            .builder
                            .build_extract_value(struct_val, 1, "list_data")
                            .map_err(|e| CompileError::LlvmError(format!("list data field: {e}")))?
                            .into_pointer_value();
                        // Evaluate index.
                        let idx_val = self.emit_expr(index_expr, frame)?.into_int_value();
                        // H-15: bounds-check before the element GEP (VM parity:
                        // reads wrap negative indices, OOB traps E0803).
                        let idx_val =
                            self.emit_checked_list_index(len_val, idx_val, true, "index read")?;
                        // GEP into the i64 data buffer.
                        let i64_ty = self.generator.context.i64_type();
                        let elem_ptr = self.generator.build_in_bounds_gep(
                            i64_ty,
                            data_ptr,
                            &[idx_val],
                            "list_val_idx",
                        )?;
                        let elem_int = self
                            .generator
                            .build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "list_val_i64")?
                            .into_int_value();
                        // Convert the loaded i64 to the proper element type.
                        // The element type = expression.ty (e.g. List<string> for
                        // xs[0] where xs: List<List<string>>).
                        let result_llvm_ty = self.lower_type(&expression.ty)?;
                        // F-018 (0.40.1.22): a record (non-string struct) element
                        // taken directly from a *call result* list is not yet
                        // ownership-safe in the resolved native slice — the
                        // element heap box is not claimed for the caller's scope,
                        // so reading it yields a use-after-free (silent wrong
                        // value, the worst L1 class; kernel §13.15 forbids it).
                        // Fail closed with an actionable error; the equivalent
                        // local-binding form (`let out = f(); out[0]`) is fully
                        // supported. Scalar and string elements are ownership-safe
                        // and fall through to convert_list_elem_i64.
                        if matches!(&value.kind, ResolvedExprKind::Call(_)) {
                            if let BasicTypeEnum::StructType(sty) = result_llvm_ty {
                                let fields = sty.get_field_types();
                                let is_string = fields.len() == 2
                                    && matches!(&fields[0], BasicTypeEnum::PointerType(_))
                                    && matches!(
                                        &fields[1],
                                        BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                                    );
                                if !is_string {
                                    return Err(CompileError::Unsupported(
                                        "indexing a record element directly from a list returned by a \
                                         function call is not yet supported by the resolved native \
                                         slice (ownership of the element heap box is not claimed). Bind \
                                         the list to a local first, e.g. `let out = f(); out[0]`. \
                                         Tracked as F-018."
                                            .into(),
                                    ));
                                }
                            }
                        }
                        self.convert_list_elem_i64(elem_int, result_llvm_ty)
                    }
                    // 0.32.5: Field value projection for record rvalue access.
                    crate::core::ir::ResolvedValueProjection::Field(field_id) => {
                        let BasicValueEnum::StructValue(struct_val) = agg else {
                            return Err(CompileError::Unsupported(
                                "field projection on non-struct (record) value".into(),
                            ));
                        };
                        // Look up field name from type definitions.
                        let field_name = self.lookup_field_name(field_id)?;
                        let field_index = self.lookup_field_index(field_id, &field_name)?;
                        // `get_field_at_index` is a const-value accessor and
                        // returns None for runtime SSA aggregates. Use the
                        // builder form so a record loaded from a List slot is
                        // projected from the actual value (the same rule as
                        // the tuple projection above).
                        self.generator
                            .builder
                            .build_extract_value(struct_val, field_index, "record_proj")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("record field {field_index}: {e}"))
                            })
                    }
                    other => Err(CompileError::Unsupported(format!(
                        "value projection {other:?} escaped resolved native eligibility"
                    ))),
                }
            }
            ResolvedExprKind::Binary { op, left, right } => {
                // C-4 (full-audit 2026-08-05, VERIFIED CRITICAL L1): `and`/`or`
                // were compiled eagerly — RHS evaluated unconditionally, then
                // mapped to bitwise and/or. The VM and the legacy emitter
                // (operator.rs compile_short_circuit_expr, "VERIFIED CRITICAL,
                // L1") both short-circuit, so the eager lowering trapped on
                // effects the VM never reaches (`x != 0 and 10/x > 2` with
                // x=0 → spurious E0801) and ran skipped-branch side effects.
                // Lower through the block-structured short-circuit machine.
                if matches!(
                    op,
                    ResolvedBinaryOp::LogicalAnd | ResolvedBinaryOp::LogicalOr
                ) {
                    return self.emit_short_circuit(*op, left, right, frame);
                }
                let left = self.emit_expr(left, frame)?;
                let right = self.emit_expr(right, frame)?;
                // §6-#57 (audit-2026-08-05, L1): the i32 width context for a
                // binop comes from the checker-finalized canonical type of the
                // whole expression — NOT from operand bit widths. `xs[0] + 1`
                // on a List<i64> is i64 (both operands widened), while
                // `x: i32 + 1` is i32; operand widths alone cannot tell the
                // two apart (`1 + x: i32` is 64+32 but i32-width). The legacy
                // heuristic (None) stays correct there because legacy list
                // elements are i64 slots, but the resolved emitter passes the
                // exact canonical answer.
                let binop_i32_ctx = matches!(
                    self.program.resolved_types().get(&expression.ty),
                    Some(crate::core::ResolvedType::Primitive(
                        crate::core::PrimitiveType::I32
                    ))
                );
                self.generator
                    .compile_binop(binary_op(*op), left, right, Some(binop_i32_ctx))
            }
            ResolvedExprKind::Unary { op, operand } => match op {
                // 0.37.x: borrow shared/mutable expressions produce a pointer
                // to the operand's storage (or a temporary alloca for an
                // rvalue). This enables `let ref` / `&mut` locals in the
                // resolved slice.
                ResolvedUnaryOp::BorrowShared | ResolvedUnaryOp::BorrowMutable => {
                    if let ResolvedExprKind::Load(place) = &operand.kind {
                        let entry = self.root_place(frame, place, false)?;
                        return Ok(BasicValueEnum::PointerValue(entry.storage));
                    }
                    let inner = self.emit_expr(operand, frame)?;
                    let slot = self
                        .generator
                        .build_alloca(inner.get_type(), "borrow_tmp")?;
                    self.generator.build_store(slot, inner)?;
                    Ok(BasicValueEnum::PointerValue(slot))
                }
                // Dereference of a reference pointer loads through the
                // pointer value.
                ResolvedUnaryOp::Dereference => {
                    let ptr = self.emit_expr(operand, frame)?;
                    let pv = ptr.into_pointer_value();
                    let target_ty = self.lower_type(&expression.ty)?;
                    self.generator
                        .build_load(target_ty, pv, "deref_load")
                        .map_err(|e| CompileError::LlvmError(format!("deref load: {e}")))
                }
                _ => {
                    let value = self.emit_expr(operand, frame)?;
                    self.emit_unary(*op, value)
                }
            },
            ResolvedExprKind::Cast { value, conversion } => {
                let value = self.emit_expr(value, frame)?;
                self.apply_conversion(value, conversion)
            }
            ResolvedExprKind::Spawn(value) => self.emit_spawn(value, frame),
            ResolvedExprKind::Await(value) => self.emit_await(value, &expression.ty, frame),
            ResolvedExprKind::Call(call) => {
                // 0.39.x (L1 parity fix): generic calls are compiled once as a
                // single resolved skeleton whose type variable T is never
                // substituted, so `lower_type` drops T to i64 and the call ABI
                // mismatches the monomorphized callee whenever the concrete
                // instance does not share the skeleton's i64-based return ABI
                // (observed: `func first<T>(xs: List<T>) -> T { xs[0] }` called
                // with `List<string>` segfaulted and with `List<f64>` silently
                // returned raw bits; the interpreter is correct).
                // The resolved IR already carries the concrete type arguments in
                // `call.type_arguments`, so monomorphize the callee on demand via
                // the legacy emitter using those args, and call the mangled
                // instance directly. This keeps the resolved fast path for the
                // surrounding body while fixing the call's ABI.
                let mut generic_symbol_override: Option<String> = None;
                if let ResolvedCallee::Function(owner) = &call.callee {
                    if let Some(callee_fn) = self.program.functions().get(owner) {
                        if !callee_fn.generics.is_empty() && !call.type_arguments.is_empty() {
                            // The resolved native slice compiles a generic callee
                            // once with its type variable T unsubstituted, so it
                            // cannot lower instances whose ABI differs from that
                            // skeleton — `lower_type` drops T to i64 and the call
                            // ABI mismatches the concrete instantiation (observed:
                            // `List<string>` element segfaulted, `List<f64>`
                            // silently returned raw bits as i64). Route the call
                            // to the legacy monomorphizer (which builds a
                            // correctly-substituted instance from
                            // `call.type_arguments`) UNLESS the concrete result
                            // provably shares the skeleton's i64-based ABI: small
                            // integers / bool / char coerce losslessly, and Unit
                            // is void in both. Floats live in xmm registers,
                            // i128/u128 use a wide ABI, strings and every
                            // composite (nominal/tuple/flow state) differ — those
                            // fail closed to the legacy emitter. Unknown result
                            // types fail closed the same way.
                            let rt = self.program.resolved_types().get(&call.result);
                            let result_abi_safe = match rt {
                                Some(ResolvedType::Primitive(p)) => matches!(
                                    p,
                                    PrimitiveType::I8
                                        | PrimitiveType::I16
                                        | PrimitiveType::I32
                                        | PrimitiveType::I64
                                        | PrimitiveType::Isize
                                        | PrimitiveType::U8
                                        | PrimitiveType::U16
                                        | PrimitiveType::U32
                                        | PrimitiveType::U64
                                        | PrimitiveType::Usize
                                        | PrimitiveType::Bool
                                        | PrimitiveType::Char
                                        | PrimitiveType::Unit
                                ),
                                _ => false,
                            };
                            // A generic function whose type parameter is
                            // instantiated to a composite / non-skeleton type
                            // (string, float, i128, tuple, named, …) cannot be
                            // lowered by the resolved skeleton, which collapses
                            // the type variable T to i64. The body needs the
                            // concrete type to decode `List<T>` elements
                            // (Display / index / to_json / contains), so route it
                            // to the legacy monomorphizer — exactly the same
                            // treatment already given to composite RETURN types.
                            // Previously only the RETURN type was checked, so
                            // `func show<T>(xs: List<T>)` (returns Unit) stayed an
                            // abstract skeleton and printed `List<unknown>`,
                            // breaking cross-emitter `List<(T,T)>` / nested
                            // non-scalar element returns. Closes that gap.
                            let args_abi_safe = call.type_arguments.iter().all(|tid| {
                                match self.program.resolved_types().get(tid) {
                                    Some(ResolvedType::Primitive(p)) => matches!(
                                        p,
                                        PrimitiveType::I8
                                            | PrimitiveType::I16
                                            | PrimitiveType::I32
                                            | PrimitiveType::I64
                                            | PrimitiveType::Isize
                                            | PrimitiveType::U8
                                            | PrimitiveType::U16
                                            | PrimitiveType::U32
                                            | PrimitiveType::U64
                                            | PrimitiveType::Usize
                                            | PrimitiveType::Bool
                                            | PrimitiveType::Char
                                            | PrimitiveType::Unit
                                    ),
                                    _ => false,
                                }
                            });
                            let needs_legacy = !result_abi_safe || !args_abi_safe;
                            if needs_legacy {
                                // E0722 根治 scaffold: record the composite-T / cap
                                // generic instance required by this call so a later
                                // round can emit a resolved monomorphization instead
                                // of routing to the legacy monomorphizer. Emission
                                // behavior is unchanged by this recording.
                                self.pending_generic_instances.push((
                                    callee_fn.qualified_name.clone(),
                                    call.type_arguments.clone(),
                                ));
                                if let Some(fdef) = self
                                    .generator
                                    .func_defs
                                    .get(&callee_fn.qualified_name)
                                    .cloned()
                                {
                                    let ast_map = resolved_type_args_to_ast(
                                        &callee_fn.generics,
                                        &call.type_arguments,
                                        self.program.resolved_types(),
                                    );
                                    if !ast_map.is_empty() {
                                        let mangled =
                                            CodeGenerator::mangle_name(&fdef.name, &ast_map);
                                        // GENERIC-SHADOW-MONO-001 mirror: compile when the mangled name is absent
                                        // OR only forward-declared (no body) — see the shadow arm.
                                        let needs_compile = self
                                            .generator
                                            .module
                                            .get_function(&mangled)
                                            .map(|f| f.count_basic_blocks() == 0)
                                            .unwrap_or(true);
                                        if needs_compile {
                                            self.generator.compile_generic_func(&fdef, &ast_map)?;
                                        }
                                        generic_symbol_override = Some(mangled);
                                    }
                                }
                            }
                        }
                    }
                }
                // 0.36.15 L1: `exit(...)` is the explicit fault-propagation
                // call — run registered `on failure` compensations (LIFO)
                // before it, mirroring the legacy block.rs exit hook. The
                // guard blocks belong to the enclosing function body
                // (current_body; Copy so the borrow does not conflict with
                // the &mut self emit below).
                let is_exit = matches!(
                    &call.callee,
                    ResolvedCallee::Function(owner)
                        if self.program.functions().get(owner).map(|f| f.qualified_name.as_str()) == Some("exit")
                );
                if is_exit && !self.comp_scopes.is_empty() {
                    // The guard blocks live in the callee-owner's body
                    // registry; re-fetch and clone it (rare path) so the
                    // &mut self emit below does not alias `self.program`.
                    let pending = std::mem::take(&mut self.comp_scopes);
                    if let ResolvedCallee::Function(owner) = &call.callee {
                        if let Some(guard_callable) = self.program.callable(owner).cloned() {
                            for block in pending.iter().rev() {
                                self.emit_block(&guard_callable.body, block, frame)?;
                            }
                        }
                    }
                }
                // 0.34.43: positions whose callee parameter is a non-self
                // view/mutate borrow are passed BY ADDRESS (the caller's
                // storage pointer) — the reference ABI legacy declare_func
                // promises. Anything not a bare place load fails closed to
                // the legacy per-function fallback (no silent ABI mismatch).
                let borrow_positions: Vec<bool> = match &call.callee {
                    ResolvedCallee::Function(owner) => self
                        .program
                        .callable(owner)
                        .map(|callee_callable| {
                            callee_callable
                                .signature
                                .parameters
                                .iter()
                                .map(|p| {
                                    p.name != "self"
                                        && matches!(
                                            p.permission,
                                            Some(crate::core::ir::Permission::View)
                                                | Some(crate::core::ir::Permission::Mutate)
                                        )
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    _ => Vec::new(),
                };
                // Evaluate arguments (shared by all callee kinds)
                let mut arguments = Vec::with_capacity(call.arguments.len());
                for (index, argument) in call.arguments.iter().enumerate() {
                    if borrow_positions.get(index).copied().unwrap_or(false) {
                        let crate::core::ResolvedExprKind::Load(place) = &argument.value.kind
                        else {
                            return Err(CompileError::Unsupported(format!(
                                "borrow argument '{}' is not a place load",
                                argument.value.node_id.0
                            )));
                        };
                        if !place.projections.is_empty()
                            || !matches!(argument.conversion.kind, CheckedConversionKind::Identity)
                        {
                            return Err(CompileError::Unsupported(format!(
                                "borrow argument '{}' has projections or conversion",
                                argument.value.node_id.0
                            )));
                        }
                        let entry = frame.locals.get(&place.base).ok_or_else(|| {
                            CompileError::Unsupported(format!(
                                "borrow argument '{}' base local is not in frame",
                                argument.value.node_id.0
                            ))
                        })?;
                        arguments.push(BasicMetadataValueEnum::PointerValue(entry.storage));
                        continue;
                    }
                    let value = self.emit_expr(&argument.value, frame)?;
                    let value = self.apply_conversion(value, &argument.conversion)?;
                    arguments.push(BasicMetadataValueEnum::from(value));
                }
                match &call.callee {
                    ResolvedCallee::ActorMethod { actor, method } => {
                        return self.emit_actor_method_call(
                            call, &arguments, actor, method, expression, frame,
                        );
                    }
                    ResolvedCallee::Function(owner) => {
                        let symbol = if let Some(ov) = generic_symbol_override.clone() {
                            ov
                        } else {
                            self.callable_symbol(owner)?.to_string()
                        };
                        let callee =
                            self.generator.module.get_function(&symbol).ok_or_else(|| {
                                CompileError::LlvmError(format!(
                                    "resolved callee '{symbol}' is undeclared"
                                ))
                            })?;
                        // 0.32.19: Coerce arguments to match the callee's
                        // declared parameter types. The resolved IR may type
                        // an argument as i64 (e.g. literal 7) while the
                        // callee's forward declaration (from legacy emitter's
                        // generic instantiation) expects i32. Without this
                        // coercion, LLVM verification fails on type mismatch.
                        let params = callee.get_params();
                        for (i, arg) in arguments.iter_mut().enumerate() {
                            if let Some(param) = params.get(i) {
                                let param_ty = param.get_type();
                                let arg_basic: BasicValueEnum = match *arg {
                                    BasicMetadataValueEnum::IntValue(iv) => iv.into(),
                                    BasicMetadataValueEnum::FloatValue(fv) => fv.into(),
                                    BasicMetadataValueEnum::PointerValue(pv) => pv.into(),
                                    BasicMetadataValueEnum::StructValue(sv) => sv.into(),
                                    _ => continue,
                                };
                                if arg_basic.get_type() != param_ty {
                                    let coerced = self.coerce_to(arg_basic, param_ty)?;
                                    *arg = BasicMetadataValueEnum::from(coerced);
                                }
                            }
                        }
                        let call_result = self
                            .generator
                            .build_call(callee, &arguments, "resolved_call")?
                            .try_as_basic_value_opt();
                        call_result
                            .ok_or_else(|| {
                                CompileError::LlvmError(format!(
                                    "resolved callee '{symbol}' returned void"
                                ))
                            })
                            .and_then(|result| {
                                // L6: when the callee returns a custom enum,
                                // register its payload box for a tag-conditional
                                // free at the caller's scope exit. The callee
                                // (legacy or resolved) claimed the box on return
                                // (claim_returned_enum_box) — ownership transfers
                                // here. Non-enum callees pass through (detected
                                // via the callee's return-type AST in func_defs).
                                let result = self
                                    .generator
                                    .track_enum_box_return_lifetime(&symbol, result)?;
                                // Heap-return ownership: resolved calls need
                                // caller-side tracking for String, List, and
                                // heap-field Record results. Register every
                                // pointer leaf so `free_heap_allocs` at the
                                // caller's scope exit releases it once.
                                if !matches!(
                                    self.program.resolved_types().get(&call.result),
                                    Some(ResolvedType::Function {
                                        abi: FunctionTypeAbi::Mimi,
                                        ..
                                    })
                                ) {
                                    let result_ty = self.lower_type(&call.result)?;
                                    // Top-level List<string> needs per-element
                                    // ownership: the data array is registered as
                                    // a StringListData heap entry so each string
                                    // data pointer is freed before the array.
                                    if matches!(
                                        self.program.resolved_types().get(&call.result),
                                        Some(ResolvedType::Nominal {
                                            item,
                                            arguments,
                                            ..
                                        }) if item.as_str() == "builtin:type:List"
                                            && arguments.len() == 1
                                            && matches!(
                                                self.program
                                                    .resolved_types()
                                                    .get(&arguments[0]),
                                                Some(ResolvedType::Primitive(
                                                    PrimitiveType::String
                                                ))
                                            )
                                    ) {
                                        let BasicValueEnum::StructValue(sv) = result else {
                                            return Ok(result);
                                        };
                                        let BasicTypeEnum::StructType(list_ty) = result_ty else {
                                            return Ok(result.into());
                                        };
                                        // 0.39.x (L1 parity fix): a
                                        // legacy-monomorphized instance's
                                        // list boxes borrowed payload pointers;
                                        // give the returned list private
                                        // element copies before registering it
                                        // for unconditional per-element frees.
                                        if generic_symbol_override.is_some() {
                                            self.copy_string_list_elements_owned(sv)?;
                                        }
                                        self.generator
                                            .register_returned_string_list(sv, list_ty)?;
                                        return Ok(result);
                                    }
                                    // List<List<string>> from an override call:
                                    // same borrowed-payload hazard one level
                                    // deeper; normalize inner elements before
                                    // the StringListListData registration in
                                    // track_returned_heap_pointers below.
                                    if generic_symbol_override.is_some()
                                        && self.string_list_list_shape(&call.result)
                                    {
                                        if let (
                                            BasicValueEnum::StructValue(outer_sv),
                                            Some(ResolvedType::Nominal {
                                                arguments: outer_args,
                                                ..
                                            }),
                                        ) = (
                                            result,
                                            self.program.resolved_types().get(&call.result),
                                        ) {
                                            if let Some(ResolvedType::Nominal {
                                                item: inner_item,
                                                ..
                                            }) =
                                                self.program.resolved_types().get(&outer_args[0])
                                            {
                                                if inner_item.as_str() == "builtin:type:List" {
                                                    if let BasicTypeEnum::StructType(elem_list_ty) =
                                                        self.lower_type(&outer_args[0])?
                                                    {
                                                        self.copy_string_list_list_elements_owned(
                                                            outer_sv,
                                                            elem_list_ty,
                                                        )?;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    self.track_returned_heap_pointers(
                                        result,
                                        result_ty,
                                        Some(call.result.clone()),
                                    )?;
                                    return Ok(result);
                                }
                                // B9 (audit): when the callee returns a Mimi
                                // closure, register its env so the caller's
                                // scope exit releases it. The callee (legacy
                                // or resolved emitter) already claimed the env
                                // on its side — ownership transfers here.
                                let sv = match result {
                                    BasicValueEnum::StructValue(sv) => sv,
                                    other => return Ok(other),
                                };
                                let fields = sv.get_type().get_field_types();
                                let is_closure_struct = fields.len() == 2
                                    && matches!(fields[0], BasicTypeEnum::PointerType(_))
                                    && matches!(fields[1], BasicTypeEnum::PointerType(_));
                                if !is_closure_struct {
                                    return Ok(sv.into());
                                }
                                if let BasicValueEnum::PointerValue(env) = self
                                    .generator
                                    .build_extract_value(sv.into(), 1, "resolved_closure_env")?
                                {
                                    // Registered as a raw Ptr: freed at scope
                                    // exit; if the closure is returned, the
                                    // resolved emitter drains the whole scope
                                    // (caller takes ownership).
                                    self.generator.register_heap_alloc(env);
                                }
                                Ok(result)
                            })
                    }
                    ResolvedCallee::Constructor(variant_id) => {
                        // 0.37.2: user-defined newtype and enum variant
                        // constructor expressions. Newtypes are value-identity
                        // wrappers; enum variants reuse the resolved custom
                        // enum ctor ({i32 tag, i64 payload}).
                        if self.is_newtype_variant(variant_id) {
                            if call.arguments.len() != 1 {
                                return Err(CompileError::Unsupported(format!(
                                    "newtype constructor '{}' expects 1 argument, got {}",
                                    self.lookup_variant_name(variant_id)?,
                                    call.arguments.len()
                                )));
                            }
                            let value = self.emit_expr(&call.arguments[0].value, frame)?;
                            return self.apply_conversion(value, &call.arguments[0].conversion);
                        }
                        let variant_name = self.lookup_variant_name(variant_id)?;
                        // 0.39.x stdlib matrix sweep: multi-field enum
                        // constructors compile inside the resolved slice by
                        // reusing the registered `{Type}_{Variant}`
                        // constructor functions — the same layout source of
                        // truth the legacy emitter and the match-side decoder
                        // already share. This removes the silent
                        // per-function legacy degradation for any body that
                        // touches a multi-field variant (previously ANY such
                        // function fell back to legacy wholesale).
                        if call.arguments.len() > 1 {
                            let ResolvedType::Nominal { item, .. } = self
                                .program
                                .resolved_types()
                                .get(&expression.ty)
                                .ok_or_else(|| {
                                    CompileError::Unsupported(
                                        "custom enum ctor: missing type".into(),
                                    )
                                })?
                            else {
                                return Err(CompileError::Unsupported(
                                    "custom enum ctor: expression type is not Nominal".into(),
                                ));
                            };
                            let item_str = item.as_str();
                            let type_name = item_str.strip_prefix("type:").unwrap_or(item_str);
                            let ctor_name = format!("{type_name}_{variant_name}");
                            let ctor_fn =
                                self.generator.module.get_function(&ctor_name).ok_or_else(
                                    || {
                                        CompileError::Unsupported(format!(
                                            "multi-field enum constructor '{variant_name}' \
                                         has no registered ctor '{ctor_name}'"
                                        ))
                                    },
                                )?;
                            let mut compiled_args = Vec::with_capacity(call.arguments.len());
                            for argument in &call.arguments {
                                let value = self.emit_expr(&argument.value, frame)?;
                                let value = self.apply_conversion(value, &argument.conversion)?;
                                compiled_args.push(value);
                            }
                            let call_args = self
                                .generator
                                .maybe_pack_enum_ctor_args(&compiled_args, ctor_fn)?;
                            return self.generator.emit_direct_call(
                                ctor_fn,
                                &call_args,
                                "enum_ctor",
                            );
                        }
                        // 0.37.2 safety: emit_custom_enum_ctor currently
                        // supports zero/one-payload enum variants. Multi-field
                        // variants are handled above via the registered
                        // per-variant constructor functions.
                        return self.emit_custom_enum_ctor(&variant_name, call, expression, frame);
                    }
                    ResolvedCallee::Builtin(builtin_id) => {
                        let mut name = builtin_id.as_str();
                        // 2026-08-06 (audit 1c): resolved string METHOD calls
                        // (s.trim() etc.) arrive as `builtin.method.string.X`;
                        // map them to the global str_* builtin so the same
                        // emitters + guards apply (they used to E0709).
                        if let Some(method) = name.strip_prefix("builtin.method.string.") {
                            let mapped = match method {
                                "trim" => Some("str_trim"),
                                "to_upper" => Some("str_to_upper"),
                                "to_lower" => Some("str_to_lower"),
                                "contains" => Some("str_contains"),
                                "starts_with" => Some("str_starts_with"),
                                "ends_with" => Some("str_ends_with"),
                                "split" => Some("str_split"),
                                "replace" => Some("str_replace"),
                                "repeat" => Some("str_repeat"),
                                "char_at" => Some("str_char_at"),
                                // `len` is special-cased before trait dispatch in
                                // the legacy path; the resolved path receives it
                                // as `builtin.method.string.len` and must map to
                                // the polymorphic `len` builtin (which already
                                // unboxes the fat-ABI string correctly). Without
                                // this, `s.len()` on a spawn/await result hard-
                                // errors E0722 ("no resolved-native emitter").
                                "len" => Some("len"),
                                // D-5 (2026-08-06): method form is strict in
                                // the VM — match the legacy method mapping.
                                "substring" => Some("str_substring_strict"),
                                // Parity with the legacy `string_method_to_builtin`
                                // table so a future checker that admits this method
                                // does not regress to E0722.
                                "count_substring" => Some("str_count_substring"),
                                "index_of" => Some("str_index_of"),
                                "parse_int" => Some("str_parse_int"),
                                "parse_float" => Some("str_parse_float"),
                                _ => None,
                            };
                            if let Some(mapped) = mapped {
                                name = mapped;
                            }
                        }
                        // 0.1.8 Phase E: resolved SessionChan METHOD calls
                        // (`ch.send(v)` etc.) arrive as
                        // `builtin.method.session.X`; map them to the existing
                        // session_* free-function builtins.
                        if let Some(method) = name.strip_prefix("builtin.method.session.") {
                            let mapped = match method {
                                "send" => Some("session_send"),
                                "recv" => Some("session_recv"),
                                "close" => Some("session_close"),
                                _ => None,
                            };
                            if let Some(mapped) = mapped {
                                name = mapped;
                            }
                        }
                        // 0.39.37 (SET-REMOVE-CODEGEN-001 closed): resolved
                        // SET METHOD calls (`s.size()`, `s.remove(v)`,
                        // `s.contains(v)`, ...) arrive as
                        // `builtin.method.set.X`. They used to E0709 (only the
                        // ProtocolMethod callee form reached the set handler).
                        // Route the Builtin form to the same handler.
                        if let Some(method) = name.strip_prefix("builtin.method.set.") {
                            if let Some(value) =
                                self.emit_builtin_set_protocol_method(method, &arguments)?
                            {
                                return Ok(value);
                            }
                        }
                        // 0.1.10 (BUG K): resolved List METHOD calls arrive as
                        // `builtin.method.list.len` — `resolve_builtin_method`
                        // registers ONLY `len` for the list family (every other
                        // List method is trait-dispatched via `ListExt` and
                        // already works in the resolved path). The resolved
                        // emitter previously had no mapping for it and hard-
                        // errored E0722, which fired the moment a `List` method
                        // was called inside a resolved-forced context: spawn/
                        // await results, or any program containing a `fails`
                        // flow transition (the `?` operator forces the resolved
                        // emitter for the whole program, so even a plain
                        // `xs.len()` in `main` broke). `len` is the polymorphic
                        // builtin (it already unboxes fat-ABI strings and
                        // handles List/Map/Set/string), so route `list.len` to
                        // it exactly like `string.len` (BUG G).
                        if let Some(method) = name.strip_prefix("builtin.method.list.") {
                            if method == "len" {
                                name = "len";
                            }
                        }
                        // 2026-08-06 (audit 1g): str_contains List haystack →
                        // compile_contains (VM polymorphism parity); the guard
                        // below keeps rejecting Set/other receivers.
                        // (audit 1k) Set haystacks → mimi_set_contains.
                        if name == "str_contains" && !call.arguments.is_empty() {
                            let hay_ty = resolved_type_display_name(
                                self.program,
                                &call.arguments[0].value.ty,
                            );
                            if hay_ty.starts_with("Set") {
                                if call.arguments.len() < 2 {
                                    return Err(CompileError::WrongArgCount(
                                        "str_contains expects 2 arguments".into(),
                                    ));
                                }
                                return self.generator.compile_set_contains_fn(
                                    arguments[0],
                                    arguments[1],
                                    resolved_type_display_name(
                                        self.program,
                                        &call.arguments[1].value.ty,
                                    ) == "string",
                                );
                            }
                            if hay_ty.starts_with("List") {
                                name = "contains";
                            }
                        }
                        // 2026-08-06 (audit 1f): exec_safe(prog, args...) —
                        // every vararg must be a string (codegen used to pack
                        // List varargs into argv as garbage; VM: E0800).
                        if name == "exec_safe" && call.arguments.len() > 1 {
                            for (i, arg) in call.arguments.iter().enumerate().skip(1) {
                                let arg_ty =
                                    resolved_type_display_name(self.program, &arg.value.ty);
                                if self.generator.is_definitely_not_string(&arg_ty) {
                                    return Err(CompileError::TypeMismatch(format!(
                                        "exec_safe: all arguments must be strings (argument {} is {})",
                                        i, arg_ty
                                    )));
                                }
                            }
                        }
                        // 2026-08-06 (audit 1c): `contains` is polymorphic in
                        // the VM ((string|List|Set, value)); compile_contains
                        // only handles List and a string haystack would SIGSEGV
                        // (load_list_len on a string struct). Redirect string
                        // haystacks to str_contains — the guard below then
                        // enforces the string needle too. (audit 1j) Set
                        // haystacks: bare i64 handle → mimi_set_contains
                        // (was a VM-only gap).
                        if name == "contains" && !call.arguments.is_empty() {
                            let hay_ty = resolved_type_display_name(
                                self.program,
                                &call.arguments[0].value.ty,
                            );
                            if hay_ty.starts_with("Set") {
                                if call.arguments.len() < 2 {
                                    return Err(CompileError::WrongArgCount(
                                        "contains expects 2 arguments".into(),
                                    ));
                                }
                                return self.generator.compile_set_contains_fn(
                                    arguments[0],
                                    arguments[1],
                                    resolved_type_display_name(
                                        self.program,
                                        &call.arguments[1].value.ty,
                                    ) == "string",
                                );
                            }
                            if hay_ty == "string" {
                                name = "str_contains";
                            }
                        }
                        // 2026-08-06 (audit 1): string-only builtins — reject a
                        // definitely non-string argument at compile time
                        // (List arrives as a raw pointer, indistinguishable from
                        // a string pointer in the emitter; VM parity: E0800).
                        // Guards every string argument position of the whole
                        // str_* / regex_* family.
                        if let Some(pos) = CodeGenerator::string_only_builtin_string_args(name) {
                            for &p in pos {
                                let p = p as usize;
                                if p >= call.arguments.len() {
                                    break; // arg-count error is reported later
                                }
                                let arg_ty = resolved_type_display_name(
                                    self.program,
                                    &call.arguments[p].value.ty,
                                );
                                if self.generator.is_definitely_not_string(&arg_ty) {
                                    return Err(CompileError::TypeMismatch(format!(
                                        "{} expects a string argument at position {}, found {}",
                                        name, p, arg_ty
                                    )));
                                }
                            }
                        }
                        // 0.37.x: reduce(list, fn, init) requires the resolved
                        // emitter to drive the closure loop; the legacy
                        // compile_builtin_call does not implement this
                        // compile-time intrinsic.
                        if name == "reduce" && call.arguments.len() == 3 {
                            return self.emit_resolved_reduce(call, &arguments, frame);
                        }
                        // 0.35.23 deep-eval (mimi-log main): read_lines_each
                        // takes a closure — the legacy compile_read_lines_each
                        // call emitter builds the C thunk, stores the closure
                        // {fn_ptr, env_ptr} in TLS and calls the runtime with
                        // the thunk pointer. The generic runtime call path
                        // would coerce the closure struct to a bare ptr
                        // (numeric_convert {ptr,ptr} → ptr fails) and the
                        // whole function fell back to legacy.
                        if name == "read_lines_each" {
                            let mut compiled_args: Vec<BasicValueEnum<'ctx>> =
                                Vec::with_capacity(arguments.len());
                            for argument in &arguments {
                                match *argument {
                                    BasicMetadataValueEnum::IntValue(iv) => {
                                        compiled_args.push(iv.into())
                                    }
                                    BasicMetadataValueEnum::FloatValue(fv) => {
                                        compiled_args.push(fv.into())
                                    }
                                    BasicMetadataValueEnum::PointerValue(pv) => {
                                        compiled_args.push(pv.into())
                                    }
                                    BasicMetadataValueEnum::StructValue(sv) => {
                                        compiled_args.push(sv.into())
                                    }
                                    _ => {
                                        return Err(CompileError::Unsupported(
                                            "resolved read_lines_each: unsupported argument kind"
                                                .into(),
                                        ))
                                    }
                                }
                            }
                            return self.generator.compile_read_lines_each_call(&compiled_args);
                        }
                        // to_int/to_float aggregate guard (VM message parity):
                        // a statically known aggregate argument cannot be
                        // converted; reject with the VM-aligned E0800 message
                        // instead of letting the native parser strlen the
                        // aggregate pointer and report "invalid digit".
                        if CodeGenerator::is_conversion_builtin(name) && !call.arguments.is_empty()
                        {
                            let arg_ty = resolved_type_display_name(
                                self.program,
                                &call.arguments[0].value.ty,
                            );
                            if self.generator.is_definitely_not_convertible(&arg_ty) {
                                return Err(CompileError::TypeMismatch(format!(
                                    "[E0800] {} cannot convert this type ({})",
                                    name, arg_ty
                                )));
                            }
                        }
                        // push/pop need the *original alloca pointer* for
                        // their first argument — the legacy `compile_push`
                        // (require_list_pointer) GEPs into the struct fields
                        // and stores back, which only works with a pointer.
                        // When the first argument is a simple local variable,
                        // load its alloca pointer instead of the loaded value.
                        if matches!(name, "push" | "pop") && !arguments.is_empty() {
                            if let Some(first_arg) = call.arguments.first() {
                                use crate::core::ir::ResolvedExprKind;
                                if let ResolvedExprKind::Load(place) = &first_arg.value.kind {
                                    if place.projections.is_empty() {
                                        if let Some(entry) = frame.locals.get(&place.base) {
                                            if matches!(
                                                entry.llvm_type,
                                                BasicTypeEnum::StructType(_)
                                            ) {
                                                arguments[0] = BasicMetadataValueEnum::PointerValue(
                                                    entry.storage,
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // Option/Result constructors are handled by the legacy
                        // compile_constructor path, not compile_builtin_call.
                        if matches!(name, "Some" | "None" | "Ok" | "Err") {
                            // 0.32.17: If the expression's type is a custom
                            // enum (not built-in Result/Option), compile as
                            // custom enum construction ({i32 tag, i64 payload}).
                            let is_custom_enum = matches!(
                                self.program.resolved_types().get(&expression.ty),
                                Some(ResolvedType::Nominal { item, .. })
                                    if self.program.type_defs().values().any(|td| {
                                        let item_str = item.as_str();
                                        let tn = item_str
                                            .strip_prefix("type:")
                                            .unwrap_or(item_str);
                                        (td.qualified_name == tn
                                            || td.qualified_name == item_str)
                                            && matches!(
                                                td.kind,
                                                crate::core::resolved::ResolvedTypeKind::Enum
                                            )
                                    })
                            );
                            if is_custom_enum {
                                return self.emit_custom_enum_ctor(name, call, expression, frame);
                            }
                            // 0.34.30 (dx-backlog #11): build Option/Result values
                            // in the *resolved* layout ({bool, ok_llvm, i64_err})
                            // instead of the legacy compile_constructor. Legacy
                            // compile_err_constructor hard-codes the ok-pad as
                            // i64 outside list-packing, so Err(..) yields
                            // {bool, i64, i64} while Ok(1.5) yields {bool, double,
                            // i64} — if/else branch merging then fails on the
                            // numeric conversion and the whole trait-impl
                            // function falls back to legacy.
                            let ctor_args: Vec<BasicValueEnum<'ctx>> = call
                                .arguments
                                .iter()
                                .map(|arg| -> Result<_, CompileError> {
                                    let value = self.emit_expr(&arg.value, frame)?;
                                    self.apply_conversion(value, &arg.conversion)
                                })
                                .collect::<Result<_, _>>()?;
                            return self.emit_resolved_optional_ctor(
                                name,
                                ctor_args,
                                &expression.ty,
                            );
                        }
                        // 0.32.17: Option/Result predicate and accessor methods.
                        if matches!(
                            name,
                            "builtin.method.option.is_some"
                                | "builtin.method.option.is_none"
                                | "builtin.method.result.is_ok"
                                | "builtin.method.result.is_err"
                        ) {
                            // Receiver is the first argument (the Option/Result value).
                            let recv = self.emit_expr(&call.arguments[0].value, frame)?;
                            let sv = match recv {
                                BasicValueEnum::StructValue(sv) => sv,
                                BasicValueEnum::PointerValue(pv) => {
                                    let sty = self.lower_type(&call.arguments[0].value.ty)?;
                                    self.generator
                                        .build_load(sty, pv, "method_recv")?
                                        .into_struct_value()
                                }
                                _ => {
                                    return Err(CompileError::Unsupported(
                                        "Option/Result method on non-struct receiver".into(),
                                    ))
                                }
                            };
                            let disc = self
                                .generator
                                .builder
                                .build_extract_value(sv, 0, "method_disc")
                                .map_err(|e| CompileError::LlvmError(format!("method disc: {e}")))?
                                .into_int_value();
                            let zero = disc.get_type().const_int(0, false);
                            let is_positive = matches!(
                                name,
                                "builtin.method.option.is_some" | "builtin.method.result.is_ok"
                            );
                            let pred = if is_positive {
                                inkwell::IntPredicate::NE
                            } else {
                                inkwell::IntPredicate::EQ
                            };
                            let result = self
                                .generator
                                .builder
                                .build_int_compare(pred, disc, zero, "method_pred")
                                .map_err(|e| CompileError::LlvmError(format!("method cmp: {e}")))?;
                            return Ok(BasicValueEnum::IntValue(result));
                        }
                        if matches!(
                            name,
                            "builtin.method.option.unwrap_or" | "builtin.method.result.unwrap_or"
                        ) {
                            // unwrap_or(default): if disc!=0 return payload, else default.
                            let recv = self.emit_expr(&call.arguments[0].value, frame)?;
                            let default_val = self.emit_expr(&call.arguments[1].value, frame)?;
                            let sv = match recv {
                                BasicValueEnum::StructValue(sv) => sv,
                                BasicValueEnum::PointerValue(pv) => {
                                    let sty = self.lower_type(&call.arguments[0].value.ty)?;
                                    self.generator
                                        .build_load(sty, pv, "unwrap_recv")?
                                        .into_struct_value()
                                }
                                _ => {
                                    return Err(CompileError::Unsupported(
                                        "unwrap_or on non-struct receiver".into(),
                                    ))
                                }
                            };
                            let disc = self
                                .generator
                                .builder
                                .build_extract_value(sv, 0, "unwrap_disc")
                                .map_err(|e| CompileError::LlvmError(format!("unwrap disc: {e}")))?
                                .into_int_value();
                            let payload = self
                                .generator
                                .builder
                                .build_extract_value(sv, 1, "unwrap_payload")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("unwrap payload: {e}"))
                                })?;
                            let zero = disc.get_type().const_int(0, false);
                            let has_val = self
                                .generator
                                .builder
                                .build_int_compare(
                                    inkwell::IntPredicate::NE,
                                    disc,
                                    zero,
                                    "unwrap_has",
                                )
                                .map_err(|e| CompileError::LlvmError(format!("unwrap cmp: {e}")))?;
                            let target_ty = self.lower_type(&expression.ty)?;
                            let payload = self.coerce_to(payload, target_ty)?;
                            let default_val = self.coerce_to(default_val, target_ty)?;
                            let result = self
                                .generator
                                .builder
                                .build_select(has_val, payload, default_val, "unwrap_result")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("unwrap select: {e}"))
                                })?;
                            return Ok(result);
                        }
                        // 0.1.9 Phase G (0.39.126): panicking `unwrap` on
                        // Result/Option — mirror legacy compile_unwrap_expect:
                        // Some/Ok → payload; None/Err → mimi_try_exit(0)
                        // (noreturn) → unreachable.
                        // 0.39.x matrix sweep: `expect(msg)` shares the same
                        // shape (the msg only affects stderr text, which is
                        // outside the L1 stdout contract).
                        if matches!(
                            name,
                            "builtin.method.result.unwrap"
                                | "builtin.method.option.unwrap"
                                | "builtin.method.result.expect"
                                | "builtin.method.option.expect"
                        ) {
                            let recv = self.emit_expr(&call.arguments[0].value, frame)?;
                            let sv = match recv {
                                BasicValueEnum::StructValue(sv) => sv,
                                BasicValueEnum::PointerValue(pv) => {
                                    let sty = self.lower_type(&call.arguments[0].value.ty)?;
                                    self.generator
                                        .build_load(sty, pv, "unwrap_recv")?
                                        .into_struct_value()
                                }
                                _ => {
                                    return Err(CompileError::Unsupported(
                                        "unwrap on non-struct receiver".into(),
                                    ))
                                }
                            };
                            let disc = self
                                .generator
                                .builder
                                .build_extract_value(sv, 0, "unwrap_disc")
                                .map_err(|e| CompileError::LlvmError(format!("unwrap disc: {e}")))?
                                .into_int_value();
                            let payload = self
                                .generator
                                .builder
                                .build_extract_value(sv, 1, "unwrap_payload")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("unwrap payload: {e}"))
                                })?;
                            let zero = disc.get_type().const_int(0, false);
                            let has_val = self
                                .generator
                                .builder
                                .build_int_compare(
                                    inkwell::IntPredicate::NE,
                                    disc,
                                    zero,
                                    "unwrap_has",
                                )
                                .map_err(|e| CompileError::LlvmError(format!("unwrap cmp: {e}")))?;
                            let function = self.current_function()?;
                            let ok_bb = self
                                .generator
                                .context
                                .append_basic_block(function, "unwrap_ok");
                            let trap_bb = self
                                .generator
                                .context
                                .append_basic_block(function, "unwrap_trap");
                            self.generator.build_cond_br(has_val, ok_bb, trap_bb)?;
                            // None/Err path: mimi_try_exit(0) → unreachable.
                            self.generator.builder.position_at_end(trap_bb);
                            let try_exit_fn = self.generator.get_runtime_fn("mimi_try_exit")?;
                            self.generator
                                .builder
                                .build_call(
                                    try_exit_fn,
                                    &[inkwell::values::BasicMetadataValueEnum::IntValue(
                                        self.generator.context.i64_type().const_zero(),
                                    )],
                                    "unwrap_trap",
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("unwrap trap call: {e}"))
                                })?;
                            self.generator.builder.build_unreachable().map_err(|e| {
                                CompileError::LlvmError(format!("unwrap unreachable: {e}"))
                            })?;
                            // Some/Ok path: recover payload.
                            self.generator.builder.position_at_end(ok_bb);
                            let target_ty = self.lower_type(&expression.ty)?;
                            return Ok(self.coerce_to(payload, target_ty)?);
                        }
                        // 0.39.x / BUG N: `Option.map` / `Option.map_err` in
                        // resolved-forced contexts (spawn/await results, Flow
                        // payloads) had no resolved-native emitter and
                        // hard-errored E0722 ("has no resolved-native emitter").
                        // VM semantics (src/interp/bytecode/vm.rs ~3140):
                        //   Some.map(f) → Some(f(payload)); None.map(_) → None
                        //   Option.map_err(_) → receiver unchanged (Option has no
                        //   err slot, so there is nothing to transform).
                        // Layout: Option = {i1 disc, i64 payload}. Mirrors the
                        // Result.map handler below.
                        if matches!(
                            name,
                            "builtin.method.option.map" | "builtin.method.option.map_err"
                        ) {
                            let is_map = name.ends_with(".map");
                            let recv = self.emit_expr(&call.arguments[0].value, frame)?;
                            let sv = match recv {
                                BasicValueEnum::StructValue(sv) => sv,
                                BasicValueEnum::PointerValue(pv) => {
                                    let sty = self.lower_type(&call.arguments[0].value.ty)?;
                                    self.generator
                                        .build_load(sty, pv, "opt_map_recv")?
                                        .into_struct_value()
                                }
                                _ => {
                                    return Err(CompileError::Unsupported(
                                        "Option map on non-struct receiver".into(),
                                    ))
                                }
                            };
                            // Option.map_err on an Option is a no-op pass-through.
                            if !is_map {
                                return Ok(BasicValueEnum::StructValue(sv));
                            }
                            let f_val = self.emit_expr(&call.arguments[1].value, frame)?;
                            let f_sv = match f_val {
                                BasicValueEnum::StructValue(sv) => sv,
                                _ => {
                                    return Err(CompileError::Unsupported(
                                        "Option map closure must be a closure struct".into(),
                                    ))
                                }
                            };
                            let disc = self
                                .generator
                                .builder
                                .build_extract_value(sv, 0, "opt_map_disc")
                                .map_err(|e| CompileError::LlvmError(format!("opt map disc: {e}")))?
                                .into_int_value();
                            let payload = self
                                .generator
                                .builder
                                .build_extract_value(sv, 1, "opt_map_payload")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("opt map payload: {e}"))
                                })?;
                            let ret_slot_ty = {
                                let resolved_result_ty = self
                                    .program
                                    .resolved_types()
                                    .get(&call.result)
                                    .cloned()
                                    .ok_or_else(|| {
                                        CompileError::Unsupported(
                                            "Option map: missing result type".into(),
                                        )
                                    })?;
                                // Option is represented as `ResolvedType::Option(
                                // inner)` (canonical after resolved lowering, e.g.
                                // for spawn/await results) or `ResolvedType::
                                // Nominal` with one type argument. (BUG M sibling:
                                // only the Nominal form was previously accepted.)
                                match resolved_result_ty {
                                    ResolvedType::Option(inner) => inner,
                                    ResolvedType::Nominal { arguments: ta, .. } => {
                                        ta.first().cloned().ok_or_else(|| {
                                            CompileError::Unsupported(
                                                "Option map: missing type argument".into(),
                                            )
                                        })?
                                    }
                                    _ => {
                                        return Err(CompileError::Unsupported(
                                            "Option map: result type not an Option".into(),
                                        ))
                                    }
                                }
                            };
                            let ret_slot_llvm = self.lower_type(&ret_slot_ty)?;
                            let ptr_ty = self
                                .generator
                                .context
                                .ptr_type(inkwell::AddressSpace::default());
                            let fn_ptr = self
                                .generator
                                .builder
                                .build_extract_value(f_sv, 0, "opt_map_fn_ptr")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("opt map fn ptr: {e}"))
                                })?
                                .into_pointer_value();
                            let env_ptr = self
                                .generator
                                .builder
                                .build_extract_value(f_sv, 1, "opt_map_env_ptr")
                                .map_err(|e| CompileError::LlvmError(format!("opt map env: {e}")))?
                                .into_pointer_value();
                            let arg_meta: BasicMetadataTypeEnum = match ret_slot_llvm {
                                BasicTypeEnum::IntType(t) => t.into(),
                                BasicTypeEnum::FloatType(t) => t.into(),
                                BasicTypeEnum::PointerType(t) => t.into(),
                                BasicTypeEnum::StructType(t) => t.into(),
                                _ => self.generator.context.i64_type().into(),
                            };
                            // Slot materialization: scalars sit in the i64 slot
                            // directly; NON-scalar payloads are stored as a HEAP
                            // POINTER truncated into the i64 slot. The closure
                            // expects the real value, so rebuild it per type.
                            let materialize_slot = |slot: BasicValueEnum<'ctx>,
                                                    target: BasicTypeEnum<'ctx>|
                             -> Result<
                                BasicValueEnum<'ctx>,
                                CompileError,
                            > {
                                let b = &self.generator.builder;
                                let raw_i64 = match slot {
                                    BasicValueEnum::IntValue(iv) => iv,
                                    _ => self.generator.context.i64_type().const_zero(),
                                };
                                match target {
                                    BasicTypeEnum::IntType(t) => {
                                        if raw_i64.get_type().get_bit_width() > t.get_bit_width() {
                                            Ok(b.build_int_truncate(raw_i64, t, "slot_trunc")
                                                .map_err(|e| {
                                                    CompileError::LlvmError(format!(
                                                        "slot trunc: {e}"
                                                    ))
                                                })?
                                                .into())
                                        } else if raw_i64.get_type().get_bit_width()
                                            < t.get_bit_width()
                                        {
                                            Ok(b.build_int_z_extend(raw_i64, t, "slot_zext")
                                                .map_err(|e| {
                                                    CompileError::LlvmError(format!(
                                                        "slot zext: {e}"
                                                    ))
                                                })?
                                                .into())
                                        } else {
                                            Ok(BasicValueEnum::IntValue(raw_i64))
                                        }
                                    }
                                    BasicTypeEnum::PointerType(t) => {
                                        // Inline pointer slot: pass through.
                                        // Otherwise the slot is a boxed i64 pointer.
                                        if let BasicValueEnum::PointerValue(pv) = slot {
                                            Ok(pv.into())
                                        } else {
                                            Ok(b.build_int_to_ptr(raw_i64, t, "slot_int2ptr")
                                                .map_err(|e| {
                                                    CompileError::LlvmError(format!(
                                                        "slot i2p: {e}"
                                                    ))
                                                })?
                                                .into())
                                        }
                                    }
                                    BasicTypeEnum::StructType(t) => {
                                        // Resolved stores non-scalar slots two ways:
                                        // inline (the slot already is the {ptr,len}
                                        // struct) or boxed (slot = ptrtoint(box)).
                                        // Pass an inline struct through; otherwise
                                        // reconstruct it from the box.
                                        if let BasicValueEnum::StructValue(sv) = slot {
                                            Ok(sv.into())
                                        } else {
                                            let raw_ptr = self
                                                .generator
                                                .context
                                                .ptr_type(inkwell::AddressSpace::default());
                                            let ptr = b
                                                .build_int_to_ptr(
                                                    raw_i64,
                                                    raw_ptr,
                                                    "slot_boxed_ptr",
                                                )
                                                .map_err(|e| {
                                                    CompileError::LlvmError(format!(
                                                        "slot box ptr: {e}"
                                                    ))
                                                })?;
                                            Ok(self.generator.build_load(
                                                t,
                                                ptr,
                                                "slot_boxed_load",
                                            )?)
                                        }
                                    }
                                    _ => Err(CompileError::Unsupported(
                                        "Option map on float-typed slot".into(),
                                    )),
                                }
                            };
                            let indirect_fn_ty = match ret_slot_llvm {
                                BasicTypeEnum::IntType(t) => {
                                    t.fn_type(&[ptr_ty.into(), arg_meta], false)
                                }
                                BasicTypeEnum::FloatType(t) => {
                                    t.fn_type(&[ptr_ty.into(), arg_meta], false)
                                }
                                BasicTypeEnum::PointerType(t) => {
                                    t.fn_type(&[ptr_ty.into(), arg_meta], false)
                                }
                                BasicTypeEnum::StructType(t) => {
                                    t.fn_type(&[ptr_ty.into(), arg_meta], false)
                                }
                                _ => self
                                    .generator
                                    .context
                                    .i64_type()
                                    .fn_type(&[ptr_ty.into(), arg_meta], false),
                            };
                            let function = self.current_function()?;
                            let has_bb = self
                                .generator
                                .context
                                .append_basic_block(function, "opt_map_has");
                            let passthrough_bb = self
                                .generator
                                .context
                                .append_basic_block(function, "opt_map_passthrough");
                            let cont_bb = self
                                .generator
                                .context
                                .append_basic_block(function, "opt_map_cont");
                            let zero = disc.get_type().const_int(0, false);
                            let has_val = self
                                .generator
                                .builder
                                .build_int_compare(
                                    inkwell::IntPredicate::NE,
                                    disc,
                                    zero,
                                    "opt_map_has_val",
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("opt map cmp: {e}"))
                                })?;
                            self.generator
                                .build_cond_br(has_val, has_bb, passthrough_bb)?;
                            // Transform branch: new_slot = f(env, src_slot).
                            self.generator.builder.position_at_end(has_bb);
                            let materialized = materialize_slot(payload, ret_slot_llvm)?;
                            let call_args: Vec<BasicMetadataValueEnum> = vec![
                                BasicMetadataValueEnum::PointerValue(env_ptr),
                                match materialized {
                                    BasicValueEnum::IntValue(iv) => {
                                        BasicMetadataValueEnum::IntValue(iv)
                                    }
                                    BasicValueEnum::FloatValue(fv) => {
                                        BasicMetadataValueEnum::FloatValue(fv)
                                    }
                                    BasicValueEnum::PointerValue(pv) => {
                                        BasicMetadataValueEnum::PointerValue(pv)
                                    }
                                    BasicValueEnum::StructValue(svs) => {
                                        BasicMetadataValueEnum::StructValue(svs)
                                    }
                                    other => other.into(),
                                },
                            ];
                            let transformed = self
                                .generator
                                .builder
                                .build_indirect_call(
                                    indirect_fn_ty,
                                    fn_ptr,
                                    &call_args,
                                    "opt_map_closure_call",
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("opt map closure call: {e}"))
                                })?
                                .try_as_basic_value_opt()
                                .ok_or_else(|| {
                                    CompileError::Unsupported(
                                        "closure in option map returned void".into(),
                                    )
                                })?;
                            let transformed = self.coerce_to(transformed, ret_slot_llvm)?;
                            // Coerce the native closure return into the i64 slot
                            // type (mirrors BUG M fix for Result): the Option
                            // struct stores payload as i64 regardless of the
                            // native Ok shape.
                            let slot_llvm_ty = payload.get_type();
                            // Option payload is stored inline in its natural lowered
                            // type; the closure already produced that type, so coerce
                            // (a no-op for matching shapes; widens scalar i32 -> i64).
                            let transformed_slot: BasicValueEnum<'ctx> =
                                self.coerce_to(transformed, slot_llvm_ty)?;
                            self.generator
                                .builder
                                .build_unconditional_branch(cont_bb)
                                .map_err(|e| CompileError::LlvmError(format!("opt map br: {e}")))?;
                            // Passthrough branch: keep the original payload.
                            self.generator.builder.position_at_end(passthrough_bb);
                            self.generator
                                .builder
                                .build_unconditional_branch(cont_bb)
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("opt map br2: {e}"))
                                })?;
                            // Merge: rebuild the Option struct.
                            self.generator.builder.position_at_end(cont_bb);
                            let struct_ty = sv.get_type();
                            let phi_slot = self
                                .generator
                                .builder
                                .build_phi(slot_llvm_ty, "opt_map_slot_phi")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("opt map phi: {e}"))
                                })?;
                            let transformed_basic: BasicValueEnum<'ctx> = transformed_slot;
                            phi_slot.add_incoming(&[
                                (&transformed_basic, has_bb),
                                (&payload, passthrough_bb),
                            ]);
                            let mut rebuilt = struct_ty.get_undef();
                            rebuilt = self
                                .generator
                                .builder
                                .build_insert_value(
                                    rebuilt,
                                    disc.get_type().const_int(1, false),
                                    0,
                                    "opt_map_disc_out",
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("opt map disc out: {e}"))
                                })?
                                .into_struct_value();
                            rebuilt = self
                                .generator
                                .builder
                                .build_insert_value(
                                    rebuilt,
                                    phi_slot.as_basic_value(),
                                    1,
                                    "opt_map_payload_out",
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("opt map payload out: {e}"))
                                })?
                                .into_struct_value();
                            return Ok(BasicValueEnum::StructValue(rebuilt));
                        }
                        // 0.39.x matrix sweep: `map` / `map_err` on builtin
                        // Result — the stdlib impl bodies delegate by calling
                        // these faces (`self.map(f)`), so once lowering routed
                        // builtin faces first they arrive HERE and must have
                        // real emitters (the pre-sweep code produced an
                        // infinitely self-recursive trampoline instead).
                        // Layout: Result = {i1 disc, i64 payload, i64 err}.
                        if matches!(
                            name,
                            "builtin.method.result.map" | "builtin.method.result.map_err"
                        ) {
                            let is_map = name.ends_with(".map");
                            let recv = self.emit_expr(&call.arguments[0].value, frame)?;
                            let sv = match recv {
                                BasicValueEnum::StructValue(sv) => sv,
                                BasicValueEnum::PointerValue(pv) => {
                                    let sty = self.lower_type(&call.arguments[0].value.ty)?;
                                    self.generator
                                        .build_load(sty, pv, "map_recv")?
                                        .into_struct_value()
                                }
                                _ => {
                                    return Err(CompileError::Unsupported(
                                        "Result map/map_err on non-struct receiver".into(),
                                    ))
                                }
                            };
                            let f_val = self.emit_expr(&call.arguments[1].value, frame)?;
                            let f_sv = match f_val {
                                BasicValueEnum::StructValue(sv) => sv,
                                _ => {
                                    return Err(CompileError::Unsupported(
                                        "Result map/map_err closure must be a closure struct"
                                            .into(),
                                    ))
                                }
                            };
                            let disc = self
                                .generator
                                .builder
                                .build_extract_value(sv, 0, "map_disc")
                                .map_err(|e| CompileError::LlvmError(format!("map disc: {e}")))?
                                .into_int_value();
                            let payload = self
                                .generator
                                .builder
                                .build_extract_value(sv, 1, "map_payload")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("map payload: {e}"))
                                })?;
                            let err_slot = self
                                .generator
                                .builder
                                .build_extract_value(sv, 2, "map_err_slot")
                                .map_err(|e| CompileError::LlvmError(format!("map err: {e}")))?;
                            // Closure invocation: fn(env, value) -> ret. The
                            // transformed slot type comes from the call's
                            // result type arguments: map → U = args[0],
                            // map_err → F = args[1] of Result<T, E>.
                            // The closure `f` carries its own true parameter and
                            // result types. Derive the call ABI from THERE, not
                            // from the Result's error type argument: the latter
                            // is stored as `i64` (the err-slot representation), so
                            // using it would force a wrong i64 ABI onto a
                            // `{ptr, i64}` string closure and corrupt the call
                            // (BUG O). The closure param/result already encode
                            // the correct direction (Ok->U for map, E->F for
                            // map_err), so we read them directly.
                            let closure_ty_id = call.arguments[1].value.ty.clone();
                            let (arg_ty, ret_ty) =
                                match self.program.resolved_types().get(&closure_ty_id) {
                                    Some(ResolvedType::Function {
                                        parameters, result, ..
                                    }) => {
                                        let arg = parameters.first().cloned().ok_or_else(|| {
                                            CompileError::Unsupported(
                                                "Result map/map_err closure has no parameter"
                                                    .into(),
                                            )
                                        })?;
                                        (arg, result.clone())
                                    }
                                    _ => {
                                        return Err(CompileError::Unsupported(
                                            "Result map/map_err closure type is not a Function"
                                                .into(),
                                        ))
                                    }
                                };
                            let arg_slot_llvm = self.lower_type(&arg_ty)?;
                            // The closure's *return* LLVM type is the true result of
                            // the transform: for `map` it is `U` (which may differ from
                            // the parameter type `T`), for `map_err` it is `F` (which may
                            // differ from the error parameter `E`). Derive the indirect
                            // call's return type from the closure's actual result type so
                            // the ABI matches the real callee and the rebuilt Result slot
                            // has the correct shape. Deriving it from the *parameter* type
                            // only happened to work when `U == T` / `F == E` and silently
                            // corrupted every `map` whose Ok payload type changed — BUG O.
                            let ret_slot_llvm = self.lower_type(&ret_ty)?;
                            let ptr_ty = self
                                .generator
                                .context
                                .ptr_type(inkwell::AddressSpace::default());
                            let fn_ptr = self
                                .generator
                                .builder
                                .build_extract_value(f_sv, 0, "map_fn_ptr")
                                .map_err(|e| CompileError::LlvmError(format!("map fn ptr: {e}")))?
                                .into_pointer_value();
                            let env_ptr = self
                                .generator
                                .builder
                                .build_extract_value(f_sv, 1, "map_env_ptr")
                                .map_err(|e| CompileError::LlvmError(format!("map env: {e}")))?
                                .into_pointer_value();
                            let src_slot = if is_map { &payload } else { &err_slot };
                            let arg_meta: BasicMetadataTypeEnum = match arg_slot_llvm {
                                BasicTypeEnum::IntType(t) => t.into(),
                                BasicTypeEnum::FloatType(t) => t.into(),
                                BasicTypeEnum::PointerType(t) => t.into(),
                                BasicTypeEnum::StructType(t) => t.into(),
                                _ => self.generator.context.i64_type().into(),
                            };
                            // Slot materialization: builtin Result stores a
                            // NON-scalar payload/error as a HEAP POINTER
                            // truncated into the i64 slot (Err ctor emits
                            // `ptrtoint malloc-ptr`), while scalars sit in
                            // the slot directly. The closure expects the real
                            // value, so rebuild it per the declared type.
                            let materialize_slot = |slot: BasicValueEnum<'ctx>,
                                                    target: BasicTypeEnum<'ctx>|
                             -> Result<
                                BasicValueEnum<'ctx>,
                                CompileError,
                            > {
                                let b = &self.generator.builder;
                                let raw_i64 = match slot {
                                    BasicValueEnum::IntValue(iv) => iv,
                                    _ => self.generator.context.i64_type().const_zero(),
                                };
                                match target {
                                    BasicTypeEnum::IntType(t) => {
                                        if raw_i64.get_type().get_bit_width() > t.get_bit_width() {
                                            Ok(b.build_int_truncate(raw_i64, t, "slot_trunc")
                                                .map_err(|e| {
                                                    CompileError::LlvmError(format!(
                                                        "slot trunc: {e}"
                                                    ))
                                                })?
                                                .into())
                                        } else if raw_i64.get_type().get_bit_width()
                                            < t.get_bit_width()
                                        {
                                            Ok(b.build_int_z_extend(raw_i64, t, "slot_zext")
                                                .map_err(|e| {
                                                    CompileError::LlvmError(format!(
                                                        "slot zext: {e}"
                                                    ))
                                                })?
                                                .into())
                                        } else {
                                            Ok(BasicValueEnum::IntValue(raw_i64))
                                        }
                                    }
                                    BasicTypeEnum::PointerType(t) => {
                                        // Inline pointer slot: pass through.
                                        // Otherwise the slot is a boxed i64 pointer.
                                        if let BasicValueEnum::PointerValue(pv) = slot {
                                            Ok(pv.into())
                                        } else {
                                            Ok(b.build_int_to_ptr(raw_i64, t, "slot_int2ptr")
                                                .map_err(|e| {
                                                    CompileError::LlvmError(format!(
                                                        "slot i2p: {e}"
                                                    ))
                                                })?
                                                .into())
                                        }
                                    }
                                    BasicTypeEnum::StructType(t) => {
                                        // Resolved stores non-scalar slots two ways:
                                        // inline (the slot already is the {ptr,len}
                                        // struct) or boxed (slot = ptrtoint(box)).
                                        // Pass an inline struct through; otherwise
                                        // reconstruct it from the box.
                                        if let BasicValueEnum::StructValue(sv) = slot {
                                            Ok(sv.into())
                                        } else {
                                            let raw_ptr = self
                                                .generator
                                                .context
                                                .ptr_type(inkwell::AddressSpace::default());
                                            let ptr = b
                                                .build_int_to_ptr(
                                                    raw_i64,
                                                    raw_ptr,
                                                    "slot_boxed_ptr",
                                                )
                                                .map_err(|e| {
                                                    CompileError::LlvmError(format!(
                                                        "slot box ptr: {e}"
                                                    ))
                                                })?;
                                            Ok(self.generator.build_load(
                                                t,
                                                ptr,
                                                "slot_boxed_load",
                                            )?)
                                        }
                                    }
                                    _ => Err(CompileError::Unsupported(
                                        "Result map/map_err on float-typed slot".into(),
                                    )),
                                }
                            };
                            let indirect_fn_ty = match ret_slot_llvm {
                                BasicTypeEnum::IntType(t) => {
                                    t.fn_type(&[ptr_ty.into(), arg_meta], false)
                                }
                                BasicTypeEnum::FloatType(t) => {
                                    t.fn_type(&[ptr_ty.into(), arg_meta], false)
                                }
                                BasicTypeEnum::PointerType(t) => {
                                    t.fn_type(&[ptr_ty.into(), arg_meta], false)
                                }
                                BasicTypeEnum::StructType(t) => {
                                    t.fn_type(&[ptr_ty.into(), arg_meta], false)
                                }
                                _ => self
                                    .generator
                                    .context
                                    .i64_type()
                                    .fn_type(&[ptr_ty.into(), arg_meta], false),
                            };
                            let function = self.current_function()?;
                            let has_bb = self
                                .generator
                                .context
                                .append_basic_block(function, "map_has");
                            let passthrough_bb = self
                                .generator
                                .context
                                .append_basic_block(function, "map_passthrough");
                            let cont_bb = self
                                .generator
                                .context
                                .append_basic_block(function, "map_cont");
                            let is_positive = if is_map { true } else { false };
                            let pred = if is_positive {
                                inkwell::IntPredicate::NE
                            } else {
                                inkwell::IntPredicate::EQ
                            };
                            let zero = disc.get_type().const_int(0, false);
                            let applies = self
                                .generator
                                .builder
                                .build_int_compare(pred, disc, zero, "map_applies")
                                .map_err(|e| CompileError::LlvmError(format!("map cmp: {e}")))?;
                            self.generator
                                .build_cond_br(applies, has_bb, passthrough_bb)?;
                            // Transform branch: new_slot = f(env, src_slot).
                            self.generator.builder.position_at_end(has_bb);
                            let materialized = materialize_slot(*src_slot, arg_slot_llvm)?;
                            let call_args: Vec<BasicMetadataValueEnum> = vec![
                                BasicMetadataValueEnum::PointerValue(env_ptr),
                                match materialized {
                                    BasicValueEnum::IntValue(iv) => {
                                        BasicMetadataValueEnum::IntValue(iv)
                                    }
                                    BasicValueEnum::FloatValue(fv) => {
                                        BasicMetadataValueEnum::FloatValue(fv)
                                    }
                                    BasicValueEnum::PointerValue(pv) => {
                                        BasicMetadataValueEnum::PointerValue(pv)
                                    }
                                    BasicValueEnum::StructValue(svs) => {
                                        BasicMetadataValueEnum::StructValue(svs)
                                    }
                                    other => other.into(),
                                },
                            ];
                            let transformed = self
                                .generator
                                .builder
                                .build_indirect_call(
                                    indirect_fn_ty,
                                    fn_ptr,
                                    &call_args,
                                    "map_closure_call",
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("map closure call: {e}"))
                                })?
                                .try_as_basic_value_opt()
                                .ok_or_else(|| {
                                    CompileError::Unsupported(
                                        "closure in map/map_err returned void".into(),
                                    )
                                })?;
                            let transformed = self.coerce_to(transformed, ret_slot_llvm)?;
                            // BUG M (cont.): the Result struct stores its
                            // payload/error slots as i64 regardless of the native
                            // `ret_slot_llvm` shape (Result<i32, _> has an i32 Ok
                            // materialized from an i64 slot; Result<string, _>
                            // stores a pointer as i64). Coerce the closure's native
                            // return value into the i64 slot type so the merge phi
                            // and `insertvalue` stay type-consistent with the
                            // {…, i64, i64} struct — otherwise LLVM verification
                            // fails with a type-mismatched insertvalue. The original
                            // handler only worked when ret_slot_llvm already lowered
                            // to i64 (e.g. Result<i64, _>).
                            // The transformed slot's LLVM type is the *new* Result slot
                            // type: for `map` the Ok payload becomes `U` (the closure's
                            // return type, which may differ from the receiver's `T`); for
                            // `map_err` the err slot is always an `i64` handle (resolved
                            // always yields i64). Using the receiver's slot type here
                            // produced a phi/insertvalue whose field shape didn't match
                            // `Result<U, E>` / `Result<T, F>` and made LLVM verification
                            // fail — BUG O.
                            let slot_llvm_ty: BasicTypeEnum<'ctx> = if is_map {
                                self.lower_type(&ret_ty)?
                            } else {
                                src_slot.get_type()
                            };
                            let transformed_slot: BasicValueEnum<'ctx> = if is_map {
                                // Ok/payload slot is stored inline in its natural
                                // lowered type; the closure already produced that
                                // type, so coerce (a no-op for matching shapes).
                                self.coerce_to(transformed, slot_llvm_ty)?
                            } else {
                                // Err slot is always an i64 handle: scalars widen,
                                // aggregates (e.g. string) heap-box via the shared
                                // err->handle helper used by `Err(..)` and `?`.
                                self.resolved_err_to_handle(transformed)?
                            };
                            // The value above may be defined in a block different
                            // from `has_bb`: `resolved_err_to_handle` opens its own
                            // `err_str_malloc_ok` block for aggregates, leaving the
                            // builder there. The merge phi must attribute the value
                            // to its TRUE defining block, otherwise LLVM rejects the
                            // phi (incoming block is not a predecessor of `cont_bb`)
                            // and the whole function silently falls back to the
                            // legacy emitter (which mis-decodes string errors).
                            let transformed_bb =
                                self.generator.builder.get_insert_block().ok_or_else(|| {
                                    CompileError::LlvmError(
                                        "map_err: missing insert block after transform".into(),
                                    )
                                })?;
                            self.generator
                                .builder
                                .build_unconditional_branch(cont_bb)
                                .map_err(|e| CompileError::LlvmError(format!("map br: {e}")))?;
                            // Passthrough branch: keep the original slots.
                            self.generator.builder.position_at_end(passthrough_bb);
                            self.generator
                                .builder
                                .build_unconditional_branch(cont_bb)
                                .map_err(|e| CompileError::LlvmError(format!("map br2: {e}")))?;
                            // Merge: rebuild the Result struct.
                            self.generator.builder.position_at_end(cont_bb);
                            // Rebuild with the *result* Result type's struct layout, not
                            // the receiver's: `map` changes the Ok payload type (T -> U)
                            // and `map_err` the error type (E -> F). Using the receiver's
                            // struct here produced a struct whose field shape didn't match
                            // `Result<U, E>` / `Result<T, F>`, so the caller's `coerce_to`
                            // failed and the whole function fell back to the legacy emitter
                            // (which mis-decodes string errors) — BUG O. The call's result
                            // type carries the correct layout.
                            let struct_ty_basic: BasicTypeEnum<'ctx> =
                                self.lower_type(&call.result)?;
                            let struct_ty: StructType<'ctx> = match struct_ty_basic {
                                BasicTypeEnum::StructType(st) => st,
                                _ => {
                                    return Err(CompileError::Unsupported(
                                        "Result map/map_err result type is not a struct".into(),
                                    ))
                                }
                            };
                            let phi_slot = self
                                .generator
                                .builder
                                .build_phi(slot_llvm_ty, "map_slot_phi")
                                .map_err(|e| CompileError::LlvmError(format!("map phi: {e}")))?;
                            let transformed_basic: BasicValueEnum<'ctx> = transformed_slot;
                            // Passthrough incoming: for `map_err` the Ok field is
                            // unchanged (`payload`); for `map` the Ok field changes type
                            // `T -> U`, but on the Err path it is never read, so supply an
                            // `undef` of the new slot type to keep the phi type-consistent
                            // (inserting the original `i64` payload into a `{ptr, i64}`
                            // field would fail LLVM verification).
                            let passthrough_val: BasicValueEnum<'ctx> = if is_map {
                                // On the Err path the Ok field is never read, but it
                                // must have the new slot type `U` to satisfy the phi.
                                match slot_llvm_ty {
                                    BasicTypeEnum::StructType(st) => st.get_undef().into(),
                                    BasicTypeEnum::IntType(it) => it.get_undef().into(),
                                    BasicTypeEnum::FloatType(ft) => ft.get_undef().into(),
                                    BasicTypeEnum::PointerType(pt) => pt.get_undef().into(),
                                    other => other.const_zero(),
                                }
                            } else {
                                err_slot.into()
                            };
                            phi_slot.add_incoming(&[
                                (&transformed_basic, transformed_bb),
                                (&passthrough_val, passthrough_bb),
                            ]);
                            let mut rebuilt = struct_ty.get_undef();
                            rebuilt = self
                                .generator
                                .builder
                                .build_insert_value(
                                    rebuilt,
                                    // The discriminant tag is never changed by map/map_err:
                                    // only the Ok payload (map) or the Err value (map_err) is
                                    // transformed, so the original tag must be preserved.
                                    // Hardcoding `1`/`0` here silently flips an `Ok` produced
                                    // by `map_err` (or an `Err` produced by `map`) into the
                                    // opposite variant and corrupts the downstream match.
                                    disc,
                                    0,
                                    "map_disc_out",
                                )
                                .map_err(|e| CompileError::LlvmError(format!("map disc out: {e}")))?
                                .into_struct_value();
                            rebuilt = self
                                .generator
                                .builder
                                .build_insert_value(
                                    rebuilt,
                                    if is_map {
                                        phi_slot.as_basic_value()
                                    } else {
                                        payload
                                    },
                                    1,
                                    "map_payload_out",
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("map payload out: {e}"))
                                })?
                                .into_struct_value();
                            rebuilt = self
                                .generator
                                .builder
                                .build_insert_value(
                                    rebuilt,
                                    if is_map {
                                        err_slot
                                    } else {
                                        phi_slot.as_basic_value()
                                    },
                                    2,
                                    "map_err_out",
                                )
                                .map_err(|e| CompileError::LlvmError(format!("map err out: {e}")))?
                                .into_struct_value();
                            return Ok(BasicValueEnum::StructValue(rebuilt));
                        }
                        // Print-family builtins need arg type hints for formatting dispatch.
                        if matches!(name, "println" | "print" | "eprintln" | "format") {
                            self.generator.pending_print_arg_types = call
                                .arguments
                                .iter()
                                .map(|arg| resolved_type_display_name(self.program, &arg.value.ty))
                                .collect();
                        }
                        // 0.35.23 deep-eval: `len(string)` must pick strlen, not
                        // the list-length helper. The legacy call-site setter
                        // (expr/call/simple.rs) does this via expr_is_string;
                        // the resolved emitter has the canonical type at hand.
                        if (name == "len" || name == "is_empty") && call.arguments.len() == 1 {
                            let arg_display = resolved_type_display_name(
                                self.program,
                                &call.arguments[0].value.ty,
                            );
                            self.generator.pending_len_is_string = arg_display == "string";
                            // is_empty: map and set both lower to bare i64
                            // handles — the canonical arg type disambiguates
                            // them for compile_is_empty.
                            self.generator.pending_is_empty_kind = if name == "is_empty" {
                                classify_is_empty_kind(&arg_display)
                            } else {
                                None
                            };
                        }
                        // 0.32.18: Builtin/module-function name shadowing.
                        // The checker may resolve a call to a module function
                        // (e.g. `contains` from std::strings) as
                        // ResolvedCallee::Builtin when a builtin with the same
                        // name exists. If a user-defined function with this
                        // name has been forward-declared in the LLVM module,
                        // call it directly instead of delegating to
                        // compile_builtin_call (which would dispatch to the
                        // wrong builtin implementation, e.g. list-contains
                        // instead of string-contains → SIGSEGV).
                        if let Some(shadow_fn) = self.generator.module.get_function(name) {
                            // Only shadow if the function is user-defined
                            // (has a body or is forward-declared from a
                            // module file). Runtime builtins like printf
                            // are also in the module but should NOT be
                            // shadowed — they don't conflict with module
                            // functions.
                            let is_user_decl = self.generator.func_defs.contains_key(name);
                            if is_user_decl {
                                // 0.39.x matrix sweep (GENERIC-SHADOW-MONO-001):
                                // a GENERIC stdlib free function whose name
                                // shadows a builtin (reduce_list over the builtin
                                // reduce family) arrives here with
                                // ResolvedCallee::Builtin — the Function-arm
                                // monomorphization below never runs, so the call
                                // went to the i64-fallback SKELETON: f64
                                // accumulators were bitwise-i64-added
                                // (`sum_float([2.5,1.5])` printed the bit pattern
                                // 4616189618054758000). Monomorphize on demand
                                // and retarget to the mangled instance, exactly
                                // like the Function arm.
                                if let Some(fdef) = self.generator.func_defs.get(name).cloned() {
                                    let mut ast_map: std::collections::HashMap<
                                        String,
                                        crate::ast::Type,
                                    >;
                                    if !call.type_arguments.is_empty() {
                                        ast_map = resolved_type_args_to_ast(
                                            &fdef.generics,
                                            &call.type_arguments,
                                            self.program.resolved_types(),
                                        );
                                    } else {
                                        // Inferred (non-turbofish) call: recover the
                                        // bindings structurally from argument types.
                                        let argument_types: Vec<crate::core::ResolvedTypeId> = call
                                            .arguments
                                            .iter()
                                            .map(|a| a.value.ty.clone())
                                            .collect();
                                        let recovered = infer_type_args_from_call_site(
                                            &fdef,
                                            &argument_types,
                                            self.program.resolved_types(),
                                            resolved_type_to_ast,
                                        );
                                        // Require EVERY generic to be recovered; a
                                        // partial map would mis-instantiate.
                                        let complete = !fdef.generics.is_empty()
                                            && fdef
                                                .generics
                                                .iter()
                                                .all(|g| recovered.contains_key(&g.name));
                                        ast_map = if complete {
                                            recovered
                                        } else {
                                            Default::default()
                                        };
                                    }
                                    if !fdef.generics.is_empty() && !ast_map.is_empty() {
                                        let mangled =
                                            CodeGenerator::mangle_name(&fdef.name, &ast_map);
                                        // GENERIC-SHADOW-MONO-001: a prior
                                        // call site may have forward-DECLARED
                                        // the mangled name without a body —
                                        // `is_none()` would then skip
                                        // instantiation and link against an
                                        // empty declaration. Compile whenever
                                        // no definition exists yet.
                                        let needs_compile = self
                                            .generator
                                            .module
                                            .get_function(&mangled)
                                            .map(|f| f.count_basic_blocks() == 0)
                                            .unwrap_or(true);
                                        if needs_compile {
                                            self.generator.compile_generic_func(&fdef, &ast_map)?;
                                        }
                                        if let Some(shadow_fn) =
                                            self.generator.module.get_function(&mangled)
                                        {
                                            let params = shadow_fn.get_params();
                                            for (i, arg) in arguments.iter_mut().enumerate() {
                                                if let Some(param) = params.get(i) {
                                                    let param_ty = param.get_type();
                                                    let arg_basic: BasicValueEnum = match *arg {
                                                        BasicMetadataValueEnum::IntValue(iv) => {
                                                            iv.into()
                                                        }
                                                        BasicMetadataValueEnum::FloatValue(fv) => {
                                                            fv.into()
                                                        }
                                                        BasicMetadataValueEnum::PointerValue(
                                                            pv,
                                                        ) => pv.into(),
                                                        BasicMetadataValueEnum::StructValue(
                                                            svs,
                                                        ) => svs.into(),
                                                        _ => continue,
                                                    };
                                                    if arg_basic.get_type() != param_ty {
                                                        let coerced =
                                                            self.coerce_to(arg_basic, param_ty)?;
                                                        *arg =
                                                            BasicMetadataValueEnum::from(coerced);
                                                    }
                                                }
                                            }
                                            let result = self
                                                    .generator
                                                    .build_call(
                                                        shadow_fn,
                                                        &arguments,
                                                        "resolved_generic_shadow_call",
                                                    )?
                                                    .try_as_basic_value_opt()
                                                    .ok_or_else(|| {
                                                        CompileError::LlvmError(format!(
                                                            "generic shadow call '{mangled}' returned void"
                                                        ))
                                                    })?;
                                            return self
                                                .wrap_builtin_string_result(result, &call.result);
                                        }
                                    }
                                }
                                // Coerce arguments to match the shadow
                                // function's declared parameter types (same
                                // as ResolvedCallee::Function path).
                                let params = shadow_fn.get_params();
                                for (i, arg) in arguments.iter_mut().enumerate() {
                                    if let Some(param) = params.get(i) {
                                        let param_ty = param.get_type();
                                        let arg_basic: BasicValueEnum = match *arg {
                                            BasicMetadataValueEnum::IntValue(iv) => iv.into(),
                                            BasicMetadataValueEnum::FloatValue(fv) => fv.into(),
                                            BasicMetadataValueEnum::PointerValue(pv) => pv.into(),
                                            BasicMetadataValueEnum::StructValue(sv) => sv.into(),
                                            _ => continue,
                                        };
                                        if arg_basic.get_type() != param_ty {
                                            let coerced = self.coerce_to(arg_basic, param_ty)?;
                                            *arg = BasicMetadataValueEnum::from(coerced);
                                        }
                                    }
                                }
                                let result = self
                                    .generator
                                    .build_call(shadow_fn, &arguments, "resolved_shadow_call")?
                                    .try_as_basic_value_opt()
                                    .ok_or_else(|| {
                                        CompileError::LlvmError(format!(
                                            "resolved shadow call '{name}' returned void"
                                        ))
                                    })?;
                                return self.wrap_builtin_string_result(result, &call.result);
                            }
                        }
                        // 0.32.22: Blacklist builtins that generate inline control
                        // flow (early returns, unreachable) which corrupts
                        // the enclosing function's return type. These must
                        // be compiled by the legacy emitter which handles
                        // the control flow correctly.
                        const CONTROL_FLOW_BUILTINS: &[&str] =
                            &["write_file", "read_file", "file_exists"];
                        if CONTROL_FLOW_BUILTINS.contains(&name) {
                            return Err(CompileError::Unsupported(format!(
                                "builtin '{name}' generates inline control flow \
                                 (not safe for resolved delegation)"
                            )));
                        }
                        // 2026-08-06 (audit 1o): substring builtins take the
                        // Mimi string VALUE ({ptr,i64} struct) but the runtime
                        // helpers (mimi_str_substring / mimi_str_substring_clamp)
                        // take a raw C-string ptr — the runtime-direct path
                        // coerced {ptr,i64} → ptr and failed (E0722). Skip the
                        // direct-runtime shortcut so the call falls through to
                        // compile_builtin_call → the string emitters, which
                        // extract the data pointer via extract_string_arg
                        // (handles both the struct and raw-ptr forms).
                        // 0.35.7 (dx-backlog #19): extended to the whole
                        // str_* family — the same {ptr,i64}→ptr coercion failed
                        // EVERY stdlib function body calling them (e.g. the
                        // `impl Str for string` methods in std/strings.mimi)
                        // during resolved compilation, demoting them to legacy
                        // (strings/collections module-body slice blocked).
                        const STRING_ABI_BUILTINS: &[&str] = &[
                            "str_char_at",
                            "str_contains",
                            "str_starts_with",
                            "str_ends_with",
                            "str_parse_int",
                            "str_parse_float",
                            "str_index_of",
                            "str_count_substring",
                            "str_repeat",
                            "str_trim",
                            "str_to_upper",
                            "str_to_lower",
                            "str_substring",
                            "str_substring_strict",
                            "str_split",
                            "str_join",
                            "str_replace",
                        ];
                        let runtime_fn_name = if STRING_ABI_BUILTINS.contains(&name)
                            // 0.35.23 deep-eval: the print family goes through
                            // compile_builtin_call's io emitters, which accept
                            // the {ptr,i64} string struct directly. The
                            // runtime-direct parameter coerce below would try
                            // to coerce a string struct → raw C ptr
                            // (mimi_print_line params are i8*) and fail with
                            // "resolved numeric conversion {ptr,i64} → ptr"
                            // for `println(to_string(n))` (mimi-log main).
                            || matches!(name, "println" | "print" | "eprintln" | "format")
                            // len: compile_len dispatches on pending_len_is_string
                            // (strlen vs list helper); the runtime-direct coerce
                            // would demand a raw ptr for `len(str_trim(line))`
                            // (mimi-log main) and fail the same way.
                            || name == "len"
                        {
                            String::new() // sentinel: no direct runtime call
                        } else {
                            let runtime_fn_name = format!("mimi_{name}");
                            if self
                                .generator
                                .module
                                .get_function(&runtime_fn_name)
                                .is_some()
                            {
                                runtime_fn_name
                            } else {
                                // Alias mapping for builtins that delegate to
                                // differently-named runtime functions.
                                match name {
                                    "session_send" => "mimi_channel_send".to_string(),
                                    "session_recv" => "mimi_channel_recv".to_string(),
                                    "session_close" => "mimi_channel_drop".to_string(),
                                    _ => runtime_fn_name,
                                }
                            }
                        };
                        // 0.32.22: Coerce integer arguments to match the runtime
                        // function's declared parameter types. Builtins like
                        // mutex_new call runtime functions (mimi_mutex_new)
                        // declared with i64 params, but the resolved IR types
                        // integer literals as i32. Look up the runtime function
                        // and coerce to match its signature.
                        if let Some(runtime_fn) =
                            self.generator.module.get_function(&runtime_fn_name)
                        {
                            let params = runtime_fn.get_params();
                            for (i, arg) in arguments.iter_mut().enumerate() {
                                if let Some(param) = params.get(i) {
                                    let param_ty = param.get_type();
                                    let arg_basic: BasicValueEnum = match *arg {
                                        BasicMetadataValueEnum::IntValue(iv) => iv.into(),
                                        BasicMetadataValueEnum::FloatValue(fv) => fv.into(),
                                        BasicMetadataValueEnum::PointerValue(pv) => pv.into(),
                                        BasicMetadataValueEnum::StructValue(sv) => sv.into(),
                                        _ => continue,
                                    };
                                    if arg_basic.get_type() != param_ty {
                                        let coerced = self.coerce_to(arg_basic, param_ty)?;
                                        *arg = BasicMetadataValueEnum::from(coerced);
                                    }
                                }
                            }
                        }
                        // Audit wave2 (D-5a): element-type channel for
                        // sum(List<f64>) — mirrors the legacy call-site
                        // setter in expr/call/simple.rs so the builtin does
                        // not accumulate f64 bit patterns as i64.
                        if name == "sum" {
                            self.generator.pending_sum_elem_type =
                                call.arguments.first().and_then(|arg| {
                                    CodeGenerator::strip_list_element_type(
                                        &resolved_type_display_name(self.program, &arg.value.ty),
                                    )
                                });
                        }
                        // The resolved emitter does not set legacy pending
                        // helpers that `compile_to_string` needs for `Any`.
                        // Tell it when the argument is an untyped map value so
                        // `to_string(values(record)[i])` goes through
                        // `mimi_any_to_string` instead of snprintf on the
                        // raw ValueHandle.
                        if name == "to_string" && call.arguments.len() == 1 {
                            let arg_ty = resolved_type_display_name(
                                self.program,
                                &call.arguments[0].value.ty,
                            );
                            self.generator.pending_to_string_is_any =
                                matches!(arg_ty.as_str(), "Any" | "any" | "unknown");
                            self.generator.pending_to_string_arg_type = Some(arg_ty);
                        }
                        // 0.1.8 Phase D: the resolved emitter must not lower
                        // typed `from_json::<T>` through the untyped
                        // `mimi_from_json` ABI. Route it through the legacy
                        // typed deserializer so `List<string>` uses fat MimiStr
                        // boxes, records use typed structs, etc.
                        if name == "from_json"
                            && !call.type_arguments.is_empty()
                            && call.arguments.len() == 1
                        {
                            let display = resolved_type_display_name(self.program, &expression.ty);
                            if let Some(ty) =
                                crate::codegen::expr::call::helpers::parse_type_str(&display)
                            {
                                let raw_ptr = match arguments[0] {
                                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                                    BasicMetadataValueEnum::StructValue(sv) => self
                                        .generator
                                        .builder
                                        .build_extract_value(sv, 0, "from_json_str_ptr")
                                        .map_err(|e| {
                                            CompileError::LlvmError(format!(
                                                "extract from_json string: {e}"
                                            ))
                                        })?
                                        .into_pointer_value(),
                                    _ => {
                                        return Err(CompileError::Unsupported(
                                            "resolved from_json requires a string argument".into(),
                                        ))
                                    }
                                };
                                return self.generator.compile_from_json_raw(&ty, raw_ptr);
                            }
                        }
                        // 0.39.136 architecture (M1): ONE shared typed
                        // dispatcher for both emitters — lists, sets,
                        // maps, records, tuples and nested-product
                        // combinations all route identically to legacy.
                        if name == "to_json" && !call.arguments.is_empty() {
                            let obj_type = resolved_type_display_name(
                                self.program,
                                &call.arguments[0].value.ty,
                            );
                            // The recursive serializer must GEP the *real* box
                            // layout. For a bare variable argument the resolved
                            // emitter allocates `Option<List>` / `Result<List>`
                            // either embedded (`{i1,{i64,ptr}}`) or heap-packed
                            // (`{i1,ptr}` / `{i1,i64}`) depending on the program's
                            // global heap-packing, while `llvm_type_for` always
                            // force-heaps. So take the *actual* storage type from
                            // the variable entry when the argument is a plain
                            // variable; otherwise fall back to `llvm_type_for`.
                            let actual_ty: Option<BasicTypeEnum<'ctx>> = match &call.arguments[0]
                                .value
                                .kind
                            {
                                ResolvedExprKind::Load(place) if place.projections.is_empty() => {
                                    frame.locals.get(&place.base).map(|entry| entry.llvm_type)
                                }
                                _ => crate::codegen::expr::call::helpers::parse_type_str(&obj_type)
                                    .and_then(|t| self.generator.llvm_type_for(&t)),
                            };
                            if let Some(value) = self.generator.emit_typed_to_json_dispatch(
                                &obj_type,
                                arguments[0],
                                actual_ty,
                            )? {
                                return self.wrap_builtin_string_result(value, &call.result);
                            }
                        }
                        // 0.39.x matrix sweep: fail closed on builtin METHOD
                        // faces this emitter does not implement (and_then,
                        // ok_or, …). Falling through to compile_builtin_call
                        // used to emit nonsense for them — the poisoned-body
                        // class of bugs. The function falls back to the legacy
                        // emitter, which owns correct semantics for these.
                        if name.starts_with("builtin.method.") {
                            return Err(CompileError::Unsupported(format!(
                                "builtin method '{name}' has no resolved-native emitter"
                            )));
                        }
                        let result = self.generator.compile_builtin_call(name, &arguments)?;
                        // ABI bridge: builtins return raw ptr for strings, but the
                        // resolved emitter expects {ptr, i64} structs. Wrap if needed.
                        self.wrap_builtin_string_result(result, &call.result)
                    }
                    // 0.32.16: LocalClosure — indirect call through closure struct.
                    ResolvedCallee::LocalClosure(local_id) => {
                        let entry = frame.locals.get(local_id).ok_or_else(|| {
                            CompileError::Unsupported(format!(
                                "closure local '{}' not found in frame",
                                local_id.0 .0
                            ))
                        })?;
                        let closure_val = self.generator.build_load(
                            entry.llvm_type,
                            entry.storage,
                            "closure_load",
                        )?;
                        let sv = closure_val.into_struct_value();
                        let ptr_ty = self
                            .generator
                            .context
                            .ptr_type(inkwell::AddressSpace::default());
                        let fn_ptr = self
                            .generator
                            .builder
                            .build_extract_value(sv, 0, "closure_fn_ptr")
                            .map_err(|e| CompileError::LlvmError(format!("extract fn_ptr: {e}")))?
                            .into_pointer_value();
                        let env_ptr = self
                            .generator
                            .builder
                            .build_extract_value(sv, 1, "closure_env_ptr")
                            .map_err(|e| CompileError::LlvmError(format!("extract env_ptr: {e}")))?
                            .into_pointer_value();
                        // Build indirect call: ret(env_ptr, args...).
                        let ret_llvm_ty = self.lower_type(&expression.ty)?;
                        let mut all_meta: Vec<BasicMetadataTypeEnum> =
                            vec![BasicMetadataTypeEnum::PointerType(ptr_ty)];
                        for arg in &arguments {
                            all_meta.push(match arg {
                                BasicMetadataValueEnum::IntValue(iv) => {
                                    BasicMetadataTypeEnum::IntType(iv.get_type())
                                }
                                BasicMetadataValueEnum::FloatValue(fv) => {
                                    BasicMetadataTypeEnum::FloatType(fv.get_type())
                                }
                                BasicMetadataValueEnum::PointerValue(pv) => {
                                    BasicMetadataTypeEnum::PointerType(pv.get_type())
                                }
                                BasicMetadataValueEnum::StructValue(sv) => {
                                    BasicMetadataTypeEnum::StructType(sv.get_type())
                                }
                                _ => BasicMetadataTypeEnum::IntType(
                                    self.generator.context.i64_type(),
                                ),
                            });
                        }
                        let indirect_fn_ty = match ret_llvm_ty {
                            BasicTypeEnum::IntType(t) => t.fn_type(&all_meta, false),
                            BasicTypeEnum::FloatType(t) => t.fn_type(&all_meta, false),
                            BasicTypeEnum::PointerType(t) => t.fn_type(&all_meta, false),
                            BasicTypeEnum::StructType(t) => t.fn_type(&all_meta, false),
                            _ => self.generator.context.i64_type().fn_type(&all_meta, false),
                        };
                        let mut call_args: Vec<BasicMetadataValueEnum> =
                            vec![BasicMetadataValueEnum::PointerValue(env_ptr)];
                        call_args.extend_from_slice(&arguments);
                        let call = self
                            .generator
                            .builder
                            .build_indirect_call(indirect_fn_ty, fn_ptr, &call_args, "closure_call")
                            .map_err(|e| CompileError::LlvmError(format!("indirect call: {e}")))?;
                        call.try_as_basic_value_opt().ok_or_else(|| {
                            CompileError::LlvmError("closure call returned void".into())
                        })
                    }
                    // 0.32.20: Flow transition calls. The legacy emitter
                    // converts transitions to synthetic functions with names
                    // like "Counter__inc__from_Zero". These are forward-
                    // declared before the resolved subset is compiled.
                    ResolvedCallee::Transition(ref tid) => {
                        let symbol =
                            format!("{}__{}__from_{}", tid.flow.0, tid.event, tid.source.name);
                        let callee =
                            self.generator.module.get_function(&symbol).ok_or_else(|| {
                                CompileError::LlvmError(format!(
                                    "resolved transition callee '{symbol}' is undeclared"
                                ))
                            })?;
                        // Coerce arguments to match the callee's parameter types.
                        let params = callee.get_params();
                        for (i, arg) in arguments.iter_mut().enumerate() {
                            if let Some(param) = params.get(i) {
                                let param_ty = param.get_type();
                                let arg_basic: BasicValueEnum = match *arg {
                                    BasicMetadataValueEnum::IntValue(iv) => iv.into(),
                                    BasicMetadataValueEnum::FloatValue(fv) => fv.into(),
                                    BasicMetadataValueEnum::PointerValue(pv) => pv.into(),
                                    BasicMetadataValueEnum::StructValue(sv) => sv.into(),
                                    _ => continue,
                                };
                                if arg_basic.get_type() != param_ty {
                                    let coerced = self.coerce_to(arg_basic, param_ty)?;
                                    *arg = BasicMetadataValueEnum::from(coerced);
                                }
                            }
                        }
                        self.generator
                            .build_call(callee, &arguments, "resolved_transition")?
                            .try_as_basic_value_opt()
                            .ok_or_else(|| {
                                CompileError::LlvmError(format!(
                                    "resolved transition '{symbol}' returned void"
                                ))
                            })
                    }
                    // 0.32.22: ProtocolMethod — static trait method dispatch.
                    // Extract the concrete type and method name from the
                    // MethodId, construct the impl function name
                    // ("{Type}_{method}"), and call it directly.
                    ResolvedCallee::ProtocolMethod { ref method, .. } => {
                        // MethodId format: "function:{Trait}:for:{Type}::{method}:{hash}"
                        let method_str = method.as_str();
                        let (impl_type, method_name) = method_str
                            .strip_prefix("function:")
                            .and_then(|s: &str| s.split_once(":for:"))
                            .and_then(|(_, rest): (&str, &str)| {
                                rest.split_once("::")
                                    .map(|(ty, method_hash): (&str, &str)| {
                                        let method_name = method_hash
                                            .rsplit_once(':')
                                            .map(|(m, _)| m)
                                            .unwrap_or(method_hash);
                                        (ty.to_string(), method_name.to_string())
                                    })
                            })
                            .ok_or_else(|| {
                                CompileError::Unsupported(format!(
                                    "cannot parse ProtocolMethod MethodId '{method_str}'"
                                ))
                            })?;
                        // Builtin SetExt methods must call the runtime directly.
                        // The resolved lowering re-dispatches `self.size()` inside
                        // the synthetic `Set_size` impl body back through
                        // ProtocolMethod, producing self-recursive trampolines
                        // (`call Set_size -> Set_size -> ...`). The legacy emitter
                        // already gives builtin Set semantics precedence; mirror
                        // that here before looking up the generic symbol.
                        if impl_type == "Set" || impl_type.starts_with("Set<") {
                            if let Some(value) =
                                self.emit_builtin_set_protocol_method(&method_name, &arguments)?
                            {
                                return Ok(value);
                            }
                        }
                        let symbol = format!("{}_{}", impl_type, method_name);
                        let callee =
                            self.generator.module.get_function(&symbol).ok_or_else(|| {
                                CompileError::LlvmError(format!(
                                    "resolved ProtocolMethod callee '{symbol}' is undeclared"
                                ))
                            })?;
                        // ⚠ 0.32.24: Do NOT add ABI coercion (struct→ptr)
                        // here. Builtin types (String= {ptr,i64}, List={i64,ptr})
                        // use impl methods that expect raw char*/data pointers,
                        // NOT struct pointers. Coercion produces invalid code.
                        // Functions needing such fallback should fall through
                        // to the legacy emitter correctly.
                        let params = callee.get_params();
                        for (i, arg) in arguments.iter_mut().enumerate() {
                            if let Some(param) = params.get(i) {
                                let param_ty = param.get_type();
                                let arg_basic: BasicValueEnum = match *arg {
                                    BasicMetadataValueEnum::IntValue(iv) => iv.into(),
                                    BasicMetadataValueEnum::FloatValue(fv) => fv.into(),
                                    BasicMetadataValueEnum::PointerValue(pv) => pv.into(),
                                    BasicMetadataValueEnum::StructValue(sv) => sv.into(),
                                    _ => continue,
                                };
                                if arg_basic.get_type() != param_ty {
                                    let coerced = self.coerce_to(arg_basic, param_ty)?;
                                    *arg = BasicMetadataValueEnum::from(coerced);
                                }
                            }
                        }
                        self.generator
                            .build_call(callee, &arguments, "resolved_trait_call")?
                            .try_as_basic_value_opt()
                            .ok_or_else(|| {
                                CompileError::LlvmError(format!(
                                    "resolved ProtocolMethod '{symbol}' returned void"
                                ))
                            })
                    }
                    ResolvedCallee::Extern(callee_id) => {
                        // 0.32.26: Extern FFI call — look up the wrapper function
                        // by name (declared by legacy emitter in step 1) and call it.
                        let ext_name = self.program.extern_blocks().values()
                            .flat_map(|block| block.signatures.iter())
                            .find(|sig| sig.node_id == *callee_id)
                            .map(|sig| sig.name.as_str())
                            .ok_or_else(|| CompileError::LlvmError(format!(
                                "resolved extern callee '{callee_id:?}' not found in any extern block"
                            )))?;
                        // 0.34.35b (M-001): wrapper 显式命名 `{name}.extern_wrapper`，
                        // 必须经 extern_wrapper_fns map 查找——module.get_function(声明名)
                        // 现会命中 extern 原符号（跳过 wrapper 的 ABI 参数转换）。
                        let callee =
                            self.generator.extern_wrapper_fn(ext_name).ok_or_else(|| {
                                CompileError::LlvmError(format!(
                                    "resolved extern wrapper '{ext_name}' is undeclared"
                                ))
                            })?;
                        // Coerce arguments to match the wrapper's parameter types.
                        let params = callee.get_params();
                        for (i, arg) in arguments.iter_mut().enumerate() {
                            if let Some(param) = params.get(i) {
                                let param_ty = param.get_type();
                                let arg_basic: BasicValueEnum = match *arg {
                                    BasicMetadataValueEnum::IntValue(iv) => iv.into(),
                                    BasicMetadataValueEnum::FloatValue(fv) => fv.into(),
                                    BasicMetadataValueEnum::PointerValue(pv) => pv.into(),
                                    BasicMetadataValueEnum::StructValue(sv) => sv.into(),
                                    _ => continue,
                                };
                                if arg_basic.get_type() != param_ty {
                                    let coerced = self.coerce_to(arg_basic, param_ty)?;
                                    *arg = BasicMetadataValueEnum::from(coerced);
                                }
                            }
                        }
                        self.generator
                            .build_call(callee, &arguments, "resolved_extern_call")?
                            .try_as_basic_value_opt()
                            .ok_or_else(|| {
                                CompileError::LlvmError(format!(
                                    "resolved extern '{ext_name}' returned void"
                                ))
                            })
                    }
                }
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => self.emit_if(expression, condition, then_block, else_block, frame, false),
            ResolvedExprKind::Block(block) | ResolvedExprKind::Comptime(block) => {
                // A nested block expression: emit inline and return its value.
                let value = self.emit_block(
                    &self
                        .program
                        .callable(&frame.owner)
                        .ok_or_else(|| {
                            CompileError::Unsupported(format!(
                                "resolved callable '{}' is absent for block expression",
                                frame.owner.0
                            ))
                        })?
                        .body,
                    block,
                    frame,
                )?;
                value.ok_or_else(|| {
                    CompileError::Unsupported(format!(
                        "resolved block expression '{}' produced no value",
                        expression.node_id.0
                    ))
                })
            }
            ResolvedExprKind::Scope {
                body: scope_block, ..
            } => {
                let callable_body = &self
                    .program
                    .callable(&frame.owner)
                    .ok_or_else(|| {
                        CompileError::Unsupported(format!(
                            "resolved callable '{}' is absent for scope expression",
                            frame.owner.0
                        ))
                    })?
                    .body;
                let value = self.emit_block(callable_body, scope_block, frame)?;
                value.ok_or_else(|| {
                    CompileError::Unsupported(format!(
                        "resolved scope expression '{}' produced no value",
                        expression.node_id.0
                    ))
                })
            }
            ResolvedExprKind::FString(parts) => self.emit_fstring(parts, frame),
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.emit_match(expression, scrutinee, arms, frame)
            }
            // 0.32.10: Try expression (`?` operator).
            // On Ok/Some: extract payload, continue.
            // On Err/None: call mimi_try_exit(err_val) → unreachable.
            ResolvedExprKind::Try { value, .. } => self.emit_try(value, &expression.ty, frame),
            // 0.32.16: Lambda expression (non-capturing closure).
            ResolvedExprKind::Lambda(lambda) => self.emit_lambda(lambda, &expression.ty, frame),
            // 0.32.31: Slice expression (xs[start:end]).
            // View semantics: no data copy, new struct points into existing buffer.
            ResolvedExprKind::Slice { target, start, end } => {
                self.emit_slice(target, start.as_deref(), end.as_deref(), frame)
            }
            // 0.32.32: Old expression (contract `old(x)`). Identity in codegen
            // when contracts are erased; under --verify-contracts (0.34.41
            // 第二档) it loads the entry snapshot captured by
            // emit_contract_prologue.
            ResolvedExprKind::Old(inner) => {
                if let Some(entry) = frame.old_snapshots.get(&expression.node_id) {
                    let entry = *entry;
                    return self
                        .generator
                        .build_load(entry.llvm_type, entry.storage, "old_load");
                }
                self.emit_expr(inner, frame)
            }
            // 0.32.33: Comprehension ([value for pattern in iterable if guard]).
            // Lowered to: pre-allocate buffer of iterable_len, loop, filter, store, build list.
            ResolvedExprKind::Comprehension {
                pattern,
                value,
                iterable,
                guard,
            } => self.emit_comprehension(
                pattern,
                value,
                iterable,
                guard.as_deref(),
                &expression.ty,
                frame,
            ),
            // 0.32.34: OptionalChain (receiver?.field).
            // If receiver is Some/Ok: project field from payload, wrap in Some.
            // If receiver is None/Err: return None.
            ResolvedExprKind::OptionalChain {
                receiver,
                field,
                field_type,
            } => self.emit_optional_chain(receiver, field, field_type, frame),
            // 0.32.35: Callable (first-class function value).
            // Returns a pointer to the declared LLVM function symbol.
            ResolvedExprKind::Callable(callee) => self.emit_callable_ref(callee),
            other => Err(CompileError::Unsupported(format!(
                "resolved expression {other:?} escaped resolved native eligibility at '{}'",
                expression.node_id.0
            ))),
        }
    }

    fn emit_literal(
        &mut self,
        ty: &ResolvedTypeId,
        literal: &ResolvedLiteral,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let llvm_type = self.lower_type(ty)?;
        match (literal, llvm_type) {
            (ResolvedLiteral::Int(value), BasicTypeEnum::IntType(integer)) => {
                Ok(integer.const_int(*value as u64, true).into())
            }
            (ResolvedLiteral::Bool(value), BasicTypeEnum::IntType(integer)) => {
                Ok(integer.const_int(u64::from(*value), false).into())
            }
            (ResolvedLiteral::FloatBits(bits), BasicTypeEnum::FloatType(float)) => {
                Ok(float.const_float(f64::from_bits(*bits)).into())
            }
            (ResolvedLiteral::Unit, BasicTypeEnum::IntType(integer)) => {
                Ok(integer.const_zero().into())
            }
            (ResolvedLiteral::String(text), BasicTypeEnum::StructType(st)) => {
                // String ABI: {ptr, i64} struct (ptr to data, byte length).
                // NUL-safe global (BUG E): a string's bytes are stored verbatim
                // (trailing NUL appended for C consumers); the embedded NUL in a
                // literal like "a\0b" must NOT truncate the buffer — `build_global_string_ptr`
                // would, so use `build_global_string_bytes`.
                let ptr_val = self
                    .generator
                    .build_global_string_bytes(text, "resolved_str")?;
                let len_val = self
                    .generator
                    .context
                    .i64_type()
                    .const_int(text.len() as u64, false);
                let agg = st.const_named_struct(&[ptr_val.into(), len_val.into()]);
                Ok(agg.into())
            }
            _ => Err(CompileError::Unsupported(
                "resolved literal does not match its canonical type".into(),
            )),
        }
    }

    fn emit_const_value(
        &mut self,
        ty: &ResolvedTypeId,
        value: &ResolvedConstValue,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let llvm_type = self.lower_type(ty)?;
        match (value, llvm_type) {
            (ResolvedConstValue::Int(v), BasicTypeEnum::IntType(integer)) => {
                Ok(integer.const_int(*v as u64, true).into())
            }
            (ResolvedConstValue::Bool(v), BasicTypeEnum::IntType(integer)) => {
                Ok(integer.const_int(u64::from(*v), false).into())
            }
            (ResolvedConstValue::Float(v), BasicTypeEnum::FloatType(float)) => {
                Ok(float.const_float(*v).into())
            }
            (ResolvedConstValue::Unit, BasicTypeEnum::IntType(integer)) => {
                Ok(integer.const_zero().into())
            }
            (ResolvedConstValue::String(text), BasicTypeEnum::StructType(st)) => {
                // NUL-safe global (BUG E): see `emit_resolved_literal` for rationale.
                let ptr_val = self
                    .generator
                    .build_global_string_bytes(text, "resolved_const_str")?;
                let len_val = self
                    .generator
                    .context
                    .i64_type()
                    .const_int(text.len() as u64, false);
                let agg = st.const_named_struct(&[ptr_val.into(), len_val.into()]);
                Ok(agg.into())
            }
            _ => Err(CompileError::Unsupported(
                "resolved constant value does not match its canonical type".into(),
            )),
        }
    }

    fn emit_fstring(
        &mut self,
        parts: &[ResolvedFStringPart],
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // Fast path: text-only f-string → global constant.
        let all_text: Option<String> = parts
            .iter()
            .map(|p| match p {
                ResolvedFStringPart::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        if let Some(text) = all_text {
            // NUL-safe global (BUG E): a text-only f-string may itself contain an
            // embedded NUL (e.g. f"a\0b"); use the byte-preserving helper.
            let ptr_val = self
                .generator
                .build_global_string_bytes(&text, "resolved_fstr")?;
            let ptr_ty = self
                .generator
                .context
                .ptr_type(inkwell::AddressSpace::default());
            let i64_ty = self.generator.context.i64_type();
            let struct_ty = self.generator.context.struct_type(
                &[
                    BasicTypeEnum::PointerType(ptr_ty),
                    BasicTypeEnum::IntType(i64_ty),
                ],
                false,
            );
            let len_val = i64_ty.const_int(text.len() as u64, false);
            let agg = struct_ty.const_named_struct(&[ptr_val.into(), len_val.into()]);
            return Ok(agg.into());
        }

        // Interpolation path: compile each part tracking (ptr, len), malloc the
        // exact total, compose with memcpy at tracked offsets.
        //
        // AUDIT FIX A2 (full-audit-2026-08-05 §16 / roadmap §4-A2): the old
        // path assembled a printf format string (%s/%d/%g) and snprintf'd into
        // a 4096-byte STACK buffer — four defects at once:
        //   1. %s stops at an embedded NUL → f"a{chr(0)}b" lost the NUL
        //      (CG len 2, VM len 3; the VM's ConcatStr is length-based,
        //      interp/bytecode/compiler.rs:2888).
        //   2. Bool rendered via %d → "1"/"0" instead of the VM's "true"/"false".
        //   3. Float rendered via %g → diverges from the VM's Rust shortest
        //      round-trip Display beyond 6 significant digits (1e+06 vs 1000000).
        //   4. The result struct pointed into the stack frame with len=0
        //      ("runtime uses null terminator") — dangling past the frame and
        //      lying about the length channel.
        // Rewritten to the same length-based discipline as expr/literal.rs's
        // f-string assembly: authoritative len fields, exact-size heap buffer,
        // memcpy composition, tracked len end-to-end — NUL bytes survive and
        // the len field never needs strlen(buf).
        let i64_ty = self.generator.context.i64_type();
        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());

        // strlen: only used over NUL-free renderings (bool literals, snprintf
        // temp buffers, mimi_to_string_f64 results) or raw C-string pointers
        // that carry no length at all. Never over composed data.
        let strlen_fn = self
            .generator
            .module
            .get_function("strlen")
            .unwrap_or_else(|| {
                let ty = i64_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(ptr_ty)], false);
                self.generator.module.add_function(
                    "strlen",
                    ty,
                    Some(inkwell::module::Linkage::External),
                )
            });

        enum CompiledPart<'ctx> {
            Text(String),
            Interp {
                ptr: PointerValue<'ctx>,
                len: inkwell::values::IntValue<'ctx>,
            },
        }
        let mut compiled_parts: Vec<CompiledPart<'ctx>> = Vec::new();
        // +1 for the trailing NUL handed to C-string consumers downstream.
        let mut total_size = i64_ty.const_int(1, false);

        for (i, part) in parts.iter().enumerate() {
            match part {
                ResolvedFStringPart::Text(t) => {
                    total_size = self
                        .generator
                        .builder
                        .build_int_add(
                            total_size,
                            i64_ty.const_int(t.len() as u64, false),
                            &format!("fstr_text_sz_{}", i),
                        )
                        .map_err(|e| CompileError::LlvmError(format!("add error: {e}")))?;
                    compiled_parts.push(CompiledPart::Text(t.clone()));
                }
                ResolvedFStringPart::Interpolation(expr) => {
                    let value = self.emit_expr(expr, frame)?;
                    let prim = match self.program.resolved_types().get(&expr.ty) {
                        Some(ResolvedType::Primitive(p)) => Some(p),
                        _ => None,
                    };
                    if matches!(prim, Some(PrimitiveType::Bool)) {
                        // "true"/"false" globals — VM parity (the old %d path
                        // printed 1/0).
                        let iv = match value {
                            BasicValueEnum::IntValue(iv) => iv,
                            _ => {
                                return Err(CompileError::Unsupported(
                                    "f-string bool interpolation did not lower to an integer"
                                        .into(),
                                ))
                            }
                        };
                        let true_g = self
                            .generator
                            .builder
                            .build_global_string_ptr("true", &format!("fstr_true_{}", i))
                            .map_err(|e| CompileError::LlvmError(format!("string: {e}")))?
                            .as_pointer_value();
                        let false_g = self
                            .generator
                            .builder
                            .build_global_string_ptr("false", &format!("fstr_false_{}", i))
                            .map_err(|e| CompileError::LlvmError(format!("string: {e}")))?
                            .as_pointer_value();
                        let zero = iv.get_type().const_zero();
                        let cond = self
                            .generator
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::NE,
                                iv,
                                zero,
                                &format!("fstr_bool_nz_{}", i),
                            )
                            .map_err(|e| CompileError::LlvmError(format!("cmp error: {e}")))?;
                        let ptr = self
                            .generator
                            .builder
                            .build_select(
                                cond,
                                BasicValueEnum::PointerValue(true_g),
                                BasicValueEnum::PointerValue(false_g),
                                &format!("fstr_bool_sel_{}", i),
                            )
                            .map_err(|e| CompileError::LlvmError(format!("select error: {e}")))?
                            .into_pointer_value();
                        let len =
                            self.call_strlen(strlen_fn, ptr, &format!("fstr_bool_strlen_{}", i))?;
                        total_size = self
                            .generator
                            .builder
                            .build_int_add(total_size, len, &format!("fstr_bool_sz_{}", i))
                            .map_err(|e| CompileError::LlvmError(format!("add error: {e}")))?;
                        compiled_parts.push(CompiledPart::Interp { ptr, len });
                    } else if matches!(prim, Some(PrimitiveType::F32 | PrimitiveType::F64)) {
                        // mimi_to_string_f64 = the same Rust shortest round-trip
                        // Display the VM uses; returns a NUL-free heap C string,
                        // so strlen is safe.
                        let fv = match value {
                            BasicValueEnum::FloatValue(fv) => fv,
                            _ => {
                                return Err(CompileError::Unsupported(
                                    "f-string float interpolation did not lower to a float".into(),
                                ))
                            }
                        };
                        let fv64 = if fv.get_type().get_bit_width() == 32 {
                            self.generator
                                .builder
                                .build_float_ext(
                                    fv,
                                    self.generator.context.f64_type(),
                                    &format!("fstr_fext_{}", i),
                                )
                                .map_err(|e| CompileError::LlvmError(format!("fext: {e}")))?
                        } else {
                            fv
                        };
                        let to_str_fn = self.generator.get_runtime_fn("mimi_to_string_f64")?;
                        let ptr = self
                            .generator
                            .build_call(
                                to_str_fn,
                                &[BasicMetadataValueEnum::FloatValue(fv64)],
                                &format!("fstr_f64_{}", i),
                            )?
                            .try_as_basic_value_opt()
                            .ok_or_else(|| {
                                CompileError::LlvmError("mimi_to_string_f64 returned void".into())
                            })?
                            .into_pointer_value();
                        // Heap-owned by this f-string evaluation; freed at scope
                        // exit via the heap-scope registry.
                        self.generator.register_heap_alloc(ptr);
                        let len =
                            self.call_strlen(strlen_fn, ptr, &format!("fstr_strlen_{}", i))?;
                        total_size = self
                            .generator
                            .builder
                            .build_int_add(total_size, len, &format!("fstr_isz_{}", i))
                            .map_err(|e| CompileError::LlvmError(format!("add error: {e}")))?;
                        compiled_parts.push(CompiledPart::Interp { ptr, len });
                    } else if matches!(prim, Some(PrimitiveType::String)) {
                        match value {
                            // String struct {i8*, i64}: the authoritative len
                            // field, never strlen — embedded NULs survive
                            // composition exactly like the VM's ConcatStr.
                            BasicValueEnum::StructValue(sv) => {
                                let fields = sv.get_type().get_field_types();
                                let is_string_shape = matches!(
                                    fields.as_slice(),
                                    [BasicTypeEnum::PointerType(_), BasicTypeEnum::IntType(t)]
                                        if t.get_bit_width() == 64
                                );
                                if !is_string_shape {
                                    return Err(CompileError::Unsupported(format!(
                                        "f-string string interpolation lowered to unexpected struct shape in part {}",
                                        i
                                    )));
                                }
                                let data_ptr = self
                                    .generator
                                    .build_extract_value(sv.into(), 0, "fstr_str_data")?
                                    .into_pointer_value();
                                let len = self
                                    .generator
                                    .build_extract_value(sv.into(), 1, "fstr_str_len")?
                                    .into_int_value();
                                total_size = self
                                    .generator
                                    .builder
                                    .build_int_add(
                                        total_size,
                                        len,
                                        &format!("fstr_isz_{}", i),
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("add error: {e}"))
                                    })?;
                                compiled_parts.push(CompiledPart::Interp {
                                    ptr: data_ptr,
                                    len,
                                });
                            }
                            // Raw C-string pointer: length only recoverable via
                            // strlen (length-carrying strings travel as structs).
                            BasicValueEnum::PointerValue(pv) => {
                                let len = self.call_strlen(strlen_fn, pv, &format!("fstr_strlen_{}", i))?;
                                total_size = self
                                    .generator
                                    .builder
                                    .build_int_add(
                                        total_size,
                                        len,
                                        &format!("fstr_isz_{}", i),
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("add error: {e}"))
                                    })?;
                                compiled_parts.push(CompiledPart::Interp { ptr: pv, len });
                            }
                            _ => {
                                return Err(CompileError::Unsupported(format!(
                                    "f-string string interpolation did not lower to a string value in part {}",
                                    i
                                )))
                            }
                        }
                    } else {
                        // Integer family (i8..i128, char, usize, ...): snprintf
                        // %ld into a 32-byte heap temp buffer. The rendering is
                        // NUL-free, so strlen over the temp buffer is safe.
                        let iv = match value {
                            BasicValueEnum::IntValue(iv) => iv,
                            _ => {
                                return Err(CompileError::Unsupported(format!(
                                    "f-string interpolation of type '{}' is not supported",
                                    expr.ty.as_str()
                                )))
                            }
                        };
                        let bw = iv.get_type().get_bit_width();
                        let ext_iv = if bw < 64 {
                            self.generator
                                .builder
                                .build_int_s_extend(iv, i64_ty, &format!("fstr_ext_{}", i))
                                .map_err(|e| CompileError::LlvmError(format!("sext: {e}")))?
                        } else if bw > 64 {
                            // i128: %ld reads the low 64 bits (pre-existing
                            // limitation of the printf-based rendering).
                            self.generator
                                .builder
                                .build_int_truncate(iv, i64_ty, &format!("fstr_trunc_{}", i))
                                .map_err(|e| CompileError::LlvmError(format!("trunc: {e}")))?
                        } else {
                            iv
                        };
                        let temp_buf = self.generator.malloc_or_abort(
                            i64_ty.const_int(32, false),
                            &format!("fstr_temp_{}", i),
                        )?;
                        self.generator.register_heap_alloc(temp_buf);
                        let fmt = self
                            .generator
                            .builder
                            .build_global_string_ptr("%ld", &format!("fstr_fmt_{}", i))
                            .map_err(|e| CompileError::LlvmError(format!("string: {e}")))?;
                        let snprintf = self.get_or_declare_snprintf();
                        self.generator
                            .build_call(
                                snprintf,
                                &[
                                    BasicMetadataValueEnum::PointerValue(temp_buf),
                                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(32, false)),
                                    BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                    BasicMetadataValueEnum::IntValue(ext_iv),
                                ],
                                &format!("fstr_snprintf_{}", i),
                            )?
                            .try_as_basic_value_opt()
                            .ok_or_else(|| {
                                CompileError::LlvmError("snprintf returned void".into())
                            })?;
                        let len =
                            self.call_strlen(strlen_fn, temp_buf, &format!("fstr_strlen_{}", i))?;
                        total_size = self
                            .generator
                            .builder
                            .build_int_add(total_size, len, &format!("fstr_isz_{}", i))
                            .map_err(|e| CompileError::LlvmError(format!("add error: {e}")))?;
                        compiled_parts.push(CompiledPart::Interp { ptr: temp_buf, len });
                    }
                }
            }
        }

        // Phase 2: exact-size HEAP buffer filled via memcpy at tracked offsets
        // (no strcpy/strcat/strlen over composed data — embedded NULs survive).
        let buf = self.generator.malloc_or_abort(total_size, "fstr_buf")?;
        self.generator.register_heap_alloc(buf);
        let memcpy_fn = self.generator.get_runtime_fn("memcpy")?;
        let i8_ty = self.generator.context.i8_type();
        let mut offset = i64_ty.const_int(0, false);
        for (i, part) in compiled_parts.iter().enumerate() {
            let (src_ptr, part_len): (PointerValue<'ctx>, inkwell::values::IntValue<'ctx>) =
                match part {
                    CompiledPart::Text(t) => {
                        if t.is_empty() {
                            continue;
                        }
                        let global = self
                            .generator
                            .builder
                            .build_global_string_ptr(t, &format!("fstr_part_{}", i))
                            .map_err(|e| CompileError::LlvmError(format!("string: {e}")))?;
                        // Exact byte count: the global carries a trailing NUL
                        // that must NOT be copied into the composition.
                        (
                            global.as_pointer_value(),
                            i64_ty.const_int(t.len() as u64, false),
                        )
                    }
                    CompiledPart::Interp { ptr, len } => (*ptr, *len),
                };
            let dst = self.generator.build_in_bounds_gep(
                BasicTypeEnum::IntType(i8_ty),
                buf,
                &[offset],
                &format!("fstr_dst_{}", i),
            )?;
            // SAFETY: `dst` is buf + offset with offset + part_len <= total_size
            // (total_size accumulated every part's exact length plus the
            // terminator); `src_ptr` is valid for `part_len` bytes by
            // construction in phase 1 (globals are t.len() bytes, temp buffers
            // and runtime strings carry their measured length).
            self.generator
                .build_call(
                    memcpy_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(dst),
                        BasicMetadataValueEnum::PointerValue(src_ptr),
                        BasicMetadataValueEnum::IntValue(part_len),
                    ],
                    &format!("fstr_memcpy_{}", i),
                )
                .map_err(|e| CompileError::LlvmError(format!("memcpy: {e}")))?;
            offset = self
                .generator
                .builder
                .build_int_add(offset, part_len, &format!("fstr_off_{}", i))
                .map_err(|e| CompileError::LlvmError(format!("add error: {e}")))?;
        }

        // Phase 3: trailing NUL for C-string consumers, then the canonical
        // {i8*, i64} struct whose len field is the TRACKED total — never
        // strlen(buf) — so interior NUL bytes do not truncate the value.
        let nul_dst = self.generator.build_in_bounds_gep(
            BasicTypeEnum::IntType(i8_ty),
            buf,
            &[offset],
            "fstr_nul_gep",
        )?;
        self.generator
            .build_store(nul_dst, i8_ty.const_int(0, false))?;
        self.generator.build_string_struct(buf, offset)
    }

    /// Emit a builtin `SetExt` method directly against the runtime set API.
    ///
    /// The resolved lowering represents each trait impl method as a synthetic
    /// function (`Set_size`, `Set_insert`, ...). Its body is the original
    /// `self.method(...)` call, which would re-enter the same ProtocolMethod
    /// symbol and create a self-recursive trampoline. Builtin Set operations
    /// mutate/pass the handle in-place, so call the runtime helpers directly
    /// and coerce to the small LLVM types used by the trait signatures.
    fn emit_builtin_set_protocol_method(
        &mut self,
        method_name: &str,
        arguments: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
        if arguments.is_empty() {
            return Ok(None);
        }
        let i64_ty = self.generator.context.i64_type();
        let handle = match arguments[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            BasicMetadataValueEnum::PointerValue(pv) => self
                .generator
                .builder
                .build_ptr_to_int(pv, i64_ty, "set_self_handle")
                .map_err(|e| CompileError::LlvmError(format!("set ptrtoint: {e}")))?,
            _ => return Ok(None),
        };

        match method_name {
            "size" | "len" => {
                let func = self.generator.get_runtime_fn("mimi_set_size")?;
                let result = self
                    .generator
                    .build_call(
                        func,
                        &[BasicMetadataValueEnum::IntValue(handle)],
                        "set_size",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("mimi_set_size returned void".into()))?
                    .into_int_value();
                let i32_ty = self.generator.context.i32_type();
                let result_i32 = self
                    .generator
                    .builder
                    .build_int_truncate(result, i32_ty, "set_size_i32")
                    .map_err(|e| CompileError::LlvmError(format!("set_size trunc: {e}")))?;
                Ok(Some(result_i32.into()))
            }
            "is_empty" => {
                let func = self.generator.get_runtime_fn("mimi_set_size")?;
                let result = self
                    .generator
                    .build_call(
                        func,
                        &[BasicMetadataValueEnum::IntValue(handle)],
                        "set_size",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("mimi_set_size returned void".into()))?
                    .into_int_value();
                let zero = i64_ty.const_zero();
                let is_empty = self
                    .generator
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, result, zero, "set_is_empty")
                    .map_err(|e| CompileError::LlvmError(format!("set is_empty cmp: {e}")))?;
                Ok(Some(is_empty.into()))
            }
            "contains" | "insert" | "remove" => {
                if arguments.len() < 2 {
                    return Err(CompileError::Generic(
                        "set method expects a value argument".into(),
                    ));
                }
                let value = match arguments[1] {
                    BasicMetadataValueEnum::IntValue(iv) => {
                        // mimi_set_* take i64 value handles; a narrow literal
                        // (e.g. `s.remove(1)` → i32) must be widened to i64.
                        if iv.get_type().get_bit_width() < 64 {
                            self.generator
                                .builder
                                .build_int_s_extend(iv, i64_ty, "set_value_i64")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("set value sext: {e}"))
                                })?
                        } else {
                            iv
                        }
                    }
                    BasicMetadataValueEnum::PointerValue(pv) => self
                        .generator
                        .builder
                        .build_ptr_to_int(pv, i64_ty, "set_value_handle")
                        .map_err(|e| CompileError::LlvmError(format!("set value ptrtoint: {e}")))?,
                    _ => i64_ty.const_zero(),
                };
                let runtime = match method_name {
                    "contains" => self.generator.get_runtime_fn("mimi_set_contains")?,
                    "insert" => self.generator.get_runtime_fn("mimi_set_insert")?,
                    _ => self.generator.get_runtime_fn("mimi_set_remove")?,
                };
                let call_name = match method_name {
                    "contains" => "set_contains",
                    "insert" => "set_insert",
                    _ => "set_remove",
                };
                let result = self
                    .generator
                    .build_call(
                        runtime,
                        &[
                            BasicMetadataValueEnum::IntValue(handle),
                            BasicMetadataValueEnum::IntValue(value),
                        ],
                        call_name,
                    )?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError(format!("{call_name} returned void")))?;
                if method_name == "contains" {
                    let one = i64_ty.const_int(1, false);
                    let as_bool = self
                        .generator
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::EQ,
                            result.into_int_value(),
                            one,
                            "set_contains_bool",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("set contains cmp: {e}")))?;
                    Ok(Some(as_bool.into()))
                } else {
                    Ok(Some(result))
                }
            }
            "to_list" => {
                let out_len = self.generator.build_alloca(i64_ty, "set_to_list_len")?;
                let func = self.generator.get_runtime_fn("mimi_set_to_list")?;
                let result = self
                    .generator
                    .build_call(
                        func,
                        &[
                            BasicMetadataValueEnum::IntValue(handle),
                            BasicMetadataValueEnum::PointerValue(out_len),
                        ],
                        "set_to_list",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| {
                        CompileError::LlvmError("mimi_set_to_list returned void".into())
                    })?
                    .into_pointer_value();
                let len = self
                    .generator
                    .build_load(i64_ty, out_len, "set_to_list_len_val")?
                    .into_int_value();
                Ok(Some(self.generator.build_list_struct(len, result)?))
            }
            _ => Ok(None),
        }
    }

    /// ABI bridge: if the expected result type is String ({ptr, i64}) but the
    /// builtin returned a raw pointer, wrap it in a string struct.
    fn wrap_builtin_string_result(
        &mut self,
        value: BasicValueEnum<'ctx>,
        expected_ty: &ResolvedTypeId,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let is_string = matches!(
            self.program.resolved_types().get(expected_ty),
            Some(ResolvedType::Primitive(crate::core::PrimitiveType::String))
        );
        if is_string {
            if let BasicValueEnum::PointerValue(ptr) = value {
                // Wrap raw ptr into {ptr, i64} struct. The raw pointer is a
                // NUL-terminated C string with no length channel, so strlen is
                // the ONLY length source — materialize it into the len field
                // NOW (AUDIT FIX A2 follow-up): consumers like len() read the
                // authoritative field with a bounded scan; a len=0 placeholder
                // would make them report 0 for every wrapped builtin string.
                let ptr_ty = self
                    .generator
                    .context
                    .ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.generator.context.i64_type();
                let struct_ty = self.generator.context.struct_type(
                    &[
                        BasicTypeEnum::PointerType(ptr_ty),
                        BasicTypeEnum::IntType(i64_ty),
                    ],
                    false,
                );
                let strlen_fn = self
                    .generator
                    .module
                    .get_function("strlen")
                    .unwrap_or_else(|| {
                        let ty =
                            i64_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(ptr_ty)], false);
                        self.generator.module.add_function(
                            "strlen",
                            ty,
                            Some(inkwell::module::Linkage::External),
                        )
                    });
                let len_val = self.call_strlen(strlen_fn, ptr, "builtin_str_wrap_len")?;
                let alloca = self.generator.build_alloca(struct_ty, "builtin_str_wrap")?;
                let ptr_field = self
                    .generator
                    .builder
                    .build_struct_gep(struct_ty, alloca, 0, "str_ptr_f")
                    .map_err(|e| CompileError::LlvmError(format!("str wrap gep0: {e}")))?;
                let len_field = self
                    .generator
                    .builder
                    .build_struct_gep(struct_ty, alloca, 1, "str_len_f")
                    .map_err(|e| CompileError::LlvmError(format!("str wrap gep1: {e}")))?;
                self.generator.build_store(ptr_field, ptr)?;
                self.generator.build_store(len_field, len_val)?;
                return self.generator.build_load(struct_ty, alloca, "str_wrapped");
            }
        }
        Ok(value)
    }

    /// Call strlen over a pointer whose bytes are NUL-free by construction
    /// (bool literals, snprintf temp buffers, mimi_to_string_f64 results) or
    /// over a raw C-string that carries no length channel at all. Never used
    /// over composed f-string data (that path tracks len through memcpy).
    fn call_strlen(
        &self,
        strlen_fn: inkwell::values::FunctionValue<'ctx>,
        ptr: PointerValue<'ctx>,
        name: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
        self.generator
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(ptr)],
                name,
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("strlen returned void".into()))
            .map(|v| v.into_int_value())
    }

    fn get_or_declare_snprintf(&self) -> inkwell::values::FunctionValue<'ctx> {
        self.generator
            .module
            .get_function("snprintf")
            .unwrap_or_else(|| {
                let ptr = self
                    .generator
                    .context
                    .ptr_type(inkwell::AddressSpace::default());
                let i64 = self.generator.context.i64_type();
                // snprintf returns int (i32), not i64 — CG-C3 (the legacy
                // expr/literal.rs declares it with i32 as well; keep the two
                // declarations signature-consistent within one module).
                let i32_ty = self.generator.context.i32_type();
                let fn_type = i32_ty.fn_type(
                    &[
                        BasicMetadataTypeEnum::from(ptr),
                        BasicMetadataTypeEnum::from(i64),
                        BasicMetadataTypeEnum::from(ptr),
                    ],
                    true, // variadic
                );
                self.generator.module.add_function(
                    "snprintf",
                    fn_type,
                    Some(inkwell::module::Linkage::External),
                )
            })
    }

    fn emit_match(
        &mut self,
        expression: &ResolvedExpr,
        scrutinee: &ResolvedExpr,
        arms: &[crate::core::ir::MatchArm],
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // 0.36.56 (Phase E): single-target flow results are plain state
        // records. Their first field may be f64/bool, and the match does not
        // have a __MultiTarget enum tag; the arm naming the scrutinee's static
        // state binds fields directly from the record, while other state arms
        // are statically dead but still compile with sentinel bindings.
        let scrutinee_display = resolved_type_display_name(self.program, &scrutinee.ty);
        // 0.40.x (L1): a single-state flow match is monomorphic — the scrutinee
        // static type names exactly one state, so there is no discriminant to
        // test. Previously this fast path required EVERY arm to be a `state:`
        // Constructor, which excluded the common `… | _ =>` fallback (and a
        // leading `_` arm) and forced those programs into the general path that
        // looks the flow-state variant up in the enum catalog and hard-errors
        // (E0722). Accept wildcard/binding arms here; emit_static_flow_match
        // treats them as the (unreachable) fallback and still compiles them.
        let is_static_flow = scrutinee_display.starts_with("state:")
            && arms.iter().all(|arm| match &arm.pattern.kind {
                crate::core::ir::ResolvedPatternKind::Constructor { variant, .. } => {
                    variant.0.starts_with("state:")
                }
                crate::core::ir::ResolvedPatternKind::Wildcard => true,
                crate::core::ir::ResolvedPatternKind::Binding { .. } => true,
                _ => false,
            });
        if is_static_flow {
            return self.emit_static_flow_match(
                expression,
                scrutinee,
                arms,
                frame,
                scrutinee_display,
            );
        }

        let result_type = self.lower_type(&expression.ty)?;
        let result_alloca = self.generator.build_alloca(result_type, "match_result")?;
        let scrutinee_val = self.emit_expr(scrutinee, frame)?;
        // If the scrutinee is a pointer (e.g. an alloca), load the struct
        // value so Constructor patterns can extract fields.
        let scrutinee_val = if let BasicValueEnum::PointerValue(pv) = scrutinee_val {
            let sty = self.lower_type(&scrutinee.ty)?;
            self.generator.build_load(sty, pv, "match_scrutinee")?
        } else {
            scrutinee_val
        };

        let function = self.current_function()?;
        let merge_bb = self
            .generator
            .context
            .append_basic_block(function, "match_merge");

        // Track the fallthrough block: where the next arm's comparison is emitted.
        // Initially it's the current block (entry). After each non-wildcard arm,
        // it becomes the `next_bb` that the cond_br falls through to.
        let mut fallthrough_bb = self
            .generator
            .builder
            .get_insert_block()
            .ok_or_else(|| CompileError::LlvmError("no insert block for match".into()))?;

        for (arm_index, arm) in arms.iter().enumerate() {
            let is_last = arm_index == arms.len() - 1;

            let always_matches = matches!(
                arm.pattern.kind,
                ResolvedPatternKind::Wildcard | ResolvedPatternKind::Binding { .. }
            ) && arm.guard.is_none();

            let arm_bb = self
                .generator
                .context
                .append_basic_block(function, &format!("match_arm{arm_index}"));

            // Position at fallthrough to emit comparison or unconditional br.
            self.generator.builder.position_at_end(fallthrough_bb);

            if !always_matches {
                let pattern_matches = match &arm.pattern.kind {
                    ResolvedPatternKind::Literal(lit) => {
                        let lit_val = self.emit_literal(&arm.pattern.ty, lit)?;
                        let cmp = self.generator.compile_binop(
                            BinOp::EqCmp,
                            scrutinee_val,
                            lit_val,
                            None,
                        )?;
                        self.ensure_bool(cmp)?
                    }
                    ResolvedPatternKind::Wildcard | ResolvedPatternKind::Binding { .. } => {
                        self.generator.context.bool_type().const_all_ones()
                    }
                    // 0.32.6: Constructor pattern — check the discriminant
                    // (field 0) of the Option/Result struct.
                    // 0.32.12: Extended to user-defined enum variants
                    // ({i32 tag, i64 payload} — compare tag == ordinal).
                    ResolvedPatternKind::Constructor { variant, .. } => {
                        // 0.32.15: Newtype constructors always match.
                        if self.is_newtype_variant(variant) {
                            // No tag check — newtype has exactly one variant.
                            self.generator.context.bool_type().const_all_ones()
                        } else {
                            let variant_name = self.lookup_variant_name(variant)?;
                            // Get the scrutinee as a struct value. If it's a
                            // pointer (alloca), load it first.
                            let sv = match scrutinee_val {
                                BasicValueEnum::StructValue(sv) => sv,
                                BasicValueEnum::PointerValue(pv) => {
                                    let sty = self.lower_type(&scrutinee.ty)?;
                                    self.generator
                                        .build_load(sty, pv, "ctor_scrutinee")?
                                        .into_struct_value()
                                }
                                _ => {
                                    return Err(CompileError::Unsupported(
                                        "constructor match on non-struct scrutinee".into(),
                                    ))
                                }
                            };
                            // Extract discriminant (field 0).
                            let disc = self
                                .generator
                                .builder
                                .build_extract_value(sv, 0, "ctor_disc")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("extract disc: {e}"))
                                })?;
                            let disc_int = match disc {
                                BasicValueEnum::IntValue(iv) => iv,
                                other => {
                                    return Err(CompileError::Unsupported(format!(
                                        "constructor discriminant is not an integer: {other:?}"
                                    )))
                                }
                            };
                            // Determine the expected discriminant value.
                            let is_builtin =
                                matches!(variant_name.as_str(), "Some" | "Ok" | "None" | "Err");
                            if is_builtin {
                                // Built-in Option/Result: i1 discriminant.
                                let bool_ty = self.generator.context.bool_type();
                                let disc_expected = matches!(variant_name.as_str(), "Some" | "Ok");
                                let expected = bool_ty.const_int(disc_expected as u64, false);
                                // Ensure disc is i1 for comparison.
                                let disc_i1 = if disc_int.get_type().get_bit_width() > 1 {
                                    self.generator
                                        .builder
                                        .build_int_truncate(disc_int, bool_ty, "disc_trunc")
                                        .map_err(|e| {
                                            CompileError::LlvmError(format!("disc trunc: {e}"))
                                        })?
                                } else {
                                    disc_int
                                };
                                self.generator
                                    .builder
                                    .build_int_compare(
                                        inkwell::IntPredicate::EQ,
                                        disc_i1,
                                        expected,
                                        "ctor_cmp",
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("ctor cmp: {e}"))
                                    })?
                            } else {
                                // User-defined enum: i32 tag == ordinal.
                                let ordinal = self.enum_variant_ordinal(variant)?;
                                let i32_ty = self.generator.context.i32_type();
                                let expected = i32_ty.const_int(ordinal, false);
                                // Ensure disc is i32.
                                let disc_i32 = if disc_int.get_type().get_bit_width() != 32 {
                                    if disc_int.get_type().get_bit_width() > 32 {
                                        self.generator
                                            .builder
                                            .build_int_truncate(disc_int, i32_ty, "disc_trunc32")
                                            .map_err(|e| {
                                                CompileError::LlvmError(format!("disc trunc: {e}"))
                                            })?
                                    } else {
                                        self.generator
                                            .builder
                                            .build_int_z_extend(disc_int, i32_ty, "disc_zext32")
                                            .map_err(|e| {
                                                CompileError::LlvmError(format!("disc zext: {e}"))
                                            })?
                                    }
                                } else {
                                    disc_int
                                };
                                self.generator
                                    .builder
                                    .build_int_compare(
                                        inkwell::IntPredicate::EQ,
                                        disc_i32,
                                        expected,
                                        "enum_cmp",
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("enum cmp: {e}"))
                                    })?
                            }
                        } // end else (non-newtype Constructor)
                    }

                    _ => {
                        return Err(CompileError::Unsupported(
                            "match pattern escaped resolved native eligibility".into(),
                        ))
                    }
                };

                // §6-#58 (audit 2026-08-05): match-guard short-circuit. A guard is
                // evaluated ONLY after the pattern matched — Rust match-guard
                // semantics. Pre-fix emit_expr ran the guard unconditionally
                // here, so a side effect inside the guard (a function call/
                // println/...) executed on the fallthrough path even when the
                // pattern did NOT match, diverging from the base/eval semantics.
                if let Some(guard) = &arm.guard {
                    let guard_bb = self
                        .generator
                        .context
                        .append_basic_block(function, &format!("match_guard{arm_index}"));
                    if is_last {
                        self.generator
                            .build_cond_br(pattern_matches, guard_bb, merge_bb)?;
                    } else {
                        let next_bb = self
                            .generator
                            .context
                            .append_basic_block(function, &format!("match_next{arm_index}"));
                        self.generator
                            .build_cond_br(pattern_matches, guard_bb, next_bb)?;
                        fallthrough_bb = next_bb;
                    }
                    self.generator.builder.position_at_end(guard_bb);
                    let guard_val = self.emit_expr(guard, frame)?;
                    let guard_bool = self.ensure_bool(guard_val)?;
                    // guard failure falls through to the same next-arm as
                    // a pattern mismatch.
                    if is_last {
                        self.generator.build_cond_br(guard_bool, arm_bb, merge_bb)?;
                    } else {
                        let next_bb = fallthrough_bb;
                        self.generator.build_cond_br(guard_bool, arm_bb, next_bb)?;
                    }
                } else {
                    // No guard: pattern-match decides the branch.
                    let cond = pattern_matches;
                    if is_last {
                        self.generator.build_cond_br(cond, arm_bb, merge_bb)?;
                    } else {
                        let next_bb = self
                            .generator
                            .context
                            .append_basic_block(function, &format!("match_next{arm_index}"));
                        self.generator.build_cond_br(cond, arm_bb, next_bb)?;
                        fallthrough_bb = next_bb;
                    }
                };
            } else {
                // Unconditional match.
                self.generator.build_br(arm_bb)?;
            }

            // Emit arm body.
            self.generator.builder.position_at_end(arm_bb);
            match &arm.pattern.kind {
                ResolvedPatternKind::Binding {
                    by_reference: None, ..
                } => {
                    let callable_body = &self
                        .program
                        .callable(&frame.owner)
                        .ok_or_else(|| {
                            CompileError::Unsupported("callable absent for match binding".into())
                        })?
                        .body;
                    self.bind_pattern(callable_body, &arm.pattern, scrutinee_val, frame)?;
                }
                // 0.32.6: Constructor pattern — extract payload fields and
                // bind sub-patterns. Option: {i1, T}; Result: {i1, T, E}.
                ResolvedPatternKind::Constructor { variant, fields } => {
                    // 0.32.15: Newtype Constructor — the scrutinee IS the
                    // inner value. Bind directly to sub-patterns.
                    if self.is_newtype_variant(variant) {
                        let callable_body = &self
                            .program
                            .callable(&frame.owner)
                            .ok_or_else(|| {
                                CompileError::Unsupported(
                                    "callable absent for newtype binding".into(),
                                )
                            })?
                            .body;
                        for (_field_id, sub_pattern) in fields.iter() {
                            self.bind_pattern(callable_body, sub_pattern, scrutinee_val, frame)?;
                        }
                    } else {
                        let variant_name = self.lookup_variant_name(variant)?;
                        // Get the scrutinee as a struct value.
                        let sv = match scrutinee_val {
                            BasicValueEnum::StructValue(sv) => sv,
                            BasicValueEnum::PointerValue(pv) => {
                                let sty = self.lower_type(&scrutinee.ty)?;
                                self.generator
                                    .build_load(sty, pv, "ctor_bind_scrutinee")?
                                    .into_struct_value()
                            }
                            _ => {
                                return Err(CompileError::Unsupported(
                                    "constructor match on non-struct scrutinee".into(),
                                ))
                            }
                        };
                        // Determine which struct field holds the payload.
                        // Built-in: Some/Ok → field 1; Err → field 2; None → no payload.
                        // User-defined enum: {i32 tag, i64 payload} → field 1.
                        let is_builtin_variant =
                            matches!(variant_name.as_str(), "Some" | "Ok" | "Err" | "None");
                        let payload_field_index: Option<u32> = if is_builtin_variant {
                            match variant_name.as_str() {
                                "Some" | "Ok" => Some(1),
                                "Err" => Some(2),
                                "None" => None,
                                _ => unreachable!(),
                            }
                        } else {
                            // 0.32.12: User-defined enum variants have payload
                            // at field 1 (the i64 slot). For unit variants,
                            // fields is empty so the loop below won't execute.
                            Some(1)
                        };
                        let callable_body = &self
                            .program
                            .callable(&frame.owner)
                            .ok_or_else(|| {
                                CompileError::Unsupported("callable absent for ctor binding".into())
                            })?
                            .body;
                        // 0.32.12: User-defined enum payload decoding.
                        // The struct is {i32 tag, i64 payload}. For variants
                        // with fields, decode the i64 payload:
                        //   - 1 field: bitcast i64 → field LLVM type
                        //   - 2+ fields: inttoptr → load heap struct → extract
                        if !is_builtin_variant && !fields.is_empty() {
                            let raw_payload = self
                                .generator
                                .builder
                                .build_extract_value(sv, 1, "enum_raw_payload")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("extract enum payload: {e}"))
                                })?
                                .into_int_value();
                            if fields.len() == 1 {
                                // Single field: bitcast i64 to the field type.
                                let (_field_id, sub_pattern) = &fields[0];
                                let field_llvm_ty = self.lower_type(&sub_pattern.ty)?;
                                // Custom enum string payloads follow the compact
                                // enum packing convention: the i64 is
                                // ptrtoint(heap_box{ptr,len}), not a raw C
                                // string pointer as in list data.
                                let decoded = match field_llvm_ty {
                                    BasicTypeEnum::StructType(sty) => {
                                        let fields = sty.get_field_types();
                                        let is_string_shape = fields.len() == 2
                                            && matches!(&fields[0], BasicTypeEnum::PointerType(_))
                                            && matches!(&fields[1], BasicTypeEnum::IntType(bit)
                                                if bit.get_bit_width() == 64);
                                        if is_string_shape {
                                            let ptr = self
                                                .generator
                                                .builder
                                                .build_int_to_ptr(
                                                    raw_payload,
                                                    self.generator
                                                        .context
                                                        .ptr_type(inkwell::AddressSpace::default()),
                                                    "enum_str_payload_ptr",
                                                )
                                                .map_err(|e| {
                                                    CompileError::LlvmError(format!(
                                                        "enum string inttoptr: {e}"
                                                    ))
                                                })?;
                                            self.generator
                                                .builder
                                                .build_load(
                                                    BasicTypeEnum::StructType(sty),
                                                    ptr,
                                                    "enum_str_payload_struct",
                                                )
                                                .map_err(|e| {
                                                    CompileError::LlvmError(format!(
                                                        "enum string payload load: {e}"
                                                    ))
                                                })?
                                        } else {
                                            self.convert_list_elem_i64(raw_payload, field_llvm_ty)?
                                        }
                                    }
                                    _ => self.convert_list_elem_i64(raw_payload, field_llvm_ty)?,
                                };
                                self.bind_pattern(callable_body, sub_pattern, decoded, frame)?;
                            } else {
                                // Multi-field: inttoptr + load heap struct.
                                let mut field_tys = Vec::with_capacity(fields.len());
                                for (_fid, sp) in fields.iter() {
                                    field_tys.push(self.lower_type(&sp.ty)?);
                                }
                                let heap_struct_ty =
                                    self.generator.context.struct_type(&field_tys, false);
                                let ptr = self
                                    .generator
                                    .builder
                                    .build_int_to_ptr(
                                        raw_payload,
                                        self.generator
                                            .context
                                            .ptr_type(inkwell::AddressSpace::default()),
                                        "enum_payload_ptr",
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("enum inttoptr: {e}"))
                                    })?;
                                let loaded = self
                                    .generator
                                    .builder
                                    .build_load(
                                        BasicTypeEnum::StructType(heap_struct_ty),
                                        ptr,
                                        "enum_payload_struct",
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("enum payload load: {e}"))
                                    })?
                                    .into_struct_value();
                                for (i, (_field_id, sub_pattern)) in fields.iter().enumerate() {
                                    let field_val = self
                                        .generator
                                        .builder
                                        .build_extract_value(
                                            loaded,
                                            i as u32,
                                            &format!("enum_field_{i}"),
                                        )
                                        .map_err(|e| {
                                            CompileError::LlvmError(format!(
                                                "extract enum field {i}: {e}"
                                            ))
                                        })?;
                                    self.bind_pattern(
                                        callable_body,
                                        sub_pattern,
                                        field_val,
                                        frame,
                                    )?;
                                }
                            }
                        } else {
                            // Built-in Option/Result payload extraction (existing logic).
                            for (i, (_field_id, sub_pattern)) in fields.iter().enumerate() {
                                let field_idx = payload_field_index
                                    .map(|base| base + i as u32)
                                    .unwrap_or(i as u32);
                                let payload_val = self
                                    .generator
                                    .builder
                                    .build_extract_value(
                                        sv,
                                        field_idx,
                                        &format!("ctor_payload_{field_idx}"),
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!(
                                            "extract payload field {field_idx}: {e}"
                                        ))
                                    })?;
                                // For built-in Err, the payload at index 2 is an
                                // opaque handle per the PRODUCER's ABI. Two
                                // conventions exist side by side:
                                //   - fails-transition flow errors
                                //     Result<T, (Source, E)> (compile_try_rejected):
                                //     a heap POINTER to a {i64, i64} handle pair
                                //     (each field ptrtoint'd / widened to i64);
                                //   - plain Err payloads (Result<string,string>,
                                //     Result<T, custom-enum>): a heap {ptr,i64}
                                //     string struct / {i32,i64} enum handle whose
                                //     target type is the binding's declared type.
                                // 0.36.37: decode the tuple-handle convention
                                // per element (struct → inttoptr + load, pointer →
                                // inttoptr, int → truncate) and bind the element
                                // patterns directly — loading the inline tuple
                                // struct type from handle memory misread both
                                // fields (garbage string/state pointers → SIGSEGV
                                // in flow_order_system's Err((src, e)) arm).
                                if variant_name.as_str() == "Err"
                                    && matches!(payload_val, BasicValueEnum::IntValue(_))
                                    && matches!(sub_pattern.kind, ResolvedPatternKind::Tuple(_))
                                {
                                    let i64_ty = self.generator.context.i64_type();
                                    let pair_ty = self.generator.context.struct_type(
                                        &[
                                            BasicTypeEnum::IntType(i64_ty),
                                            BasicTypeEnum::IntType(i64_ty),
                                        ],
                                        false,
                                    );
                                    let pair_ptr = self
                                        .generator
                                        .builder
                                        .build_int_to_ptr(
                                            payload_val.into_int_value(),
                                            self.generator
                                                .context
                                                .ptr_type(inkwell::AddressSpace::default()),
                                            "err_tuple_ptr",
                                        )
                                        .map_err(|e| {
                                            CompileError::LlvmError(format!(
                                                "inttoptr err tuple: {e}"
                                            ))
                                        })?;
                                    let pair = self
                                        .generator
                                        .builder
                                        .build_load(pair_ty, pair_ptr, "err_tuple_pair")
                                        .map_err(|e| {
                                            CompileError::LlvmError(format!(
                                                "load err tuple pair: {e}"
                                            ))
                                        })?
                                        .into_struct_value();
                                    if let ResolvedPatternKind::Tuple(elems) = &sub_pattern.kind {
                                        for (ei, elem) in elems.iter().enumerate() {
                                            let handle = self
                                                .generator
                                                .builder
                                                .build_extract_value(
                                                    pair,
                                                    ei as u32,
                                                    &format!("err_pair_field_{ei}"),
                                                )
                                                .map_err(|e| {
                                                    CompileError::LlvmError(format!(
                                                        "extract err pair field {ei}: {e}"
                                                    ))
                                                })?
                                                .into_int_value();
                                            let elem_llvm = self.lower_type(&elem.ty)?;
                                            let elem_val: BasicValueEnum<'ctx> = match elem_llvm
                                            {
                                                BasicTypeEnum::StructType(_)
                                                | BasicTypeEnum::ArrayType(_) => {
                                                    let elem_ptr = self
                                                        .generator
                                                        .builder
                                                        .build_int_to_ptr(
                                                            handle,
                                                            self.generator.context.ptr_type(
                                                                inkwell::AddressSpace::default(),
                                                            ),
                                                            &format!("err_elem_{ei}_ptr"),
                                                        )
                                                        .map_err(|e| {
                                                            CompileError::LlvmError(format!(
                                                                "inttoptr err elem {ei}: {e}"
                                                            ))
                                                        })?;
                                                    self.generator
                                                        .builder
                                                        .build_load(
                                                            elem_llvm,
                                                            elem_ptr,
                                                            &format!("err_elem_{ei}_val"),
                                                        )
                                                        .map_err(|e| {
                                                            CompileError::LlvmError(format!(
                                                                "load err elem {ei}: {e}"
                                                            ))
                                                        })?
                                                }
                                                BasicTypeEnum::PointerType(_) => {
                                                    let elem_ptr = self
                                                        .generator
                                                        .builder
                                                        .build_int_to_ptr(
                                                            handle,
                                                            elem_llvm.into_pointer_type(),
                                                            &format!("err_elem_{ei}_ptr"),
                                                        )
                                                        .map_err(|e| {
                                                            CompileError::LlvmError(format!(
                                                                "inttoptr err elem {ei}: {e}"
                                                            ))
                                                        })?;
                                                    BasicValueEnum::PointerValue(elem_ptr)
                                                }
                                                BasicTypeEnum::IntType(it) => {
                                                    if it.get_bit_width() < 64 {
                                                        self.generator
                                                            .builder
                                                            .build_int_truncate(
                                                                handle,
                                                                it,
                                                                &format!("err_elem_{ei}_trunc"),
                                                            )
                                                            .map_err(|e| {
                                                                CompileError::LlvmError(format!(
                                                                    "trunc err elem {ei}: {e}"
                                                                ))
                                                            })?
                                                            .into()
                                                    } else {
                                                        handle.into()
                                                    }
                                                }
                                                _ => {
                                                    return Err(CompileError::Unsupported(
                                                        format!(
                                                            "flow Err tuple element {ei} has an unsupported decoded LLVM type"
                                                        ),
                                                    ))
                                                }
                                            };
                                            self.bind_pattern(
                                                callable_body,
                                                elem,
                                                elem_val,
                                                frame,
                                            )?;
                                        }
                                    }
                                    continue;
                                }
                                let decoded_val = if variant_name.as_str() == "Err"
                                    && matches!(payload_val, BasicValueEnum::IntValue(_))
                                {
                                    let target_llvm = self.lower_type(&sub_pattern.ty)?;
                                    match target_llvm {
                                        BasicTypeEnum::StructType(_) => {
                                            let ptr = self
                                                .generator
                                                .builder
                                                .build_int_to_ptr(
                                                    payload_val.into_int_value(),
                                                    self.generator
                                                        .context
                                                        .ptr_type(inkwell::AddressSpace::default()),
                                                    "err_payload_ptr",
                                                )
                                                .map_err(|e| {
                                                    CompileError::LlvmError(format!(
                                                        "inttoptr err: {e}"
                                                    ))
                                                })?;
                                            self.generator
                                                .builder
                                                .build_load(target_llvm, ptr, "err_payload_val")
                                                .map_err(|e| {
                                                    CompileError::LlvmError(format!(
                                                        "load err payload: {e}"
                                                    ))
                                                })?
                                        }
                                        BasicTypeEnum::FloatType(ft)
                                            if ft.get_bit_width() == 64 =>
                                        {
                                            self.generator
                                                .builder
                                                .build_bit_cast(
                                                    payload_val.into_int_value(),
                                                    BasicTypeEnum::FloatType(ft),
                                                    "err_f64_bits_back",
                                                )
                                                .map_err(|e| {
                                                    CompileError::LlvmError(format!(
                                                        "err f64 bitcast: {e}"
                                                    ))
                                                })?
                                        }
                                        _ => payload_val,
                                    }
                                } else {
                                    payload_val
                                };
                                self.bind_pattern(callable_body, sub_pattern, decoded_val, frame)?;
                            }
                        } // end else (builtin variant payload extraction)
                    } // end else (non-newtype Constructor)
                }
                _ => {}
            }
            let arm_value = self.emit_expr(&arm.body, frame)?;
            if !self.current_block_terminated() {
                let arm_value = self.coerce_to(arm_value, result_type)?;
                self.generator.build_store(result_alloca, arm_value)?;
                self.generator.build_br(merge_bb)?;
            }

            if always_matches {
                break;
            }
        }

        self.generator.builder.position_at_end(merge_bb);
        self.generator
            .build_load(result_type, result_alloca, "match_val")
    }

    /// 0.36.56 (Phase E): single-target flow-result match lowering.
    ///
    /// The scrutinee's static type is exactly one state of a flow (a plain
    /// record, no `__MultiTarget` enum). The arm naming that static state
    /// binds its fields directly from the record; all other state arms are
    /// statically dead but still compile with sentinel bindings so the match
    /// result type unifies.
    fn emit_static_flow_match(
        &mut self,
        expression: &ResolvedExpr,
        scrutinee: &ResolvedExpr,
        arms: &[crate::core::ir::MatchArm],
        frame: &mut ResolvedFrame<'ctx>,
        static_display: String,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let result_type = self.lower_type(&expression.ty)?;
        let result_alloca = self.generator.build_alloca(result_type, "match_result")?;
        let scrutinee_val = self.emit_expr(scrutinee, frame)?;
        let scrutinee_val = if let BasicValueEnum::PointerValue(pv) = scrutinee_val {
            let sty = self.lower_type(&scrutinee.ty)?;
            self.generator
                .build_load(sty, pv, "static_match_scrutinee")?
        } else {
            scrutinee_val
        };
        let sv = match scrutinee_val {
            BasicValueEnum::StructValue(sv) => sv,
            other => {
                return Err(CompileError::Unsupported(format!(
                    "static flow match scrutinee must be a record struct, got {other:?}"
                )))
            }
        };

        let function = self.current_function()?;
        let merge_bb = self
            .generator
            .context
            .append_basic_block(function, "static_match_merge");
        let callable_body = &self
            .program
            .callable(&frame.owner)
            .ok_or_else(|| {
                CompileError::Unsupported("callable absent for static flow match".into())
            })?
            .body;

        let mut fallthrough_bb = self.generator.builder.get_insert_block().ok_or_else(|| {
            CompileError::LlvmError("no insert block for static flow match".into())
        })?;

        // A single-state flow match is monomorphic: the scrutinee's static type
        // names exactly one state, so at runtime the value is always that state.
        // Match arms are therefore evaluated in order; the first arm whose
        // pattern can match the known state wins. A wildcard/binding arm always
        // matches; a `state:` Constructor matches only when its variant equals
        // the static state. An arm is unreachable only if an earlier arm already
        // matches unconditionally (no guard). Guarded matching arms stay
        // reachable (the guard may fail) and fall through.
        let pattern_matches_static = |arm: &crate::core::ir::MatchArm| match &arm.pattern.kind {
            crate::core::ir::ResolvedPatternKind::Constructor { variant, .. } => {
                variant.0 == static_display
            }
            crate::core::ir::ResolvedPatternKind::Wildcard => true,
            crate::core::ir::ResolvedPatternKind::Binding { .. } => true,
            _ => false,
        };

        for (arm_index, arm) in arms.iter().enumerate() {
            let is_last = arm_index == arms.len() - 1;
            // Unreachable if an earlier arm matches the known state without a
            // guard (it would always take first).
            let unconditionally_taken_before =
                (0..arm_index).any(|j| pattern_matches_static(&arms[j]) && arms[j].guard.is_none());
            let pms = pattern_matches_static(arm);
            let is_live = pms && arm.guard.is_none() && !unconditionally_taken_before;
            let is_live_guarded = pms && arm.guard.is_some() && !unconditionally_taken_before;
            let arm_bb = self
                .generator
                .context
                .append_basic_block(function, &format!("static_arm{arm_index}"));

            if !is_live && !is_live_guarded {
                // Statically dead arm: fall through to the next arm's dispatch.
                let next_bb = self
                    .generator
                    .context
                    .append_basic_block(function, &format!("static_dead_next{arm_index}"));
                self.generator.builder.position_at_end(fallthrough_bb);
                self.generator.build_br(next_bb)?;
                fallthrough_bb = next_bb;
            } else if is_live_guarded {
                // Live guarded arm: bind state fields (or the whole record for a
                // binding arm) before evaluating the guard, mirroring legacy
                // match semantics.
                let guard_bb = self
                    .generator
                    .context
                    .append_basic_block(function, &format!("static_guard{arm_index}"));
                let next_bb = if is_last {
                    merge_bb
                } else {
                    self.generator
                        .context
                        .append_basic_block(function, &format!("static_guard_next{arm_index}"))
                };
                self.generator.builder.position_at_end(fallthrough_bb);
                self.generator.build_br(guard_bb)?;
                self.generator.builder.position_at_end(guard_bb);
                self.bind_static_flow_arm_live(arm, sv, callable_body, frame)?;
                let guard_val = self.emit_expr(arm.guard.as_ref().unwrap(), frame)?;
                let guard_bool = self.ensure_bool(guard_val)?;
                self.generator.build_cond_br(guard_bool, arm_bb, next_bb)?;
                if !is_last {
                    fallthrough_bb = next_bb;
                }
            } else {
                // Live arm without guard: always taken.
                let next_bb = if is_last {
                    None
                } else {
                    Some(
                        self.generator
                            .context
                            .append_basic_block(function, &format!("static_next{arm_index}")),
                    )
                };
                self.generator.builder.position_at_end(fallthrough_bb);
                self.generator.build_br(arm_bb)?;
                if let Some(nb) = next_bb {
                    fallthrough_bb = nb;
                }
            }

            // Emit the arm body. A guarded live arm already bound its variables
            // in the guard block; an unguarded live arm binds here; a dead arm
            // binds sentinels (its body is unreachable).
            self.generator.builder.position_at_end(arm_bb);
            if is_live {
                self.bind_static_flow_arm_live(arm, sv, callable_body, frame)?;
            } else if !is_live_guarded {
                self.bind_static_flow_arm_dead(arm, sv, callable_body, frame)?;
            }
            let arm_value = self.emit_expr(&arm.body, frame)?;
            if !self.current_block_terminated() {
                let arm_value = self.coerce_to(arm_value, result_type)?;
                self.generator.build_store(result_alloca, arm_value)?;
                self.generator.build_br(merge_bb)?;
            }
        }

        // Any remaining fallthrough block (from a dead/guarded last arm) must
        // be terminated. It is unreachable in practice but still a CFG edge.
        if fallthrough_bb.get_terminator().is_none() {
            self.generator.builder.position_at_end(fallthrough_bb);
            self.generator.build_br(merge_bb)?;
        }

        self.generator.builder.position_at_end(merge_bb);
        self.generator
            .build_load(result_type, result_alloca, "static_match_val")
    }

    /// Bind pattern variables for a single flow-state match arm from a plain
    /// state record. For the live arm, extract real fields; for dead arms use
    /// zero-initialized sentinels of the bound pattern's type.
    fn bind_flow_arm_variables(
        &mut self,
        arm: &crate::core::ir::MatchArm,
        sv: inkwell::values::StructValue<'ctx>,
        is_static: bool,
        callable_body: &crate::core::ir::ResolvedBody,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<(), CompileError> {
        let crate::core::ir::ResolvedPatternKind::Constructor { fields, .. } = &arm.pattern.kind
        else {
            return Ok(());
        };
        for (field_id, sub_pattern) in fields {
            let field_name = self.lookup_field_name(field_id)?;
            let field_idx = self.lookup_field_index(field_id, &field_name)?;
            let val = if is_static {
                self.generator
                    .builder
                    .build_extract_value(sv, field_idx, "static_flow_field")
                    .map_err(|e| {
                        CompileError::LlvmError(format!("static flow field extract: {e}"))
                    })?
            } else {
                let ty = self.lower_type(&sub_pattern.ty)?;
                ty.const_zero().into()
            };
            self.bind_pattern(callable_body, sub_pattern, val, frame)?;
        }
        Ok(())
    }

    /// Bind the variables of a *live* single-state flow match arm. For a
    /// `state:` Constructor arm, extract the real record fields; for a binding
    /// fallback arm, bind the whole scrutinee record; for a wildcard arm, bind
    /// nothing. (Live = the only arm whose pattern can actually match.)
    fn bind_static_flow_arm_live(
        &mut self,
        arm: &crate::core::ir::MatchArm,
        sv: inkwell::values::StructValue<'ctx>,
        callable_body: &crate::core::ir::ResolvedBody,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<(), CompileError> {
        match &arm.pattern.kind {
            crate::core::ir::ResolvedPatternKind::Constructor { .. } => {
                self.bind_flow_arm_variables(arm, sv, true, callable_body, frame)
            }
            crate::core::ir::ResolvedPatternKind::Binding { .. } => {
                self.bind_pattern(callable_body, &arm.pattern, sv.into(), frame)
            }
            crate::core::ir::ResolvedPatternKind::Wildcard => Ok(()),
            _ => Ok(()),
        }
    }

    /// Bind the variables of a *statically dead* single-state flow match arm.
    /// Its body is unreachable, but it must still compile. A `state:`
    /// Constructor arm binds sentinel fields; a binding fallback binds the
    /// whole record (harmless, never executed); a wildcard binds nothing.
    fn bind_static_flow_arm_dead(
        &mut self,
        arm: &crate::core::ir::MatchArm,
        sv: inkwell::values::StructValue<'ctx>,
        callable_body: &crate::core::ir::ResolvedBody,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<(), CompileError> {
        match &arm.pattern.kind {
            crate::core::ir::ResolvedPatternKind::Constructor { .. } => {
                self.bind_flow_arm_variables(arm, sv, false, callable_body, frame)
            }
            crate::core::ir::ResolvedPatternKind::Binding { .. } => {
                self.bind_pattern(callable_body, &arm.pattern, sv.into(), frame)
            }
            crate::core::ir::ResolvedPatternKind::Wildcard => Ok(()),
            _ => Ok(()),
        }
    }

    fn emit_unary(
        &mut self,
        op: ResolvedUnaryOp,
        value: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (op, value) {
            (ResolvedUnaryOp::Negate, BasicValueEnum::IntValue(value)) => {
                let zero = value.get_type().const_zero();
                self.generator
                    .compile_binop(BinOp::Sub, zero.into(), value.into(), None)
            }
            (ResolvedUnaryOp::Negate, BasicValueEnum::FloatValue(value)) => {
                // 0.39.136 (L1): `fneg` sign-flip, not `0.0 - x` — subtraction
                // loses negative zero (-(0.0) printed "0" natively, "-0" on
                // the VM). Mirrors operator.rs UnOp::Neg float arm.
                self.generator
                    .builder
                    .build_float_neg(value, "resolved_fneg")
                    .map(BasicValueEnum::from)
                    .map_err(|e| CompileError::LlvmError(format!("fneg error: {e}")))
            }
            // H-16 (full-audit 2026-08-05, HIGH): builtin predicates return
            // i64 0/1 in the LLVM ABI (operator.rs:201 ABI note), so a bare
            // `build_not` on a wide integer flips bits instead of truth value:
            // `not 1` became -2, and the following `!= 0` compared true —
            // the branch direction inverted (PoC: `not contains(xs, 2)` took
            // the then-arm). Legacy normalizes wide bools via `x == 0`
            // (operator.rs:274-283). Mirror that: i1 passes through build_not,
            // wider integers normalize to an i1 boolean first.
            (ResolvedUnaryOp::Not, BasicValueEnum::IntValue(value)) => {
                if value.get_type().get_bit_width() == 1 {
                    self.generator
                        .builder
                        .build_not(value, "resolved_not")
                        .map(BasicValueEnum::from)
                        .map_err(|error| CompileError::LlvmError(format!("not error: {error}")))
                } else {
                    let zero = value.get_type().const_int(0, false);
                    self.generator
                        .builder
                        .build_int_compare(inkwell::IntPredicate::EQ, value, zero, "resolved_not")
                        .map(BasicValueEnum::from)
                        .map_err(|error| CompileError::LlvmError(format!("not error: {error}")))
                }
            }
            _ => Err(CompileError::Unsupported(
                "resolved unary operator is not in the scalar-leaf slice".into(),
            )),
        }
    }

    /// C-4: short-circuit lowering for `and`/`or`, mirroring the legacy
    /// emitter (`compile_short_circuit_expr`, operator.rs) and the bytecode
    /// VM (`compile_short_circuit`, interp/bytecode/compiler.rs):
    ///
    /// - `l and r`: evaluate `l`; falsy → constant `false` without touching
    ///   `r`; truthy → the value of `r` (evaluated only on this path).
    /// - `l or r`: evaluate `l`; truthy → constant `true` without touching
    ///   `r`; falsy → the value of `r`.
    ///
    /// The checker restricts `and`/`or` operands to bool (E0202); builtin
    /// predicate results that surface as i64-bool are normalized by
    /// `ensure_bool`, which mirrors the VM's `is_truthy`.
    fn emit_short_circuit(
        &mut self,
        op: ResolvedBinaryOp,
        left: &ResolvedExpr,
        right: &ResolvedExpr,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let function = self.current_function()?;
        let lhs = self.emit_expr(left, frame)?;
        let cond = self.ensure_bool(lhs)?;

        let rhs_bb = self
            .generator
            .context
            .append_basic_block(function, "sc_rhs");
        let const_bb = self
            .generator
            .context
            .append_basic_block(function, "sc_const");
        let merge_bb = self
            .generator
            .context
            .append_basic_block(function, "sc_merge");

        // and: truthy LHS → evaluate RHS; falsy LHS → constant false.
        // or:  truthy LHS → constant true;  falsy LHS → evaluate RHS.
        let (truthy_bb, falsy_bb) = match op {
            ResolvedBinaryOp::LogicalAnd => (rhs_bb, const_bb),
            ResolvedBinaryOp::LogicalOr => (const_bb, rhs_bb),
            _ => {
                return Err(CompileError::Unsupported(format!(
                    "short-circuit lowering requires `and`/`or`, got {op:?}"
                )))
            }
        };
        self.generator.build_cond_br(cond, truthy_bb, falsy_bb)?;

        // RHS arm — compiled ONLY on the branch that needs it.
        self.generator.builder.position_at_end(rhs_bb);
        let rhs = self.emit_expr(right, frame)?;
        let result_ty = rhs.get_type();
        let rhs_reaches = !self.current_block_terminated();
        if rhs_reaches {
            self.generator.build_br(merge_bb)?;
        }
        let rhs_end_bb = rhs_reaches
            .then(|| self.generator.builder.get_insert_block())
            .flatten();

        // Constant arm: `false` for `and`, `true` for `or`, in the RHS
        // value's own type so the merge phi is well-typed. The VM yields
        // Bool(false/true) here; for wider i64-bool RHS values the 0/1
        // constant is truthiness-equivalent (legacy short_circuit_const).
        self.generator.builder.position_at_end(const_bb);
        let const_bit = match op {
            ResolvedBinaryOp::LogicalAnd => 0u64, // LHS falsy → false
            ResolvedBinaryOp::LogicalOr => 1u64,  // LHS truthy → true
            _ => unreachable!("guarded above"),
        };
        let const_val: BasicValueEnum<'ctx> = match result_ty {
            BasicTypeEnum::IntType(int_ty) => int_ty.const_int(const_bit, false).into(),
            other => {
                return Err(CompileError::Unsupported(format!(
                    "'and'/'or' result must be boolean, got {other:?}"
                )))
            }
        };
        self.generator.build_br(merge_bb)?;

        self.generator.builder.position_at_end(merge_bb);
        let phi = self
            .generator
            .builder
            .build_phi(result_ty, "sc_result")
            .map_err(|error| CompileError::LlvmError(format!("phi error: {error}")))?;
        if let Some(bb) = rhs_end_bb {
            phi.add_incoming(&[(&rhs as &dyn inkwell::values::BasicValue, bb)]);
        }
        phi.add_incoming(&[(&const_val as &dyn inkwell::values::BasicValue, const_bb)]);
        Ok(phi.as_basic_value())
    }

    fn apply_conversion(
        &self,
        value: BasicValueEnum<'ctx>,
        conversion: &CheckedConversion,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match conversion.kind {
            CheckedConversionKind::Identity
            // Alias/Newtype conversions are identity at the LLVM level.
            | CheckedConversionKind::AliasWrap
            | CheckedConversionKind::AliasUnwrap
            | CheckedConversionKind::NewtypeWrap
            | CheckedConversionKind::NewtypeUnwrap
            | CheckedConversionKind::ContainerErase
            | CheckedConversionKind::TupleErase
            // Ownership annotations are runtime-transparent: shared/weak
            // values share the target's LLVM representation.
            | CheckedConversionKind::OwnershipWrap
            | CheckedConversionKind::OwnershipDowngrade
            | CheckedConversionKind::OwnershipRead => Ok(value),
            CheckedConversionKind::NumericWiden | CheckedConversionKind::NumericNarrowChecked => {
                // K-4 复核（2026-08-07）：NumericNarrowChecked 只来自显式 cast
                // （lower.rs checked_explicit_conversion，仅 Expr::Cast 调用），
                // 保持 wrap 语义（0.34.34 裁决）。隐式收窄在两道门禁处拒绝：
                // checker E0209/E0211 + lower implicit_conversion 仅允许 widen
                // （I32→I64/F64），故此处无需再区分 Bind/Assign/call 实参——
                // 它们不会产生 NumericNarrowChecked conversion。
                let target = self.lower_type(&conversion.to)?;
                self.numeric_convert(value, target)
            }
            // C3 (audit 2026-08-03): concrete → Any. DynamicAny lowers to i64
            // (resolved/types.rs); sub-64-bit ints widen to match the map value
            // box ABI, everything else flows through untouched. This mirrors
            // how the builtin map_set accepts arbitrary values.
            CheckedConversionKind::DynamicAnyPack => {
                let target = self.lower_type(&conversion.to)?;
                match value {
                    BasicValueEnum::IntValue(iv) => {
                        let i64_ty = self.generator.context.i64_type();
                        if iv.get_type().get_bit_width() == 64 {
                            Ok(value)
                        } else {
                            let widened = self.generator.builder.build_int_s_extend(
                                iv,
                                i64_ty,
                                "dynany_sext",
                            ).map_err(|e| CompileError::LlvmError(format!("dynany sext: {}", e)))?;
                            Ok(BasicValueEnum::IntValue(widened))
                        }
                    }
                    _ if target == value.get_type() => Ok(value),
                    _ => Err(CompileError::TypeMismatch(format!(
                        "DynamicAnyPack: cannot pack {} into {}",
                        value.get_type(),
                        target
                    ))),
                }
            }
            // 0.39.136: DynamicAny → concrete unpack. Mirror of Pack: the
            // box is an i64 slot; narrow back to the concrete int width or
            // pass through when the target already matches.
            CheckedConversionKind::DynamicAnyUnpack => {
                let target = self.lower_type(&conversion.to)?;
                match value {
                    BasicValueEnum::IntValue(iv) => {
                        let i64_ty = self.generator.context.i64_type();
                        if iv.get_type().get_bit_width() == 64 {
                            if target == value.get_type() {
                                Ok(value)
                            } else if let BasicTypeEnum::IntType(it) = target {
                                let truncated = self.generator.builder.build_int_truncate(
                                    iv,
                                    it,
                                    "dynany_trunc",
                                ).map_err(|e| CompileError::LlvmError(format!("dynany trunc: {}", e)))?;
                                Ok(BasicValueEnum::IntValue(truncated))
                            } else {
                                Ok(value)
                            }
                        } else {
                            let widened = self.generator.builder.build_int_s_extend(
                                iv,
                                i64_ty,
                                "dynany_sext",
                            ).map_err(|e| CompileError::LlvmError(format!("dynany sext: {}", e)))?;
                            Ok(BasicValueEnum::IntValue(widened))
                        }
                    }
                    _ if target == value.get_type() => Ok(value),
                    _ => Err(CompileError::TypeMismatch(format!(
                        "DynamicAnyUnpack: cannot unpack {} into {}",
                        value.get_type(),
                        target
                    ))),
                }
            }
            other => Err(CompileError::Unsupported(format!(
                "resolved conversion {other:?} escaped resolved native eligibility"
            ))),
        }
    }

    /// General numeric conversion: handles widen, narrow, int↔float.
    fn numeric_convert(
        &self,
        value: BasicValueEnum<'ctx>,
        target: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if value.get_type() == target {
            return Ok(value);
        }
        match (value, target) {
            // int → int
            (BasicValueEnum::IntValue(iv), BasicTypeEnum::IntType(target_ty)) => {
                let src_width = iv.get_type().get_bit_width();
                let dst_width = target_ty.get_bit_width();
                if src_width > dst_width {
                    self.generator
                        .builder
                        .build_int_truncate(iv, target_ty, "resolved_narrow")
                        .map(BasicValueEnum::from)
                        .map_err(|e| CompileError::LlvmError(format!("int truncate: {e}")))
                } else if src_width == 1 {
                    // bool → wider int: zext (avoid sign-extending true to -1)
                    self.generator
                        .builder
                        .build_int_z_extend(iv, target_ty, "resolved_bool_ext")
                        .map(BasicValueEnum::from)
                        .map_err(|e| CompileError::LlvmError(format!("bool zext: {e}")))
                } else {
                    self.generator
                        .builder
                        .build_int_s_extend(iv, target_ty, "resolved_widen")
                        .map(BasicValueEnum::from)
                        .map_err(|e| CompileError::LlvmError(format!("int widen: {e}")))
                }
            }
            // float → int
            (BasicValueEnum::FloatValue(fv), BasicTypeEnum::IntType(target_ty)) => self
                .generator
                .builder
                .build_float_to_signed_int(fv, target_ty, "resolved_fptosi")
                .map(BasicValueEnum::from)
                .map_err(|e| CompileError::LlvmError(format!("fptosi: {e}"))),
            // int → float
            (BasicValueEnum::IntValue(iv), BasicTypeEnum::FloatType(target_ty)) => self
                .generator
                .builder
                .build_signed_int_to_float(iv, target_ty, "resolved_sitofp")
                .map(BasicValueEnum::from)
                .map_err(|e| CompileError::LlvmError(format!("sitofp: {e}"))),
            // float → float
            (BasicValueEnum::FloatValue(fv), BasicTypeEnum::FloatType(target_ty)) => {
                let src_width = fv.get_type().get_bit_width();
                let dst_width = target_ty.get_bit_width();
                if src_width > dst_width {
                    self.generator
                        .builder
                        .build_float_trunc(fv, target_ty, "resolved_fptrunc")
                        .map(BasicValueEnum::from)
                        .map_err(|e| CompileError::LlvmError(format!("fptrunc: {e}")))
                } else {
                    self.generator
                        .builder
                        .build_float_ext(fv, target_ty, "resolved_fpext")
                        .map(BasicValueEnum::from)
                        .map_err(|e| CompileError::LlvmError(format!("fpext: {e}")))
                }
            }
            // Deep-eval 2026-08-09 (resolved Ok(list) E0722): a type-erased
            // container handle (ptr to the {i64,ptr} list struct) packed into
            // a by-value Ok slot — load the struct. Mirrors the legacy
            // compile_ok_constructor's natural-shape payload slot. Guarded to
            // the List layout only: a bare function pointer (e.g. `add_impl`
            // inside a tuple) also arrives as a ptr but must NOT be loaded
            // from the code section — that SIGSEGVs (real_world_tuple_fn_element_call).
            (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(sty))
                if sty.get_field_types().len() == 2
                    && matches!(
                        sty.get_field_types()[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                    )
                    && matches!(sty.get_field_types()[1], BasicTypeEnum::PointerType(_)) =>
            {
                self.generator
                    .builder
                    .build_load(
                        BasicTypeEnum::StructType(sty),
                        pv,
                        "resolved_ptr_struct_load",
                    )
                    .map(BasicValueEnum::from)
                    .map_err(|e| CompileError::LlvmError(format!("ptr struct load: {e}")))
            }
            // 0.39.136 (L1): string struct {ptr,i64} → i64 ValueHandle.
            // Erased-value ABI positions (map_set/map_remove values, Any
            // slots) store heap C-string handles as raw ints. Extract the
            // pointer, heap-clone it (mirrors legacy compile_map_set's
            // strlen+mimi_str_clone: the stored handle must outlive the
            // source temporary) and ptrtoint. Without this arm every
            // string-valued map_set failed "resolved numeric conversion"
            // and fell the whole function back to legacy.
            (BasicValueEnum::StructValue(sv), BasicTypeEnum::IntType(it))
                if it.get_bit_width() == 64
                    && sv.get_type().get_field_types().len() == 2
                    && matches!(
                        sv.get_type().get_field_types()[0],
                        BasicTypeEnum::PointerType(_)
                    )
                    && matches!(
                        sv.get_type().get_field_types()[1],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                    ) =>
            {
                let ptr = self
                    .generator
                    .builder
                    .build_extract_value(sv, 0, "resolved_str_handle_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("resolved str unwrap: {e}")))?
                    .into_pointer_value();
                let strlen_fn = self
                    .generator
                    .module
                    .get_function("strlen")
                    .ok_or_else(|| CompileError::LlvmError("strlen not declared".into()))?;
                let len = self
                    .generator
                    .build_call(
                        strlen_fn,
                        &[BasicMetadataValueEnum::PointerValue(ptr)],
                        "resolved_str_handle_len",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("strlen returned void".into()))?
                    .into_int_value();
                let clone_fn = self
                    .generator
                    .module
                    .get_function("mimi_str_clone")
                    .ok_or_else(|| CompileError::LlvmError("mimi_str_clone not declared".into()))?;
                let handle = self
                    .generator
                    .build_call(
                        clone_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(ptr),
                            BasicMetadataValueEnum::IntValue(len),
                        ],
                        "resolved_str_handle",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("mimi_str_clone returned void".into()))?
                    .into_int_value();
                let i64_ty = self.generator.context.i64_type();
                let widened = if handle.get_type().get_bit_width() < 64 {
                    self.generator
                        .builder
                        .build_int_s_extend(handle, i64_ty, "resolved_str_handle_sext")
                        .map_err(|e| CompileError::LlvmError(format!("handle sext: {e}")))?
                } else {
                    handle
                };
                Ok(BasicValueEnum::IntValue(widened))
            }
            // 0.39.136 (L1): list struct {i64, ptr} → i64 opaque handle.
            // Erased-value positions (map_set values, Any slots) store
            // container handles as raw ints; heap-pack the whole struct and
            // ptrtoint (mirrors legacy compile_map_set's list arm). Kept
            // separate from the string arm — field order distinguishes the
            // two layouts.
            (BasicValueEnum::StructValue(sv), BasicTypeEnum::IntType(it))
                if it.get_bit_width() == 64
                    && sv.get_type().get_field_types().len() == 2
                    && matches!(
                        sv.get_type().get_field_types()[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                    )
                    && matches!(
                        sv.get_type().get_field_types()[1],
                        BasicTypeEnum::PointerType(_)
                    ) =>
            {
                let struct_ty = sv.get_type();
                let size_bytes = self
                    .generator
                    .llvm_type_size_bytes(BasicTypeEnum::StructType(struct_ty));
                let heap = self.generator.malloc_or_abort(
                    self.generator
                        .context
                        .i64_type()
                        .const_int(size_bytes as u64, false),
                    "resolved_list_handle_pack",
                )?;
                let i8_ptr = self
                    .generator
                    .context
                    .ptr_type(inkwell::AddressSpace::default());
                let typed = self
                    .generator
                    .build_bit_cast(
                        heap.into(),
                        BasicTypeEnum::PointerType(i8_ptr),
                        "resolved_list_handle_ptr",
                    )?
                    .into_pointer_value();
                self.generator.build_store(typed, sv)?;
                self.generator
                    .build_ptr_to_int(
                        typed,
                        self.generator.context.i64_type(),
                        "resolved_list_handle_int",
                    )
                    .map(BasicValueEnum::from)
            }
            // 0.35.23 deep-eval: string struct {ptr,i64} ↔ raw C-string ptr.
            // The resolved emitter feeds string structs into runtime-direct
            // builtins whose params are i8* (mimi-log main: json_get_string /
            // to_int over args_list elements), and if-branches merge a string
            // VALUE with a literal's raw ptr (`let cmd = if .. { parts[0] }
            // else { "" }`). Without these two arms every such site failed
            // "resolved numeric conversion" and the whole function fell back
            // to legacy (which then hit its own List<record> gaps).
            (BasicValueEnum::StructValue(sv), BasicTypeEnum::PointerType(_))
                if sv.get_type().get_field_types().len() == 2
                    && matches!(
                        sv.get_type().get_field_types()[0],
                        BasicTypeEnum::PointerType(_)
                    )
                    && matches!(
                        sv.get_type().get_field_types()[1],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                    ) =>
            {
                let ptr = self
                    .generator
                    .builder
                    .build_extract_value(sv, 0, "resolved_str_unwrap")
                    .map_err(|e| CompileError::LlvmError(format!("resolved str unwrap: {e}")))?
                    .into_pointer_value();
                Ok(BasicValueEnum::PointerValue(ptr))
            }
            (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(sty))
                if sty.get_field_types().len() == 2
                    && matches!(sty.get_field_types()[0], BasicTypeEnum::PointerType(_))
                    && matches!(
                        sty.get_field_types()[1],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                    ) =>
            {
                self.generator.wrap_c_string(pv).map(BasicValueEnum::from)
            }
            _ => Err(CompileError::Unsupported(format!(
                "resolved numeric conversion {:?} → {target:?} is not supported",
                value.get_type()
            ))),
        }
    }

    fn coerce_to(
        &self,
        value: BasicValueEnum<'ctx>,
        target: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        self.numeric_convert(value, target)
    }

    /// 0.34.30 (dx-backlog #11): construct `Some`/`None`/`Ok`/`Err` in the
    /// resolved layout ({bool, ok_llvm, i64_err}) derived from the target
    /// `expression.ty`. This keeps Ok(1.5) ({bool, double, i64}) and Err(..)
    /// ({bool, double, i64}) layout-consistent inside if/else merges, which the
    /// legacy compile_constructor (non-list path, ok-pad hard-coded to i64)
    /// breaks for non-i64 ok payloads such as `Result<f64, string>`.
    fn emit_resolved_optional_ctor(
        &mut self,
        name: &str,
        args: Vec<BasicValueEnum<'ctx>>,
        ty: &ResolvedTypeId,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let BasicTypeEnum::StructType(sty) = self.lower_type(ty)? else {
            return Err(CompileError::Unsupported(format!(
                "Option/Result constructor for '{name}' did not lower to an LLVM struct"
            )));
        };
        let bool_ty = self.generator.context.bool_type();
        let alloca = self.generator.build_alloca(sty, "opt_result_ctor")?;
        self.generator
            .build_store(alloca, BasicValueEnum::StructValue(sty.const_zero()))?;
        let disc_gep = self
            .generator
            .builder
            .build_struct_gep(sty, alloca, 0, "ctor_disc")
            .map_err(|e| CompileError::LlvmError(format!("ctor disc gep: {e}")))?;
        let slot1_ty = sty.get_field_type_at_index(1).ok_or_else(|| {
            CompileError::LlvmError("Option/Result struct has no payload slot".into())
        })?;
        match name {
            "None" => {
                self.generator
                    .build_store(disc_gep, bool_ty.const_int(0, false))?;
            }
            "Ok" | "Some" => {
                let payload = args
                    .into_iter()
                    .next()
                    .ok_or_else(|| CompileError::LlvmError(format!("{name} expects 1 argument")))?;
                self.generator
                    .build_store(disc_gep, bool_ty.const_int(1, false))?;
                let slot = self.numeric_convert(payload, slot1_ty)?;
                let slot_gep = self
                    .generator
                    .builder
                    .build_struct_gep(sty, alloca, 1, "ctor_payload")
                    .map_err(|e| CompileError::LlvmError(format!("ctor payload gep: {e}")))?;
                self.generator.build_store(slot_gep, slot)?;
            }
            "Err" => {
                let payload = args
                    .into_iter()
                    .next()
                    .ok_or_else(|| CompileError::LlvmError("Err expects 1 argument".into()))?;
                self.generator
                    .build_store(disc_gep, bool_ty.const_int(0, false))?;
                // err value → i64 handle, mirroring the legacy
                // compile_err_constructor non-list path: ints widen/truncate,
                // pointers ptrtoint, strings heap-pack as {ptr,len} then
                // ptrtoint (the `?` operator reconstructs via inttoptr+GEP),
                // enums store their i32 tag. Self-contained so the resolved
                // ctor never depends on legacy struct slot extraction.
                let err_handle = self.resolved_err_to_handle(payload)?;
                let err_gep = self
                    .generator
                    .builder
                    .build_struct_gep(sty, alloca, 2, "ctor_err")
                    .map_err(|e| CompileError::LlvmError(format!("ctor err gep: {e}")))?;
                self.generator.build_store(err_gep, err_handle)?;
            }
            _ => {
                return Err(CompileError::Unsupported(format!(
                    "resolved optional ctor '{name}'"
                )))
            }
        }
        self.generator
            .build_load(BasicTypeEnum::StructType(sty), alloca, "ctor_val")
    }

    /// 0.34.30: err payload → i64 handle, mirroring the legacy
    /// `compile_err_constructor` non-list path so the `?` operator's
    /// inttoptr+GEP reconstruction stays ABI-compatible.
    fn resolved_err_to_handle(
        &mut self,
        value: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i64_ty = self.generator.context.i64_type();
        match value {
            BasicValueEnum::FloatValue(fv) if fv.get_type().get_bit_width() == 64 => {
                let bits = self
                    .generator
                    .builder
                    .build_bit_cast(
                        BasicValueEnum::FloatValue(fv),
                        BasicTypeEnum::IntType(i64_ty),
                        "err_f64_bits",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("err f64 bitcast: {e}")))?;
                Ok(bits)
            }
            BasicValueEnum::IntValue(iv) => {
                let bw = iv.get_type().get_bit_width();
                let widened = if bw == 1 {
                    self.generator
                        .builder
                        .build_int_z_extend(iv, i64_ty, "err_bool_zext")
                        .map_err(|e| CompileError::LlvmError(format!("err bool zext: {e}")))?
                } else if bw < 64 {
                    self.generator
                        .builder
                        .build_int_s_extend(iv, i64_ty, "err_sext")
                        .map_err(|e| CompileError::LlvmError(format!("err sext: {e}")))?
                } else if bw > 64 {
                    self.generator
                        .builder
                        .build_int_truncate(iv, i64_ty, "err_trunc")
                        .map_err(|e| CompileError::LlvmError(format!("err trunc: {e}")))?
                } else {
                    iv
                };
                Ok(BasicValueEnum::IntValue(widened))
            }
            BasicValueEnum::PointerValue(pv) => self
                .generator
                .build_ptr_to_int(pv, i64_ty, "err_to_i64")
                .map(BasicValueEnum::IntValue),
            BasicValueEnum::StructValue(sv) => {
                let fields = sv.get_type().get_field_types();
                let is_mimi_string = fields.len() == 2
                    && matches!(&fields[0], BasicTypeEnum::PointerType(_))
                    && matches!(&fields[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64);
                if is_mimi_string {
                    // Heap-pack {ptr, len} so `?` (inttoptr + GEP 0/1) can
                    // reconstruct the full string.
                    let i8_ptr = self
                        .generator
                        .context
                        .ptr_type(inkwell::AddressSpace::default());
                    let string_ty = self.generator.context.struct_type(
                        &[
                            BasicTypeEnum::PointerType(i8_ptr),
                            BasicTypeEnum::IntType(i64_ty),
                        ],
                        false,
                    );
                    let alloc_size = i64_ty.const_int(16, false);
                    let heap_ptr = self
                        .generator
                        .malloc_or_abort(alloc_size, "err_str_malloc")?;
                    let str_ptr_gep = self
                        .generator
                        .builder
                        .build_struct_gep(string_ty, heap_ptr, 0, "err_str_ptr_gep")
                        .map_err(|e| CompileError::LlvmError(format!("err str ptr gep: {e}")))?;
                    self.generator.build_store(
                        str_ptr_gep,
                        self.generator
                            .build_extract_value(sv.into(), 0, "err_str_ptr")?,
                    )?;
                    let str_len_gep = self
                        .generator
                        .builder
                        .build_struct_gep(string_ty, heap_ptr, 1, "err_str_len_gep")
                        .map_err(|e| CompileError::LlvmError(format!("err str len gep: {e}")))?;
                    self.generator.build_store(
                        str_len_gep,
                        self.generator
                            .build_extract_value(sv.into(), 1, "err_str_len")?,
                    )?;
                    if self.generator.value_glue_enabled() {
                        self.generator.register_heap_box(heap_ptr);
                    }
                    Ok(BasicValueEnum::IntValue(self.generator.build_ptr_to_int(
                        heap_ptr,
                        i64_ty,
                        "err_str_heap_i64",
                    )?))
                } else {
                    // P0-3: full StructValue error payloads are heap-copied and
                    // stored as a pointer, matching the match/inttoptr decode.
                    let struct_ty = sv.get_type();
                    let size_bytes = struct_ty
                        .size_of()
                        .and_then(|s| s.get_zero_extended_constant())
                        .unwrap_or(64)
                        .max(1);
                    let heap_ptr = self.generator.malloc_or_abort(
                        i64_ty.const_int(size_bytes, false),
                        "err_struct_malloc",
                    )?;
                    self.generator.build_store(heap_ptr, sv)?;
                    Ok(BasicValueEnum::IntValue(self.generator.build_ptr_to_int(
                        heap_ptr,
                        i64_ty,
                        "err_struct_heap_i64",
                    )?))
                }
            }
            _ => Err(CompileError::Unsupported(format!(
                "resolved Err payload type {:?} cannot become an i64 handle",
                value.get_type()
            ))),
        }
    }

    /// Coerce a `string` value to an i64 list-slot handle. Since 0.38.26
    /// `List<string>` slots store a fat `MimiStr { magic, ptr, len }` box
    /// (`mimi_str_box`), not the raw C-string pointer. The legacy
    /// `coerce_to_i64` path is still used for non-string values.
    fn coerce_string_to_i64(
        &self,
        value: BasicValueEnum<'ctx>,
    ) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
        let (raw_ptr, raw_len) = match value {
            BasicValueEnum::PointerValue(pv) => {
                let len = self.generator.string_len(pv)?;
                (pv, len)
            }
            BasicValueEnum::StructValue(sv) => {
                let sv_fields = sv.get_type().get_field_types();
                if sv_fields.len() != 2
                    || !matches!(&sv_fields[0], BasicTypeEnum::PointerType(_))
                    || !matches!(&sv_fields[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64)
                {
                    return Err(CompileError::Unsupported(format!(
                        "resolved string list element is not {{ptr, len}}: {sv_fields:?}"
                    )));
                }
                let raw_ptr = self
                    .generator
                    .build_extract_value(sv.into(), 0, "str_list_ptr")?
                    .into_pointer_value();
                let raw_len = self
                    .generator
                    .build_extract_value(sv.into(), 1, "str_list_len")?
                    .into_int_value();
                (raw_ptr, raw_len)
            }
            other => {
                return Err(CompileError::Unsupported(format!(
                    "resolved string list element cannot become a fat string box: {other:?}"
                )))
            }
        };
        let box_fn = self.generator.get_runtime_fn("mimi_str_box")?;
        let boxed = self
            .generator
            .build_call(
                box_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(raw_ptr),
                    BasicMetadataValueEnum::IntValue(raw_len),
                ],
                "resolved_str_box",
            )?
            .try_as_basic_value_opt()
            .ok_or("mimi_str_box returned void")?
            .into_int_value();
        Ok(boxed)
    }

    /// Coerce a value to i64 for list element storage. Handles int
    /// sign/zero-extension, float-to-int, bool zext, pointer ptrtoint,
    /// and struct-value pointer extraction (for string/record/nested list
    /// values stored as heap pointers in the list data array).
    fn coerce_to_i64(
        &self,
        value: BasicValueEnum<'ctx>,
    ) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
        let i64_ty = self.generator.context.i64_type();
        match value {
            BasicValueEnum::IntValue(iv) => {
                let bw = iv.get_type().get_bit_width();
                if bw == 1 {
                    // Bool: zero-extend.
                    Ok(self
                        .generator
                        .builder
                        .build_int_z_extend(iv, i64_ty, "list_bool_zext")
                        .map_err(|e| CompileError::LlvmError(format!("bool zext: {e}")))?)
                } else if bw < 64 {
                    Ok(self
                        .generator
                        .builder
                        .build_int_s_extend(iv, i64_ty, "list_sext")
                        .map_err(|e| CompileError::LlvmError(format!("sext: {e}")))?)
                } else if bw > 64 {
                    Ok(self
                        .generator
                        .builder
                        .build_int_truncate(iv, i64_ty, "list_trunc")
                        .map_err(|e| CompileError::LlvmError(format!("trunc: {e}")))?)
                } else {
                    Ok(iv)
                }
            }
            BasicValueEnum::FloatValue(fv) => {
                // Float → i64 bitcast (store raw bits).
                Ok(self
                    .generator
                    .builder
                    .build_bit_cast(fv, i64_ty, "list_float_bits")
                    .map_err(|e| CompileError::LlvmError(format!("float bitcast: {e}")))?
                    .into_int_value())
            }
            BasicValueEnum::PointerValue(pv) => {
                // Pointer: ptrtoint to i64 (for string/record heap pointers).
                self.generator
                    .build_ptr_to_int(pv, i64_ty, "list_ptr_to_handle")
            }
            BasicValueEnum::StructValue(sv) => {
                // Struct value: match the legacy emitter's approach.
                // For string structs {ptr, i64}: extract the raw C-string
                // pointer and store it directly as i64.
                // For all other structs (nested lists, tuples, records,
                // Option, Result): heap-allocate the struct, store the
                // pointer as i64 — matching legacy data layout exactly.
                let sv_fields = sv.get_type().get_field_types();
                if sv_fields.len() == 2
                    && matches!(&sv_fields[0], BasicTypeEnum::PointerType(_))
                    && matches!(&sv_fields[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64)
                {
                    // String struct {i8*, i64}: extract raw pointer.
                    let raw_ptr = self
                        .generator
                        .build_extract_value(sv.into(), 0, "str_ptr")?
                        .into_pointer_value();
                    return self
                        .generator
                        .build_ptr_to_int(raw_ptr, i64_ty, "str_to_i64");
                }
                // Non-string struct: heap-allocate, store, return pointer.
                // §6-#68 (closed 0.36.105 by design): this allocation is
                // deliberately not registered in `heap_allocs` because list
                // element boxes are not single-scope-owned and the current
                // list ownership model does not track element lifetimes.
                // Registering them would risk freeing still-reachable elements
                // or double-freeing on returned/mutated lists; the process-level
                // reclaim at termination keeps codegen correct at the cost of a
                // long-lived leak. Tracked for Wave-3 list ownership overhaul.
                // See `call/method.rs` from_json list note.
                let struct_ty = sv.get_type();
                let size = self
                    .generator
                    .llvm_type_size_bytes(BasicTypeEnum::StructType(struct_ty));
                let size_val = i64_ty.const_int(size, false);
                let ptr = self.generator.malloc_or_abort(size_val, "struct_to_i64")?;
                let i8_ptr_ty = self
                    .generator
                    .context
                    .ptr_type(inkwell::AddressSpace::default());
                let typed_ptr = self
                    .generator
                    .build_pointer_cast(ptr, i8_ptr_ty, "struct_ptr")?;
                self.generator.build_store(typed_ptr, sv)?;
                self.generator
                    .build_ptr_to_int(ptr, i64_ty, "struct_handle")
            }
            other => Err(CompileError::Unsupported(format!(
                "cannot coerce {other:?} to i64 for list storage"
            ))),
        }
    }

    /// Convert a list element loaded as i64 back to its proper type.
    /// Mirror of the legacy emitter's `try_convert_list_element` / `convert_list_elem_by_type`:
    /// - String ({i8*, i64}): load i64 → inttoptr → strlen → build struct
    /// - Nested list/tuple/record/Option/Result: inttoptr → load struct from heap pointer
    /// - Pointer: inttoptr
    /// - Int/Float: use as-is
    fn convert_list_elem_i64(
        &self,
        elem_int: inkwell::values::IntValue<'ctx>,
        elem_llvm_ty: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());
        match elem_llvm_ty {
            BasicTypeEnum::StructType(sty) => {
                let fields = sty.get_field_types();
                let is_string = fields.len() == 2
                    && matches!(&fields[0], BasicTypeEnum::PointerType(_))
                    && matches!(&fields[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64);
                if is_string {
                    // String list slots hold a pointer to a fat MimiStr box.
                    let boxed =
                        self.generator
                            .build_int_to_ptr(elem_int, ptr_ty, "elem_str_ptr")?;
                    self.generator.load_fat_list_string(boxed)
                } else {
                    // Non-string struct: stored as heap pointer.
                    let struct_ptr = self
                        .generator
                        .build_int_to_ptr(elem_int, ptr_ty, "elem_ptr")?;
                    self.generator.build_load(
                        BasicTypeEnum::StructType(sty),
                        struct_ptr,
                        "elem_struct",
                    )
                }
            }
            BasicTypeEnum::PointerType(_) => {
                // Raw pointer (string char*).
                let ptr = self
                    .generator
                    .build_int_to_ptr(elem_int, ptr_ty, "elem_ptr")?;
                Ok(BasicValueEnum::PointerValue(ptr))
            }
            BasicTypeEnum::IntType(target_ty) => {
                // Truncate i64 → target width if needed (e.g. List<i32> elements).
                if target_ty.get_bit_width() < 64 {
                    let truncated = self
                        .generator
                        .builder
                        .build_int_truncate(elem_int, target_ty, "elem_trunc")
                        .map_err(|e| CompileError::LlvmError(format!("elem truncate: {e}")))?;
                    Ok(BasicValueEnum::IntValue(truncated))
                } else {
                    Ok(BasicValueEnum::IntValue(elem_int))
                }
            }
            BasicTypeEnum::FloatType(_) => {
                // Float — bitcast i64 to float.
                let fv = self
                    .generator
                    .builder
                    .build_bit_cast(
                        BasicValueEnum::IntValue(elem_int),
                        elem_llvm_ty,
                        "i64_to_float",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("i64 to float: {e}")))?
                    .into_float_value();
                Ok(BasicValueEnum::FloatValue(fv))
            }
            _ => Err(CompileError::Unsupported(format!(
                "cannot convert list element from i64 to {elem_llvm_ty:?}"
            ))),
        }
    }

    /// Look up the variant name from a variant NodeId by searching type definitions.
    fn lookup_variant_name(&self, variant_id: &NodeId) -> Result<String, CompileError> {
        for td in self.program.type_defs().values() {
            for (name, id) in &td.variant_ids {
                if id == variant_id {
                    return Ok(name.clone());
                }
            }
        }
        // Fallback: check if the NodeId's variant fragment matches a known
        // builtin variant name. 2026-08-06 (§6-#65): the old `contains(name)`
        // substring match misclassified ANY NodeId containing "Err" — e.g. a
        // user enum variant `Errors` (fragment `variant.Errors`) resolved to
        // builtin "Err" and compiled the wrong constructor. Match exact
        // suffixes instead: builtin Option/Result use `builtin:variant:Option::Some`
        // (colon form), custom enums use `<node>/decl.variant/variant.<name>`.
        let id_str = &variant_id.0;
        for name in ["Some", "None", "Ok", "Err"] {
            if id_str.ends_with(&format!("::{name}"))
                || id_str.ends_with(&format!("variant.{name}"))
            {
                return Ok(name.to_string());
            }
        }
        // 0.36.56: Flow state constructor variants are `state:F::B`. There is
        // no synthetic enum type in type_defs; the state name is the last
        // `::` segment of the state path.
        if let Some(rest) = id_str.strip_prefix("state:") {
            let path = rest.split('/').next().unwrap_or(rest);
            let name = path.rsplit("::").next().unwrap_or(path);
            return Ok(name.to_string());
        }
        Err(CompileError::Unsupported(format!(
            "variant '{}' not found in any type definition",
            variant_id.0
        )))
    }

    /// Check if a variant belongs to a newtype (not an enum).
    /// Newtype constructors always match and bind the scrutinee directly.
    /// For newtypes, the Constructor pattern's variant NodeId is the type's
    /// own NodeId (e.g., "type:UserId"), since newtypes don't have separate
    /// variant entries in variant_ids.
    fn is_newtype_variant(&self, variant_id: &NodeId) -> bool {
        self.program.type_defs().values().any(|td| {
            matches!(td.kind, crate::core::resolved::ResolvedTypeKind::Newtype)
                && (td.node_id == *variant_id
                    || td.variant_ids.values().any(|id| id == variant_id)
                    || variant_id.0.contains(&td.qualified_name))
        })
    }

    /// Look up the field name from a field NodeId by searching type definitions.
    fn lookup_field_name(&self, field_id: &NodeId) -> Result<String, CompileError> {
        for td in self.program.type_defs().values() {
            for (name, id) in &td.field_ids {
                if id == field_id {
                    return Ok(name.clone());
                }
            }
        }
        // 0.32.21: Flow state field fallback. Extract the state path and
        // source span from the field_id, then match against the legacy
        // type_defs registered by compile_flow().
        if let Some((flow_type_name, span_str)) = Self::parse_flow_field_id(field_id) {
            if let Some(td) = self.generator.type_defs.get(&flow_type_name) {
                if let crate::ast::TypeDefKind::Record(fields) = &td.kind {
                    if let Some(field) = Self::match_field_by_span(fields, &span_str) {
                        return Ok(field.name.clone());
                    }
                }
            }
        }
        // 0.1.8 Phase D/F: builtin record schemas (PeerFault, ExecResult,
        // StatResult, ...) live outside the checked type_defs catalog. Their
        // field ids are `builtin:type:PeerFault/field:peer_id`; map them back
        // to the canonical schema name.
        if let Some(owner_part) = field_id.0.strip_prefix("builtin:type:") {
            let owner = format!(
                "builtin:type:{}",
                owner_part.split('/').next().unwrap_or("")
            );
            if let Some(schema) = crate::core::resolved::builtin_record_schema(&owner) {
                if let Some(field_path) = owner_part.split('/').nth(1) {
                    if let Some(field_name) = field_path.strip_prefix("field:") {
                        if schema.iter().any(|(name, _)| *name == field_name) {
                            return Ok(field_name.to_string());
                        }
                    }
                }
            }
        }
        Err(CompileError::Unsupported(format!(
            "field '{}' not found in any type definition",
            field_id.0
        )))
    }

    /// Parse a Flow state field_id into (flow_type_name, span_string).
    /// Format: "state:Flow::State/node:decl.field@external:HASH:LINE:COL-LINE:COL"
    /// Returns ("flow::Flow::State", "LINE:COL-LINE:COL").
    /// Well-known field order of the Fault crash-context records
    /// (SystemTrace/MemoryDump/PanicPayload). The index values are shared
    /// across the records where names collide (unexpected_event/snapshot are
    /// SystemTrace index 1/2 and the Fault record's own 1/2), so a single
    /// name→index map is consistent for every projection the checker can
    /// produce on these builtins.
    fn builtin_trace_field_index(field_name: &str) -> Option<u32> {
        Some(match field_name {
            "last_state_name" => 0,
            "unexpected_event" => 1,
            "snapshot" => 2,
            "memory_dump" => 3,
            "panic_payload" => 4,
            // MemoryDump { fields: string, count: i32 }
            "fields" => 0,
            "count" => 1,
            // PanicPayload { error_type: string, file: string, line: i32, stack: string }
            "error_type" => 0,
            "file" => 1,
            "line" => 2,
            "stack" => 3,
            _ => return None,
        })
    }

    /// Fallback for checker-internal builtin record fields whose TypeDefs are
    /// not part of the checked program's type_defs catalog. `field_id` has the
    /// form `builtin:type:PeerFault/field:peer_id`.
    fn builtin_schema_field_index(field_id: &str, field_name: &str) -> Option<u32> {
        let id = field_id.strip_prefix("builtin:type:")?;
        let owner = format!("builtin:type:{}", id.split('/').next()?);
        let schema = crate::core::resolved::builtin_record_schema(&owner)?;
        schema
            .iter()
            .position(|(name, _)| *name == field_name)
            .map(|i| i as u32)
    }

    fn parse_flow_field_id(field_id: &NodeId) -> Option<(String, String)> {
        let id_str = &field_id.0;
        let state_path = id_str.strip_prefix("state:")?;
        let slash_pos = state_path.find('/')?;
        let state_name = &state_path[..slash_pos];
        // Extract span: last segment after the last ':'-separated hash.
        // Format after slash: "node:decl.field@external:HASH:L:C-L:C"
        let after_slash = &state_path[slash_pos + 1..];
        // The trailing span is usually `LINE:COL-LINE:COL`; it may be preceded
        // by either an external hash (`@external:HASH:4:15-4:25`) or a simple
        // source marker (`@unknown-source:4:15-4:25`). Parse from the last
        // hyphen so both forms work. For generated fault fields there is no
        // source span (`generated:decl.field:...last_state`); still return a
        // best-effort suffix so `lookup_field_index` can match by name.
        if let Some(hyphen) = after_slash.rfind('-') {
            let end_part = &after_slash[hyphen + 1..];
            let start_part = &after_slash[..hyphen];
            let end_parts: Vec<&str> = end_part.split(':').collect();
            let start_parts: Vec<&str> = start_part.split(':').collect();
            if end_parts.len() >= 2 && start_parts.len() >= 2 {
                if let (Ok(end_line), Ok(end_col), Ok(start_line), Ok(start_col)) = (
                    end_parts[end_parts.len() - 2].parse::<usize>(),
                    end_parts[end_parts.len() - 1].parse::<usize>(),
                    start_parts[start_parts.len() - 2].parse::<usize>(),
                    start_parts[start_parts.len() - 1].parse::<usize>(),
                ) {
                    return Some((
                        format!("flow::{state_name}"),
                        format!("{start_line}:{start_col}-{end_line}:{end_col}"),
                    ));
                }
            }
        }
        Some((format!("flow::{state_name}"), after_slash.to_string()))
    }

    /// Match a field by comparing source spans. The span_str format is
    /// "LINE:COL-LINE:COL" (possibly with extra hash prefix).
    fn match_field_by_span<'a>(
        fields: &'a [crate::ast::Field],
        span_str: &str,
    ) -> Option<&'a crate::ast::Field> {
        // Parse "L:C-L:C" from span_str (take the last valid pattern).
        let parts: Vec<&str> = span_str.split('-').collect();
        if parts.len() < 2 {
            return None;
        }
        let end_part = parts.last()?;
        let start_part = parts[parts.len() - 2];
        let (end_line, end_col) = end_part.split_once(':')?;
        let (start_line, start_col) = start_part.split_once(':')?;
        let end_line: usize = end_line.parse().ok()?;
        let end_col: usize = end_col.parse().ok()?;
        let start_line: usize = start_line.parse().ok()?;
        let start_col: usize = start_col.parse().ok()?;
        fields.iter().find(|f| {
            let s = &f.meta.span;
            s.start_line == start_line
                && s.start_col == start_col
                && s.end_line == end_line
                && s.end_col == end_col
        })
    }

    /// Build the LLVM struct type for a user-defined record NominalTypeId.
    /// Looks up the type definition, resolves each field's type display
    /// string to a ResolvedTypeId via the type table, and builds the struct.
    fn record_llvm_type(
        &self,
        item: &crate::core::NominalTypeId,
    ) -> Result<inkwell::types::StructType<'ctx>, CompileError> {
        let item_str = item.as_str();
        let type_name = item_str.strip_prefix("type:").unwrap_or(item_str);
        // Find the type definition.
        let td = self
            .program
            .type_defs()
            .values()
            .find(|td| td.qualified_name == type_name || td.qualified_name == item_str)
            .ok_or_else(|| {
                CompileError::Unsupported(format!("type definition for '{item_str}' not found"))
            })?;
        // Build LLVM field types. PREFER the resolved type table: each record
        // field has a stable NodeId (in `td.field_ids`) whose concrete
        // ResolvedType lives in `program.resolved_field_types`. Lowering that
        // ResolvedType with `lower_type` — the exact path every other type in
        // the resolved emitter uses — means composite field types (tuple
        // `(i32, i32)`, `Option<T>`, `Result<T, E>`, nested records) get the
        // SAME layout as the rest of the program and stay ABI-compatible with
        // the legacy backend (which lowers the same record via
        // `register_type_def` → `llvm_type_for`/`mimi_type_to_llvm`, widening
        // sub-64-bit integer tuple fields to i64 exactly as `lower_type` does).
        //
        // The old display-string path (`resolve_type_display` on each
        // `td.fields` entry) cannot represent tuple/Option/Result field types:
        // it failed on "(i32, i32)" and forced `main` to fall back to the
        // legacy emitter, which then mis-lowered generic returns over such
        // records (error[E0700] field access on an i64 slot). We keep it only
        // as a fallback when a field identity is not present in the resolved
        // field-type map.
        let mut field_types = Vec::with_capacity(td.fields.len());
        let resolved_fields = self.program.resolved_field_types();
        for (name, type_display) in &td.fields {
            let lowered = td
                .field_ids
                .get(name)
                .and_then(|fid| resolved_fields.get(fid))
                .map(|rid| self.lower_type(rid));
            match lowered {
                Some(ty) => field_types.push(ty?),
                None => field_types.push(self.resolve_type_display(type_display)?),
            }
        }
        Ok(self.generator.context.struct_type(&field_types, false))
    }

    /// Resolve a type display string (e.g. "i64", "string") to an LLVM type
    /// by scanning the ResolvedTypeTable for a matching primitive.
    fn resolve_type_display(&self, display: &str) -> Result<BasicTypeEnum<'ctx>, CompileError> {
        // Fast path: primitive types.
        if let Some(prim) = PrimitiveType::from_language_name(display) {
            return Ok(match prim {
                PrimitiveType::I8 | PrimitiveType::U8 => {
                    BasicTypeEnum::IntType(self.generator.context.i8_type())
                }
                PrimitiveType::I16 | PrimitiveType::U16 => {
                    BasicTypeEnum::IntType(self.generator.context.i16_type())
                }
                PrimitiveType::I32 | PrimitiveType::U32 | PrimitiveType::Char => {
                    BasicTypeEnum::IntType(self.generator.context.i32_type())
                }
                PrimitiveType::I64
                | PrimitiveType::U64
                | PrimitiveType::Isize
                | PrimitiveType::Usize => BasicTypeEnum::IntType(self.generator.context.i64_type()),
                PrimitiveType::I128 | PrimitiveType::U128 => {
                    BasicTypeEnum::IntType(self.generator.context.i128_type())
                }
                PrimitiveType::F32 => BasicTypeEnum::FloatType(self.generator.context.f32_type()),
                PrimitiveType::F64 => BasicTypeEnum::FloatType(self.generator.context.f64_type()),
                PrimitiveType::Bool => BasicTypeEnum::IntType(self.generator.context.bool_type()),
                PrimitiveType::String => {
                    let ptr = BasicTypeEnum::PointerType(
                        self.generator
                            .context
                            .ptr_type(inkwell::AddressSpace::default()),
                    );
                    let i64 = BasicTypeEnum::IntType(self.generator.context.i64_type());
                    BasicTypeEnum::StructType(
                        self.generator.context.struct_type(&[ptr, i64], false),
                    )
                }
                PrimitiveType::Unit => BasicTypeEnum::IntType(self.generator.context.i64_type()),
            });
        }
        // Fallback: scan the type table for a matching type.
        for (id, ty) in self.program.resolved_types().iter() {
            let nominal_matches = matches!(ty, ResolvedType::Nominal { item, .. } if item.as_str() == display
                    || item.as_str().strip_prefix("type:") == Some(display));
            if id.as_str() == display
                || id
                    .as_str()
                    .strip_prefix("type:")
                    .is_some_and(|name| name == display)
                || nominal_matches
                || format!("{ty:?}") == display
            {
                return self.lower_type(id);
            }
        }
        // 0.37.36: List<T> display names (e.g. "List<i32>") can appear in
        // record field metadata. The resolved list ABI is always {i64, ptr},
        // independent of the element type.
        if display.starts_with("List<") && display.ends_with('>') {
            return Ok(BasicTypeEnum::StructType(self.generator.list_struct_type()));
        }
        Err(CompileError::Unsupported(format!(
            "cannot resolve type display '{display}' to LLVM type"
        )))
    }

    /// Look up the field index within a record type definition.
    /// Searches all type definitions for one whose `field_ids` contains
    /// the given field NodeId, then returns the field's position.
    fn lookup_field_index(&self, field_id: &NodeId, field_name: &str) -> Result<u32, CompileError> {
        for td in self.program.type_defs().values() {
            if td.field_ids.values().any(|id| id == field_id) {
                // Found the type definition. Find the field's position.
                for (i, (name, _)) in td.fields.iter().enumerate() {
                    if name == field_name {
                        return Ok(i as u32);
                    }
                }
            }
        }
        // Fallback: try matching by name across all type definitions.
        for td in self.program.type_defs().values() {
            for (i, (name, _)) in td.fields.iter().enumerate() {
                if name == field_name {
                    return Ok(i as u32);
                }
            }
        }
        // 0.32.21: Flow state field fallback.
        if let Some((flow_type_name, span_str)) = Self::parse_flow_field_id(field_id) {
            if let Some(td) = self.generator.type_defs.get(&flow_type_name) {
                if let crate::ast::TypeDefKind::Record(fields) = &td.kind {
                    // Match by name first.
                    for (i, f) in fields.iter().enumerate() {
                        if f.name == field_name {
                            return Ok(i as u32);
                        }
                    }
                    // Match by span.
                    if let Some(field) = Self::match_field_by_span(fields, &span_str) {
                        for (i, f) in fields.iter().enumerate() {
                            if std::ptr::eq(f, field) {
                                return Ok(i as u32);
                            }
                        }
                    }
                }
            }
        }
        // 0.36.7 (裁决 3/DoD #4): the Fault crash-context records
        // (SystemTrace/MemoryDump/PanicPayload) are checker-internal
        // builtins — their TypeDefs live in the checker's self.types but NOT
        // in the checked program's type_defs catalog, and their field ids
        // ship as bare `field:<name>`. Map them by the well-known constant
        // field order (mirrors the layouts in codegen/resolved/types.rs and
        // legacy codegen/compile.rs). Reached only after every catalog/name
        // fallback misses, so it cannot shadow user or flow-record fields.
        if let Some(index) = Self::builtin_trace_field_index(field_name) {
            return Ok(index);
        }
        // 0.1.8 Phase D: builtin record schemas (PeerFault, ExecResult,
        // StatResult, ...) also live outside the checked type_defs catalog.
        // Field ids are `builtin:type:PeerFault/field:peer_id`; map them by
        // the canonical schema order.
        if let Some(index) = Self::builtin_schema_field_index(field_id.0.as_str(), field_name) {
            return Ok(index);
        }
        Err(CompileError::Unsupported(format!(
            "field '{field_name}' ({}) not found in any type definition",
            field_id.0
        )))
    }

    /// Resolve a place to a pointer + type. `read` distinguishes index-read
    /// sites (negative indices wrap Python-style, VM ListGet parity) from
    /// index-write sites (negative indices trap, VM ListSet parity) — see
    /// `emit_checked_list_index` (H-15).
    fn root_place(
        &mut self,
        frame: &mut ResolvedFrame<'ctx>,
        place: &ResolvedPlace,
        read: bool,
    ) -> Result<ResolvedVarEntry<'ctx>, CompileError> {
        let base_entry = frame.locals.get(&place.base).copied().ok_or_else(|| {
            CompileError::Unsupported(format!(
                "resolved local '{}' is not bound in '{}'",
                place.base.0 .0, frame.owner.0
            ))
        })?;
        if place.projections.is_empty() {
            return Ok(base_entry);
        }
        // Walk projections: Tuple via struct GEP, Index via list data GEP.
        let mut current_ptr = base_entry.storage;
        let mut current_type = base_entry.llvm_type;
        for projection in &place.projections {
            match projection {
                // 0.37.x: `*ref` deref projection resolves through the
                // pointer stored in the reference local.
                crate::core::ir::ResolvedProjection::Deref { ty } => {
                    let ptr_ty = self
                        .generator
                        .context
                        .ptr_type(inkwell::AddressSpace::default());
                    let target_ptr = self
                        .generator
                        .build_load(
                            BasicTypeEnum::PointerType(ptr_ty),
                            current_ptr,
                            "deref_place_ptr",
                        )?
                        .into_pointer_value();
                    current_ptr = target_ptr;
                    current_type = self.lower_type(ty)?;
                }
                crate::core::ir::ResolvedProjection::Tuple { index, ty: _ } => {
                    let BasicTypeEnum::StructType(struct_type) = current_type else {
                        return Err(CompileError::Unsupported(
                            "tuple projection on non-struct place".into(),
                        ));
                    };
                    current_ptr = self
                        .generator
                        .builder
                        .build_struct_gep(struct_type, current_ptr, *index as u32, "place_gep")
                        .map_err(|e| CompileError::LlvmError(format!("place gep: {e}")))?;
                    current_type = struct_type
                        .get_field_type_at_index(*index as u32)
                        .ok_or_else(|| {
                            CompileError::LlvmError(format!(
                                "tuple field {index} absent in place projection"
                            ))
                        })?;
                }
                // 0.32.2: Index projection for List element access.
                // List is {i64 len, ptr data}; load data ptr, GEP by index.
                crate::core::ir::ResolvedProjection::Index { index, ty } => {
                    let BasicTypeEnum::StructType(struct_type) = current_type else {
                        return Err(CompileError::Unsupported(
                            "index projection on non-struct (list) place".into(),
                        ));
                    };
                    // Load the len (field 0) for the H-15 bounds check.
                    let i64_ty = self.generator.context.i64_type();
                    let len_gep = self
                        .generator
                        .builder
                        .build_struct_gep(struct_type, current_ptr, 0, "list_len_gep")
                        .map_err(|e| CompileError::LlvmError(format!("list len gep: {e}")))?;
                    let len_val = self
                        .generator
                        .build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "list_len_val")?
                        .into_int_value();
                    // Load the data pointer (field 1).
                    let data_gep = self
                        .generator
                        .builder
                        .build_struct_gep(struct_type, current_ptr, 1, "list_data_gep")
                        .map_err(|e| CompileError::LlvmError(format!("list data gep: {e}")))?;
                    let ptr_ty = self
                        .generator
                        .context
                        .ptr_type(inkwell::AddressSpace::default());
                    let data_ptr = self
                        .generator
                        .build_load(
                            BasicTypeEnum::PointerType(ptr_ty),
                            data_gep,
                            "list_data_ptr",
                        )?
                        .into_pointer_value();
                    // Evaluate the index expression.
                    let idx_val = match index {
                        crate::core::ir::ResolvedIndex::Constant(c) => self
                            .generator
                            .context
                            .i64_type()
                            .const_int(*c as u64, false),
                        crate::core::ir::ResolvedIndex::Dynamic(expr_id) => {
                            // Look up the index expression from place_inputs
                            // and emit it. Clone to release the immutable
                            // borrow on self before calling emit_expr.
                            let idx_expr =
                                self.place_inputs.get(expr_id).cloned().ok_or_else(|| {
                                    CompileError::Unsupported(format!(
                                        "dynamic index expression '{}' not in place_inputs",
                                        expr_id.0
                                    ))
                                })?;
                            self.emit_expr(&idx_expr, frame)?.into_int_value()
                        }
                    };
                    // H-15: bounds-check before the element GEP (VM parity:
                    // reads wrap negative indices, writes trap; OOB traps E0803).
                    let idx_val = self.emit_checked_list_index(
                        len_val,
                        idx_val,
                        read,
                        if read { "index read" } else { "index write" },
                    )?;
                    // GEP into the data buffer.
                    current_ptr = self.generator.build_in_bounds_gep(
                        i64_ty,
                        data_ptr,
                        &[idx_val],
                        "list_idx_gep",
                    )?;
                    // Element type: lower the resolved type identity.
                    let elem_llvm_ty = self.lower_type(ty)?;
                    // When the element is a struct (nested list, tuple, record,
                    // Option, Result) or a pointer (string), the i64 slot stores
                    // a pointer-to-value, not the value itself. Load the i64,
                    // inttoptr, then load the struct from the heap pointer —
                    // matching the legacy data layout.
                    // Only the string struct {ptr, i64} stores the raw pointer
                    // (field 0) directly; all other structs are heap-allocated.
                    // F-016 (0.40.1.20): when this Index is the FINAL projection
                    // and we are writing (`read == false`), the data-array slot
                    // is an `i64` handle — keep `current_ptr` as the element GEP
                    // and let the call site box the value into the slot. READ
                    // (`read == true`) and deeper writes (`ss[i].field = v`)
                    // still materialize the element value into an alloca. The
                    // prior code always loaded into an alloca, so `ss[i] = v`
                    // wrote the new value into a throwaway local and the data
                    // array was never updated (silent L1 divergence vs the VM).
                    let is_final_index = matches!(
                        place.projections.last(),
                        Some(crate::core::ir::ResolvedProjection::Index { .. })
                    );
                    match elem_llvm_ty {
                        BasicTypeEnum::StructType(sty) => {
                            if read {
                                let fields = sty.get_field_types();
                                let is_string = fields.len() == 2
                                    && matches!(&fields[0], BasicTypeEnum::PointerType(_))
                                    && matches!(&fields[1], BasicTypeEnum::IntType(bit) if bit.get_bit_width() == 64);
                                if is_string {
                                    // String: the i64 is a pointer to a fat MimiStr box.
                                    let loaded = self
                                        .generator
                                        .build_load(
                                            BasicTypeEnum::IntType(i64_ty),
                                            current_ptr,
                                            "list_elem_i64",
                                        )?
                                        .into_int_value();
                                    let ptr_ty = self
                                        .generator
                                        .context
                                        .ptr_type(inkwell::AddressSpace::default());
                                    let boxed = self.generator.build_int_to_ptr(
                                        loaded,
                                        ptr_ty,
                                        "elem_str_ptr",
                                    )?;
                                    let str_val = self.generator.load_fat_list_string(boxed)?;
                                    let str_alloca =
                                        self.generator.build_alloca(sty, "str_struct")?;
                                    self.generator.build_store(str_alloca, str_val)?;
                                    current_ptr = str_alloca;
                                    current_type = elem_llvm_ty;
                                } else {
                                    // Non-string struct (nested list, tuple, record, etc.):
                                    // load i64 pointer, inttoptr, load struct, store in alloca.
                                    let loaded = self
                                        .generator
                                        .build_load(
                                            BasicTypeEnum::IntType(i64_ty),
                                            current_ptr,
                                            "list_elem_i64",
                                        )?
                                        .into_int_value();
                                    let ptr_ty = self
                                        .generator
                                        .context
                                        .ptr_type(inkwell::AddressSpace::default());
                                    let struct_ptr = self
                                        .generator
                                        .build_int_to_ptr(loaded, ptr_ty, "elem_ptr")?;
                                    let struct_val = self.generator.build_load(
                                        BasicTypeEnum::StructType(sty),
                                        struct_ptr,
                                        "elem_struct",
                                    )?;
                                    let alloca = self.generator.build_alloca(sty, "elem_alloca")?;
                                    self.generator.build_store(alloca, struct_val)?;
                                    current_ptr = alloca;
                                    current_type = elem_llvm_ty;
                                }
                            } else if is_final_index {
                                // F-016 (0.40.1.20): final-index WRITE keeps the
                                // element GEP; the call site boxes the value into
                                // the `i64` data-array slot.
                                current_type = BasicTypeEnum::IntType(i64_ty);
                            } else {
                                // F-017 (0.40.1.21): non-final Index WRITE — the
                                // element has further projections (`rs[i].field = v`).
                                // The slot holds a pointer to the heap-boxed element;
                                // keep `current_ptr` as that box pointer so the
                                // subsequent Field/Tuple projection writes into the
                                // *real* element, not a discarded copy. The prior code
                                // loaded the struct into a local alloca and the store
                                // landed in the copy — a silent L1 divergence vs the
                                // VM (which mutates the actual element). This mirrors
                                // the dereference the read path already performs.
                                let loaded = self
                                    .generator
                                    .build_load(
                                        BasicTypeEnum::IntType(i64_ty),
                                        current_ptr,
                                        "list_elem_i64",
                                    )?
                                    .into_int_value();
                                let ptr_ty = self
                                    .generator
                                    .context
                                    .ptr_type(inkwell::AddressSpace::default());
                                let box_ptr = self.generator.build_int_to_ptr(
                                    loaded,
                                    ptr_ty,
                                    "elem_box_ptr",
                                )?;
                                current_ptr = box_ptr;
                                current_type = elem_llvm_ty;
                            }
                        }
                        BasicTypeEnum::PointerType(_) => {
                            if read || !is_final_index {
                                // String/raw pointer: load i64, inttoptr.
                                let loaded = self
                                    .generator
                                    .build_load(
                                        BasicTypeEnum::IntType(i64_ty),
                                        current_ptr,
                                        "list_elem_i64",
                                    )?
                                    .into_int_value();
                                let ptr_ty = self
                                    .generator
                                    .context
                                    .ptr_type(inkwell::AddressSpace::default());
                                let raw_ptr = self
                                    .generator
                                    .build_int_to_ptr(loaded, ptr_ty, "elem_ptr")?;
                                current_ptr = raw_ptr;
                                current_type = elem_llvm_ty;
                            } else {
                                // F-016 (0.40.1.20): final-index WRITE keeps the
                                // element GEP; the call site boxes the value into
                                // the `i64` data-array slot.
                                current_type = BasicTypeEnum::IntType(i64_ty);
                            }
                        }
                        _ => {
                            // Scalar element: keep the GEP'd i64 pointer.
                            current_type = elem_llvm_ty;
                        }
                    }
                }
                // 0.32.5: Field projection for record field access.
                // Look up the field index from the type definition catalog.
                crate::core::ir::ResolvedProjection::Field { field, name, ty } => {
                    let BasicTypeEnum::StructType(struct_type) = current_type else {
                        return Err(CompileError::Unsupported(
                            "field projection on non-struct (record) place".into(),
                        ));
                    };
                    let field_index = self.lookup_field_index(field, name)?;
                    current_ptr = self
                        .generator
                        .builder
                        .build_struct_gep(struct_type, current_ptr, field_index, "rec_field_gep")
                        .map_err(|e| CompileError::LlvmError(format!("field gep: {e}")))?;
                    current_type = self.lower_type(ty)?;
                }
            }
        }
        Ok(ResolvedVarEntry {
            storage: current_ptr,
            llvm_type: current_type,
        })
    }

    fn current_block_terminated(&self) -> bool {
        self.generator
            .builder
            .get_insert_block()
            .and_then(|block| block.get_terminator())
            .is_some()
    }

    fn current_function(&self) -> Result<inkwell::values::FunctionValue<'ctx>, CompileError> {
        self.generator
            .builder
            .get_insert_block()
            .and_then(|block| block.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no current function for block creation".into()))
    }

    /// Ensure a value is `i1` for use as a branch condition.
    fn ensure_bool(
        &self,
        value: BasicValueEnum<'ctx>,
    ) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
        match value {
            BasicValueEnum::IntValue(int) if int.get_type().get_bit_width() == 1 => Ok(int),
            BasicValueEnum::IntValue(int) => {
                let zero = int.get_type().const_zero();
                self.generator
                    .builder
                    .build_int_compare(inkwell::IntPredicate::NE, int, zero, "to_bool")
                    .map_err(|e| CompileError::LlvmError(format!("bool conversion: {e}")))
            }
            _ => Err(CompileError::Unsupported(
                "condition value is not an integer".into(),
            )),
        }
    }

    /// H-15 (full-audit 2026-08-05, HIGH): bounds-check a list index before
    /// the element GEP. The previous code went straight to
    /// `build_in_bounds_gep`, so an OOB index read garbage at O0 and poison
    /// at O1+ (silent miscompilation + L3) while the VM traps (E0803
    /// "index out of bounds") and legacy checks (check_list_bounds).
    ///
    /// Semantics follow the VM (Op::ListGet / Op::ListSet):
    /// - READ: negative indices wrap Python-style (`xs[-1]` = last element);
    ///   wrap past the front traps.
    /// - WRITE: negative indices trap outright (ListSet rejects them).
    /// Both directions trap when the effective index >= len.
    ///
    /// Returns the effective (possibly wrapped) i64 index to GEP with.
    fn emit_checked_list_index(
        &mut self,
        len: inkwell::values::IntValue<'ctx>,
        idx: inkwell::values::IntValue<'ctx>,
        read: bool,
        operation: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
        let i64_ty = self.generator.context.i64_type();
        // Widen narrow index values (i32 literals/locals) to the i64 width
        // of `len` so the compares are well-formed.
        let idx = match idx.get_type().get_bit_width() {
            64 => idx,
            width if width < 64 => self
                .generator
                .builder
                .build_int_s_extend(idx, i64_ty, "idx_sext")
                .map_err(|e| CompileError::LlvmError(format!("index sext: {e}")))?,
            _ => {
                return Err(CompileError::Unsupported(format!(
                    "list index width {} is not supported",
                    idx.get_type().get_bit_width()
                )))
            }
        };
        let zero = i64_ty.const_int(0, false);
        // READ: wrap negative indices (VM ListGet parity).
        let idx = if read {
            let is_neg = self
                .generator
                .builder
                .build_int_compare(inkwell::IntPredicate::SLT, idx, zero, "idx_neg")
                .map_err(|e| CompileError::LlvmError(format!("index cmp: {e}")))?;
            let wrapped = self
                .generator
                .builder
                .build_int_add(idx, len, "idx_wrap")
                .map_err(|e| CompileError::LlvmError(format!("index wrap: {e}")))?;
            self.generator
                .builder
                .build_select(is_neg, wrapped, idx, "idx_eff")
                .map_err(|e| CompileError::LlvmError(format!("index select: {e}")))?
                .into_int_value()
        } else {
            idx
        };
        // OOB condition: still negative (read wrapped past the front, or a
        // negative write) or >= len.
        let neg = self
            .generator
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, idx, zero, "idx_oob_neg")
            .map_err(|e| CompileError::LlvmError(format!("index cmp: {e}")))?;
        let ge_len = self
            .generator
            .builder
            .build_int_compare(inkwell::IntPredicate::UGE, idx, len, "idx_oob_ge")
            .map_err(|e| CompileError::LlvmError(format!("index cmp: {e}")))?;
        let oob = self
            .generator
            .builder
            .build_or(neg, ge_len, "idx_oob")
            .map_err(|e| CompileError::LlvmError(format!("index or: {e}")))?;

        let function = self.current_function()?;
        let trap_bb = self
            .generator
            .context
            .append_basic_block(function, "list_idx_oob");
        let ok_bb = self
            .generator
            .context
            .append_basic_block(function, "list_idx_ok");
        self.generator.build_cond_br(oob, trap_bb, ok_bb)?;

        // Trap block: E0803 (index out of bounds at runtime). The code rides
        // the message the same way contract messages carry `[E0808]`
        // (mimi_runtime_abort doc, 0.34.34).
        self.generator.builder.position_at_end(trap_bb);
        let abort_fn = self.generator.get_or_declare_abort_fn();
        let msg = format!("[E0803] list index out of bounds: {operation}");
        let msg_ptr = self
            .generator
            .builder
            .build_global_string_ptr(&msg, "idx_oob_msg")
            .map_err(|e| CompileError::LlvmError(format!("index oob msg: {e}")))?;
        self.generator.build_call(
            abort_fn,
            &[BasicMetadataValueEnum::PointerValue(
                msg_ptr.as_pointer_value(),
            )],
            "idx_oob_abort",
        )?;
        // SAFETY: mimi_runtime_abort is noreturn; this block is unreachable.
        self.generator
            .builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("index unreachable: {e}")))?;

        self.generator.builder.position_at_end(ok_bb);
        Ok(idx)
    }

    /// Compile a resolved if expression. When `as_statement` is true the
    /// branch values are discarded (statement-position if — legacy
    /// `compile_if_stmt` merge_vars=true parity). Branch values are NOT
    /// coerced into the unit result type: e.g. `if cond { push(list, x) }`
    /// yields push's list pointer in the then branch while the if's type is
    /// unit (i64) — coercing ptr→i64 failed and dropped the whole function
    /// to the legacy emitter (mimi-log collect_latencies, 2026-08-09).
    fn emit_if(
        &mut self,
        expression: &ResolvedExpr,
        condition: &ResolvedExpr,
        then_block: &ResolvedBlock,
        else_block: &ResolvedBlock,
        frame: &mut ResolvedFrame<'ctx>,
        as_statement: bool,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let result_type = self.lower_type(&expression.ty)?;
        let result_alloca = if as_statement {
            None
        } else {
            Some(self.generator.build_alloca(result_type, "if_result")?)
        };

        let cond_value = self.emit_expr(condition, frame)?;
        let cond_bool = self.ensure_bool(cond_value)?;

        let function = self.current_function()?;
        let then_bb = self.generator.context.append_basic_block(function, "then");
        let else_bb = self.generator.context.append_basic_block(function, "else");
        let merge_bb = self
            .generator
            .context
            .append_basic_block(function, "if_merge");

        self.generator.build_cond_br(cond_bool, then_bb, else_bb)?;

        // Then branch
        self.generator.builder.position_at_end(then_bb);
        let body = self.program.callable(&frame.owner).ok_or_else(|| {
            CompileError::Unsupported(format!(
                "resolved callable '{}' is absent for if",
                frame.owner.0
            ))
        })?;
        let then_value = self.emit_block(&body.body, then_block, frame)?;
        let then_terminated = self.current_block_terminated();
        if !then_terminated {
            if !as_statement {
                let result_alloca = result_alloca.expect("if alloca present in expr mode");
                if let Some(value) = then_value {
                    let value = self.coerce_to(value, result_type)?;
                    self.generator.build_store(result_alloca, value)?;
                } else {
                    self.generator
                        .build_store(result_alloca, result_type.const_zero())?;
                }
            }
            self.generator.build_br(merge_bb)?;
        }

        // Else branch
        self.generator.builder.position_at_end(else_bb);
        let else_value = self.emit_block(&body.body, else_block, frame)?;
        let else_terminated = self.current_block_terminated();
        if !else_terminated {
            if !as_statement {
                let result_alloca = result_alloca.expect("if alloca present in expr mode");
                if let Some(value) = else_value {
                    let value = self.coerce_to(value, result_type)?;
                    self.generator.build_store(result_alloca, value)?;
                } else {
                    self.generator
                        .build_store(result_alloca, result_type.const_zero())?;
                }
            }
            self.generator.build_br(merge_bb)?;
        }

        // Merge
        self.generator.builder.position_at_end(merge_bb);
        if as_statement {
            Ok(result_type.const_zero())
        } else {
            let result_alloca = result_alloca.expect("if alloca present in expr mode");
            self.generator
                .build_load(result_type, result_alloca, "if_val")
        }
    }

    fn emit_while(
        &mut self,
        body: &ResolvedBody,
        condition: &ResolvedExpr,
        loop_body: &ResolvedBlock,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<(), CompileError> {
        let function = self.current_function()?;
        let header = self
            .generator
            .context
            .append_basic_block(function, "while_header");
        let body_bb = self
            .generator
            .context
            .append_basic_block(function, "while_body");
        let exit = self
            .generator
            .context
            .append_basic_block(function, "while_exit");

        self.generator.build_br(header)?;

        // Header: evaluate condition
        self.generator.builder.position_at_end(header);
        let cond = self.emit_expr(condition, frame)?;
        let cond = self.ensure_bool(cond)?;
        self.generator.build_cond_br(cond, body_bb, exit)?;

        // Body
        self.generator.builder.position_at_end(body_bb);
        self.loop_stack.push(LoopContext { header, exit });
        // 0.37.30: deterministic per-iteration drop for loop-body locals.
        self.generator.push_heap_scope();
        self.emit_block(body, loop_body, frame)?;
        self.loop_stack.pop();
        if !self.current_block_terminated() {
            self.generator.free_heap_allocs()?;
        } else {
            self.generator.drain_heap_scope();
        }
        if !self.current_block_terminated() {
            self.generator.build_br(header)?;
            // C1c (0.35.41): disable aggressive unrolling of serial-chain hot
            // loops (dsp-style) — see CodeGenerator::cap_loop_unroll.
            self.generator.cap_loop_unroll()?;
        }

        // Exit
        self.generator.builder.position_at_end(exit);
        Ok(())
    }

    fn emit_loop(
        &mut self,
        body: &ResolvedBody,
        loop_body: &ResolvedBlock,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<(), CompileError> {
        let function = self.current_function()?;
        let header = self
            .generator
            .context
            .append_basic_block(function, "loop_header");
        let exit = self
            .generator
            .context
            .append_basic_block(function, "loop_exit");

        self.generator.build_br(header)?;

        // Header: unconditional entry to body (infinite loop)
        self.generator.builder.position_at_end(header);
        self.loop_stack.push(LoopContext { header, exit });
        // 0.37.30: deterministic per-iteration drop for loop-body locals.
        self.generator.push_heap_scope();
        self.emit_block(body, loop_body, frame)?;
        self.loop_stack.pop();
        if !self.current_block_terminated() {
            self.generator.free_heap_allocs()?;
        } else {
            self.generator.drain_heap_scope();
        }
        if !self.current_block_terminated() {
            self.generator.build_br(header)?;
            self.generator.cap_loop_unroll()?;
        }

        // Exit (only reachable via break)
        self.generator.builder.position_at_end(exit);
        Ok(())
    }

    fn emit_for(
        &mut self,
        body: &ResolvedBody,
        pattern: &ResolvedPattern,
        iterable: &ResolvedExpr,
        loop_body: &ResolvedBlock,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<(), CompileError> {
        match &iterable.kind {
            ResolvedExprKind::Range { start, end } => {
                self.emit_for_range(body, pattern, start, end, loop_body, frame)
            }
            // 0.32.14: `range(start, end)` as a builtin call — dispatch
            // to emit_for_range by extracting the call arguments.
            ResolvedExprKind::Call(call)
                if matches!(call.callee, ResolvedCallee::Builtin(ref id) if id.as_str() == "range")
                    && call.arguments.len() == 2 =>
            {
                self.emit_for_range(
                    body,
                    pattern,
                    &call.arguments[0].value,
                    &call.arguments[1].value,
                    loop_body,
                    frame,
                )
            }
            // 0.32.8–0.32.9: for-in-list iteration. Any expression whose
            // type is List<T> is accepted (Load, Call, Project, etc.).
            _ => self.emit_for_list(body, pattern, iterable, loop_body, frame),
        }
    }

    /// For-in-range: `for i in range(start, end) { ... }`
    fn emit_for_range(
        &mut self,
        body: &ResolvedBody,
        pattern: &ResolvedPattern,
        start: &ResolvedExpr,
        end: &ResolvedExpr,
        loop_body: &ResolvedBlock,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<(), CompileError> {
        let start_value = self.emit_expr(start, frame)?;
        let end_value = self.emit_expr(end, frame)?;

        // Bind the loop variable (pattern) to an alloca
        let ResolvedPatternKind::Binding {
            local,
            by_reference: None,
        } = &pattern.kind
        else {
            return Err(CompileError::Unsupported(
                "non-binding for-loop pattern escaped eligibility".into(),
            ));
        };
        let metadata = body.locals.get(local).ok_or_else(|| {
            CompileError::Unsupported(format!(
                "resolved for-loop local '{}' is absent",
                local.0 .0
            ))
        })?;
        let llvm_type = self.lower_type(&metadata.ty)?;
        let storage = self
            .generator
            .build_alloca(llvm_type, &metadata.display_name)?;
        let start_value = self.coerce_to(start_value, llvm_type)?;
        self.generator.build_store(storage, start_value)?;
        frame
            .locals
            .insert(local.clone(), ResolvedVarEntry { storage, llvm_type });

        let end_value = self.coerce_to(end_value, llvm_type)?;
        let end_int = match end_value {
            BasicValueEnum::IntValue(int) => int,
            _ => {
                return Err(CompileError::Unsupported(
                    "for-loop end is not an integer".into(),
                ))
            }
        };

        // Determine signedness from the start expression's canonical type
        let predicate = if is_signed_integer_type(self.program, &start.ty) {
            inkwell::IntPredicate::SLT
        } else {
            inkwell::IntPredicate::ULT
        };

        let function = self.current_function()?;
        let header = self
            .generator
            .context
            .append_basic_block(function, "for_header");
        let body_bb = self
            .generator
            .context
            .append_basic_block(function, "for_body");
        let exit = self
            .generator
            .context
            .append_basic_block(function, "for_exit");

        self.generator.build_br(header)?;

        // Header: compare counter < end
        self.generator.builder.position_at_end(header);
        let counter = self
            .generator
            .build_load(llvm_type, storage, "for_counter")?;
        let counter_int = match counter {
            BasicValueEnum::IntValue(int) => int,
            _ => {
                return Err(CompileError::Unsupported(
                    "for-loop counter is not an integer".into(),
                ))
            }
        };
        let cond = self
            .generator
            .builder
            .build_int_compare(predicate, counter_int, end_int, "for_cond")
            .map_err(|e| CompileError::LlvmError(format!("for compare: {e}")))?;
        self.generator.build_cond_br(cond, body_bb, exit)?;

        // Body
        self.generator.builder.position_at_end(body_bb);
        // AUD-1 (2026-08-20): point `continue` at a latch block that
        // increments, so the counter advances instead of freezing.
        let latch_bb = self
            .generator
            .context
            .append_basic_block(function, "for_latch");
        self.loop_stack.push(LoopContext {
            header: latch_bb,
            exit,
        });
        // 0.37.30: deterministic per-iteration drop for loop-body locals.
        self.generator.push_heap_scope();
        self.emit_block(body, loop_body, frame)?;
        self.loop_stack.pop();

        // Normal (non-terminated) body fall-through -> latch. `continue`
        // already targets `latch_bb` via LoopContext.header.
        if !self.current_block_terminated() {
            self.generator.free_heap_allocs()?;
        } else {
            self.generator.drain_heap_scope();
        }
        if !self.current_block_terminated() {
            self.generator.build_br(latch_bb)?;
        }

        // Latch: increment the counter and branch back to the condition header.
        // If the body already terminated (break/return) this block is
        // unreachable, but it still needs a valid terminator, so always emit.
        self.generator.builder.position_at_end(latch_bb);
        let current = self
            .generator
            .build_load(llvm_type, storage, "for_reload")?;
        let current_int = match current {
            BasicValueEnum::IntValue(int) => int,
            _ => {
                return Err(CompileError::Unsupported(
                    "for-loop counter is not an integer".into(),
                ))
            }
        };
        let one = current_int.get_type().const_int(1, false);
        let next = self
            .generator
            .builder
            .build_int_add(current_int, one, "for_next")
            .map_err(|e| CompileError::LlvmError(format!("for increment: {e}")))?;
        self.generator.build_store(storage, next)?;
        self.generator.build_br(header)?;

        // Exit
        self.generator.builder.position_at_end(exit);
        Ok(())
    }

    /// For-in-list: `for x in expr { ... }` where expr: List<T>.
    /// Lowered to: idx=0; while idx < len(xs) { x = xs[idx]; body; idx++ }
    fn emit_for_list(
        &mut self,
        body: &ResolvedBody,
        pattern: &ResolvedPattern,
        iterable: &ResolvedExpr,
        loop_body: &ResolvedBlock,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<(), CompileError> {
        let i64_ty = self.generator.context.i64_type();

        // Evaluate the iterable expression to get the list struct {i64, ptr}.
        let list_val = self.emit_expr(iterable, frame)?;
        // 0.39.136: call-result iterables (e.g. `for w in str_split(s, " ")`
        // inside std::strings::words) can surface as pointers to the list
        // struct rather than bare struct values; load through them instead of
        // refusing and silently falling the whole function back to legacy.
        let list_struct = match list_val {
            BasicValueEnum::StructValue(sv) => sv,
            BasicValueEnum::PointerValue(pv) => self
                .generator
                .builder
                .build_load(
                    BasicTypeEnum::StructType(self.generator.list_struct_type()),
                    pv,
                    "for_in_list_ptr_load",
                )
                .map_err(|e| CompileError::LlvmError(format!("for-in-list ptr load: {e}")))?
                .into_struct_value(),
            _ => {
                return Err(CompileError::Unsupported(
                    "for-in-list iterable is not a list struct".into(),
                ))
            }
        };

        // Extract len (field 0) and data pointer (field 1).
        let len_val = self
            .generator
            .builder
            .build_extract_value(list_struct, 0, "for_list_len")
            .map_err(|e| CompileError::LlvmError(format!("extract list len: {e}")))?
            .into_int_value();
        let data_ptr = self
            .generator
            .builder
            .build_extract_value(list_struct, 1, "for_list_data")
            .map_err(|e| CompileError::LlvmError(format!("extract list data: {e}")))?
            .into_pointer_value();

        // Determine the element LLVM type from the pattern's local metadata.
        let ResolvedPatternKind::Binding {
            local,
            by_reference: None,
        } = &pattern.kind
        else {
            return Err(CompileError::Unsupported(
                "non-binding for-in-list pattern escaped eligibility".into(),
            ));
        };
        let metadata = body.locals.get(local).ok_or_else(|| {
            CompileError::Unsupported(format!(
                "resolved for-in-list local '{}' is absent",
                local.0 .0
            ))
        })?;
        let elem_llvm_ty = self.lower_type(&metadata.ty)?;

        // Allocate index counter = 0.
        let idx_storage = self
            .generator
            .build_alloca(BasicTypeEnum::IntType(i64_ty), "for_list_idx")?;
        self.generator
            .build_store(idx_storage, i64_ty.const_int(0, false))?;

        // Allocate element variable storage.
        let elem_storage = self
            .generator
            .build_alloca(elem_llvm_ty, &metadata.display_name)?;
        frame.locals.insert(
            local.clone(),
            ResolvedVarEntry {
                storage: elem_storage,
                llvm_type: elem_llvm_ty,
            },
        );

        let function = self.current_function()?;
        let header = self
            .generator
            .context
            .append_basic_block(function, "for_list_header");
        let body_bb = self
            .generator
            .context
            .append_basic_block(function, "for_list_body");
        let exit = self
            .generator
            .context
            .append_basic_block(function, "for_list_exit");

        self.generator.build_br(header)?;

        // Header: idx < len
        self.generator.builder.position_at_end(header);
        let idx_val = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                idx_storage,
                "for_list_idx_val",
            )?
            .into_int_value();
        let cond = self
            .generator
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                idx_val,
                len_val,
                "for_list_cond",
            )
            .map_err(|e| CompileError::LlvmError(format!("for-list compare: {e}")))?;
        self.generator.build_cond_br(cond, body_bb, exit)?;

        // Body: load element, bind, emit loop body.
        self.generator.builder.position_at_end(body_bb);

        // GEP into data array at idx, load i64, convert to element type.
        let elem_ptr = self.generator.build_in_bounds_gep(
            i64_ty,
            data_ptr,
            &[idx_val],
            "for_list_elem_ptr",
        )?;
        let elem_i64 = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                elem_ptr,
                "for_list_elem_i64",
            )?
            .into_int_value();
        let elem_val = self.convert_list_elem_i64(elem_i64, elem_llvm_ty)?;
        self.generator.build_store(elem_storage, elem_val)?;

        // AUD-1 (2026-08-20): point `continue` at a latch block that
        // increments the index, so the loop advances instead of freezing.
        let latch_bb = self
            .generator
            .context
            .append_basic_block(function, "for_list_latch");
        self.loop_stack.push(LoopContext {
            header: latch_bb,
            exit,
        });
        // 0.37.30: deterministic per-iteration drop for loop-body locals.
        self.generator.push_heap_scope();
        self.emit_block(body, loop_body, frame)?;
        self.loop_stack.pop();

        // Normal (non-terminated) body fall-through -> latch. `continue`
        // already targets `latch_bb` via LoopContext.header.
        if !self.current_block_terminated() {
            self.generator.free_heap_allocs()?;
        } else {
            self.generator.drain_heap_scope();
        }
        if !self.current_block_terminated() {
            self.generator.build_br(latch_bb)?;
        }

        // Latch: increment idx and branch back to the condition header.
        // Unreachable when the body terminated (break/return) but still needs
        // a valid terminator, so always emit it.
        self.generator.builder.position_at_end(latch_bb);
        let cur_idx = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                idx_storage,
                "for_list_idx_reload",
            )?
            .into_int_value();
        let next_idx = self
            .generator
            .builder
            .build_int_add(cur_idx, i64_ty.const_int(1, false), "for_list_idx_next")
            .map_err(|e| CompileError::LlvmError(format!("for-list increment: {e}")))?;
        self.generator.build_store(idx_storage, next_idx)?;
        self.generator.build_br(header)?;

        // Exit
        self.generator.builder.position_at_end(exit);
        // Remove the loop variable from the frame after the loop.
        frame.locals.remove(local);
        Ok(())
    }

    /// Try expression (`?` operator): unwrap Result/Option or exit.
    ///
    /// Layout:
    /// 0.32.35: Callable (first-class function value).
    ///
    /// Returns a pointer to the declared LLVM function symbol. The function
    /// must already be declared (by the legacy emitter's forward declaration
    /// pass or by the resolved emitter's own declarations).
    fn emit_callable_ref(
        &self,
        callee: &ResolvedCallee,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match callee {
            ResolvedCallee::Function(callee_owner) => {
                let callee_fn = self.program.functions().get(callee_owner).ok_or_else(|| {
                    CompileError::Unsupported(format!(
                        "callable reference to unknown function '{:?}'",
                        callee_owner
                    ))
                })?;
                let fn_name = &callee_fn.qualified_name;
                let llvm_fn = self.generator.module.get_function(fn_name).ok_or_else(|| {
                    CompileError::LlvmError(format!(
                        "callable reference: function '{}' not declared in LLVM module",
                        fn_name
                    ))
                })?;
                Ok(llvm_fn.as_global_value().as_pointer_value().into())
            }
            _ => Err(CompileError::Unsupported(
                "only Function callees are supported as first-class values".into(),
            )),
        }
    }

    /// 0.32.34: OptionalChain (receiver?.field).
    ///
    /// Semantics: if receiver is Some(x)/Ok(x), project field from x and wrap
    /// in Some. If receiver is None/Err, return None.
    ///
    /// LLVM lowering:
    /// 1. Emit receiver → Option/Result struct {i1 disc, T payload, ...}
    /// 2. Branch on discriminant
    /// 3. Some/Ok: extract payload, project field, build {i1 1, field_val}
    /// 4. None/Err: build {i1 0, zero}
    /// 5. PHI merge
    fn emit_optional_chain(
        &mut self,
        receiver: &ResolvedExpr,
        field: &NodeId,
        field_type: &ResolvedTypeId,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // Emit receiver → struct value.
        let recv_val = self.emit_expr(receiver, frame)?;
        let sv = match recv_val {
            BasicValueEnum::StructValue(sv) => sv,
            BasicValueEnum::PointerValue(pv) => {
                let llvm_ty = self.lower_type(&receiver.ty)?;
                self.generator
                    .build_load(llvm_ty, pv, "opt_chain_load")?
                    .into_struct_value()
            }
            _ => {
                return Err(CompileError::Unsupported(
                    "optional chain receiver is not a struct".into(),
                ))
            }
        };

        // Extract discriminant (field 0).
        let disc = self
            .generator
            .builder
            .build_extract_value(sv, 0, "opt_chain_disc")
            .map_err(|e| CompileError::LlvmError(format!("opt_chain disc: {e}")))?
            .into_int_value();

        // Extract payload (field 1) — the record value.
        let payload = self
            .generator
            .builder
            .build_extract_value(sv, 1, "opt_chain_payload")
            .map_err(|e| CompileError::LlvmError(format!("opt_chain payload: {e}")))?;

        // Get field name and index.
        let field_name = self.program.resolved_member_name(field).ok_or_else(|| {
            CompileError::Unsupported(format!(
                "optional chain field '{:?}' has no resolved name",
                field
            ))
        })?;
        let field_index = self.lookup_field_index(field, field_name)?;

        // Determine the result LLVM type: Option<FieldType> = {i1, FieldType}.
        let result_llvm_ty = self.lower_type(field_type)?;

        // Branch on discriminant.
        let function = self.current_function()?;
        let some_bb = self
            .generator
            .context
            .append_basic_block(function, "opt_chain_some");
        let none_bb = self
            .generator
            .context
            .append_basic_block(function, "opt_chain_none");
        let merge_bb = self
            .generator
            .context
            .append_basic_block(function, "opt_chain_merge");

        let is_some = self
            .generator
            .builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                disc,
                disc.get_type().const_zero(),
                "opt_chain_is_some",
            )
            .map_err(|e| CompileError::LlvmError(format!("opt_chain cmp: {e}")))?;
        self.generator.build_cond_br(is_some, some_bb, none_bb)?;

        // Some/Ok branch: project field from payload, build Some result.
        self.generator.builder.position_at_end(some_bb);
        let payload_struct = match payload {
            BasicValueEnum::StructValue(psv) => psv,
            _ => {
                return Err(CompileError::Unsupported(
                    "optional chain payload is not a struct (expected record)".into(),
                ))
            }
        };
        let field_val = self
            .generator
            .builder
            .build_extract_value(payload_struct, field_index, "opt_chain_field")
            .map_err(|e| CompileError::LlvmError(format!("opt_chain field extract: {e}")))?;
        // Build Some {i1 1, field_val}.
        let bool_ty = self.generator.context.bool_type();
        let some_result = self.generator.context.struct_type(
            &[BasicTypeEnum::IntType(bool_ty), field_val.get_type()],
            false,
        );
        let some_val = some_result.get_undef();
        let some_val = self
            .generator
            .builder
            .build_insert_value(some_val, bool_ty.const_int(1, false), 0, "some_disc")
            .map_err(|e| CompileError::LlvmError(format!("opt_chain some disc: {e}")))?
            .into_struct_value();
        let some_val = self
            .generator
            .builder
            .build_insert_value(some_val, field_val, 1, "some_payload")
            .map_err(|e| CompileError::LlvmError(format!("opt_chain some payload: {e}")))?
            .into_struct_value();
        self.generator.build_br(merge_bb)?;
        let some_bb_end = self.generator.builder.get_insert_block().ok_or_else(|| {
            CompileError::LlvmError("opt_chain: no insert block after some_bb".into())
        })?;

        // None/Err branch: build None {i1 0, zero}.
        self.generator.builder.position_at_end(none_bb);
        let none_val = some_result.get_undef();
        let none_val = self
            .generator
            .builder
            .build_insert_value(none_val, bool_ty.const_int(0, false), 0, "none_disc")
            .map_err(|e| CompileError::LlvmError(format!("opt_chain none disc: {e}")))?
            .into_struct_value();
        let zero_payload = field_val.get_type().const_zero();
        let none_val = self
            .generator
            .builder
            .build_insert_value(none_val, zero_payload, 1, "none_payload")
            .map_err(|e| CompileError::LlvmError(format!("opt_chain none payload: {e}")))?
            .into_struct_value();
        self.generator.build_br(merge_bb)?;
        let none_bb_end = self.generator.builder.get_insert_block().ok_or_else(|| {
            CompileError::LlvmError("opt_chain: no insert block after none_bb".into())
        })?;

        // Merge: PHI node.
        self.generator.builder.position_at_end(merge_bb);
        let phi = self
            .generator
            .builder
            .build_phi(result_llvm_ty, "opt_chain_result")
            .map_err(|e| CompileError::LlvmError(format!("opt_chain phi: {e}")))?;
        phi.add_incoming(&[
            (&some_val as &dyn inkwell::values::BasicValue, some_bb_end),
            (&none_val as &dyn inkwell::values::BasicValue, none_bb_end),
        ]);
        Ok(phi.as_basic_value())
    }

    /// 0.32.33: Comprehension ([value for pattern in iterable if guard]).
    ///
    /// Lowering: pre-allocate buffer of iterable_len elements, loop over
    /// iterable, bind pattern, check guard, evaluate value, store at count
    /// offset, increment count. Build result list { count, data_ptr }.
    #[allow(clippy::too_many_arguments)]
    fn emit_comprehension(
        &mut self,
        pattern: &ResolvedPattern,
        value: &ResolvedExpr,
        iterable: &ResolvedExpr,
        guard: Option<&ResolvedExpr>,
        _result_ty: &ResolvedTypeId,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i64_ty = self.generator.context.i64_type();
        let list_ty = self.generator.list_struct_type();

        // Evaluate iterable → list struct.
        let list_val = self.emit_expr(iterable, frame)?;
        let list_struct = match list_val {
            BasicValueEnum::StructValue(sv) => sv,
            _ => {
                return Err(CompileError::Unsupported(
                    "comprehension iterable is not a list struct".into(),
                ))
            }
        };
        let iter_len = self
            .generator
            .builder
            .build_extract_value(list_struct, 0, "comp_iter_len")
            .map_err(|e| CompileError::LlvmError(format!("comp extract len: {e}")))?
            .into_int_value();
        let iter_data = self
            .generator
            .builder
            .build_extract_value(list_struct, 1, "comp_iter_data")
            .map_err(|e| CompileError::LlvmError(format!("comp extract data: {e}")))?
            .into_pointer_value();

        // Pre-allocate result buffer: iter_len * 8 bytes (worst case: all pass guard).
        let elem_size = i64_ty.const_int(8, false);
        let alloc_bytes = self
            .generator
            .builder
            .build_int_mul(iter_len, elem_size, "comp_alloc_bytes")
            .map_err(|e| CompileError::LlvmError(format!("comp alloc mul: {e}")))?;
        let result_data = self.generator.malloc_or_abort(alloc_bytes, "comp_malloc")?;

        // Count variable (starts at 0).
        let count_storage = self
            .generator
            .build_alloca(BasicTypeEnum::IntType(i64_ty), "comp_count")?;
        self.generator
            .build_store(count_storage, i64_ty.const_int(0, false))?;

        // Index variable (starts at 0).
        let idx_storage = self
            .generator
            .build_alloca(BasicTypeEnum::IntType(i64_ty), "comp_idx")?;
        self.generator
            .build_store(idx_storage, i64_ty.const_int(0, false))?;

        // Bind pattern: allocate storage for the loop variable.
        let ResolvedPatternKind::Binding {
            local,
            by_reference: None,
        } = &pattern.kind
        else {
            return Err(CompileError::Unsupported(
                "comprehension pattern escaped eligibility (not a simple binding)".into(),
            ));
        };
        // We need the body to look up local metadata. Use the expression's type
        // context — the iterable element type is the pattern's type.
        let elem_llvm_ty = BasicTypeEnum::IntType(i64_ty); // elements stored as i64
        let elem_storage = self.generator.build_alloca(elem_llvm_ty, "comp_elem")?;
        frame.locals.insert(
            local.clone(),
            ResolvedVarEntry {
                storage: elem_storage,
                llvm_type: elem_llvm_ty,
            },
        );

        // Loop blocks.
        let function = self.current_function()?;
        let header_bb = self
            .generator
            .context
            .append_basic_block(function, "comp_header");
        let body_bb = self
            .generator
            .context
            .append_basic_block(function, "comp_body");
        let guard_bb = self
            .generator
            .context
            .append_basic_block(function, "comp_guard");
        let push_bb = self
            .generator
            .context
            .append_basic_block(function, "comp_push");
        let incr_bb = self
            .generator
            .context
            .append_basic_block(function, "comp_incr");
        let done_bb = self
            .generator
            .context
            .append_basic_block(function, "comp_done");

        // Branch to header.
        self.generator.build_br(header_bb)?;

        // Header: idx < iter_len?
        self.generator.builder.position_at_end(header_bb);
        let idx_val = self
            .generator
            .build_load(BasicTypeEnum::IntType(i64_ty), idx_storage, "comp_idx_val")?
            .into_int_value();
        let cond = self
            .generator
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, idx_val, iter_len, "comp_cond")
            .map_err(|e| CompileError::LlvmError(format!("comp cmp: {e}")))?;
        self.generator.build_cond_br(cond, body_bb, done_bb)?;

        // Body: load element at iter_data[idx], store in elem_storage.
        self.generator.builder.position_at_end(body_bb);
        let elem_ptr =
            self.generator
                .build_in_bounds_gep(i64_ty, iter_data, &[idx_val], "comp_elem_ptr")?;
        let elem_val =
            self.generator
                .build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "comp_elem_val")?;
        self.generator.build_store(elem_storage, elem_val)?;

        // If guard exists, branch to guard_bb; otherwise go to push_bb.
        if guard.is_some() {
            self.generator.build_br(guard_bb)?;
        } else {
            self.generator.build_br(push_bb)?;
        }

        // Guard: evaluate guard expression, branch on truthiness.
        if let Some(guard_expr) = guard {
            self.generator.builder.position_at_end(guard_bb);
            let guard_val = self.emit_expr(guard_expr, frame)?;
            let guard_bool = self.ensure_bool(guard_val)?;
            self.generator.build_cond_br(guard_bool, push_bb, incr_bb)?;
        }

        // Push: evaluate value expression, store at result_data[count].
        self.generator.builder.position_at_end(push_bb);
        let val = self.emit_expr(value, frame)?;
        let val_i64 = self.coerce_to_i64(val)?;
        let count_val = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                count_storage,
                "comp_count_val",
            )?
            .into_int_value();
        let store_ptr = self.generator.build_in_bounds_gep(
            i64_ty,
            result_data,
            &[count_val],
            "comp_store_ptr",
        )?;
        self.generator.build_store(store_ptr, val_i64)?;
        // Increment count.
        let new_count = self
            .generator
            .builder
            .build_int_add(count_val, i64_ty.const_int(1, false), "comp_new_count")
            .map_err(|e| CompileError::LlvmError(format!("comp add: {e}")))?;
        self.generator.build_store(count_storage, new_count)?;
        self.generator.build_br(incr_bb)?;

        // Incr: increment idx, branch to header.
        self.generator.builder.position_at_end(incr_bb);
        let idx_cur = self
            .generator
            .build_load(BasicTypeEnum::IntType(i64_ty), idx_storage, "comp_idx_cur")?
            .into_int_value();
        let idx_next = self
            .generator
            .builder
            .build_int_add(idx_cur, i64_ty.const_int(1, false), "comp_idx_next")
            .map_err(|e| CompileError::LlvmError(format!("comp idx add: {e}")))?;
        self.generator.build_store(idx_storage, idx_next)?;
        self.generator.build_br(header_bb)?;

        // Done: build result list { count, result_data }.
        self.generator.builder.position_at_end(done_bb);
        let final_count = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                count_storage,
                "comp_final_count",
            )?
            .into_int_value();
        let result_ptr = self.generator.build_list_struct(final_count, result_data)?;
        self.generator.build_load(
            BasicTypeEnum::StructType(list_ty),
            result_ptr.into_pointer_value(),
            "comp_result",
        )
    }

    /// 0.32.31: Slice expression (xs[start:end]).
    ///
    /// View semantics: no data copy. Builds a new `{i64 len, ptr data}` struct
    /// pointing into the existing list's data buffer at an offset.
    /// Indices are clamped to [0, list_len] to prevent OOB pointer arithmetic.
    fn emit_slice(
        &mut self,
        target: &ResolvedExpr,
        start: Option<&ResolvedExpr>,
        end: Option<&ResolvedExpr>,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i64_ty = self.generator.context.i64_type();
        let list_ty = self.generator.list_struct_type();
        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());

        // Emit target → list struct value, then alloca+store to get a pointer.
        let target_val = self.emit_expr(target, frame)?;
        let target_alloca = self
            .generator
            .build_alloca(BasicTypeEnum::StructType(list_ty), "slice_target")?;
        self.generator.build_store(target_alloca, target_val)?;
        let target_ptr = target_alloca;

        // Load list length (field 0) and data pointer (field 1).
        let len_gep = self
            .generator
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(list_ty),
                target_ptr,
                0,
                "slice_len_ptr",
            )
            .map_err(|e| CompileError::LlvmError(format!("slice gep len: {e}")))?;
        let list_len = self
            .generator
            .build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "slice_len")?
            .into_int_value();
        let data_gep = self
            .generator
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(list_ty),
                target_ptr,
                1,
                "slice_data_ptr",
            )
            .map_err(|e| CompileError::LlvmError(format!("slice gep data: {e}")))?;
        let data_ptr = self
            .generator
            .build_load(BasicTypeEnum::PointerType(ptr_ty), data_gep, "slice_data")?
            .into_pointer_value();

        // 0.34.36 (audit wave-2 #6 / slice 4th-axis): this emitter previously
        // CLAMPED out-of-bounds indices to [0, len] and ALIASED the source
        // data buffer (`new_data = data + offset`). Both diverge from the VM
        // reference (`builtin_slice`, interp/bytecode/builtins/list.rs):
        //   - negative indices WRAP Python-style: (len + idx).max(0)
        //   - start/end > len and start > end TRAP (E0814 slice error)
        //   - the result COPIES the range into a fresh buffer (l[start..end]
        //     .to_vec()) — aliasing made scope-exit `free(data+offset)` free a
        //     non-malloc base → munmap_chunk invalid pointer / double free.
        let zero = i64_ty.const_int(0, false);
        let function = self.current_function()?;

        // Resolve start (default 0) and end (default len).
        let start_raw = match start {
            Some(expr) => {
                let v = self.emit_expr(expr, frame)?;
                self.coerce_to_i64(v)?
            }
            None => zero,
        };
        let end_raw = match end {
            Some(expr) => {
                let v = self.emit_expr(expr, frame)?;
                self.coerce_to_i64(v)?
            }
            None => list_len,
        };

        // Negative wrap (VM parity): idx < 0 → (len + idx).max(0).
        let wrap_idx = |b: &inkwell::builder::Builder<'ctx>,
                        idx: inkwell::values::IntValue<'ctx>,
                        len: inkwell::values::IntValue<'ctx>,
                        label: &str|
         -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
            let is_neg = b
                .build_int_compare(
                    inkwell::IntPredicate::SLT,
                    idx,
                    zero,
                    &format!("{label}_neg"),
                )
                .map_err(|e| CompileError::LlvmError(format!("slice cmp: {e}")))?;
            let wrapped = b
                .build_int_add(idx, len, &format!("{label}_wrap"))
                .map_err(|e| CompileError::LlvmError(format!("slice add: {e}")))?;
            let idx = b
                .build_select(is_neg, wrapped, idx, &format!("{label}_wrapped"))
                .map_err(|e| CompileError::LlvmError(format!("slice select: {e}")))?
                .into_int_value();
            // After wrap, clamp to 0 (max(0, len+idx) for very negative idx).
            let still_neg = b
                .build_int_compare(
                    inkwell::IntPredicate::SLT,
                    idx,
                    zero,
                    &format!("{label}_still_neg"),
                )
                .map_err(|e| CompileError::LlvmError(format!("slice cmp: {e}")))?;
            let idx = b
                .build_select(still_neg, zero, idx, &format!("{label}_max0"))
                .map_err(|e| CompileError::LlvmError(format!("slice select: {e}")))?
                .into_int_value();
            Ok(idx)
        };

        // Emit a trap block that aborts with a VM-shaped E0814 message and
        // branches to the ok block on the happy path.
        let emit_bounds_trap = |this: &Self,
                                cond: inkwell::values::IntValue<'ctx>,
                                bb_ok: inkwell::basic_block::BasicBlock<'ctx>,
                                msg: &str|
         -> Result<(), CompileError> {
            let bb_trap = this
                .generator
                .context
                .append_basic_block(function, "slice_oob_trap");
            this.generator.build_cond_br(cond, bb_trap, bb_ok)?;
            this.generator.builder.position_at_end(bb_trap);
            let abort_fn = this.generator.get_or_declare_abort_fn();
            let msg_global = this
                .generator
                .builder
                .build_global_string_ptr(msg, "slice_oob_msg")
                .map_err(|e| CompileError::LlvmError(format!("slice msg: {e}")))?;
            this.generator.build_call(
                abort_fn,
                &[inkwell::values::BasicMetadataValueEnum::PointerValue(
                    msg_global.as_pointer_value(),
                )],
                "slice_trap",
            )?;
            // 0.35.11-fix (dx-backlog #20 follow-up): the trap block must be
            // terminated. `mimi_runtime_abort` is declared `noreturn`, but a
            // block ending in a plain call has no terminator — LLVM verify
            // rejects the function and the whole main body silently demotes
            // to the legacy emitter (where list print dispatch breaks).
            this.generator
                .builder
                .build_unreachable()
                .map_err(|e| CompileError::LlvmError(format!("slice trap unreachable: {e}")))?;
            Ok(())
        };

        // start > len → trap (VM: "slice start out of bounds").
        let start_idx = wrap_idx(&self.generator.builder, start_raw, list_len, "start")?;
        let start_exceeds = self
            .generator
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SGT,
                start_idx,
                list_len,
                "start_exceeds",
            )
            .map_err(|e| CompileError::LlvmError(format!("slice cmp: {e}")))?;
        let bb_start_ok = self
            .generator
            .context
            .append_basic_block(function, "slice_start_ok");
        emit_bounds_trap(
            self,
            start_exceeds,
            bb_start_ok,
            "[E0814] slice start out of bounds",
        )?;
        self.generator.builder.position_at_end(bb_start_ok);

        // end > len → trap (VM: "slice end out of bounds").
        let end_idx = wrap_idx(&self.generator.builder, end_raw, list_len, "end")?;
        let end_exceeds = self
            .generator
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, end_idx, list_len, "end_exceeds")
            .map_err(|e| CompileError::LlvmError(format!("slice cmp: {e}")))?;
        let bb_end_ok = self
            .generator
            .context
            .append_basic_block(function, "slice_end_ok");
        emit_bounds_trap(
            self,
            end_exceeds,
            bb_end_ok,
            "[E0814] slice end out of bounds",
        )?;
        self.generator.builder.position_at_end(bb_end_ok);

        // start > end → trap (VM: "slice start > end").
        let start_gt_end = self
            .generator
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SGT,
                start_idx,
                end_idx,
                "slice_start_gt_end",
            )
            .map_err(|e| CompileError::LlvmError(format!("slice cmp: {e}")))?;
        let bb_range_ok = self
            .generator
            .context
            .append_basic_block(function, "slice_range_ok");
        emit_bounds_trap(self, start_gt_end, bb_range_ok, "[E0814] slice start > end")?;
        self.generator.builder.position_at_end(bb_range_ok);

        // new_len = end - start (>= 0 by the traps above).
        let new_len = self
            .generator
            .builder
            .build_int_sub(end_idx, start_idx, "slice_new_len")
            .map_err(|e| CompileError::LlvmError(format!("slice sub: {e}")))?;

        // Copy path: fresh malloc + memcpy (VM `.to_vec()` parity — the
        // result OWNS its buffer; no aliasing, no free(non-malloc-base)).
        let elem_size = i64_ty.const_int(8, false);
        let is_empty = self
            .generator
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, new_len, zero, "slice_empty_cmp")
            .map_err(|e| CompileError::LlvmError(format!("slice cmp: {e}")))?;
        let bb_empty = self
            .generator
            .context
            .append_basic_block(function, "slice_empty_bb");
        let bb_copy = self
            .generator
            .context
            .append_basic_block(function, "slice_copy_bb");
        let bb_merge = self
            .generator
            .context
            .append_basic_block(function, "slice_merge_bb");
        self.generator.build_cond_br(is_empty, bb_empty, bb_copy)?;

        // Empty: null data (VM returns Vec::new() → null data, len 0).
        self.generator.builder.position_at_end(bb_empty);
        let null_data = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default())
            .const_null();
        let empty_list = self.generator.build_list_struct(new_len, null_data)?;
        self.generator
            .builder
            .build_unconditional_branch(bb_merge)
            .map_err(|e| CompileError::LlvmError(format!("slice br: {e}")))?;
        let bb_empty_end =
            self.generator.builder.get_insert_block().ok_or_else(|| {
                CompileError::LlvmError("no insert block after empty slice".into())
            })?;

        // Copy: malloc `new_len * 8` bytes, memcpy from data + start*8.
        self.generator.builder.position_at_end(bb_copy);
        let bytes = self
            .generator
            .builder
            .build_int_mul(new_len, elem_size, "slice_bytes")
            .map_err(|e| CompileError::LlvmError(format!("slice mul: {e}")))?;
        let dest = self.generator.malloc_or_abort(bytes, "slice_data")?;
        let byte_offset = self
            .generator
            .builder
            .build_int_mul(start_idx, elem_size, "slice_byte_offset")
            .map_err(|e| CompileError::LlvmError(format!("slice mul: {e}")))?;
        let data_i8 = self
            .generator
            .builder
            .build_pointer_cast(data_ptr, ptr_ty, "data_as_i8")
            .map_err(|e| CompileError::LlvmError(format!("slice cast: {e}")))?;
        let src_i8 = self.generator.build_in_bounds_gep(
            self.generator.context.i8_type(),
            data_i8,
            &[byte_offset],
            "slice_src",
        )?;
        let memcpy_fn = self.generator.get_runtime_fn("memcpy")?;
        self.generator.build_call(
            memcpy_fn,
            &[
                inkwell::values::BasicMetadataValueEnum::PointerValue(dest),
                inkwell::values::BasicMetadataValueEnum::PointerValue(src_i8),
                inkwell::values::BasicMetadataValueEnum::IntValue(bytes),
            ],
            "slice_memcpy",
        )?;
        let copy_list = self.generator.build_list_struct(new_len, dest)?;
        self.generator
            .builder
            .build_unconditional_branch(bb_merge)
            .map_err(|e| CompileError::LlvmError(format!("slice br: {e}")))?;
        let bb_copy_end =
            self.generator.builder.get_insert_block().ok_or_else(|| {
                CompileError::LlvmError("no insert block after slice copy".into())
            })?;

        self.generator.builder.position_at_end(bb_merge);
        let phi = self
            .generator
            .builder
            .build_phi(empty_list.get_type(), "slice_result_phi")
            .map_err(|e| CompileError::LlvmError(format!("slice phi: {e}")))?;
        phi.add_incoming(&[
            (
                &empty_list as &dyn inkwell::values::BasicValue,
                bb_empty_end,
            ),
            (&copy_list as &dyn inkwell::values::BasicValue, bb_copy_end),
        ]);
        return self.generator.build_load(
            BasicTypeEnum::StructType(list_ty),
            phi.as_basic_value().into_pointer_value(),
            "slice_result",
        );
    }

    ///   Result<T, E> → {i1 disc, T ok_val, i64 err_val}
    ///   Option<T>    → {i1 disc, T payload}
    ///
    /// On Ok/Some (disc != 0): extract field 1 → expression value.
    /// On Err/None (disc == 0): call mimi_try_exit(err_val) → unreachable.
    fn emit_try(
        &mut self,
        value: &ResolvedExpr,
        result_ty: &ResolvedTypeId,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i64_ty = self.generator.context.i64_type();

        // Determine whether the inner type is a built-in Result/Option or a
        // custom Ok/Err enum (Res-style).
        let inner_kind = match self.program.resolved_types().get(&value.ty) {
            Some(ResolvedType::Result { .. }) => TryInnerKind::ResolvedBuiltinResult,
            Some(ResolvedType::Option(_)) => TryInnerKind::ResolvedBuiltinOption,
            Some(ResolvedType::Nominal { .. })
                if self.custom_try_enum_error_ordinal(&value.ty).is_ok() =>
            {
                TryInnerKind::CustomEnum
            }
            _ => {
                return Err(CompileError::Unsupported(
                    "try inner type is not Result, Option, or Ok/Err enum".into(),
                ))
            }
        };

        // Emit the inner expression → struct value.
        let inner_val = self.emit_expr(value, frame)?;
        let sv = match inner_val {
            BasicValueEnum::StructValue(sv) => sv,
            BasicValueEnum::PointerValue(pv) => {
                // Load through pointer if needed.
                let llvm_ty = self.lower_type(&value.ty)?;
                self.generator
                    .build_load(llvm_ty, pv, "try_ptr_load")?
                    .into_struct_value()
            }
            _ => {
                return Err(CompileError::Unsupported(
                    "try inner value is not a struct (Result/Option/enum)".into(),
                ))
            }
        };

        // Extract discriminant (field 0).
        let disc = self
            .generator
            .builder
            .build_extract_value(sv, 0, "try_disc")
            .map_err(|e| CompileError::LlvmError(format!("try disc extract: {e}")))?
            .into_int_value();

        // Extract payload (field 1) — the Ok/Some value.
        let payload = self
            .generator
            .builder
            .build_extract_value(sv, 1, "try_payload")
            .map_err(|e| CompileError::LlvmError(format!("try payload extract: {e}")))?;

        let (err_disc, is_custom) = match inner_kind {
            TryInnerKind::ResolvedBuiltinResult => (0u32, false),
            TryInnerKind::ResolvedBuiltinOption => (0u32, false),
            TryInnerKind::CustomEnum => {
                let ordinal = self.custom_try_enum_error_ordinal(&value.ty)?;
                (ordinal, true)
            }
        };
        let err_disc_val = disc.get_type().const_int(err_disc as u64, false);
        let is_err = self
            .generator
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, disc, err_disc_val, "try_is_err")
            .map_err(|e| CompileError::LlvmError(format!("try compare: {e}")))?;

        // Branch: err → err_bb, else → ok_bb.
        let function = self.current_function()?;
        let ok_bb = self
            .generator
            .context
            .append_basic_block(function, "try_ok");
        let err_bb = self
            .generator
            .context
            .append_basic_block(function, "try_err");

        self.generator.build_cond_br(is_err, err_bb, ok_bb)?;

        // ── Err path ──
        self.generator.builder.position_at_end(err_bb);
        if is_custom {
            // Custom Ok/Err enums follow the real `?` semantics: propagate
            // the Err variant as the current function's return value.
            if !self.defer_scopes.is_empty() || !self.comp_scopes.is_empty() {
                return Err(CompileError::Unsupported(
                    "custom enum try with active defer/on-failure scopes".into(),
                ));
            }
            let enum_ty = self.lower_type(&value.ty)?;
            let struct_ty = enum_ty.into_struct_type();
            let mut result = struct_ty.get_undef();
            result = self
                .generator
                .builder
                .build_insert_value(
                    result,
                    disc.get_type().const_int(err_disc as u64, false),
                    0,
                    "try_err_tag",
                )
                .map_err(|e| CompileError::LlvmError(format!("try err tag insert: {e}")))?
                .into_struct_value();
            result = self
                .generator
                .builder
                .build_insert_value(result, payload, 1, "try_err_payload")
                .map_err(|e| CompileError::LlvmError(format!("try err payload insert: {e}")))?
                .into_struct_value();
            let ret_val: BasicValueEnum<'ctx> = result.into();
            self.generator.build_return(Some(&ret_val))?;
        } else {
            // 0.1.7 audit P0-2: built-in Result/Option propagates when the
            // enclosing function returns the same built-in type, matching the
            // VM's return-early semantics.
            let same_return_type = self
                .program
                .callable(&frame.owner)
                .map(|c| c.signature.result == value.ty)
                .unwrap_or(false);
            if same_return_type {
                let ret_val: BasicValueEnum<'ctx> = BasicValueEnum::StructValue(sv);
                self.generator.build_return(Some(&ret_val))?;
            } else {
                // Built-in Result/Option: runtime exit on Err/None
                // (legacy-compatible codegen path).
                let try_exit_fn = self.generator.get_runtime_fn("mimi_try_exit")?;
                // For Result, the error slot is field 2; Option has no error slot.
                let err_val = if matches!(inner_kind, TryInnerKind::ResolvedBuiltinResult) {
                    self.generator
                        .builder
                        .build_extract_value(sv, 2, "try_err_val")
                        .map_err(|e| CompileError::LlvmError(format!("try err extract: {e}")))?
                } else {
                    BasicValueEnum::IntValue(i64_ty.const_zero())
                };
                let err_int = match err_val {
                    BasicValueEnum::IntValue(iv) => {
                        // Ensure i64.
                        if iv.get_type().get_bit_width() < 64 {
                            self.generator
                                .builder
                                .build_int_z_extend(iv, i64_ty, "try_err_zext")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("try err zext: {e}"))
                                })?
                        } else {
                            iv
                        }
                    }
                    _ => i64_ty.const_zero(),
                };
                self.generator
                    .builder
                    .build_call(
                        try_exit_fn,
                        &[inkwell::values::BasicMetadataValueEnum::IntValue(err_int)],
                        "try_exit_call",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("try_exit call: {e}")))?;
                // mimi_try_exit is noreturn — emit unreachable.
                self.generator
                    .builder
                    .build_unreachable()
                    .map_err(|e| CompileError::LlvmError(format!("try unreachable: {e}")))?;
            }
        }

        // ── Ok path: position at ok_bb, recover payload. ──
        self.generator.builder.position_at_end(ok_bb);
        let target_llvm_ty = self.lower_type(result_ty)?;
        if is_custom {
            self.coerce_from_i64(payload.into_int_value(), target_llvm_ty)
        } else {
            self.coerce_to(payload, target_llvm_ty)
        }
    }

    /// Return the sorted variant ordinal of the `Err` variant in a custom
    /// two-variant `Ok`/`Err` enum.
    fn custom_try_enum_error_ordinal(&self, id: &ResolvedTypeId) -> Result<u32, CompileError> {
        let ResolvedType::Nominal { item, .. } = self
            .program
            .resolved_types()
            .get(id)
            .ok_or_else(|| CompileError::Unsupported("custom try: missing type".into()))?
        else {
            return Err(CompileError::Unsupported(
                "custom try type is not Nominal".into(),
            ));
        };
        let item_str = item.as_str();
        let type_name = item_str.strip_prefix("type:").unwrap_or(item_str);
        let td = self
            .program
            .type_defs()
            .values()
            .find(|td| {
                (td.qualified_name == type_name || td.qualified_name == item_str)
                    && matches!(td.kind, crate::core::resolved::ResolvedTypeKind::Enum)
            })
            .ok_or_else(|| {
                CompileError::Unsupported(format!("custom try enum '{type_name}' not found"))
            })?;
        let mut variant_names: Vec<&str> =
            td.variants.iter().map(|(name, _)| name.as_str()).collect();
        variant_names.sort();
        variant_names
            .iter()
            .position(|name| *name == "Err")
            .map(|index| index as u32)
            .ok_or_else(|| {
                CompileError::Unsupported(format!(
                    "custom try enum '{type_name}' has no Err variant"
                ))
            })
    }

    /// Recover a value encoded as i64 (the custom enum payload slot) into a
    /// concrete LLVM type. Mirrors the inverse of `coerce_to_i64`.
    fn coerce_from_i64(
        &self,
        payload: inkwell::values::IntValue<'ctx>,
        target: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match target {
            BasicTypeEnum::IntType(int_ty) => {
                let bw = int_ty.get_bit_width();
                if bw < 64 {
                    self.generator
                        .builder
                        .build_int_truncate(payload, int_ty, "try_payload_trunc")
                        .map(|v| v.into())
                        .map_err(|e| CompileError::LlvmError(format!("try trunc: {e}")))
                } else if bw == 64 {
                    Ok(BasicValueEnum::IntValue(payload))
                } else {
                    self.generator
                        .builder
                        .build_int_z_extend(payload, int_ty, "try_payload_zext")
                        .map(|v| v.into())
                        .map_err(|e| CompileError::LlvmError(format!("try zext: {e}")))
                }
            }
            BasicTypeEnum::FloatType(float_ty) => self
                .generator
                .builder
                .build_bit_cast(payload, float_ty, "try_payload_float_bits")
                .map(|v| v.into())
                .map_err(|e| CompileError::LlvmError(format!("try float bitcast: {e}"))),
            BasicTypeEnum::PointerType(ptr_ty) => self
                .generator
                .builder
                .build_int_to_ptr(payload, ptr_ty, "try_payload_ptr")
                .map(|v| v.into())
                .map_err(|e| CompileError::LlvmError(format!("try inttoptr: {e}"))),
            BasicTypeEnum::StructType(sty) => {
                // Struct payloads are stored by pointer in the i64 slot
                // (strings/records are heap-boxed by the enum ctor).
                let ptr_ty = self
                    .generator
                    .context
                    .ptr_type(inkwell::AddressSpace::default());
                let box_ptr = self
                    .generator
                    .builder
                    .build_int_to_ptr(payload, ptr_ty, "try_payload_box")
                    .map_err(|e| CompileError::LlvmError(format!("try inttoptr box: {e}")))?;
                self.generator.build_load(
                    BasicTypeEnum::StructType(sty),
                    box_ptr,
                    "try_payload_box_load",
                )
            }
            _ => Err(CompileError::Unsupported(
                "custom try payload type cannot be recovered from i64 slot".into(),
            )),
        }
    }

    /// Emit an actor method call through the mailbox runtime. The resolved
    /// slice treats actor handles as opaque pointers; the first call argument
    /// is the implicit `self` handle, followed by the user-facing arguments.
    /// The remaining arguments are packed into the same fixed-size `i8`
    /// blob used by the legacy actor call site.
    fn emit_actor_method_call(
        &mut self,
        _call: &ResolvedCall,
        arguments: &[BasicMetadataValueEnum<'ctx>],
        actor: &NodeId,
        method: &MethodId,
        _expression: &ResolvedExpr,
        _frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i8_ty = self.generator.context.i8_type();
        let i32_ty = self.generator.context.i32_type();
        let i64_ty = self.generator.context.i64_type();
        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());

        // Resolve the actor name / method name.
        let actor_type = actor
            .0
            .strip_prefix("actor:")
            .ok_or_else(|| CompileError::Unsupported(format!("invalid actor id '{}'", actor.0)))?;
        let method_name = method.as_str().rsplit("::").next().ok_or_else(|| {
            CompileError::Unsupported(format!("invalid method id '{}'", method.as_str()))
        })?;
        let method_key = format!("{actor_type}::{method_name}");
        let method_id = *self
            .generator
            .actor_method_ids
            .get(&method_key)
            .ok_or_else(|| {
                CompileError::Unsupported(format!("unknown actor method '{method_key}'"))
            })?;
        let actor_def = self.generator.actor_defs.get(actor_type).ok_or_else(|| {
            CompileError::Unsupported(format!("unknown actor type '{actor_type}'"))
        })?;
        let method_def = actor_def
            .methods
            .iter()
            .find(|m| m.name == method_name)
            .ok_or_else(|| {
                CompileError::Unsupported(format!(
                    "actor '{actor_type}' has no method '{method_name}'"
                ))
            })?;
        let method_params: Vec<crate::ast::Type> =
            method_def.params.iter().map(|p| p.ty.clone()).collect();
        let method_ret = method_def.ret.clone();

        // The first argument is the actor handle. It must be a pointer.
        let Some(BasicMetadataValueEnum::PointerValue(handle_ptr)) = arguments.first() else {
            return Err(CompileError::Unsupported(
                "actor method call: first argument is not an actor handle pointer".into(),
            ));
        };

        // Arguments blob + result blob.
        let blob_array = i8_ty.array_type(RESOLVED_ACTOR_BLOB_CAPACITY as u32);
        let args_blob = self.generator.build_alloca(blob_array, "actor_args_blob")?;
        let result_blob = self
            .generator
            .build_alloca(blob_array, "actor_result_blob")?;

        let mut blob_offset: u64 = 0;
        for (slot_index, arg) in arguments.iter().enumerate().skip(1) {
            let arg_basic: BasicValueEnum<'ctx> = match *arg {
                BasicMetadataValueEnum::IntValue(iv) => iv.into(),
                BasicMetadataValueEnum::FloatValue(fv) => fv.into(),
                BasicMetadataValueEnum::PointerValue(pv) => pv.into(),
                BasicMetadataValueEnum::StructValue(sv) => sv.into(),
                _ => {
                    return Err(CompileError::Unsupported(
                        "actor method argument has unsupported LLVM metadata kind".into(),
                    ))
                }
            };
            let param_ty = method_params
                .get(slot_index - 1)
                .map(|t| self.generator.actor_abi_type_for(t));
            let store_ty = param_ty.unwrap_or_else(|| arg_basic.get_type());
            let slot_size = self.generator.actor_abi_slot_size(store_ty);
            let offset = i64_ty.const_int(blob_offset, false);
            let gep =
                self.generator
                    .build_in_bounds_gep(i8_ty, args_blob, &[offset], "actor_arg_gep")?;
            let cast_ptr = self
                .generator
                .builder
                .build_bit_cast(gep, ptr_ty, &format!("actor_arg_cast_{}", slot_index))
                .map_err(|e| CompileError::LlvmError(format!("actor arg bitcast: {e}")))?
                .into_pointer_value();

            match (arg_basic, store_ty) {
                (BasicValueEnum::IntValue(iv), BasicTypeEnum::IntType(t)) => {
                    let stored = if iv.get_type().get_bit_width() < t.get_bit_width() {
                        if iv.get_type().get_bit_width() == 1 {
                            self.generator
                                .builder
                                .build_int_z_extend(
                                    iv,
                                    t,
                                    &format!("actor_arg_zext_{}", slot_index),
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("actor arg zext: {e}"))
                                })?
                        } else {
                            self.generator
                                .builder
                                .build_int_s_extend(
                                    iv,
                                    t,
                                    &format!("actor_arg_sext_{}", slot_index),
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("actor arg sext: {e}"))
                                })?
                        }
                    } else if iv.get_type().get_bit_width() > t.get_bit_width() {
                        self.generator
                            .builder
                            .build_int_truncate(iv, t, &format!("actor_arg_trunc_{}", slot_index))
                            .map_err(|e| CompileError::LlvmError(format!("actor arg trunc: {e}")))?
                    } else {
                        iv
                    };
                    self.generator.build_store(cast_ptr, stored)?;
                }
                (BasicValueEnum::FloatValue(fv), BasicTypeEnum::FloatType(_)) => {
                    self.generator.build_store(cast_ptr, fv)?;
                }
                (BasicValueEnum::PointerValue(pv), BasicTypeEnum::PointerType(_)) => {
                    self.generator.build_store(cast_ptr, pv)?;
                }
                (BasicValueEnum::StructValue(sv), BasicTypeEnum::StructType(_st)) => {
                    self.generator.build_store(cast_ptr, sv)?;
                }
                (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(st)) => {
                    let fields = st.get_field_types();
                    let is_string_shape = fields.len() == 2
                        && matches!(fields[0], BasicTypeEnum::PointerType(_))
                        && matches!(fields[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64);
                    if is_string_shape {
                        let wrapped = self.generator.wrap_c_string(pv)?;
                        self.generator.build_store(cast_ptr, wrapped)?;
                    } else {
                        return Err(CompileError::Unsupported(
                            "actor method pointer-to-struct argument is not yet supported".into(),
                        ));
                    }
                }
                _ => {
                    return Err(CompileError::Unsupported(format!(
                        "actor method argument {slot_index} cannot be packed ({store_ty:?})"
                    )));
                }
            }
            blob_offset += slot_size;
        }

        let args_size = i64_ty.const_int(blob_offset, false);
        let args_blob_i8ptr = self
            .generator
            .builder
            .build_bit_cast(args_blob, ptr_ty, "actor_args_blob_i8")
            .map_err(|e| CompileError::LlvmError(format!("actor args bitcast: {e}")))?
            .into_pointer_value();
        let result_blob_i8ptr = self
            .generator
            .builder
            .build_bit_cast(result_blob, ptr_ty, "actor_result_blob_i8")
            .map_err(|e| CompileError::LlvmError(format!("actor result bitcast: {e}")))?
            .into_pointer_value();

        let call_fn = self.generator.get_runtime_fn("mimi_actor_call")?;
        self.generator
            .builder
            .build_call(
                call_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(*handle_ptr),
                    BasicMetadataValueEnum::IntValue(i32_ty.const_int(method_id as u64, false)),
                    BasicMetadataValueEnum::PointerValue(args_blob_i8ptr),
                    BasicMetadataValueEnum::IntValue(args_size),
                    BasicMetadataValueEnum::PointerValue(result_blob_i8ptr),
                ],
                "actor_call_result",
            )
            .map_err(|e| CompileError::LlvmError(format!("mimi_actor_call: {e}")))?;

        let result_cast = self
            .generator
            .builder
            .build_bit_cast(result_blob, ptr_ty, "actor_result_ptr")
            .map_err(|e| CompileError::LlvmError(format!("actor result bitcast: {e}")))?
            .into_pointer_value();
        let result_ty = match &method_ret {
            Some(ty) => self.generator.actor_abi_type_for(ty),
            None => BasicTypeEnum::IntType(i64_ty),
        };
        self.generator
            .build_load(result_ty, result_cast, "actor_method_result")
    }

    /// Emit a runtime loop that frees each cloned string element inside a
    /// `List<string>` worker argument after the worker call has completed.
    /// The list data buffer itself is freed separately by the caller.
    fn emit_spawn_string_list_element_free(
        &mut self,
        len: inkwell::values::IntValue<'ctx>,
        data: inkwell::values::PointerValue<'ctx>,
    ) -> Result<(), CompileError> {
        let i64_ty = self.generator.context.i64_type();
        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());
        let function = self.current_function()?;
        let header = self
            .generator
            .context
            .append_basic_block(function, "spawn_str_list_free_header");
        let body = self
            .generator
            .context
            .append_basic_block(function, "spawn_str_list_free_body");
        let exit = self
            .generator
            .context
            .append_basic_block(function, "spawn_str_list_free_exit");
        let idx_storage = self
            .generator
            .build_alloca(BasicTypeEnum::IntType(i64_ty), "spawn_str_list_idx")?;
        self.generator
            .build_store(idx_storage, i64_ty.const_int(0, false))?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(header);
        let idx = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                idx_storage,
                "spawn_str_list_idx_val",
            )?
            .into_int_value();
        let cond = self
            .generator
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, idx, len, "spawn_str_list_cond")
            .map_err(|e| CompileError::LlvmError(format!("spawn string list cmp: {e}")))?;
        self.generator.build_cond_br(cond, body, exit)?;

        self.generator.builder.position_at_end(body);
        let elem_slot =
            self.generator
                .build_in_bounds_gep(i64_ty, data, &[idx], "spawn_str_list_elem_slot")?;
        let elem_i64 = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                elem_slot,
                "spawn_str_list_elem_i64",
            )?
            .into_int_value();
        // Each `List<string>` slot is a fat `MimiStr` box handle (an i64), not a
        // bare C-string pointer. Freeing it with `mimi_string_free` (which frees
        // the 16-byte box as a single byte buffer) leaked the boxed string's
        // inner byte allocation and is otherwise wrong. Use `mimi_str_free_box`,
        // which frees both the inner bytes and the box.
        let free_box = self.generator.get_runtime_fn("mimi_str_free_box")?;
        self.generator
            .builder
            .build_call(
                free_box,
                &[BasicMetadataValueEnum::IntValue(elem_i64)],
                "spawn_str_list_elem_free",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn string elem free: {e}")))?;
        let next = self
            .generator
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "spawn_str_list_idx_next")
            .map_err(|e| CompileError::LlvmError(format!("spawn string list inc: {e}")))?;
        self.generator.build_store(idx_storage, next)?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(exit);
        Ok(())
    }

    /// Emit a runtime loop that deep-copies each string element of a source
    /// `List<string>` into a fresh `List<string>` data buffer. The copy is
    /// stored into `dst_data` as i64 pointer handles for the worker env.
    fn emit_spawn_string_list_clone(
        &mut self,
        len: inkwell::values::IntValue<'ctx>,
        src_data: inkwell::values::PointerValue<'ctx>,
        dst_data: inkwell::values::PointerValue<'ctx>,
    ) -> Result<(), CompileError> {
        let i64_ty = self.generator.context.i64_type();
        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());
        let function = self.current_function()?;
        let header = self
            .generator
            .context
            .append_basic_block(function, "spawn_str_list_clone_header");
        let body = self
            .generator
            .context
            .append_basic_block(function, "spawn_str_list_clone_body");
        let exit = self
            .generator
            .context
            .append_basic_block(function, "spawn_str_list_clone_exit");
        let idx_storage = self
            .generator
            .build_alloca(BasicTypeEnum::IntType(i64_ty), "spawn_str_list_clone_idx")?;
        self.generator
            .build_store(idx_storage, i64_ty.const_int(0, false))?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(header);
        let idx = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                idx_storage,
                "spawn_str_list_clone_idx_val",
            )?
            .into_int_value();
        let cond = self
            .generator
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                idx,
                len,
                "spawn_str_list_clone_cond",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn string list clone cmp: {e}")))?;
        self.generator.build_cond_br(cond, body, exit)?;

        self.generator.builder.position_at_end(body);
        let src_slot = self.generator.build_in_bounds_gep(
            i64_ty,
            src_data,
            &[idx],
            "spawn_str_list_clone_src_slot",
        )?;
        let src_i64 = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                src_slot,
                "spawn_str_list_clone_src_i64",
            )?
            .into_int_value();
        // Since 0.1.8 every `List<string>` slot is a fat `MimiStr` box handle
        // (an i64 that points at `{magic, _pad, char* ptr, i64 len}`), NOT a
        // raw C-string pointer. The old code inttoptr'd the box to a `char*` and
        // ran `strlen` + `mimi_str_clone` on it, so it read the magic bytes as
        // "string data" and returned garbage (BUG C). Unbox the handle into
        // `(ptr, len)` and deep-copy the bytes into a fresh owned box so the
        // worker thread owns its own copy of each string element.
        let out_ptr_slot = self
            .generator
            .build_alloca(BasicTypeEnum::PointerType(ptr_ty), "spawn_str_list_out_ptr")?;
        let out_len_slot = self
            .generator
            .build_alloca(BasicTypeEnum::IntType(i64_ty), "spawn_str_list_out_len")?;
        let unbox_fn = self.generator.get_runtime_fn("mimi_str_unbox")?;
        self.generator
            .builder
            .build_call(
                unbox_fn,
                &[
                    BasicMetadataValueEnum::IntValue(src_i64),
                    BasicMetadataValueEnum::PointerValue(out_ptr_slot),
                    BasicMetadataValueEnum::PointerValue(out_len_slot),
                ],
                "spawn_str_list_clone_unbox",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn string list unbox: {e}")))?;
        let inner_ptr = self
            .generator
            .build_load(
                BasicTypeEnum::PointerType(ptr_ty),
                out_ptr_slot,
                "spawn_str_list_clone_inner_ptr",
            )?
            .into_pointer_value();
        let inner_len = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                out_len_slot,
                "spawn_str_list_clone_inner_len",
            )?
            .into_int_value();
        let box_copy_fn = self.generator.get_runtime_fn("mimi_str_box_copy")?;
        let clone_handle = self
            .generator
            .builder
            .build_call(
                box_copy_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(inner_ptr),
                    BasicMetadataValueEnum::IntValue(inner_len),
                ],
                "spawn_str_list_clone",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn string list clone: {e}")))?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("spawn string list clone returned void".into()))?
            .into_int_value();
        let dst_slot = self.generator.build_in_bounds_gep(
            i64_ty,
            dst_data,
            &[idx],
            "spawn_str_list_clone_dst_slot",
        )?;
        self.generator.build_store(dst_slot, clone_handle)?;
        let next = self
            .generator
            .builder
            .build_int_add(
                idx,
                i64_ty.const_int(1, false),
                "spawn_str_list_clone_idx_next",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn string list clone inc: {e}")))?;
        self.generator.build_store(idx_storage, next)?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(exit);
        Ok(())
    }

    /// Emit a runtime loop that deep-copies a `List<List<i32>>`/`List<List<f64>>`
    /// worker argument into a fresh outer data array. Each inner list is
    /// cloned element-by-element into a new inner data buffer and heap-boxed,
    /// so the worker env owns every level of the nested container.
    fn emit_spawn_nested_list_clone(
        &mut self,
        outer_len: inkwell::values::IntValue<'ctx>,
        src_outer: inkwell::values::PointerValue<'ctx>,
        dst_outer: inkwell::values::PointerValue<'ctx>,
        inner_elem_size: u64,
        inner_is_string: bool,
    ) -> Result<(), CompileError> {
        let i64_ty = self.generator.context.i64_type();
        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.generator.list_struct_type();
        let function = self.current_function()?;
        let header = self
            .generator
            .context
            .append_basic_block(function, "spawn_nested_clone_header");
        let body = self
            .generator
            .context
            .append_basic_block(function, "spawn_nested_clone_body");
        let exit = self
            .generator
            .context
            .append_basic_block(function, "spawn_nested_clone_exit");
        let idx_storage = self
            .generator
            .build_alloca(BasicTypeEnum::IntType(i64_ty), "spawn_nested_clone_idx")?;
        self.generator
            .build_store(idx_storage, i64_ty.const_int(0, false))?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(header);
        let idx = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                idx_storage,
                "spawn_nested_clone_idx_val",
            )?
            .into_int_value();
        let cond = self
            .generator
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                idx,
                outer_len,
                "spawn_nested_clone_cond",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn nested clone cmp: {e}")))?;
        self.generator.build_cond_br(cond, body, exit)?;

        self.generator.builder.position_at_end(body);
        let src_slot = self.generator.build_in_bounds_gep(
            i64_ty,
            src_outer,
            &[idx],
            "spawn_nested_clone_src_slot",
        )?;
        let src_inner_i64 = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                src_slot,
                "spawn_nested_clone_src_inner_i64",
            )?
            .into_int_value();
        let src_inner_ptr = self
            .generator
            .builder
            .build_int_to_ptr(src_inner_i64, ptr_ty, "spawn_nested_clone_src_inner_ptr")
            .map_err(|e| CompileError::LlvmError(format!("spawn nested inner ptr: {e}")))?;
        let inner_val = self
            .generator
            .build_load(
                BasicTypeEnum::StructType(list_ty),
                src_inner_ptr,
                "spawn_nested_clone_inner_val",
            )?
            .into_struct_value();
        let inner_len = self
            .generator
            .build_extract_value(inner_val.into(), 0, "spawn_nested_clone_inner_len")?
            .into_int_value();
        let inner_data = self
            .generator
            .build_extract_value(inner_val.into(), 1, "spawn_nested_clone_inner_data")?
            .into_pointer_value();
        let inner_clone_data = if inner_is_string {
            let inner_bytes = self
                .generator
                .builder
                .build_int_mul(
                    inner_len,
                    i64_ty.const_int(8, false),
                    "spawn_nested_clone_inner_bytes",
                )
                .map_err(|e| CompileError::LlvmError(format!("spawn nested inner size: {e}")))?;
            let inner_clone_data = self
                .generator
                .malloc_or_abort(inner_bytes, "spawn_nested_clone_inner_data")?;
            self.emit_spawn_string_list_clone(inner_len, inner_data, inner_clone_data)?;
            inner_clone_data
        } else {
            // Each inner-list element is stored as an i64 (8-byte stride) in the
            // data buffer, exactly like the top-level scalar-list clone path (FIX
            // 1): `emit_list_literal` allocates `count*8` bytes and coerces every
            // element to i64. `inner_elem_size` is the *semantic* width (e.g. 4
            // for i32, 1 for bool), which would under-copy the buffer and leave
            // the worker reading garbage / OOB. The storage stride is always 8.
            let inner_size = i64_ty.const_int(8, false);
            let inner_bytes = self
                .generator
                .builder
                .build_int_mul(inner_len, inner_size, "spawn_nested_clone_inner_bytes")
                .map_err(|e| CompileError::LlvmError(format!("spawn nested inner size: {e}")))?;
            let inner_clone_data = self
                .generator
                .malloc_or_abort(inner_bytes, "spawn_nested_clone_inner_data")?;
            let memcpy_fn = self.generator.get_runtime_fn("memcpy")?;
            self.generator
                .builder
                .build_call(
                    memcpy_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(inner_clone_data),
                        BasicMetadataValueEnum::PointerValue(inner_data),
                        BasicMetadataValueEnum::IntValue(inner_bytes),
                    ],
                    "spawn_nested_clone_inner_copy",
                )
                .map_err(|e| CompileError::LlvmError(format!("spawn nested inner copy: {e}")))?;
            inner_clone_data
        };
        let new_inner = list_ty.get_undef();
        let new_inner = self
            .generator
            .builder
            .build_insert_value(new_inner, inner_len, 0, "spawn_nested_clone_new_len")
            .map_err(|e| CompileError::LlvmError(format!("spawn nested inner len: {e}")))?
            .into_struct_value();
        let new_inner = self
            .generator
            .builder
            .build_insert_value(
                new_inner,
                inner_clone_data,
                1,
                "spawn_nested_clone_new_data",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn nested inner data: {e}")))?
            .into_struct_value();
        let box_size = self
            .generator
            .llvm_type_size_bytes(BasicTypeEnum::StructType(list_ty));
        let box_size_val = i64_ty.const_int(box_size, false);
        let new_box = self
            .generator
            .malloc_or_abort(box_size_val, "spawn_nested_clone_box")?;
        self.generator
            .builder
            .build_store(new_box, new_inner)
            .map_err(|e| CompileError::LlvmError(format!("spawn nested box store: {e}")))?;
        let new_box_i64 = self
            .generator
            .builder
            .build_ptr_to_int(new_box, i64_ty, "spawn_nested_clone_box_i64")
            .map_err(|e| CompileError::LlvmError(format!("spawn nested box int: {e}")))?;
        let dst_slot = self.generator.build_in_bounds_gep(
            i64_ty,
            dst_outer,
            &[idx],
            "spawn_nested_clone_dst_slot",
        )?;
        self.generator.build_store(dst_slot, new_box_i64)?;
        let next = self
            .generator
            .builder
            .build_int_add(
                idx,
                i64_ty.const_int(1, false),
                "spawn_nested_clone_idx_next",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn nested clone inc: {e}")))?;
        self.generator.build_store(idx_storage, next)?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(exit);
        Ok(())
    }

    /// Emit a runtime loop that frees the inner boxes and inner data buffers
    /// of a deep-copied `List<List<i32>>`/`List<List<f64>>` worker argument.
    /// The outer data buffer itself is freed separately by the caller.
    fn emit_spawn_nested_list_free(
        &mut self,
        outer_len: inkwell::values::IntValue<'ctx>,
        outer_data: inkwell::values::PointerValue<'ctx>,
        inner_is_string: bool,
    ) -> Result<(), CompileError> {
        let i64_ty = self.generator.context.i64_type();
        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.generator.list_struct_type();
        let function = self.current_function()?;
        let header = self
            .generator
            .context
            .append_basic_block(function, "spawn_nested_free_header");
        let body = self
            .generator
            .context
            .append_basic_block(function, "spawn_nested_free_body");
        let exit = self
            .generator
            .context
            .append_basic_block(function, "spawn_nested_free_exit");
        let idx_storage = self
            .generator
            .build_alloca(BasicTypeEnum::IntType(i64_ty), "spawn_nested_free_idx")?;
        self.generator
            .build_store(idx_storage, i64_ty.const_int(0, false))?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(header);
        let idx = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                idx_storage,
                "spawn_nested_free_idx_val",
            )?
            .into_int_value();
        let cond = self
            .generator
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                idx,
                outer_len,
                "spawn_nested_free_cond",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn nested free cmp: {e}")))?;
        self.generator.build_cond_br(cond, body, exit)?;

        self.generator.builder.position_at_end(body);
        let slot = self.generator.build_in_bounds_gep(
            i64_ty,
            outer_data,
            &[idx],
            "spawn_nested_free_slot",
        )?;
        let inner_i64 = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                slot,
                "spawn_nested_free_inner_i64",
            )?
            .into_int_value();
        let inner_ptr = self
            .generator
            .builder
            .build_int_to_ptr(inner_i64, ptr_ty, "spawn_nested_free_inner_ptr")
            .map_err(|e| CompileError::LlvmError(format!("spawn nested free ptr: {e}")))?;
        let inner_val = self
            .generator
            .build_load(
                BasicTypeEnum::StructType(list_ty),
                inner_ptr,
                "spawn_nested_free_inner_val",
            )?
            .into_struct_value();
        let inner_len = self
            .generator
            .build_extract_value(inner_val.into(), 0, "spawn_nested_free_inner_len")?
            .into_int_value();
        let inner_data = self
            .generator
            .build_extract_value(inner_val.into(), 1, "spawn_nested_free_inner_data")?
            .into_pointer_value();
        if inner_is_string {
            self.emit_spawn_string_list_element_free(inner_len, inner_data)?;
        }
        let free_fn = self.generator.get_runtime_fn("free")?;
        self.generator
            .builder
            .build_call(
                free_fn,
                &[BasicMetadataValueEnum::PointerValue(inner_data)],
                "spawn_nested_free_inner_data",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn nested inner data free: {e}")))?;
        self.generator
            .builder
            .build_call(
                free_fn,
                &[BasicMetadataValueEnum::PointerValue(inner_ptr)],
                "spawn_nested_free_inner_box",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn nested inner box free: {e}")))?;
        let next = self
            .generator
            .builder
            .build_int_add(
                idx,
                i64_ty.const_int(1, false),
                "spawn_nested_free_idx_next",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn nested free inc: {e}")))?;
        self.generator.build_store(idx_storage, next)?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(exit);
        Ok(())
    }

    /// Runtime loop to free each deep-copied record box in a `List<Record>`
    /// worker argument. The boxes are allocated by
    /// `emit_spawn_struct_list_clone`. Any cloned String/List data inside
    /// each element is released before freeing the box.
    fn emit_spawn_struct_list_element_free(
        &mut self,
        len: inkwell::values::IntValue<'ctx>,
        data: inkwell::values::PointerValue<'ctx>,
        elem_ty: inkwell::types::StructType<'ctx>,
        string_paths: &[Vec<u32>],
        list_paths: &[Vec<u32>],
    ) -> Result<(), CompileError> {
        let i64_ty = self.generator.context.i64_type();
        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());
        let function = self.current_function()?;
        let header = self
            .generator
            .context
            .append_basic_block(function, "spawn_struct_list_free_header");
        let body = self
            .generator
            .context
            .append_basic_block(function, "spawn_struct_list_free_body");
        let exit = self
            .generator
            .context
            .append_basic_block(function, "spawn_struct_list_free_exit");
        let idx_storage = self
            .generator
            .build_alloca(BasicTypeEnum::IntType(i64_ty), "spawn_struct_list_free_idx")?;
        self.generator
            .build_store(idx_storage, i64_ty.const_int(0, false))?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(header);
        let idx = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                idx_storage,
                "spawn_struct_list_free_idx_val",
            )?
            .into_int_value();
        let cond = self
            .generator
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                idx,
                len,
                "spawn_struct_list_free_cond",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn struct list free cmp: {e}")))?;
        self.generator.build_cond_br(cond, body, exit)?;

        self.generator.builder.position_at_end(body);
        let elem_slot = self.generator.build_in_bounds_gep(
            i64_ty,
            data,
            &[idx],
            "spawn_struct_list_free_elem_slot",
        )?;
        let elem_handle = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                elem_slot,
                "spawn_struct_list_free_elem_handle",
            )?
            .into_int_value();
        let elem_ptr = self
            .generator
            .builder
            .build_int_to_ptr(elem_handle, ptr_ty, "spawn_struct_list_free_elem_ptr")
            .map_err(|e| CompileError::LlvmError(format!("spawn struct list elem ptr: {e}")))?;
        let elem = self
            .generator
            .build_load(
                BasicTypeEnum::StructType(elem_ty),
                elem_ptr,
                "spawn_struct_list_free_elem",
            )?
            .into_struct_value();
        let free_fn = self.generator.get_runtime_fn("free")?;
        let free_str = self.generator.get_runtime_fn("mimi_string_free")?;
        for path in string_paths {
            let mut cur = elem;
            let last = path.len() - 1;
            for (idx, &field_idx) in path.iter().enumerate() {
                let step = self
                    .generator
                    .build_extract_value(cur.into(), field_idx, "spawn_struct_list_str_path")?
                    .into_struct_value();
                if idx == last {
                    let str_data = self
                        .generator
                        .build_extract_value(step.into(), 0, "spawn_struct_list_str_data")?
                        .into_pointer_value();
                    self.generator
                        .builder
                        .build_call(
                            free_str,
                            &[BasicMetadataValueEnum::PointerValue(str_data)],
                            "spawn_struct_list_str_free",
                        )
                        .map_err(|e| {
                            CompileError::LlvmError(format!("spawn struct list str free: {e}"))
                        })?;
                } else {
                    cur = step;
                }
            }
        }
        for path in list_paths {
            let mut cur = elem;
            let last = path.len() - 1;
            for (idx, &field_idx) in path.iter().enumerate() {
                let step = self
                    .generator
                    .build_extract_value(cur.into(), field_idx, "spawn_struct_list_list_path")?
                    .into_struct_value();
                if idx == last {
                    let list_data = self
                        .generator
                        .build_extract_value(step.into(), 1, "spawn_struct_list_list_data")?
                        .into_pointer_value();
                    self.generator
                        .builder
                        .build_call(
                            free_fn,
                            &[BasicMetadataValueEnum::PointerValue(list_data)],
                            "spawn_struct_list_list_free",
                        )
                        .map_err(|e| {
                            CompileError::LlvmError(format!("spawn struct list list free: {e}"))
                        })?;
                } else {
                    cur = step;
                }
            }
        }
        self.generator
            .builder
            .build_call(
                free_fn,
                &[BasicMetadataValueEnum::PointerValue(elem_ptr)],
                "spawn_struct_list_free_box",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn struct list box free: {e}")))?;
        let next = self
            .generator
            .builder
            .build_int_add(
                idx,
                i64_ty.const_int(1, false),
                "spawn_struct_list_free_idx_next",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn struct list free inc: {e}")))?;
        self.generator.build_store(idx_storage, next)?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(exit);
        Ok(())
    }

    /// Runtime loop that deep-copies a `List<Record>` data buffer. Each
    /// source element is a pointer to a heap-allocated record struct. String
    /// and scalar-List leaves inside each element are deep-copied using the
    /// same recursive paths used for whole-Record worker arguments.
    fn emit_spawn_struct_list_clone(
        &mut self,
        len: inkwell::values::IntValue<'ctx>,
        src_data: inkwell::values::PointerValue<'ctx>,
        dst_data: inkwell::values::PointerValue<'ctx>,
        elem_ty: inkwell::types::StructType<'ctx>,
        string_paths: &[Vec<u32>],
        list_paths: &[Vec<u32>],
    ) -> Result<(), CompileError> {
        let i64_ty = self.generator.context.i64_type();
        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());
        let function = self.current_function()?;
        let header = self
            .generator
            .context
            .append_basic_block(function, "spawn_struct_list_clone_header");
        let body = self
            .generator
            .context
            .append_basic_block(function, "spawn_struct_list_clone_body");
        let exit = self
            .generator
            .context
            .append_basic_block(function, "spawn_struct_list_clone_exit");
        let idx_storage = self.generator.build_alloca(
            BasicTypeEnum::IntType(i64_ty),
            "spawn_struct_list_clone_idx",
        )?;
        self.generator
            .build_store(idx_storage, i64_ty.const_int(0, false))?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(header);
        let idx = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                idx_storage,
                "spawn_struct_list_clone_idx_val",
            )?
            .into_int_value();
        let cond = self
            .generator
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                idx,
                len,
                "spawn_struct_list_clone_cond",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn struct list clone cmp: {e}")))?;
        self.generator.build_cond_br(cond, body, exit)?;

        self.generator.builder.position_at_end(body);
        let src_slot = self.generator.build_in_bounds_gep(
            i64_ty,
            src_data,
            &[idx],
            "spawn_struct_list_clone_src_slot",
        )?;
        let src_handle = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                src_slot,
                "spawn_struct_list_clone_src_handle",
            )?
            .into_int_value();
        let src_ptr = self
            .generator
            .builder
            .build_int_to_ptr(src_handle, ptr_ty, "spawn_struct_list_clone_src_ptr")
            .map_err(|e| CompileError::LlvmError(format!("spawn struct list src ptr: {e}")))?;
        let elem = self
            .generator
            .build_load(
                BasicTypeEnum::StructType(elem_ty),
                src_ptr,
                "spawn_struct_list_clone_elem",
            )?
            .into_struct_value();
        let mut cloned = elem;
        for path in string_paths {
            let mut pairs = Vec::new();
            let mut cur = cloned;
            for &field_idx in path {
                pairs.push((cur, field_idx));
                cur = self
                    .generator
                    .build_extract_value(cur.into(), field_idx, "spawn_struct_list_str_pair")?
                    .into_struct_value();
            }
            let str_sv = cur;
            let str_data = self
                .generator
                .build_extract_value(str_sv.into(), 0, "spawn_struct_list_str_data_in")?
                .into_pointer_value();
            let str_len = self
                .generator
                .build_extract_value(str_sv.into(), 1, "spawn_struct_list_str_len_in")?
                .into_int_value();
            let clone_fn = self.generator.get_runtime_fn("mimi_str_clone")?;
            let handle = self
                .generator
                .builder
                .build_call(
                    clone_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(str_data),
                        BasicMetadataValueEnum::IntValue(str_len),
                    ],
                    "spawn_struct_list_str_clone",
                )
                .map_err(|e| CompileError::LlvmError(format!("spawn struct list str clone: {e}")))?
                .try_as_basic_value_opt()
                .ok_or_else(|| {
                    CompileError::LlvmError("spawn struct list str clone returned void".into())
                })?
                .into_int_value();
            let clone_ptr = self
                .generator
                .builder
                .build_int_to_ptr(handle, ptr_ty, "spawn_struct_list_str_clone_ptr")
                .map_err(|e| CompileError::LlvmError(format!("spawn struct list str ptr: {e}")))?;
            let string_ty = str_sv.get_type();
            let new_str = string_ty.get_undef();
            let new_str = self
                .generator
                .builder
                .build_insert_value(new_str, clone_ptr, 0, "spawn_struct_list_str_new_ptr")
                .map_err(|e| {
                    CompileError::LlvmError(format!("spawn struct list str new ptr: {e}"))
                })?
                .into_struct_value();
            let new_str = self
                .generator
                .builder
                .build_insert_value(new_str, str_len, 1, "spawn_struct_list_str_new_len")
                .map_err(|e| {
                    CompileError::LlvmError(format!("spawn struct list str new len: {e}"))
                })?
                .into_struct_value();
            let mut rebuilt = new_str;
            for (parent, parent_idx) in pairs.into_iter().rev() {
                rebuilt = self
                    .generator
                    .builder
                    .build_insert_value(
                        parent,
                        rebuilt,
                        parent_idx,
                        "spawn_struct_list_str_rebuild",
                    )
                    .map_err(|e| {
                        CompileError::LlvmError(format!("spawn struct list str rebuild: {e}"))
                    })?
                    .into_struct_value();
            }
            cloned = rebuilt;
        }
        for path in list_paths {
            let mut pairs = Vec::new();
            let mut cur = cloned;
            for &field_idx in path {
                pairs.push((cur, field_idx));
                cur = self
                    .generator
                    .build_extract_value(cur.into(), field_idx, "spawn_struct_list_list_pair")?
                    .into_struct_value();
            }
            let list_sv = cur;
            let list_len = self
                .generator
                .build_extract_value(list_sv.into(), 0, "spawn_struct_list_list_len_in")?
                .into_int_value();
            let list_data = self
                .generator
                .build_extract_value(list_sv.into(), 1, "spawn_struct_list_list_data_in")?
                .into_pointer_value();
            let total = self
                .generator
                .builder
                .build_int_mul(
                    list_len,
                    i64_ty.const_int(8, false),
                    "spawn_struct_list_list_bytes",
                )
                .map_err(|e| {
                    CompileError::LlvmError(format!("spawn struct list list size: {e}"))
                })?;
            let clone_data = self
                .generator
                .malloc_or_abort(total, "spawn_struct_list_list_heap")?;
            let memcpy_fn = self.generator.get_runtime_fn("memcpy")?;
            self.generator
                .builder
                .build_call(
                    memcpy_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(clone_data),
                        BasicMetadataValueEnum::PointerValue(list_data),
                        BasicMetadataValueEnum::IntValue(total),
                    ],
                    "spawn_struct_list_list_copy",
                )
                .map_err(|e| {
                    CompileError::LlvmError(format!("spawn struct list list copy: {e}"))
                })?;
            let new_list = list_sv.get_type().get_undef();
            let new_list = self
                .generator
                .builder
                .build_insert_value(new_list, list_len, 0, "spawn_struct_list_list_new_len")
                .map_err(|e| {
                    CompileError::LlvmError(format!("spawn struct list list new len: {e}"))
                })?
                .into_struct_value();
            let new_list = self
                .generator
                .builder
                .build_insert_value(new_list, clone_data, 1, "spawn_struct_list_list_new_data")
                .map_err(|e| {
                    CompileError::LlvmError(format!("spawn struct list list new data: {e}"))
                })?
                .into_struct_value();
            let mut rebuilt = new_list;
            for (parent, parent_idx) in pairs.into_iter().rev() {
                rebuilt = self
                    .generator
                    .builder
                    .build_insert_value(
                        parent,
                        rebuilt,
                        parent_idx,
                        "spawn_struct_list_list_rebuild",
                    )
                    .map_err(|e| {
                        CompileError::LlvmError(format!("spawn struct list list rebuild: {e}"))
                    })?
                    .into_struct_value();
            }
            cloned = rebuilt;
        }
        let box_size = self
            .generator
            .llvm_type_size_bytes(BasicTypeEnum::StructType(elem_ty));
        let box_size_val = i64_ty.const_int(box_size, false);
        let dst_box = self
            .generator
            .malloc_or_abort(box_size_val, "spawn_struct_list_clone_box")?;
        self.generator.build_store(dst_box, cloned)?;
        let dst_handle = self
            .generator
            .builder
            .build_ptr_to_int(dst_box, i64_ty, "spawn_struct_list_clone_handle")
            .map_err(|e| CompileError::LlvmError(format!("spawn struct list handle: {e}")))?;
        let dst_slot = self.generator.build_in_bounds_gep(
            i64_ty,
            dst_data,
            &[idx],
            "spawn_struct_list_clone_dst_slot",
        )?;
        self.generator.build_store(dst_slot, dst_handle)?;
        let next = self
            .generator
            .builder
            .build_int_add(
                idx,
                i64_ty.const_int(1, false),
                "spawn_struct_list_clone_idx_next",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn struct list clone inc: {e}")))?;
        self.generator.build_store(idx_storage, next)?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(exit);
        Ok(())
    }

    /// Real-thread spawn for direct calls to named functions.
    ///
    /// This is the default path when the environment variable
    /// `MIMI_EAGER_SPAWN` is unset. It allocates a heap environment for the
    /// call arguments, generates a poll function that runs the call on a worker
    /// thread, stores the result at future offset 16, and marks the future
    /// completed. Non-call expressions and calls with borrow parameters fall
    /// back to the eager/synchronous resolved path.
    fn try_emit_spawn_thread(
        &mut self,
        value: &ResolvedExpr,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
        // A struct field is scalar-like when it does not transitively carry
        // a pointer (heap allocation). Such fields can be copied by value into
        // worker envs without any deep-copy/cleanup.
        fn llvm_type_has_heap(ty: &BasicTypeEnum<'_>) -> bool {
            match ty {
                BasicTypeEnum::PointerType(_) => true,
                BasicTypeEnum::StructType(st) => {
                    st.get_field_types().iter().any(llvm_type_has_heap)
                }
                _ => false,
            }
        }
        fn is_string_shape(ty: &BasicTypeEnum<'_>) -> bool {
            matches!(
                ty,
                BasicTypeEnum::StructType(inner)
                    if inner.get_field_types().len() == 2
                        && matches!(
                            inner.get_field_types()[0],
                            BasicTypeEnum::PointerType(_)
                        )
                        && matches!(
                            inner.get_field_types()[1],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                        )
            )
        }
        fn is_list_shape(ty: &BasicTypeEnum<'_>) -> bool {
            matches!(
                ty,
                BasicTypeEnum::StructType(inner)
                    if inner.get_field_types().len() == 2
                        && matches!(
                            inner.get_field_types()[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                        )
                        && matches!(inner.get_field_types()[1], BasicTypeEnum::PointerType(_))
            )
        }
        fn collect_struct_heap_paths(
            ty: &BasicTypeEnum<'_>,
            prefix: &mut Vec<u32>,
            string_paths: &mut Vec<Vec<u32>>,
            list_paths: &mut Vec<Vec<u32>>,
            supported: &mut bool,
        ) {
            match ty {
                BasicTypeEnum::IntType(_) | BasicTypeEnum::FloatType(_) => {}
                BasicTypeEnum::StructType(_) if !llvm_type_has_heap(ty) => {}
                BasicTypeEnum::StructType(_) if is_string_shape(ty) => {
                    string_paths.push(prefix.clone());
                }
                BasicTypeEnum::StructType(_) if is_list_shape(ty) => {
                    list_paths.push(prefix.clone());
                }
                BasicTypeEnum::StructType(st) => {
                    for (idx, field_ty) in st.get_field_types().iter().enumerate() {
                        prefix.push(idx as u32);
                        collect_struct_heap_paths(
                            field_ty,
                            prefix,
                            string_paths,
                            list_paths,
                            supported,
                        );
                        prefix.pop();
                    }
                }
                _ => *supported = false,
            }
        }
        if std::env::var("MIMI_EAGER_SPAWN").is_ok() {
            return Ok(None);
        }
        let ResolvedExprKind::Call(call) = &value.kind else {
            return Ok(None);
        };
        let ResolvedCallee::Function(owner) = &call.callee else {
            return Ok(None);
        };
        let symbol = self.callable_symbol(owner)?.to_string();
        let Some(callee) = self.generator.module.get_function(&symbol) else {
            return Ok(None);
        };
        // Borrow parameters cannot be copied into a worker env safely.
        let borrow_positions: Vec<bool> = self
            .program
            .callable(owner)
            .map(|callee_callable| {
                callee_callable
                    .signature
                    .parameters
                    .iter()
                    .map(|p| {
                        matches!(
                            p.permission,
                            Some(crate::core::ir::Permission::View)
                                | Some(crate::core::ir::Permission::Mutate)
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        if borrow_positions.iter().any(|b| *b) || borrow_positions.len() != call.arguments.len() {
            return Ok(None);
        }

        // Evaluate and coerce call arguments.
        let mut arg_values: Vec<BasicValueEnum<'ctx>> = Vec::with_capacity(call.arguments.len());
        let mut is_string_arg: Vec<bool> = Vec::with_capacity(call.arguments.len());
        let mut is_list_arg: Vec<bool> = Vec::with_capacity(call.arguments.len());
        let mut is_string_list_arg: Vec<bool> = Vec::with_capacity(call.arguments.len());
        let mut is_nested_list_arg: Vec<bool> = Vec::with_capacity(call.arguments.len());
        let mut nested_inner_elem_sizes: Vec<u64> = Vec::with_capacity(call.arguments.len());
        let mut nested_inner_is_string: Vec<bool> = Vec::with_capacity(call.arguments.len());
        let mut is_scalar_struct_arg: Vec<bool> = Vec::with_capacity(call.arguments.len());
        let mut is_heap_struct_arg: Vec<bool> = Vec::with_capacity(call.arguments.len());
        let mut heap_struct_string_paths: Vec<Vec<Vec<u32>>> =
            Vec::with_capacity(call.arguments.len());
        let mut heap_struct_list_paths: Vec<Vec<Vec<u32>>> =
            Vec::with_capacity(call.arguments.len());
        let mut list_elem_sizes: Vec<u64> = Vec::with_capacity(call.arguments.len());
        let mut is_struct_list_arg: Vec<bool> = Vec::with_capacity(call.arguments.len());
        let mut struct_list_elem_types: Vec<Option<inkwell::types::StructType<'ctx>>> =
            Vec::with_capacity(call.arguments.len());
        let mut struct_list_string_paths: Vec<Vec<Vec<u32>>> =
            Vec::with_capacity(call.arguments.len());
        let mut struct_list_list_paths: Vec<Vec<Vec<u32>>> =
            Vec::with_capacity(call.arguments.len());
        let params = callee.get_params();
        for (i, argument) in call.arguments.iter().enumerate() {
            let v = self.emit_expr(&argument.value, frame)?;
            let v = self.apply_conversion(v, &argument.conversion)?;
            let v = if let Some(param) = params.get(i) {
                let param_ty = param.get_type();
                if v.get_type() != param_ty {
                    self.coerce_to(v, param_ty)?
                } else {
                    v
                }
            } else {
                v
            };
            let is_string = matches!(
                self.program.resolved_types().get(&argument.value.ty),
                Some(ResolvedType::Primitive(PrimitiveType::String))
            );
            is_string_arg.push(is_string);
            let (
                is_scalar_list,
                is_string_list,
                is_nested_list,
                elem_size,
                nested_inner_size,
                nested_inner_string,
                is_struct_list,
                struct_elem_ty,
                struct_string_paths,
                struct_list_paths,
            ) = match self.program.resolved_types().get(&argument.value.ty) {
                Some(ResolvedType::Nominal {
                    item, arguments, ..
                }) if item.as_str() == "builtin:type:List" && arguments.len() == 1 => {
                    let elem_ty = self.lower_type(&arguments[0])?;
                    if matches!(
                        elem_ty,
                        BasicTypeEnum::IntType(_) | BasicTypeEnum::FloatType(_)
                    ) {
                        (
                            true,
                            false,
                            false,
                            self.generator.llvm_type_size_bytes(elem_ty),
                            0,
                            false,
                            false,
                            None,
                            Vec::new(),
                            Vec::new(),
                        )
                    } else if matches!(
                        self.program.resolved_types().get(&arguments[0]),
                        Some(ResolvedType::Primitive(PrimitiveType::String))
                    ) {
                        (
                            false,
                            true,
                            false,
                            0,
                            0,
                            false,
                            false,
                            None,
                            Vec::new(),
                            Vec::new(),
                        )
                    } else if let Some(ResolvedType::Nominal {
                        item: inner_item,
                        arguments: inner_arguments,
                        ..
                    }) = self.program.resolved_types().get(&arguments[0])
                    {
                        if inner_item.as_str() == "builtin:type:List" && inner_arguments.len() == 1
                        {
                            let inner_elem_ty = self.lower_type(&inner_arguments[0])?;
                            if matches!(
                                inner_elem_ty,
                                BasicTypeEnum::IntType(_) | BasicTypeEnum::FloatType(_)
                            ) {
                                (
                                    false,
                                    false,
                                    true,
                                    0,
                                    self.generator.llvm_type_size_bytes(inner_elem_ty),
                                    false,
                                    false,
                                    None,
                                    Vec::new(),
                                    Vec::new(),
                                )
                            } else if matches!(
                                self.program.resolved_types().get(&inner_arguments[0]),
                                Some(ResolvedType::Primitive(PrimitiveType::String))
                            ) {
                                (
                                    false,
                                    false,
                                    true,
                                    0,
                                    0,
                                    true,
                                    false,
                                    None,
                                    Vec::new(),
                                    Vec::new(),
                                )
                            } else {
                                (
                                    false,
                                    false,
                                    false,
                                    0,
                                    0,
                                    false,
                                    false,
                                    None,
                                    Vec::new(),
                                    Vec::new(),
                                )
                            }
                        } else if let BasicTypeEnum::StructType(st) = elem_ty {
                            let mut string_paths = Vec::new();
                            let mut list_paths = Vec::new();
                            let mut all_supported = true;
                            let mut prefix = Vec::new();
                            collect_struct_heap_paths(
                                &elem_ty,
                                &mut prefix,
                                &mut string_paths,
                                &mut list_paths,
                                &mut all_supported,
                            );
                            if all_supported {
                                (
                                    false,
                                    false,
                                    false,
                                    0,
                                    0,
                                    false,
                                    true,
                                    Some(st),
                                    string_paths,
                                    list_paths,
                                )
                            } else {
                                (
                                    false,
                                    false,
                                    false,
                                    0,
                                    0,
                                    false,
                                    false,
                                    None,
                                    Vec::new(),
                                    Vec::new(),
                                )
                            }
                        } else {
                            (
                                false,
                                false,
                                false,
                                0,
                                0,
                                false,
                                false,
                                None,
                                Vec::new(),
                                Vec::new(),
                            )
                        }
                    } else if let BasicTypeEnum::StructType(st) = elem_ty {
                        let mut string_paths = Vec::new();
                        let mut list_paths = Vec::new();
                        let mut all_supported = true;
                        let mut prefix = Vec::new();
                        collect_struct_heap_paths(
                            &elem_ty,
                            &mut prefix,
                            &mut string_paths,
                            &mut list_paths,
                            &mut all_supported,
                        );
                        if all_supported {
                            (
                                false,
                                false,
                                false,
                                0,
                                0,
                                false,
                                true,
                                Some(st),
                                string_paths,
                                list_paths,
                            )
                        } else {
                            (
                                false,
                                false,
                                false,
                                0,
                                0,
                                false,
                                false,
                                None,
                                Vec::new(),
                                Vec::new(),
                            )
                        }
                    } else {
                        (
                            false,
                            false,
                            false,
                            0,
                            0,
                            false,
                            false,
                            None,
                            Vec::new(),
                            Vec::new(),
                        )
                    }
                }
                _ => (
                    false,
                    false,
                    false,
                    0,
                    0,
                    false,
                    false,
                    None,
                    Vec::new(),
                    Vec::new(),
                ),
            };
            is_list_arg.push(is_scalar_list || is_string_list || is_nested_list || is_struct_list);
            is_string_list_arg.push(is_string_list);
            is_nested_list_arg.push(is_nested_list);
            nested_inner_elem_sizes.push(nested_inner_size);
            nested_inner_is_string.push(nested_inner_string);
            list_elem_sizes.push(elem_size);
            is_struct_list_arg.push(is_struct_list);
            struct_list_elem_types.push(struct_elem_ty);
            struct_list_string_paths.push(struct_string_paths);
            struct_list_list_paths.push(struct_list_paths);
            let is_list_value =
                is_scalar_list || is_string_list || is_nested_list || is_struct_list;
            let (is_scalar_struct, is_heap_struct, heap_string_paths, heap_list_paths) =
                if is_list_value {
                    (false, false, Vec::new(), Vec::new())
                } else {
                    match v.get_type() {
                        BasicTypeEnum::StructType(st) => {
                            let mut string_paths = Vec::new();
                            let mut list_paths = Vec::new();
                            let mut all_supported = true;
                            let mut prefix = Vec::new();
                            collect_struct_heap_paths(
                                &st.into(),
                                &mut prefix,
                                &mut string_paths,
                                &mut list_paths,
                                &mut all_supported,
                            );
                            let scalar_struct =
                                st.get_field_types().iter().all(|f| !llvm_type_has_heap(f));
                            (
                                scalar_struct,
                                !scalar_struct && all_supported,
                                string_paths,
                                list_paths,
                            )
                        }
                        _ => (false, false, Vec::new(), Vec::new()),
                    }
                };
            is_scalar_struct_arg.push(is_scalar_struct);
            is_heap_struct_arg.push(is_heap_struct);
            heap_struct_string_paths.push(heap_string_paths);
            heap_struct_list_paths.push(heap_list_paths);
            arg_values.push(v);
        }

        // Only scalar/pointer arguments are safe to copy into the worker env
        // without deep-copying. String and scalar-element List structs are
        // allowed because they get deep-copied heap buffers below. Other
        // Struct/Array/closure/record values stay on the eager path.
        for (
            (
                (
                    (
                        ((((arg, is_string), is_list), is_string_list), is_nested_list),
                        is_struct_list,
                    ),
                    is_scalar_struct,
                ),
                is_heap_struct,
            ),
            elem_size,
        ) in arg_values
            .iter()
            .zip(&is_string_arg)
            .zip(&is_list_arg)
            .zip(&is_string_list_arg)
            .zip(&is_nested_list_arg)
            .zip(&is_struct_list_arg)
            .zip(&is_scalar_struct_arg)
            .zip(&is_heap_struct_arg)
            .zip(&list_elem_sizes)
        {
            if matches!(arg.get_type(), BasicTypeEnum::ArrayType(_)) {
                return Ok(None);
            }
            let is_struct = matches!(arg.get_type(), BasicTypeEnum::StructType(_));
            if is_struct && !is_string && !is_list && !is_scalar_struct && !is_heap_struct {
                return Ok(None);
            }
            if *is_string && *is_list {
                return Ok(None);
            }
            if (*is_scalar_struct || *is_heap_struct) && (*is_string || *is_list) {
                return Ok(None);
            }
            if !is_struct && (*is_string || *is_list || *is_scalar_struct || *is_heap_struct) {
                return Ok(None);
            }
            if *is_list && *elem_size == 0 && !is_string_list && !is_nested_list && !is_struct_list
            {
                return Ok(None);
            }
        }

        let i8_ty = self.generator.context.i8_type();
        let i64_ty = self.generator.context.i64_type();
        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());
        let void_ty = self.generator.context.void_type();
        let result_ty = self.lower_type(&value.ty)?;
        let result_bytes = self.generator.llvm_type_size_bytes(result_ty);

        // ── Generate poll function: void(ptr future) ──
        let poll_name = format!("__resolved_spawn_poll_{}", self.generator.spawn_counter);
        self.generator.spawn_counter += 1;
        let poll_fn_type = void_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(ptr_ty)], false);
        let poll_fn = self
            .generator
            .module
            .add_function(&poll_name, poll_fn_type, None);
        let poll_entry = self.generator.context.append_basic_block(poll_fn, "entry");
        let saved_block = self.generator.builder.get_insert_block();
        self.generator.builder.position_at_end(poll_entry);
        let future_ptr_param = poll_fn
            .get_nth_param(0)
            .ok_or_else(|| CompileError::LlvmError("spawn poll param missing".into()))?
            .into_pointer_value();

        let mut env_ptr: Option<inkwell::values::PointerValue<'ctx>> = None;
        if !arg_values.is_empty() {
            let arg_tys: Vec<BasicTypeEnum<'ctx>> =
                arg_values.iter().map(|v| v.get_type()).collect();
            let env_struct_type = self.generator.context.struct_type(&arg_tys, false);

            // Load env pointer stored at future data offset 16.
            let env_slot_i8 = self.generator.build_in_bounds_gep(
                i8_ty,
                future_ptr_param,
                &[i64_ty.const_int(RESOLVED_FUTURE_DATA_OFFSET, false)],
                "spawn_poll_env_slot",
            )?;
            let env_slot_ptr = self
                .generator
                .builder
                .build_bit_cast(env_slot_i8, ptr_ty, "spawn_poll_env_typed")
                .map_err(|e| CompileError::LlvmError(format!("spawn poll env cast: {e}")))?
                .into_pointer_value();
            let env_loaded = self
                .generator
                .build_load(
                    BasicTypeEnum::PointerType(ptr_ty),
                    env_slot_ptr,
                    "spawn_env_val",
                )?
                .into_pointer_value();
            let env_struct_ptr =
                self.generator
                    .build_pointer_cast(env_loaded, ptr_ty, "spawn_poll_env_struct")?;
            env_ptr = Some(env_loaded);

            let mut thunk_args: Vec<BasicMetadataValueEnum<'ctx>> =
                Vec::with_capacity(arg_values.len());
            let mut string_free_ptrs: Vec<inkwell::values::PointerValue<'ctx>> = Vec::new();
            let mut list_free_ptrs: Vec<inkwell::values::PointerValue<'ctx>> = Vec::new();
            let mut string_list_elems: Vec<(
                inkwell::values::IntValue<'ctx>,
                inkwell::values::PointerValue<'ctx>,
            )> = Vec::new();
            let mut nested_list_elems: Vec<(
                inkwell::values::IntValue<'ctx>,
                inkwell::values::PointerValue<'ctx>,
                bool,
            )> = Vec::new();
            let mut struct_list_elems: Vec<(
                inkwell::values::IntValue<'ctx>,
                inkwell::values::PointerValue<'ctx>,
                inkwell::types::StructType<'ctx>,
            )> = Vec::new();
            for (i, arg_ty) in arg_tys.iter().enumerate() {
                let field_gep = self
                    .generator
                    .builder
                    .build_struct_gep(env_struct_type, env_struct_ptr, i as u32, "spawn_env_gep")
                    .map_err(|e| CompileError::LlvmError(format!("spawn env gep: {e}")))?;
                let field_val = self.generator.build_load(*arg_ty, field_gep, "spawn_arg")?;
                if is_string_arg[i] {
                    let sv = field_val.into_struct_value();
                    let data = self
                        .generator
                        .build_extract_value(sv.into(), 0, "spawn_str_data")?
                        .into_pointer_value();
                    string_free_ptrs.push(data);
                }
                if is_list_arg[i] {
                    let sv = field_val.into_struct_value();
                    let data = self
                        .generator
                        .build_extract_value(sv.into(), 1, "spawn_list_data")?
                        .into_pointer_value();
                    if is_string_list_arg[i] {
                        let len = self
                            .generator
                            .build_extract_value(sv.into(), 0, "spawn_string_list_len")?
                            .into_int_value();
                        string_list_elems.push((len, data));
                    }
                    if is_nested_list_arg[i] {
                        let len = self
                            .generator
                            .build_extract_value(sv.into(), 0, "spawn_nested_list_len")?
                            .into_int_value();
                        nested_list_elems.push((len, data, nested_inner_is_string[i]));
                    }
                    list_free_ptrs.push(data);
                }
                if is_struct_list_arg[i] {
                    let sv = field_val.into_struct_value();
                    let len = self
                        .generator
                        .build_extract_value(sv.into(), 0, "spawn_struct_list_len")?
                        .into_int_value();
                    let data = self
                        .generator
                        .build_extract_value(sv.into(), 1, "spawn_struct_list_data")?
                        .into_pointer_value();
                    if let Some(st) = struct_list_elem_types[i] {
                        struct_list_elems.push((len, data, st));
                    }
                }
                if is_heap_struct_arg[i] {
                    let sv = field_val.into_struct_value();
                    for path in &heap_struct_string_paths[i] {
                        if path.is_empty() {
                            continue;
                        }
                        let mut cur = sv;
                        let last = path.len() - 1;
                        for (idx, &field_idx) in path.iter().enumerate() {
                            let step = self
                                .generator
                                .build_extract_value(cur.into(), field_idx, "spawn_heap_str_path")?
                                .into_struct_value();
                            if idx == last {
                                let data = self
                                    .generator
                                    .build_extract_value(
                                        step.into(),
                                        0,
                                        "spawn_heap_str_path_data",
                                    )?
                                    .into_pointer_value();
                                string_free_ptrs.push(data);
                            } else {
                                cur = step;
                            }
                        }
                    }
                    for path in &heap_struct_list_paths[i] {
                        // An empty path means the value itself (not a field of a
                        // struct) was classified as a heap-struct list/string; it
                        // is handled by the `is_list_arg` / `is_string_list_arg`
                        // arms above, not here. Skipping it also avoids the
                        // `path.len() - 1` underflow panic that fired for
                        // `List<List<List<T>>>` (the nested-list classifier pushes
                        // an empty prefix when the element type is itself a
                        // list/string shape).
                        if path.is_empty() {
                            continue;
                        }
                        let mut cur = sv;
                        let last = path.len() - 1;
                        for (idx, &field_idx) in path.iter().enumerate() {
                            let step = self
                                .generator
                                .build_extract_value(cur.into(), field_idx, "spawn_heap_list_path")?
                                .into_struct_value();
                            if idx == last {
                                let data = self
                                    .generator
                                    .build_extract_value(
                                        step.into(),
                                        1,
                                        "spawn_heap_list_path_data",
                                    )?
                                    .into_pointer_value();
                                list_free_ptrs.push(data);
                            } else {
                                cur = step;
                            }
                        }
                    }
                }
                thunk_args.push(BasicMetadataValueEnum::from(field_val));
            }
            let call = self
                .generator
                .builder
                .build_call(callee, &thunk_args, "spawn_poll_call")
                .map_err(|e| CompileError::LlvmError(format!("spawn poll call: {e}")))?;
            let result = call
                .try_as_basic_value_opt()
                .ok_or_else(|| CompileError::LlvmError("spawn poll call returned void".into()))?;
            let result = self.coerce_to(result, result_ty)?;

            let result_slot = self.generator.build_in_bounds_gep(
                i8_ty,
                future_ptr_param,
                &[i64_ty.const_int(RESOLVED_FUTURE_DATA_OFFSET, false)],
                "spawn_poll_result_i8",
            )?;
            let result_ptr = self
                .generator
                .builder
                .build_bit_cast(result_slot, ptr_ty, "spawn_poll_result_ptr")
                .map_err(|e| CompileError::LlvmError(format!("spawn poll result cast: {e}")))?
                .into_pointer_value();
            self.generator.build_store(result_ptr, result)?;

            // Free the deep-copied string buffers now that the callee has
            // finished reading them.
            for data_ptr in string_free_ptrs {
                let free_str = self.generator.get_runtime_fn("mimi_string_free")?;
                self.generator
                    .builder
                    .build_call(
                        free_str,
                        &[BasicMetadataValueEnum::PointerValue(data_ptr)],
                        "spawn_clone_free",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("spawn string free: {e}")))?;
            }
            // Free deep-copied string elements inside List<string> arguments.
            for (len, data) in string_list_elems {
                self.emit_spawn_string_list_element_free(len, data)?;
            }
            // Free deep-copied nested List<List<i32|f64>> inner boxes/data.
            for (len, data, inner_is_string) in nested_list_elems {
                self.emit_spawn_nested_list_free(len, data, inner_is_string)?;
            }
            // Free deep-copied record boxes inside List<Record> arguments.
            for (si, (len, data, st)) in struct_list_elems.into_iter().enumerate() {
                self.emit_spawn_struct_list_element_free(
                    len,
                    data,
                    st,
                    &struct_list_string_paths[si],
                    &struct_list_list_paths[si],
                )?;
            }
            // Free deep-copied list data buffers (scalar lists) and the
            // List<string> data arrays (after their element strings).
            for data_ptr in list_free_ptrs {
                let free_fn = self.generator.get_runtime_fn("free")?;
                self.generator
                    .builder
                    .build_call(
                        free_fn,
                        &[BasicMetadataValueEnum::PointerValue(data_ptr)],
                        "spawn_list_free",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("spawn list free: {e}")))?;
            }
        } else {
            let call = self
                .generator
                .builder
                .build_call(callee, &[], "spawn_poll_call")
                .map_err(|e| CompileError::LlvmError(format!("spawn poll call: {e}")))?;
            let result = call
                .try_as_basic_value_opt()
                .ok_or_else(|| CompileError::LlvmError("spawn poll call returned void".into()))?;
            let result = self.coerce_to(result, result_ty)?;
            let result_slot = self.generator.build_in_bounds_gep(
                i8_ty,
                future_ptr_param,
                &[i64_ty.const_int(RESOLVED_FUTURE_DATA_OFFSET, false)],
                "spawn_poll_result_i8",
            )?;
            let result_ptr = self
                .generator
                .builder
                .build_bit_cast(result_slot, ptr_ty, "spawn_poll_result_ptr")
                .map_err(|e| CompileError::LlvmError(format!("spawn poll result cast: {e}")))?
                .into_pointer_value();
            self.generator.build_store(result_ptr, result)?;
        }

        let set_fn = self.generator.get_runtime_fn("mimi_future_set_completed")?;
        self.generator
            .builder
            .build_call(
                set_fn,
                &[BasicMetadataValueEnum::PointerValue(future_ptr_param)],
                "spawn_poll_set",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn set completed: {e}")))?;

        if let Some(env_ptr) = env_ptr {
            let free_fn = self.generator.get_runtime_fn("free")?;
            self.generator
                .builder
                .build_call(
                    free_fn,
                    &[BasicMetadataValueEnum::PointerValue(env_ptr)],
                    "spawn_env_free",
                )
                .map_err(|e| CompileError::LlvmError(format!("spawn env free: {e}")))?;
        }
        self.generator.build_return(None)?;
        if let Some(bb) = saved_block {
            self.generator.builder.position_at_end(bb);
        }

        // ── At spawn site: allocate future + env, start worker thread ──
        let alloc_fn = self.generator.get_runtime_fn("mimi_future_alloc")?;
        let total_size = i64_ty.const_int(RESOLVED_FUTURE_DATA_OFFSET + result_bytes.max(8), false);
        let future_ptr = self
            .generator
            .builder
            .build_call(
                alloc_fn,
                &[BasicMetadataValueEnum::IntValue(total_size)],
                "spawn_future_alloc",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn future alloc: {e}")))?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("spawn future alloc returned void".into()))?
            .into_pointer_value();

        if !arg_values.is_empty() {
            let arg_tys: Vec<BasicTypeEnum<'ctx>> =
                arg_values.iter().map(|v| v.get_type()).collect();
            let env_struct_type = self.generator.context.struct_type(&arg_tys, false);
            let env_size = env_struct_type
                .size_of()
                .ok_or_else(|| CompileError::Unsupported("spawn env size_of failed".into()))?;
            let env_heap_ptr = self.generator.malloc_or_abort(env_size, "spawn_env_heap")?;
            for (i, arg) in arg_values.iter().enumerate() {
                let field_gep = self
                    .generator
                    .builder
                    .build_struct_gep(
                        env_struct_type,
                        env_heap_ptr,
                        i as u32,
                        "spawn_env_store_gep",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("spawn env store gep: {e}")))?;
                if is_string_arg[i] {
                    let sv = (*arg).into_struct_value();
                    let data = self
                        .generator
                        .build_extract_value(sv.into(), 0, "spawn_str_data_in")?
                        .into_pointer_value();
                    let len = self
                        .generator
                        .build_extract_value(sv.into(), 1, "spawn_str_len_in")?
                        .into_int_value();
                    let clone_fn = self.generator.get_runtime_fn("mimi_str_clone")?;
                    let handle = self
                        .generator
                        .builder
                        .build_call(
                            clone_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(data),
                                BasicMetadataValueEnum::IntValue(len),
                            ],
                            "spawn_str_clone",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("spawn str clone: {e}")))?
                        .try_as_basic_value_opt()
                        .ok_or_else(|| {
                            CompileError::LlvmError("spawn str clone returned void".into())
                        })?
                        .into_int_value();
                    let clone_ptr = self
                        .generator
                        .builder
                        .build_int_to_ptr(handle, ptr_ty, "spawn_str_clone_ptr")
                        .map_err(|e| {
                            CompileError::LlvmError(format!("spawn str clone cast: {e}"))
                        })?;
                    let string_ty = arg.get_type().into_struct_type();
                    let string_val = string_ty.get_undef();
                    let string_val = self
                        .generator
                        .builder
                        .build_insert_value(string_val, clone_ptr, 0, "spawn_str_with_ptr")
                        .map_err(|e| CompileError::LlvmError(format!("spawn str build: {e}")))?;
                    let string_val = self
                        .generator
                        .builder
                        .build_insert_value(string_val, len, 1, "spawn_str_with_len")
                        .map_err(|e| CompileError::LlvmError(format!("spawn str build: {e}")))?;
                    self.generator.build_store(field_gep, string_val)?;
                } else if is_list_arg[i] && is_struct_list_arg[i] {
                    let sv = (*arg).into_struct_value();
                    let len = self
                        .generator
                        .build_extract_value(sv.into(), 0, "spawn_struct_list_len_in")?
                        .into_int_value();
                    let data = self
                        .generator
                        .build_extract_value(sv.into(), 1, "spawn_struct_list_data_in")?
                        .into_pointer_value();
                    let total = self
                        .generator
                        .builder
                        .build_int_mul(len, i64_ty.const_int(8, false), "spawn_struct_list_bytes")
                        .map_err(|e| {
                            CompileError::LlvmError(format!("spawn struct list size: {e}"))
                        })?;
                    let clone_data = self
                        .generator
                        .malloc_or_abort(total, "spawn_struct_list_heap")?;
                    if let Some(st) = struct_list_elem_types[i] {
                        self.emit_spawn_struct_list_clone(
                            len,
                            data,
                            clone_data,
                            st,
                            &struct_list_string_paths[i],
                            &struct_list_list_paths[i],
                        )?;
                    }
                    let list_ty = arg.get_type().into_struct_type();
                    let list_val = list_ty.get_undef();
                    let list_val = self
                        .generator
                        .builder
                        .build_insert_value(list_val, len, 0, "spawn_struct_list_with_len")
                        .map_err(|e| {
                            CompileError::LlvmError(format!("spawn struct list build: {e}"))
                        })?;
                    let list_val = self
                        .generator
                        .builder
                        .build_insert_value(list_val, clone_data, 1, "spawn_struct_list_with_data")
                        .map_err(|e| {
                            CompileError::LlvmError(format!("spawn struct list build: {e}"))
                        })?;
                    self.generator.build_store(field_gep, list_val)?;
                } else if is_list_arg[i] && is_string_list_arg[i] {
                    let sv = (*arg).into_struct_value();
                    let len = self
                        .generator
                        .build_extract_value(sv.into(), 0, "spawn_string_list_len_in")?
                        .into_int_value();
                    let data = self
                        .generator
                        .build_extract_value(sv.into(), 1, "spawn_string_list_data_in")?
                        .into_pointer_value();
                    let total = self
                        .generator
                        .builder
                        .build_int_mul(len, i64_ty.const_int(8, false), "spawn_string_list_bytes")
                        .map_err(|e| CompileError::LlvmError(format!("spawn list size: {e}")))?;
                    let clone_data = self.generator.malloc_or_abort(total, "spawn_list_heap")?;
                    self.emit_spawn_string_list_clone(len, data, clone_data)?;
                    let list_ty = arg.get_type().into_struct_type();
                    let list_val = list_ty.get_undef();
                    let list_val = self
                        .generator
                        .builder
                        .build_insert_value(list_val, len, 0, "spawn_list_with_len")
                        .map_err(|e| CompileError::LlvmError(format!("spawn list build: {e}")))?;
                    let list_val = self
                        .generator
                        .builder
                        .build_insert_value(list_val, clone_data, 1, "spawn_list_with_data")
                        .map_err(|e| CompileError::LlvmError(format!("spawn list build: {e}")))?;
                    self.generator.build_store(field_gep, list_val)?;
                } else if is_list_arg[i] && is_nested_list_arg[i] {
                    let sv = (*arg).into_struct_value();
                    let len = self
                        .generator
                        .build_extract_value(sv.into(), 0, "spawn_nested_list_len_in")?
                        .into_int_value();
                    let data = self
                        .generator
                        .build_extract_value(sv.into(), 1, "spawn_nested_list_data_in")?
                        .into_pointer_value();
                    let total = self
                        .generator
                        .builder
                        .build_int_mul(len, i64_ty.const_int(8, false), "spawn_nested_list_bytes")
                        .map_err(|e| CompileError::LlvmError(format!("spawn list size: {e}")))?;
                    let clone_data = self.generator.malloc_or_abort(total, "spawn_list_heap")?;
                    self.emit_spawn_nested_list_clone(
                        len,
                        data,
                        clone_data,
                        nested_inner_elem_sizes[i],
                        nested_inner_is_string[i],
                    )?;
                    let list_ty = arg.get_type().into_struct_type();
                    let list_val = list_ty.get_undef();
                    let list_val = self
                        .generator
                        .builder
                        .build_insert_value(list_val, len, 0, "spawn_list_with_len")
                        .map_err(|e| CompileError::LlvmError(format!("spawn list build: {e}")))?;
                    let list_val = self
                        .generator
                        .builder
                        .build_insert_value(list_val, clone_data, 1, "spawn_list_with_data")
                        .map_err(|e| CompileError::LlvmError(format!("spawn list build: {e}")))?;
                    self.generator.build_store(field_gep, list_val)?;
                } else if is_list_arg[i] {
                    let sv = (*arg).into_struct_value();
                    let len = self
                        .generator
                        .build_extract_value(sv.into(), 0, "spawn_list_len_in")?
                        .into_int_value();
                    let data = self
                        .generator
                        .build_extract_value(sv.into(), 1, "spawn_list_data_in")?
                        .into_pointer_value();
                    // List data buffers store every scalar element as `i64`
                    // (8-byte stride) — see `emit_list_literal`, which allocates
                    // `count * 8` bytes and `coerce_to_i64`s each element, and
                    // the for-loop reader which GEPs `i64` over the data pointer.
                    // The semantic element width (`list_elem_sizes[i]`, e.g. 4
                    // for i32) is NOT the storage width; using it here allocated
                    // only half the bytes and the worker then read out of bounds
                    // (heap OOB) past the malloc'd region. Storage stride is
                    // always 8 bytes, matching the sibling string/nested-list
                    // clone paths below.
                    let elem_size = i64_ty.const_int(8, false);
                    let total = self
                        .generator
                        .builder
                        .build_int_mul(len, elem_size, "spawn_list_bytes")
                        .map_err(|e| CompileError::LlvmError(format!("spawn list size: {e}")))?;
                    let clone_data = self.generator.malloc_or_abort(total, "spawn_list_heap")?;
                    let memcpy_fn = self.generator.get_runtime_fn("memcpy")?;
                    self.generator
                        .builder
                        .build_call(
                            memcpy_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(clone_data),
                                BasicMetadataValueEnum::PointerValue(data),
                                BasicMetadataValueEnum::IntValue(total),
                            ],
                            "spawn_list_copy",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("spawn list copy: {e}")))?;
                    let list_ty = arg.get_type().into_struct_type();
                    let list_val = list_ty.get_undef();
                    let list_val = self
                        .generator
                        .builder
                        .build_insert_value(list_val, len, 0, "spawn_list_with_len")
                        .map_err(|e| CompileError::LlvmError(format!("spawn list build: {e}")))?;
                    let list_val = self
                        .generator
                        .builder
                        .build_insert_value(list_val, clone_data, 1, "spawn_list_with_data")
                        .map_err(|e| CompileError::LlvmError(format!("spawn list build: {e}")))?;
                    self.generator.build_store(field_gep, list_val)?;
                } else if is_heap_struct_arg[i] {
                    let sv = (*arg).into_struct_value();
                    let mut struct_val = sv;
                    for path in &heap_struct_string_paths[i] {
                        let mut pairs = Vec::new();
                        let mut cur = sv;
                        for &idx in path {
                            pairs.push((cur, idx));
                            cur = self
                                .generator
                                .build_extract_value(cur.into(), idx, "spawn_heap_str_pair")?
                                .into_struct_value();
                        }
                        let str_sv = cur;
                        let data = self
                            .generator
                            .build_extract_value(str_sv.into(), 0, "spawn_heap_str_path_data_in")?
                            .into_pointer_value();
                        let len = self
                            .generator
                            .build_extract_value(str_sv.into(), 1, "spawn_heap_str_path_len_in")?
                            .into_int_value();
                        let clone_fn = self.generator.get_runtime_fn("mimi_str_clone")?;
                        let handle = self
                            .generator
                            .builder
                            .build_call(
                                clone_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(data),
                                    BasicMetadataValueEnum::IntValue(len),
                                ],
                                "spawn_heap_str_clone",
                            )
                            .map_err(|e| {
                                CompileError::LlvmError(format!("spawn heap str clone: {e}"))
                            })?
                            .try_as_basic_value_opt()
                            .ok_or_else(|| {
                                CompileError::LlvmError("spawn heap str clone returned void".into())
                            })?
                            .into_int_value();
                        let clone_ptr = self
                            .generator
                            .builder
                            .build_int_to_ptr(handle, ptr_ty, "spawn_heap_str_clone_ptr")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("spawn heap str ptr: {e}"))
                            })?;
                        let string_ty = str_sv.get_type();
                        let new_str = string_ty.get_undef();
                        let new_str = self
                            .generator
                            .builder
                            .build_insert_value(new_str, clone_ptr, 0, "spawn_heap_str_new_ptr")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("spawn heap str ptr ins: {e}"))
                            })?
                            .into_struct_value();
                        let new_str = self
                            .generator
                            .builder
                            .build_insert_value(new_str, len, 1, "spawn_heap_str_new_len")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("spawn heap str len ins: {e}"))
                            })?
                            .into_struct_value();
                        let mut rebuilt = new_str;
                        for (parent, parent_idx) in pairs.into_iter().rev() {
                            rebuilt = self
                                .generator
                                .builder
                                .build_insert_value(
                                    parent,
                                    rebuilt,
                                    parent_idx,
                                    "spawn_heap_str_rebuild",
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("spawn heap str rebuild: {e}"))
                                })?
                                .into_struct_value();
                        }
                        struct_val = rebuilt;
                    }
                    for path in &heap_struct_list_paths[i] {
                        let mut pairs = Vec::new();
                        let mut cur = sv;
                        for &idx in path {
                            pairs.push((cur, idx));
                            cur = self
                                .generator
                                .build_extract_value(cur.into(), idx, "spawn_heap_list_pair")?
                                .into_struct_value();
                        }
                        let list_sv = cur;
                        let len = self
                            .generator
                            .build_extract_value(list_sv.into(), 0, "spawn_heap_list_path_len_in")?
                            .into_int_value();
                        let data = self
                            .generator
                            .build_extract_value(list_sv.into(), 1, "spawn_heap_list_path_data_in")?
                            .into_pointer_value();
                        let total = self
                            .generator
                            .builder
                            .build_int_mul(len, i64_ty.const_int(8, false), "spawn_heap_list_bytes")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("spawn heap list size: {e}"))
                            })?;
                        let clone_data = self
                            .generator
                            .malloc_or_abort(total, "spawn_heap_list_clone")?;
                        let memcpy_fn = self.generator.get_runtime_fn("memcpy")?;
                        self.generator
                            .builder
                            .build_call(
                                memcpy_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(clone_data),
                                    BasicMetadataValueEnum::PointerValue(data),
                                    BasicMetadataValueEnum::IntValue(total),
                                ],
                                "spawn_heap_list_copy",
                            )
                            .map_err(|e| {
                                CompileError::LlvmError(format!("spawn heap list copy: {e}"))
                            })?;
                        let list_ty = list_sv.get_type();
                        let new_list = list_ty.get_undef();
                        let new_list = self
                            .generator
                            .builder
                            .build_insert_value(new_list, len, 0, "spawn_heap_list_new_len")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("spawn heap list len: {e}"))
                            })?
                            .into_struct_value();
                        let new_list = self
                            .generator
                            .builder
                            .build_insert_value(new_list, clone_data, 1, "spawn_heap_list_new_data")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("spawn heap list data: {e}"))
                            })?
                            .into_struct_value();
                        let mut rebuilt = new_list;
                        for (parent, parent_idx) in pairs.into_iter().rev() {
                            rebuilt = self
                                .generator
                                .builder
                                .build_insert_value(
                                    parent,
                                    rebuilt,
                                    parent_idx,
                                    "spawn_heap_list_rebuild",
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("spawn heap list rebuild: {e}"))
                                })?
                                .into_struct_value();
                        }
                        struct_val = rebuilt;
                    }
                    self.generator.build_store(field_gep, struct_val)?;
                } else {
                    self.generator.build_store(field_gep, *arg)?;
                }
            }
            let env_slot = self.generator.build_in_bounds_gep(
                i8_ty,
                future_ptr,
                &[i64_ty.const_int(RESOLVED_FUTURE_DATA_OFFSET, false)],
                "spawn_env_slot_i8",
            )?;
            let env_slot_ptr = self
                .generator
                .builder
                .build_bit_cast(env_slot, ptr_ty, "spawn_env_slot_ptr")
                .map_err(|e| CompileError::LlvmError(format!("spawn env slot cast: {e}")))?
                .into_pointer_value();
            self.generator
                .build_store(env_slot_ptr, BasicValueEnum::PointerValue(env_heap_ptr))?;
        } else {
            let env_slot = self.generator.build_in_bounds_gep(
                i8_ty,
                future_ptr,
                &[i64_ty.const_int(RESOLVED_FUTURE_DATA_OFFSET, false)],
                "spawn_null_slot_i8",
            )?;
            let env_slot_ptr = self
                .generator
                .builder
                .build_bit_cast(env_slot, ptr_ty, "spawn_null_slot_ptr")
                .map_err(|e| CompileError::LlvmError(format!("spawn null slot cast: {e}")))?
                .into_pointer_value();
            self.generator.build_store(
                env_slot_ptr,
                BasicValueEnum::PointerValue(ptr_ty.const_null()),
            )?;
        }

        let spawn_fn = self.generator.get_runtime_fn("mimi_spawn_future")?;
        let poll_fn_ptr = self
            .generator
            .builder
            .build_bit_cast(
                poll_fn.as_global_value().as_pointer_value(),
                BasicTypeEnum::PointerType(ptr_ty),
                "spawn_poll_fn_ptr",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn poll fn cast: {e}")))?
            .into_pointer_value();
        self.generator
            .builder
            .build_call(
                spawn_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(future_ptr),
                    BasicMetadataValueEnum::PointerValue(poll_fn_ptr),
                ],
                "spawn_future_thread",
            )
            .map_err(|e| CompileError::LlvmError(format!("spawn_future call: {e}")))?;

        Ok(Some(BasicValueEnum::PointerValue(future_ptr)))
    }

    /// Recursively transform string leaves inside a returned heap-owned
    /// value into malloc-owned string data. Top-level string returns already
    /// go through `claim_resolved_string_return`; records containing String
    /// fields need the same ownership probe so the caller's later
    /// `free`/`mimi_string_free` never touches a `.rodata` literal.
    /// Ensure every string element in a `List<string>` is owned by heap
    /// storage. The list's data array is mutated in place so the returned
    /// value remains valid and the caller can safely `mimi_string_free` each
    /// element during scope exit.
    fn ensure_list_string_owned(
        &mut self,
        list_sv: inkwell::values::StructValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i64_ty = self.generator.context.i64_type();
        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());
        let len = self
            .generator
            .builder
            .build_extract_value(list_sv, 0, "ret_list_str_len")
            .map_err(|e| CompileError::LlvmError(format!("ret list str len: {e}")))?
            .into_int_value();
        let data = self
            .generator
            .builder
            .build_extract_value(list_sv, 1, "ret_list_str_data")
            .map_err(|e| CompileError::LlvmError(format!("ret list str data: {e}")))?
            .into_pointer_value();
        let function = self.generator.current_function().ok_or_else(|| {
            CompileError::LlvmError("ret list str ensure outside function".into())
        })?;
        let header = self
            .generator
            .context
            .append_basic_block(function, "ret_list_str_header");
        let body = self
            .generator
            .context
            .append_basic_block(function, "ret_list_str_body");
        let exit = self
            .generator
            .context
            .append_basic_block(function, "ret_list_str_exit");
        let idx_storage = self
            .generator
            .build_alloca(BasicTypeEnum::IntType(i64_ty), "ret_list_str_idx")?;
        self.generator
            .build_store(idx_storage, i64_ty.const_int(0, false))?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(header);
        let idx = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                idx_storage,
                "ret_list_str_idx_val",
            )?
            .into_int_value();
        let cond = self.generator.builder.build_int_compare(
            inkwell::IntPredicate::SLT,
            idx,
            len,
            "ret_list_str_cond",
        );
        let cond = cond.map_err(|e| CompileError::LlvmError(format!("ret list str cmp: {e}")))?;
        self.generator.build_cond_br(cond, body, exit)?;

        self.generator.builder.position_at_end(body);
        let elem_slot =
            self.generator
                .build_in_bounds_gep(i64_ty, data, &[idx], "ret_list_str_elem_slot")?;
        let elem_i64 = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                elem_slot,
                "ret_list_str_elem_i64",
            )?
            .into_int_value();
        let elem_ptr =
            self.generator
                .build_int_to_ptr(elem_i64, ptr_ty, "ret_list_str_elem_ptr")?;
        // 0.1.8 Phase B fat ABI: each list slot is a MimiStr box handle.
        // Unpack the box to `{ptr, len}`, ensure the payload is heap-owned,
        // then update the box's `ptr`/`len` fields in place. The slot must
        // continue to hold the box pointer (not a raw string pointer).
        let string_sv = self
            .generator
            .load_fat_list_string(elem_ptr)?
            .into_struct_value();
        let owned = self
            .generator
            .claim_resolved_string_return(string_sv.into())?
            .into_struct_value();
        let new_ptr = self
            .generator
            .build_extract_value(owned.into(), 0, "ret_list_str_owned_ptr")?
            .into_pointer_value();
        let new_len = self
            .generator
            .build_extract_value(owned.into(), 1, "ret_list_str_owned_len")?
            .into_int_value();
        let i32_ty = self.generator.context.i32_type();
        let fat_ty = self.generator.context.struct_type(
            &[
                BasicTypeEnum::IntType(i32_ty),
                BasicTypeEnum::IntType(i32_ty),
                BasicTypeEnum::PointerType(ptr_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let fat_ptr_gep = self
            .generator
            .gep()
            .build_struct_gep(fat_ty, elem_ptr, 2, "ret_list_str_box_ptr")
            .map_err(|e| CompileError::LlvmError(format!("ret list str box ptr: {e}")))?;
        self.generator.build_store(fat_ptr_gep, new_ptr)?;
        let fat_len_gep = self
            .generator
            .gep()
            .build_struct_gep(fat_ty, elem_ptr, 3, "ret_list_str_box_len")
            .map_err(|e| CompileError::LlvmError(format!("ret list str box len: {e}")))?;
        self.generator.build_store(fat_len_gep, new_len)?;
        let next = self
            .generator
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "ret_list_str_idx_next")
            .map_err(|e| CompileError::LlvmError(format!("ret list str inc: {e}")))?;
        self.generator.build_store(idx_storage, next)?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(exit);
        Ok(list_sv.into())
    }

    /// Ensure every inner `List<string>` in a `List<List<string>>` has
    /// heap-owned string elements. The outer data array contains heap box
    /// handles; each inner box is mutated in place.
    fn ensure_string_list_list_owned(
        &mut self,
        list_sv: inkwell::values::StructValue<'ctx>,
        elem_list_ty: inkwell::types::StructType<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i64_ty = self.generator.context.i64_type();
        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());
        let len = self
            .generator
            .builder
            .build_extract_value(list_sv, 0, "ret_lsl_len")
            .map_err(|e| CompileError::LlvmError(format!("ret lsl len: {e}")))?
            .into_int_value();
        let data = self
            .generator
            .builder
            .build_extract_value(list_sv, 1, "ret_lsl_data")
            .map_err(|e| CompileError::LlvmError(format!("ret lsl data: {e}")))?
            .into_pointer_value();
        let function = self
            .generator
            .current_function()
            .ok_or_else(|| CompileError::LlvmError("ret lsl ensure outside function".into()))?;
        let header = self
            .generator
            .context
            .append_basic_block(function, "ret_lsl_header");
        let body = self
            .generator
            .context
            .append_basic_block(function, "ret_lsl_body");
        let exit = self
            .generator
            .context
            .append_basic_block(function, "ret_lsl_exit");
        let idx_storage = self
            .generator
            .build_alloca(BasicTypeEnum::IntType(i64_ty), "ret_lsl_idx")?;
        self.generator
            .build_store(idx_storage, i64_ty.const_int(0, false))?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(header);
        let idx = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                idx_storage,
                "ret_lsl_idx_val",
            )?
            .into_int_value();
        let cond = self
            .generator
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, idx, len, "ret_lsl_cond")
            .map_err(|e| CompileError::LlvmError(format!("ret lsl cmp: {e}")))?;
        self.generator.build_cond_br(cond, body, exit)?;

        self.generator.builder.position_at_end(body);
        let elem_slot =
            self.generator
                .build_in_bounds_gep(i64_ty, data, &[idx], "ret_lsl_elem_slot")?;
        let inner_handle = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                elem_slot,
                "ret_lsl_inner_handle",
            )?
            .into_int_value();
        let inner_ptr =
            self.generator
                .build_int_to_ptr(inner_handle, ptr_ty, "ret_lsl_inner_ptr")?;
        let inner_sv = self
            .generator
            .build_load(
                BasicTypeEnum::StructType(elem_list_ty),
                inner_ptr,
                "ret_lsl_inner",
            )?
            .into_struct_value();
        let owned_inner = self.ensure_list_string_owned(inner_sv)?.into_struct_value();
        self.generator.build_store(inner_ptr, owned_inner)?;
        let next = self
            .generator
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "ret_lsl_idx_next")
            .map_err(|e| CompileError::LlvmError(format!("ret lsl inc: {e}")))?;
        self.generator.build_store(idx_storage, next)?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(exit);
        Ok(list_sv.into())
    }

    /// Recursively ensure strings inside a returned value are heap-owned.
    /// The optional resolved type lets list containers convert their element
    /// string pointers (which are raw `char*` handles in list data arrays)
    /// before the caller frees them.
    fn ensure_returned_heap_strings_owned(
        &mut self,
        value: BasicValueEnum<'ctx>,
        ty: BasicTypeEnum<'ctx>,
        type_id: Option<ResolvedTypeId>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if let Some(type_id) = type_id.as_ref() {
            if let Some(ResolvedType::Nominal {
                item, arguments, ..
            }) = self.program.resolved_types().get(type_id)
            {
                if item.as_str() == "builtin:type:List"
                    && arguments.len() == 1
                    && matches!(
                        self.program.resolved_types().get(&arguments[0]),
                        Some(ResolvedType::Primitive(PrimitiveType::String))
                    )
                {
                    if let (BasicValueEnum::StructValue(sv), BasicTypeEnum::StructType(_)) =
                        (value, ty)
                    {
                        return self.ensure_list_string_owned(sv);
                    }
                }
                if item.as_str() == "builtin:type:List"
                    && arguments.len() == 1
                    && matches!(
                        self.program.resolved_types().get(&arguments[0]),
                        Some(ResolvedType::Nominal {
                            item,
                            arguments,
                            ..
                        }) if item.as_str() == "builtin:type:List"
                            && arguments.len() == 1
                            && matches!(
                                self.program.resolved_types().get(&arguments[0]),
                                Some(ResolvedType::Primitive(PrimitiveType::String))
                            )
                    )
                {
                    if let (BasicValueEnum::StructValue(sv), BasicTypeEnum::StructType(_)) =
                        (value, ty)
                    {
                        if let BasicTypeEnum::StructType(elem_list_ty) =
                            self.lower_type(&arguments[0])?
                        {
                            return self.ensure_string_list_list_owned(sv, elem_list_ty);
                        }
                    }
                }
            }
        }
        match (value, ty) {
            (BasicValueEnum::StructValue(sv), BasicTypeEnum::StructType(st))
                if st.get_field_types().len() == 2
                    && matches!(st.get_field_types()[0], BasicTypeEnum::PointerType(_))
                    && matches!(
                        st.get_field_types()[1],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                    ) =>
            {
                // Adopted StringBox/product returns bypass this legacy
                // recursive pass entirely. Reaching this branch therefore
                // means the enclosing shape is still on the old path (for
                // example Option/List); normalize its leaf exactly once.
                self.generator.claim_resolved_string_return(sv.into())
            }
            (BasicValueEnum::StructValue(sv), BasicTypeEnum::StructType(st)) => {
                let field_displays: Option<Vec<String>> = if let Some(type_id) = type_id.as_ref() {
                    if let Some(ResolvedType::Nominal { item, .. }) =
                        self.program.resolved_types().get(type_id)
                    {
                        let item_str = item.as_str();
                        let type_name = item_str.strip_prefix("type:").unwrap_or(item_str);
                        self.program
                            .type_defs()
                            .values()
                            .find(|td| {
                                td.qualified_name == type_name || td.qualified_name == item_str
                            })
                            .map(|td| td.fields.iter().map(|(_, d)| d.clone()).collect())
                    } else {
                        None
                    }
                } else {
                    None
                };
                let mut rebuilt = sv;
                for i in 0..st.count_fields() {
                    let field_ty = st.get_field_types()[i as usize];
                    let field = self.generator.build_extract_value(
                        sv.into(),
                        i,
                        "ret_heap_str_field_in",
                    )?;
                    let child_id = field_displays
                        .as_ref()
                        .and_then(|v| v.get(i as usize))
                        .and_then(|d| self.resolved_type_id_by_display(d));
                    let field =
                        self.ensure_returned_heap_strings_owned(field, field_ty, child_id)?;
                    rebuilt = self
                        .generator
                        .builder
                        .build_insert_value(rebuilt, field, i, "ret_heap_str_field_owned")
                        .map_err(|e| {
                            CompileError::LlvmError(format!(
                                "return record string ownership rebuild: {e}"
                            ))
                        })?
                        .into_struct_value();
                }
                Ok(rebuilt.into())
            }
            (other, _) => Ok(other),
        }
    }

    /// 0.39.x (L1 parity fix): give the returned `List<string>` private,
    /// heap-owned copies of every element payload. Legacy-monomorphized
    /// instances build list literals with `mimi_str_box`, which boxes
    /// BORROWED pointers (argument aliases, `.rodata` literals), while the
    /// caller-side `register_returned_string_list` frees every element box
    /// payload unconditionally at scope exit — freeing memory the list never
    /// owned (double-free / free-of-global). Unlike
    /// `ensure_list_string_owned` this is NOT a probe: the list's destructor
    /// is unconditional, so each payload must become a private copy.
    fn copy_string_list_elements_owned(
        &mut self,
        list_sv: inkwell::values::StructValue<'ctx>,
    ) -> Result<(), CompileError> {
        let i64_ty = self.generator.context.i64_type();
        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());
        let len = self
            .generator
            .builder
            .build_extract_value(list_sv, 0, "own_list_str_len")
            .map_err(|e| CompileError::LlvmError(format!("own list str len: {e}")))?
            .into_int_value();
        let data = self
            .generator
            .builder
            .build_extract_value(list_sv, 1, "own_list_str_data")
            .map_err(|e| CompileError::LlvmError(format!("own list str data: {e}")))?
            .into_pointer_value();
        let function = self.generator.current_function().ok_or_else(|| {
            CompileError::LlvmError("own list str elements outside function".into())
        })?;
        let header = self
            .generator
            .context
            .append_basic_block(function, "own_list_str_header");
        let body = self
            .generator
            .context
            .append_basic_block(function, "own_list_str_body");
        let exit = self
            .generator
            .context
            .append_basic_block(function, "own_list_str_exit");
        let idx_storage = self
            .generator
            .build_alloca(BasicTypeEnum::IntType(i64_ty), "own_list_str_idx")?;
        self.generator
            .build_store(idx_storage, i64_ty.const_int(0, false))?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(header);
        let idx = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                idx_storage,
                "own_list_str_idx_val",
            )?
            .into_int_value();
        let cond = self.generator.builder.build_int_compare(
            inkwell::IntPredicate::SLT,
            idx,
            len,
            "own_list_str_cond",
        );
        let cond = cond.map_err(|e| CompileError::LlvmError(format!("own list str cmp: {e}")))?;
        self.generator.build_cond_br(cond, body, exit)?;

        self.generator.builder.position_at_end(body);
        let elem_slot =
            self.generator
                .build_in_bounds_gep(i64_ty, data, &[idx], "own_list_str_elem_slot")?;
        let elem_i64 = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                elem_slot,
                "own_list_str_elem_i64",
            )?
            .into_int_value();
        let elem_ptr = self
            .generator
            .builder
            .build_int_to_ptr(elem_i64, ptr_ty, "own_list_str_elem_ptr")
            .map_err(|e| CompileError::LlvmError(format!("own list str elem ptr: {e}")))?;
        // Fat box layout {i32, i32, ptr, i64}: fields 2/3 are the payload
        // {ptr, len}. Replace the payload with a fresh heap copy; the box
        // itself stays in place (the slot must keep holding the box pointer).
        let string_sv = self
            .generator
            .load_fat_list_string(elem_ptr)?
            .into_struct_value();
        let owned = self
            .generator
            .heap_copy_string_value(string_sv.into())?
            .into_struct_value();
        let new_ptr = self
            .generator
            .build_extract_value(owned.into(), 0, "own_list_str_copy_ptr")?
            .into_pointer_value();
        let new_len = self
            .generator
            .build_extract_value(owned.into(), 1, "own_list_str_copy_len")?
            .into_int_value();
        let i32_ty = self.generator.context.i32_type();
        let fat_ty = self.generator.context.struct_type(
            &[
                BasicTypeEnum::IntType(i32_ty),
                BasicTypeEnum::IntType(i32_ty),
                BasicTypeEnum::PointerType(ptr_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let fat_ptr_gep = self
            .generator
            .gep()
            .build_struct_gep(fat_ty, elem_ptr, 2, "own_list_str_box_ptr")
            .map_err(|e| CompileError::LlvmError(format!("own list str box ptr: {e}")))?;
        self.generator.build_store(fat_ptr_gep, new_ptr)?;
        let fat_len_gep = self
            .generator
            .gep()
            .build_struct_gep(fat_ty, elem_ptr, 3, "own_list_str_box_len")
            .map_err(|e| CompileError::LlvmError(format!("own list str box len: {e}")))?;
        self.generator.build_store(fat_len_gep, new_len)?;
        let next = self
            .generator
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "own_list_str_idx_next")
            .map_err(|e| CompileError::LlvmError(format!("own list str inc: {e}")))?;
        self.generator.build_store(idx_storage, next)?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(exit);
        Ok(())
    }

    /// 0.39.x (L1 parity fix): same ownership normalization as
    /// [`Self::copy_string_list_elements_owned`] for `List<List<string>>`
    /// values returned by legacy-monomorphized instances: walk the outer
    /// slots, load each inner list box, and give every inner element payload
    /// a private heap copy so the scope-exit `StringListListData` teardown
    /// (inner boxes + payloads + arrays) never frees borrowed memory.
    fn copy_string_list_list_elements_owned(
        &mut self,
        outer_sv: inkwell::values::StructValue<'ctx>,
        elem_list_ty: inkwell::types::StructType<'ctx>,
    ) -> Result<(), CompileError> {
        let i64_ty = self.generator.context.i64_type();
        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());
        let len = self
            .generator
            .builder
            .build_extract_value(outer_sv, 0, "own_llist_len")
            .map_err(|e| CompileError::LlvmError(format!("own llist len: {e}")))?
            .into_int_value();
        let data = self
            .generator
            .builder
            .build_extract_value(outer_sv, 1, "own_llist_data")
            .map_err(|e| CompileError::LlvmError(format!("own llist data: {e}")))?
            .into_pointer_value();
        let function = self
            .generator
            .current_function()
            .ok_or_else(|| CompileError::LlvmError("own llist elements outside function".into()))?;
        let header = self
            .generator
            .context
            .append_basic_block(function, "own_llist_header");
        let body = self
            .generator
            .context
            .append_basic_block(function, "own_llist_body");
        let exit = self
            .generator
            .context
            .append_basic_block(function, "own_llist_exit");
        let idx_storage = self
            .generator
            .build_alloca(BasicTypeEnum::IntType(i64_ty), "own_llist_idx")?;
        self.generator
            .build_store(idx_storage, i64_ty.const_int(0, false))?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(header);
        let idx = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                idx_storage,
                "own_llist_idx_val",
            )?
            .into_int_value();
        let cond = self.generator.builder.build_int_compare(
            inkwell::IntPredicate::SLT,
            idx,
            len,
            "own_llist_cond",
        );
        let cond = cond.map_err(|e| CompileError::LlvmError(format!("own llist cmp: {e}")))?;
        self.generator.build_cond_br(cond, body, exit)?;

        self.generator.builder.position_at_end(body);
        let elem_slot =
            self.generator
                .build_in_bounds_gep(i64_ty, data, &[idx], "own_llist_elem_slot")?;
        let inner_handle = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                elem_slot,
                "own_llist_inner_handle",
            )?
            .into_int_value();
        let inner_ptr = self
            .generator
            .builder
            .build_int_to_ptr(inner_handle, ptr_ty, "own_llist_inner_ptr")
            .map_err(|e| CompileError::LlvmError(format!("own llist inner ptr: {e}")))?;
        let inner_list_sv = self
            .generator
            .build_load(
                BasicTypeEnum::StructType(elem_list_ty),
                inner_ptr,
                "own_llist_inner",
            )?
            .into_struct_value();
        self.copy_string_list_elements_owned(inner_list_sv)?;
        let next = self
            .generator
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "own_llist_idx_next")
            .map_err(|e| CompileError::LlvmError(format!("own llist inc: {e}")))?;
        self.generator.build_store(idx_storage, next)?;
        self.generator.build_br(header)?;

        self.generator.builder.position_at_end(exit);
        Ok(())
    }

    /// Is this resolved type `builtin:type:List<builtin:type:List<String>>`?
    fn string_list_list_shape(&self, id: &ResolvedTypeId) -> bool {
        matches!(
            self.program.resolved_types().get(id),
            Some(ResolvedType::Nominal {
                item,
                arguments,
                ..
            }) if item.as_str() == "builtin:type:List"
                && arguments.len() == 1
                && matches!(
                    self.program.resolved_types().get(&arguments[0]),
                    Some(ResolvedType::Nominal {
                        item: inner_item,
                        arguments: inner_args,
                        ..
                    }) if inner_item.as_str() == "builtin:type:List"
                        && inner_args.len() == 1
                        && matches!(
                            self.program.resolved_types().get(&inner_args[0]),
                            Some(ResolvedType::Primitive(PrimitiveType::String))
                        )
                )
        )
    }

    /// Find a canonical `ResolvedTypeId` whose display name matches a
    /// `TypeDef` field display (e.g. `"List<string>"` or `"Inner"`).
    fn resolved_type_id_by_display(&self, display: &str) -> Option<ResolvedTypeId> {
        let normalized = display.replace(' ', "");
        self.program.resolved_types().iter().find_map(|(id, _)| {
            let name = resolved_type_display_name(self.program, id);
            if name.replace(' ', "") == normalized {
                Some(id.clone())
            } else {
                None
            }
        })
    }

    /// Register every heap pointer inside a function-call result with the
    /// caller's heap scope. Resolved calls do not currently share the legacy
    /// emitter's `track_string_return_lifetime` / list-return ownership
    /// paths; without this, returned String, List, and heap-field Records
    /// leak when the caller reassigns the value across many iterations.
    ///
    /// The optional resolved type is used to special-case `List<string>`
    /// (top-level and direct Record fields) so each string element is freed
    /// before the list data array.
    fn track_returned_heap_pointers(
        &self,
        value: BasicValueEnum<'ctx>,
        ty: BasicTypeEnum<'ctx>,
        type_id: Option<ResolvedTypeId>,
    ) -> Result<(), CompileError> {
        if let Some(type_id) = type_id.as_ref() {
            let ownership =
                crate::codegen::abi::ownership::classify_resolved(self.program, type_id);
            if self
                .generator
                .register_returned_value_with_derived_glue(&ownership, value)?
            {
                return Ok(());
            }
        }
        if let Some(type_id) = type_id.as_ref() {
            if let Some(ResolvedType::Nominal {
                item, arguments, ..
            }) = self.program.resolved_types().get(type_id)
            {
                if item.as_str() == "builtin:type:List"
                    && arguments.len() == 1
                    && matches!(
                        self.program.resolved_types().get(&arguments[0]),
                        Some(ResolvedType::Primitive(PrimitiveType::String))
                    )
                {
                    if let (BasicValueEnum::StructValue(sv), BasicTypeEnum::StructType(list_ty)) =
                        (value, ty)
                    {
                        self.generator.register_returned_string_list(sv, list_ty)?;
                    }
                    return Ok(());
                }
                if item.as_str() == "builtin:type:List"
                    && arguments.len() == 1
                    && matches!(
                        self.program.resolved_types().get(&arguments[0]),
                        Some(ResolvedType::Nominal {
                            item,
                            arguments,
                            ..
                        }) if item.as_str() == "builtin:type:List"
                            && arguments.len() == 1
                            && matches!(
                                self.program.resolved_types().get(&arguments[0]),
                                Some(ResolvedType::Primitive(PrimitiveType::String))
                            )
                    )
                {
                    if let (BasicValueEnum::StructValue(sv), BasicTypeEnum::StructType(list_ty)) =
                        (value, ty)
                    {
                        if let BasicTypeEnum::StructType(elem_list_ty) =
                            self.lower_type(&arguments[0])?
                        {
                            self.generator.register_returned_string_list_list(
                                sv,
                                list_ty,
                                elem_list_ty,
                            )?;
                        }
                    }
                    return Ok(());
                }
            }
        }
        match (value, ty) {
            (BasicValueEnum::PointerValue(pv), _) => {
                self.generator.register_heap_alloc(pv);
            }
            (BasicValueEnum::StructValue(sv), BasicTypeEnum::StructType(st)) => {
                let field_displays: Option<Vec<String>> = if let Some(type_id) = type_id.as_ref() {
                    if let Some(ResolvedType::Nominal { item, .. }) =
                        self.program.resolved_types().get(type_id)
                    {
                        let item_str = item.as_str();
                        let type_name = item_str.strip_prefix("type:").unwrap_or(item_str);
                        self.program
                            .type_defs()
                            .values()
                            .find(|td| {
                                td.qualified_name == type_name || td.qualified_name == item_str
                            })
                            .map(|td| td.fields.iter().map(|(_, d)| d.clone()).collect())
                    } else {
                        None
                    }
                } else {
                    None
                };
                for i in 0..st.count_fields() {
                    let field_ty = st.get_field_types()[i as usize];
                    let field =
                        self.generator
                            .build_extract_value(sv.into(), i, "call_heap_ret_field")?;
                    if let Some(display) = field_displays.as_ref().and_then(|v| v.get(i as usize)) {
                        if display.replace(' ', "") == "List<string>" {
                            if let (
                                BasicValueEnum::StructValue(fsv),
                                BasicTypeEnum::StructType(flt),
                            ) = (field, field_ty)
                            {
                                self.generator.register_returned_string_list(fsv, flt)?;
                                continue;
                            }
                        } else {
                            let child_id = self.resolved_type_id_by_display(display);
                            self.track_returned_heap_pointers(field, field_ty, child_id)?;
                            continue;
                        }
                    }
                    self.track_returned_heap_pointers(field, field_ty, None)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Claim every heap pointer inside a returned resolved value. The
    /// early-return deterministic drop uses `flush_heap_scopes_to_boundary`;
    /// claiming the exact data pointers carried by the returned value keeps
    /// ownership transfer intact while still freeing all other locals.
    /// `List<string>` values additionally register their element string
    /// pointers as claimed so the flush does not free strings that the caller
    /// will own.
    fn claim_returned_heap_pointers(
        &self,
        value: BasicValueEnum<'ctx>,
        ty: BasicTypeEnum<'ctx>,
        type_id: Option<ResolvedTypeId>,
    ) -> Result<(), CompileError> {
        if let Some(type_id) = type_id.as_ref() {
            if let Some(ResolvedType::Nominal {
                item, arguments, ..
            }) = self.program.resolved_types().get(type_id)
            {
                if item.as_str() == "builtin:type:List"
                    && arguments.len() == 1
                    && matches!(
                        self.program.resolved_types().get(&arguments[0]),
                        Some(ResolvedType::Primitive(PrimitiveType::String))
                    )
                {
                    if let (BasicValueEnum::StructValue(sv), BasicTypeEnum::StructType(list_ty)) =
                        (value, ty)
                    {
                        self.generator.claim_returned_string_list(sv, list_ty)?;
                    }
                }
                if item.as_str() == "builtin:type:List"
                    && arguments.len() == 1
                    && matches!(
                        self.program.resolved_types().get(&arguments[0]),
                        Some(ResolvedType::Nominal {
                            item,
                            arguments,
                            ..
                        }) if item.as_str() == "builtin:type:List"
                            && arguments.len() == 1
                            && matches!(
                                self.program.resolved_types().get(&arguments[0]),
                                Some(ResolvedType::Primitive(PrimitiveType::String))
                            )
                    )
                {
                    if let (BasicValueEnum::StructValue(sv), BasicTypeEnum::StructType(list_ty)) =
                        (value, ty)
                    {
                        if let BasicTypeEnum::StructType(elem_list_ty) =
                            self.lower_type(&arguments[0])?
                        {
                            self.generator.claim_returned_string_list_list(
                                sv,
                                list_ty,
                                elem_list_ty,
                            )?;
                        }
                    }
                }
                // 0.39.x L1 (E0722 family): a returned generic `List<T>` whose
                // element type is neither a string nor a nested `List<string>`
                // owns exactly one heap buffer — its i64 data array. Claim it
                // (via `claim_returned_generic_list`) so the early-return flush
                // transfers that buffer's ownership to the caller. This mirrors
                // `claim_returned_string_list` for `List<string>`, but here the
                // data array itself is the owned payload (a generic list has no
                // per-element heap pointers to claim), so `emit_generic_list_contains`
                // matches the data-array pointer directly.
                if item.as_str() == "builtin:type:List" && arguments.len() == 1 {
                    let elem = self.program.resolved_types().get(&arguments[0]);
                    let elem_is_string =
                        matches!(elem, Some(ResolvedType::Primitive(PrimitiveType::String)));
                    let elem_is_nested_string = matches!(
                        elem,
                        Some(ResolvedType::Nominal { item: eitem, arguments: eargs, .. })
                            if eitem.as_str() == "builtin:type:List"
                                && eargs.len() == 1
                                && matches!(
                                    self.program.resolved_types().get(&eargs[0]),
                                    Some(ResolvedType::Primitive(PrimitiveType::String))
                                )
                    );
                    if !elem_is_string && !elem_is_nested_string {
                        if let (
                            BasicValueEnum::StructValue(sv),
                            BasicTypeEnum::StructType(list_ty),
                        ) = (value, ty)
                        {
                            self.generator.claim_returned_generic_list(sv, list_ty)?;
                        }
                    }
                }
            }
        }
        match (value, ty) {
            (BasicValueEnum::PointerValue(pv), _) => {
                self.generator.claim_closure_env(pv);
            }
            (BasicValueEnum::StructValue(sv), BasicTypeEnum::StructType(st)) => {
                let field_displays: Option<Vec<String>> = if let Some(type_id) = type_id.as_ref() {
                    if let Some(ResolvedType::Nominal { item, .. }) =
                        self.program.resolved_types().get(type_id)
                    {
                        let item_str = item.as_str();
                        let type_name = item_str.strip_prefix("type:").unwrap_or(item_str);
                        self.program
                            .type_defs()
                            .values()
                            .find(|td| {
                                td.qualified_name == type_name || td.qualified_name == item_str
                            })
                            .map(|td| td.fields.iter().map(|(_, d)| d.clone()).collect())
                    } else {
                        None
                    }
                } else {
                    None
                };
                for i in 0..st.count_fields() {
                    let field_ty = st.get_field_types()[i as usize];
                    let field = self.generator.build_extract_value(
                        sv.into(),
                        i,
                        "return_heap_claim_field",
                    )?;
                    if let Some(display) = field_displays.as_ref().and_then(|v| v.get(i as usize)) {
                        if display.replace(' ', "") == "List<string>" {
                            if let (
                                BasicValueEnum::StructValue(fsv),
                                BasicTypeEnum::StructType(flt),
                            ) = (field, field_ty)
                            {
                                self.generator.claim_returned_string_list(fsv, flt)?;
                            }
                            // Also claim the list data pointer itself.
                            self.claim_returned_heap_pointers(field, field_ty, None)?;
                            continue;
                        }
                        let child_id = self.resolved_type_id_by_display(display);
                        self.claim_returned_heap_pointers(field, field_ty, child_id)?;
                        continue;
                    }
                    self.claim_returned_heap_pointers(field, field_ty, None)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    // NOTE (0.40.2, A1): return and capture ownership policy is derived from
    // `abi::ownership::OwnershipClass`; this emitter no longer maintains a
    // parallel AST/LLVM-shape ownership table.

    fn emit_spawn(
        &mut self,
        value: &ResolvedExpr,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // 0.37.x: real-thread spawn path for direct calls to named
        // functions. Set MIMI_EAGER_SPAWN=1 to force the older
        // eager/synchronous fallback for debugging. All other shapes remain
        // eager/synchronous.
        if let Some(real) = self.try_emit_spawn_thread(value, frame)? {
            return Ok(real);
        }
        let i8_ty = self.generator.context.i8_type();
        let i64_ty = self.generator.context.i64_type();
        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());

        // Run the expression synchronously (the future is completed before
        // the caller sees the handle).
        let inner_val = self.emit_expr(value, frame)?;
        let result_ty = inner_val.get_type();
        let result_bytes = self.generator.llvm_type_size_bytes(result_ty);
        let total_size = i64_ty.const_int(RESOLVED_FUTURE_DATA_OFFSET + result_bytes.max(8), false);

        let alloc_fn = self.generator.get_runtime_fn("mimi_future_alloc")?;
        let alloc = self
            .generator
            .builder
            .build_call(
                alloc_fn,
                &[BasicMetadataValueEnum::IntValue(total_size)],
                "spawn_future_alloc",
            )
            .map_err(|e| CompileError::LlvmError(format!("mimi_future_alloc: {e}")))?;
        let future_ptr = alloc
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("mimi_future_alloc returned void".into()))?
            .into_pointer_value();

        // Store the result at the future data region start (offset 16).
        let offset = i64_ty.const_int(RESOLVED_FUTURE_DATA_OFFSET, false);
        let data_i8 =
            self.generator
                .build_in_bounds_gep(i8_ty, future_ptr, &[offset], "spawn_result_i8")?;
        let data_ptr = self
            .generator
            .builder
            .build_bit_cast(data_i8, ptr_ty, "spawn_result_ptr")
            .map_err(|e| CompileError::LlvmError(format!("spawn result bitcast: {e}")))?
            .into_pointer_value();
        self.generator.build_store(data_ptr, inner_val)?;

        let set_fn = self.generator.get_runtime_fn("mimi_future_set_completed")?;
        self.generator
            .builder
            .build_call(
                set_fn,
                &[BasicMetadataValueEnum::PointerValue(future_ptr)],
                "spawn_set_completed",
            )
            .map_err(|e| CompileError::LlvmError(format!("mimi_future_set_completed: {e}")))?;

        Ok(BasicValueEnum::PointerValue(future_ptr))
    }

    /// Emit `await expr`: wait for a completed future and load its result.
    fn emit_await(
        &mut self,
        value: &ResolvedExpr,
        result_ty: &ResolvedTypeId,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i8_ty = self.generator.context.i8_type();
        let i64_ty = self.generator.context.i64_type();
        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());

        let handle_val = self.emit_expr(value, frame)?;
        let future_ptr = match handle_val {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => {
                return Err(CompileError::Unsupported(
                    "await operand is not a future pointer".into(),
                ))
            }
        };

        // Keep the same executor/await sequence as the legacy path so future
        // metadata and ownership behavior stay aligned.
        let executor_fn = self.generator.get_runtime_fn("mimi_executor_run")?;
        self.generator
            .builder
            .build_call(executor_fn, &[], "executor_run")
            .map_err(|e| CompileError::LlvmError(format!("mimi_executor_run: {e}")))?;

        let await_fn = self.generator.get_runtime_fn("mimi_await_future")?;
        self.generator
            .builder
            .build_call(
                await_fn,
                &[BasicMetadataValueEnum::PointerValue(future_ptr)],
                "await_future",
            )
            .map_err(|e| CompileError::LlvmError(format!("mimi_await_future: {e}")))?;

        let offset = i64_ty.const_int(RESOLVED_FUTURE_DATA_OFFSET, false);
        let data_i8 =
            self.generator
                .build_in_bounds_gep(i8_ty, future_ptr, &[offset], "await_result_i8")?;
        let data_ptr = self
            .generator
            .builder
            .build_bit_cast(data_i8, ptr_ty, "await_result_ptr")
            .map_err(|e| CompileError::LlvmError(format!("await result bitcast: {e}")))?
            .into_pointer_value();
        let result =
            self.generator
                .build_load(self.lower_type(result_ty)?, data_ptr, "future_result")?;

        let free_fn = self.generator.get_runtime_fn("mimi_future_free")?;
        self.generator
            .builder
            .build_call(
                free_fn,
                &[BasicMetadataValueEnum::PointerValue(future_ptr)],
                "future_free",
            )
            .map_err(|e| CompileError::LlvmError(format!("mimi_future_free: {e}")))?;

        Ok(result)
    }

    /// Emit a non-capturing lambda: generate a function + build closure struct.
    ///
    /// Closure struct layout: {ptr fn_ptr, ptr env_ptr} (matching legacy).
    /// Lambda function signature: (env_ptr: ptr, params...) -> ret.
    fn emit_lambda(
        &mut self,
        lambda: &crate::core::ir::ResolvedLambda,
        expr_ty: &ResolvedTypeId,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // Determine parameter and return LLVM types from the expression type.
        let (param_tys, ret_ty) = match self.program.resolved_types().get(expr_ty) {
            Some(ResolvedType::Function {
                parameters, result, ..
            }) => {
                let mut ptys = Vec::with_capacity(parameters.len());
                for p in parameters {
                    ptys.push(self.lower_type(p)?);
                }
                let rt = self.lower_type(result)?;
                (ptys, rt)
            }
            _ => {
                return Err(CompileError::Unsupported(
                    "lambda expression type is not a Function type".into(),
                ))
            }
        };

        // Build the LLVM function type: (ptr env, params...) -> ret.
        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());

        // Gather capture types from the enclosing frame before the lambda
        // body emitter switches to lambda_frame.
        let mut capture_tys: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(lambda.captures.len());
        for cap_id in &lambda.captures {
            let entry = frame.locals.get(cap_id).ok_or_else(|| {
                CompileError::Unsupported(format!(
                    "captured local '{}' not found in caller frame",
                    cap_id.0 .0
                ))
            })?;
            capture_tys.push(entry.llvm_type);
        }

        let mut fn_param_tys: Vec<BasicMetadataTypeEnum> =
            vec![BasicMetadataTypeEnum::PointerType(ptr_ty)];
        for pty in &param_tys {
            fn_param_tys.push(match *pty {
                BasicTypeEnum::IntType(t) => BasicMetadataTypeEnum::IntType(t),
                BasicTypeEnum::FloatType(t) => BasicMetadataTypeEnum::FloatType(t),
                BasicTypeEnum::PointerType(t) => BasicMetadataTypeEnum::PointerType(t),
                BasicTypeEnum::StructType(t) => BasicMetadataTypeEnum::StructType(t),
                BasicTypeEnum::ArrayType(t) => BasicMetadataTypeEnum::ArrayType(t),
                other => {
                    return Err(CompileError::Unsupported(format!(
                        "lambda param type {other:?} unsupported"
                    )))
                }
            });
        }
        let fn_type = match ret_ty {
            BasicTypeEnum::IntType(t) => t.fn_type(&fn_param_tys, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&fn_param_tys, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&fn_param_tys, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&fn_param_tys, false),
            other => {
                return Err(CompileError::Unsupported(format!(
                    "lambda return type {other:?} unsupported"
                )))
            }
        };

        // Generate a unique function name.
        let lambda_name = format!("__resolved_lambda_{}", self.generator.spawn_counter);
        self.generator.spawn_counter += 1;
        let lambda_fn = self
            .generator
            .module
            .add_function(&lambda_name, fn_type, None);
        let entry = self
            .generator
            .context
            .append_basic_block(lambda_fn, "entry");

        // Save current builder position.
        let saved_block = self.generator.builder.get_insert_block();
        self.generator.builder.position_at_end(entry);

        // Bind lambda parameters (skip param 0 = env_ptr).
        let callable_body = self
            .program
            .callable(&frame.owner)
            .ok_or_else(|| CompileError::Unsupported("callable absent for lambda".into()))?
            .body
            .clone();
        let mut lambda_frame = ResolvedFrame {
            owner: frame.owner.clone(),
            locals: BTreeMap::new(),
            old_snapshots: BTreeMap::new(),
        };
        for (i, local_id) in lambda.parameters.iter().enumerate() {
            let metadata = callable_body.locals.get(local_id).ok_or_else(|| {
                CompileError::Unsupported(format!("lambda param local '{}' absent", local_id.0 .0))
            })?;
            let llvm_ty = self.lower_type(&metadata.ty)?;
            let storage = self
                .generator
                .build_alloca(llvm_ty, &metadata.display_name)?;
            // Param index i+1 (0 is env_ptr).
            let param_val = lambda_fn
                .get_nth_param((i + 1) as u32)
                .ok_or_else(|| CompileError::Unsupported(format!("lambda param {i} missing")))?;
            let param_val = self.coerce_to(param_val, llvm_ty)?;
            self.generator.build_store(storage, param_val)?;
            lambda_frame.locals.insert(
                local_id.clone(),
                ResolvedVarEntry {
                    storage,
                    llvm_type: llvm_ty,
                },
            );
        }

        // Load captured variables from the env struct into local allocas.
        // The lambda body refers to captured variables by their original
        // ResolvedLocalId, so we insert matching entries into lambda_frame.
        if !lambda.captures.is_empty() {
            let env_struct_type = self.generator.context.struct_type(&capture_tys, false);
            let env_ptr = lambda_fn
                .get_nth_param(0)
                .ok_or_else(|| CompileError::Unsupported("lambda env ptr missing".into()))?
                .into_pointer_value();
            let env_struct_ptr = self.generator.build_pointer_cast(
                env_ptr,
                self.generator
                    .context
                    .ptr_type(inkwell::AddressSpace::default()),
                "env_struct",
            )?;
            let callable_body = self
                .program
                .callable(&frame.owner)
                .ok_or_else(|| CompileError::Unsupported("callable absent for lambda".into()))?
                .body
                .clone();
            for (i, cap_id) in lambda.captures.iter().enumerate() {
                let metadata = callable_body.locals.get(cap_id).ok_or_else(|| {
                    CompileError::Unsupported(format!(
                        "lambda capture local '{}' absent",
                        cap_id.0 .0
                    ))
                })?;
                let llvm_ty = self.lower_type(&metadata.ty)?;
                let field_gep = self
                    .generator
                    .builder
                    .build_struct_gep(env_struct_type, env_struct_ptr, i as u32, "env_cap_gep")
                    .map_err(|e| CompileError::LlvmError(format!("lambda env cap gep: {e}")))?;
                let field_val = self.generator.build_load(llvm_ty, field_gep, "cap_val")?;
                let storage = self
                    .generator
                    .build_alloca(llvm_ty, &metadata.display_name)?;
                self.generator.build_store(storage, field_val)?;
                lambda_frame.locals.insert(
                    cap_id.clone(),
                    ResolvedVarEntry {
                        storage,
                        llvm_type: llvm_ty,
                    },
                );
            }
        }

        // Emit the lambda body.
        self.generator.push_heap_scope();
        let body_val = self.emit_block(&callable_body, &lambda.body, &mut lambda_frame)?;
        if !self.current_block_terminated() {
            if let Some(val) = body_val {
                let val = self.coerce_to(val, ret_ty)?;
                // Deep-eval 2026-08-09 (demos/test_closure_call garbage):
                // string-returning closures must hand the caller a buffer
                // that OUTLIVES this scope's heap cleanup. The concat buffer
                // is registered in this scope, so returning it and then
                // freeing dangles the {ptr,len} value. Ownership probing
                // cannot help here (free_heap_allocs has no claim guard), so
                // heap-copy unconditionally BEFORE the cleanup — the copy
                // reads the source data first, then the cleanup releases the
                // original. Caller-side track_closure_return_lifetime
                // re-registers the copy for release.
                let val = self.generator.heap_copy_string_value(val)?;
                // Free heap allocations before returning (non-string returns
                // are scalar — they don't own heap data).
                let _ = self.generator.free_heap_allocs();
                self.generator.build_return(Some(&val))?;
            } else {
                let _ = self.generator.free_heap_allocs();
                // GENERIC-RET-ALIGN: unit lambdas lower to a non-void
                // signature slot; `ret void` is invalid IR that O1's CVP
                // crashes on. Return the signature's zero instead.
                let zero = self.generator.zero_value_for(ret_ty);
                self.generator.build_return(Some(&zero))?;
            }
        }

        // Restore builder position.
        if let Some(bb) = saved_block {
            self.generator.builder.position_at_end(bb);
        }

        // Build closure struct {fn_ptr, env_ptr}.
        let closure_ty = self.generator.context.struct_type(
            &[
                BasicTypeEnum::PointerType(ptr_ty),
                BasicTypeEnum::PointerType(ptr_ty),
            ],
            false,
        );
        let closure_alloca = self
            .generator
            .build_alloca(BasicTypeEnum::StructType(closure_ty), "closure")?;
        let fn_gep = self
            .generator
            .builder
            .build_struct_gep(closure_ty, closure_alloca, 0, "fn_gep")
            .map_err(|e| CompileError::LlvmError(format!("closure fn gep: {e}")))?;
        let fn_ptr_val = self
            .generator
            .builder
            .build_bit_cast(
                lambda_fn.as_global_value().as_pointer_value(),
                BasicTypeEnum::PointerType(ptr_ty),
                "fn_ptr_cast",
            )
            .map_err(|e| CompileError::LlvmError(format!("fn ptr cast: {e}")))?;
        self.generator.build_store(fn_gep, fn_ptr_val)?;
        let env_gep = self
            .generator
            .builder
            .build_struct_gep(closure_ty, closure_alloca, 1, "env_gep")
            .map_err(|e| CompileError::LlvmError(format!("closure env gep: {e}")))?;
        if lambda.captures.is_empty() {
            self.generator.build_store(env_gep, ptr_ty.const_null())?;
        } else {
            // Heap-allocate and populate the capture environment.
            let env_struct_type = self.generator.context.struct_type(&capture_tys, false);
            let env_size = env_struct_type
                .size_of()
                .ok_or_else(|| CompileError::Unsupported("closure env size_of failed".into()))?;
            let env_heap_ptr = self.generator.malloc_or_abort(env_size, "lambda_env")?;
            let env_ptr_i8 =
                self.generator
                    .build_pointer_cast(env_heap_ptr, ptr_ty, "lambda_env_i8")?;
            // The env is owned and released by the enclosing heap scope.
            self.generator.register_heap_alloc(env_ptr_i8);
            for (i, cap_id) in lambda.captures.iter().enumerate() {
                let entry = frame.locals.get(cap_id).ok_or_else(|| {
                    CompileError::Unsupported(format!(
                        "captured local '{}' not found in caller frame",
                        cap_id.0 .0
                    ))
                })?;
                let field_gep = self
                    .generator
                    .builder
                    .build_struct_gep(env_struct_type, env_heap_ptr, i as u32, "lambda_env_gep")
                    .map_err(|e| CompileError::LlvmError(format!("lambda env store gep: {e}")))?;
                let val =
                    self.generator
                        .build_load(entry.llvm_type, entry.storage, "capture_val")?;
                self.generator.build_store(field_gep, val)?;
            }
            self.generator.build_store(env_gep, env_ptr_i8)?;
        }
        self.generator.build_load(
            BasicTypeEnum::StructType(closure_ty),
            closure_alloca,
            "closure_val",
        )
    }

    /// Implement `reduce(list, fn, init)` for the resolved slice.
    ///
    /// The closure argument arrives as an already-constructed closure struct
    /// `{fn_ptr, env_ptr}`. We loop over the list, call the closure with
    /// `(env_ptr, acc, elem)`, and accumulate the result.
    fn emit_resolved_reduce(
        &mut self,
        call: &crate::core::ir::ResolvedCall,
        arguments: &[BasicMetadataValueEnum<'ctx>],
        _frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if arguments.len() != 3 {
            return Err(CompileError::Unsupported(
                "resolved reduce expects (list, closure, init)".into(),
            ));
        }
        let list_sv = match arguments[0] {
            BasicMetadataValueEnum::StructValue(sv) => sv,
            _ => {
                return Err(CompileError::Unsupported(
                    "resolved reduce first argument is not a list struct".into(),
                ))
            }
        };
        let len_val = self
            .generator
            .builder
            .build_extract_value(list_sv, 0, "reduce_len")
            .map_err(|e| CompileError::LlvmError(format!("reduce list len: {e}")))?
            .into_int_value();
        let data_ptr = self
            .generator
            .builder
            .build_extract_value(list_sv, 1, "reduce_data")
            .map_err(|e| CompileError::LlvmError(format!("reduce list data: {e}")))?
            .into_pointer_value();
        let closure_sv = match arguments[1] {
            BasicMetadataValueEnum::StructValue(sv) => sv,
            _ => {
                return Err(CompileError::Unsupported(
                    "resolved reduce second argument is not a closure struct".into(),
                ))
            }
        };
        let fn_ptr = self
            .generator
            .builder
            .build_extract_value(closure_sv, 0, "reduce_fn_ptr")
            .map_err(|e| CompileError::LlvmError(format!("reduce fn ptr: {e}")))?
            .into_pointer_value();
        let env_ptr = self
            .generator
            .builder
            .build_extract_value(closure_sv, 1, "reduce_env_ptr")
            .map_err(|e| CompileError::LlvmError(format!("reduce env ptr: {e}")))?
            .into_pointer_value();

        // Closure type: func(acc, elem) -> acc.
        let (acc_ty, elem_ty, ret_ty) = match self
            .program
            .resolved_types()
            .get(&call.arguments[1].value.ty)
        {
            Some(crate::core::ResolvedType::Function {
                parameters, result, ..
            }) if parameters.len() >= 2 => (
                self.lower_type(&parameters[0])?,
                self.lower_type(&parameters[1])?,
                self.lower_type(result)?,
            ),
            _ => {
                return Err(CompileError::Unsupported(
                    "resolved reduce closure type is not a two-parameter function".into(),
                ))
            }
        };

        let init_val: BasicValueEnum = match arguments[2] {
            BasicMetadataValueEnum::IntValue(iv) => iv.into(),
            BasicMetadataValueEnum::FloatValue(fv) => fv.into(),
            BasicMetadataValueEnum::PointerValue(pv) => pv.into(),
            BasicMetadataValueEnum::StructValue(sv) => sv.into(),
            _ => {
                return Err(CompileError::Unsupported(
                    "resolved reduce unsupported init value".into(),
                ))
            }
        };
        let init_adj = self.coerce_to(init_val, acc_ty)?;
        let acc_storage = self.generator.build_alloca(acc_ty, "reduce_acc")?;
        self.generator.build_store(acc_storage, init_adj)?;

        let i64_ty = self.generator.context.i64_type();
        let idx_storage = self
            .generator
            .build_alloca(BasicTypeEnum::IntType(i64_ty), "reduce_idx")?;
        self.generator
            .build_store(idx_storage, i64_ty.const_int(0, false))?;

        let function = self.current_function()?;
        let loop_bb = self
            .generator
            .context
            .append_basic_block(function, "reduce_loop");
        let body_bb = self
            .generator
            .context
            .append_basic_block(function, "reduce_body");
        let done_bb = self
            .generator
            .context
            .append_basic_block(function, "reduce_done");
        self.generator.build_br(loop_bb)?;

        self.generator.builder.position_at_end(loop_bb);
        let idx = self
            .generator
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                idx_storage,
                "reduce_idx_val",
            )?
            .into_int_value();
        let cond = self
            .generator
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, idx, len_val, "reduce_cond")
            .map_err(|e| CompileError::LlvmError(format!("reduce compare: {e}")))?;
        self.generator.build_cond_br(cond, body_bb, done_bb)?;

        self.generator.builder.position_at_end(body_bb);
        let elem_ptr =
            self.generator
                .build_in_bounds_gep(i64_ty, data_ptr, &[idx], "reduce_elem_ptr")?;
        let elem_i64 = self
            .generator
            .build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "reduce_elem_i64")?
            .into_int_value();
        let elem_val = self.convert_list_elem_i64(elem_i64, elem_ty)?;
        let acc_val = self
            .generator
            .build_load(acc_ty, acc_storage, "reduce_acc_val")?;

        let ptr_ty = self
            .generator
            .context
            .ptr_type(inkwell::AddressSpace::default());
        let all_meta: Vec<BasicMetadataTypeEnum> = vec![
            BasicMetadataTypeEnum::PointerType(ptr_ty),
            BasicMetadataTypeEnum::from(acc_ty),
            BasicMetadataTypeEnum::from(elem_ty),
        ];
        let indirect_fn_ty = match ret_ty {
            BasicTypeEnum::IntType(t) => t.fn_type(&all_meta, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&all_meta, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&all_meta, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&all_meta, false),
            _ => i64_ty.fn_type(&all_meta, false),
        };
        let call_args = [
            BasicMetadataValueEnum::PointerValue(env_ptr),
            BasicMetadataValueEnum::from(acc_val),
            BasicMetadataValueEnum::from(elem_val),
        ];
        let call_result = self
            .generator
            .builder
            .build_indirect_call(indirect_fn_ty, fn_ptr, &call_args, "reduce_call")
            .map_err(|e| CompileError::LlvmError(format!("reduce indirect call: {e}")))?;
        let reduced = call_result
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("reduce closure returned void".into()))?;
        let reduced_adj = self.coerce_to(reduced, acc_ty)?;
        self.generator.build_store(acc_storage, reduced_adj)?;
        let next = self
            .generator
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "reduce_next")
            .map_err(|e| CompileError::LlvmError(format!("reduce increment: {e}")))?;
        self.generator.build_store(idx_storage, next)?;
        self.generator.build_br(loop_bb)?;

        self.generator.builder.position_at_end(done_bb);
        self.generator
            .build_load(acc_ty, acc_storage, "reduce_result")
    }

    /// Look up the alphabetical ordinal of an enum variant by its NodeId.
    fn enum_variant_ordinal(&self, variant_id: &NodeId) -> Result<u64, CompileError> {
        let variant = self.program.resolved_variant(variant_id).ok_or_else(|| {
            CompileError::Unsupported(format!(
                "variant '{}' not found in resolved variant catalog",
                variant_id.0
            ))
        })?;
        let owner_td = self
            .program
            .type_defs()
            .values()
            .find(|td| td.node_id == variant.owner)
            .ok_or_else(|| {
                CompileError::Unsupported(format!(
                    "enum owner type for variant '{}' not found",
                    variant.name
                ))
            })?;
        let mut names: Vec<&str> = owner_td.variants.iter().map(|(n, _)| n.as_str()).collect();
        names.sort();
        names
            .iter()
            .position(|n| *n == variant.name)
            .map(|p| p as u64)
            .ok_or_else(|| {
                CompileError::Unsupported(format!(
                    "variant '{}' not in owner's variant list",
                    variant.name
                ))
            })
    }

    /// Construct a custom enum variant with payload: {i32 tag, i64 payload}.
    /// Used when Ok/Err/Some/None names refer to custom enum variants
    /// (not built-in Result/Option).
    fn emit_custom_enum_ctor(
        &mut self,
        variant_name: &str,
        call: &crate::core::ir::ResolvedCall,
        expression: &ResolvedExpr,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i32_ty = self.generator.context.i32_type();
        let i64_ty = self.generator.context.i64_type();

        // Find the ordinal of this variant in the custom enum.
        let ResolvedType::Nominal { item, .. } = self
            .program
            .resolved_types()
            .get(&expression.ty)
            .ok_or_else(|| CompileError::Unsupported("custom enum ctor: missing type".into()))?
        else {
            return Err(CompileError::Unsupported(
                "custom enum ctor: expression type is not Nominal".into(),
            ));
        };
        let item_str = item.as_str();
        let type_name = item_str.strip_prefix("type:").unwrap_or(item_str);
        let td = self
            .program
            .type_defs()
            .values()
            .find(|td| {
                (td.qualified_name == type_name || td.qualified_name == item_str)
                    && matches!(td.kind, crate::core::resolved::ResolvedTypeKind::Enum)
            })
            .ok_or_else(|| {
                CompileError::Unsupported(format!("custom enum type '{type_name}' not found"))
            })?;
        let mut variant_names: Vec<&str> = td.variants.iter().map(|(n, _)| n.as_str()).collect();
        variant_names.sort();
        let ordinal = variant_names
            .iter()
            .position(|n| *n == variant_name)
            .ok_or_else(|| {
                CompileError::Unsupported(format!(
                    "variant '{variant_name}' not found in enum '{type_name}'"
                ))
            })? as u64;

        // Encode the payload as i64.
        let payload_i64 = if call.arguments.is_empty() {
            // Unit variant (no payload).
            i64_ty.const_zero()
        } else {
            // Single-payload variant: emit the argument and coerce to i64.
            let arg_val = self.emit_expr(&call.arguments[0].value, frame)?;
            let arg_val = self.apply_conversion(arg_val, &call.arguments[0].conversion)?;
            // Deep-eval 2026-08-09 (demos/07 custom Res segv): a string
            // payload must follow the Packed convention shared with the
            // legacy variant ctors and the built-in Result<T,string> error
            // slot (Q1): heap-box the {ptr,len} struct and store
            // ptrtoint(box) in the payload slot. Encoding the raw data
            // pointer inline makes the match-side decode_payload_struct load
            // the string BYTES as a {ptr,len} struct (garbage display →
            // segv), and the caller-side tag-conditional box free would then
            // free the data pointer itself (free(.rodata literal) abort).
            if let BasicValueEnum::StructValue(sv) = arg_val {
                let fields = sv.get_type().get_field_types();
                let is_string_shape = fields.len() == 2
                    && matches!(fields[0], BasicTypeEnum::PointerType(_))
                    && matches!(fields[1], BasicTypeEnum::IntType(_));
                if is_string_shape {
                    let box_ptr = self
                        .generator
                        .malloc_or_abort(i64_ty.const_int(16, false), "enum_str_box")?;
                    self.generator.build_store(box_ptr, sv)?;
                    // Callee scope-exit frees the box unless the enum escapes
                    // via return: register it, then claim the pointer so the
                    // return path's free guard skips it (ownership transfers
                    // to the caller, whose EnumBox registration re-adopts the
                    // box with a tag-conditional free).
                    self.generator.register_heap_box(box_ptr);
                    self.generator.claim_closure_env(box_ptr);
                    let payload =
                        self.generator
                            .build_ptr_to_int(box_ptr, i64_ty, "enum_str_box_i")?;
                    let struct_ty = self.generator.context.struct_type(
                        &[
                            BasicTypeEnum::IntType(i32_ty),
                            BasicTypeEnum::IntType(i64_ty),
                        ],
                        false,
                    );
                    let mut result = struct_ty.get_undef();
                    result = self
                        .generator
                        .builder
                        .build_insert_value(result, i32_ty.const_int(ordinal, false), 0, "enum_tag")
                        .map_err(|e| CompileError::LlvmError(format!("enum tag insert: {e}")))?
                        .into_struct_value();
                    result = self
                        .generator
                        .builder
                        .build_insert_value(result, payload, 1, "enum_payload")
                        .map_err(|e| CompileError::LlvmError(format!("enum payload insert: {e}")))?
                        .into_struct_value();
                    return Ok(BasicValueEnum::StructValue(result));
                }
            }
            self.coerce_to_i64(arg_val)?
        };

        // Build {i32 tag, i64 payload} struct.
        let struct_ty = self.generator.context.struct_type(
            &[
                BasicTypeEnum::IntType(i32_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let mut result = struct_ty.get_undef();
        result = self
            .generator
            .builder
            .build_insert_value(result, i32_ty.const_int(ordinal, false), 0, "enum_tag")
            .map_err(|e| CompileError::LlvmError(format!("enum tag insert: {e}")))?
            .into_struct_value();
        result = self
            .generator
            .builder
            .build_insert_value(result, payload_i64, 1, "enum_payload")
            .map_err(|e| CompileError::LlvmError(format!("enum payload insert: {e}")))?
            .into_struct_value();
        Ok(BasicValueEnum::StructValue(result))
    }

    /// Construct an enum unit variant value: {i32 tag, i64 0}.
    /// The tag is the alphabetical ordinal of the variant within its
    /// owning type (matching the legacy emitter's register_type_def).
    fn emit_enum_unit_variant(
        &self,
        variant: &crate::core::resolved::ResolvedVariantSchema,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i32_ty = self.generator.context.i32_type();
        let i64_ty = self.generator.context.i64_type();

        // Find the ordinal: alphabetical index among the owner's variants.
        let owner_td = self
            .program
            .type_defs()
            .values()
            .find(|td| td.node_id == variant.owner)
            .ok_or_else(|| {
                CompileError::Unsupported(format!(
                    "enum owner type for variant '{}' not found in type_defs",
                    variant.name
                ))
            })?;
        let mut variant_names: Vec<&str> =
            owner_td.variants.iter().map(|(n, _)| n.as_str()).collect();
        variant_names.sort();
        let ordinal = variant_names
            .iter()
            .position(|n| *n == variant.name)
            .ok_or_else(|| {
                CompileError::Unsupported(format!(
                    "variant '{}' not found in owner's variant list",
                    variant.name
                ))
            })? as u64;

        // Build {i32 tag, i64 0} struct.
        let struct_ty = self.generator.context.struct_type(
            &[
                BasicTypeEnum::IntType(i32_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let tag_val = i32_ty.const_int(ordinal, false);
        let payload_val = i64_ty.const_zero();
        let mut result = struct_ty.get_undef();
        result = self
            .generator
            .builder
            .build_insert_value(result, tag_val, 0, "enum_tag")
            .map_err(|e| CompileError::LlvmError(format!("enum tag insert: {e}")))?
            .into_struct_value();
        result = self
            .generator
            .builder
            .build_insert_value(result, payload_val, 1, "enum_payload")
            .map_err(|e| CompileError::LlvmError(format!("enum payload insert: {e}")))?
            .into_struct_value();
        Ok(BasicValueEnum::StructValue(result))
    }

    fn bind_pattern_uninitialized(
        &mut self,
        body: &ResolvedBody,
        pattern: &ResolvedPattern,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<(), CompileError> {
        match &pattern.kind {
            ResolvedPatternKind::Wildcard => Ok(()),
            ResolvedPatternKind::Binding {
                local,
                by_reference: None,
            } => {
                let metadata = body.locals.get(local).ok_or_else(|| {
                    CompileError::Unsupported(format!(
                        "resolved binding local '{}' is absent",
                        local.0 .0
                    ))
                })?;
                let llvm_type = self.lower_type(&metadata.ty)?;
                let storage = self
                    .generator
                    .build_alloca(llvm_type, &metadata.display_name)?;
                frame
                    .locals
                    .insert(local.clone(), ResolvedVarEntry { storage, llvm_type });
                Ok(())
            }
            _ => Err(CompileError::Unsupported(format!(
                "resolved pattern '{}' escaped resolved native eligibility",
                pattern.node_id.0
            ))),
        }
    }
}

fn binary_op(op: ResolvedBinaryOp) -> BinOp {
    match op {
        ResolvedBinaryOp::Add => BinOp::Add,
        ResolvedBinaryOp::Subtract => BinOp::Sub,
        ResolvedBinaryOp::Multiply => BinOp::Mul,
        ResolvedBinaryOp::Divide => BinOp::Div,
        ResolvedBinaryOp::Remainder => BinOp::Mod,
        ResolvedBinaryOp::Power => BinOp::Pow,
        ResolvedBinaryOp::Equal => BinOp::EqCmp,
        ResolvedBinaryOp::NotEqual => BinOp::NeCmp,
        ResolvedBinaryOp::Less => BinOp::Lt,
        ResolvedBinaryOp::Greater => BinOp::Gt,
        ResolvedBinaryOp::LessEqual => BinOp::Le,
        ResolvedBinaryOp::GreaterEqual => BinOp::Ge,
        ResolvedBinaryOp::LogicalAnd => BinOp::And,
        ResolvedBinaryOp::LogicalOr => BinOp::Or,
        ResolvedBinaryOp::BitAnd => BinOp::BitAnd,
        ResolvedBinaryOp::BitOr => BinOp::BitOr,
        ResolvedBinaryOp::BitXor => BinOp::BitXor,
        ResolvedBinaryOp::ShiftLeft => BinOp::Shl,
        ResolvedBinaryOp::ShiftRight => BinOp::Shr,
    }
}

fn is_signed_integer_type(program: &CheckedProgram, ty: &ResolvedTypeId) -> bool {
    use crate::core::PrimitiveType;
    matches!(
        program.resolved_types().get(ty),
        Some(ResolvedType::Primitive(
            PrimitiveType::I8
                | PrimitiveType::I16
                | PrimitiveType::I32
                | PrimitiveType::I64
                | PrimitiveType::I128
                | PrimitiveType::Isize
        ))
    )
}

/// is_empty (0.1.9): classify an arg's canonical type display into the
/// Map-vs-Set codegen kind (both are bare i64 handles at runtime).
fn classify_is_empty_kind(type_name: &str) -> Option<&'static str> {
    if type_name == "map" || type_name.starts_with("Map") || type_name == "Record" {
        Some("map")
    } else if type_name == "set" || type_name.starts_with("Set") {
        Some("set")
    } else {
        None
    }
}

/// Map a canonical type identity to the display name expected by the
/// print-family builtin formatting dispatch (`pending_print_arg_types`).
///
/// The print-formatter chain in `src/codegen/builtins/io.rs` dispatches on
/// strings like `"List<string>"`, `"Option<i32>"`, `"Result<i32, string>"`,
/// `"(i32, string)"`, `"Map<string, i32>"`, `"Set<string>"`, etc.  This
/// function recovers those names from the resolved type graph so that the
/// resolved emitter's per-function dispatch path selects the correct
/// runtime formatter — instead of falling through to the `emit_list_i32`
/// default (which prints pointer addresses as decimal integers).
fn resolved_type_display_name(program: &CheckedProgram, ty: &ResolvedTypeId) -> String {
    use crate::core::PrimitiveType;
    use crate::core::ResolvedType::*;
    let rty = match program.resolved_types().get(ty) {
        Some(t) => t,
        None => return "unknown".to_string(),
    };
    match rty {
        Primitive(p) => match p {
            PrimitiveType::I8 | PrimitiveType::I16 | PrimitiveType::I32 => "i32".to_string(),
            PrimitiveType::I64 | PrimitiveType::Isize => "i64".to_string(),
            PrimitiveType::U8 | PrimitiveType::U16 | PrimitiveType::U32 => "u32".to_string(),
            PrimitiveType::U64 | PrimitiveType::Usize => "u64".to_string(),
            PrimitiveType::F32 | PrimitiveType::F64 => "f64".to_string(),
            PrimitiveType::Bool => "bool".to_string(),
            PrimitiveType::String | PrimitiveType::Char => "string".to_string(),
            PrimitiveType::Unit => "unit".to_string(),
            _ => "unknown".to_string(),
        },
        Nominal {
            item, arguments, ..
        } => {
            let name = item
                .as_str()
                .strip_prefix("builtin:type:")
                .or_else(|| item.as_str().strip_prefix("type:"))
                .unwrap_or(item.as_str())
                .to_string();
            if arguments.is_empty() {
                name
            } else {
                let args: Vec<String> = arguments
                    .iter()
                    .map(|a| resolved_type_display_name(program, a))
                    .collect();
                format!("{}<{}>", name, args.join(", "))
            }
        }
        Option(inner) => {
            format!("Option<{}>", resolved_type_display_name(program, inner))
        }
        Result { ok, error } => {
            format!(
                "Result<{}, {}>",
                resolved_type_display_name(program, ok),
                resolved_type_display_name(program, error)
            )
        }
        Tuple(elems) => {
            let inner: Vec<String> = elems
                .iter()
                .map(|e| resolved_type_display_name(program, e))
                .collect();
            format!("({})", inner.join(", "))
        }
        Reference {
            target, mutable, ..
        } => {
            let prefix = if *mutable { "&mut " } else { "&" };
            format!("{}{}", prefix, resolved_type_display_name(program, target))
        }
        Function {
            parameters, result, ..
        } => {
            let params: Vec<String> = parameters
                .iter()
                .map(|p| resolved_type_display_name(program, p))
                .collect();
            format!(
                "fn({}) -> {}",
                params.join(", "),
                resolved_type_display_name(program, result)
            )
        }
        Slice(inner) => {
            format!("[]{}", resolved_type_display_name(program, inner))
        }
        Newtype { inner, .. } => resolved_type_display_name(program, inner),
        Array { element, .. } => {
            format!("[{}]", resolved_type_display_name(program, element))
        }
        FlowStateSet { .. } => "unknown".to_string(),
        _ => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::eligibility::require_resolved_native_callable;
    use super::*;

    #[test]
    fn resolved_codegen_boundary_has_no_surface_body_backdoor() {
        let sources = [
            ("mod.rs", include_str!("mod.rs")),
            ("eligibility.rs", include_str!("eligibility.rs")),
            ("types.rs", include_str!("types.rs")),
        ];
        let retained_body_accessor = concat!("legacy_body", "_file(");
        let surface_emitter_entry = concat!("compile_", "file(");

        for (name, source) in sources {
            assert!(
                !source.contains(retained_body_accessor),
                "{name} must not recover the retained surface body"
            );
            assert!(
                !source.contains(surface_emitter_entry),
                "{name} must not enter the surface emitter"
            );
            for line in source.lines().map(str::trim) {
                if line.starts_with("use crate::ast") {
                    assert_eq!(
                        line, "use crate::ast::BinOp;",
                        "{name} may reuse only the representation-free BinOp enum"
                    );
                }
            }
        }
    }

    fn checked(source: &str) -> CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        crate::core::check_program(&file).expect("check")
    }

    #[test]
    fn scalar_leaf_emits_from_resolved_callables() {
        let program = checked(
            r#"
func add(left: i32, right: i32) -> i32 { left + right }
func main() -> i32 { add(40, 2) }
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_scalar");
        generator
            .compile_resolved_native(&program)
            .expect("resolved native compile");
        generator.module.verify().expect("valid LLVM");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("define i32 @add(i32"), "{ir}");
        assert!(ir.contains("define i32 @main(i32"), "{ir}");
        assert!(ir.contains("call i32 @add"), "{ir}");
    }

    #[test]
    fn match_literal_patterns_emit_from_resolved_ir() {
        let program = checked(
            r#"
func main() -> i32 {
    let x = 1
    match x {
        1 => 10,
        _ => 20,
    }
}
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_match");
        generator
            .compile_resolved_native(&program)
            .expect("match with literal patterns is in the resolved native slice");
        generator.module.verify().expect("valid LLVM");
    }

    /// 0.32.2: List literals and indexing are now in the resolved native
    /// slice. This test was previously a rejection test; now it verifies
    /// successful compilation.
    #[test]
    fn list_literal_compiles_through_resolved_emitter() {
        let program = checked(
            r#"
func main() -> i32 {
    let xs = [1, 2, 3]
    xs[0]
}
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_list_lit");
        generator
            .compile_resolved_native(&program)
            .expect("list literal is now in the resolved native slice");
        generator.module.verify().expect("valid LLVM");
    }

    #[test]
    fn parameters_and_bindings_use_resolved_local_identities() {
        let program = checked(
            r#"
func increment(input: i32) -> i32 {
    let output = input + 1
    output
}
func main() -> i32 { increment(41) }
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_shadow");
        generator
            .compile_resolved_native(&program)
            .expect("parameters and bindings use stable local IDs");
        generator.module.verify().expect("valid LLVM");
    }

    #[test]
    fn if_else_expression_emits_from_resolved_ir() {
        let program = checked(
            r#"
func abs_value(x: i32) -> i32 {
    if x < 0 { 0 - x } else { x }
}
func main() -> i32 { abs_value(0 - 7) }
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_if");
        generator
            .compile_resolved_native(&program)
            .expect("if/else is in the resolved native slice");
        generator.module.verify().expect("valid LLVM");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("then"), "{ir}");
        assert!(ir.contains("else"), "{ir}");
        assert!(ir.contains("if_merge"), "{ir}");
    }

    #[test]
    fn while_loop_emits_from_resolved_ir() {
        let program = checked(
            r#"
func sum_to(n: i32) -> i32 {
    let mut i = 0
    let mut sum = 0
    while i < n {
        sum = sum + i
        i = i + 1
    }
    sum
}
func main() -> i32 { sum_to(5) }
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_while");
        generator
            .compile_resolved_native(&program)
            .expect("while loop is in the resolved native slice");
        generator.module.verify().expect("valid LLVM");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("while_header"), "{ir}");
        assert!(ir.contains("while_body"), "{ir}");
        assert!(ir.contains("while_exit"), "{ir}");
    }

    #[test]
    fn loop_unroll_cap_emits_self_referential_metadata() {
        // C1c (0.35.41): `MIMI_LOOP_UNROLL_CAP` emits `llvm.loop.unroll.count`
        // metadata on the loop latch. LLVM requires the loop metadata node to
        // be self-referential AND distinct (`!N = distinct !{!N, !M}`); a plain
        // uniqued `!{!M}` is silently ignored (0.35.30 audit).
        std::env::set_var("MIMI_LOOP_UNROLL_CAP", "4");
        let program = checked(
            r#"
func sum_to(n: i32) -> i32 {
    let mut i = 0
    let mut sum = 0
    while i < n {
        sum = sum + i
        i = i + 1
    }
    sum
}
func main() -> i32 { sum_to(5) }
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_loop_cap");
        generator
            .compile_resolved_native(&program)
            .expect("while loop is in the resolved native slice");
        generator.module.verify().expect("valid LLVM");
        let ir = generator.module.print_to_string().to_string();
        std::env::remove_var("MIMI_LOOP_UNROLL_CAP");
        assert!(ir.contains("llvm.loop.unroll.count"), "{ir}");
        // Self-referential distinct node: `!N = distinct !{!N, !M}`.
        assert!(
            ir.contains("distinct !{!") || ir.contains("= distinct !{"),
            "loop metadata must be distinct: {ir}"
        );
    }

    #[test]
    fn nested_if_inside_while_compiles() {
        // NOTE: `for i in 0..n` is not testable here because the checker's
        // resolved body lowering does not yet support range iterables
        // (CHECKER-GAP: TOOL-RESOLUTION-001 binary sugar). The For/Range
        // emitter code is retained for when the checker catches up.
        let program = checked(
            r#"
func count_eq(n: i32) -> i32 {
    let mut i = 0
    let mut count = 0
    while i < n {
        if i == 3 {
            count = count + 1
        }
        i = i + 1
    }
    count
}
func main() -> i32 { count_eq(5) }
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_nested");
        generator
            .compile_resolved_native(&program)
            .expect("nested if inside while is in the resolved native slice");
        generator.module.verify().expect("valid LLVM");
    }

    #[test]
    fn early_return_inside_if_skips_merge() {
        let program = checked(
            r#"
func guard(x: i32) -> i32 {
    if x < 0 {
        return 0 - 1
    }
    x
}
func main() -> i32 { guard(5) }
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_early_ret");
        generator
            .compile_resolved_native(&program)
            .expect("early return inside if is valid");
        generator.module.verify().expect("valid LLVM");
    }

    #[test]
    fn builtin_call_wrapping_add_emits_from_resolved_ir() {
        let program = checked(
            r#"
func main() -> i64 {
    wrapping_add(40, 2)
}
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_builtin_add");
        generator
            .compile_resolved_native(&program)
            .expect("wrapping_add builtin is in the resolved native slice");
        generator.module.verify().expect("valid LLVM");
    }

    #[test]
    fn builtin_math_chain_emits_from_resolved_ir() {
        let program = checked(
            r#"
func compute() -> f64 {
    let x = sqrt(16.0)
    let y = floor(x)
    y
}
func main() -> i32 {
    let r = compute()
    if r == 4.0 { 1 } else { 0 }
}
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_builtin_math");
        generator
            .compile_resolved_native(&program)
            .expect("sqrt/floor builtins are in the resolved native slice");
        generator.module.verify().expect("valid LLVM");
    }

    #[test]
    fn builtin_predicate_in_if_condition() {
        let program = checked(
            r#"
func main() -> i32 {
    if is_nan(0.0) { 1 } else { 0 }
}
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_builtin_pred");
        generator
            .compile_resolved_native(&program)
            .expect("is_nan builtin in if condition is valid");
        generator.module.verify().expect("valid LLVM");
    }

    #[test]
    fn numeric_narrow_cast_emits_from_resolved_ir() {
        let program = checked(
            r#"
func main() -> i32 {
    let x = sqrt(16.0)
    let y = floor(x)
    let z = y as i32
    z
}
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_narrow");
        generator
            .compile_resolved_native(&program)
            .expect("f64→i32 narrowing cast is in the resolved native slice");
        generator.module.verify().expect("valid LLVM");
        let ir = generator.module.print_to_string().to_string();
        assert!(ir.contains("fptosi"), "expected fptosi instruction: {ir}");
    }

    #[test]
    fn println_with_string_literal_emits_from_resolved_ir() {
        let program = checked(
            r#"
func main() -> i32 {
    println("hello resolved")
    0
}
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_println");
        generator
            .compile_resolved_native(&program)
            .expect("println with string literal is in the resolved native slice");
        generator.module.verify().expect("valid LLVM");
        let ir = generator.module.print_to_string().to_string();
        assert!(
            ir.contains("hello resolved"),
            "expected string constant in IR: {ir}"
        );
    }

    /// 0.32.1: Option<i64> return type is now eligible (types.rs already
    /// lowers Option to {i1, T}). Verify the emitter handles Some/None
    /// construction and Option-typed return values.
    #[test]
    fn option_return_emits_from_resolved_ir() {
        let program = checked(
            r#"
func maybe_double(x: i64, flag: bool) -> Option<i64> {
    if flag { Some(x * 2) } else { None }
}
func main() -> i32 { 0 }
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_option");
        generator
            .compile_resolved_native(&program)
            .expect("Option<i64> return is in the resolved native slice");
        generator.module.verify().expect("valid LLVM");
    }

    /// 0.32.2: List<i64> construction, indexing, and len() through the
    /// resolved native emitter. Verifies LLVM IR is valid.
    #[test]
    fn list_construct_index_emits_from_resolved_ir() {
        let program = checked(
            r#"
func sum(xs: List<i64>) -> i64 {
    let mut total: i64 = 0
    let mut i: i64 = 0
    while i < len(xs) {
        total = total + xs[i]
        i = i + 1
    }
    total
}
func main() -> i32 {
    let nums: List<i64> = [10, 20, 30]
    let s = sum(nums)
    if s == 60 { 0 } else { 1 }
}
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_list");
        generator
            .compile_resolved_native(&program)
            .expect("List construct/index is in the resolved native slice");
        generator.module.verify().expect("valid LLVM");
    }

    /// 0.32.5: User-defined record construction and field access through
    /// the resolved native emitter.
    #[test]
    fn record_construct_field_access_emits_from_resolved_ir() {
        let program = checked(
            r#"
type Point { x: i64, y: i64 }
func make_point(a: i64, b: i64) -> Point { Point { x: a, y: b } }
func get_x(p: Point) -> i64 { p.x }
func main() -> i32 {
    let p = make_point(3, 4)
    let x = get_x(p)
    if x == 3 { 0 } else { 1 }
}
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_record");
        generator
            .compile_resolved_native(&program)
            .expect("Record construct/field access is in the resolved native slice");
        generator.module.verify().expect("valid LLVM");
    }

    /// 0.32.6: Option match with Constructor patterns (Some/None) through
    /// the resolved native emitter.
    #[test]
    fn option_match_constructor_emits_from_resolved_ir() {
        let program = checked(
            r#"
func safe_div(a: i64, b: i64) -> Option<i64> {
    if b == 0 { None } else { Some(a / b) }
}
func unwrap_or(opt: Option<i64>, default: i64) -> i64 {
    match opt {
        Some(v) => v,
        None => default,
    }
}
func main() -> i32 {
    let r = unwrap_or(safe_div(10, 3), 0 - 1)
    if r == 3 { 0 } else { 1 }
}
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_opt_match");
        generator
            .compile_resolved_native(&program)
            .expect("Option match with Constructor patterns is in the resolved native slice");
        generator.module.verify().expect("valid LLVM");
    }

    /// 0.32.8: For-in-list iteration compiles through the resolved emitter.
    #[test]
    fn for_in_list_compiles_through_resolved_emitter() {
        let program = checked(
            r#"
func main() -> i32 {
    let xs = [10, 20, 30]
    let mut sum = 0
    for x in xs {
        sum += x
    }
    if sum == 60 { 0 } else { 1 }
}
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_for_list");
        generator
            .compile_resolved_native(&program)
            .expect("for-in-list is in the resolved native slice");
        generator.module.verify().expect("valid LLVM");
    }

    /// 0.32.8: For-in-list with string elements.
    #[test]
    fn for_in_string_list_compiles_through_resolved_emitter() {
        let program = checked(
            r#"
func main() -> i32 {
    let names = ["alice", "bob", "carol"]
    let mut count = 0
    for name in names {
        if len(name) > 3 {
            count += 1
        }
    }
    if count == 2 { 0 } else { 1 }
}
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_for_str_list");
        generator
            .compile_resolved_native(&program)
            .expect("for-in-string-list is in the resolved native slice");
        generator.module.verify().expect("valid LLVM");
    }

    /// 0.32.10: Try expression with Result compiles through resolved emitter.
    #[test]
    fn try_result_compiles_through_resolved_emitter() {
        let program = checked(
            r#"
func safe_div(a: i64, b: i64) -> Result<i64, i64> {
    if b == 0 { Err(1) } else { Ok(a / b) }
}
func main() -> i32 {
    let r = safe_div(10, 2)?
    if r == 5 { 0 } else { 1 }
}
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_try_result");
        generator
            .compile_resolved_native(&program)
            .expect("Try with Result is in the resolved native slice");
        generator.module.verify().expect("valid LLVM");
    }

    /// 0.32.10: Try expression with Option compiles through resolved emitter.
    #[test]
    fn try_option_compiles_through_resolved_emitter() {
        let program = checked(
            r#"
func first(xs: List<i64>) -> Option<i64> {
    if len(xs) > 0 { Some(xs[0]) } else { None }
}
func main() -> i32 {
    let xs: List<i64> = [42, 7]
    let v = first(xs)?
    if v == 42 { 0 } else { 1 }
}
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_try_option");
        generator
            .compile_resolved_native(&program)
            .expect("Try with Option is in the resolved native slice");
        generator.module.verify().expect("valid LLVM");
    }

    /// Diagnostic: dispatch distribution across representative programs.
    /// Run with `--nocapture` to see the per-function eligibility report.
    /// This test documents the current resolved-native coverage and identifies
    /// the most common ineligibility reasons for 0.1.2 Phase A planning.
    #[test]
    fn dispatch_diagnostic_coverage_report() {
        let probes: Vec<(&str, &str)> = vec![
            (
                "pure_scalar",
                r#"
func add(a: i32, b: i32) -> i32 { a + b }
func main() -> i32 { add(1, 2) }
"#,
            ),
            (
                "tuple_return",
                r#"
func divmod(a: i64, b: i64) -> (i64, i64) { (a / b, a % b) }
func main() -> i32 { let (q, r) = divmod(17, 5); 0 }
"#,
            ),
            (
                "while_loop",
                r#"
func sum_to(n: i32) -> i32 {
    let mut s = 0
    let mut i = 0
    while i < n { s = s + i; i = i + 1 }
    s
}
func main() -> i32 { sum_to(10) }
"#,
            ),
            (
                "if_else_expr",
                r#"
func abs(x: i32) -> i32 { if x < 0 { 0 - x } else { x } }
func main() -> i32 { abs(0 - 5) }
"#,
            ),
            (
                "match_literal",
                r#"
func classify(x: i32) -> i32 {
    match x { 0 => 10, 1 => 20, _ => 30 }
}
func main() -> i32 { classify(1) }
"#,
            ),
            (
                "builtin_math",
                r#"
func compute() -> f64 { sqrt(16.0) }
func main() -> i32 { let r = compute(); if r == 4.0 { 1 } else { 0 } }
"#,
            ),
            (
                "println_scalar",
                r#"
func main() -> i32 { println(42); 0 }
"#,
            ),
            (
                "list_param",
                r#"
func sum(xs: List<i64>) -> i64 {
    let mut t: i64 = 0
    let mut i: i64 = 0
    while i < len(xs) { t = t + xs[i]; i = i + 1 }
    t
}
func main() -> i32 { 0 }
"#,
            ),
            (
                "string_param",
                r#"
func greet(name: string) -> string { name }
func main() -> i32 { 0 }
"#,
            ),
            (
                "closure_param",
                r#"
func apply_twice(x: i32) -> i32 { x + x }
func main() -> i32 { apply_twice(5) }
"#,
            ),
            (
                "option_return",
                r#"
func find(xs: List<i64>, target: i64) -> Option<i64> {
    let mut i: i64 = 0
    while i < len(xs) {
        if xs[i] == target { return Some(i) }
        i = i + 1
    }
    None
}
func main() -> i32 { 0 }
"#,
            ),
            (
                "pure_option",
                r#"
func maybe_double(x: i64, flag: bool) -> Option<i64> {
    if flag { Some(x * 2) } else { None }
}
func main() -> i32 { 0 }
"#,
            ),
            (
                "fstring",
                r#"
func main() -> i32 {
    let x = 42
    println(f"x = {x}")
    0
}
"#,
            ),
            (
                "multi_func_chain",
                r#"
func step1(x: i32) -> i32 { x + 1 }
func step2(x: i32) -> i32 { x * 2 }
func pipeline(x: i32) -> i32 { step2(step1(x)) }
func main() -> i32 { pipeline(5) }
"#,
            ),
            (
                "early_return",
                r#"
func guard(x: i32) -> i32 {
    if x < 0 { return 0 - 1 }
    x
}
func main() -> i32 { guard(5) }
"#,
            ),
            (
                "nested_calls",
                r#"
func double(x: i32) -> i32 { x * 2 }
func quadruple(x: i32) -> i32 { double(double(x)) }
func main() -> i32 { quadruple(3) }
"#,
            ),
            (
                "record_type",
                r#"
type Point { x: i64, y: i64 }
func make_point(a: i64, b: i64) -> Point { Point { x: a, y: b } }
func get_x(p: Point) -> i64 { p.x }
func main() -> i32 { println(get_x(make_point(3, 4))); 0 }
"#,
            ),
        ];

        let mut total_functions = 0;
        let mut total_eligible = 0;
        let mut rejection_reasons: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();

        for (name, source) in &probes {
            let program = checked(source);
            let all_fns: Vec<_> = program
                .functions()
                .values()
                .filter(|f| !f.is_comptime)
                .collect();
            let eligible =
                eligible_function_ids_with_stats(&program, false).map(|(set, _stats)| set);

            let eligible_set = match &eligible {
                Ok(set) => set.clone(),
                Err(e) => {
                    eprintln!("  [{name}] PROGRAM-LEVEL REJECT: {}", e.reason);
                    *rejection_reasons
                        .entry(format!("program-level: {}", e.reason))
                        .or_insert(0) += 1;
                    total_functions += all_fns.len();
                    continue;
                }
            };

            let mut eligible_names = Vec::new();
            let mut ineligible_names = Vec::new();
            for f in &all_fns {
                total_functions += 1;
                if eligible_set.contains(&f.node_id) {
                    total_eligible += 1;
                    eligible_names.push(f.qualified_name.as_str());
                } else {
                    ineligible_names.push(f.qualified_name.as_str());
                    // Determine rejection reason by re-checking individually
                    if let Some(callable) = program.callable(&f.node_id) {
                        if let Err(e) = require_resolved_native_callable(&program, callable) {
                            *rejection_reasons.entry(e.reason.clone()).or_insert(0) += 1;
                        } else {
                            *rejection_reasons
                                .entry("origin/generics/qualified filter".into())
                                .or_insert(0) += 1;
                        }
                    }
                }
            }
            eprintln!(
                "  [{name}] eligible: {:?}  ineligible: {:?}",
                eligible_names, ineligible_names
            );
        }

        eprintln!("\n=== DISPATCH DIAGNOSTIC SUMMARY ===");
        eprintln!(
            "  Functions: {total_eligible}/{total_functions} eligible ({}%)",
            if total_functions > 0 {
                total_eligible * 100 / total_functions
            } else {
                0
            }
        );
        eprintln!("  Rejection reasons:");
        for (reason, count) in rejection_reasons.iter().rev() {
            eprintln!("    {count}x {reason}");
        }

        // Sanity: pure scalar programs must be 100% eligible
        assert!(
            total_eligible > 0,
            "at least some probe functions should be eligible"
        );
    }
}
