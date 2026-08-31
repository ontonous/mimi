use crate::ast::*;
use crate::codegen::types;
use crate::codegen::{CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::types::BasicTypeEnum;
use inkwell::values::BasicValueEnum;
use std::collections::{BTreeMap, HashMap};

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_lambda_expr(
        &mut self,
        params: &[Param],
        ret: &Option<Type>,
        body: &Block,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let param_names: std::collections::HashSet<String> =
            params.iter().map(|p| p.name.clone()).collect();
        let mut free_vars = BTreeMap::new();
        self.collect_free_vars(body, &param_names, vars, &mut free_vars);

        let ret_type = self.lambda_ret_type(ret);
        let param_types_llvm = self.lambda_param_types(params);
        let fn_type = lambda_fn_type(self.context, ret_type, &param_types_llvm);

        let lambda_name = format!("__lambda_{}_{}", self.spawn_counter, body.len());
        self.spawn_counter += 1;
        let lambda_fn = self.module.add_function(&lambda_name, fn_type, None);
        let entry = self.context.append_basic_block(lambda_fn, "entry");
        let saved_block = self.builder.get_insert_block();
        self.builder.position_at_end(entry);

        // H2 (audit-codegen): an escaping lambda is a STANDALONE LLVM
        // function, but it is compiled while the enclosing fallible
        // transition's flag set is live — a trap inside the lambda would
        // otherwise emit a wrong-typed fault return (ABI garbage / UB:
        // `define i32 @__lambda_N` returning `{i32,i64}` fault records).
        // Clear the transition context for the lambda body, restore after.
        let saved_mt = self.in_multi_target_transition;
        let saved_states = std::mem::take(&mut self.multi_target_states);
        let saved_from = std::mem::take(&mut self.current_from_state);
        let saved_ret_ty_ast = self.current_fn_ret_ty_ast.clone();
        self.in_multi_target_transition = false;
        self.current_fn_ret_ty_ast = ret.clone();

        let mut lambda_vars = vars.clone();
        let env_ptr_param = lambda_fn
            .get_nth_param(0)
            .ok_or_else(|| "codegen: lambda env_ptr param index out of range".to_string())?
            .into_pointer_value();
        self.load_captured_vars(&free_vars, env_ptr_param, &mut lambda_vars)?;
        self.bind_lambda_params(params, lambda_fn, &mut lambda_vars)?;

        let body_result = self.emit_lambda_body(body, ret_type, &mut lambda_vars);
        self.in_multi_target_transition = saved_mt;
        self.multi_target_states = saved_states;
        self.current_from_state = saved_from;
        self.current_fn_ret_ty_ast = saved_ret_ty_ast;
        body_result?;

        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        self.build_closure_struct(lambda_fn, &free_vars)
    }

    /// F-004 (0.40.1.8): does the returned `expr` name a `Lambda` that captures a
    /// heap-collection-typed free variable (List/Set/Map, possibly nested inside
    /// Option/Result/Tuple)? Used to fail-closed returning such a closure on the
    /// native backend — the captured data array is freed when the enclosing scope
    /// exits (closure captures have no escape-set-null like direct returns do), so
    /// the escaped closure is left with a dangling pointer (use-after-free when
    /// later called). The check uses the precise Mimi type name of each captured
    /// variable (from `var_type_names`); it deliberately does NOT inspect the LLVM
    /// layout, which under LLVM 18 opaque pointers cannot distinguish a `List`
    /// handle from a scalar local's alloca, nor a captured closure struct.
    pub(in crate::codegen) fn returned_closure_captures_heap(
        &self,
        expr: Option<&Expr>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<bool, CompileError> {
        let (params, body) = match expr.and_then(|e| match e.unlocated() {
            Expr::Lambda { params, body, .. } => Some((params, body)),
            _ => None,
        }) {
            Some(pb) => pb,
            None => return Ok(false),
        };
        let param_names: std::collections::HashSet<String> =
            params.iter().map(|p| p.name.clone()).collect();
        let mut free_vars = BTreeMap::new();
        self.collect_free_vars(body, &param_names, vars, &mut free_vars);
        Ok(free_vars.keys().any(|name| {
            self.var_types
                .get(name)
                .cloned()
                .or_else(|| {
                    self.var_type_names
                        .get(name)
                        .map(|display| crate::codegen::parse_inner_type(display))
                })
                .is_some_and(|ty| {
                    crate::codegen::abi::ownership::classify_surface(&ty, &self.type_defs)
                        .contains_heap_collection()
                })
        }))
    }

    /// I-H13: compile a nested `func` statement.
    /// - No free vars: standalone LLVM function (existing dual_nested_func path).
    /// - With free vars: lambda-style closure bound as a local of type `Func`.
    pub(in crate::codegen) fn compile_nested_func_stmt(
        &mut self,
        f: &FuncDef,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> Result<(), CompileError> {
        let mut param_names: std::collections::HashSet<String> =
            f.params.iter().map(|p| p.name.clone()).collect();
        param_names.insert(f.name.clone());
        let mut free_vars = BTreeMap::new();
        self.collect_free_vars(&f.body, &param_names, vars, &mut free_vars);

        if free_vars.is_empty() {
            // V-11 (audit 2026-08-05): a nested func whose bare name already
            // resolves to another definition (same-named global or earlier
            // sibling) SHADOWS it for the rest of the enclosing body — the
            // checker registers the nested signature in its bare-name
            // directory at the declaration (check_stmt.rs Stmt::Func). The
            // LLVM symbol namespace is flat, so the shadow is emitted under
            // a mangled symbol; `nested_shadow_symbols` redirects bare-name
            // calls and the displaced func_defs entry is swapped so argument
            // coercion/named-arg helpers see the NESTED signature. Both are
            // restored by the compile_func_legacy frame guard.
            let shadows_existing =
                self.func_defs.contains_key(&f.name) || self.module.get_function(&f.name).is_some();
            let compile_target: FuncDef = if shadows_existing {
                self.nested_shadow_counter += 1;
                let enclosing = if self.current_legacy_fn.is_empty() {
                    "module".to_string()
                } else {
                    self.current_legacy_fn.clone()
                };
                let mangled = format!(
                    "{}#nested#{}#{}",
                    enclosing, f.name, self.nested_shadow_counter
                );
                let mut renamed = f.clone();
                renamed.name = mangled.clone();
                let prior = self.func_defs.insert(f.name.clone(), renamed.clone());
                self.func_defs.insert(mangled.clone(), renamed.clone());
                self.nested_shadow_symbols
                    .insert(f.name.clone(), (mangled, prior));
                renamed
            } else {
                // Capture-free, unshadowed: keep prior dual-backend path
                // (named LLVM function).
                self.func_defs
                    .entry(f.name.clone())
                    .or_insert_with(|| f.clone());
                f.clone()
            };
            let saved_block = self.builder.get_insert_block();
            let saved_type_map = self.type_map.clone();
            let saved_var_types = std::mem::take(&mut self.var_types);
            let saved_var_type_names = std::mem::take(&mut self.var_type_names);
            let saved_list_elem = std::mem::take(&mut self.list_elem_llvm_types);
            // H2 (audit-codegen): same standalone-function guard as
            // compile_lambda_expr — a nested func compiled inside a fallible
            // transition must not inherit the fault-return context.
            let saved_mt = self.in_multi_target_transition;
            let saved_states = std::mem::take(&mut self.multi_target_states);
            let saved_from = std::mem::take(&mut self.current_from_state);
            self.in_multi_target_transition = false;
            let result = self.compile_func_legacy(&compile_target);
            self.in_multi_target_transition = saved_mt;
            self.multi_target_states = saved_states;
            self.current_from_state = saved_from;
            result?;
            self.var_types = saved_var_types;
            self.var_type_names = saved_var_type_names;
            self.list_elem_llvm_types = saved_list_elem;
            self.type_map = saved_type_map;
            if let Some(bb) = saved_block {
                self.builder.position_at_end(bb);
            }
            return Ok(());
        }

        // Capturing nested func → closure value in local scope.
        let closure_val = self.compile_lambda_expr(&f.params, &f.ret, &f.body, vars)?;
        let closure_ty = types::closure_struct_type(self.context);
        let alloca = self.build_alloca(BasicTypeEnum::StructType(closure_ty), &f.name)?;
        self.build_store(alloca, closure_val)?;
        vars.insert(
            f.name.clone(),
            (alloca, BasicTypeEnum::StructType(closure_ty)),
        );

        let param_tys: Vec<Type> = f.params.iter().map(|p| p.ty.clone()).collect();
        let ret_ty = f
            .ret
            .clone()
            .unwrap_or_else(|| Type::Name("i32".into(), vec![]));
        self.var_types
            .insert(f.name.clone(), Type::Func(param_tys, Box::new(ret_ty)));
        Ok(())
    }

    /// Load captured variables from the env struct into the lambda's local scope.
    fn load_captured_vars(
        &self,
        free_vars: &BTreeMap<String, VarEntry<'ctx>>,
        env_ptr_param: inkwell::values::PointerValue<'ctx>,
        lambda_vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> Result<(), CompileError> {
        if free_vars.is_empty() {
            return Ok(());
        }
        let env_struct_type = env_struct_type_for(self.context, free_vars);
        let env_struct_ptr = self.build_pointer_cast(
            env_ptr_param,
            self.context.ptr_type(inkwell::AddressSpace::default()),
            "env_struct",
        )?;
        for (i, (name, &(_, ty))) in free_vars.iter().enumerate() {
            let field_gep = self
                .gep()
                .build_struct_gep(
                    env_struct_type,
                    env_struct_ptr,
                    i as u32,
                    &format!("env_{}_gep", name),
                )
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let field_val = self.build_load(ty, field_gep, &format!("cap_{}", name))?;
            let alloca = self.build_alloca(ty, &format!("cap_{}_alloca", name))?;
            self.build_store(alloca, field_val)?;
            lambda_vars.insert(name.clone(), (alloca, ty));
        }
        Ok(())
    }

    /// Store regular lambda parameters into stack allocas.
    fn bind_lambda_params(
        &mut self,
        params: &[Param],
        lambda_fn: inkwell::values::FunctionValue<'ctx>,
        lambda_vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> Result<(), CompileError> {
        for (i, p) in params.iter().enumerate() {
            let param_idx = i as u32 + 1;
            let ty = self
                .llvm_type_for(&p.ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
            let alloca = self.build_alloca(ty, &p.name)?;
            let param_val = lambda_fn
                .get_nth_param(param_idx)
                .ok_or_else(|| "codegen: lambda param index out of range".to_string())?;
            self.build_store(alloca, param_val)?;
            lambda_vars.insert(p.name.clone(), (alloca, ty));
            // Track type name for field access and method dispatch
            if let Type::Name(tn, _) = p.ty.unlocated() {
                self.var_type_names.insert(p.name.clone(), tn.clone());
                self.var_types.insert(p.name.clone(), p.ty.clone());
            }
        }
        Ok(())
    }

    /// Compile the lambda body and emit a final return if needed.
    fn emit_lambda_body(
        &mut self,
        body: &Block,
        ret_type: BasicTypeEnum<'ctx>,
        lambda_vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> Result<(), CompileError> {
        self.begin_function_heap_scope();
        let mut last_val = default_ret_value(self.context, ret_type);
        let mut last_expr: Option<&Expr> = None;
        let mut returned = false;
        for (stmt_index, stmt) in body.iter().enumerate() {
            let is_tail = stmt_index + 1 == body.len();
            match stmt.unlocated() {
                Stmt::Expr(e) => {
                    let ret_ty_ast = self.current_fn_ret_ty_ast.clone();
                    last_val = if is_tail {
                        self.compile_expr_for_return(e, lambda_vars, ret_type, ret_ty_ast.as_ref())?
                    } else {
                        self.compile_expr(e, lambda_vars)?
                    };
                    last_expr = Some(e);
                }
                Stmt::Return(Some(e)) => {
                    let ret_ty_ast = self.current_fn_ret_ty_ast.clone();
                    let v = self.compile_expr_for_return(
                        e,
                        lambda_vars,
                        ret_type,
                        ret_ty_ast.as_ref(),
                    )?;
                    let v = self.load_return_value_if_needed(v)?;
                    let v = self.coerce_variant_value(
                        v,
                        ret_type,
                        self.current_fn_ret_ty_ast.as_ref(),
                    )?;
                    // Claim string/tuple return values so free_heap_allocs
                    // doesn't free them before the caller receives them.
                    let claimed =
                        self.claim_string_return_value(v, ret_type, Some(e), lambda_vars)?;
                    let claimed = self.clone_surface_structural_return_with_glue(
                        self.current_fn_ret_ty_ast.as_ref(),
                        claimed,
                    )?;
                    // L6: claim a returned custom-enum payload box (caller
                    // re-registers via EnumBox). Mirrors func.rs emit_return.
                    self.claim_returned_enum_box(claimed, ret_type)?;
                    self.flush_heap_scopes_to_boundary()?;
                    self.build_return(Some(&claimed))?;
                    returned = true;
                    break;
                }
                Stmt::Return(None) => {
                    self.flush_heap_scopes_to_boundary()?;
                    // GENERIC-RET-ALIGN: a bare `return` must still satisfy the
                    // declared signature (unit lambdas lower to an i64 slot —
                    // `ret void` is invalid IR that O1's CVP crashes on).
                    let zero = self.zero_value_for(ret_type);
                    self.build_return(Some(&zero))?;
                    returned = true;
                    break;
                }
                Stmt::Let {
                    pat,
                    init: Some(init),
                    ..
                } => {
                    let val = self.compile_expr(init, lambda_vars)?;
                    let val = self.normalize_string_value(val, init)?;
                    self.compile_pattern_bind(pat, val, lambda_vars)
                        .map_err(|e| {
                            CompileError::LlvmError(format!("pattern bind error: {}", e))
                        })?;
                }
                Stmt::If { cond, then_, else_ } => {
                    let ret_ty_ast = self.current_fn_ret_ty_ast.clone();
                    let value = if is_tail {
                        self.compile_if_stmt_with_expected_return(
                            cond,
                            then_,
                            else_,
                            lambda_vars,
                            false,
                            ret_ty_ast.as_ref().map(|ast| (ret_type, ast)),
                        )?
                    } else {
                        self.compile_if_stmt(cond, then_, else_, lambda_vars, false)?
                    };
                    if let Some(v) = value {
                        let v = self.adjust_int_value_width(v, ret_type, "lambda_if_ret")?;
                        last_val = v;
                        last_expr = None;
                    }
                }
                Stmt::Assign { target, value } => {
                    self.compile_assign_stmt(target, value, lambda_vars)?;
                }
                Stmt::While { cond, body } => {
                    self.compile_while_stmt(cond, body, lambda_vars)?;
                }
                Stmt::For {
                    var,
                    iterable,
                    body,
                } => {
                    self.compile_for_stmt(var, iterable, body, lambda_vars)?;
                }
                Stmt::Block(block) => {
                    self.compile_block(block, lambda_vars)?;
                }
                _ => {}
            }
        }
        if !returned && !self.block_has_terminator() {
            // GENERIC-RET-ALIGN (0.39.x sweep): the tail expression may be a
            // narrower integer than the declared signature slot (observed:
            // compose's inner lambda returning i32 against an i64 signature).
            // The If-arm path above already adjusts widths — do the same for
            // the tail expression so `ret <mismatch>` never reaches O1.
            let last_val = self.adjust_int_value_width(last_val, ret_type, "lambda_tail_ret")?;
            let last_val = self.load_return_value_if_needed(last_val)?;
            let last_val =
                self.coerce_variant_value(last_val, ret_type, self.current_fn_ret_ty_ast.as_ref())?;
            let claimed =
                self.claim_string_return_value(last_val, ret_type, last_expr, lambda_vars)?;
            let claimed = self.clone_surface_structural_return_with_glue(
                self.current_fn_ret_ty_ast.as_ref(),
                claimed,
            )?;
            // L6: claim a returned custom-enum payload box (caller re-registers
            // via EnumBox). Mirrors the explicit-return path above.
            self.claim_returned_enum_box(claimed, ret_type)?;
            self.flush_heap_scopes_to_boundary()?;
            self.build_return(Some(&claimed))?;
        } else if !returned {
            // block_has_terminator but not via return (e.g. panic macro)
            // Still need to pop the heap scopes — bookkeeping only, since
            // the block is terminated (unreachable) and no free calls can
            // be emitted after the terminator.
            let boundary = self
                .heap_boundaries
                .borrow()
                .last()
                .copied()
                .unwrap_or(self.heap_allocs.borrow().len().saturating_sub(1));
            while self.heap_allocs.borrow().len() > boundary {
                self.heap_allocs.borrow_mut().pop();
            }
        }
        self.end_function_heap_scope();
        Ok(())
    }

    /// Build and return the closure struct { fn_ptr: i8*, env_ptr: i8* }.
    fn build_closure_struct(
        &mut self,
        lambda_fn: inkwell::values::FunctionValue<'ctx>,
        free_vars: &BTreeMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let closure_struct_type = types::closure_struct_type(self.context);
        let closure_alloca =
            self.build_alloca(BasicTypeEnum::StructType(closure_struct_type), "closure")?;

        let fn_ptr = lambda_fn.as_global_value().as_pointer_value();
        let fn_gep = self
            .gep()
            .build_struct_gep(closure_struct_type, closure_alloca, 0, "fn_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(fn_gep, fn_ptr)?;

        let env_gep = self
            .gep()
            .build_struct_gep(closure_struct_type, closure_alloca, 1, "env_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        if free_vars.is_empty() {
            self.build_store(env_gep, i8_ptr.const_null())?;
        } else {
            let env_heap_ptr = self.allocate_closure_env(free_vars)?;
            // Cast first, then register the cast pointer (MEM-C13).
            // free_heap_allocs / claim compare by LLVM value identity; the
            // pointer extracted from the closure struct is the cast value.
            let env_ptr_i8 = self.build_pointer_cast(env_heap_ptr, i8_ptr, "env_ptr_i8")?;
            // Register so non-escaping closures free their env at scope exit.
            // Escaping returns claim via claim_returned_closure_env.
            self.register_heap_alloc(env_ptr_i8);
            self.build_store(env_gep, env_ptr_i8)?;
        }

        self.build_load(
            BasicTypeEnum::StructType(closure_struct_type),
            closure_alloca,
            "closure_val",
        )
    }

    /// Allocate and populate the closure environment struct on the heap.
    ///
    /// MEM-C13: caller must `register_heap_alloc` the cast `i8*` env pointer
    /// (or claim it on escape). Raw malloc here is not tracked.
    fn allocate_closure_env(
        &self,
        free_vars: &BTreeMap<String, VarEntry<'ctx>>,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        let env_field_types: Vec<BasicTypeEnum<'ctx>> =
            free_vars.values().map(|&(_, ty)| ty).collect();
        let env_struct_type = self.context.struct_type(&env_field_types, false);
        let env_byte_size = env_struct_type
            .size_of()
            .ok_or_else(|| "size_of error".to_string())?;
        // B4: use malloc_or_abort so OOM aborts instead of null-deref.
        let env_heap_ptr = self.malloc_or_abort(env_byte_size, "env_heap")?;

        for (i, (name, &(var_alloca, ty))) in free_vars.iter().enumerate() {
            let val = self.build_load(ty, var_alloca, &format!("cap_val_{}", name))?;
            let field_gep = self
                .gep()
                .build_struct_gep(
                    env_struct_type,
                    env_heap_ptr,
                    i as u32,
                    &format!("env_{}_gep", name),
                )
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            self.build_store(field_gep, val)?;
        }
        Ok(env_heap_ptr)
    }

    /// Collect free variables used in a block that are defined in the enclosing scope
    pub(in crate::codegen) fn collect_free_vars(
        &self,
        block: &Block,
        param_names: &std::collections::HashSet<String>,
        vars: &HashMap<String, VarEntry<'ctx>>,
        free_vars: &mut BTreeMap<
            String,
            (inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>),
        >,
    ) {
        let mut defined = param_names.clone();
        for stmt in block {
            match stmt.unlocated() {
                Stmt::Expr(e) => self.collect_free_vars_expr(e, &defined, vars, free_vars),
                Stmt::Let {
                    pat,
                    init: Some(init),
                    ..
                } => {
                    self.collect_free_vars_expr(init, &defined, vars, free_vars);
                    if let PatternKind::Variable(name) = &pat.kind {
                        defined.insert(name.clone());
                    }
                }
                Stmt::Return(Some(e)) => self.collect_free_vars_expr(e, &defined, vars, free_vars),
                Stmt::If { cond, then_, else_ } => {
                    self.collect_free_vars_expr(cond, &defined, vars, free_vars);
                    self.collect_free_vars(then_, &defined, vars, free_vars);
                    if let Some(eb) = else_ {
                        self.collect_free_vars(eb, &defined, vars, free_vars);
                    }
                }
                Stmt::Assign { target, value } => {
                    self.collect_free_vars_expr(target, &defined, vars, free_vars);
                    self.collect_free_vars_expr(value, &defined, vars, free_vars);
                }
                Stmt::While { cond, body } => {
                    self.collect_free_vars_expr(cond, &defined, vars, free_vars);
                    self.collect_free_vars(body, &defined, vars, free_vars);
                }
                Stmt::WhileLet { init, body, .. } => {
                    self.collect_free_vars_expr(init, &defined, vars, free_vars);
                    self.collect_free_vars(body, &defined, vars, free_vars);
                }
                Stmt::Loop(body) => {
                    self.collect_free_vars(body, &defined, vars, free_vars);
                }
                Stmt::For { iterable, body, .. } => {
                    self.collect_free_vars_expr(iterable, &defined, vars, free_vars);
                    self.collect_free_vars(body, &defined, vars, free_vars);
                }
                Stmt::Block(block) => {
                    self.collect_free_vars(block, &defined, vars, free_vars);
                }
                Stmt::SharedLet { init, .. } => {
                    self.collect_free_vars_expr(init, &defined, vars, free_vars);
                }
                Stmt::Drop(expr) => {
                    self.collect_free_vars_expr(expr, &defined, vars, free_vars);
                }
                Stmt::OnFailure(block) | Stmt::Arena(block) | Stmt::Unsafe(block) => {
                    self.collect_free_vars(block, &defined, vars, free_vars);
                }
                Stmt::Parasteps(block) => {
                    self.collect_free_vars(block, &defined, vars, free_vars);
                }
                _ => {}
            }
        }
    }

    pub(in crate::codegen) fn collect_free_vars_expr(
        &self,
        expr: &Expr,
        defined: &std::collections::HashSet<String>,
        vars: &HashMap<String, VarEntry<'ctx>>,
        free_vars: &mut BTreeMap<
            String,
            (inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>),
        >,
    ) {
        match expr.unlocated() {
            Expr::Ident(name) => {
                if !defined.contains(name.as_str()) {
                    if let Some(&(ptr, ty)) = vars.get(name.as_str()) {
                        free_vars.entry(name.clone()).or_insert((ptr, ty));
                    }
                }
            }
            Expr::Binary(_, l, r) => {
                self.collect_free_vars_expr(l, defined, vars, free_vars);
                self.collect_free_vars_expr(r, defined, vars, free_vars);
            }
            Expr::Unary(_, e) => self.collect_free_vars_expr(e, defined, vars, free_vars),
            Expr::Call(callee, args) => {
                self.collect_free_vars_expr(callee, defined, vars, free_vars);
                for arg in args {
                    self.collect_free_vars_expr(arg, defined, vars, free_vars);
                }
            }
            Expr::Field(obj, _) => self.collect_free_vars_expr(obj, defined, vars, free_vars),
            Expr::Index(obj, idx) => {
                self.collect_free_vars_expr(obj, defined, vars, free_vars);
                self.collect_free_vars_expr(idx, defined, vars, free_vars);
            }
            Expr::List(elems) | Expr::Tuple(elems) => {
                for e in elems {
                    self.collect_free_vars_expr(e, defined, vars, free_vars);
                }
            }
            Expr::If { cond, then_, else_ } => {
                self.collect_free_vars_expr(cond, defined, vars, free_vars);
                self.collect_free_vars(then_, defined, vars, free_vars);
                if let Some(eb) = else_ {
                    self.collect_free_vars(eb, defined, vars, free_vars);
                }
            }
            Expr::Record { fields, .. } => {
                for f in fields {
                    self.collect_free_vars_expr(&f.value, defined, vars, free_vars);
                }
            }
            Expr::Spawn(inner) | Expr::Await(inner) | Expr::Try(inner) | Expr::Old(inner) => {
                self.collect_free_vars_expr(inner, defined, vars, free_vars);
            }
            Expr::Match(scrutinee, arms) => {
                self.collect_free_vars_expr(scrutinee, defined, vars, free_vars);
                for arm in arms {
                    self.collect_free_vars_expr(&arm.body, defined, vars, free_vars);
                }
            }
            Expr::SliceExpr { target, start, end } => {
                self.collect_free_vars_expr(target, defined, vars, free_vars);
                if let Some(s) = start {
                    self.collect_free_vars_expr(s, defined, vars, free_vars);
                }
                if let Some(e) = end {
                    self.collect_free_vars_expr(e, defined, vars, free_vars);
                }
            }
            Expr::Lambda { params, body, .. } => {
                let param_names: std::collections::HashSet<String> =
                    params.iter().map(|p| p.name.clone()).collect();
                let mut extended_defined = defined.clone();
                extended_defined.extend(param_names);
                self.collect_free_vars(body, &extended_defined, vars, free_vars);
            }
            Expr::Comprehension {
                expr: comp_expr,
                iter,
                guard,
                ..
            } => {
                self.collect_free_vars_expr(iter, defined, vars, free_vars);
                self.collect_free_vars_expr(comp_expr, defined, vars, free_vars);
                if let Some(g) = guard {
                    self.collect_free_vars_expr(g, defined, vars, free_vars);
                }
            }
            Expr::Turbofish(_, _, args) => {
                for arg in args {
                    self.collect_free_vars_expr(arg, defined, vars, free_vars);
                }
            }
            Expr::TupleIndex(inner, _) => {
                self.collect_free_vars_expr(inner, defined, vars, free_vars);
            }
            Expr::TypeOf(inner) => {
                self.collect_free_vars_expr(inner, defined, vars, free_vars);
            }
            Expr::Arena(block) => {
                self.collect_free_vars(block, defined, vars, free_vars);
            }
            Expr::Block(block) => {
                self.collect_free_vars(block, defined, vars, free_vars);
            }
            Expr::SetLiteral(elems) => {
                for e in elems {
                    self.collect_free_vars_expr(e, defined, vars, free_vars);
                }
            }
            Expr::MapLiteral { entries } => {
                for (k, v) in entries {
                    self.collect_free_vars_expr(k, defined, vars, free_vars);
                    self.collect_free_vars_expr(v, defined, vars, free_vars);
                }
            }
            _ => {}
        }
    }

    /// Determine LLVM parameter types for a lambda function (env_ptr + params),
    /// using self.type_llvm so user-defined record types are resolved correctly.
    fn lambda_param_types(&self, params: &[Param]) -> Vec<BasicTypeEnum<'ctx>> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let mut result = vec![BasicTypeEnum::PointerType(i8_ptr)];
        for p in params {
            result.push(
                self.llvm_type_for(&p.ty)
                    .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type())),
            );
        }
        result
    }

    /// Determine the LLVM return type for a lambda function. Uses
    /// `self.llvm_type_for` (which consults `type_llvm`) so user-defined custom
    /// enums/records resolve to their real layout (e.g. a custom enum →
    /// `{i32 tag, i64 payload}`) instead of the standalone `mimi_type_to_llvm`
    /// fallback that maps unknown names to `i64`. Without this, a lambda
    /// declared `-> Shape` is compiled as `define i64` while its body returns
    /// the `{i32,i64}` ctor result — a signature/body mismatch that miscompiles
    /// the closure call and the subsequent match.
    pub(in crate::codegen) fn lambda_ret_type(&self, ret: &Option<Type>) -> BasicTypeEnum<'ctx> {
        match ret {
            Some(ty) => self
                .llvm_type_for(ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type())),
            None => BasicTypeEnum::IntType(self.context.i64_type()),
        }
    }
}

pub(in crate::codegen) fn lambda_fn_type<'ctx>(
    context: &'ctx inkwell::context::Context,
    ret_type: BasicTypeEnum<'ctx>,
    param_types_llvm: &[BasicTypeEnum<'ctx>],
) -> inkwell::types::FunctionType<'ctx> {
    let metadata_params: Vec<_> = param_types_llvm
        .iter()
        .map(|t| types::basic_to_metadata(context, *t))
        .collect();
    match ret_type {
        BasicTypeEnum::IntType(t) => t.fn_type(&metadata_params, false),
        BasicTypeEnum::FloatType(t) => t.fn_type(&metadata_params, false),
        BasicTypeEnum::PointerType(t) => t.fn_type(&metadata_params, false),
        BasicTypeEnum::StructType(t) => t.fn_type(&metadata_params, false),
        BasicTypeEnum::ArrayType(t) => t.fn_type(&metadata_params, false),
        _ => context.i64_type().fn_type(&metadata_params, false),
    }
}

fn env_struct_type_for<'ctx>(
    context: &'ctx inkwell::context::Context,
    free_vars: &BTreeMap<String, VarEntry<'ctx>>,
) -> inkwell::types::StructType<'ctx> {
    let env_field_types: Vec<BasicTypeEnum<'ctx>> = free_vars.values().map(|&(_, ty)| ty).collect();
    context.struct_type(&env_field_types, false)
}

fn default_ret_value<'ctx>(
    context: &'ctx inkwell::context::Context,
    ret_type: BasicTypeEnum<'ctx>,
) -> BasicValueEnum<'ctx> {
    match ret_type {
        BasicTypeEnum::IntType(it) => it.const_int(0, false).into(),
        BasicTypeEnum::FloatType(ft) => ft.const_float(0.0).into(),
        _ => context.i64_type().const_int(0, false).into(),
    }
}
