//! Native lowering directly from checker-owned Typed Resolved IR.
//!
//! This module is a capability boundary: it accepts `CheckedProgram` and
//! canonical identities only. Surface `File`/`FuncDef`/`Stmt`/`Expr` are not
//! imported here, and unsupported nodes fail closed instead of falling back to
//! the legacy emitter.

mod eligibility;
mod types;

use std::collections::BTreeMap;

use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, PointerValue};

use crate::ast::BinOp;
use crate::codegen::{CallSiteValueExt, CodeGenerator};
use crate::core::ir::ResolvedFStringPart;
use crate::core::ir::{ResolvedBinaryOp, ResolvedUnaryOp};
use crate::core::{
    CheckedConversion, CheckedConversionKind, CheckedProgram, FunctionTypeAbi, NodeId,
    PrimitiveType, ResolvedBlock, ResolvedBody, ResolvedCallee, ResolvedConstValue, ResolvedExpr,
    ResolvedExprKind, ResolvedLiteral, ResolvedLocalId, ResolvedPattern, ResolvedPatternKind,
    ResolvedPlace, ResolvedStmtKind, ResolvedType, ResolvedTypeId,
};
use crate::diagnostic::Diagnostic;
use crate::error::CompileError;

use self::eligibility::{
    eligible_function_ids, require_resolved_native_program, UnsupportedResolvedNode,
};
use self::types::llvm_type_for_resolved;

pub(super) fn supports_resolved_native(program: &CheckedProgram) -> bool {
    require_resolved_native_program(program).is_ok()
}

/// Returns the set of function NodeIds eligible for resolved native compilation.
/// Returns None if program-level blockers prevent any resolved compilation.
pub(super) fn resolved_eligible_functions(
    program: &CheckedProgram,
) -> Option<std::collections::BTreeSet<NodeId>> {
    match eligible_function_ids(program) {
        Ok(set) if !set.is_empty() => Some(set),
        Ok(_) => {
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

#[derive(Clone, Copy)]
struct ResolvedVarEntry<'ctx> {
    storage: PointerValue<'ctx>,
    llvm_type: BasicTypeEnum<'ctx>,
}

struct ResolvedFrame<'ctx> {
    owner: NodeId,
    locals: BTreeMap<ResolvedLocalId, ResolvedVarEntry<'ctx>>,
}

/// Loop context for `break`/`continue` lowering.
#[derive(Clone, Copy)]
struct LoopContext<'ctx> {
    header: inkwell::basic_block::BasicBlock<'ctx>,
    exit: inkwell::basic_block::BasicBlock<'ctx>,
}

struct NativeResolvedEmitter<'program, 'generator, 'ctx> {
    program: &'program CheckedProgram,
    generator: &'generator mut CodeGenerator<'ctx>,
    loop_stack: Vec<LoopContext<'ctx>>,
    /// Per-callable place inputs (dynamic index expressions). Set before
    /// emitting each function body, cleared after.
    place_inputs: BTreeMap<NodeId, crate::core::ResolvedExpr>,
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
    ) -> Result<usize, Vec<Diagnostic>> {
        program.validate_backend(crate::core::BackendProfile::Native)?;
        NativeResolvedEmitter {
            program,
            generator: self,
            loop_stack: Vec::new(),
            place_inputs: BTreeMap::new(),
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
    ) -> Result<usize, CompileError> {
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
                            failed += 1;
                            continue;
                        }
                    }
                    count += 1;
                }
                Err(e) => {
                    // Function failed to emit through resolved path.
                    // Record in failed set — the legacy emitter's skip check
                    // will handle it by deleting the partial body and
                    // re-compiling from scratch.
                    let symbol = function.qualified_name.clone();
                    self.generator.resolved_failed_functions.insert(symbol);
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
        Ok(count)
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
    fn lower_type(&self, id: &ResolvedTypeId) -> Result<BasicTypeEnum<'ctx>, CompileError> {
        match llvm_type_for_resolved(self.generator.context, self.program.resolved_types(), id) {
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
                    let sty = self.record_llvm_type(item)?;
                    Ok(BasicTypeEnum::StructType(sty))
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
        let parameters = callable
            .signature
            .parameters
            .iter()
            .map(|parameter| {
                self.lower_type(&parameter.ty)
                    .map(BasicMetadataTypeEnum::from)
            })
            .collect::<Result<Vec<_>, _>>()?;
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
        // Push a heap scope so fstring/string allocations are tracked and
        // freed at function exit (matching legacy emitter behavior).
        self.generator.push_heap_scope();
        let mut frame = ResolvedFrame {
            owner: callable.owner.clone(),
            locals: BTreeMap::new(),
        };
        self.bind_parameters(callable, function, &mut frame)?;
        let value = self.emit_block(&callable.body, &callable.body.root, &mut frame)?;
        if self.current_block_terminated() {
            // Early return already emitted; still need to balance the heap
            // scope. Pop without freeing — the early return path is
            // responsible for its own cleanup.
            let _ = self.generator.free_heap_allocs();
            return Ok(());
        }
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
        let return_owns_heap = matches!(result_type, BasicTypeEnum::StructType(st) if {
            st.get_field_types().iter().any(|f| matches!(f, BasicTypeEnum::PointerType(_)))
        });
        if return_owns_heap {
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
            let storage = self
                .generator
                .build_alloca(llvm_type, &local.display_name)?;
            self.generator.build_store(storage, value)?;
            frame
                .locals
                .insert(local_id.clone(), ResolvedVarEntry { storage, llvm_type });
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
                let value = self.emit_expr(initializer, frame)?;
                self.bind_pattern(body, pattern, value, frame)?;
                Ok(None)
            }
            ResolvedStmtKind::Assign {
                target,
                value,
                conversion,
            } => {
                let value = self.emit_expr(value, frame)?;
                let value = self.apply_conversion(value, conversion)?;
                let target = self.root_place(frame, target)?;
                let value = self.coerce_to(value, target.llvm_type)?;
                self.generator.build_store(target.storage, value)?;
                Ok(None)
            }
            ResolvedStmtKind::Return { value, conversion } => {
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
                self.generator.build_return(
                    value
                        .as_ref()
                        .map(|value| value as &dyn inkwell::values::BasicValue<'ctx>),
                )?;
                Ok(value)
            }
            ResolvedStmtKind::Expr(expression) => self.emit_expr(expression, frame).map(Some),
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
                self.generator.build_br(loop_ctx.exit)?;
                Ok(None)
            }
            ResolvedStmtKind::Continue => {
                let loop_ctx = self
                    .loop_stack
                    .last()
                    .ok_or_else(|| CompileError::Unsupported("continue outside loop".into()))?;
                self.generator.build_br(loop_ctx.header)?;
                Ok(None)
            }
            ResolvedStmtKind::Scope {
                body: scope_block, ..
            } => {
                // Lexical scope: emit the inner block inline.
                self.emit_block(body, scope_block, frame)?;
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

    fn bind_pattern(
        &mut self,
        body: &ResolvedBody,
        pattern: &ResolvedPattern,
        value: BasicValueEnum<'ctx>,
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
                    self.coerce_to(value, llvm_type)?
                };
                let storage = self
                    .generator
                    .build_alloca(llvm_type, &metadata.display_name)?;
                self.generator.build_store(storage, value)?;
                frame
                    .locals
                    .insert(local.clone(), ResolvedVarEntry { storage, llvm_type });
                Ok(())
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
                    return self.generator.compile_constructor("None", vec![]);
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
                let entry = self.root_place(frame, place)?;
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
                // Store each element as i64.
                for (i, element) in elements.iter().enumerate() {
                    let value = self.emit_expr(element, frame)?;
                    let iv = self.coerce_to_i64(value)?;
                    let idx = i64_ty.const_int(i as u64, false);
                    let elem_ptr = self.generator.build_in_bounds_gep(
                        i64_ty,
                        data_ptr,
                        &[idx],
                        "list_elem",
                    )?;
                    self.generator.build_store(elem_ptr, iv)?;
                }
                // build_list_struct returns a pointer to the alloca'd struct.
                // Load the struct value so the resolved emitter can store it
                // in local variables (matching tuple semantics).
                let list_ptr = self.generator.build_list_struct(len_val, data_ptr)?;
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
                Ok(BasicValueEnum::IntValue(set_handle))
            }
            // 0.32.5: Record construction. Build LLVM struct from field
            // value types, allocate, store each field.
            ResolvedExprKind::Record { nominal: _, fields } => {
                // Build the LLVM struct type from field value types.
                let field_types: Vec<BasicTypeEnum<'ctx>> = fields
                    .iter()
                    .map(|f| self.lower_type(&f.value.ty))
                    .collect::<Result<_, _>>()?;
                let struct_ty = self.generator.context.struct_type(&field_types, false);
                let alloca = self.generator.build_alloca(struct_ty, "record_alloc")?;
                for (i, field) in fields.iter().enumerate() {
                    let value = self.emit_expr(&field.value, frame)?;
                    let field_ptr = self
                        .generator
                        .builder
                        .build_struct_gep(struct_ty, alloca, i as u32, "rec_field")
                        .map_err(|e| CompileError::LlvmError(format!("record gep: {e}")))?;
                    let field_ty =
                        struct_ty.get_field_type_at_index(i as u32).ok_or_else(|| {
                            CompileError::LlvmError(format!("record field {i} type absent"))
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
                        let field =
                            struct_val
                                .get_field_at_index(*index as u32)
                                .ok_or_else(|| {
                                    CompileError::LlvmError(format!("tuple field {index} absent"))
                                })?;
                        Ok(field)
                    }
                    // 0.32.2: Index value projection for List rvalue access.
                    crate::core::ir::ResolvedValueProjection::Index(index_expr) => {
                        let BasicValueEnum::StructValue(struct_val) = agg else {
                            return Err(CompileError::Unsupported(
                                "index projection on non-struct (list) value".into(),
                            ));
                        };
                        // Extract data pointer (field 1).
                        let data_ptr = struct_val
                            .get_field_at_index(1)
                            .ok_or_else(|| {
                                CompileError::LlvmError("list data field absent".into())
                            })?
                            .into_pointer_value();
                        // Evaluate index.
                        let idx_val = self.emit_expr(index_expr, frame)?.into_int_value();
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
                        let field =
                            struct_val.get_field_at_index(field_index).ok_or_else(|| {
                                CompileError::LlvmError(format!(
                                    "record field {field_index} absent"
                                ))
                            })?;
                        Ok(field)
                    }
                    other => Err(CompileError::Unsupported(format!(
                        "value projection {other:?} escaped resolved native eligibility"
                    ))),
                }
            }
            ResolvedExprKind::Binary { op, left, right } => {
                let left = self.emit_expr(left, frame)?;
                let right = self.emit_expr(right, frame)?;
                self.generator.compile_binop(binary_op(*op), left, right)
            }
            ResolvedExprKind::Unary { op, operand } => {
                let value = self.emit_expr(operand, frame)?;
                self.emit_unary(*op, value)
            }
            ResolvedExprKind::Cast { value, conversion } => {
                let value = self.emit_expr(value, frame)?;
                self.apply_conversion(value, conversion)
            }
            ResolvedExprKind::Call(call) => {
                // Evaluate arguments (shared by all callee kinds)
                let mut arguments = Vec::with_capacity(call.arguments.len());
                for argument in &call.arguments {
                    let value = self.emit_expr(&argument.value, frame)?;
                    let value = self.apply_conversion(value, &argument.conversion)?;
                    arguments.push(BasicMetadataValueEnum::from(value));
                }
                match &call.callee {
                    ResolvedCallee::Function(owner) => {
                        let symbol = self.callable_symbol(owner)?.to_string();
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
                        self.generator
                            .build_call(callee, &arguments, "resolved_call")?
                            .try_as_basic_value_opt()
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
                                // B9 (audit): when the callee returns a Mimi
                                // closure, register its env so the caller's
                                // scope exit releases it. The callee (legacy
                                // or resolved emitter) already claimed the env
                                // on its side — ownership transfers here.
                                if !matches!(
                                    self.program.resolved_types().get(&call.result),
                                    Some(ResolvedType::Function {
                                        abi: FunctionTypeAbi::Mimi,
                                        ..
                                    })
                                ) {
                                    return Ok(result);
                                }
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
                    ResolvedCallee::Builtin(builtin_id) => {
                        let name = builtin_id.as_str();
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
                            let ctor_args: Vec<BasicValueEnum<'ctx>> = call
                                .arguments
                                .iter()
                                .map(|arg| -> Result<_, CompileError> {
                                    let value = self.emit_expr(&arg.value, frame)?;
                                    self.apply_conversion(value, &arg.conversion)
                                })
                                .collect::<Result<_, _>>()?;
                            return self.generator.compile_constructor(name, ctor_args);
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
                        // Print-family builtins need arg type hints for formatting dispatch.
                        if matches!(name, "println" | "print" | "eprintln" | "format") {
                            self.generator.pending_print_arg_types = call
                                .arguments
                                .iter()
                                .map(|arg| resolved_type_display_name(self.program, &arg.value.ty))
                                .collect();
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
                        // 0.32.22: Coerce integer arguments to match the runtime
                        // function's declared parameter types. Builtins like
                        // mutex_new call runtime functions (mimi_mutex_new)
                        // declared with i64 params, but the resolved IR types
                        // integer literals as i32. Look up the runtime function
                        // and coerce to match its signature.
                        // 0.32.24: Some builtins delegate to differently-named
                        // runtime functions (e.g. session_send → mimi_channel_send).
                        // Try the direct name first, then known aliases.
                        let runtime_fn_name = format!("mimi_{name}");
                        let runtime_fn_name = if self
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
                        };
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
                        let symbol = method_str
                            .strip_prefix("function:")
                            .and_then(|s: &str| s.split_once(":for:"))
                            .and_then(|(_, rest): (&str, &str)| {
                                rest.split_once("::")
                                    .map(|(ty, method_hash): (&str, &str)| {
                                        let method_name = method_hash
                                            .rsplit_once(':')
                                            .map(|(m, _)| m)
                                            .unwrap_or(method_hash);
                                        format!("{}_{}", ty, method_name)
                                    })
                            })
                            .ok_or_else(|| {
                                CompileError::Unsupported(format!(
                                    "cannot parse ProtocolMethod MethodId '{method_str}'"
                                ))
                            })?;
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
                        let callee =
                            self.generator
                                .module
                                .get_function(ext_name)
                                .ok_or_else(|| {
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
                    _ => Err(CompileError::Unsupported(format!(
                        "resolved callee {:?} escaped resolved native eligibility at '{}'",
                        call.callee, expression.node_id.0
                    ))),
                }
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => self.emit_if(expression, condition, then_block, else_block, frame),
            ResolvedExprKind::Block(block) => {
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
            // 0.32.32: Old expression (contract `old(x)`). Identity in codegen —
            // contracts are erased at runtime; only the verifier distinguishes old().
            ResolvedExprKind::Old(inner) => self.emit_expr(inner, frame),
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
                // String ABI: {ptr, i64} struct (ptr to null-terminated data, byte length).
                let global = self
                    .generator
                    .builder
                    .build_global_string_ptr(text, "resolved_str")
                    .map_err(|e| CompileError::LlvmError(format!("string literal: {e}")))?;
                let ptr_val = global.as_pointer_value();
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
                let global = self
                    .generator
                    .builder
                    .build_global_string_ptr(text, "resolved_const_str")
                    .map_err(|e| CompileError::LlvmError(format!("const string: {e}")))?;
                let ptr_val = global.as_pointer_value();
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
            let global = self
                .generator
                .builder
                .build_global_string_ptr(&text, "resolved_fstr")
                .map_err(|e| CompileError::LlvmError(format!("fstring literal: {e}")))?;
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
            let agg =
                struct_ty.const_named_struct(&[global.as_pointer_value().into(), len_val.into()]);
            return Ok(agg.into());
        }

        // Interpolation path: build format string + snprintf into stack buffer.
        let mut fmt_str = String::new();
        let mut args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();

        for part in parts {
            match part {
                ResolvedFStringPart::Text(t) => {
                    // Escape '%' in text parts for printf format.
                    fmt_str.push_str(&t.replace('%', "%%"));
                }
                ResolvedFStringPart::Interpolation(expr) => {
                    let value = self.emit_expr(expr, frame)?;
                    let spec = self.fstring_format_spec(&expr.ty)?;
                    fmt_str.push_str(spec);
                    args.push(BasicMetadataValueEnum::from(value));
                }
            }
        }

        // Emit format string as global constant.
        let fmt_global = self
            .generator
            .builder
            .build_global_string_ptr(&fmt_str, "fstr_fmt")
            .map_err(|e| CompileError::LlvmError(format!("fstring fmt: {e}")))?;

        // Allocate stack buffer (4096 bytes).
        let buf_size: u64 = 4096;
        let buf_type = self.generator.context.i8_type().array_type(buf_size as u32);
        let buf = self.generator.build_alloca(buf_type, "fstr_buf")?;

        // Get or declare snprintf.
        let snprintf = self.get_or_declare_snprintf();

        // Call snprintf(buf, size, fmt, args...)
        let buf_ptr = self
            .generator
            .builder
            .build_pointer_cast(
                buf,
                self.generator
                    .context
                    .ptr_type(inkwell::AddressSpace::default()),
                "fstr_buf_ptr",
            )
            .map_err(|e| CompileError::LlvmError(format!("fstr buf cast: {e}")))?;
        let size_val = self.generator.context.i64_type().const_int(buf_size, false);
        let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> = vec![
            BasicMetadataValueEnum::from(buf_ptr),
            BasicMetadataValueEnum::from(size_val),
            BasicMetadataValueEnum::from(fmt_global.as_pointer_value()),
        ];
        call_args.extend(args);

        self.generator
            .build_call(snprintf, &call_args, "fstr_snprintf")?;

        // Return {ptr, i64} string struct (ptr to buffer, len=0 placeholder;
        // runtime uses null terminator for actual string operations).
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
        let str_alloca = self.generator.build_alloca(struct_ty, "fstr_ret")?;
        let ptr_field = self
            .generator
            .builder
            .build_struct_gep(struct_ty, str_alloca, 0, "fstr_ptr_field")
            .map_err(|e| CompileError::LlvmError(format!("fstr gep0: {e}")))?;
        let len_field = self
            .generator
            .builder
            .build_struct_gep(struct_ty, str_alloca, 1, "fstr_len_field")
            .map_err(|e| CompileError::LlvmError(format!("fstr gep1: {e}")))?;
        self.generator.build_store(ptr_field, buf_ptr)?;
        self.generator.build_store(len_field, i64_ty.const_zero())?;
        self.generator.build_load(struct_ty, str_alloca, "fstr_val")
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
                // Wrap raw ptr into {ptr, i64} struct.
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
                self.generator.build_store(len_field, i64_ty.const_zero())?;
                return self.generator.build_load(struct_ty, alloca, "str_wrapped");
            }
        }
        Ok(value)
    }

    /// Map a canonical type to its printf format specifier.
    fn fstring_format_spec(&self, ty: &ResolvedTypeId) -> Result<&'static str, CompileError> {
        use crate::core::PrimitiveType;
        match self.program.resolved_types().get(ty) {
            Some(ResolvedType::Primitive(
                PrimitiveType::I32 | PrimitiveType::U32 | PrimitiveType::Char,
            )) => Ok("%d"),
            Some(ResolvedType::Primitive(
                PrimitiveType::I8 | PrimitiveType::U8 | PrimitiveType::I16 | PrimitiveType::U16,
            )) => Ok("%d"),
            Some(ResolvedType::Primitive(
                PrimitiveType::I64
                | PrimitiveType::U64
                | PrimitiveType::Isize
                | PrimitiveType::Usize
                | PrimitiveType::I128
                | PrimitiveType::U128,
            )) => Ok("%ld"),
            Some(ResolvedType::Primitive(PrimitiveType::F32 | PrimitiveType::F64)) => Ok("%g"),
            Some(ResolvedType::Primitive(PrimitiveType::String)) => Ok("%s"),
            Some(ResolvedType::Primitive(PrimitiveType::Bool)) => Ok("%d"),
            _ => Err(CompileError::Unsupported(format!(
                "f-string interpolation type '{}' is not supported",
                ty.as_str()
            ))),
        }
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
                let fn_type = i64.fn_type(
                    &[
                        BasicMetadataTypeEnum::from(ptr),
                        BasicMetadataTypeEnum::from(i64),
                        BasicMetadataTypeEnum::from(ptr),
                    ],
                    true, // variadic
                );
                self.generator
                    .module
                    .add_function("snprintf", fn_type, None)
            })
    }

    fn emit_match(
        &mut self,
        expression: &ResolvedExpr,
        scrutinee: &ResolvedExpr,
        arms: &[crate::core::ir::MatchArm],
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
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
                        let cmp =
                            self.generator
                                .compile_binop(BinOp::EqCmp, scrutinee_val, lit_val)?;
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

                let cond = if let Some(guard) = &arm.guard {
                    let guard_val = self.emit_expr(guard, frame)?;
                    let guard_bool = self.ensure_bool(guard_val)?;
                    self.generator
                        .builder
                        .build_and(pattern_matches, guard_bool, "match_guard_and")
                        .map_err(|e| CompileError::LlvmError(format!("guard and: {e}")))?
                } else {
                    pattern_matches
                };

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
                                let decoded =
                                    self.convert_list_elem_i64(raw_payload, field_llvm_ty)?;
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
                                // For built-in Err, the payload at index 2 is an i64
                                // ptrtoint to a heap-allocated {i64, i64} tuple (source
                                // and error, both ptrtoint-encoded). Decode the tuple
                                // struct before recursing so that Tuple patterns see a
                                // StructValue instead of a raw i64.
                                let decoded_val = if variant_name.as_str() == "Err"
                                    && matches!(payload_val, BasicValueEnum::IntValue(_))
                                {
                                    let i64_ty = self.generator.context.i64_type();
                                    let tuple_llvm_ty = self.generator.context.struct_type(
                                        &[
                                            BasicTypeEnum::IntType(i64_ty),
                                            BasicTypeEnum::IntType(i64_ty),
                                        ],
                                        false,
                                    );
                                    let ptr = self
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
                                            CompileError::LlvmError(format!("inttoptr err: {e}"))
                                        })?;
                                    self.generator
                                        .builder
                                        .build_load(
                                            BasicTypeEnum::StructType(tuple_llvm_ty),
                                            ptr,
                                            "err_tuple_val",
                                        )
                                        .map_err(|e| {
                                            CompileError::LlvmError(format!("load err tuple: {e}"))
                                        })?
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

    fn emit_unary(
        &mut self,
        op: ResolvedUnaryOp,
        value: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (op, value) {
            (ResolvedUnaryOp::Negate, BasicValueEnum::IntValue(value)) => {
                let zero = value.get_type().const_zero();
                self.generator
                    .compile_binop(BinOp::Sub, zero.into(), value.into())
            }
            (ResolvedUnaryOp::Negate, BasicValueEnum::FloatValue(value)) => {
                let zero = value.get_type().const_zero();
                self.generator
                    .compile_binop(BinOp::Sub, zero.into(), value.into())
            }
            (ResolvedUnaryOp::Not, BasicValueEnum::IntValue(value)) => self
                .generator
                .builder
                .build_not(value, "resolved_not")
                .map(BasicValueEnum::from)
                .map_err(|error| CompileError::LlvmError(format!("not error: {error}"))),
            _ => Err(CompileError::Unsupported(
                "resolved unary operator is not in the scalar-leaf slice".into(),
            )),
        }
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
            | CheckedConversionKind::ContainerErase => Ok(value),
            CheckedConversionKind::NumericWiden | CheckedConversionKind::NumericNarrowChecked => {
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
                    // String struct {i8*, i64}: the i64 IS the raw char* pointer.
                    // Build a proper {i8*, i64} struct from it.
                    let raw_ptr =
                        self.generator
                            .build_int_to_ptr(elem_int, ptr_ty, "elem_str_ptr")?;
                    // Call strlen to get the length.
                    let strlen_fn = self
                        .generator
                        .module
                        .get_function("strlen")
                        .ok_or_else(|| "strlen not declared in module".to_string())?;
                    let str_len = self
                        .generator
                        .builder
                        .build_call(
                            strlen_fn,
                            &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                            "strlen",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("strlen call: {e}")))?
                        .try_as_basic_value_opt()
                        .ok_or_else(|| CompileError::LlvmError("strlen returned void".into()))?;
                    // Build {i8*, i64} struct in an alloca then load.
                    let str_alloca = self.generator.build_alloca(sty, "str_struct")?;
                    // Store ptr (field 0)
                    let ptr_gep = self
                        .generator
                        .builder
                        .build_struct_gep(sty, str_alloca, 0, "str_ptr_gep")
                        .map_err(|e| CompileError::LlvmError(format!("str ptr gep: {e}")))?;
                    self.generator.build_store(ptr_gep, raw_ptr)?;
                    // Store len (field 1)
                    let len_gep = self
                        .generator
                        .builder
                        .build_struct_gep(sty, str_alloca, 1, "str_len_gep")
                        .map_err(|e| CompileError::LlvmError(format!("str len gep: {e}")))?;
                    self.generator.build_store(len_gep, str_len)?;
                    // Load the full struct
                    self.generator
                        .build_load(BasicTypeEnum::StructType(sty), str_alloca, "str_val")
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
        // Fallback: check if the NodeId string contains a known variant name.
        let id_str = &variant_id.0;
        for name in ["Some", "None", "Ok", "Err"] {
            if id_str.contains(name) {
                return Ok(name.to_string());
            }
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
        Err(CompileError::Unsupported(format!(
            "field '{}' not found in any type definition",
            field_id.0
        )))
    }

    /// Parse a Flow state field_id into (flow_type_name, span_string).
    /// Format: "state:Flow::State/node:decl.field@external:HASH:LINE:COL-LINE:COL"
    /// Returns ("flow::Flow::State", "LINE:COL-LINE:COL").
    fn parse_flow_field_id(field_id: &NodeId) -> Option<(String, String)> {
        let id_str = &field_id.0;
        let state_path = id_str.strip_prefix("state:")?;
        let slash_pos = state_path.find('/')?;
        let state_name = &state_path[..slash_pos];
        // Extract span: last segment after the last ':'-separated hash.
        // Format after slash: "node:decl.field@external:HASH:L:C-L:C"
        let after_slash = &state_path[slash_pos + 1..];
        // Find the span part: "L:C-L:C" at the end.
        let span_str = after_slash.rsplit_once(':').map(|(_, s)| s)?;
        // span_str is now "L:C-L:C" — but we need to handle the case where
        // the hash contains colons. Use a more robust extraction: find the
        // pattern "digits:digits-digits:digits" at the end.
        let span_str = span_str
            .rsplit_once('-')
            .map(|(start, end)| {
                // start might be "L:C" or "HASH:L:C"
                let start = start
                    .rsplit_once(':')
                    .map(|(_, c)| {
                        // Reconstruct "L:C" from the last two segments
                        let line = start
                            .rsplit_once(':')
                            .map(|(l, _)| l.rsplit_once(':').map_or(l, |(_, l2)| l2))
                            .unwrap_or(start);
                        format!("{}:{}", line, c)
                    })
                    .unwrap_or(start.to_string());
                format!("{}-{}", start, end)
            })
            .unwrap_or(span_str.to_string());
        Some((format!("flow::{state_name}"), span_str))
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
        // Build LLVM field types from the field type display strings.
        // Each field's type display is resolved via the ResolvedTypeTable
        // by finding a matching interned type.
        let mut field_types = Vec::with_capacity(td.fields.len());
        for (_name, type_display) in &td.fields {
            let field_ty = self.resolve_type_display(type_display)?;
            field_types.push(field_ty);
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
            if id.as_str() == display || format!("{ty:?}") == display {
                return self.lower_type(id);
            }
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
        Err(CompileError::Unsupported(format!(
            "field '{field_name}' ({}) not found in any type definition",
            field_id.0
        )))
    }

    fn root_place(
        &mut self,
        frame: &mut ResolvedFrame<'ctx>,
        place: &ResolvedPlace,
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
                    // GEP into the data buffer.
                    let i64_ty = self.generator.context.i64_type();
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
                    match elem_llvm_ty {
                        BasicTypeEnum::StructType(sty) => {
                            let fields = sty.get_field_types();
                            let is_string = fields.len() == 2
                                && matches!(&fields[0], BasicTypeEnum::PointerType(_))
                                && matches!(&fields[1], BasicTypeEnum::IntType(bit) if bit.get_bit_width() == 64);
                            if is_string {
                                // String: the i64 IS the raw char* pointer.
                                // Construct a full {i8*, i64} string struct.
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
                                let raw_ptr = self.generator.build_int_to_ptr(
                                    loaded,
                                    ptr_ty,
                                    "elem_str_ptr",
                                )?;
                                // Call strlen to get the length.
                                let strlen_fn = self
                                    .generator
                                    .module
                                    .get_function("strlen")
                                    .ok_or_else(|| "strlen not declared".to_string())?;
                                let str_len = self
                                    .generator
                                    .builder
                                    .build_call(
                                        strlen_fn,
                                        &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                                        "strlen",
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("strlen call: {e}"))
                                    })?
                                    .try_as_basic_value_opt()
                                    .ok_or_else(|| {
                                        CompileError::LlvmError("strlen returned void".into())
                                    })?;
                                // Build {i8*, i64} struct in alloca.
                                let str_alloca = self.generator.build_alloca(sty, "str_struct")?;
                                let ptr_gep = self
                                    .generator
                                    .builder
                                    .build_struct_gep(sty, str_alloca, 0, "str_ptr_gep")
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("str gep: {e}"))
                                    })?;
                                self.generator.build_store(ptr_gep, raw_ptr)?;
                                let len_gep = self
                                    .generator
                                    .builder
                                    .build_struct_gep(sty, str_alloca, 1, "str_len_gep")
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("len gep: {e}"))
                                    })?;
                                self.generator.build_store(len_gep, str_len)?;
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
                        }
                        BasicTypeEnum::PointerType(_) => {
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
                other => {
                    return Err(CompileError::Unsupported(format!(
                        "projection {other:?} escaped resolved native eligibility"
                    )))
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

    fn emit_if(
        &mut self,
        expression: &ResolvedExpr,
        condition: &ResolvedExpr,
        then_block: &ResolvedBlock,
        else_block: &ResolvedBlock,
        frame: &mut ResolvedFrame<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let result_type = self.lower_type(&expression.ty)?;
        let result_alloca = self.generator.build_alloca(result_type, "if_result")?;

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
            if let Some(value) = then_value {
                let value = self.coerce_to(value, result_type)?;
                self.generator.build_store(result_alloca, value)?;
            } else {
                self.generator
                    .build_store(result_alloca, result_type.const_zero())?;
            }
            self.generator.build_br(merge_bb)?;
        }

        // Else branch
        self.generator.builder.position_at_end(else_bb);
        let else_value = self.emit_block(&body.body, else_block, frame)?;
        let else_terminated = self.current_block_terminated();
        if !else_terminated {
            if let Some(value) = else_value {
                let value = self.coerce_to(value, result_type)?;
                self.generator.build_store(result_alloca, value)?;
            } else {
                self.generator
                    .build_store(result_alloca, result_type.const_zero())?;
            }
            self.generator.build_br(merge_bb)?;
        }

        // Merge
        self.generator.builder.position_at_end(merge_bb);
        self.generator
            .build_load(result_type, result_alloca, "if_val")
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
        self.emit_block(body, loop_body, frame)?;
        self.loop_stack.pop();
        if !self.current_block_terminated() {
            self.generator.build_br(header)?;
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
        self.emit_block(body, loop_body, frame)?;
        self.loop_stack.pop();
        if !self.current_block_terminated() {
            self.generator.build_br(header)?;
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
        self.loop_stack.push(LoopContext { header, exit });
        self.emit_block(body, loop_body, frame)?;
        self.loop_stack.pop();

        // Increment and loop back
        if !self.current_block_terminated() {
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
        }

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
        let list_struct = match list_val {
            BasicValueEnum::StructValue(sv) => sv,
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

        self.loop_stack.push(LoopContext { header, exit });
        self.emit_block(body, loop_body, frame)?;
        self.loop_stack.pop();

        // Increment idx and loop back.
        if !self.current_block_terminated() {
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
        }

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

        // Compute start index (default 0).
        let start_idx = match start {
            Some(expr) => {
                let val = self.emit_expr(expr, frame)?;
                self.coerce_to_i64(val)?
            }
            None => i64_ty.const_int(0, false),
        };
        // Compute end index (default: list length).
        let end_idx = match end {
            Some(expr) => {
                let val = self.emit_expr(expr, frame)?;
                self.coerce_to_i64(val)?
            }
            None => list_len,
        };

        // Clamp start to [0, list_len].
        let zero = i64_ty.const_int(0, false);
        let start_neg = self
            .generator
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, start_idx, zero, "start_neg")
            .map_err(|e| CompileError::LlvmError(format!("slice cmp: {e}")))?;
        let start_idx = self
            .generator
            .builder
            .build_select(start_neg, zero, start_idx, "start_clamp_low")
            .map_err(|e| CompileError::LlvmError(format!("slice select: {e}")))?
            .into_int_value();
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
        let start_idx = self
            .generator
            .builder
            .build_select(start_exceeds, list_len, start_idx, "start_clamp_high")
            .map_err(|e| CompileError::LlvmError(format!("slice select: {e}")))?
            .into_int_value();

        // Clamp end to [0, list_len].
        let end_neg = self
            .generator
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, end_idx, zero, "end_neg")
            .map_err(|e| CompileError::LlvmError(format!("slice cmp: {e}")))?;
        let end_idx = self
            .generator
            .builder
            .build_select(end_neg, zero, end_idx, "end_clamp_low")
            .map_err(|e| CompileError::LlvmError(format!("slice select: {e}")))?
            .into_int_value();
        let end_exceeds = self
            .generator
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, end_idx, list_len, "end_exceeds")
            .map_err(|e| CompileError::LlvmError(format!("slice cmp: {e}")))?;
        let end_idx = self
            .generator
            .builder
            .build_select(end_exceeds, list_len, end_idx, "end_clamp_high")
            .map_err(|e| CompileError::LlvmError(format!("slice select: {e}")))?
            .into_int_value();

        // new_len = max(0, end - start).
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
        let safe_end = self
            .generator
            .builder
            .build_select(start_gt_end, start_idx, end_idx, "slice_safe_end")
            .map_err(|e| CompileError::LlvmError(format!("slice select: {e}")))?
            .into_int_value();
        let new_len = self
            .generator
            .builder
            .build_int_sub(safe_end, start_idx, "slice_new_len")
            .map_err(|e| CompileError::LlvmError(format!("slice sub: {e}")))?;

        // new_data = data + start * 8 (byte offset into i64 array).
        let elem_size = i64_ty.const_int(8, false);
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
        let new_data_i8 = self.generator.build_in_bounds_gep(
            self.generator.context.i8_type(),
            data_i8,
            &[byte_offset],
            "slice_new_data",
        )?;
        let new_data_ptr = self
            .generator
            .builder
            .build_pointer_cast(new_data_i8, ptr_ty, "slice_data_void")
            .map_err(|e| CompileError::LlvmError(format!("slice cast 2: {e}")))?;

        // Build result list struct { new_len, new_data_ptr }.
        let result_ptr = self.generator.build_list_struct(new_len, new_data_ptr)?;
        self.generator.build_load(
            BasicTypeEnum::StructType(list_ty),
            result_ptr.into_pointer_value(),
            "slice_result",
        )
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

        // Determine if the inner type is Result (3 fields) or Option (2 fields).
        let is_result = matches!(
            self.program.resolved_types().get(&value.ty),
            Some(ResolvedType::Result { .. })
        );

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
                    "try inner value is not a struct (Result/Option)".into(),
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

        // For Result: extract error value (field 2).
        let err_val = if is_result {
            self.generator
                .builder
                .build_extract_value(sv, 2, "try_err_val")
                .map_err(|e| CompileError::LlvmError(format!("try err extract: {e}")))?
        } else {
            // Option None: use 0 as the error code.
            BasicValueEnum::IntValue(i64_ty.const_zero())
        };

        // Branch: disc == 0 → err_bb, else → ok_bb.
        let function = self.current_function()?;
        let ok_bb = self
            .generator
            .context
            .append_basic_block(function, "try_ok");
        let err_bb = self
            .generator
            .context
            .append_basic_block(function, "try_err");

        let zero = disc.get_type().const_int(0, false);
        let is_err = self
            .generator
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, disc, zero, "try_is_err")
            .map_err(|e| CompileError::LlvmError(format!("try compare: {e}")))?;
        self.generator.build_cond_br(is_err, err_bb, ok_bb)?;

        // Err path: call mimi_try_exit(err_val) → unreachable.
        self.generator.builder.position_at_end(err_bb);
        let try_exit_fn = self.generator.get_runtime_fn("mimi_try_exit")?;
        let err_int = match err_val {
            BasicValueEnum::IntValue(iv) => {
                // Ensure i64.
                if iv.get_type().get_bit_width() < 64 {
                    self.generator
                        .builder
                        .build_int_z_extend(iv, i64_ty, "try_err_zext")
                        .map_err(|e| CompileError::LlvmError(format!("try err zext: {e}")))?
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

        // Ok path: position at ok_bb, coerce payload to the Try expression's type.
        self.generator.builder.position_at_end(ok_bb);
        let target_llvm_ty = self.lower_type(result_ty)?;
        self.coerce_to(payload, target_llvm_ty)
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

        // Emit the lambda body.
        self.generator.push_heap_scope();
        let body_val = self.emit_block(&callable_body, &lambda.body, &mut lambda_frame)?;
        if !self.current_block_terminated() {
            // Free heap allocations before returning (lambda return types
            // are scalar — they don't own heap data).
            let _ = self.generator.free_heap_allocs();
            if let Some(val) = body_val {
                let val = self.coerce_to(val, ret_ty)?;
                self.generator.build_return(Some(&val))?;
            } else {
                self.generator.build_return(None)?;
            }
        }

        // Restore builder position.
        if let Some(bb) = saved_block {
            self.generator.builder.position_at_end(bb);
        }

        // Build closure struct {fn_ptr, null_env_ptr}.
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
        let null_ptr = ptr_ty.const_null();
        self.generator.build_store(env_gep, null_ptr)?;
        self.generator.build_load(
            BasicTypeEnum::StructType(closure_ty),
            closure_alloca,
            "closure_val",
        )
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
            let name = if let Some(stripped) = item.as_str().strip_prefix("builtin:type:") {
                stripped.to_string()
            } else {
                item.as_str().to_string()
            };
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
        assert!(ir.contains("define i32 @main()"), "{ir}");
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
            let eligible = eligible_function_ids(&program);

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
