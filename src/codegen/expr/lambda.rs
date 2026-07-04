use crate::ast::*;
use crate::codegen::types;
use crate::codegen::{CallSiteValueExt, CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
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

        let ret_type = lambda_ret_type(self.context, ret);
        let param_types_llvm = self.lambda_param_types(params);
        let fn_type = lambda_fn_type(self.context, ret_type, &param_types_llvm);

        let lambda_name = format!("__lambda_{}_{}", self.spawn_counter, body.len());
        self.spawn_counter += 1;
        let lambda_fn = self.module.add_function(&lambda_name, fn_type, None);
        let entry = self.context.append_basic_block(lambda_fn, "entry");
        let saved_block = self.builder.get_insert_block();
        self.builder.position_at_end(entry);

        let mut lambda_vars = vars.clone();
        let env_ptr_param = lambda_fn
            .get_nth_param(0)
            .ok_or_else(|| "codegen: lambda env_ptr param index out of range".to_string())?
            .into_pointer_value();

        self.load_captured_vars(&free_vars, env_ptr_param, &mut lambda_vars)?;
        self.bind_lambda_params(params, lambda_fn, &mut lambda_vars)?;
        self.emit_lambda_body(body, ret_type, &mut lambda_vars)?;

        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        self.build_closure_struct(lambda_fn, &free_vars)
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
            if let Type::Name(tn, _) = &p.ty {
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
        let mut last_val = default_ret_value(self.context, ret_type);
        for stmt in body {
            match stmt {
                Stmt::Expr(e) => {
                    last_val = self.compile_expr(e, lambda_vars)?;
                }
                Stmt::Return(Some(e)) => {
                    let v = self.compile_expr(e, lambda_vars)?;
                    let v = self.load_return_value_if_needed(v)?;
                    self.build_return(Some(&v))?;
                    break;
                }
                Stmt::Return(None) => {
                    self.build_return(None)?;
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
                _ => {}
            }
        }
        if !self.block_has_terminator() {
            let last_val = self.load_return_value_if_needed(last_val)?;
            self.build_return(Some(&last_val))?;
        }
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
            let env_ptr_i8 = self.build_pointer_cast(env_heap_ptr, i8_ptr, "env_ptr_i8")?;
            self.build_store(env_gep, env_ptr_i8)?;
        }

        self.build_load(
            BasicTypeEnum::StructType(closure_struct_type),
            closure_alloca,
            "closure_val",
        )
    }

    /// Allocate and populate the closure environment struct on the heap.
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
        let malloc_fn = self
            .module
            .get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let env_heap_ptr = self
            .build_call(
                malloc_fn,
                &[BasicMetadataValueEnum::IntValue(env_byte_size)],
                "env_heap",
            )?
            .try_as_basic_value_opt()
            .ok_or("malloc returned void")?
            .into_pointer_value();

        // NOTE: not registered in heap_allocs — closure env must outlive the
        // creating scope if the closure escapes (returned or stored to a shared
        // variable), so we cannot auto-free it on scope exit.
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
            match stmt {
                Stmt::Expr(e) => self.collect_free_vars_expr(e, &defined, vars, free_vars),
                Stmt::Let {
                    pat,
                    init: Some(init),
                    ..
                } => {
                    self.collect_free_vars_expr(init, &defined, vars, free_vars);
                    if let Pattern::Variable(name) = pat {
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
                Stmt::Alloc { body, .. } => {
                    self.collect_free_vars(body, &defined, vars, free_vars);
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
        match expr {
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
            Expr::Range { start, end } => {
                self.collect_free_vars_expr(start, defined, vars, free_vars);
                self.collect_free_vars_expr(end, defined, vars, free_vars);
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
}

fn lambda_ret_type<'ctx>(
    context: &'ctx inkwell::context::Context,
    ret: &Option<Type>,
) -> BasicTypeEnum<'ctx> {
    match ret {
        Some(ty) => types::mimi_type_to_llvm(context, ty)
            .unwrap_or(BasicTypeEnum::IntType(context.i64_type())),
        None => BasicTypeEnum::IntType(context.i64_type()),
    }
}

fn lambda_fn_type<'ctx>(
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
        BasicTypeEnum::IntType(_) => context.i64_type().const_int(0, false).into(),
        BasicTypeEnum::FloatType(ft) => ft.const_float(0.0).into(),
        _ => context.i64_type().const_int(0, false).into(),
    }
}
