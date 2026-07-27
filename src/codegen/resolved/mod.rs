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
    CheckedConversion, CheckedConversionKind, CheckedProgram, NodeId, ResolvedBlock, ResolvedBody,
    ResolvedCallee, ResolvedConstValue, ResolvedExpr, ResolvedExprKind, ResolvedLiteral,
    ResolvedLocalId, ResolvedPattern, ResolvedPatternKind, ResolvedPlace, ResolvedStmtKind,
    ResolvedType, ResolvedTypeId,
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
                            failed += 1;
                            continue;
                        }
                    }
                    count += 1;
                }
                Err(e) => {
                    // Function failed to emit through resolved path.
                    // Legacy emitter will compile it (0 basic blocks → not skipped).
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

    fn declare_callable(
        &mut self,
        callable: &crate::core::ResolvedCallable,
    ) -> Result<(), CompileError> {
        let symbol = self.callable_symbol(&callable.owner)?.to_string();
        let result = llvm_type_for_resolved(
            self.generator.context,
            self.program.resolved_types(),
            &callable.signature.result,
        )?;
        let parameters = callable
            .signature
            .parameters
            .iter()
            .map(|parameter| {
                llvm_type_for_resolved(
                    self.generator.context,
                    self.program.resolved_types(),
                    &parameter.ty,
                )
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
        let mut frame = ResolvedFrame {
            owner: callable.owner.clone(),
            locals: BTreeMap::new(),
        };
        self.bind_parameters(callable, function, &mut frame)?;
        let value = self.emit_block(&callable.body, &callable.body.root, &mut frame)?;
        if self.current_block_terminated() {
            return Ok(());
        }
        let result_type = llvm_type_for_resolved(
            self.generator.context,
            self.program.resolved_types(),
            &callable.signature.result,
        )?;
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
            let llvm_type = llvm_type_for_resolved(
                self.generator.context,
                self.program.resolved_types(),
                &local.ty,
            )?;
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
                let llvm_type = llvm_type_for_resolved(
                    self.generator.context,
                    self.program.resolved_types(),
                    &metadata.ty,
                )?;
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
                let llvm_type = llvm_type_for_resolved(
                    self.generator.context,
                    self.program.resolved_types(),
                    &expression.ty,
                )?;
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
            ResolvedExprKind::Project { value, projection } => {
                let crate::core::ir::ResolvedValueProjection::Tuple(index) = projection else {
                    return Err(CompileError::Unsupported(
                        "non-tuple value projection escaped eligibility".into(),
                    ));
                };
                let agg = self.emit_expr(value, frame)?;
                let BasicValueEnum::StructValue(struct_val) = agg else {
                    return Err(CompileError::Unsupported(
                        "projected value is not a struct".into(),
                    ));
                };
                let field = struct_val
                    .get_field_at_index(*index as u32)
                    .ok_or_else(|| {
                        CompileError::LlvmError(format!("tuple field {index} absent"))
                    })?;
                Ok(field)
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
            llvm_type_for_resolved(self.generator.context, self.program.resolved_types(), ty)?;
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
            llvm_type_for_resolved(self.generator.context, self.program.resolved_types(), ty)?;
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
        let result_type = llvm_type_for_resolved(
            self.generator.context,
            self.program.resolved_types(),
            &expression.ty,
        )?;
        let result_alloca = self.generator.build_alloca(result_type, "match_result")?;
        let scrutinee_val = self.emit_expr(scrutinee, frame)?;

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
            if let ResolvedPatternKind::Binding {
                by_reference: None, ..
            } = &arm.pattern.kind
            {
                let callable_body = &self
                    .program
                    .callable(&frame.owner)
                    .ok_or_else(|| {
                        CompileError::Unsupported("callable absent for match binding".into())
                    })?
                    .body;
                self.bind_pattern(callable_body, &arm.pattern, scrutinee_val, frame)?;
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
                let target = llvm_type_for_resolved(
                    self.generator.context,
                    self.program.resolved_types(),
                    &conversion.to,
                )?;
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

    fn root_place(
        &mut self,
        frame: &ResolvedFrame<'ctx>,
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
        // Walk tuple projections via struct GEP.
        let mut current_ptr = base_entry.storage;
        let mut current_type = base_entry.llvm_type;
        for projection in &place.projections {
            let crate::core::ir::ResolvedProjection::Tuple { index, ty: _ } = projection else {
                return Err(CompileError::Unsupported(
                    "non-tuple place projection escaped eligibility".into(),
                ));
            };
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
            // Use the actual struct field type (which may be widened to i64
            // for legacy ABI compatibility) rather than re-lowering the
            // resolved type identity.
            current_type = struct_type
                .get_field_type_at_index(*index as u32)
                .ok_or_else(|| {
                    CompileError::LlvmError(format!(
                        "tuple field {index} absent in place projection"
                    ))
                })?;
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
        let result_type = llvm_type_for_resolved(
            self.generator.context,
            self.program.resolved_types(),
            &expression.ty,
        )?;
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
        let llvm_type = llvm_type_for_resolved(
            self.generator.context,
            self.program.resolved_types(),
            &metadata.ty,
        )?;
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
                let llvm_type = llvm_type_for_resolved(
                    self.generator.context,
                    self.program.resolved_types(),
                    &metadata.ty,
                )?;
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
    fn unsupported_list_fails_without_surface_fallback() {
        let program = checked(
            r#"
func main() -> i32 {
    let xs = [1, 2, 3]
    xs[0]
}
"#,
        );
        let context = inkwell::context::Context::create();
        let mut generator = CodeGenerator::new(&context, "resolved_reject");
        let diagnostics = generator
            .compile_resolved_native(&program)
            .expect_err("list literal is outside the current slice");
        assert!(
            diagnostics[0]
                .message
                .contains("resolved native slice rejected"),
            "{}",
            diagnostics[0].message
        );
        assert!(generator.module.get_function("main").is_none());
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
}
