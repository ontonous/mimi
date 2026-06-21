use crate::ast::*;
use crate::codegen::CallSiteValueExt;
use crate::error::CompileError;

use inkwell::types::BasicType;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
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
            Expr::Comptime(_) => {
                Err("comptime { ... } block encountered in runtime function: compile-time evaluation must be resolved before codegen (use `mimi run` to evaluate compile-time code)".into())
            }
            Expr::Quote(_) => {
                Err("quote { ... } expression encountered in runtime function: quoted AST construction must be resolved before codegen (use `mimi run` to evaluate quote expressions)".into())
            }
            Expr::QuoteInterpolate(_) => {
                Err("${ ... } interpolation encountered in runtime function: interpolation must be resolved before codegen (use `mimi run` to evaluate interpolated expressions)".into())
            }
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
                let ptr_ty = ty.ptr_type(inkwell::AddressSpace::default());
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
                // Try to detect: if it points to a struct with ptr+len, load field 0.
                // For now, assume it's a raw C string pointer (string literal case).
                // String variables may produce pointer-to-struct — handle below.
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
