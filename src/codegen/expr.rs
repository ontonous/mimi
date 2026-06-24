use crate::ast::*;
use crate::codegen::CallSiteValueExt;
use crate::codegen::call_try_basic_value;
use crate::error::CompileError;

use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, IntValue};
use std::collections::HashMap;

use super::CodeGenerator;
use super::VarEntry;

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_expr(
        &mut self,
        expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match expr {
            Expr::Literal(lit) => self.compile_literal_expr(lit, vars),
            Expr::Ident(name) => self.compile_ident_expr(name, vars),
            Expr::Binary(op, lhs, rhs) => self.compile_binary_expr(*op, lhs, rhs, vars),
            Expr::Unary(op, inner) => self.compile_unary_expr(*op, inner, vars),
            Expr::Call(callee, args) => self.compile_call_expr(callee, args, vars),
            Expr::Turbofish(name, type_args, args) => self.compile_turbofish_expr(name, type_args, args, vars),
            Expr::Match(scrutinee, arms) => self.compile_match_expr(scrutinee, arms, vars),
            Expr::Record { ty, fields } => self.compile_record_expr(ty, fields, vars),
            Expr::Field(obj, field_name) => self.compile_field_expr(obj, field_name, vars),
            Expr::List(elems) => self.compile_list_expr(elems, vars),
            Expr::Index(obj, idx_expr) => self.compile_index_expr(obj, idx_expr, vars),
            Expr::Spawn(expr) => self.compile_spawn_expr(expr, vars),
            Expr::Await(expr) => self.compile_await_expr(expr, vars),
            Expr::Try(inner) => self.compile_try_expr(inner, vars),
            Expr::TypeOf(inner) => self.compile_typeof_expr(inner, vars),
            Expr::TypeInfo(ty) => self.compile_typeinfo_expr(ty, vars),
            Expr::Old(inner) => self.compile_old_expr(inner, vars),
            Expr::Tuple(elems) => self.compile_tuple_expr(elems, vars),
            Expr::TupleIndex(tuple_expr, index) => self.compile_tuple_index_expr(tuple_expr, *index, vars),
            Expr::If { cond, then_, else_ } => self.compile_if_expr(cond, then_, else_, vars),
            Expr::Range { start, end } => self.compile_range_expr(start, end, vars),
            Expr::SliceExpr { target, start, end } => self.compile_slice_expr(target, start, end, vars),
            Expr::Lambda { params, ret, body } => self.compile_lambda_expr(params, ret, body, vars),
            Expr::Comprehension { expr: comp_expr, var, iter, guard } => self.compile_comprehension_expr(comp_expr, var, iter, guard, vars),
            Expr::Arena(block) => {
                let function = self.current_function()
                    .ok_or_else(|| CompileError::LlvmError("arena outside function".to_string()))?;
                let arena_body_bb = self.context.append_basic_block(function, "arena_expr_body");
                let arena_cont_bb = self.context.append_basic_block(function, "arena_expr_cont");
                if !self.block_has_terminator() {
                    self.builder.build_unconditional_branch(arena_body_bb)
                        .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                }
                self.builder.position_at_end(arena_body_bb);
                let saved = self.build_stacksave()?;
                let mut arena_vars = vars.clone();
                let val = self.compile_block_last_val(block, &mut arena_vars)?;
                self.build_stackrestore(saved)?;
                if !self.block_has_terminator() {
                    self.builder.build_unconditional_branch(arena_cont_bb)
                        .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                }
                self.builder.position_at_end(arena_cont_bb);
                Ok(val)
            }
            Expr::Block(block) => {
                let mut block_vars = vars.clone();
                self.compile_block_last_val(block, &mut block_vars)
            }
            Expr::Comptime(_) => {
                Err("comptime { ... } block encountered in runtime function: compile-time evaluation must be resolved before codegen (use `mimi run` to evaluate compile-time code)".into())
            }
            Expr::Quote(block) => {
                // Compile-time folding for literal-only quote blocks:
                // quote! { literal } is equivalent to the literal itself.
                if let Some(val) = self.compile_quote_fold(block) {
                    return Ok(val);
                }
                Err("quote { ... } expression encountered in runtime function: quoted AST construction must be resolved before codegen (use `mimi run` to evaluate quote expressions)".into())
            }
            Expr::QuoteInterpolate(_) => {
                Err("${ ... } interpolation encountered in runtime function: interpolation must be resolved before codegen (use `mimi run` to evaluate quote expressions)".into())
            }
            Expr::MapLiteral { entries } => self.compile_map_literal(entries, vars),
            #[allow(unreachable_patterns)]
            _ => Err(format!("unsupported expression in codegen: {:?}", expr).into())
        }
    }

    fn compile_ident_expr(
        &mut self,
        name: &String,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if let Some(&(alloca, ty)) = vars.get(name) {
            if self.shared_var_names.contains(name.as_str()) {
                // Shared variable: the alloca stores a T* pointer to heap memory.
                // First load the pointer, then load the value from the heap.
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let heap_ptr = self.builder.build_load(ptr_ty, alloca, name)
                    .map_err(|e| CompileError::LlvmError(format!("shared heap ptr load error: {}", e)))?;
                let heap_pointer = heap_ptr.into_pointer_value();
                self.builder.build_load(ty, heap_pointer, name)
                    .map_err(|e| CompileError::LlvmError(format!("shared value load error: {}", e)))
            } else {
                self.builder.build_load(ty, alloca, name)
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))
            }
        } else if self.cap_type_names.contains(name.as_str()) {
            // Cap literal: call mimi_cap_register(name) to get handle
            if let Some(register_fn) = self.module.get_function("mimi_cap_register") {
                let name_global = self.builder.build_global_string_ptr(
                    &format!("{}\0", name), &format!("cap_name_{}", name))
                    .map_err(|e| CompileError::LlvmError(format!("string global error: {}", e)))?;
                let name_ptr = name_global.as_pointer_value();
                let handle = self.builder.build_call(register_fn, &[
                    BasicMetadataValueEnum::PointerValue(name_ptr),
                ], &format!("cap_register_{}", name))
                    .map_err(|e| CompileError::LlvmError(format!("cap_register error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("mimi_cap_register returned void")?;
                Ok(handle)
            } else {
                Err(format!("cap literal '{}' requires mimi_cap_register runtime", name).into())
            }
        } else if self.find_variant_owner(name).is_some() {
            // Unit enum variant used as a value (e.g. `Yes` or `Pending`)
            self.compile_call(name, &[], vars)
        } else if name == "None" {
            // Bare built-in None constructor (e.g. `let x: Option<i32> = None`)
            self.compile_constructor("None", vec![])
        } else if let Some(function) = self.module.get_function(name) {
            // First-class function reference: return function pointer as value
            Ok(function.as_global_value().as_pointer_value().into())
        } else {
            Err(format!("undefined variable '{}'", name).into())
        }
    }

    fn compile_old_expr(
        &mut self,
        inner: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // old(expr): snapshot value at function entry.
        // Merge old snapshots into the vars map so variable references within
        // old() resolve to the entry-time alloca, not the current value.
        if self.old_snapshots.is_empty() {
            self.compile_expr(inner, vars)
        } else {
            let mut old_vars = vars.clone();
            for (name, entry) in &self.old_snapshots {
                old_vars.insert(name.clone(), *entry);
            }
            self.compile_expr(inner, &old_vars)
        }
    }

    #[allow(clippy::only_used_in_recursion)]
    pub(super) fn infer_object_type(&self, expr: &Expr, vars: &HashMap<String, VarEntry<'ctx>>) -> String {
        match expr {
            Expr::Ident(name) => {
                // Look up variable's type name from our tracking map
                if let Some(ty_name) = self.var_type_names.get(name) {
                    ty_name.clone()
                } else {
                    name.clone()
                }
            }
            Expr::Record { ty: Some(name), .. } => name.clone(),
            Expr::Call(callee, _) => {
                // constructor call like ActorName(args) -> return type is the name
                if let Expr::Ident(name) = callee.as_ref() {
                    // Try to strip _new suffix used by our codegen constructors
                    if let Some(stripped) = name.strip_suffix("_new") {
                        stripped.to_string()
                    } else {
                        name.clone()
                    }
                } else {
                    String::new()
                }
            }
            Expr::Field(obj, _) => self.infer_object_type(obj, vars),
            Expr::Index(obj, _) => {
                // Index into a List<T> returns T. Infer the list's element type.
                let obj_type = self.infer_object_type(obj, vars);
                if obj_type.starts_with("List<") {
                    let inner = &obj_type[5..];
                    let mut depth = 0u32;
                    for (i, ch) in inner.char_indices() {
                        match ch {
                            '<' => depth += 1,
                            '>' => {
                                if depth == 0 {
                                    return inner[..=i].trim().to_string();
                                }
                                depth -= 1;
                            }
                            _ => {}
                        }
                    }
                    inner.trim().to_string()
                } else {
                    String::new()
                }
            }
            Expr::Block(block) => {
                block.last().and_then(|last| {
                    if let Stmt::Expr(e) = last {
                        Some(self.infer_object_type(e, vars))
                    } else {
                        None
                    }
                }).unwrap_or_default()
            }
            _ => String::new(),
        }
    }

    /// Extract a raw C string pointer (i8*) from a Mimi string argument.
    /// Mimi strings are represented as either:
    ///   - An i8* raw C string (from string literals)
    ///   - A {i8*, i64} struct (from string variables)
    pub(super) fn extract_raw_str_ptr(&self, arg: &BasicMetadataValueEnum<'ctx>) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        match arg {
            BasicMetadataValueEnum::PointerValue(pv) => {
                // Could be a raw C string pointer OR a pointer to a Mimi string struct {i8*, i64}.
                // For now, assume it's a raw C string pointer (string literal case).
                // String variables that hold recv() results produce struct values, not pointers.
                Ok(*pv)
            }
            BasicMetadataValueEnum::StructValue(sv) => {
                let extracted = self.builder.build_extract_value(*sv, 0, "str_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("extract str ptr error: {}", e)))?;
                match extracted {
                    BasicValueEnum::PointerValue(pv) => Ok(pv),
                    _ => Err("string struct field 0 is not a pointer".into()),
                }
            }
            _ => Err("expected a string argument".into()),
        }
    }

    /// Return an error if running in no_std mode for a builtin that depends on libc.
    pub(super) fn require_std(&self, builtin: &str) -> Result<(), CompileError> {
        if self.no_std {
            Err(CompileError::Generic(format!("[E0750] '{}' requires libc (not available in no_std mode)", builtin)))
        } else {
            Ok(())
        }
    }

    /// Compile-time fold a literal-only quote! block.
    /// quote! { 42 } → returns i64(42), bypassing QuotedAst construction.
    fn compile_quote_fold(&self, block: &Block) -> Option<BasicValueEnum<'ctx>> {
        match block.as_slice() {
            [Stmt::Expr(expr)] => self.compile_quote_fold_expr(expr),
            _ => None,
        }
    }

    fn compile_quote_fold_expr(&self, expr: &Expr) -> Option<BasicValueEnum<'ctx>> {
        match expr {
            Expr::Literal(lit) => self.compile_literal_const(lit),
            Expr::Block(block) => match block.as_slice() {
                [Stmt::Expr(e)] => self.compile_quote_fold_expr(e),
                _ => None,
            },
            _ => None,
        }
    }

    fn compile_literal_const(&self, lit: &Lit) -> Option<BasicValueEnum<'ctx>> {
        match lit {
            Lit::Int(v) => Some(self.context.i64_type().const_int(*v as u64, true).into()),
            Lit::Float(v) => Some(self.context.f64_type().const_float(*v).into()),
            Lit::Bool(v) => Some(self.context.bool_type().const_int(*v as u64, false).into()),
            Lit::String(s) => {
                let global = self.builder.build_global_string_ptr(s, "str").ok()?;
                Some(global.as_pointer_value().into())
            }
            Lit::Unit => Some(self.context.i64_type().const_int(0, false).into()),
            Lit::FString(_) => None,
        }
    }

    pub(super) fn compile_map_literal(
        &mut self,
        entries: &[(Expr, Expr)],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let map_new = self.module.get_function("mimi_map_new")
            .ok_or("mimi_map_new not declared")?;
        let result = self.builder.build_call(map_new, &[], "map_new_call")
            .map_err(|e| format!("map_new error: {}", e))?;
        let map_handle = call_try_basic_value(&result)
            .ok_or("mimi_map_new returned void")?
            .into_int_value();

        let map_set = self.module.get_function("mimi_map_set")
            .ok_or("mimi_map_set not declared")?;

        for (key, value) in entries {
            let key_val = self.compile_expr(key, vars)?;
            let val_val = self.compile_expr(value, vars)?;
            // Key must be a string pointer
            let key_ptr = match &key_val {
                BasicValueEnum::PointerValue(pv) => *pv,
                BasicValueEnum::StructValue(sv) => {
                    self.builder.build_extract_value(*sv, 0, "key_str_ptr")
                        .map_err(|e| CompileError::LlvmError(format!("extract key str ptr: {}", e)))?
                        .into_pointer_value()
                }
                _ => return Err("map literal key must be a string".into()),
            };
            // Value is cast to i64 (ValueHandle) for storage
            let val_i64 = self.any_value_to_handle(val_val)?;
            self.builder.build_call(map_set, &[
                BasicMetadataValueEnum::IntValue(map_handle),
                BasicMetadataValueEnum::PointerValue(key_ptr),
                BasicMetadataValueEnum::IntValue(val_i64),
            ], "map_set_call")
                .map_err(|e| format!("map_set error: {}", e))?;
        }

        Ok(BasicValueEnum::IntValue(map_handle))
    }

    /// Convert any basic value to an i64 ValueHandle for map storage.
    fn any_value_to_handle(&self, val: BasicValueEnum<'ctx>) -> Result<IntValue<'ctx>, CompileError> {
        Ok(match val {
            BasicValueEnum::IntValue(iv) => iv,
            BasicValueEnum::PointerValue(pv) => {
                self.builder.build_ptr_to_int(pv, self.context.i64_type(), "ptr_to_handle")
                    .map_err(|e| CompileError::LlvmError(format!("ptr_to_int: {}", e)))?
            }
            BasicValueEnum::StructValue(sv) => {
                // Extract first field (string struct has ptr at 0)
                let field = self.builder.build_extract_value(sv, 0, "struct_field")
                    .map_err(|e| CompileError::LlvmError(format!("extract: {}", e)))?;
                match field {
                    BasicValueEnum::PointerValue(pv) => {
                        self.builder.build_ptr_to_int(pv, self.context.i64_type(), "struct_ptr_to_handle")
                            .map_err(|e| CompileError::LlvmError(format!("ptr_to_int: {}", e)))?
                    }
                    BasicValueEnum::IntValue(iv) => iv,
                    _ => return Err("unsupported struct field type for map value handle".into()),
                }
            }
            BasicValueEnum::FloatValue(fv) => {
                self.builder.build_bit_cast(fv, self.context.i64_type(), "float_to_handle")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast: {}", e)))?
                    .into_int_value()
            }
            _ => return Err("unsupported value type for map storage".into()),
        })
    }
}

mod access;
mod call;
mod control;
mod lambda;
mod literal;
mod r#match;
mod operator;
mod record;
mod try_expr;
mod type_expr;
