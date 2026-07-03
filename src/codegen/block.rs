use crate::ast::*;
use crate::codegen::call_try_basic_value;
use crate::codegen::types;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

use crate::error::{CompileError, MimiResult};

use super::CodeGenerator;
use super::VarEntry;

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_block(
        &mut self,
        block: &Block,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        self.push_comp_scope();
        self.push_shared_scope();
        self.push_heap_scope();
        for stmt in block {
            // Run compensations before exit()
            if let Stmt::Expr(Expr::Call(callee, _)) = stmt {
                if let Expr::Ident(name) = &**callee {
                    if name == "exit" {
                        self.compile_compensations(vars)?;
                    }
                }
            }
            match stmt {
                Stmt::Expr(expr) => {
                    self.compile_expr(expr, vars)?;
                }
                Stmt::Return(Some(expr)) => {
                    let mut val = self.compile_expr(expr, vars)?;
                    let ret_type = self.current_fn_ret_type();
                    val = self.adjust_int_val(val, ret_type)?;
                    val = self.load_return_value_if_needed(val)?;
                    let ensures = self.ensures_stmts.clone();
                    for ensures_expr in &ensures {
                        let fn_name: String = self
                            .current_function()
                            .map(|f| f.get_name().to_string_lossy().into_owned())
                            .unwrap_or_else(|| "unknown".to_string());
                        self.compile_contract_assert(
                            ensures_expr,
                            vars,
                            &format!("ensures violation in '{}'", fn_name),
                        )?;
                    }
                    self.build_return(Some(&val))?;
                    return Ok(());
                }
                Stmt::Return(None) => {
                    let ensures = self.ensures_stmts.clone();
                    for ensures_expr in &ensures {
                        let fn_name: String = self
                            .current_function()
                            .map(|f| f.get_name().to_string_lossy().into_owned())
                            .unwrap_or_else(|| "unknown".to_string());
                        self.compile_contract_assert(
                            ensures_expr,
                            vars,
                            &format!("ensures violation in '{}'", fn_name),
                        )?;
                    }
                    self.build_return(None)?;
                    return Ok(());
                }
                Stmt::Let {
                    pat,
                    init: Some(init),
                    ty,
                    ..
                } => {
                    // dyn Trait let-binding: build fat pointer from concrete value (requires Variable pattern)
                    if let Some(Type::DynTrait(trait_names)) = &ty {
                        let name = match pat {
                            Pattern::Variable(n) => n.clone(),
                            _ => {
                                return Err(CompileError::LlvmError(
                                    "dyn Trait binding requires a simple variable pattern"
                                        .to_string(),
                                ))
                            }
                        };
                        let concrete_val = self.compile_expr(init, vars)?;
                        let concrete_type = match init {
                            Expr::Record { ty: Some(tn), .. } => tn.clone(),
                            Expr::Ident(var_name) => self
                                .var_type_names
                                .get(var_name)
                                .cloned()
                                .unwrap_or_default(),
                            _ => {
                                return Err(CompileError::LlvmError(format!(
                                    "cannot infer concrete type for dyn Trait binding '{}'",
                                    name
                                )));
                            }
                        };
                        if concrete_type.is_empty() {
                            return Err(CompileError::LlvmError(format!(
                                "cannot infer concrete type for dyn Trait binding '{}'",
                                name
                            )));
                        }
                        let trait_name = &trait_names[0];
                        let concrete_ty = self
                            .type_llvm
                            .get(&concrete_type)
                            .cloned()
                            .unwrap_or_else(|| concrete_val.get_type());
                        let data_alloca =
                            self.build_alloca(concrete_ty, &format!("{}_data", name))?;
                        self.build_store(data_alloca, concrete_val)?;
                        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                        let data_ptr = self
                            .builder
                            .build_pointer_cast(data_alloca, i8_ptr, &format!("{}_data_i8", name))
                            .map_err(|e| {
                                CompileError::LlvmError(format!("pointer cast error: {}", e))
                            })?;
                        let vtable_key = format!("{}__{}", concrete_type, trait_name);
                        let vtable_gv = self.vtable_globals.get(&vtable_key).ok_or_else(|| {
                            CompileError::LlvmError(format!(
                                "no vtable for {}.{}",
                                concrete_type, trait_name
                            ))
                        })?;
                        let vtable_ptr = self
                            .builder
                            .build_pointer_cast(
                                vtable_gv.as_pointer_value(),
                                i8_ptr,
                                &format!("{}_vtable_i8", name),
                            )
                            .map_err(|e| {
                                CompileError::LlvmError(format!("pointer cast error: {}", e))
                            })?;
                        let fat_ty = BasicTypeEnum::StructType(self.context.struct_type(
                            &[
                                BasicTypeEnum::PointerType(i8_ptr),
                                BasicTypeEnum::PointerType(i8_ptr),
                            ],
                            false,
                        ));
                        let fat_alloca = self.build_alloca(fat_ty, &name)?;
                        let data_gep = self
                            .gep()
                            .build_struct_gep(fat_ty, fat_alloca, 0, &format!("{}_data_gep", name))
                            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        self.build_store(data_gep, data_ptr)?;
                        let vtable_gep = self
                            .gep()
                            .build_struct_gep(
                                fat_ty,
                                fat_alloca,
                                1,
                                &format!("{}_vtable_gep", name),
                            )
                            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        self.build_store(vtable_gep, vtable_ptr)?;
                        let ty_ref = ty.as_ref().ok_or_else(|| {
                            CompileError::LlvmError(format!("missing type for variable '{}'", name))
                        })?;
                        let dyn_type_str = crate::core::fmt_type(ty_ref);
                        self.var_type_names.insert(name.clone(), dyn_type_str);
                        vars.insert(name, (fat_alloca, fat_ty));
                        continue;
                    }
                    // Shared ref copy: let v = shared_var
                    if let Pattern::Variable(name) = pat {
                        if let Expr::Ident(src_name) = init {
                            if self.shared_var_names.contains(src_name.as_str()) {
                                self.compile_shared_ref_copy(name, src_name, vars)?;
                                continue;
                            }
                        }
                    }
                    // Shared var clone: let v = shared_var.clone()
                    if let Pattern::Variable(name) = pat {
                        if let Expr::Call(callee, cargs) = init {
                            if cargs.is_empty() {
                                if let Expr::Field(obj, method_name) = callee.as_ref() {
                                    if method_name == "clone" {
                                        if let Expr::Ident(src_name) = obj.as_ref() {
                                            if self.shared_var_names.contains(src_name.as_str()) {
                                                self.compile_shared_ref_copy(name, src_name, vars)?;
                                                continue;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Non-dyn Trait: compile init and bind via recursive pattern matching
                    let mut val = self.compile_expr(init, vars)?;
                    if let Some(decl_ty) = ty {
                        let target = types::mimi_type_to_llvm(self.context, decl_ty)
                            .unwrap_or_else(|| val.get_type());
                        val = self.adjust_int_val(val, target)?;
                    }
                    // For simple Variable patterns, track type info
                    if let Pattern::Variable(name) = pat {
                        if let Some(Type::Name(tn, args)) = &ty {
                            if !args.is_empty() {
                                if let Some(full) =
                                    self.get_full_type_name(ty.as_ref().expect("ty is Some"))
                                {
                                    self.var_type_names.insert(name.clone(), full);
                                }
                            } else {
                                self.var_type_names.insert(name.clone(), tn.clone());
                            }
                        } else if self.expr_is_string(init) {
                            self.var_type_names
                                .insert(name.clone(), "string".to_string());
                        } else if let Expr::Record { ty: Some(_), .. } = init {
                            self.var_type_names
                                .insert(name.clone(), "string".to_string());
                        } else if let Expr::Record { ty: Some(tn), .. } = init {
                            self.var_type_names.insert(name.clone(), tn.clone());
                        } else if matches!(init, Expr::SetLiteral(_)) {
                            self.var_type_names.insert(name.clone(), "set".to_string());
                        } else if let Expr::List(list_elems) = init {
                            // D1: infer List<T> type from first element
                            if let Some(first) = list_elems.first() {
                                let elem_type = self.infer_object_type(first, vars);
                                if !elem_type.is_empty() {
                                    self.var_type_names
                                        .insert(name.clone(), format!("List<{}>", elem_type));
                                }
                            }
                        } else if let Expr::Index(_, _) = init {
                            // D1: infer element type via infer_object_type (handles List<T> stripping)
                            let elem_type = self.infer_object_type(init, vars);
                            if !elem_type.is_empty() {
                                self.var_type_names.insert(name.clone(), elem_type);
                            }
                        } else if let Expr::Call(callee, _) = init {
                            if let Expr::Field(obj, method_name) = callee.as_ref() {
                                if method_name == "spawn" {
                                    let obj_type = self.infer_object_type(obj, vars);
                                    if !obj_type.is_empty() {
                                        self.var_type_names.insert(name.clone(), obj_type);
                                    }
                                } else if matches!(
                                    method_name.as_str(),
                                    "map" | "and_then" | "map_err" | "ok_or"
                                ) {
                                    let obj_type = self.infer_object_type(obj, vars);
                                    if obj_type.starts_with("Result") {
                                        self.var_type_names
                                            .insert(name.clone(), "Result".to_string());
                                    } else if obj_type.starts_with("Option") {
                                        self.var_type_names
                                            .insert(name.clone(), "Option".to_string());
                                    }
                                } else if matches!(method_name.as_str(), "insert" | "remove") {
                                    let obj_type = self.infer_object_type(obj, vars);
                                    if obj_type.starts_with("Set") || obj_type == "set" {
                                        self.var_type_names.insert(name.clone(), obj_type);
                                    }
                                } else if method_name == "upgrade" {
                                    self.track_weak_upgrade_type(name, obj);
                                }
                            } else if let Expr::Ident(func_name) = callee.as_ref() {
                                match func_name.as_str() {
                                    "Ok" | "Err" => {
                                        self.var_type_names
                                            .insert(name.clone(), "Result".to_string());
                                    }
                                    "Some" | "None" => {
                                        self.var_type_names
                                            .insert(name.clone(), "Option".to_string());
                                    }
                                    _ => {
                                        if let Some((type_name, _)) =
                                            self.find_variant_owner(func_name)
                                        {
                                            self.var_type_names.insert(name.clone(), type_name);
                                        } else if let Some(fdef) = self.func_defs.get(func_name) {
                                            if let Some(ret_ty) = &fdef.ret {
                                                match ret_ty {
                                                    Type::ImplTrait(traits) => {
                                                        self.var_type_names.insert(
                                                            name.clone(),
                                                            format!("impl {}", traits.join(" + ")),
                                                        );
                                                    }
                                                    Type::Name(tn, _) => {
                                                        self.var_type_names
                                                            .insert(name.clone(), tn.clone());
                                                    }
                                                    #[allow(unreachable_patterns)]
                                                    _ => {}
                                                }
                                                // For async functions, track the inner result type for await.
                                                if fdef.is_async {
                                                    if let Some(llvm_ret) =
                                                        self.llvm_type_for(ret_ty)
                                                    {
                                                        self.async_var_inner_types
                                                            .insert(name.clone(), llvm_ret);
                                                    }
                                                }
                                            }
                                        } else if let Some(crate::ast::Type::Name(tn, _)) = self
                                            .extern_func_defs
                                            .get(func_name)
                                            .and_then(|ef| ef.ret.as_ref())
                                        {
                                            self.var_type_names.insert(name.clone(), tn.clone());
                                        }
                                        // Track return types for builtins
                                        match func_name.as_str() {
                                            "listdir" | "walk_dir" | "str_split" | "keys" => {
                                                self.var_type_names.insert(
                                                    name.clone(),
                                                    "List<string>".to_string(),
                                                );
                                                self.var_types.insert(
                                                    name.clone(),
                                                    Type::Name(
                                                        "List".into(),
                                                        vec![Type::Name("string".into(), vec![])],
                                                    ),
                                                );
                                            }
                                            "sort_str" => {
                                                self.var_type_names.insert(
                                                    name.clone(),
                                                    "List<string>".to_string(),
                                                );
                                                self.var_types.insert(
                                                    name.clone(),
                                                    Type::Name(
                                                        "List".into(),
                                                        vec![Type::Name("string".into(), vec![])],
                                                    ),
                                                );
                                            }
                                            "sort_f64" => {
                                                self.var_type_names
                                                    .insert(name.clone(), "List<f64>".to_string());
                                                self.var_types.insert(
                                                    name.clone(),
                                                    Type::Name(
                                                        "List".into(),
                                                        vec![Type::Name("f64".into(), vec![])],
                                                    ),
                                                );
                                            }
                                            "exec" => {
                                                self.var_type_names
                                                    .insert(name.clone(), "ExecResult".to_string());
                                            }
                                            "file_stat" => {
                                                self.var_type_names
                                                    .insert(name.clone(), "StatResult".to_string());
                                            }
                                            "append_file" => {
                                                self.var_type_names
                                                    .insert(name.clone(), "bool".to_string());
                                            }
                                            "set_env" => {
                                                self.var_type_names
                                                    .insert(name.clone(), "bool".to_string());
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            } else if let Expr::Turbofish(_func_name, turbo_type_args, _) = init {
                                if let Some(ta) = turbo_type_args.first() {
                                    if let Type::Name(tn, args) = ta {
                                        if tn == "List" && !args.is_empty() {
                                            if let Some(full) = self.get_full_type_name(ta) {
                                                self.var_type_names.insert(name.clone(), full);
                                            }
                                        } else {
                                            self.var_type_names.insert(name.clone(), tn.clone());
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Track list element type for nested List<List<T>> indexing
                    if let Pattern::Variable(name) = pat {
                        if let Some(decl_ty) = &ty {
                            self.register_list_elem_type(name, decl_ty);
                        }
                    }
                    self.compile_pattern_bind(pat, val, vars)?;
                    if let Pattern::Variable(name) = pat {
                        if let Expr::Ident(fn_name) = init {
                            if self.module.get_function(fn_name).is_some() {
                                self.fn_ptr_var_names.insert(name.clone());
                            }
                            if self.cap_type_names.contains(fn_name.as_str()) {
                                self.var_type_names.insert(name.clone(), fn_name.clone());
                            }
                        }
                    }
                }
                Stmt::Let {
                    pat,
                    init: None,
                    ty,
                    ..
                } => {
                    // let x; or let (a, b); — needs type annotation
                    if let Pattern::Variable(name) = pat {
                        let llvm_ty = match ty {
                            Some(decl_ty) => types::mimi_type_to_llvm(self.context, decl_ty)
                                .ok_or_else(|| {
                                    CompileError::LlvmError(format!(
                                        "unknown type for 'let {};'",
                                        name
                                    ))
                                })?,
                            None => {
                                return Err(CompileError::LlvmError(format!(
                                    "'let {};' requires an explicit type annotation",
                                    name
                                )))
                            }
                        };
                        let alloca = self.build_alloca(llvm_ty, name)?;
                        match llvm_ty {
                            BasicTypeEnum::IntType(ty) => {
                                self.build_store(alloca, ty.const_int(0, false))?;
                            }
                            BasicTypeEnum::FloatType(ty) => {
                                self.build_store(alloca, ty.const_float(0.0))?;
                            }
                            BasicTypeEnum::PointerType(ty) => {
                                self.build_store(alloca, ty.const_null())?;
                            }
                            _ => {}
                        }
                        vars.insert(name.clone(), (alloca, llvm_ty));
                    } else {
                        return Err(CompileError::LlvmError(
                            "'let' with no initializer requires a simple variable pattern"
                                .to_string(),
                        ));
                    }
                }
                Stmt::Assign { target, value } => {
                    self.compile_assign_stmt(target, value, vars)?;
                }
                Stmt::If { cond, then_, else_ } => {
                    self.compile_if_stmt(cond, then_, else_, vars, true)?;
                }
                Stmt::Break(_) => {
                    self.compile_break_stmt()?;
                }
                Stmt::Continue => {
                    self.compile_continue_stmt()?;
                }
                Stmt::MmsBlock { .. } => {
                    // Skip MMS blocks in codegen (they're for documentation/contracts)
                }
                Stmt::Parasteps(block) => {
                    // Parasteps: execute spawn statements in parallel, join at block end
                    self.enter_parasteps();
                    self.compile_block(block, vars)?;
                    self.leave_parasteps()?;
                }
                Stmt::Drop(expr) => {
                    // Drop: evaluate expression and discard result (for linear capabilities)
                    self.compile_expr(expr, vars)?;
                }
                Stmt::SharedLet {
                    kind,
                    name,
                    ty,
                    init,
                } => {
                    self.compile_shared_let_stmt(kind, name, ty, init, vars)?;
                }
                Stmt::OnFailure(block) => {
                    // Register compensation block for LIFO execution on error exit
                    self.register_comp(block);
                }
                Stmt::Arena(block) => {
                    self.compile_arena_block(block, vars, "arena")?;
                }
                Stmt::Unsafe(block) => {
                    // Unsafe: execute block (no restrictions in codegen)
                    self.compile_block(block, vars)?;
                }
                Stmt::Alloc {
                    kind: AllocKind::Arena,
                    body,
                } => {
                    self.compile_arena_block(body, vars, "alloc(Arena)")?;
                }
                Stmt::Alloc { body, .. } => {
                    // Alloc: execute body sequentially (simplified)
                    self.compile_block(body, vars)?;
                }
                Stmt::Desc(..)
                | Stmt::Rule(..)
                | Stmt::Requires(..)
                | Stmt::Ensures(..)
                | Stmt::Invariant(..)
                | Stmt::Math(_)
                | Stmt::Ellipsis => {
                    // Skip contract-related statements in codegen
                }
                Stmt::Block(block) => {
                    self.compile_block(block, vars)?;
                }
                Stmt::While { cond, body } => {
                    self.compile_while_stmt(cond, body, vars)?;
                }
                Stmt::WhileLet { pat, init, body } => {
                    self.compile_while_let_stmt(pat, init, body, vars)?;
                }
                Stmt::Loop(body) => {
                    self.compile_loop_stmt(body, vars)?;
                }
                Stmt::For {
                    var,
                    iterable,
                    body,
                } => {
                    self.compile_for_stmt(var, iterable, body, vars)?;
                }
                // Defensive wildcard: compile_block handles all current Stmt variants
                // explicitly; this arm guards against future variants causing a
                // non-exhaustive match error during development.
                #[allow(unreachable_patterns)]
                _ => {}
            }
        }
        self.pop_shared_scope()?;
        self.free_heap_allocs()?;
        self.pop_comp_scope();
        Ok(())
    }

    /// Compile a `break` statement by branching to the current loop break target.
    fn compile_break_stmt(&mut self) -> Result<(), CompileError> {
        if let Some(target) = self.loop_break {
            self.build_br(target)?;
            let function = self.current_function().ok_or_else(|| {
                CompileError::LlvmError("codegen: no current function for break".to_string())
            })?;
            let unreachable = self.context.append_basic_block(function, "unreachable");
            self.builder.position_at_end(unreachable);
            Ok(())
        } else {
            Err(CompileError::BreakOutsideLoop)
        }
    }

    /// Compile a `continue` statement by branching to the current loop continue target.
    fn compile_continue_stmt(&mut self) -> Result<(), CompileError> {
        if let Some(target) = self.loop_continue {
            self.build_br(target)?;
            let function = self.current_function().ok_or_else(|| {
                CompileError::LlvmError("codegen: no current function for continue".to_string())
            })?;
            let unreachable = self.context.append_basic_block(function, "unreachable");
            self.builder.position_at_end(unreachable);
            Ok(())
        } else {
            Err(CompileError::ContinueOutsideLoop)
        }
    }

    /// Compile an `if` statement or if-expression.
    ///
    /// When `merge_vars` is `true`, variables introduced in either branch are merged
    /// back into `vars` (used for statement-position `if`). When `false`, the value
    /// of the branches is merged with a phi node and returned (used for
    /// `compile_block_last_val`).
    fn compile_if_stmt(
        &mut self,
        cond: &Expr,
        then_: &Block,
        else_: &Option<Block>,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
        merge_vars: bool,
    ) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
        let cond_val = self.compile_expr(cond, vars)?;
        let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
            iv
        } else {
            let function = self.current_function().ok_or_else(|| {
                CompileError::LlvmError("codegen: no current function for if block".to_string())
            })?;
            let fn_name = function.get_name().to_str().unwrap_or("unknown");
            return Err(CompileError::TypeMismatch(format!(
                "if condition must be bool, got {} in function '{}'",
                cond_val.get_type(),
                fn_name
            )));
        };

        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("codegen: no current function for if block".to_string())
        })?;
        let (then_label, else_label, merge_label) = if merge_vars {
            ("then", "else", "ifcont")
        } else {
            ("blt_then", "blt_else", "blt_merge")
        };
        let then_bb = self.context.append_basic_block(function, then_label);
        let else_bb = self.context.append_basic_block(function, else_label);
        let merge_bb = self.context.append_basic_block(function, merge_label);

        self.build_cond_br(cond_bool, then_bb, else_bb)?;

        // Then branch
        self.builder.position_at_end(then_bb);
        let mut then_vars = vars.clone();
        let then_val = if merge_vars {
            self.compile_block(then_, &mut then_vars)?;
            None
        } else {
            Some(self.compile_block_last_val(then_, &mut then_vars)?)
        };
        let then_reaches = !self.block_has_terminator();
        if then_reaches {
            self.build_br(merge_bb)?;
        }
        let then_bb_end = self.builder.get_insert_block().ok_or_else(|| {
            CompileError::LlvmError("codegen: no insert block after then branch".to_string())
        })?;

        // Else branch
        self.builder.position_at_end(else_bb);
        let mut else_vars = vars.clone();
        let else_val = if let Some(else_block) = else_ {
            if merge_vars {
                self.compile_block(else_block, &mut else_vars)?;
                None
            } else {
                Some(self.compile_block_last_val(else_block, &mut else_vars)?)
            }
        } else if merge_vars {
            None
        } else {
            // No else block: fall through to merge with a default value.
            Some(self.context.i64_type().const_int(0, false).into())
        };
        let else_reaches = !self.block_has_terminator();
        if else_reaches {
            self.build_br(merge_bb)?;
        }
        let else_bb_end = self.builder.get_insert_block().ok_or_else(|| {
            CompileError::LlvmError("codegen: no insert block after else branch".to_string())
        })?;

        // Merge branch-local variables back into the outer scope when compiling a statement.
        if merge_vars {
            for (k, v) in then_vars {
                vars.entry(k).or_insert(v);
            }
            if else_.is_some() {
                for (k, v) in else_vars {
                    vars.entry(k).or_insert(v);
                }
            }
            self.builder.position_at_end(merge_bb);
            return Ok(None);
        }

        // Value mode: build a phi of the values produced by each reaching branch.
        self.builder.position_at_end(merge_bb);
        let default_val = self.context.i64_type().const_int(0, false).into();
        let then_val = then_val.unwrap_or(default_val);
        let else_val = else_val.unwrap_or(default_val);
        let mut incoming: Vec<(
            &dyn inkwell::values::BasicValue<'ctx>,
            inkwell::basic_block::BasicBlock<'ctx>,
        )> = Vec::new();
        if then_reaches {
            incoming.push((&then_val, then_bb_end));
        }
        if else_reaches {
            incoming.push((&else_val, else_bb_end));
        }
        if !incoming.is_empty() {
            let phi = self
                .builder
                .build_phi(then_val.get_type(), "if_lastval")
                .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
            phi.add_incoming(&incoming);
            Ok(Some(phi.as_basic_value()))
        } else {
            // Both branches returned; the merge block is unreachable.
            Ok(Some(then_val))
        }
    }

    /// Call @llvm.stacksave() to capture the current stack pointer for arena region management
    pub(super) fn build_stacksave(&self) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let fn_type = i8_ptr.fn_type(&[], false);
        let fn_val = self
            .module
            .get_function("llvm.stacksave")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "llvm.stacksave",
                    fn_type,
                    Some(inkwell::module::Linkage::External),
                )
            });
        let call = self
            .builder
            .build_call(fn_val, &[], "saved_stack")
            .map_err(|e| CompileError::LlvmError(format!("stacksave: {}", e)))?;
        let val = call_try_basic_value(&call)
            .ok_or_else(|| CompileError::LlvmError("stacksave returned void".to_string()))?;
        match val {
            BasicValueEnum::PointerValue(ptr) => Ok(ptr),
            _ => Err(CompileError::LlvmError(format!(
                "stacksave didn't return pointer, got {:?}",
                val
            ))),
        }
    }

    /// Call @llvm.stackrestore(i8*) to restore the stack pointer, freeing arena allocations
    pub(super) fn build_stackrestore(
        &self,
        saved: inkwell::values::PointerValue<'ctx>,
    ) -> MimiResult<()> {
        let i8_ptr_meta = BasicMetadataTypeEnum::PointerType(
            self.context.ptr_type(inkwell::AddressSpace::default()),
        );
        let fn_type = self.context.void_type().fn_type(&[i8_ptr_meta], false);
        let fn_val = self
            .module
            .get_function("llvm.stackrestore")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "llvm.stackrestore",
                    fn_type,
                    Some(inkwell::module::Linkage::External),
                )
            });
        self.builder
            .build_call(fn_val, &[BasicMetadataValueEnum::PointerValue(saved)], "")
            .map_err(|e| CompileError::LlvmError(format!("stackrestore: {}", e)))?;
        Ok(())
    }

    /// Compile a block and return the value of its last expression (for if-expressions)
    pub(super) fn compile_block_last_val(
        &mut self,
        block: &Block,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let mut last_val = self.context.i64_type().const_int(0, false).into();
        for stmt in block {
            match stmt {
                Stmt::Expr(e) => {
                    last_val = self.compile_expr(e, vars)?;
                }
                Stmt::Return(Some(e)) => {
                    let mut val = self.compile_expr(e, vars)?;
                    let ret_type = self.current_fn_ret_type();
                    val = self.adjust_int_val(val, ret_type)?;
                    // P0-4: heap-copy string returns so the caller
                    // doesn't later free() a .rodata literal pointer.
                    val = self.claim_string_return_value(val, ret_type, Some(e), vars)?;
                    val = self.load_return_value_if_needed(val)?;
                    self.build_return(Some(&val))?;
                    return Ok(val);
                }
                Stmt::Return(None) => {
                    self.build_return(None)?;
                    return Ok(self.context.i64_type().const_int(0, false).into());
                }
                Stmt::Let {
                    pat,
                    init: Some(init),
                    ..
                } => {
                    let val = self.compile_expr(init, vars)?;
                    self.compile_pattern_bind(pat, val, vars)?;
                    if let Pattern::Variable(name) = pat {
                        if self.expr_is_string(init) {
                            self.var_type_names
                                .insert(name.clone(), "string".to_string());
                        }
                        if let Expr::Ident(fn_name) = init {
                            if self.module.get_function(fn_name.as_str()).is_some() {
                                self.fn_ptr_var_names.insert(name.clone());
                            }
                            if self.cap_type_names.contains(fn_name.as_str()) {
                                self.var_type_names.insert(name.clone(), fn_name.clone());
                            }
                            // Track return types for builtins whose result is
                            // a List<T> or other type the caller needs to
                            // recover when indexing. Without this, `let xs =
                            // sort_str(ys)` would leave `xs` untyped and
                            // `xs[i]` would be returned as i64 (the raw
                            // element slot) instead of the proper struct/
                            // string pointer.
                            match fn_name.as_str() {
                                "listdir" | "walk_dir" | "str_split" | "sort_str" | "keys" => {
                                    self.var_type_names
                                        .insert(name.clone(), "List<string>".to_string());
                                    self.var_types.insert(
                                        name.clone(),
                                        Type::Name(
                                            "List".into(),
                                            vec![Type::Name("string".into(), vec![])],
                                        ),
                                    );
                                }
                                "sort_f64" => {
                                    self.var_type_names
                                        .insert(name.clone(), "List<f64>".to_string());
                                    self.var_types.insert(
                                        name.clone(),
                                        Type::Name(
                                            "List".into(),
                                            vec![Type::Name("f64".into(), vec![])],
                                        ),
                                    );
                                }
                                "exec" => {
                                    self.var_type_names
                                        .insert(name.clone(), "ExecResult".to_string());
                                }
                                "file_stat" => {
                                    self.var_type_names
                                        .insert(name.clone(), "StatResult".to_string());
                                }
                                _ => {}
                            }
                        }
                        // Track return types for calls that produce List<string>
                        // (e.g. std::strings::words/lines/split).  The callee is a
                        // function name, not a bare identifier, so it is not covered
                        // by the branch above.
                        if let Expr::Call(callee, _) = init {
                            if let Expr::Ident(fn_name) = callee.as_ref() {
                                match fn_name.as_str() {
                                    "words" | "lines" | "split" | "str_split" | "listdir"
                                    | "walk_dir" | "sort_str" | "keys" => {
                                        self.var_type_names
                                            .insert(name.clone(), "List<string>".to_string());
                                        self.var_types.insert(
                                            name.clone(),
                                            Type::Name(
                                                "List".into(),
                                                vec![Type::Name("string".into(), vec![])],
                                            ),
                                        );
                                    }
                                    "sort_f64" => {
                                        self.var_type_names
                                            .insert(name.clone(), "List<f64>".to_string());
                                        self.var_types.insert(
                                            name.clone(),
                                            Type::Name(
                                                "List".into(),
                                                vec![Type::Name("f64".into(), vec![])],
                                            ),
                                        );
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                Stmt::Assign {
                    target: Expr::Ident(name),
                    value,
                } => {
                    let val = self.compile_expr(value, vars)?;
                    if let Some(&(alloca, ty)) = vars.get(name) {
                        self.assign_to_var(name, val, alloca, ty)?;
                        last_val = val;
                    }
                }
                Stmt::Assign {
                    target: Expr::Field(obj, field_name),
                    value,
                } => {
                    let val = self.compile_expr(value, vars)?;
                    self.compile_field_assign(obj, field_name, val, vars)?;
                    last_val = val;
                }
                Stmt::Assign {
                    target: Expr::Index(obj, idx),
                    value,
                } => {
                    let val = self.compile_expr(value, vars)?;
                    self.compile_index_assign(obj, idx, val, vars)?;
                    last_val = val;
                }
                Stmt::Assign {
                    target: Expr::Unary(crate::ast::UnOp::Deref, inner),
                    value,
                } => {
                    let val = self.compile_expr(value, vars)?;
                    self.compile_deref_assign(inner, val, vars)?;
                    last_val = val;
                }
                Stmt::If { cond, then_, else_ } => {
                    if let Some(v) = self.compile_if_stmt(cond, then_, else_, vars, false)? {
                        last_val = v;
                    }
                }
                Stmt::Break(_) => {
                    self.compile_break_stmt()?;
                }
                Stmt::Continue => {
                    self.compile_continue_stmt()?;
                }
                Stmt::While { cond, body } => {
                    self.compile_while_stmt(cond, body, vars)?;
                }
                Stmt::WhileLet { pat, init, body } => {
                    self.compile_while_let_stmt(pat, init, body, vars)?;
                }
                Stmt::Loop(body) => {
                    self.compile_loop_stmt(body, vars)?;
                }
                Stmt::For {
                    var,
                    iterable,
                    body,
                } => {
                    self.compile_for_stmt(var, iterable, body, vars)?;
                }
                _ => {}
            }
        }
        Ok(last_val)
    }
}
