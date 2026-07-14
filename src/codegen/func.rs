#![allow(clippy::unwrap_used)]
use crate::ast::*;
use crate::codegen::types;
use std::collections::HashMap;
use std::ops::ControlFlow;

use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue};
use inkwell::AddressSpace;

use crate::codegen::CallSiteValueExt;
use crate::error::{CompileError, MimiResult};

/// Recursively collect all Stmt::Ensures from a list of statements,
/// descending into nested blocks (if, while, for, parasteps, lambda, expr block).
fn collect_ensures(stmts: &[Stmt]) -> Vec<Expr> {
    let mut result = Vec::new();
    for s in stmts {
        match s {
            Stmt::Ensures(expr, _) => result.push(expr.clone()),
            Stmt::If { then_, else_, .. } => {
                result.extend(collect_ensures(then_));
                if let Some(eb) = else_ {
                    result.extend(collect_ensures(eb));
                }
            }
            Stmt::While { body, .. } => result.extend(collect_ensures(body)),
            Stmt::Loop(body) => result.extend(collect_ensures(body)),
            Stmt::For { body, .. } => result.extend(collect_ensures(body)),
            Stmt::Parasteps(body) => result.extend(collect_ensures(body)),
            Stmt::Expr(Expr::Lambda { body, .. }) => result.extend(collect_ensures(body)),
            Stmt::Expr(Expr::Block(body)) => result.extend(collect_ensures(body)),
            Stmt::Return(Some(Expr::Block(body))) => result.extend(collect_ensures(body)),
            _ => {}
        }
    }
    result
}

use super::CodeGenerator;
use super::VarEntry;

/// CG-H10 (audit): collect all identifier names referenced via `old(name)`
/// inside a postcondition expression. Walks the full Expr tree recursively
/// to find all `Old(inner)` nodes, then extracts the root identifier(s) from
/// each `old(...)` expression.
///
/// This is a comprehensive walker that handles ALL Expr variants so that
/// `old(x)` nested inside `if`, `match`, `cast`, `range`, etc. is not
/// silently missed (which would cause the variable to not be snapshotted,
/// defeating the postcondition check).
fn collect_old_idents(expr: &crate::ast::Expr) -> Vec<String> {
    let mut out = Vec::new();
    collect_old_idents_walker(expr, &mut out);
    out
}

/// Walk all sub-expressions recursively. When we encounter `Old(inner)`,
/// collect all identifier names from the inner expression — these are the
/// variables that need to be snapshotted at function entry.
fn collect_old_idents_walker(expr: &crate::ast::Expr, out: &mut Vec<String>) {
    use crate::ast::Expr;
    match expr {
        Expr::Old(inner) => {
            // Found an `old(...)` — collect all identifiers inside it.
            collect_idents_in_old(inner, out);
        }
        // Recurse into all sub-expressions for every other variant:
        Expr::Binary(_, l, r) => {
            collect_old_idents_walker(l, out);
            collect_old_idents_walker(r, out);
        }
        Expr::Unary(_, e) => collect_old_idents_walker(e, out),
        Expr::Field(e, _) => collect_old_idents_walker(e, out),
        Expr::Index(e, idx) => {
            collect_old_idents_walker(e, out);
            collect_old_idents_walker(idx, out);
        }
        Expr::Call(callee, args) => {
            collect_old_idents_walker(callee, out);
            for a in args {
                collect_old_idents_walker(a, out);
            }
        }
        Expr::Tuple(es) | Expr::List(es) => {
            for e in es {
                collect_old_idents_walker(e, out);
            }
        }
        Expr::Block(stmts) => {
            for s in stmts {
                collect_old_idents_in_stmt(s, out);
            }
        }
        Expr::If { cond, then_, else_ } => {
            collect_old_idents_walker(cond, out);
            for s in then_ {
                collect_old_idents_in_stmt(s, out);
            }
            if let Some(e) = else_ {
                for s in e {
                    collect_old_idents_in_stmt(s, out);
                }
            }
        }
        Expr::Match(scrut, arms) => {
            collect_old_idents_walker(scrut, out);
            for arm in arms {
                collect_old_idents_walker(&arm.body, out);
                if let Some(g) = &arm.guard {
                    collect_old_idents_walker(g, out);
                }
            }
        }
        Expr::Cast(e, _) => collect_old_idents_walker(e, out),
        Expr::Try(e) => collect_old_idents_walker(e, out),
        Expr::Spawn(e) => collect_old_idents_walker(e, out),
        Expr::Await(e) => collect_old_idents_walker(e, out),
        Expr::TypeOf(e) => collect_old_idents_walker(e, out),
        Expr::Range { start, end } => {
            collect_old_idents_walker(start, out);
            collect_old_idents_walker(end, out);
        }
        Expr::SliceExpr { target, start, end } => {
            collect_old_idents_walker(target, out);
            if let Some(s) = start {
                collect_old_idents_walker(s, out);
            }
            if let Some(e) = end {
                collect_old_idents_walker(e, out);
            }
        }
        Expr::Comprehension {
            expr,
            var: _,
            iter,
            guard,
        } => {
            collect_old_idents_walker(expr, out);
            collect_old_idents_walker(iter, out);
            if let Some(g) = guard {
                collect_old_idents_walker(g, out);
            }
        }
        Expr::Record { ty: _, fields } => {
            for f in fields {
                collect_old_idents_walker(&f.value, out);
            }
        }
        Expr::MapLiteral { entries } => {
            for (k, v) in entries {
                collect_old_idents_walker(k, out);
                collect_old_idents_walker(v, out);
            }
        }
        Expr::SetLiteral(es) => {
            for e in es {
                collect_old_idents_walker(e, out);
            }
        }
        Expr::Turbofish(_, _, args) => {
            for a in args {
                collect_old_idents_walker(a, out);
            }
        }
        Expr::TupleIndex(e, _) => collect_old_idents_walker(e, out),
        Expr::OptionalChain(e, _) => collect_old_idents_walker(e, out),
        Expr::NamedArg(_, e) => collect_old_idents_walker(e, out),
        Expr::Arena(stmts) | Expr::Comptime(stmts) | Expr::Quote(stmts) => {
            for s in stmts {
                collect_old_idents_in_stmt(s, out);
            }
        }
        Expr::QuoteInterpolate(e) => collect_old_idents_walker(e, out),
        Expr::Lambda {
            params: _,
            ret: _,
            body,
        } => {
            for s in body {
                collect_old_idents_in_stmt(s, out);
            }
        }
        Expr::TypeInfo(_) | Expr::Literal(_) | Expr::Ident(_) => {}
    }
}

/// Collect identifier names from within an `old(...)` expression. The root
/// identifier is the variable being snapshotted. For `old(x.foo)`, we
/// snapshot `x`. For `old(old(x))`, we recurse and snapshot `x`.
fn collect_idents_in_old(expr: &crate::ast::Expr, out: &mut Vec<String>) {
    use crate::ast::Expr;
    match expr {
        Expr::Ident(name) => out.push(name.clone()),
        Expr::Field(inner, _) | Expr::Index(inner, _) | Expr::TupleIndex(inner, _) => {
            collect_idents_in_old(inner, out);
        }
        Expr::OptionalChain(inner, _) => collect_idents_in_old(inner, out),
        Expr::Binary(_, l, r) => {
            collect_idents_in_old(l, out);
            collect_idents_in_old(r, out);
        }
        Expr::Unary(_, e) => collect_idents_in_old(e, out),
        Expr::Call(callee, args) => {
            collect_idents_in_old(callee, out);
            for a in args {
                collect_idents_in_old(a, out);
            }
        }
        Expr::Old(inner) => collect_idents_in_old(inner, out),
        Expr::Cast(e, _) => collect_idents_in_old(e, out),
        Expr::Tuple(es) | Expr::List(es) | Expr::SetLiteral(es) => {
            for e in es {
                collect_idents_in_old(e, out);
            }
        }
        Expr::Record { ty: _, fields } => {
            for f in fields {
                collect_idents_in_old(&f.value, out);
            }
        }
        // For complex expressions inside old(), collect all Idents recursively.
        _ => {
            // Fallback: walk the full expression and collect all Idents
            collect_all_idents(expr, out);
        }
    }
}

/// Fallback: collect ALL identifiers from any expression tree.
/// CG-H8: depth-limited to avoid stack overflow on pathological ASTs.
fn collect_all_idents(expr: &crate::ast::Expr, out: &mut Vec<String>) {
    collect_all_idents_depth(expr, out, 0);
}

const COLLECT_IDENTS_MAX_DEPTH: u32 = 256;

fn collect_all_idents_depth(expr: &crate::ast::Expr, out: &mut Vec<String>, depth: u32) {
    if depth > COLLECT_IDENTS_MAX_DEPTH {
        return;
    }
    use crate::ast::Expr;
    let d = depth + 1;
    match expr {
        Expr::Ident(name) => out.push(name.clone()),
        Expr::Binary(_, l, r) => {
            collect_all_idents_depth(l, out, d);
            collect_all_idents_depth(r, out, d);
        }
        Expr::Unary(_, e) => collect_all_idents_depth(e, out, d),
        Expr::Field(e, _) | Expr::Index(e, _) | Expr::TupleIndex(e, _) => {
            collect_all_idents_depth(e, out, d)
        }
        Expr::OptionalChain(e, _) => collect_all_idents_depth(e, out, d),
        Expr::Call(callee, args) => {
            collect_all_idents_depth(callee, out, d);
            for a in args {
                collect_all_idents_depth(a, out, d);
            }
        }
        Expr::Tuple(es) | Expr::List(es) | Expr::SetLiteral(es) => {
            for e in es {
                collect_all_idents_depth(e, out, d);
            }
        }
        Expr::If { cond, then_, else_ } => {
            collect_all_idents_depth(cond, out, d);
            for s in then_ {
                collect_all_idents_in_stmt_depth(s, out, d);
            }
            if let Some(e) = else_ {
                for s in e {
                    collect_all_idents_in_stmt_depth(s, out, d);
                }
            }
        }
        Expr::Match(scrut, arms) => {
            collect_all_idents_depth(scrut, out, d);
            for arm in arms {
                collect_all_idents_depth(&arm.body, out, d);
                if let Some(g) = &arm.guard {
                    collect_all_idents_depth(g, out, d);
                }
            }
        }
        Expr::Cast(e, _)
        | Expr::Try(e)
        | Expr::Spawn(e)
        | Expr::Await(e)
        | Expr::TypeOf(e)
        | Expr::Old(e)
        | Expr::QuoteInterpolate(e)
        | Expr::NamedArg(_, e) => collect_all_idents_depth(e, out, d),
        Expr::Range { start, end } => {
            collect_all_idents_depth(start, out, d);
            collect_all_idents_depth(end, out, d);
        }
        Expr::SliceExpr { target, start, end } => {
            collect_all_idents_depth(target, out, d);
            if let Some(s) = start {
                collect_all_idents_depth(s, out, d);
            }
            if let Some(e) = end {
                collect_all_idents_depth(e, out, d);
            }
        }
        Expr::Record { ty: _, fields } => {
            for f in fields {
                collect_all_idents_depth(&f.value, out, d);
            }
        }
        Expr::MapLiteral { entries } => {
            for (k, v) in entries {
                collect_all_idents_depth(k, out, d);
                collect_all_idents_depth(v, out, d);
            }
        }
        Expr::Turbofish(_, _, args) => {
            for a in args {
                collect_all_idents_depth(a, out, d);
            }
        }
        Expr::Block(stmts)
        | Expr::Arena(stmts)
        | Expr::Comptime(stmts)
        | Expr::Quote(stmts)
        | Expr::Lambda {
            params: _,
            ret: _,
            body: stmts,
        } => {
            for s in stmts {
                collect_all_idents_in_stmt_depth(s, out, d);
            }
        }
        Expr::Comprehension {
            expr,
            var: _,
            iter,
            guard,
        } => {
            collect_all_idents_depth(expr, out, d);
            collect_all_idents_depth(iter, out, d);
            if let Some(g) = guard {
                collect_all_idents_depth(g, out, d);
            }
        }
        Expr::TypeInfo(_) | Expr::Literal(_) => {}
    }
}

/// Walk statements for old() collection — covers Let init, Expr, Return,
/// If/While/For/Match bodies, etc.
fn collect_old_idents_in_stmt(stmt: &crate::ast::Stmt, out: &mut Vec<String>) {
    use crate::ast::Stmt;
    match stmt {
        Stmt::Expr(e) => collect_old_idents_walker(e, out),
        Stmt::Let { init: Some(e), .. } => collect_old_idents_walker(e, out),
        Stmt::Return(Some(e)) => collect_old_idents_walker(e, out),
        _ => {}
    }
}

fn collect_all_idents_in_stmt_depth(stmt: &crate::ast::Stmt, out: &mut Vec<String>, depth: u32) {
    use crate::ast::Stmt;
    match stmt {
        Stmt::Expr(e) => collect_all_idents_depth(e, out, depth),
        Stmt::Let { init: Some(e), .. } => collect_all_idents_depth(e, out, depth),
        Stmt::Return(Some(e)) => collect_all_idents_depth(e, out, depth),
        _ => {}
    }
}

// Submodules for clearly independent method groups. The originally suggested
// groups (params, actor, shared) do not map to standalone methods in this file:
//
// - Parameter handling and ABI layout are inlined in `compile_func` / `compile_generic_func`;
//   there is no `compile_param` helper to extract without restructuring logic.
// - Actor constructor / method compilation already lives in `codegen/actors.rs`.
// - Shared / RC scope cleanup helpers already live in `codegen/scope.rs` and `codegen/mod.rs`.
//
// What was split out:
// - `func/body.rs`: statement-level body helpers (loops and assignment forms).
// - `func/pattern.rs`: recursive `compile_pattern_bind`.
mod body;
mod export;
mod pattern;

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_async_func(&mut self, func: &FuncDef) -> MimiResult<()> {
        // 1. Compile the actual body as a hidden regular function
        let body_name = format!("{}__async_body", func.name);
        let body_func = FuncDef {
            name: body_name.clone(),
            pub_: false,
            params: func.params.clone(),
            ret: func.ret.clone(),
            body: func.body.clone(),
            where_clause: Vec::new(),
            generics: vec![],
            effects: vec![],
            is_comptime: false,
            is_async: false,
            extern_abi: None,
            pos: (0, 0),
        };
        self.compile_func(&body_func)?;

        let result_ty = func
            .ret
            .as_ref()
            .and_then(|t| self.llvm_type_for(t))
            .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
        let result_size = self.llvm_type_size_bytes(result_ty);
        let aligned_result = result_size.max(8);

        // Determine param types and sizes
        let mut param_types = Vec::new();
        let mut param_sizes: Vec<u64> = Vec::new();
        for param in &func.params {
            if let Some(ty) = self.llvm_type_for(&param.ty) {
                param_types.push(ty);
                param_sizes.push(self.llvm_type_size_bytes(ty));
            }
        }
        let total_args_size: u64 = param_sizes.iter().sum();
        // total allocation: 8 header + aligned_result (result) + total_args_size (args)
        let total_alloc_size = 8 + aligned_result + total_args_size;
        let args_offset: u64 = 8 + aligned_result;

        // i8 pointer type
        let i8_ty = self.context.i8_type();
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();

        // ── Step 2a: Generate poll function ──
        // void @foo_poll(i8* %future_ptr)
        let poll_name = format!("{}__poll", func.name);
        let poll_fn_type = i8_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
        let poll_fn = self.module.add_function(
            &poll_name,
            poll_fn_type,
            Some(inkwell::module::Linkage::Internal),
        );
        let poll_entry = self.context.append_basic_block(poll_fn, "entry");
        let saved_block = self.builder.get_insert_block();
        self.builder.position_at_end(poll_entry);

        let poll_future_ptr = poll_fn
            .get_nth_param(0)
            .ok_or_else(|| CompileError::LlvmError("poll_fn: param 0 not found".into()))?
            .into_pointer_value();

        // Load args from future + args_offset and call body
        let body_fn = self
            .module
            .get_function(&body_name)
            .ok_or_else(|| CompileError::LlvmError(format!("body fn '{}' not found", body_name)))?;
        let mut poll_call_args = Vec::new();
        let mut current_arg_offset = args_offset;
        for (param_idx, _param) in func.params.iter().enumerate() {
            if param_idx < param_types.len() {
                let ty = param_types[param_idx];
                let size = param_sizes[param_idx];
                // GEP to load arg: future + current_arg_offset
                let arg_ptr_i8 = self
                    .gep()
                    .build_gep(
                        i8_ty,
                        poll_future_ptr,
                        &[i64_ty.const_int(current_arg_offset, false)],
                        &format!("poll_arg_{}", param_idx),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("poll arg gep: {}", e)))?;
                let arg_typed_ptr = self
                    .builder
                    .build_pointer_cast(
                        arg_ptr_i8,
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        &format!("poll_arg_typed_{}", param_idx),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("poll arg cast: {}", e)))?;
                let arg_val =
                    self.build_load(ty, arg_typed_ptr, &format!("poll_arg_val_{}", param_idx))?;
                poll_call_args.push(match arg_val {
                    BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(iv),
                    BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(fv),
                    BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(pv),
                    BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(sv),
                    BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(av),
                    BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(vv),
                    BasicValueEnum::ScalableVectorValue(svv) => {
                        BasicMetadataValueEnum::ScalableVectorValue(svv)
                    }
                });
                current_arg_offset += size;
            }
        }

        let poll_body_result = self
            .build_call(body_fn, &poll_call_args, "poll_body_call")?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("poll body returned void".into()))?;

        // Store result at future + 8
        if !func
            .ret
            .as_ref()
            .map_or(true, |t| matches!(t, Type::Name(n, _) if n == "unit"))
        {
            let result_ptr_i8 = self
                .gep()
                .build_gep(
                    i8_ty,
                    poll_future_ptr,
                    &[i64_ty.const_int(8, false)],
                    "poll_result_ptr",
                )
                .map_err(|e| CompileError::LlvmError(format!("poll result gep: {}", e)))?;
            let result_typed_ptr = self
                .builder
                .build_pointer_cast(
                    result_ptr_i8,
                    self.context.ptr_type(inkwell::AddressSpace::default()),
                    "poll_result_typed",
                )
                .map_err(|e| CompileError::LlvmError(format!("poll result cast: {}", e)))?;
            self.build_store(result_typed_ptr, poll_body_result)?;
        }

        // Set completed
        let set_c_fn = self
            .module
            .get_function("mimi_future_set_completed")
            .ok_or_else(|| {
                CompileError::LlvmError("mimi_future_set_completed not declared".into())
            })?;
        self.build_call(
            set_c_fn,
            &[BasicMetadataValueEnum::PointerValue(poll_future_ptr)],
            "poll_set_completed",
        )?;

        self.build_return(None)?;

        // Restore insertion point
        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        // ── Step 2b: Generate async constructor function ──
        // foo(args...) -> i8*  (returns future pointer, submitted to executor)
        let metadata_params: Vec<_> = param_types
            .iter()
            .map(|t| types::basic_to_metadata(self.context, *t))
            .collect();

        let fn_type = i8_ptr_ty.fn_type(&metadata_params, false);
        let function = self.module.add_function(&func.name, fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        self.push_cap_scope();
        self.push_comp_scope();
        self.push_heap_scope();

        let mut vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
        for (i, param) in func.params.iter().enumerate() {
            if i < param_types.len() {
                let ty = param_types[i];
                let alloca = self.build_alloca(ty, &param.name)?;
                let param_val = function
                    .get_nth_param(i as u32)
                    .ok_or_else(|| CompileError::LlvmError(format!("param {} not found", i)))?;
                self.build_store(alloca, param_val)?;
                vars.insert(param.name.clone(), (alloca, ty));
                if let Type::Name(tn, args) = &param.ty {
                    if tn == "List" && !args.is_empty() {
                        if let Some(full) = self.get_full_type_name(&param.ty) {
                            self.var_type_names.insert(param.name.clone(), full);
                        }
                    } else {
                        self.var_type_names.insert(param.name.clone(), tn.clone());
                    }
                }
                // Register list element type for List<T> params where T is a struct
                self.register_list_elem_type(&param.name, &param.ty);
            }
        }

        // Allocate future: call mimi_future_alloc(total_size)
        let alloc_fn = self
            .module
            .get_function("mimi_future_alloc")
            .ok_or_else(|| CompileError::LlvmError("mimi_future_alloc not declared".into()))?;
        let total_size_val = i64_ty.const_int(total_alloc_size, false);
        let future_ptr = self
            .build_call(
                alloc_fn,
                &[BasicMetadataValueEnum::IntValue(total_size_val)],
                "future_alloc",
            )?
            .try_as_basic_value_opt()
            .map(|v: BasicValueEnum<'ctx>| v.into_pointer_value())
            .ok_or_else(|| CompileError::LlvmError("future_alloc returned non-pointer".into()))?;

        // Store args in future at args_offset
        let mut current_arg_store_offset = args_offset;
        for (param_idx, param) in func.params.iter().enumerate() {
            if param_idx < param_types.len() {
                let ty = param_types[param_idx];
                let size = param_sizes[param_idx];
                let alloca = vars.get(&param.name).ok_or_else(|| {
                    CompileError::LlvmError(format!("var '{}' not found", param.name))
                })?;
                let val = self.build_load(ty, alloca.0, &format!("store_{}", param.name))?;
                let arg_slot_i8 = self
                    .gep()
                    .build_gep(
                        i8_ty,
                        future_ptr,
                        &[i64_ty.const_int(current_arg_store_offset, false)],
                        &format!("arg_slot_{}", param_idx),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("arg slot gep: {}", e)))?;
                let arg_slot_typed = self
                    .builder
                    .build_pointer_cast(
                        arg_slot_i8,
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        &format!("arg_slot_typed_{}", param_idx),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("arg slot cast: {}", e)))?;
                self.build_store(arg_slot_typed, val)?;
                current_arg_store_offset += size;
            }
        }

        // Call mimi_executor_spawn(future, poll_fn)
        let spawn_fn = self
            .module
            .get_function("mimi_executor_spawn")
            .ok_or_else(|| CompileError::LlvmError("mimi_executor_spawn not declared".into()))?;
        let poll_fn_as_i8 = self
            .builder
            .build_pointer_cast(
                poll_fn.as_global_value().as_pointer_value(),
                i8_ptr_ty,
                "poll_fn_i8",
            )
            .map_err(|e| CompileError::LlvmError(format!("poll fn cast: {}", e)))?;
        self.build_call(
            spawn_fn,
            &[
                BasicMetadataValueEnum::PointerValue(future_ptr),
                BasicMetadataValueEnum::PointerValue(poll_fn_as_i8),
            ],
            "executor_spawn",
        )?;

        // Return the future pointer
        self.build_return(Some(&BasicValueEnum::PointerValue(future_ptr)))?;

        Ok(())
    }

    /// For a function returning `impl Trait`, extract the concrete return type
    /// from the function body (e.g., a record literal's type annotation).
    fn concrete_return_type_for_impl_trait(body: &[Stmt]) -> Option<String> {
        let last = body.last()?;
        match last {
            Stmt::Expr(expr) | Stmt::Return(Some(expr)) => match expr {
                Expr::Record { ty, .. } => ty.clone(),
                Expr::Call(callee, _) => {
                    if let Expr::Ident(_fname) = callee.as_ref() {
                        None
                    } else {
                        None
                    }
                }
                Expr::Block(block) => Self::concrete_return_type_for_impl_trait(block),
                _ => None,
            },
            Stmt::If {
                cond: _,
                then_,
                else_,
            } => {
                let then_ty = Self::concrete_return_type_for_impl_trait(then_);
                if then_ty.is_some() {
                    then_ty
                } else {
                    else_
                        .as_ref()
                        .and_then(|el| Self::concrete_return_type_for_impl_trait(el))
                }
            }
            Stmt::Block(block) => Self::concrete_return_type_for_impl_trait(block),
            _ => None,
        }
    }

    /// Snapshot live variable values at function entry so that `old(x)` in
    /// postconditions refers to the value at call time, not the current value.
    ///
    /// CG-H10 (audit): only snapshot variables that are actually referenced
    /// via `old(name)` inside `ensures` clauses. The previous implementation
    /// allocated a fresh alloca + load + store for *every* parameter and
    /// local, which produced O(N) wasted instructions on every function with
    /// postconditions.
    fn snapshot_old_values(&mut self, vars: &HashMap<String, VarEntry<'ctx>>) -> MimiResult<()> {
        self.old_snapshots.clear();
        if self.ensures_stmts.is_empty() {
            return Ok(());
        }
        let needed: std::collections::HashSet<String> = self
            .ensures_stmts
            .iter()
            .flat_map(collect_old_idents)
            .filter(|name| vars.contains_key(name))
            .collect();
        for name in needed {
            if let Some(&(alloca, ty)) = vars.get(&name) {
                let old_alloca = self.build_alloca(ty, &format!("{}_old", name))?;
                let val = self.build_load(ty, alloca, &format!("{}_snap", name))?;
                self.build_store(old_alloca, val)?;
                self.old_snapshots.insert(name, (old_alloca, ty));
            }
        }
        Ok(())
    }

    /// Collect `ensures` contracts and compile `requires` contracts as runtime
    /// assertions when contract verification is enabled.
    fn prepare_func_contracts(
        &mut self,
        func: &FuncDef,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        self.ensures_stmts = if self.verify_contracts {
            collect_ensures(&func.body)
        } else {
            Vec::new()
        };
        if self.verify_contracts {
            for stmt in &func.body {
                if let Stmt::Requires(expr, _) = stmt {
                    self.compile_contract_assert(
                        expr,
                        vars,
                        &format!("requires violation in '{}'", func.name),
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Transfer ownership of a string return value from local heap tracking to
    /// the caller. For string-typed returns this prevents `free_heap_allocs`
    /// from freeing the data that the caller will receive.
    ///
    /// CLOSE-GAP-5 (v0.28.19): if the returned data pointer isn't already a
    /// heap allocation (e.g. literal `"hello"` keeps a `.rodata` pointer),
    /// heap-copy it so the caller's `free_heap_allocs` can safely release it
    /// via the struct's data pointer. For expressions that already own heap
    /// allocations (concat, f-string, builtin raw returns) we pop the most
    /// recent registration as before.
    /// Check if a BasicTypeEnum is a Mimi `string` struct ({ptr,i64}).
    fn is_string_llvm_type(ty: BasicTypeEnum<'ctx>) -> bool {
        match ty {
            BasicTypeEnum::StructType(st) => {
                let fields = st.get_field_types();
                fields.len() == 2
                    && matches!(fields[0], BasicTypeEnum::PointerType(_))
                    && matches!(fields[1], BasicTypeEnum::IntType(_))
            }
            _ => false,
        }
    }

    /// Check if an expression produces a heap-allocated string whose allocation
    /// is tracked by `heap_allocs`.  Such expressions need their heap pointer
    /// popped from the tracking stack before `free_heap_allocs` runs, otherwise
    /// the string data gets freed before the caller can use it.
    ///
    /// Note: the `str_*` / `format` / `to_string` matchers assume those names
    /// are the Mimi builtins. If a user shadows one of these names with their
    /// own function, this check will mis-classify the result. The names are
    /// hardcoded rather than `starts_with("str_")` (audit CG-H12) precisely
    /// to avoid the prefix-collision case; the remaining risk is a deliberate
    /// user name collision.
    fn is_string_temp_expr(expr: &Expr, val: &BasicValueEnum<'ctx>) -> bool {
        match expr {
            Expr::Binary(BinOp::Add, _, _) => true,
            Expr::Literal(Lit::FString(_)) => true,
            Expr::Call(callee, _) => {
                matches!(val, BasicValueEnum::PointerValue(_))
                    || matches!(
                        callee.as_ref(),
                        Expr::Ident(name)
                            if matches!(
                                name.as_str(),
                                "str_concat"
                                    | "str_repeat"
                                    | "str_slice"
                                    | "str_trim"
                                    | "str_join"
                                    | "str_from"
                                    | "to_string"
                                    | "format"
                            )
                    )
            }
            _ => false,
        }
    }

    /// MEM-C13: if returning a closure `{fn_ptr, env_ptr}`, pop the env heap
    /// pointer from the current `heap_allocs` scope so `free_heap_allocs` does
    /// not free an env the caller still owns.
    pub(in crate::codegen) fn claim_returned_closure_env(
        &self,
        val: BasicValueEnum<'ctx>,
        ret_type: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let is_closure = match ret_type {
            BasicTypeEnum::StructType(st) => {
                let fields = st.get_field_types();
                fields.len() == 2
                    && matches!(fields[0], BasicTypeEnum::PointerType(_))
                    && matches!(fields[1], BasicTypeEnum::PointerType(_))
            }
            _ => false,
        };
        if !is_closure {
            return Ok(val);
        }
        // Env was registered as the most recent raw heap ptr when the lambda
        // was built (see build_closure_struct). Pop it so free_heap_allocs
        // leaves the env alive for the caller.
        let _ = self.pop_last_heap_ptr();
        let _ = val; // value passes through unchanged
        Ok(val)
    }

    pub(in crate::codegen) fn claim_string_return_value(
        &self,
        val: BasicValueEnum<'ctx>,
        ret_type: BasicTypeEnum<'ctx>,
        expr: Option<&Expr>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // Closures are not strings; claim env ownership first when applicable.
        let val = self.claim_returned_closure_env(val, ret_type)?;
        let is_string_struct = match ret_type {
            BasicTypeEnum::StructType(st) => {
                let fields = st.get_field_types();
                fields.len() == 2
                    && matches!(fields[0], BasicTypeEnum::PointerType(_))
                    && matches!(fields[1], BasicTypeEnum::IntType(_))
            }
            _ => false,
        };
        if !is_string_struct {
            // Check for variant struct (Option/Result) whose payload is a string.
            // E.g. `Ok(s + "-wrapped")` returns `Result<string, ...>` – the inner
            // string concat's heap allocation must survive free_heap_allocs.
            let is_variant_with_string_payload = match ret_type {
                BasicTypeEnum::StructType(st) => {
                    let fields = st.get_field_types();
                    if fields.len() >= 2 {
                        matches!(fields[0], BasicTypeEnum::IntType(it) if it.get_bit_width() == 1)
                            && Self::is_string_llvm_type(fields[1])
                    } else {
                        false
                    }
                }
                _ => false,
            };
            if is_variant_with_string_payload {
                // Check if the expression is a constructor wrapping a string temp,
                // e.g. `Ok(s + "-wrapped")`.  The inner string temp's heap pointer
                // must be popped so free_heap_allocs doesn't free it before return.
                if let Some(Expr::Call(callee, args)) = expr {
                    if args.len() == 1 && Self::is_string_temp_expr(&args[0], &val) {
                        if let Expr::Ident(name) = callee.as_ref() {
                            if matches!(name.as_str(), "Ok" | "Err" | "Some" | "None") {
                                let _ = self.pop_last_heap_ptr();
                            }
                        }
                    }
                }
            }
            return Ok(val);
        }

        // Returning a string variable: load the struct value and null out the
        // variable slot's data pointer so the slot is not freed before return.
        if let Some(Expr::Ident(name)) = expr {
            if self
                .var_type_names
                .get(name)
                .map(|t| t == "string")
                .unwrap_or(false)
            {
                if let Some(&(alloca, ty)) = vars.get(name) {
                    let loaded = self.build_load(ty, alloca, &format!("{}_ret", name))?;
                    let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
                    if let BasicTypeEnum::StructType(st) = ty {
                        if let Ok(data_gep) = self.gep().build_struct_gep(
                            st,
                            alloca,
                            0,
                            &format!("{}_ret_null", name),
                        ) {
                            let _ = self.builder.build_store(data_gep, null_ptr);
                        }
                    }
                    // CLOSE-GAP-5: heap-copy the loaded struct so the caller
                    // side has unambiguous ownership. The original data may be
                    // a `.rodata` global (for `let s = "hi"; s`), in which case
                    // without this copy the caller would `free()` a global
                    // pointer.
                    return self.heap_copy_string_value(loaded);
                }
            }
        }

        // For concat / fstring / builtin raw returns, the most-recent heap
        // registration owns the returned data; pop it so free_heap_allocs
        // doesn't release it before the caller sees it.
        let is_string_temp = match expr {
            Some(Expr::Binary(BinOp::Add, _, _)) => true,
            Some(Expr::Literal(Lit::FString(_))) => true,
            Some(Expr::Call(callee, _)) => {
                matches!(val, BasicValueEnum::PointerValue(_))
                    || matches!(
                        callee.as_ref(),
                        Expr::Ident(name) if name.starts_with("str_") || name == "to_string"
                    )
            }
            _ => false,
        };
        if is_string_temp {
            let _ = self.pop_last_heap_ptr();
        }

        match val {
            BasicValueEnum::PointerValue(pv) => {
                // Raw pointer result (string literal or builtin raw return).
                // `heap_copy_string_value` handles the wrap (via strlen) +
                // copy in one step.
                self.heap_copy_string_value(pv.into())
            }
            BasicValueEnum::StructValue(sv) => {
                // The struct's data pointer is referenced by the caller. If
                // ownership was transferred (pop in the previous block), the
                // data ptr is heap-owned by the result; the caller will free
                // it. If we did not pop (e.g. literal, expr = None), the data
                // ptr is a `.rodata` global — heap-copy it first.
                if is_string_temp {
                    Ok(BasicValueEnum::StructValue(sv))
                } else {
                    self.heap_copy_string_value(sv.into())
                }
            }
            _ => Ok(val),
        }
    }

    /// Heap-copy the data field of a Mimi `string` struct so the returned
    /// value is always backed by a freshly-allocated buffer. The caller (and
    /// only the caller) takes ownership. Non-string structs pass through.
    ///
    /// IMPORTANT: this does *not* register the freshly-allocated buffer on
    /// the callee's heap_allocs stack — that would cause `free_heap_allocs`
    /// to release the buffer before the return instruction completes. The
    /// caller is expected to register the resulting struct's data pointer
    /// (see `emit_function_call::track_string_return_lifetime`).
    fn heap_copy_string_value(
        &self,
        val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let (data_pv, len_iv) = match val {
            // A pointer-to-C-string (e.g. raw `i8*` from a literal, a string
            // variable holding a raw pointer, or a builtin raw-pointer
            // return). Compute the length via `strlen`, then build the
            // struct.
            BasicValueEnum::PointerValue(pv) => {
                let strlen_fn = self.get_runtime_fn("strlen")?;
                let length = self
                    .build_call(
                        strlen_fn,
                        &[BasicMetadataValueEnum::PointerValue(pv)],
                        "ret_str_strlen",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("strlen returned void".into()))?
                    .into_int_value();
                (pv, length)
            }
            BasicValueEnum::StructValue(sv) => {
                let sty = sv.get_type();
                let fields = sty.get_field_types();
                let is_mimi_string_struct = fields.len() == 2
                    && matches!(fields[0], BasicTypeEnum::PointerType(_))
                    && matches!(fields[1], BasicTypeEnum::IntType(_));
                if !is_mimi_string_struct {
                    return Ok(sv.into());
                }
                let data_ptr = self.build_extract_value(sv.into(), 0, "ret_str_data")?;
                let data_pv = match data_ptr {
                    BasicValueEnum::PointerValue(pv) => pv,
                    _ => return Ok(sv.into()),
                };
                let len_iv = match self.build_extract_value(sv.into(), 1, "ret_str_len")? {
                    BasicValueEnum::IntValue(iv) => iv,
                    _ => return Ok(sv.into()),
                };
                (data_pv, len_iv)
            }
            other => return Ok(other),
        };
        let i64_ty = self.context.i64_type();
        // len + 1 for the trailing nul so callers may use the result as a C
        // string.
        let alloc_len = self
            .builder
            .build_int_add(len_iv, i64_ty.const_int(1, false), "ret_str_alloc_len")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        // CG-H6 (audit): round `alloc_len` up to an 8-byte boundary so the
        // returned buffer is always 8-byte aligned. malloc() is allowed to
        // return any alignment, but downstream SIMD/word-size memcpy and
        // GEP operations assume 8-byte alignment on architectures such as
        // ARM/SPARC. Without this round-up we can produce UB on those
        // platforms.
        let seven = i64_ty.const_int(7, false);
        let rounded_minus_one = self
            .builder
            .build_int_add(alloc_len, seven, "ret_str_align_add")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        let aligned_len = self
            .builder
            .build_and(
                rounded_minus_one,
                i64_ty.const_int(!7u64, false),
                "ret_str_align",
            )
            .map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?;
        // B4: NULL-checked malloc.
        let heap_ptr = self.malloc_or_abort(aligned_len, "ret_str_malloc")?;
        let memcpy_fn = self.get_runtime_fn("memcpy")?;
        let _ = self.build_call(
            memcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(heap_ptr),
                BasicMetadataValueEnum::PointerValue(data_pv),
                BasicMetadataValueEnum::IntValue(len_iv),
            ],
            "ret_str_memcpy",
        )?;
        // Write nul terminator at heap_ptr[len].
        let i8_ty = self.context.i8_type();
        let nul_pos = self.build_in_bounds_gep(i8_ty, heap_ptr, &[len_iv], "ret_str_nul_pos")?;
        self.build_store(nul_pos, i8_ty.const_int(0, false))?;
        // Build the canonical {i8*, i64} struct.
        let sty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let new_sv = self
            .builder
            .build_insert_value(sty.get_undef(), heap_ptr, 0, "ret_str_new_data")
            .map_err(|e| CompileError::LlvmError(format!("insert str data: {}", e)))?
            .into_struct_value();
        let new_sv = self
            .builder
            .build_insert_value(new_sv, len_iv, 1, "ret_str_new_len")
            .map_err(|e| CompileError::LlvmError(format!("insert str len: {}", e)))?
            .into_struct_value();
        Ok(BasicValueEnum::StructValue(new_sv))
    }

    /// Emit a function return: check `ensures` contracts, clean up scopes, and
    /// build the LLVM return instruction. `val` of `None` means a bare `return;`.
    fn emit_return(
        &mut self,
        ret_type: BasicTypeEnum<'ctx>,
        ret_ty_ast: Option<&Type>,
        val: Option<BasicValueEnum<'ctx>>,
        func_name: &str,
        vars: &HashMap<String, VarEntry<'ctx>>,
        expr: Option<&Expr>,
    ) -> MimiResult<()> {
        let ensures = self.ensures_stmts.clone();
        // Adjust the value once and reuse for both ensures check and return,
        // avoiding double application of adjust_int_val (which is not idempotent).
        let val = val
            .map(|v| -> Result<BasicValueEnum<'ctx>, CompileError> {
                let adjusted = self.adjust_int_val(v, ret_type)?;
                Ok(adjusted)
            })
            .transpose()?;
        if !ensures.is_empty() {
            let result_alloca = self.build_alloca(ret_type, "result")?;
            let stored_val =
                val.unwrap_or_else(|| self.context.i64_type().const_int(0, false).into());
            self.build_store(result_alloca, stored_val)?;
            let mut ensures_vars = vars.clone();
            ensures_vars.insert("result".to_string(), (result_alloca, ret_type));
            for ensures_expr in &ensures {
                self.compile_contract_assert(
                    ensures_expr,
                    &ensures_vars,
                    &format!("ensures violation in '{}'", func_name),
                )?;
            }
        }
        let val = val
            .map(|v| self.claim_string_return_value(v, ret_type, expr, vars))
            .transpose()?;
        self.pop_shared_scope()?;
        self.free_heap_allocs()?;
        self.pop_comp_scope();
        self.pop_cap_scope();
        match val {
            Some(v) => {
                let adjusted = self.coerce_variant_value(v, ret_type, ret_ty_ast)?;
                let adjusted = self.load_return_value_if_needed(adjusted)?;
                self.build_return(Some(&adjusted))?;
            }
            None => self.build_return(None)?,
        }
        Ok(())
    }

    /// Coerce a generic Result/Option constructor value to the declared return
    /// type's concrete layout.
    ///
    /// `Ok`/`Err`/`Some`/`None` are currently compiled using the payload type that
    /// the constructor sees at the call site. When such a value is returned from a
    /// function whose declared return type has a different payload layout (e.g.
    /// `Result<string, E>` where the string payload is represented as `{ptr, i64}`
    /// but the constructor saw a raw `ptr`), the LLVM struct types no longer match
    /// and the caller misinterprets the bytes. This helper repacks the
    /// discriminant and payload into the target layout.
    fn coerce_variant_value(
        &self,
        val: BasicValueEnum<'ctx>,
        target_ty: BasicTypeEnum<'ctx>,
        ret_ty_ast: Option<&Type>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let ast_ty = match ret_ty_ast {
            Some(t) => t,
            None => return Ok(val),
        };
        let is_result = matches!(ast_ty, Type::Result(_, _))
            || matches!(ast_ty, Type::Name(n, args) if n == "Result" && args.len() == 2);
        let is_option = matches!(ast_ty, Type::Option(_))
            || matches!(ast_ty, Type::Name(n, args) if n == "Option" && args.len() == 1);
        if !is_result && !is_option {
            return Ok(val);
        }

        let target_st = match target_ty {
            BasicTypeEnum::StructType(st) => st,
            _ => return Ok(val),
        };

        // If the value is already a pointer (e.g. an alloca), try loading it as the
        // target type. When the pointer already points to the target layout this is
        // sufficient; generic allocas are handled by the StructValue path below.
        let sv = match val {
            BasicValueEnum::StructValue(sv) => sv,
            BasicValueEnum::PointerValue(pv) => {
                let loaded = self.build_load(target_ty, pv, "coerce_load")?;
                match loaded {
                    BasicValueEnum::StructValue(sv) => sv,
                    _ => return Ok(val),
                }
            }
            _ => return Ok(val),
        };

        let source_st = sv.get_type();
        if source_st == target_st {
            return Ok(val);
        }

        let source_fields = source_st.get_field_types();
        let target_fields = target_st.get_field_types();
        if source_fields.len() != target_fields.len() {
            return Ok(val);
        }

        let alloca = self.build_alloca(BasicTypeEnum::StructType(target_st), "variant_coerce")?;
        for (i, tf) in target_fields.iter().enumerate() {
            let gep = self
                .gep()
                .build_struct_gep(
                    BasicTypeEnum::StructType(target_st),
                    alloca,
                    i as u32,
                    "coerce_gep",
                )
                .map_err(|e| CompileError::LlvmError(format!("coerce gep: {}", e)))?;
            if i == 0 {
                let disc = self.build_extract_value(sv.into(), 0, "coerce_disc")?;
                self.build_store(gep, disc)?;
            } else if is_result && i == target_fields.len() - 1 {
                // CG-H6: extract Err field by target index (same layout index),
                // not source_fields.len()-1 which can mis-index when layouts differ
                // only in field types (same length was checked above).
                let err_idx = (target_fields.len() - 1) as u32;
                let err = self.build_extract_value(sv.into(), err_idx, "coerce_err")?;
                let err = self.coerce_field_to_type(err, *tf)?;
                self.build_store(gep, err)?;
            } else {
                let payload = self.build_extract_value(sv.into(), i as u32, "coerce_payload")?;
                let payload = self.coerce_field_to_type(payload, *tf)?;
                self.build_store(gep, payload)?;
            }
        }
        self.build_load(BasicTypeEnum::StructType(target_st), alloca, "coerced")
    }

    /// Helper used by `coerce_variant_value` to convert a single source field into
    /// the corresponding target field type.
    fn coerce_field_to_type(
        &self,
        val: BasicValueEnum<'ctx>,
        target: BasicTypeEnum<'ctx>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if val.get_type() == target {
            return Ok(val);
        }
        match (val, target) {
            // Wrap a raw C string pointer into the Mimi string struct {ptr, len}.
            (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(st))
                if Self::is_mimi_string_struct(st) =>
            {
                self.wrap_c_string(pv)
            }
            // Generic pad (i64 zero) -> structured payload: zero-initialize the target.
            (BasicValueEnum::IntValue(_), BasicTypeEnum::StructType(st)) => {
                Ok(BasicValueEnum::StructValue(st.const_zero()))
            }
            // Pointer -> integer (e.g. ptr err payload stored as i64).
            (BasicValueEnum::PointerValue(pv), BasicTypeEnum::IntType(it)) => {
                Ok(self.build_ptr_to_int(pv, it, "coerce_ptr_to_int")?.into())
            }
            // Integer width conversion.
            (BasicValueEnum::IntValue(_), BasicTypeEnum::IntType(_)) => {
                self.adjust_int_val(val, target)
            }
            _ => Ok(val),
        }
    }

    /// Returns true if `st` is the Mimi string struct `{ ptr, i64 }`.
    fn is_mimi_string_struct(st: inkwell::types::StructType<'ctx>) -> bool {
        let fields = st.get_field_types();
        fields.len() == 2
            && matches!(&fields[0], BasicTypeEnum::PointerType(_))
            && matches!(&fields[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64)
    }

    /// Bind all function parameters to stack allocas and track type metadata
    /// (type names, list element types, and capabilities).
    fn bind_func_params(
        &mut self,
        func: &FuncDef,
        function: FunctionValue<'ctx>,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        for (i, param) in func.params.iter().enumerate() {
            let resolved = self.resolve_type(&param.ty);
            if let Some(ty) = self.llvm_type_for(&resolved) {
                let mut param_val = function.get_nth_param(i as u32).ok_or_else(|| {
                    CompileError::LlvmError(format!(
                        "param index {} out of range for function '{}' with {} params",
                        i,
                        func.name,
                        function.count_params()
                    ))
                })?;
                // view/mutate parameters use the caller's storage directly.
                // This is the reference ABI promised by ParamBorrow: mutations
                // to a List header (len/data after realloc) become visible to
                // the caller instead of modifying a callee-local copy.
                let alloca = if param.borrow.is_some() {
                    param_val.into_pointer_value()
                } else {
                    let slot = self.build_alloca(ty, &param.name)?;
                    // String parameters may be passed as raw i8* pointers (e.g. string
                    // literals or list indexing). Wrap them in the canonical
                    // {i8*, i64} struct so the rest of the function body sees a
                    // well-formed Mimi string.
                    if let Type::Name(tn, _) = &resolved {
                        if tn == "string" {
                            if let BasicValueEnum::PointerValue(pv) = param_val {
                                let strlen_fn = self.get_runtime_fn("strlen")?;
                                let len = self
                                    .build_call(
                                        strlen_fn,
                                        &[BasicMetadataValueEnum::PointerValue(pv)],
                                        "param_strlen",
                                    )?
                                    .try_as_basic_value_opt()
                                    .ok_or("strlen returned void")?
                                    .into_int_value();
                                param_val = self.build_string_struct(pv, len)?;
                            }
                        }
                    }
                    self.build_store(slot, param_val)?;
                    slot
                };
                vars.insert(param.name.clone(), (alloca, ty));

                // Track type name for method dispatch
                if let Type::Name(tn, args) = &resolved {
                    if tn == "List" && !args.is_empty() {
                        if let Some(full) = self.get_full_type_name(&resolved) {
                            self.var_type_names.insert(param.name.clone(), full);
                        }
                    } else {
                        self.var_type_names.insert(param.name.clone(), tn.clone());
                    }
                    self.var_types.insert(param.name.clone(), resolved.clone());
                }
                if let Type::Ref(_, inner) | Type::RefMut(_, inner) = &resolved {
                    if let Type::Name(tn, args) = inner.as_ref() {
                        if tn == "List" && !args.is_empty() {
                            if let Some(full) = self.get_full_type_name(inner) {
                                self.var_type_names.insert(param.name.clone(), full);
                            }
                        } else {
                            self.var_type_names.insert(param.name.clone(), tn.clone());
                        }
                        self.var_types
                            .insert(param.name.clone(), inner.as_ref().clone());
                    }
                }
                if let Type::DynTrait(_) = &resolved {
                    self.var_type_names
                        .insert(param.name.clone(), crate::core::fmt_type(&resolved));
                    self.var_types.insert(param.name.clone(), resolved.clone());
                }
                if let Type::ImplTrait(_) = &resolved {
                    self.var_type_names
                        .insert(param.name.clone(), crate::core::fmt_type(&resolved));
                    self.var_types.insert(param.name.clone(), resolved.clone());
                }
                if let Type::Func(_, _) | Type::ExternFunc(_, _) = &resolved {
                    self.var_type_names
                        .insert(param.name.clone(), crate::core::fmt_type(&resolved));
                    self.var_types.insert(param.name.clone(), resolved.clone());
                }

                // Register list element type for List<T> params where T is a struct
                self.register_list_elem_type(&param.name, &resolved);

                // Track capability parameters
                if matches!(&param.ty, Type::Cap(_)) {
                    self.register_cap(&param.name, alloca);
                }
            }
        }
        Ok(())
    }

    /// Compile the body of a non-generic function statement-by-statement.
    /// Returns `ControlFlow::Break(())` when an explicit `return` statement
    /// has already emitted the terminator; otherwise returns the implicit last
    /// value that should be returned.
    fn compile_func_body(
        &mut self,
        func: &FuncDef,
        ret_type: BasicTypeEnum<'ctx>,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<ControlFlow<(), BasicValueEnum<'ctx>>> {
        let ret_ty_ast = func.ret.as_ref();
        // audit (MEDIUM): empty function bodies must not silently return a
        // default value of the wrong type (e.g. i64(0) for a struct-returning
        // function). For empty bodies with struct return, use `undef` —
        // this is safe because empty-body functions are abstract declarations
        // that are never called directly (LLVM `undef` is only UB if the
        // caller actually uses the return value, and abstract functions are
        // never called). For non-empty bodies, the default is overwritten by
        // the last expression in the body.
        let default_val = match ret_type {
            BasicTypeEnum::IntType(t) => t.const_int(0, false).into(),
            BasicTypeEnum::FloatType(t) => t.const_float(0.0).into(),
            BasicTypeEnum::StructType(st) if func.body.is_empty() => {
                // SAFETY: empty-body functions are abstract declarations
                // (e.g. trait method signatures). They are never called, so
                // returning `undef` does not cause UB at runtime.
                return Ok(ControlFlow::Continue(st.get_undef().into()));
            }
            BasicTypeEnum::StructType(_) => {
                // Non-empty body with struct return: placeholder, will be
                // overwritten by the last expression in the body.
                self.context.i64_type().const_int(0, false).into()
            }
            _ => {
                // PointerType, ArrayType, etc. — safe scalar default.
                self.context.i64_type().const_int(0, false).into()
            }
        };
        let mut last_val: BasicValueEnum<'ctx> = default_val;
        for stmt in &func.body {
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
                    last_val = self.compile_expr(expr, vars)?;
                    last_val = self.adjust_int_val(last_val, ret_type)?;
                    last_val = self.coerce_variant_value(last_val, ret_type, ret_ty_ast)?;
                }
                Stmt::Return(Some(expr)) => {
                    let val = self.compile_expr(expr, vars)?;
                    let val = self.adjust_int_val(val, ret_type)?;
                    self.emit_return(
                        ret_type,
                        ret_ty_ast,
                        Some(val),
                        &func.name,
                        vars,
                        Some(expr),
                    )?;
                    return Ok(ControlFlow::Break(()));
                }
                Stmt::Return(None) => {
                    self.emit_return(ret_type, ret_ty_ast, None, &func.name, vars, None)?;
                    return Ok(ControlFlow::Break(()));
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
                        vars.insert(name.clone(), (fat_alloca, fat_ty));
                        if let Some(Type::Cap(_)) = &ty {
                            self.register_cap(&name, fat_alloca);
                        }
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
                    // Normalize string values: wrap raw pointers into canonical
                    // {i8*, i64} struct so variable allocas have consistent type.
                    val = self.normalize_string_value(val, init)?;
                    // Track type info for simple Variable patterns
                    if let Pattern::Variable(name) = pat {
                        if let Some(ty_ref) = &ty {
                            if let Type::Name(tn, args) = ty_ref {
                                if !args.is_empty() {
                                    // Store full generic type name for method dispatch
                                    if let Some(full) = self.get_full_type_name(ty_ref) {
                                        self.var_type_names.insert(name.clone(), full);
                                    }
                                } else {
                                    self.var_type_names.insert(name.clone(), tn.clone());
                                }
                            }
                        } else if self.expr_is_string(init) {
                            self.var_type_names
                                .insert(name.clone(), "string".to_string());
                        } else if let Expr::Record {
                            ty: Some(tn),
                            fields,
                        } = init
                        {
                            self.var_type_names.insert(name.clone(), tn.clone());
                            // Infer concrete generic args from field values (e.g.
                            // `Pair { a: 10, b: 20 }` → `Pair<i32>`).
                            if let Some(td) = self.type_defs.get(tn) {
                                if !td.generics.is_empty() {
                                    let type_params: Vec<String> =
                                        td.generics.iter().map(|g| g.name.clone()).collect();
                                    let param_types: HashMap<String, Type> = self
                                        .try_infer_generic_from_fields(
                                            td,
                                            fields,
                                            vars,
                                            &type_params,
                                        );
                                    if param_types.len() == td.generics.len() {
                                        let args: Vec<Type> =
                                            td.generics
                                                .iter()
                                                .map(|g| {
                                                    param_types.get(&g.name).cloned().unwrap_or(
                                                        Type::Name(g.name.clone(), vec![]),
                                                    )
                                                })
                                                .collect();
                                        self.var_types
                                            .insert(name.clone(), Type::Name(tn.clone(), args));
                                    }
                                }
                            }
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
                        } else if let Expr::Call(callee, call_args) = init {
                            if let Expr::Field(obj, method_name) = callee.as_ref() {
                                if method_name == "spawn" || method_name == "spawn_detached" {
                                    let obj_type = self.infer_object_type(obj, vars);
                                    if !obj_type.is_empty() {
                                        self.var_type_names.insert(name.clone(), obj_type);
                                    }
                                } else if matches!(
                                    method_name.as_str(),
                                    "map" | "and_then" | "map_err" | "ok_or"
                                ) {
                                    // ok_or converts Option<T> → Result<T,E>;
                                    // map/and_then/map_err preserve the caller's variant type.
                                    if method_name == "ok_or" {
                                        self.var_type_names
                                            .insert(name.clone(), "Result".to_string());
                                    } else {
                                        let obj_type = self.infer_object_type(obj, vars);
                                        if obj_type.starts_with("Result") {
                                            self.var_type_names
                                                .insert(name.clone(), "Result".to_string());
                                        } else if obj_type.starts_with("Option") {
                                            self.var_type_names
                                                .insert(name.clone(), "Option".to_string());
                                        }
                                    }
                                } else if matches!(method_name.as_str(), "insert" | "remove") {
                                    let obj_type = self.infer_object_type(obj, vars);
                                    if obj_type.starts_with("Set") || obj_type == "set" {
                                        self.var_type_names.insert(name.clone(), obj_type);
                                    } else if let Expr::Ident(flow_name) = obj.as_ref() {
                                        // Flow::transition — insert/remove may be flow
                                        // transition names, not Set operations.
                                        if let Some(flow) = self.flow_defs.get(flow_name) {
                                            let from_type = call_args
                                                .first()
                                                .map(|a| self.infer_object_type(a, vars))
                                                .unwrap_or_default();
                                            let t = flow
                                                .transitions
                                                .iter()
                                                .find(|t| {
                                                    t.name == *method_name
                                                        && t.from_state == from_type
                                                })
                                                .or_else(|| {
                                                    flow.transitions
                                                        .iter()
                                                        .find(|t| t.name == *method_name)
                                                });
                                            if let Some(t) = t {
                                                if let Some(to) = t.to_states.first() {
                                                    self.var_type_names
                                                        .insert(name.clone(), to.clone());
                                                }
                                            }
                                        }
                                    }
                                } else if method_name == "upgrade" {
                                    self.track_weak_upgrade_type(name, obj);
                                } else {
                                    // Generic method call: infer return type
                                    let obj_type = self.infer_object_type(obj, vars);
                                    if obj_type == "string" {
                                        let ret_type =
                                            self.infer_string_method_return_type(method_name);
                                        if !ret_type.is_empty() {
                                            self.var_type_names.insert(name.clone(), ret_type);
                                        }
                                    } else if let Expr::Ident(flow_name) = obj.as_ref() {
                                        // Flow::transition(from, ...) → matching overload's to-state
                                        if let Some(flow) = self.flow_defs.get(flow_name) {
                                            let from_type = call_args
                                                .first()
                                                .map(|a| self.infer_object_type(a, vars))
                                                .unwrap_or_default();
                                            let t = flow
                                                .transitions
                                                .iter()
                                                .find(|t| {
                                                    t.name == *method_name
                                                        && t.from_state == from_type
                                                })
                                                .or_else(|| {
                                                    flow.transitions
                                                        .iter()
                                                        .find(|t| t.name == *method_name)
                                                });
                                            if let Some(t) = t {
                                                if let Some(to) = t.to_states.first() {
                                                    self.var_type_names
                                                        .insert(name.clone(), to.clone());
                                                }
                                            }
                                        }
                                    }
                                }
                            } else if let Expr::Ident(func_name) = callee.as_ref() {
                                match func_name.as_str() {
                                    "values" => {
                                        self.var_type_names
                                            .insert(name.clone(), "List<Any>".to_string());
                                    }
                                    "keys" => {
                                        self.var_type_names
                                            .insert(name.clone(), "List<string>".to_string());
                                    }
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
                                        } else if self.type_defs.get(func_name).is_some_and(|td| {
                                            matches!(td.kind, crate::ast::TypeDefKind::Newtype(_))
                                        }) {
                                            self.var_type_names
                                                .insert(name.clone(), func_name.clone());
                                        } else if let Some((ret_ty, _is_async)) = self
                                            .func_defs
                                            .get(func_name)
                                            .map(|fdef| (fdef.ret.clone(), fdef.is_async))
                                        {
                                            if let Some(ret_ty) = ret_ty {
                                                match &ret_ty {
                                                    Type::ImplTrait(traits) => {
                                                        self.var_type_names.insert(
                                                            name.clone(),
                                                            format!("impl {}", traits.join(" + ")),
                                                        );
                                                    }
                                                    Type::Name(tn, _) => {
                                                        let resolved =
                                                            self.substitute_type_params(&ret_ty);
                                                        let type_name = if let Some(full) =
                                                            self.get_full_type_name(&resolved)
                                                        {
                                                            full
                                                        } else {
                                                            tn.clone()
                                                        };
                                                        self.var_type_names
                                                            .insert(name.clone(), type_name);
                                                        self.var_types
                                                            .insert(name.clone(), ret_ty.clone());
                                                        self.register_list_elem_type(
                                                            name, &resolved,
                                                        );
                                                    }
                                                    _ => {}
                                                }
                                            }
                                        } else if let Some(crate::ast::Type::Name(tn, _)) = self
                                            .extern_func_defs
                                            .get(func_name)
                                            .and_then(|ef| ef.ret.as_ref())
                                        {
                                            self.var_type_names.insert(name.clone(), tn.clone());
                                        }
                                        // G-41: Track return types for builtins and std
                                        // functions that return List<string>.
                                        match func_name.as_str() {
                                            "listdir" | "walk_dir" | "str_split" | "words"
                                            | "lines" | "split" | "sort_str" | "keys" => {
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
                                            "exec" | "exec_safe" => {
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
                                            "getenv" | "base64_decode" => {
                                                self.var_type_names.insert(
                                                    name.clone(),
                                                    "Result<string,string>".to_string(),
                                                );
                                                self.var_types.insert(
                                                    name.clone(),
                                                    Type::Name(
                                                        "Result".into(),
                                                        vec![
                                                            Type::Name("string".into(), vec![]),
                                                            Type::Name("string".into(), vec![]),
                                                        ],
                                                    ),
                                                );
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        } else if let Expr::Turbofish(_func_name, turbo_type_args, _) = init {
                            if let Some(ta) = turbo_type_args.first() {
                                if let Type::Name(tn, args) = ta {
                                    // Prefer full type name for containers so later
                                    // dispatch (to_json Map, List helpers) can match.
                                    if !args.is_empty()
                                        && matches!(
                                            tn.as_str(),
                                            "List" | "Map" | "Set" | "Option" | "Result"
                                        )
                                    {
                                        if let Some(full) = self.get_full_type_name(ta) {
                                            self.var_type_names.insert(name.clone(), full);
                                        } else {
                                            self.var_type_names.insert(
                                                name.clone(),
                                                crate::core::fmt_type(ta),
                                            );
                                        }
                                    } else {
                                        self.var_type_names.insert(name.clone(), tn.clone());
                                    }
                                }
                            }
                        }
                        // Track list element type for nested List<List<T>> indexing
                        if let Some(decl_ty) = &ty {
                            self.register_list_elem_type(name, decl_ty);
                        }
                        // Track capability variables
                        if let Some(Type::Cap(_)) = &ty {
                            if let Some(&(alloca, _)) = vars.get(name) {
                                self.register_cap(name, alloca);
                            }
                        }
                    }
                    // For tuple patterns, push the tuple type onto tuple_type_stack
                    // so that compile_pattern_bind can load the struct correctly
                    if let Pattern::Tuple(sub_pats) = pat {
                        if !sub_pats.is_empty() {
                            // Try to infer tuple type from declared type or init expression
                            let tuple_ty = if let Some(Type::Tuple(elem_tys)) = &ty {
                                let field_tys: Vec<BasicTypeEnum> = elem_tys
                                    .iter()
                                    .map(|t| {
                                        types::mimi_type_to_llvm(self.context, t).unwrap_or(
                                            BasicTypeEnum::IntType(self.context.i64_type()),
                                        )
                                    })
                                    .collect();
                                self.context.struct_type(&field_tys, false)
                            } else {
                                // Fallback: create a struct with i64 fields
                                let field_tys: Vec<BasicTypeEnum> = sub_pats
                                    .iter()
                                    .map(|_| BasicTypeEnum::IntType(self.context.i64_type()))
                                    .collect();
                                self.context.struct_type(&field_tys, false)
                            };
                            self.tuple_type_stack.push(tuple_ty);
                        }
                    }
                    self.compile_pattern_bind(pat, val, vars)?;
                    // Pop tuple type stack if we pushed it
                    if let Pattern::Tuple(sub_pats) = pat {
                        if !sub_pats.is_empty() {
                            self.tuple_type_stack.pop();
                        }
                    }
                    if let Pattern::Variable(name) = pat {
                        if let Expr::Ident(fn_name) = init {
                            if self.module.get_function(fn_name.as_str()).is_some() {
                                self.fn_ptr_var_names.insert(name.clone());
                            }
                            if self.cap_type_names.contains(fn_name.as_str()) {
                                self.var_type_names.insert(name.clone(), fn_name.clone());
                            }
                        }

                        // v0.28.15: Track heap-owned string variables so their
                        // data is freed at scope exit. String literals live in
                        // LLVM globals and must not be freed; identifiers refer
                        // to variables that already have their own slot, so
                        // copying them here is not a deep copy and must not be
                        // freed again. For concat (`+`) and f-string results,
                        // transfer ownership from the expression's raw pointer
                        // registration into the variable slot.
                        let is_string = self
                            .var_type_names
                            .get(name)
                            .map(|t| t == "string")
                            .unwrap_or(false);
                        if is_string {
                            let claims_expr_result = matches!(
                                init,
                                Expr::Binary(BinOp::Add, _, _) | Expr::Literal(Lit::FString(_))
                            );
                            if claims_expr_result {
                                self.pop_last_heap_ptr();
                                if let Some(&(alloca, BasicTypeEnum::StructType(st))) =
                                    vars.get(name)
                                {
                                    if st.get_field_types().len() == 2
                                        && self
                                            .gep()
                                            .build_struct_gep(
                                                st,
                                                alloca,
                                                0,
                                                &format!("{}_str_data_gep", name),
                                            )
                                            .is_ok()
                                    {
                                        self.register_heap_slot(alloca, st, 0);
                                    }
                                }
                            }
                        }
                    }
                }
                Stmt::Assign { target, value } => {
                    self.compile_assign_stmt(target, value, vars)?;
                }
                Stmt::If { cond, then_, else_ } => {
                    let cond_val = self.compile_expr(cond, vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        return Err(CompileError::TypeMismatch(format!(
                            "if condition must be bool, got {} in function '{}'",
                            cond_val.get_type(),
                            func.name
                        )));
                    };

                    let function = self.current_function().ok_or_else(|| {
                        CompileError::LlvmError("codegen: no current function for if".to_string())
                    })?;
                    let then_bb = self.context.append_basic_block(function, "then");
                    let else_bb = self.context.append_basic_block(function, "else");
                    let merge_bb = self.context.append_basic_block(function, "ifcont");

                    self.build_cond_br(cond_bool, then_bb, else_bb)?;

                    // Then block: coerce the produced value to the function's declared
                    // return layout before branching to the merge block.
                    self.builder.position_at_end(then_bb);
                    let mut then_vars = vars.clone();
                    let then_val = self.compile_block_last_val(then_, &mut then_vars)?;
                    let then_val = self.coerce_variant_value(then_val, ret_type, ret_ty_ast)?;
                    let then_reaches = !self.block_has_terminator();
                    if then_reaches {
                        self.build_br(merge_bb)?;
                    }
                    let then_bb_end = then_reaches
                        .then(|| self.builder.get_insert_block())
                        .flatten();

                    // Else block
                    self.builder.position_at_end(else_bb);
                    let else_val = if let Some(else_block) = else_ {
                        let mut else_vars = vars.clone();
                        let v = self.compile_block_last_val(else_block, &mut else_vars)?;
                        let v = self.coerce_variant_value(v, ret_type, ret_ty_ast)?;
                        let reaches = !self.block_has_terminator();
                        if reaches {
                            self.build_br(merge_bb)?;
                        }
                        (v, reaches)
                    } else {
                        let reaches = !self.block_has_terminator();
                        if reaches {
                            self.build_br(merge_bb)?;
                        }
                        (self.context.i64_type().const_int(0, false).into(), reaches)
                    };
                    let (else_val, else_reaches) = else_val;
                    let else_bb_end = else_reaches
                        .then(|| self.builder.get_insert_block())
                        .flatten();

                    // Continue at merge, produce phi with only blocks that reach merge.
                    self.builder.position_at_end(merge_bb);
                    // Unify integer widths: after A1 restoration, then_val (e.g.
                    // i64 from a literal) and else_val (e.g. i32 from an expression)
                    // may have different widths. Extend the narrower one in its
                    // predecessor block before the terminator.
                    let then_bw = match &then_val {
                        BasicValueEnum::IntValue(iv) => iv.get_type().get_bit_width(),
                        _ => 0,
                    };
                    let else_bw = match &else_val {
                        BasicValueEnum::IntValue(iv) => iv.get_type().get_bit_width(),
                        _ => 0,
                    };
                    let (then_val, else_val) = if then_bw > 0 && else_bw > 0 && then_bw != else_bw {
                        // Extend the NARROWER value to match the WIDER value's width.
                        // Use the wider of the two types
                        let target_ty = if then_bw >= 64 || else_bw >= 64 {
                            self.context.i64_type()
                        } else {
                            self.context.i32_type()
                        };
                        let then_val = if then_bw < else_bw && then_reaches {
                            let then_end = then_bb_end.ok_or_else(|| {
                                CompileError::LlvmError(
                                    "if-then s_ext: missing then block end".into(),
                                )
                            })?;
                            self.builder.position_at_end(then_end);
                            if let Some(term) = then_end.get_terminator() {
                                self.builder.position_before(&term);
                            }
                            BasicValueEnum::IntValue(
                                self.builder
                                    .build_int_s_extend(
                                        then_val.into_int_value(),
                                        target_ty,
                                        "func_if_then_sext",
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("s_ext: {}", e))
                                    })?,
                            )
                        } else {
                            then_val
                        };
                        let else_val = if else_bw < then_bw && else_reaches {
                            let else_end = else_bb_end.ok_or_else(|| {
                                CompileError::LlvmError(
                                    "if-else s_ext: missing else block end".into(),
                                )
                            })?;
                            self.builder.position_at_end(else_end);
                            if let Some(term) = else_end.get_terminator() {
                                self.builder.position_before(&term);
                            }
                            BasicValueEnum::IntValue(
                                self.builder
                                    .build_int_s_extend(
                                        else_val.into_int_value(),
                                        target_ty,
                                        "func_if_else_sext",
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("s_ext: {}", e))
                                    })?,
                            )
                        } else {
                            else_val
                        };
                        self.builder.position_at_end(merge_bb);
                        (then_val, else_val)
                    } else {
                        (then_val, else_val)
                    };
                    if then_val.get_type() == else_val.get_type() {
                        let phi = self
                            .builder
                            .build_phi(then_val.get_type(), "if_result")
                            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
                        let mut phi_incoming: Vec<(
                            &dyn inkwell::values::BasicValue,
                            inkwell::basic_block::BasicBlock,
                        )> = Vec::new();
                        if let Some(bb) = then_bb_end {
                            phi_incoming.push((&then_val as &dyn inkwell::values::BasicValue, bb));
                        }
                        if let Some(bb) = else_bb_end {
                            phi_incoming.push((&else_val as &dyn inkwell::values::BasicValue, bb));
                        }
                        if !phi_incoming.is_empty() {
                            phi.add_incoming(&phi_incoming);
                        }
                        last_val = phi.as_basic_value();
                    }
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
                Stmt::Break(_) => {
                    if let Some(target) = self.loop_break {
                        self.build_br(target)?;
                        // Create unreachable block for subsequent statements
                        let function = self.current_function().ok_or_else(|| {
                            CompileError::LlvmError(
                                "codegen: no current function for break".to_string(),
                            )
                        })?;
                        let unreachable = self.context.append_basic_block(function, "unreachable");
                        self.builder.position_at_end(unreachable);
                    } else {
                        return Err(CompileError::BreakOutsideLoop);
                    }
                }
                Stmt::Continue => {
                    if let Some(target) = self.loop_continue {
                        self.build_br(target)?;
                        let function = self.current_function().ok_or_else(|| {
                            CompileError::LlvmError(
                                "codegen: no current function for continue".to_string(),
                            )
                        })?;
                        let unreachable = self.context.append_basic_block(function, "unreachable");
                        self.builder.position_at_end(unreachable);
                    } else {
                        return Err(CompileError::ContinueOutsideLoop);
                    }
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
                    // Drop: evaluate expression and mark capability as consumed
                    let _val = self.compile_expr(expr, vars)?;
                    // If the expression is a variable, mark it as consumed and call mimi_cap_consume
                    if let Expr::Ident(name) = expr {
                        self.consume_cap(name)?;
                        // Generate runtime cap consume call
                        if self.is_cap_var(name) {
                            if let Some(consume_fn) = self.module.get_function("mimi_cap_consume") {
                                if let Some(&(alloca, _)) = vars.get(name) {
                                    let cap_val = self.build_load(
                                        BasicTypeEnum::IntType(self.context.i64_type()),
                                        alloca,
                                        &format!("cap_val_{}", name),
                                    )?;
                                    let name_global = self
                                        .builder
                                        .build_global_string_ptr(
                                            &format!("{}\0", name),
                                            &format!("cap_name_drop_{}", name),
                                        )
                                        .map_err(|e| {
                                            CompileError::LlvmError(format!(
                                                "string global error: {}",
                                                e
                                            ))
                                        })?;
                                    let name_ptr = name_global.as_pointer_value();
                                    self.build_call(
                                        consume_fn,
                                        &[
                                            BasicMetadataValueEnum::IntValue(
                                                cap_val.into_int_value(),
                                            ),
                                            BasicMetadataValueEnum::PointerValue(name_ptr),
                                        ],
                                        &format!("cap_consume_{}", name),
                                    )?;
                                }
                            }
                        }
                    }
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
                    // Alloc: execute body sequentially (simplified - no custom allocator in codegen)
                    self.compile_block(body, vars)?;
                }
                Stmt::Func(f) => {
                    if f.is_comptime {
                        // Comptime functions: skip codegen (interpreter-only)
                    } else {
                        // Register nested function so user_func_signature_matches can find it
                        self.func_defs
                            .entry(f.name.clone())
                            .or_insert_with(|| f.clone());
                        // Save per-function state before compiling nested function, since
                        // compile_func clears these at entry.
                        let saved_block = self.builder.get_insert_block();
                        let saved_type_map = self.type_map.clone();
                        let saved_var_types = std::mem::take(&mut self.var_types);
                        let saved_var_type_names = std::mem::take(&mut self.var_type_names);
                        let saved_list_elem = std::mem::take(&mut self.list_elem_llvm_types);
                        self.compile_func(f)?;
                        self.var_types = saved_var_types;
                        self.var_type_names = saved_var_type_names;
                        self.list_elem_llvm_types = saved_list_elem;
                        self.type_map = saved_type_map;
                        if let Some(bb) = saved_block {
                            self.builder.position_at_end(bb);
                        }
                    }
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
                Stmt::Do(body) => {
                    self.compile_block(body, vars)?;
                }
                Stmt::Delegate { kind, expr, target } => {
                    // v0.29.15: delegate with write-back semantics.
                    // - view: evaluate expr, compile target lookup.
                    // - mutate/consume: evaluate expr, call target, store result back.
                    let val = self.compile_expr(expr, vars)?;
                    if !vars.contains_key(target) {
                        return Err(CompileError::Generic(format!(
                            "delegate target '{}' not found in scope",
                            target
                        )));
                    }
                    match kind {
                        DelegateKind::View => {
                            let _ = val; // side-effect discards value (no write-back)
                        }
                        DelegateKind::Mutate | DelegateKind::Consume => {
                            // Write-back: if expr is Field(obj, field_name), store
                            // result back into obj.field_name.
                            self.compile_delegate_writeback(expr, val, vars)?;
                        }
                    }
                }
                Stmt::Pinned {
                    expr,
                    timeout,
                    var,
                    body,
                } => {
                    // v0.29.32: cooperative wall-clock timeout watchdog.
                    let _pinned_to_i64: Option<inkwell::values::IntValue> = if let Some(to_expr) =
                        timeout
                    {
                        let to_val = self.compile_expr(to_expr, vars)?;
                        let to_iv = match to_val {
                            inkwell::values::BasicValueEnum::IntValue(iv) => iv,
                            other => {
                                return Err(CompileError::TypeMismatch(format!(
                                    "pinned timeout must be integer, got {:?}",
                                    other
                                )));
                            }
                        };
                        let i64_ty = self.context.i64_type();
                        let to_i64 = if to_iv.get_type().get_bit_width() < 64 {
                            self.builder
                                .build_int_s_extend(to_iv, i64_ty, "to_i64")
                                .map_err(|e| CompileError::LlvmError(format!("sext: {}", e)))?
                        } else {
                            to_iv
                        };
                        let zero = i64_ty.const_int(0, false);
                        let expired = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::SLE,
                                to_i64,
                                zero,
                                "pin_expired",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("cmp: {}", e)))?;
                        let function = self
                            .builder
                            .get_insert_block()
                            .ok_or_else(|| CompileError::LlvmError("pinned: no block".into()))?
                            .get_parent()
                            .ok_or_else(|| CompileError::LlvmError("pinned: no fn".into()))?;
                        let fail_bb = self.context.append_basic_block(function, "pin_timeout");
                        let ok_bb = self.context.append_basic_block(function, "pin_ok");
                        self.builder
                            .build_conditional_branch(expired, fail_bb, ok_bb)
                            .map_err(|e| CompileError::LlvmError(format!("cbr: {}", e)))?;
                        self.builder.position_at_end(fail_bb);
                        // v0.29.43: delayed Fault — set pending flag BEFORE abort.
                        let state_str = self
                            .builder
                            .build_global_string_ptr("FFI_Pinned", "pin_state")
                            .map_err(|e| CompileError::LlvmError(format!("gstr: {}", e)))?;
                        let fault_fn = self.get_or_declare_pinned_fault_fn();
                        self.builder
                            .build_call(
                                fault_fn,
                                &[inkwell::values::BasicMetadataValueEnum::PointerValue(
                                    state_str.as_pointer_value(),
                                )],
                                "pin_fault",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("pinned_fault: {}", e)))?;
                        let msg = self
                            .builder
                            .build_global_string_ptr(
                                "pinned timeout expired: FFI anchor watchdog",
                                "pin_to_msg",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("gstr: {}", e)))?;
                        let abort_fn = self.get_or_declare_abort_fn();
                        self.builder
                            .build_call(
                                abort_fn,
                                &[inkwell::values::BasicMetadataValueEnum::PointerValue(
                                    msg.as_pointer_value(),
                                )],
                                "pin_abort",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("abort: {}", e)))?;
                        self.builder
                            .build_unreachable()
                            .map_err(|e| CompileError::LlvmError(format!("unreach: {}", e)))?;
                        self.builder.position_at_end(ok_bb);
                        Some(to_i64)
                    } else {
                        None
                    };
                    // v0.29.32: record wall-clock start before body.
                    let _pinned_start = if _pinned_to_i64.is_some() {
                        let wc_fn = self.get_or_declare_wall_clock_fn();
                        let start = self
                            .builder
                            .build_call(wc_fn, &[], "pin_start_ms")
                            .map_err(|e| CompileError::LlvmError(format!("wc: {}", e)))?;
                        Some(
                            start
                                .try_as_basic_value_opt()
                                .ok_or_else(|| {
                                    CompileError::LlvmError(
                                        "mimi_wall_clock_ms returned void (L6)".into(),
                                    )
                                })?
                                .into_int_value(),
                        )
                    } else {
                        None
                    };
                    let val = self.compile_expr(expr, vars)?;
                    if let Some(v) = var {
                        let ty = val.get_type();
                        let alloca = self.build_alloca(ty, v)?;
                        self.build_store(alloca, val)?;
                        vars.insert(v.clone(), (alloca, ty));
                    }
                    self.compile_block(body, vars)?;
                    // v0.29.32: cooperative wall-clock expiry check after body.
                    if let (Some(to_i64), Some(start_ms)) = (_pinned_to_i64, _pinned_start) {
                        let wc_fn = self.get_or_declare_wall_clock_fn();
                        let now_call = self
                            .builder
                            .build_call(wc_fn, &[], "pin_now_ms")
                            .map_err(|e| CompileError::LlvmError(format!("wc: {}", e)))?;
                        let now_ms = now_call
                            .try_as_basic_value_opt()
                            .ok_or_else(|| {
                                CompileError::LlvmError(
                                    "mimi_wall_clock_ms returned void (L6)".into(),
                                )
                            })?
                            .into_int_value();
                        let elapsed = self
                            .builder
                            .build_int_sub(now_ms, start_ms, "pin_elapsed")
                            .map_err(|e| CompileError::LlvmError(format!("sub: {}", e)))?;
                        let exceeded = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::SGT,
                                elapsed,
                                to_i64,
                                "pin_exceeded",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("cmp: {}", e)))?;
                        let function = self
                            .builder
                            .get_insert_block()
                            .ok_or_else(|| CompileError::LlvmError("pinned: no block".into()))?
                            .get_parent()
                            .ok_or_else(|| CompileError::LlvmError("pinned: no fn".into()))?;
                        let exp_bb = self.context.append_basic_block(function, "pin_exp_abort");
                        let cont_bb = self.context.append_basic_block(function, "pin_cont");
                        self.builder
                            .build_conditional_branch(exceeded, exp_bb, cont_bb)
                            .map_err(|e| CompileError::LlvmError(format!("cbr: {}", e)))?;
                        self.builder.position_at_end(exp_bb);
                        // v0.29.43: delayed Fault — set pending flag BEFORE abort.
                        let state_str = self
                            .builder
                            .build_global_string_ptr("FFI_Pinned", "pin_exp_state")
                            .map_err(|e| CompileError::LlvmError(format!("gstr: {}", e)))?;
                        let fault_fn = self.get_or_declare_pinned_fault_fn();
                        self.builder
                            .build_call(
                                fault_fn,
                                &[inkwell::values::BasicMetadataValueEnum::PointerValue(
                                    state_str.as_pointer_value(),
                                )],
                                "pin_exp_fault",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("pinned_fault: {}", e)))?;
                        let msg = self
                            .builder
                            .build_global_string_ptr(
                                "pinned timeout expired: FFI anchor watchdog (cooperative)",
                                "pin_exp_msg",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("gstr: {}", e)))?;
                        let abort_fn = self.get_or_declare_abort_fn();
                        self.builder
                            .build_call(
                                abort_fn,
                                &[inkwell::values::BasicMetadataValueEnum::PointerValue(
                                    msg.as_pointer_value(),
                                )],
                                "pin_exp_abort",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("abort: {}", e)))?;
                        self.builder
                            .build_unreachable()
                            .map_err(|e| CompileError::LlvmError(format!("unreach: {}", e)))?;
                        self.builder.position_at_end(cont_bb);
                    }
                }
                _ => {}
            }
        }
        Ok(ControlFlow::Continue(last_val))
    }

    /// Emit the implicit return at the end of a function: check for unconsumed
    /// capabilities, convert pointer-to-struct returns, clean up scopes, verify
    /// postconditions, and build the final return instruction.
    fn emit_implicit_return(
        &mut self,
        ret_type: BasicTypeEnum<'ctx>,
        ret_ty_ast: Option<&Type>,
        last_val: BasicValueEnum<'ctx>,
        func_name: &str,
        vars: &HashMap<String, VarEntry<'ctx>>,
        expr: Option<&Expr>,
    ) -> MimiResult<()> {
        // Check for unconsumed capabilities before returning
        self.check_unconsumed_caps()?;

        // Transfer ownership of string return values before the heap cleanup below
        // frees local temporaries.
        let last_val = self.claim_string_return_value(last_val, ret_type, expr, vars)?;

        // Convert pointer-to-struct to struct value when return type expects a struct.
        // Must happen BEFORE free_heap_allocs to null out heap data pointers in the original struct,
        // preventing use-after-free on the returned value's heap-allocated data.
        //
        // Special case: string literal returns a raw i8* (PointerValue), but the Mimi string
        // type is {i8*, i64}. We need to wrap the raw pointer into a struct via wrap_c_string.
        let last_val = match (last_val, ret_type) {
            (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(st)) => {
                let field_types = st.get_field_types();
                // Check if this is the Mimi string struct {ptr, i64} — the pointer is
                // a raw C string (from literal), not a pointer to an alloca'd struct.
                let is_string_struct = field_types.len() == 2
                    && matches!(&field_types[0], BasicTypeEnum::PointerType(_))
                    && matches!(&field_types[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64);
                if is_string_struct {
                    self.wrap_c_string(pv)?
                } else {
                    let loaded = self.build_load(BasicTypeEnum::StructType(st), pv, "ret_load")?;
                    // Null out pointer-typed fields to prevent free_heap_allocs from freeing
                    // the heap data that's now owned by the caller via the returned struct value.
                    // Only pointer-typed fields can contain heap data; integer fields
                    // (discriminators, lengths, payloads) are left untouched to avoid
                    // ptr→i64 type mismatches in LLVM's backend (physreg COPY error).
                    let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
                    for (fi, ft) in field_types.iter().enumerate() {
                        if matches!(ft, BasicTypeEnum::PointerType(_)) {
                            if let Ok(fp) =
                                self.gep()
                                    .build_struct_gep(st, pv, fi as u32, "ret_data_null")
                            {
                                let _ = self.builder.build_store(fp, null_ptr);
                            }
                        }
                    }
                    loaded
                }
            }
            _ => last_val,
        };
        let last_val = self.coerce_variant_value(last_val, ret_type, ret_ty_ast)?;

        // Pop scopes (discard compensations on normal exit)
        // A function owns exactly one shared-release frame. Popping only that
        // frame preserves the caller's registrations when codegen recursively
        // monomorphizes a callee while the caller is still being emitted.
        self.pop_shared_scope()?;
        self.free_heap_allocs()?;
        self.pop_comp_scope();
        self.pop_cap_scope();

        if !self.block_has_terminator() {
            let ensures = self.ensures_stmts.clone();
            if !ensures.is_empty() {
                let result_alloca = self.build_alloca(ret_type, "result")?;
                let adjusted = self.adjust_int_val(last_val, ret_type)?;
                self.build_store(result_alloca, adjusted)?;
                let mut ensures_vars = vars.clone();
                ensures_vars.insert("result".to_string(), (result_alloca, ret_type));
                for ensures_expr in &ensures {
                    self.compile_contract_assert(
                        ensures_expr,
                        &ensures_vars,
                        &format!("ensures violation in '{}'", func_name),
                    )?;
                }
            }
        }
        let last_val = self.adjust_int_val(last_val, ret_type)?;
        let last_val = self.load_return_value_if_needed(last_val)?;
        self.build_return(Some(&last_val))?;
        Ok(())
    }

    /// Forward-declare a non-extern, non-async user function in the LLVM module.
    /// This allows functions defined later in the source (or in imported modules)
    /// to be referenced by earlier callers without a "undefined function" error.
    pub(super) fn declare_func(
        &mut self,
        func: &FuncDef,
    ) -> MimiResult<(inkwell::values::FunctionValue<'ctx>, BasicTypeEnum<'ctx>)> {
        // For impl Trait return types, determine the concrete type from the body
        // so the function's LLVM signature uses the right type.
        let effective_ret_override = if let Some(Type::ImplTrait(_)) = &func.ret {
            Self::concrete_return_type_for_impl_trait(&func.body)
                .and_then(|tn| self.type_llvm.get(&tn).cloned())
        } else {
            None
        };

        let ret_type = effective_ret_override
            .or_else(|| match &func.ret {
                Some(ty) => self.llvm_type_for(ty),
                None => None,
            })
            .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));

        let mut param_types = Vec::new();
        for param in &func.params {
            if let Some(ty) = self.llvm_type_for(&param.ty) {
                if param.borrow.is_some() {
                    param_types.push(BasicTypeEnum::PointerType(
                        self.context.ptr_type(AddressSpace::default()),
                    ));
                } else {
                    param_types.push(ty);
                }
            }
        }

        let metadata_params: Vec<_> = param_types
            .iter()
            .map(|t| types::basic_to_metadata(self.context, *t))
            .collect();

        let fn_type = match ret_type {
            BasicTypeEnum::IntType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&metadata_params, false),
            _ => self.context.i64_type().fn_type(&metadata_params, false),
        };

        // Reuse an existing declaration if it already exists. `Module::add_function`
        // panics if a function with this name exists with a mismatching type.
        let function = if let Some(existing) = self.module.get_function(&func.name) {
            existing
        } else {
            self.module.add_function(&func.name, fn_type, None)
        };
        Ok((function, ret_type))
    }

    pub(super) fn compile_func(&mut self, func: &FuncDef) -> MimiResult<()> {
        // Per-function variable type tracking must start fresh so that parameters
        // with common names (e.g. `xs`) don't inherit types from other functions.
        // Also clear the generic substitution map: non-generic functions must not
        // carry over type substitutions from previously compiled generic functions.
        self.var_types.clear();
        self.var_type_names.clear();
        self.list_elem_llvm_types.clear();
        self.type_map.clear();

        // Delegate async funcs to compile_async_func
        if func.is_async {
            return self.compile_async_func(func);
        }

        // Exported extern functions get a C ABI wrapper around an internal body.
        if func.extern_abi.is_some() && func.generics.is_empty() {
            let body_name = format!("{}__mimi_export_body", func.name);
            if self.module.get_function(&body_name).is_none() {
                let mut body_func = func.clone();
                body_func.name = body_name.clone();
                body_func.extern_abi = None;
                self.compile_func(&body_func)?;
            }
            return self.compile_export_wrapper(func, &body_name);
        }

        let (function, ret_type) = self.declare_func(func)?;
        // Set calling convention for extern "C" / extern "stdcall" etc.
        if let Some(ref abi) = func.extern_abi {
            let cc = crate::ffi::abi_to_llvm_call_conv(abi);
            function.set_call_conventions(cc);
        }
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        // v0.29.24: apply @max_children(N) process quota when compiling main.
        if func.name == "main" {
            if let Some(max) = self.max_children {
                if let Ok(set_fn) = self.get_runtime_fn("mimi_actor_set_max_children") {
                    let n = self.context.i64_type().const_int(max as u64, false);
                    let _ = self.build_call(set_fn, &[n.into()], "set_max_children");
                }
            }
        }

        // Push scopes for function body
        self.push_cap_scope();
        self.push_comp_scope();
        self.push_heap_scope();
        self.push_shared_scope();

        let mut vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
        self.bind_func_params(func, function, &mut vars)?;

        // Prepare and compile function contracts.
        self.prepare_func_contracts(func, &vars)?;
        self.snapshot_old_values(&vars)?;

        let ret_ty_ast = func.ret.as_ref();
        let last_expr = func.body.last().and_then(|s| match s {
            Stmt::Expr(e) => Some(e),
            _ => None,
        });
        match self.compile_func_body(func, ret_type, &mut vars)? {
            ControlFlow::Break(()) => return Ok(()),
            ControlFlow::Continue(last_val) => {
                self.emit_implicit_return(
                    ret_type, ret_ty_ast, last_val, &func.name, &vars, last_expr,
                )?;
            }
        }

        Ok(())
    }

    /// Compile a generic function with concrete type arguments (monomorphization)
    pub(super) fn compile_generic_func(
        &mut self,
        func: &FuncDef,
        type_map: &HashMap<String, crate::ast::Type>,
    ) -> MimiResult<()> {
        // Per-function variable type tracking must start fresh.
        self.var_types.clear();
        self.var_type_names.clear();
        self.list_elem_llvm_types.clear();

        // Save and set the type_map
        let prev_type_map = self.type_map.clone();
        self.type_map = type_map.clone();

        // The caller may be in the middle of building another function (e.g.
        // `sum` monomorphizing `reduce_list`). Save the insertion point and
        // restore it before returning so the caller's codegen continues in the
        // right basic block.
        let saved_block = self.builder.get_insert_block();

        let mangled = Self::mangle_name(&func.name, type_map);

        // Skip if already compiled
        if self.module.get_function(&mangled).is_some() {
            self.type_map = prev_type_map;
            return Ok(());
        }

        // Delegate async generic funcs to compile_async_func
        if func.is_async {
            let result = self.compile_async_func(func);
            self.type_map = prev_type_map;
            if let Some(bb) = saved_block {
                self.builder.position_at_end(bb);
            }
            return result;
        }

        // For impl Trait return types, determine the concrete type from the body
        let effective_ret_override = if let Some(Type::ImplTrait(_)) = &func.ret {
            Self::concrete_return_type_for_impl_trait(&func.body)
                .and_then(|tn| self.type_llvm.get(&tn).cloned())
        } else {
            None
        };

        // Substitute generic params in ret type and param types
        let ret_type = effective_ret_override
            .or_else(|| match &func.ret {
                Some(ty) => {
                    let resolved = self.resolve_type(ty);
                    self.llvm_type_for(&resolved)
                }
                None => None,
            })
            .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));

        let mut param_types = Vec::new();
        for param in &func.params {
            let resolved = self.resolve_type(&param.ty);
            if let Some(ty) = self.llvm_type_for(&resolved) {
                param_types.push(ty);
            }
        }

        let metadata_params: Vec<_> = param_types
            .iter()
            .map(|t| types::basic_to_metadata(self.context, *t))
            .collect();

        let fn_type = match ret_type {
            BasicTypeEnum::IntType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&metadata_params, false),
            _ => self.context.i64_type().fn_type(&metadata_params, false),
        };

        let function = self.module.add_function(&mangled, fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        self.push_cap_scope();
        self.push_comp_scope();
        self.push_heap_scope();
        self.push_shared_scope();

        let mut vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
        self.bind_func_params(func, function, &mut vars)?;

        // Prepare and compile function contracts.
        self.prepare_func_contracts(func, &vars)?;
        self.snapshot_old_values(&vars)?;

        let ret_ty_ast = func.ret.as_ref();
        let last_expr = func.body.last().and_then(|s| match s {
            Stmt::Expr(e) => Some(e),
            _ => None,
        });
        let last_val = self.compile_block_last_val(&func.body, &mut vars)?;

        self.emit_implicit_return(ret_type, ret_ty_ast, last_val, &func.name, &vars, last_expr)?;
        self.type_map = prev_type_map;
        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }
        Ok(())
    }
}
