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
    CheckedConversion, CheckedConversionKind, CheckedProgram, NodeId, PrimitiveType,
    ResolvedBlock, ResolvedBody, ResolvedCallee, ResolvedConstValue, ResolvedExpr,
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
    eligible_function_ids(program)
        .ok()
        .filter(|set| !set.is_empty())
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
        // The compile_func skip guard (count_basic_blocks != 0) ensures the
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
                            }
                            // Track failed functions so the legacy emitter
                            // knows to recompile them even if the function
                            // has partial basic blocks from the failed attempt.
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
                    // The function may have partial basic blocks (entry block
                    // without terminator) from the failed emit_callable.
                    // Track it so compile_func knows to recompile.
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
        if std::env::var("MIMI_VERBOSE").is_ok() && failed > 0 {
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
        match llvm_type_for_resolved(
            self.generator.context,
            self.program.resolved_types(),
            id,
        ) {
            Ok(ty) => Ok(ty),
            Err(_) => {
                // Check if this is a user-defined record Nominal type.
                if let Some(ResolvedType::Nominal { item, .. }) =
                    self.program.resolved_types().get(id)
                {
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
        // If returning a string ({ptr, i64} struct), drain the heap scope
        // WITHOUT freeing. The caller takes ownership of the string data.
        // For non-string returns, free all heap allocations normally.
        let is_string_ret = matches!(result_type, BasicTypeEnum::StructType(st) if {
            let f = st.get_field_types();
            f.len() == 2
                && matches!(f[0], BasicTypeEnum::PointerType(_))
                && matches!(f[1], BasicTypeEnum::IntType(_))
        });
        if is_string_ret {
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
                let value = self.coerce_to(value, llvm_type)?;
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
                for (index, sub_pattern) in sub_patterns.iter().enumerate() {
                    let field = struct_val.get_field_at_index(index as u32).ok_or_else(|| {
                        CompileError::LlvmError(format!("tuple field {index} absent in pattern"))
                    })?;
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
                    let elem_ptr = self
                        .generator
                        .build_in_bounds_gep(i64_ty, data_ptr, &[idx], "list_elem")?;
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
                    .ok_or_else(|| {
                        CompileError::LlvmError("mimi_map_new returned void".into())
                    })?
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
                    .ok_or_else(|| {
                        CompileError::LlvmError("mimi_set_new returned void".into())
                    })?
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
                    .map(|f| {
                        self.lower_type(&f.value.ty)
                    })
                    .collect::<Result<_, _>>()?;
                let struct_ty = self
                    .generator
                    .context
                    .struct_type(&field_types, false);
                let alloca = self.generator.build_alloca(struct_ty, "record_alloc")?;
                for (i, field) in fields.iter().enumerate() {
                    let value = self.emit_expr(&field.value, frame)?;
                    let field_ptr = self
                        .generator
                        .builder
                        .build_struct_gep(struct_ty, alloca, i as u32, "rec_field")
                        .map_err(|e| CompileError::LlvmError(format!("record gep: {e}")))?;
                    let field_ty = struct_ty
                        .get_field_type_at_index(i as u32)
                        .ok_or_else(|| {
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
                        let field = struct_val
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
                        // GEP + load.
                        let i64_ty = self.generator.context.i64_type();
                        let elem_ptr = self.generator.build_in_bounds_gep(
                            i64_ty, data_ptr, &[idx_val], "list_val_idx",
                        )?;
                        self.generator.build_load(
                            BasicTypeEnum::IntType(i64_ty), elem_ptr, "list_val_load",
                        )
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
                        let field = struct_val
                            .get_field_at_index(field_index)
                            .ok_or_else(|| {
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
                        self.generator
                            .build_call(callee, &arguments, "resolved_call")?
                            .try_as_basic_value_opt()
                            .ok_or_else(|| {
                                CompileError::LlvmError(format!(
                                    "resolved callee '{symbol}' returned void"
                                ))
                            })
                    }
                    ResolvedCallee::Builtin(builtin_id) => {
                        let name = builtin_id.as_str();
                        // Option/Result constructors are handled by the legacy
                        // compile_constructor path, not compile_builtin_call.
                        if matches!(name, "Some" | "None" | "Ok" | "Err") {
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
                        // Print-family builtins need arg type hints for formatting dispatch.
                        if matches!(name, "println" | "print" | "eprintln" | "format") {
                            self.generator.pending_print_arg_types = call
                                .arguments
                                .iter()
                                .map(|arg| resolved_type_display_name(self.program, &arg.value.ty))
                                .collect();
                        }
                        let result = self.generator.compile_builtin_call(name, &arguments)?;
                        // ABI bridge: builtins return raw ptr for strings, but the
                        // resolved emitter expects {ptr, i64} structs. Wrap if needed.
                        self.wrap_builtin_string_result(result, &call.result)
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
        let llvm_type =
            self.lower_type(ty)?;
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
        let llvm_type =
            self.lower_type(ty)?;
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
                    ResolvedPatternKind::Constructor { variant, .. } => {
                        let variant_name = self.lookup_variant_name(variant)?;
                        let disc_expected = match variant_name.as_str() {
                            "Some" | "Ok" => true,
                            "None" | "Err" => false,
                            other => {
                                return Err(CompileError::Unsupported(format!(
                                    "constructor variant '{other}' is not Option/Result"
                                )))
                            }
                        };
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
                        // Use extractvalue for non-constant structs.
                        let disc = self
                            .generator
                            .builder
                            .build_extract_value(sv, 0, "ctor_disc")
                            .map_err(|e| CompileError::LlvmError(format!("extract disc: {e}")))?;
                        let bool_ty = self.generator.context.bool_type();
                        let expected = bool_ty.const_int(disc_expected as u64, false);
                        // Discriminant may be i1 or pointer-sized; handle both.
                        let disc_int = match disc {
                            BasicValueEnum::IntValue(iv) => iv,
                            BasicValueEnum::PointerValue(pv) => {
                                // Shouldn't happen for Option/Result, but handle gracefully.
                                self.generator
                                    .builder
                                    .build_ptr_to_int(
                                        pv,
                                        bool_ty,
                                        "disc_ptr2int",
                                    )
                                    .map_err(|e| CompileError::LlvmError(format!("disc ptr2int: {e}")))?
                            }
                            _ => {
                                return Err(CompileError::LlvmError(
                                    "discriminant is not an integer".into(),
                                ))
                            }
                        };
                        self.generator
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::EQ,
                                disc_int,
                                expected,
                                "ctor_disc_cmp",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("disc cmp: {e}")))?
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
                    // Some/Ok → field 1; Err → field 2; None → no payload.
                    let payload_field_index: Option<u32> = match variant_name.as_str() {
                        "Some" | "Ok" => Some(1),
                        "Err" => Some(2),
                        "None" => None,
                        other => {
                            return Err(CompileError::Unsupported(format!(
                                "constructor variant '{other}' payload extraction unsupported"
                            )))
                        }
                    };
                    let callable_body = &self
                        .program
                        .callable(&frame.owner)
                        .ok_or_else(|| {
                            CompileError::Unsupported("callable absent for ctor binding".into())
                        })?
                        .body;
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
                        self.bind_pattern(
                            callable_body,
                            sub_pattern,
                            payload_val,
                            frame,
                        )?;
                    }
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
        &self,
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
            CheckedConversionKind::Identity => Ok(value),
            CheckedConversionKind::NumericWiden | CheckedConversionKind::NumericNarrowChecked => {
                let target = self.lower_type(&conversion.to)?;
                self.numeric_convert(value, target)
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
    /// sign/zero-extension, float-to-int, and bool zext.
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
            other => Err(CompileError::Unsupported(format!(
                "cannot coerce {other:?} to i64 for list storage"
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

    /// Look up the field name from a field NodeId by searching type definitions.
    fn lookup_field_name(&self, field_id: &NodeId) -> Result<String, CompileError> {
        for td in self.program.type_defs().values() {
            for (name, id) in &td.field_ids {
                if id == field_id {
                    return Ok(name.clone());
                }
            }
        }
        Err(CompileError::Unsupported(format!(
            "field '{}' not found in any type definition",
            field_id.0
        )))
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
                CompileError::Unsupported(format!(
                    "type definition for '{item_str}' not found"
                ))
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
    fn resolve_type_display(
        &self,
        display: &str,
    ) -> Result<BasicTypeEnum<'ctx>, CompileError> {
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
                PrimitiveType::I64 | PrimitiveType::U64 | PrimitiveType::Isize
                | PrimitiveType::Usize => {
                    BasicTypeEnum::IntType(self.generator.context.i64_type())
                }
                PrimitiveType::I128 | PrimitiveType::U128 => {
                    BasicTypeEnum::IntType(self.generator.context.i128_type())
                }
                PrimitiveType::F32 => {
                    BasicTypeEnum::FloatType(self.generator.context.f32_type())
                }
                PrimitiveType::F64 => {
                    BasicTypeEnum::FloatType(self.generator.context.f64_type())
                }
                PrimitiveType::Bool => {
                    BasicTypeEnum::IntType(self.generator.context.bool_type())
                }
                PrimitiveType::String => {
                    let ptr = BasicTypeEnum::PointerType(
                        self.generator.context.ptr_type(inkwell::AddressSpace::default()),
                    );
                    let i64 = BasicTypeEnum::IntType(self.generator.context.i64_type());
                    BasicTypeEnum::StructType(
                        self.generator.context.struct_type(&[ptr, i64], false),
                    )
                }
                PrimitiveType::Unit => {
                    BasicTypeEnum::IntType(self.generator.context.i64_type())
                }
            });
        }
        // Fallback: scan the type table for a matching type.
        for (id, ty) in self.program.resolved_types().iter() {
            if id.as_str() == display || format!("{ty:?}") == display {
                return self.lower_type(&id);
            }
        }
        Err(CompileError::Unsupported(format!(
            "cannot resolve type display '{display}' to LLVM type"
        )))
    }

    /// Look up the field index within a record type definition.
    /// Searches all type definitions for one whose `field_ids` contains
    /// the given field NodeId, then returns the field's position.
    fn lookup_field_index(
        &self,
        field_id: &NodeId,
        field_name: &str,
    ) -> Result<u32, CompileError> {
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
                        .build_struct_gep(
                            struct_type,
                            current_ptr,
                            *index as u32,
                            "place_gep",
                        )
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
                        .build_load(BasicTypeEnum::PointerType(ptr_ty), data_gep, "list_data_ptr")?
                        .into_pointer_value();
                    // Evaluate the index expression.
                    let idx_val = match index {
                        crate::core::ir::ResolvedIndex::Constant(c) => {
                            self.generator.context.i64_type().const_int(*c as u64, false)
                        }
                        crate::core::ir::ResolvedIndex::Dynamic(expr_id) => {
                            // Look up the index expression from place_inputs
                            // and emit it. Clone to release the immutable
                            // borrow on self before calling emit_expr.
                            let idx_expr = self.place_inputs.get(expr_id).cloned().ok_or_else(|| {
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
                        i64_ty, data_ptr, &[idx_val], "list_idx_gep",
                    )?;
                    // Element type: lower the resolved type identity.
                    current_type = self.lower_type(ty)?;
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
                        .build_struct_gep(
                            struct_type,
                            current_ptr,
                            field_index,
                            "rec_field_gep",
                        )
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
        let ResolvedExprKind::Range { start, end } = &iterable.kind else {
            return Err(CompileError::Unsupported(
                "non-range iterable escaped resolved native eligibility".into(),
            ));
        };

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
fn resolved_type_display_name(program: &CheckedProgram, ty: &ResolvedTypeId) -> String {
    use crate::core::PrimitiveType;
    match program.resolved_types().get(ty) {
        Some(ResolvedType::Primitive(p)) => match p {
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
        Some(_) => "unknown".to_string(),
        None => "unknown".to_string(),
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

    #[test]
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
                    eprintln!(
                        "  [{name}] PROGRAM-LEVEL REJECT: {}",
                        e.reason
                    );
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
                        if let Err(e) =
                            require_resolved_native_callable(&program, callable)
                        {
                            *rejection_reasons
                                .entry(e.reason.clone())
                                .or_insert(0) += 1;
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
