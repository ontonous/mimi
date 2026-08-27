#![allow(clippy::unwrap_used)]
use crate::ast::*;
use crate::codegen::block::register_qualified_var_type;
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
        match s.unlocated() {
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
            Stmt::Expr(expr) | Stmt::Return(Some(expr)) => match expr.unlocated() {
                Expr::Lambda { body, .. } | Expr::Block(body) => {
                    result.extend(collect_ensures(body));
                }
                _ => {}
            },
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
    match expr.unlocated() {
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
        Expr::Record {
            ty: _,
            fields,
            rest,
        } => {
            for f in fields {
                collect_old_idents_walker(&f.value, out);
            }
            if let Some(rest) = rest {
                collect_old_idents_walker(rest, out);
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
        Expr::Located { .. } => unreachable!("Expr::unlocated returned Located"),
    }
}

/// Collect identifier names from within an `old(...)` expression. The root
/// identifier is the variable being snapshotted. For `old(x.foo)`, we
/// snapshot `x`. For `old(old(x))`, we recurse and snapshot `x`.
fn collect_idents_in_old(expr: &crate::ast::Expr, out: &mut Vec<String>) {
    use crate::ast::Expr;
    match expr.unlocated() {
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
        Expr::Record {
            ty: _,
            fields,
            rest,
        } => {
            for f in fields {
                collect_idents_in_old(&f.value, out);
            }
            if let Some(rest) = rest {
                collect_idents_in_old(rest, out);
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
    match expr.unlocated() {
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
        Expr::SliceExpr { target, start, end } => {
            collect_all_idents_depth(target, out, d);
            if let Some(s) = start {
                collect_all_idents_depth(s, out, d);
            }
            if let Some(e) = end {
                collect_all_idents_depth(e, out, d);
            }
        }
        Expr::Record {
            ty: _,
            fields,
            rest,
        } => {
            for f in fields {
                collect_all_idents_depth(&f.value, out, d);
            }
            if let Some(rest) = rest {
                collect_all_idents_depth(rest, out, d);
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
        Expr::Located { .. } => unreachable!("Expr::unlocated returned Located"),
    }
}

/// Walk statements for old() collection — covers Let init, Expr, Return,
/// If/While/For/Match bodies, etc.
fn collect_old_idents_in_stmt(stmt: &crate::ast::Stmt, out: &mut Vec<String>) {
    use crate::ast::Stmt;
    match stmt.unlocated() {
        Stmt::Expr(e) => collect_old_idents_walker(e, out),
        Stmt::Let { init: Some(e), .. } => collect_old_idents_walker(e, out),
        Stmt::Return(Some(e)) => collect_old_idents_walker(e, out),
        _ => {}
    }
}

fn collect_all_idents_in_stmt_depth(stmt: &crate::ast::Stmt, out: &mut Vec<String>, depth: u32) {
    use crate::ast::Stmt;
    match stmt.unlocated() {
        Stmt::Expr(e) => collect_all_idents_depth(e, out, depth),
        Stmt::Let { init: Some(e), .. } => collect_all_idents_depth(e, out, depth),
        Stmt::Return(Some(e)) => collect_all_idents_depth(e, out, depth),
        _ => {}
    }
}

// Submodules for clearly independent method groups. The originally suggested
// groups (params, actor, shared) do not map to standalone methods in this file:
//
// - Parameter handling and ABI layout are inlined in `compile_func_legacy` / `compile_generic_func`;
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
    /// 0.36.7 (裁决 1 跨 flow 补全, legacy leg): the var-type name registered
    /// for a flow transition call result. For the Fault sink the FLOW-QUALIFIED
    /// `flow::<name>::Fault` must be used — the legacy emitter registers every
    /// flow state (incl. Fault) as `flow::<name>::<state>` record TypeDefs, so
    /// `infer_object_type(Field(sf, last_state))` then resolves THIS flow's
    /// StateId/EventId field types instead of the bare-name first-wins alias
    /// (which can point at another flow's enums → wrong enum in native prints).
    fn transition_result_var_type(flow_name: &str, to_state: &str) -> String {
        if to_state == "Fault" {
            format!("flow::{}::Fault", flow_name)
        } else {
            to_state.to_string()
        }
    }

    /// Strip the generator-made `flow::<name>::` prefix from a from-state name
    /// for transition-overload matching (the transition directory keys and
    /// `TransitionDef.from_state` are bare). `::` cannot appear in user
    /// identifiers, so any `flow::`-prefixed name is surface-made.
    fn bare_flow_state_name(name: &str) -> String {
        if name.starts_with("flow::") {
            name.rsplit("::").next().unwrap_or(name).to_string()
        } else {
            name.to_string()
        }
    }

    pub(super) fn compile_async_func(&mut self, func: &FuncDef) -> MimiResult<()> {
        // 1. Compile the actual body as a hidden regular function
        let body_name = format!("{}__async_body", func.name);
        let body_func = FuncDef {
            meta: AstNodeMeta::inherited(
                func.meta.span,
                AstOrigin::RuntimeSystem("codegen.async_body"),
            ),
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
            has_requires: func.has_requires,
            has_ensures: func.has_ensures,
            has_mutate_params: func.has_mutate_params,
        };
        self.compile_func_legacy(&body_func)?;

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
        // 0.34.36 (cross-agent contract, audit wave-1): future layout is
        //   header { completed: AtomicI32 @0, refs: AtomicI32 @4,
        //            data_capacity: u64 @8..16 }
        //   data region @ offset 16..
        // total allocation: 16 header + aligned_result (result) + total_args_size (args).
        // The runtime (src/runtime/future.rs) honors the requested size and
        // records it in data_capacity.
        const FUTURE_HEADER_SIZE: u64 = 16;
        let total_alloc_size = FUTURE_HEADER_SIZE + aligned_result + total_args_size;
        let args_offset: u64 = FUTURE_HEADER_SIZE + aligned_result;

        // i8 pointer type
        let i8_ty = self.context.i8_type();
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();

        // ── Step 2a: Generate poll function ──
        // void @foo_poll(i8* %future_ptr)
        let poll_name = format!("{}__poll", func.name);
        let poll_fn_type = self
            .context
            .void_type()
            .fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
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
        // K-3: param_types skips un-lowerable params (unit/nothing/Infer), so
        // index it with a counter that skips the same params — the source
        // index desyncs after any skipped param.
        let mut llvm_param_idx: usize = 0;
        for param in func.params.iter() {
            if self.llvm_type_for(&param.ty).is_none() {
                continue;
            }
            if llvm_param_idx < param_types.len() {
                let ty = param_types[llvm_param_idx];
                let size = param_sizes[llvm_param_idx];
                // GEP to load arg: future + current_arg_offset
                let arg_ptr_i8 = self
                    .gep()
                    .build_gep(
                        i8_ty,
                        poll_future_ptr,
                        &[i64_ty.const_int(current_arg_offset, false)],
                        &format!("poll_arg_{}", llvm_param_idx),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("poll arg gep: {}", e)))?;
                let arg_typed_ptr = self
                    .builder
                    .build_pointer_cast(
                        arg_ptr_i8,
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        &format!("poll_arg_typed_{}", llvm_param_idx),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("poll arg cast: {}", e)))?;
                let arg_val = self.build_load(
                    ty,
                    arg_typed_ptr,
                    &format!("poll_arg_val_{}", llvm_param_idx),
                )?;
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
                llvm_param_idx += 1;
            }
        }

        let poll_body_result = self
            .build_call(body_fn, &poll_call_args, "poll_body_call")?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("poll body returned void".into()))?;

        // Store result at future + FUTURE_HEADER_SIZE (data region start, offset 16)
        if !func.ret.as_ref().map_or(
            true,
            |t| matches!(t.unlocated(), Type::Name(n, _) if n == "unit"),
        ) {
            let result_ptr_i8 = self
                .gep()
                .build_gep(
                    i8_ty,
                    poll_future_ptr,
                    &[i64_ty.const_int(FUTURE_HEADER_SIZE, false)],
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
        // K-3: skip-aware LLVM param index (see bind_func_params).
        let mut llvm_param_idx: usize = 0;
        for param in func.params.iter() {
            if self.llvm_type_for(&param.ty).is_none() {
                continue;
            }
            if llvm_param_idx < param_types.len() {
                let ty = param_types[llvm_param_idx];
                let alloca = self.build_alloca(ty, &param.name)?;
                let param_val = function
                    .get_nth_param(llvm_param_idx as u32)
                    .ok_or_else(|| {
                        CompileError::LlvmError(format!("param {} not found", llvm_param_idx))
                    })?;
                self.build_store(alloca, param_val)?;
                vars.insert(param.name.clone(), (alloca, ty));
                if let Type::Name(tn, args) = param.ty.unlocated() {
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
                llvm_param_idx += 1;
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
        // K-3: skip-aware LLVM param index (see bind_func_params).
        let mut current_arg_store_offset = args_offset;
        let mut llvm_param_idx: usize = 0;
        for param in func.params.iter() {
            if self.llvm_type_for(&param.ty).is_none() {
                continue;
            }
            if llvm_param_idx < param_types.len() {
                let ty = param_types[llvm_param_idx];
                let size = param_sizes[llvm_param_idx];
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
                        &format!("arg_slot_{}", llvm_param_idx),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("arg slot gep: {}", e)))?;
                let arg_slot_typed = self
                    .builder
                    .build_pointer_cast(
                        arg_slot_i8,
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        &format!("arg_slot_typed_{}", llvm_param_idx),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("arg slot cast: {}", e)))?;
                self.build_store(arg_slot_typed, val)?;
                current_arg_store_offset += size;
                llvm_param_idx += 1;
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
        match last.unlocated() {
            Stmt::Expr(expr) | Stmt::Return(Some(expr)) => match expr.unlocated() {
                Expr::Record { ty, .. } => ty.clone(),
                Expr::Call(callee, _) => {
                    if let Expr::Ident(_fname) = callee.unlocated() {
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
                if let Stmt::Requires(expr, _) = stmt.unlocated() {
                    self.compile_contract_assert(
                        expr,
                        vars,
                        super::scope::ContractPhase::Requires,
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
        match expr.unlocated() {
            Expr::Binary(BinOp::Add, _, _) => true,
            Expr::Literal(Lit::FString(_)) => true,
            Expr::Call(callee, _) => {
                matches!(val, BasicValueEnum::PointerValue(_))
                    || matches!(
                        callee.unlocated(),
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

    /// MEM-C13: if returning a closure `{fn_ptr, env_ptr}`, claim the env heap
    /// pointer so `free_heap_allocs` does not free an env the caller still owns.
    ///
    /// B9 (audit): the claim is now value-exact. The returned env pointer is
    /// recorded in `claimed_returned_envs`, and the immediately following
    /// `free_heap_allocs` emits runtime guards (`ptr != claimed_env` → free).
    /// The old positional `pop_last_heap_ptr` misfired whenever an unrelated
    /// allocation was registered after the env (it popped the wrong entry,
    /// leaking the env or double-freeing data the caller received).
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
        let sv = match val {
            BasicValueEnum::StructValue(sv) => sv,
            _ => return Ok(val),
        };
        // The env is the closure struct's second field. For non-capturing
        // closures it is null — claiming null is harmless (guards only match
        // null envs, and free(null) is a C no-op).
        if let BasicValueEnum::PointerValue(env_ptr) =
            self.build_extract_value(sv.into(), 1, "b9_env_claim")?
        {
            self.claim_closure_env(env_ptr);
        }
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
        let mut val = self.claim_returned_closure_env(val, ret_type)?;
        // 0.35.23 deep-eval (mimi-make native UAF): aggregate returns that
        // CARRY string fields — `(string, string)` tuples and records —
        // leave the element heap buffers registered in this function's heap
        // scope; the scope-exit flush freed them while the returned
        // aggregate still referenced them (parse_variable's pair was later
        // fed to mimi_str_clone: src == freed-and-reused dst →
        // copy_nonoverlapping overlap abort). Claim each string-shaped
        // field's data pointer so the guarded flush skips it — ownership
        // transfers to the caller (same contract as closure envs, B9).
        // Record/tuple values may arrive as an alloca POINTER (the implicit
        // return path loads them only after this claim runs). Pre-check the
        // field shapes FIRST so no IR is emitted unless a string field can
        // actually be claimed (golden-IR stability for every other shape).
        fn agg_claim_shape(st: inkwell::types::StructType) -> bool {
            let fields = st.get_field_types();
            let is_plain_string = fields.len() == 2
                && matches!(fields[0], BasicTypeEnum::PointerType(_))
                && matches!(fields[1], BasicTypeEnum::IntType(_));
            fields.len() >= 2
                && !is_plain_string
                && fields.iter().any(|f| match f {
                    BasicTypeEnum::StructType(inner) => {
                        let fs = inner.get_field_types();
                        fs.len() == 2
                            && matches!(fs[0], BasicTypeEnum::PointerType(_))
                            && matches!(fs[1], BasicTypeEnum::IntType(_))
                    }
                    _ => false,
                })
        }
        let agg_claim_val: Option<(
            inkwell::types::StructType<'ctx>,
            inkwell::values::StructValue<'ctx>,
            Option<inkwell::values::PointerValue<'ctx>>,
        )> = match (ret_type, val) {
            (BasicTypeEnum::StructType(st), BasicValueEnum::StructValue(sv))
                if agg_claim_shape(st) =>
            {
                Some((st, sv, None))
            }
            (BasicTypeEnum::StructType(st), BasicValueEnum::PointerValue(pv))
                if agg_claim_shape(st) =>
            {
                match self.build_load(BasicTypeEnum::StructType(st), pv, "agg_claim_ld") {
                    Ok(BasicValueEnum::StructValue(sv)) => Some((st, sv, Some(pv))),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some((st, sv, pv)) = agg_claim_val {
            let fields = st.get_field_types();
            let is_plain_string = fields.len() == 2
                && matches!(fields[0], BasicTypeEnum::PointerType(_))
                && matches!(fields[1], BasicTypeEnum::IntType(_));
            if fields.len() >= 2 && !is_plain_string {
                // 0.39.x (L1 parity fix): string-shaped fields used to be
                // CLAIMED only (ownership transfer). That is unsound when the
                // leaf merely ALIASES callee input — e.g. a `.rodata` literal
                // argument packed into a returned tuple/Result by a
                // monomorphized generic instance (`func split<T>(v: T) ->
                // (T, i32)` called with a literal): the caller-side tracking
                // then frees a global (free() abort). Normalize every
                // string-shaped leaf through the same runtime probe used for
                // top-level string returns: heap-owned values transfer
                // untouched, borrowed values are replaced by fresh
                // heap copies the caller legitimately owns.
                let mut rebuilt = sv;
                for (idx, fty) in fields.iter().enumerate() {
                    if Self::is_string_llvm_type(*fty) {
                        if let Ok(BasicValueEnum::StructValue(fsv)) =
                            self.build_extract_value(sv.into(), idx as u32, "agg_str_field")
                        {
                            let normalized = self.claim_resolved_string_return(fsv.into())?;
                            if let BasicValueEnum::StructValue(nsv) = normalized {
                                rebuilt = self
                                    .builder
                                    .build_insert_value(rebuilt, nsv, idx as u32, "agg_str_owned")
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("agg str own rebuild: {e}"))
                                    })?
                                    .into_struct_value();
                            }
                            // Claim the original data pointer regardless: for
                            // heap-owned leaves this pops the callee-scope
                            // registration (transfer, as before); for copied
                            // leaves the borrowed source has no registration
                            // and the claim is a no-op.
                            if let Ok(BasicValueEnum::PointerValue(data)) =
                                self.build_extract_value(fsv.into(), 0, "agg_str_data")
                            {
                                self.claim_closure_env(data);
                            }
                        }
                    }
                }
                match pv {
                    // Implicit-return path loads the aggregate from the
                    // alloca AFTER this claim runs — write the normalized
                    // value back so the copies are visible.
                    Some(pv) => {
                        self.build_store(pv.into(), BasicValueEnum::StructValue(rebuilt))?;
                    }
                    None => {
                        val = BasicValueEnum::StructValue(rebuilt);
                    }
                }
            }
        }
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
            // Also handle Result<T, string> (string in Err payload, widened
            // to i64 in the Result struct).  The LLVM type of field 2 is i64
            // (not a string struct), so we detect this by checking whether the
            // return expression is `Err(str_expr)` where str_expr produces a
            // heap-tracked string.
            let is_err_with_string_arg = match expr {
                Some(crate::ast::Expr::Call(callee, args)) => {
                    matches!(callee.unlocated(), crate::ast::Expr::Ident(n) if n == "Err")
                        && args.len() == 1
                }
                _ => false,
            };
            if is_variant_with_string_payload || is_err_with_string_arg {
                // Pop the inner string's heap allocation so free_heap_allocs
                // does not free the returned string before the caller can use it.
                if let Some(expr) = expr {
                    if let Expr::Call(_, args) = expr.unlocated() {
                        if args.len() == 1 && Self::is_string_temp_expr(&args[0], &val) {
                            let _ = self.pop_last_heap_ptr();
                        }
                    }
                }
            }
            return Ok(val);
        }

        // Returning a string variable: load the struct value and null out the
        // variable slot's data pointer so the slot is not freed before return.
        if let Some(expr) = expr {
            if let Expr::Ident(name) = expr.unlocated() {
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
                                self.build_store(data_gep, null_ptr)?;
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
        }

        // For concat / fstring / builtin raw returns, the most-recent heap
        // registration owns the returned data; pop it so free_heap_allocs
        // doesn't release it before the caller sees it.
        let is_string_temp = match expr.map(Expr::unlocated) {
            Some(Expr::Binary(BinOp::Add, _, _)) => true,
            Some(Expr::Literal(Lit::FString(_))) => true,
            Some(Expr::Call(callee, _)) => {
                matches!(val, BasicValueEnum::PointerValue(_))
                    || matches!(
                        callee.unlocated(),
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

    /// Resolved-emitter string-return ownership contract (deep-eval
    /// 2026-08-09; demos/04_adt_match native abort): legacy return funnels
    /// claim string ownership via expression-shape heuristics
    /// (`claim_string_return_value`), but resolved-emitter returns lack that
    /// path — a match/if merge can hand back a `.rodata` literal while the
    /// caller-side `track_string_return_lifetime` unconditionally frees the
    /// returned data pointer (free(global) → munmap_chunk abort).
    ///
    /// Runtime ownership probe: compare the data pointer against null and
    /// every live heap registration of the current function scope
    /// (`heap_probe_candidates`); when nothing matches, heap-copy so the
    /// returned pointer is always malloc-owned. Heap-matching values
    /// transfer ownership WITHOUT a copy (`drain_heap_scope` drops the
    /// registration on the return path), so concat-heavy returns neither
    /// crash nor leak. Null counts as owned: free(null) is a no-op.
    pub(in crate::codegen) fn claim_resolved_string_return(
        &self,
        val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let BasicValueEnum::StructValue(sv) = val else {
            return Ok(val);
        };
        let sty = sv.get_type();
        let fields = sty.get_field_types();
        let is_string_struct = fields.len() == 2
            && matches!(fields[0], BasicTypeEnum::PointerType(_))
            && matches!(fields[1], BasicTypeEnum::IntType(_));
        if !is_string_struct {
            return Ok(val);
        }
        let data_pv = match self.build_extract_value(sv.into(), 0, "res_ret_data")? {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => return Ok(val),
        };
        let i64_ty = self.context.i64_type();
        let mut data_i = self.build_ptr_to_int(data_pv, i64_ty, "res_ret_data_i")?;
        // CVP-CRASH-001 (0.39.x sweep): when data_pv is an LLVM CONSTANT (a
        // global string literal returned by value — `default_string()`), the
        // ptrtoint folds into a ConstantExpr and the whole ownership-probe
        // branch condition becomes `br i1 icmp constexpr ...` — invalid IR
        // that LLVM 18's CalledValuePropagation SIGSEGVs on (prelude.mimi
        // compile crash). Materialize constant probes through an alloca
        // round-trip so every probe node stays a real instruction.
        if data_i.is_const() {
            let slot = self.build_alloca(BasicTypeEnum::IntType(i64_ty), "res_ret_data_i_slot")?;
            self.build_store(slot, data_i)?;
            data_i = self
                .build_load(BasicTypeEnum::IntType(i64_ty), slot, "res_ret_data_i_load")?
                .into_int_value();
        }
        let candidates = self.heap_probe_candidates();
        let mut owned = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                data_i,
                i64_ty.const_int(0, false),
                "res_ret_null",
            )
            .map_err(|e| CompileError::LlvmError(format!("res ret null cmp: {}", e)))?;
        for cand in candidates {
            let eq = self
                .builder
                .build_int_compare(inkwell::IntPredicate::EQ, data_i, cand, "res_ret_cand_eq")
                .map_err(|e| CompileError::LlvmError(format!("res ret cmp: {}", e)))?;
            owned = self
                .builder
                .build_or(owned, eq, "res_ret_owned")
                .map_err(|e| CompileError::LlvmError(format!("res ret or: {}", e)))?;
        }
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("res ret claim outside function".into()))?;
        let heap_bb = self.context.append_basic_block(parent, "res_ret_heap");
        let copy_bb = self.context.append_basic_block(parent, "res_ret_copy");
        let cont_bb = self.context.append_basic_block(parent, "res_ret_cont");
        self.build_cond_br(owned, heap_bb, copy_bb)?;
        self.builder.position_at_end(heap_bb);
        self.build_br(cont_bb)?;
        self.builder.position_at_end(copy_bb);
        let copied = self.heap_copy_string_value(sv.into())?;
        // heap_copy_string_value branches internally (malloc OOM abort), so
        // the builder may now sit in a successor of copy_bb — the phi
        // predecessor must be that ACTUAL block.
        let copy_end_bb = self
            .builder
            .get_insert_block()
            .ok_or_else(|| CompileError::LlvmError("res ret copy lost block".into()))?;
        self.build_br(cont_bb)?;
        self.builder.position_at_end(cont_bb);
        let BasicValueEnum::StructValue(copied_sv) = copied else {
            return Ok(copied);
        };
        let phi = self
            .builder
            .build_phi(sty, "res_ret_val")
            .map_err(|e| CompileError::LlvmError(format!("res ret phi: {}", e)))?;
        phi.add_incoming(&[
            (&sv as &dyn inkwell::values::BasicValue, heap_bb),
            (&copied_sv as &dyn inkwell::values::BasicValue, copy_end_bb),
        ]);
        Ok(phi.as_basic_value())
    }

    /// L6: when a function returns a custom-enum-shaped value `{i32 tag, i64
    /// payload}`, claim the payload box pointer (field 1) so the callee's
    /// scope-exit free skips it — ownership transfers to the caller, which
    /// re-registers the box via `HeapEntry::EnumBox` (see
    /// `track_enum_box_return_lifetime`). Reuses the `claimed_returned_envs`
    /// pointer-compare guard (same mechanism as escaping closure envs, B9).
    ///
    /// Detection is by LLVM shape `{i32, i64}` — a harmless over-approximation:
    /// a record that happens to have that layout would claim a non-heap field
    /// value, which simply matches no registered heap slot (no-op). The precise
    /// custom-enum check happens at the caller registration (which has the
    /// callee's return-type AST), so records are never *freed* as enum boxes.
    /// Multi-target results share the shape but cannot be returned (flow-state
    /// linearity, E0421), so they never reach here.
    /// 0.35.20 (#6): transfer ownership of List data buffers that escape this
    /// function through the return value (returned directly as a List variable,
    /// or packed inside a returned tuple). Their `HeapEntry::Slot(data)`
    /// registrations are dropped so `flush_heap_scopes_to_boundary` skips them
    /// — the caller owns the buffers now. Without this, returning a List
    /// variable freed its data array before the caller could read it
    /// (use-after-free → garbage display / SIGSEGV). Detection is by AST shape
    /// (Ident of a List-typed var, or a tuple of such), covering the common
    /// `return xs` / `(yes, no)` patterns; unrecognized shapes keep the old
    /// scope-exit free.
    ///
    /// 0.35.24 (deep-eval): the walk intentionally stops at Call — the callee's
    /// arguments are INPUTS, not part of the returned value's ownership shape.
    /// Recursing into them (0.35.23) nulled local List variables that never
    /// escape (mutate-builtin tail calls return unit at the language level, so
    /// `return push(data, n)` cannot typecheck; for user functions the callee's
    /// own return path claims whatever it hands back). That nulled the slot and
    /// turned the scope-exit free into free(null): a per-call leak. Borrow
    /// params were guarded (K3), locals were not.
    pub(in crate::codegen) fn claim_returned_lists(
        &self,
        expr: Option<&Expr>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) {
        match expr.map(|e| e.unlocated()) {
            Some(Expr::Ident(name)) => {
                // 0.35.23 deep-eval (mutate_list_push_allowed SIGSEGV): a
                // view/mutate borrow param's list storage IS the caller's
                // struct (pointer ABI). Nulling its data field here (the
                // implicit-return claim after `push(data, n)` recursed into
                // the call args) destroyed the CALLER's list — main's
                // `xs[2]` then loaded through a null data pointer.
                if self.borrow_param_names.contains(name.as_str()) {
                    return;
                }
                // List variables are stored as pointers to the {i64, ptr}
                // list struct (the struct itself is an unnamed local). Null
                // out the struct's data field: the scope-exit free loads the
                // slot and turns into free(null) — a no-op — while the
                // returned struct value (already loaded by the caller path)
                // keeps the live data pointer. Mirrors claim_string_return_value
                // (which nulls the string slot before heap cleanup).
                let is_list_var = self
                    .var_type_names
                    .get(name)
                    .map(|t| t == "List" || t.starts_with("List<"))
                    .unwrap_or(false);
                if is_list_var {
                    if let Some(&(alloca, ty)) = vars.get(name) {
                        let list_ty = self.list_struct_type();
                        let struct_ptr = match ty {
                            BasicTypeEnum::StructType(_) => alloca,
                            BasicTypeEnum::PointerType(_) => {
                                match self.build_load(
                                    self.context.ptr_type(inkwell::AddressSpace::default()),
                                    alloca,
                                    &format!("{}_ret_list", name),
                                ) {
                                    Ok(BasicValueEnum::PointerValue(p)) => p,
                                    _ => alloca,
                                }
                            }
                            _ => alloca,
                        };
                        if let Ok(data_gep) = self.gep().build_struct_gep(
                            list_ty,
                            struct_ptr,
                            1,
                            &format!("{}_ret_list_data", name),
                        ) {
                            let null_ptr = self
                                .context
                                .ptr_type(inkwell::AddressSpace::default())
                                .const_null();
                            let _ = self.build_store(data_gep, null_ptr);
                        }
                    }
                }
            }
            Some(Expr::Tuple(elems)) => {
                for e in elems {
                    self.claim_returned_lists(Some(e), vars);
                }
            }
            Some(Expr::Record { fields, .. }) => {
                for f in fields {
                    self.claim_returned_lists(Some(&f.value), vars);
                }
            }
            // 0.35.24: Call args are inputs — do not recurse (see fn doc).
            Some(Expr::Call(..)) => {}
            Some(Expr::Block(stmts)) => {
                if let Some(last) = stmts.last() {
                    if let Stmt::Expr(e) = last.unlocated() {
                        self.claim_returned_lists(Some(e), vars);
                    }
                }
            }
            _ => {}
        }
    }

    /// 0.35.20 (#6): deep-copy List *literals* that escape through a return.
    /// claim_returned_lists nulls the data slot of List *variables*, but a
    /// literal (`return [1, 2]` or a tuple like `([1, 2], [3, 4])`) has no
    /// named slot to null — its buffer is registered by build_list_struct and
    /// freed by the scope-exit flush, leaving the returned struct value
    /// dangling (garbage under O0; O1 happened to mask it). Copy the buffer
    /// here so the caller owns a fresh one while the literal's original is
    /// freed harmlessly. Recurses into tuple fields. llvm.memcpy with size 0
    /// permits null pointers (LangRef), so empty lists need no special case.
    pub(in crate::codegen) fn claim_returned_list_literals(
        &mut self,
        val: BasicValueEnum<'ctx>,
        expr: Option<&Expr>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match expr.map(|e| e.unlocated()) {
            Some(Expr::List(_)) => self.deep_copy_list_value(val),
            Some(Expr::Tuple(items)) => match val {
                BasicValueEnum::StructValue(sv) => {
                    let mut new_sv = sv;
                    for (i, item) in items.iter().enumerate() {
                        let fv = self.build_extract_value(new_sv.into(), i as u32, "ret_tup_f")?;
                        let nf = self.claim_returned_list_literals(fv, Some(item))?;
                        new_sv = self
                            .builder
                            .build_insert_value(new_sv, nf, i as u32, "ret_tup_nf")
                            .map_err(|e| CompileError::LlvmError(format!("tuple insert: {}", e)))?
                            .into_struct_value();
                    }
                    Ok(BasicValueEnum::StructValue(new_sv))
                }
                _ => Ok(val),
            },
            Some(Expr::Record { fields, .. }) => match val {
                BasicValueEnum::StructValue(sv) => {
                    let mut new_sv = sv;
                    for (i, f) in fields.iter().enumerate() {
                        let fv = self.build_extract_value(new_sv.into(), i as u32, "ret_rec_f")?;
                        let nf = self.claim_returned_list_literals(fv, Some(&f.value))?;
                        new_sv = self
                            .builder
                            .build_insert_value(new_sv, nf, i as u32, "ret_rec_nf")
                            .map_err(|e| CompileError::LlvmError(format!("record insert: {}", e)))?
                            .into_struct_value();
                    }
                    Ok(BasicValueEnum::StructValue(new_sv))
                }
                _ => Ok(val),
            },
            _ => Ok(val),
        }
    }

    /// Deep-copy a List struct value's data buffer ({i64 len, ptr data}).
    fn deep_copy_list_value(
        &mut self,
        val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let sv = match val {
            BasicValueEnum::StructValue(sv) => sv,
            BasicValueEnum::PointerValue(pv) => {
                // build_list_struct returns an alloca pointer; load it.
                let list_ty = self.list_struct_type();
                self.build_load(BasicTypeEnum::StructType(list_ty), pv, "ret_list_ld")?
                    .into_struct_value()
            }
            _ => return Ok(val),
        };
        let len = self
            .build_extract_value(sv.into(), 0, "ret_list_len")?
            .into_int_value();
        let data = self
            .build_extract_value(sv.into(), 1, "ret_list_data")?
            .into_pointer_value();
        let i64_ty = self.context.i64_type();
        let bytes = self
            .builder
            .build_int_mul(len, i64_ty.const_int(8, false), "ret_list_bytes")
            .map_err(|e| CompileError::LlvmError(format!("mul: {}", e)))?;
        let new_data = self.malloc_or_abort(bytes, "ret_list_copy")?;
        let memcpy_fn = self.get_runtime_fn("memcpy")?;
        self.builder
            .build_call(
                memcpy_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(new_data),
                    BasicMetadataValueEnum::PointerValue(data),
                    BasicMetadataValueEnum::IntValue(bytes),
                ],
                "ret_list_memcpy",
            )
            .map_err(|e| CompileError::LlvmError(format!("memcpy: {}", e)))?;
        let list_ty = self.list_struct_type();
        let new_sv = self
            .builder
            .build_insert_value(list_ty.get_undef(), len, 0, "ret_list_len")
            .map_err(|e| CompileError::LlvmError(format!("insert len: {}", e)))?
            .into_struct_value();
        let new_sv = self
            .builder
            .build_insert_value(new_sv, new_data, 1, "ret_list_data")
            .map_err(|e| CompileError::LlvmError(format!("insert data: {}", e)))?
            .into_struct_value();
        Ok(BasicValueEnum::StructValue(new_sv))
    }

    pub(in crate::codegen) fn claim_returned_enum_box(
        &self,
        val: BasicValueEnum<'ctx>,
        ret_type: BasicTypeEnum<'ctx>,
    ) -> Result<(), CompileError> {
        let BasicTypeEnum::StructType(st) = ret_type else {
            return Ok(());
        };
        let fields = st.get_field_types();
        let is_enum_shape = fields.len() == 2
            && matches!(fields[0], BasicTypeEnum::IntType(it) if it.get_bit_width() == 32)
            && matches!(fields[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64);
        if !is_enum_shape {
            return Ok(());
        }
        let BasicValueEnum::StructValue(sv) = val else {
            return Ok(());
        };
        let payload = self
            .builder
            .build_extract_value(sv, 1, "enum_ret_box_i64")
            .map_err(|e| CompileError::LlvmError(format!("enum ret box extract: {e}")))?;
        if let BasicValueEnum::IntValue(iv) = payload {
            let box_ptr = self.build_int_to_ptr(
                iv,
                self.context.ptr_type(AddressSpace::default()),
                "enum_ret_box",
            )?;
            // Reuse the closure-env claim set: a generic "skip this pointer's
            // free, the caller owns it now" mechanism (pointer-compare guard).
            self.claim_closure_env(box_ptr);
        }
        Ok(())
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
    pub(in crate::codegen) fn heap_copy_string_value(
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
        self.build_call(
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
        _func_name: &str,
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
                    super::scope::ContractPhase::Ensures,
                )?;
            }
        }
        let val = val
            .map(|v| self.claim_string_return_value(v, ret_type, expr, vars))
            .transpose()?;
        // L6: claim a returned custom-enum payload box so the callee's
        // scope-exit free (flush_heap_scopes_to_boundary below) skips it —
        // ownership transfers to the caller, which re-registers the box via
        // HeapEntry::EnumBox (track_enum_box_return_lifetime). Mirrors the
        // claim_string_return_value above. Detection is by LLVM shape
        // {i32, i64}; the precise custom-enum check is at the caller.
        if let Some(v) = val {
            self.claim_returned_enum_box(v, ret_type)?;
        }
        self.pop_shared_scope()?;
        self.pop_comp_scope();
        self.pop_cap_scope();
        match val {
            Some(v) => {
                let adjusted = self.coerce_variant_value(v, ret_type, ret_ty_ast)?;
                let adjusted = self.load_return_value_if_needed(adjusted)?;
                // 0.35.20 (#6): claim returned List variables' data buffers —
                // null out the variable slot's data field AFTER the return
                // value has been loaded, so the returned struct keeps the
                // live pointer while the scope-exit free turns into free(null)
                // (ownership transfers to the caller).
                self.claim_returned_lists(expr, vars);
                // 0.35.20 (#6): deep-copy List literals escaping the return
                // (no named slot to null). Both claims must run BEFORE the
                // flush below — the previous order flushed first, freeing the
                // buffers the claims were supposed to protect (visible under
                // O0, masked by O1).
                let mut adjusted = self.claim_returned_list_literals(adjusted, expr)?;
                // CVP-CRASH-001 companion: the claim/rebuild passes may hand
                // back a value whose LLVM type drifted from the declared
                // signature (observed: a nested tuple field returned as i32
                // while the function type says i64). `ret <mismatch>` is
                // invalid IR — O0 tolerated it, but O1's
                // CalledValuePropagation SIGSEGV'd on it. Align numeric
                // leaves here; impossible shapes fail loud instead of
                // poisoning the pass pipeline.
                if adjusted.get_type() != ret_type {
                    match (adjusted, ret_type) {
                        (BasicValueEnum::IntValue(iv), BasicTypeEnum::IntType(it)) => {
                            let w_from = iv.get_type().get_bit_width();
                            let w_to = it.get_bit_width();
                            adjusted = if w_from < w_to {
                                self.builder
                                    .build_int_s_extend(iv, it, "ret_sext")
                                    .map_err(|e| CompileError::LlvmError(format!("ret sext: {e}")))?
                                    .into()
                            } else if w_from > w_to {
                                self.builder
                                    .build_int_truncate(iv, it, "ret_trunc")
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("ret trunc: {e}"))
                                    })?
                                    .into()
                            } else {
                                adjusted
                            };
                        }
                        (BasicValueEnum::StructValue(sv), BasicTypeEnum::StructType(st)) => {
                            // GENERIC-RET-ALIGN (0.39.x sweep): the signature's
                            // tuple slots widen integer leaves to i64 while the
                            // body's tuple literal keeps exact widths — align
                            // leaf-by-leaf so `ret <mismatch>` never reaches
                            // the pass pipeline (O1's CVP crashed on it).
                            adjusted = self.align_struct_return(sv, st)?;
                        }
                        _ => {}
                    }
                    if adjusted.get_type() != ret_type {
                        return Err(CompileError::LlvmError(format!(
                            "return value type {:?} does not match declared signature {:?}",
                            adjusted.get_type(),
                            ret_type
                        )));
                    }
                }
                self.flush_heap_scopes_to_boundary()?;
                self.build_return(Some(&adjusted))?;
            }
            None => {
                // 0.35.23 (deep-eval): a bare `return` in a unit function
                // must `ret i64 0` — the unit signature is i64 (compile_func),
                // so the old `ret void` was invalid IR (mismatched terminator)
                // that O1's CalledValuePropagationPass SIGSEGV'd on
                // ("func f() { if true { return } }" crash).
                let zero = self.zero_value_for(ret_type);
                self.flush_heap_scopes_to_boundary()?;
                self.build_return(Some(&zero))?;
            }
        }
        Ok(())
    }

    /// GENERIC-RET-ALIGN: recursively align an aggregate return value to the
    /// declared signature layout. Integer leaves are sign-extended/truncated;
    /// identical types pass through; nested structs recurse; anything else is
    /// a hard error (fail loud instead of poisoning the optimizer).
    fn align_struct_return(
        &mut self,
        sv: inkwell::values::StructValue<'ctx>,
        target: inkwell::types::StructType<'ctx>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let src_ty = sv.get_type();
        let src_fields = src_ty.get_field_types();
        let dst_fields = target.get_field_types();
        if src_fields.len() != dst_fields.len() {
            return Err(CompileError::LlvmError(format!(
                "return aggregate arity mismatch: {} vs {}",
                src_fields.len(),
                dst_fields.len()
            )));
        }
        let mut rebuilt = target.const_zero();
        for (i, (sf, df)) in src_fields.iter().zip(dst_fields.iter()).enumerate() {
            let fv = self.build_extract_value(sv.into(), i as u32, "align_f")?;
            let aligned: BasicValueEnum = match (sf, df) {
                (BasicTypeEnum::IntType(sit), BasicTypeEnum::IntType(dit)) => {
                    if sit == dit {
                        fv
                    } else {
                        let w_from = sit.get_bit_width();
                        let w_to = dit.get_bit_width();
                        let iv = fv.into_int_value();
                        if w_from < w_to {
                            self.builder
                                .build_int_s_extend(iv, *dit, "align_sext")
                                .map_err(|e| CompileError::LlvmError(format!("align sext: {e}")))?
                                .into()
                        } else {
                            self.builder
                                .build_int_truncate(iv, *dit, "align_trunc")
                                .map_err(|e| CompileError::LlvmError(format!("align trunc: {e}")))?
                                .into()
                        }
                    }
                }
                (BasicTypeEnum::StructType(ss), BasicTypeEnum::StructType(ds)) => {
                    if ss == ds {
                        fv
                    } else {
                        let inner = fv.into_struct_value();
                        self.align_struct_return(inner, *ds)?
                    }
                }
                _ => {
                    if sf == df {
                        fv
                    } else {
                        return Err(CompileError::LlvmError(format!(
                            "return aggregate field {i}: incompatible layouts"
                        )));
                    }
                }
            };
            rebuilt = self
                .builder
                .build_insert_value(rebuilt, aligned, i as u32, "align_ins")
                .map_err(|e| CompileError::LlvmError(format!("align insert: {e}")))?
                .into_struct_value();
        }
        Ok(BasicValueEnum::StructValue(rebuilt))
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
    /// If a block's last expression yields a raw C-string pointer (string
    /// literal), wrap it into the canonical {ptr, i64} struct so if-expressions
    /// and merge phis see a uniform string layout.
    pub(in crate::codegen) fn normalize_block_last_string(
        &self,
        val: BasicValueEnum<'ctx>,
        block: &Block,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if let BasicValueEnum::PointerValue(_) = val {
            if let Some(last) = block.last() {
                if let Stmt::Expr(e) = last.unlocated() {
                    if self.expr_is_string(e) {
                        return self.wrap_raw_string_ptr(val.into_pointer_value());
                    }
                }
            }
        }
        Ok(val)
    }

    pub(super) fn coerce_variant_value(
        &self,
        val: BasicValueEnum<'ctx>,
        target_ty: BasicTypeEnum<'ctx>,
        ret_ty_ast: Option<&Type>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let ast_ty = match ret_ty_ast {
            Some(t) => t,
            None => return Ok(val),
        };
        let is_result = matches!(ast_ty.unlocated(), Type::Result(_, _))
            || matches!(ast_ty.unlocated(), Type::Name(n, args) if n == "Result" && args.len() == 2);
        let is_option = matches!(ast_ty.unlocated(), Type::Option(_))
            || matches!(ast_ty.unlocated(), Type::Name(n, args) if n == "Option" && args.len() == 1);
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
            // 0.35.8-fix (dx-backlog #20): pointer payload -> structured
            // target, load the pointee. `compile_ok_constructor` packs
            // {i1, ptr, i64} with the Ok payload ADDRESS in the ptr slot;
            // coercing to the declared Result<T,E> layout ({i1, T, E} with a
            // by-value T) previously stored the raw pointer into the T slot,
            // corrupting every struct payload (e.g. a Flow state with string
            // fields: `puts(0x1)` — the state's address bits read as the
            // first string's data pointer). The mimi-string wrap above keeps
            // raw C-string pointers intact; any other ptr-vs-struct mismatch
            // here is a boxed/alloca payload and must be dereferenced.
            (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(st)) => self
                .build_load(BasicTypeEnum::StructType(st), pv, "coerce_load_struct")
                .map_err(|e| CompileError::LlvmError(format!("coerce load: {}", e))),
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

    /// 0.35.7-fix: LLVM type for a legacy-function parameter. A generic
    /// parameter whose name collides with a user type (e.g. `type T` +
    /// `func eq<T>(a: T, b: T)`) must NOT resolve through the user type table
    /// — `llvm_type_for` looks up `Type::Name` in `self.type_llvm`, so the
    /// generic skeleton would be declared/bound with the user enum's struct
    /// layout and `a == b` (struct == struct) would fail with "eq requires
    /// same types". The type-map-empty skeleton is only emitted to satisfy
    /// the legacy declaration pass; real calls go through
    /// `compile_generic_func` (monomorphized). i64 keeps the skeleton
    /// compilable.
    fn legacy_param_llvm_type(
        &self,
        func: &FuncDef,
        param: &crate::ast::Param,
    ) -> Option<BasicTypeEnum<'ctx>> {
        let generic_param_names: std::collections::HashSet<&str> =
            func.generics.iter().map(|g| g.name.as_str()).collect();
        if matches!(
            param.ty.unlocated(),
            crate::ast::Type::Name(n, a) if a.is_empty() && generic_param_names.contains(n.as_str())
        ) {
            // Deep-eval 2026-08-09 (demos/03 swap_pair): during
            // monomorphization (type_map active) the generic name has a
            // concrete substitute — use it, otherwise a string-typed U
            // parameter gets an i64 alloca and the body stores the 16-byte
            // {ptr,i64} struct into it (stack corruption + SelectionDAG
            // crash). The i64 placeholder stays for the skeleton pass
            // (empty type_map), where it only has to satisfy declare_func.
            if let crate::ast::Type::Name(n, _) = param.ty.unlocated() {
                if let Some(concrete) = self.type_map.get(n) {
                    if let Some(ty) = self.llvm_type_for(concrete) {
                        return Some(ty);
                    }
                }
            }
            return Some(BasicTypeEnum::IntType(self.context.i64_type()));
        }
        let resolved = self.resolve_type(&param.ty);
        self.llvm_type_for(&resolved)
    }

    /// Bind all function parameters to stack allocas and track type metadata
    /// (type names, list element types, and capabilities).
    fn bind_func_params(
        &mut self,
        func: &FuncDef,
        function: FunctionValue<'ctx>,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        // K-3 (full-audit 2026-08-05 §3.6): parameters without an LLVM type
        // (unit/nothing/Infer → llvm_type_for = None) are skipped BOTH in the
        // signature construction (declare_func / generic instantiation) and in
        // this binding loop. The LLVM parameter index therefore advances
        // independently of the source parameter index. Indexing
        // get_nth_param by the source index (old code) made every parameter
        // after a skipped one bind the WRONG LLVM parameter, or error with an
        // out-of-range index once skipped params outnumbered the remainder.
        let mut llvm_param_idx: u32 = 0;
        for param in func.params.iter() {
            let resolved = self.resolve_type(&param.ty);
            if let Some(ty) = self.legacy_param_llvm_type(func, param) {
                let mut param_val = function.get_nth_param(llvm_param_idx).ok_or_else(|| {
                    CompileError::LlvmError(format!(
                        "param index {} out of range for function '{}' with {} params",
                        llvm_param_idx,
                        func.name,
                        function.count_params()
                    ))
                })?;
                llvm_param_idx += 1;
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
                    if let Type::Name(tn, _) = resolved.unlocated() {
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
                if let Type::Name(tn, args) = resolved.unlocated() {
                    if tn == "List" && !args.is_empty() {
                        if let Some(full) = self.get_full_type_name(&resolved) {
                            self.var_type_names.insert(param.name.clone(), full);
                        }
                    } else {
                        self.var_type_names.insert(param.name.clone(), tn.clone());
                    }
                    self.var_types.insert(param.name.clone(), resolved.clone());
                }
                if let Type::Ref(_, inner) | Type::RefMut(_, inner) = resolved.unlocated() {
                    if let Type::Name(tn, args) = inner.unlocated() {
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
                if let Type::DynTrait(_) = resolved.unlocated() {
                    self.var_type_names
                        .insert(param.name.clone(), crate::core::fmt_type(&resolved));
                    self.var_types.insert(param.name.clone(), resolved.clone());
                }
                if let Type::ImplTrait(_) = resolved.unlocated() {
                    self.var_type_names
                        .insert(param.name.clone(), crate::core::fmt_type(&resolved));
                    self.var_types.insert(param.name.clone(), resolved.clone());
                }
                if let Type::Func(_, _) | Type::ExternFunc(_, _) = resolved.unlocated() {
                    self.var_type_names
                        .insert(param.name.clone(), crate::core::fmt_type(&resolved));
                    self.var_types.insert(param.name.clone(), resolved.clone());
                }

                // Register list element type for List<T> params where T is a struct
                self.register_list_elem_type(&param.name, &resolved);

                // Track capability parameters
                if matches!(param.ty.unlocated(), Type::Cap(_) | Type::CapAtom(_)) {
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
        self.current_fn_ret_ty_ast = func.ret.clone();
        // 0.35.23 deep-eval: refresh the borrow-param guard set for this
        // body — claim_returned_lists must skip view/mutate params whose
        // list storage is the caller's struct (pointer ABI).
        self.borrow_param_names = func
            .params
            .iter()
            .filter(|p| p.borrow.is_some())
            .map(|p| p.name.clone())
            .collect();
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
        for (stmt_index, stmt) in func.body.iter().enumerate() {
            // H-8 (full-audit-2026-08-05): a tail bare/wrapper block carries
            // the implicit return value. compile_block() discards it; the
            // tail position must extract it (mirror of compile_block_last_val).
            let is_tail = stmt_index + 1 == func.body.len();
            // Run compensations before exit()
            if let Stmt::Expr(expr) = stmt.unlocated() {
                if let Expr::Call(callee, _) = expr.unlocated() {
                    if let Expr::Ident(name) = callee.unlocated() {
                        if name == "exit" {
                            self.compile_compensations(vars)?;
                        }
                    }
                }
            }
            match stmt.unlocated() {
                Stmt::Expr(expr) => {
                    // 0.35.23 deep-eval: a NON-tail statement-position match
                    // discards its value (mimi-log main `match content {..}`
                    // with heterogeneous assignment tails previously errored
                    // E0200 "match arm values have incompatible types"). Tail
                    // matches keep expression semantics (implicit return).
                    if matches!(expr.unlocated(), Expr::Match(..)) && !is_tail {
                        if let Expr::Match(scrutinee, arms) = expr.unlocated() {
                            self.compile_match_expr(scrutinee, arms, vars, true)?;
                        }
                    } else {
                        last_val = self.compile_expr(expr, vars)?;
                        last_val = self.adjust_int_val(last_val, ret_type)?;
                        last_val = self.coerce_variant_value(last_val, ret_type, ret_ty_ast)?;
                    }
                }
                Stmt::Return(Some(expr)) => {
                    let mut val = self.compile_expr(expr, vars)?;
                    // v0.34.16 (ADR-002): multi-target transition return —
                    // wrap the target state struct into the synthetic
                    // {i32 tag, i64 payload} union (payload = ptrtoint boxed
                    // state struct). Tag = the state's sorted ordinal (must
                    // match register_type_def's Enum variant ordering).
                    if self.in_multi_target_transition {
                        let state_name = match expr.unlocated() {
                            Expr::Record { ty: Some(ty_name), .. } => ty_name.clone(),
                            Expr::Located { expr: inner, .. } => {
                                match inner.unlocated() {
                                    Expr::Record { ty: Some(ty_name), .. } => ty_name.clone(),
                                    _ => {
                                        return Err(CompileError::LlvmError(
                                            "multi-target transition return must construct a target state record (e.g. `return TargetState { ... }`)".to_string(),
                                        ))
                                    }
                                }
                            }
                            _ => {
                                return Err(CompileError::LlvmError(
                                    "multi-target transition return must construct a target state record (e.g. `return TargetState { ... }`)".to_string(),
                                ))
                            }
                        };
                        // C1 fix: tag = the state's ordinal in the flow-wide
                        // __MultiTarget enum (name-sorted union of ALL
                        // multi-target states), NOT the per-transition subset.
                        // A subset-relative ordinal silently aliases another
                        // state when two transitions have different target sets.
                        let tag = self
                            .multi_target_global_ordinals
                            .get(&self.current_flow_name)
                            .and_then(|m| m.get(&state_name))
                            .copied()
                            .ok_or_else(|| {
                                CompileError::LlvmError(format!(
                                    "returned state '{state_name}' has no global multi-target ordinal (flow: {:?}, transition targets: {:?})",
                                    self.current_flow_name,
                                    self.multi_target_states
                                ))
                            })?;
                        let state_ty = self.flow_state_llvm_type(&state_name);
                        val = self.wrap_multi_target_value(val, tag, state_ty)?;
                    }
                    let val = if self.in_fails_transition {
                        self.compile_ok_constructor(vec![val])?
                    } else {
                        val
                    };
                    let val = self.adjust_int_val(val, ret_type)?;
                    self.pop_defer_scope(vars)?;
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
                    self.pop_defer_scope(vars)?;
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
                    if let Some(Type::DynTrait(trait_names)) = ty.as_ref().map(Type::unlocated) {
                        let name = match &pat.kind {
                            PatternKind::Variable(n) => n.clone(),
                            _ => {
                                return Err(CompileError::LlvmError(
                                    "dyn Trait binding requires a simple variable pattern"
                                        .to_string(),
                                ))
                            }
                        };
                        let concrete_val = self.compile_expr(init, vars)?;
                        // 条款 11 escape hatch: `dyn X = unsafe_cast_protocol(v)`
                        // infers the concrete type from the inner value. The
                        // escape hatch also permits a MISSING vtable (null) —
                        // the user guarantees conformance; calling an
                        // unimplemented method then aborts via the CG-H7 null
                        // guard instead of failing the build.
                        let mut is_unsafe_protocol_cast = false;
                        let concrete_type = match init.unlocated() {
                            Expr::Record { ty: Some(tn), .. } => tn.clone(),
                            Expr::Ident(var_name) => self
                                .var_type_names
                                .get(var_name)
                                .cloned()
                                .unwrap_or_default(),
                            Expr::Call(callee, args) if args.len() == 1 => {
                                match callee.unlocated() {
                                    Expr::Ident(fname) if fname == "unsafe_cast_protocol" => {
                                        is_unsafe_protocol_cast = true;
                                        match args[0].unlocated() {
                                            Expr::Ident(var_name) => self
                                                .var_type_names
                                                .get(var_name)
                                                .cloned()
                                                .unwrap_or_default(),
                                            Expr::Record { ty: Some(tn), .. } => tn.clone(),
                                            _ => String::new(),
                                        }
                                    }
                                    _ => String::new(),
                                }
                            }
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
                        // Records bind as pointers in Mimi (reference
                        // semantics): the compiled value is a LidarDriver*
                        // while type_llvm registers the {i32} value shape.
                        // Storing the value into a {i32} data slot truncates
                        // the pointer (garbage). When the value is already a
                        // pointer, the data slot holds the pointer itself —
                        // matching the impl method's `self: &Type` ABI.
                        let concrete_ty = match concrete_val {
                            BasicValueEnum::PointerValue(_) => self
                                .context
                                .ptr_type(inkwell::AddressSpace::default())
                                .into(),
                            _ => self
                                .type_llvm
                                .get(&concrete_type)
                                .cloned()
                                .unwrap_or_else(|| concrete_val.get_type()),
                        };
                        let data_alloca =
                            self.build_alloca(concrete_ty, &format!("{}_data", name))?;
                        self.build_store(data_alloca, concrete_val)?;
                        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                        // The fat pointer's data slot must hold the VALUE
                        // (a LidarDriver* for record semantics) that the impl
                        // method's `self: &Type` receives — not the address of
                        // the staging alloca (that would double-indirect).
                        // Load the stored value, then cast it to i8*.
                        // H5 (audit-codegen 2026-08-03): a non-record concrete
                        // type (e.g. `unsafe_cast_protocol(5)` with x: i32)
                        // makes the load produce a non-pointer — the old
                        // `into_pointer_value()` panicked (user-reachable ICE).
                        // Reject explicitly instead: dyn dispatch needs a
                        // record-shaped self (pointer semantics); scalar
                        // values cannot form a valid fat data slot.
                        let loaded = self
                            .build_load(concrete_ty, data_alloca, &format!("{}_data_val", name))
                            .map_err(|e| {
                                CompileError::LlvmError(format!(
                                    "unsafe_cast_protocol: concrete type '{}' is not a record \
                                     (load produced a non-pointer: {}); dyn trait dispatch \
                                     requires a record-typed value",
                                    concrete_type, e
                                ))
                            })?;
                        let data_ptr = match loaded {
                            BasicValueEnum::PointerValue(ptr) => ptr,
                            _ => {
                                return Err(CompileError::LlvmError(format!(
                                    "unsafe_cast_protocol: concrete type '{}' is not a record \
                                     (value is not a pointer); dyn trait dispatch requires a \
                                     record-typed value",
                                    concrete_type
                                )))
                            }
                        };
                        let data_ptr = self
                            .builder
                            .build_pointer_cast(data_ptr, i8_ptr, &format!("{}_data_i8", name))
                            .map_err(|e| {
                                CompileError::LlvmError(format!("pointer cast error: {}", e))
                            })?;
                        let vtable_key = format!("{}__{}", concrete_type, trait_name);
                        let vtable_ptr = match self.vtable_globals.get(&vtable_key) {
                            Some(vtable) => self
                                .builder
                                .build_pointer_cast(
                                    vtable.as_pointer_value(),
                                    i8_ptr,
                                    &format!("{}_vtable_i8", name),
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("pointer cast error: {}", e))
                                })?,
                            None if is_unsafe_protocol_cast => {
                                // 条款 11: conformance is user-guaranteed. A
                                // null vtable defers the failure to runtime —
                                // the CG-H7 null guard aborts on a real call.
                                i8_ptr.const_null()
                            }
                            None => {
                                return Err(CompileError::LlvmError(format!(
                                    "no vtable for {}.{}",
                                    concrete_type, trait_name
                                )));
                            }
                        };
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
                        if let Some(Type::Cap(_) | Type::CapAtom(_)) =
                            ty.as_ref().map(Type::unlocated)
                        {
                            self.register_cap(&name, fat_alloca);
                        }
                        continue;
                    }
                    // Shared ref copy: let v = shared_var
                    if let PatternKind::Variable(name) = &pat.kind {
                        if let Expr::Ident(src_name) = init.unlocated() {
                            if self.shared_var_names.contains(src_name.as_str()) {
                                self.compile_shared_ref_copy(name, src_name, vars)?;
                                continue;
                            }
                        }
                    }
                    // Shared var clone: let v = shared_var.clone()
                    if let PatternKind::Variable(name) = &pat.kind {
                        if let Expr::Call(callee, cargs) = init.unlocated() {
                            if cargs.is_empty() {
                                if let Expr::Field(obj, method_name) = callee.unlocated() {
                                    if method_name == "clone" {
                                        if let Expr::Ident(src_name) = obj.unlocated() {
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
                    let saved_list_elem = self.pending_list_elem_type.take();
                    if matches!(init.unlocated(), Expr::List(_)) {
                        if let Some(decl_ty) = ty.as_ref() {
                            if let Type::Name(n, args) = decl_ty.unlocated() {
                                if n == "List" && args.len() == 1 {
                                    self.pending_list_elem_type = Some(args[0].clone());
                                }
                            }
                        }
                    }
                    let mut val = self.compile_expr(init, vars)?;
                    self.pending_list_elem_type = saved_list_elem;
                    if let Some(decl_ty) = ty {
                        let target = types::mimi_type_to_llvm(self.context, decl_ty)
                            .unwrap_or_else(|| val.get_type());
                        // SD-7 (0.34.34): narrowing bind into an annotated i32
                        // slot range-checks before the silent truncate — the VM
                        // CheckI32 let-guard traps E0802 identically. This is
                        // the TOP-LEVEL legacy body path (compile_func_body);
                        // nested blocks are covered by compile_block.
                        if Self::annotated_type_name(decl_ty) == Some("i32") {
                            if let (BasicValueEnum::IntValue(iv), BasicTypeEnum::IntType(it)) =
                                (val, target)
                            {
                                if iv.get_type().get_bit_width() > it.get_bit_width() {
                                    self.emit_i32_range_guard(iv, "let-bind")?;
                                }
                            }
                        }
                        val = self.adjust_int_val(val, target)?;
                    }
                    // Normalize string values: wrap raw pointers into canonical
                    // {i8*, i64} struct so variable allocas have consistent type.
                    val = self.normalize_string_value(val, init)?;
                    // Track type info for simple Variable patterns
                    if let PatternKind::Variable(name) = &pat.kind {
                        // 0.35.23 deep-eval: `let y = x` inherits the source
                        // variable's tracked type (top-level body counterpart
                        // of the block.rs fixes). mimi-log main's
                        // `let mut filtered = entries` above `for e in
                        // display` lost the List<LogEntry> element type, so
                        // the element bound as bare i64 and field access
                        // failed E0700 in the legacy emitter.
                        if let Expr::Ident(src_name) = init.unlocated() {
                            if std::env::var("MIMI_VERBOSE").is_ok() {
                                eprintln!(
                                    "DBG let-ident inherit: name={} src={} src_ty_names={:?} src_ty={:?}",
                                    name,
                                    src_name,
                                    self.var_type_names.get(src_name),
                                    self.var_types.get(src_name)
                                );
                            }
                            if !self.var_type_names.contains_key(name.as_str()) {
                                if let Some(src_ty) = self.var_type_names.get(src_name).cloned() {
                                    self.var_type_names.insert(name.clone(), src_ty);
                                }
                            }
                            if !self.var_types.contains_key(name.as_str()) {
                                if let Some(src_ty) = self.var_types.get(src_name).cloned() {
                                    self.var_types.insert(name.clone(), src_ty);
                                }
                            }
                        }
                        // 0.35.23 deep-eval: `let buf = store.buffer` — a
                        // field-access init must register the field's type
                        // (infer_object_type resolves it via the record
                        // catalog). Without this, `buf[i]` on a List<string>
                        // returned the raw i64 slot and json_is_valid(msg)
                        // failed "expected a string argument" (mimichat
                        // MessageStore::store_get_by_room).
                        if let Expr::Field(_, _) = init.unlocated() {
                            let field_ty = self.infer_object_type(init, vars);
                            if !field_ty.is_empty() {
                                self.var_type_names.insert(name.clone(), field_ty);
                            }
                        }
                        if let Some(ty_ref) = &ty {
                            // Store the canonical display type name (e.g.
                            // `(Option<i64>, Result<i64, i64>)` for tuples,
                            // `Option<i64>` for wrappers, `List<i64>` for
                            // generics) so `to_json` / dispatch routing resolves
                            // the real type instead of falling back to the bare
                            // variable name. `get_full_type_name` renders every
                            // surface type form the recursive to_json generator
                            // understands.
                            if let Some(full) = self.get_full_type_name(ty_ref) {
                                self.var_type_names.insert(name.clone(), full);
                            } else if let Type::Name(tn, _) = ty_ref.unlocated() {
                                self.var_type_names.insert(name.clone(), tn.clone());
                            }
                        } else if self.expr_is_string(init) {
                            self.var_type_names
                                .insert(name.clone(), "string".to_string());
                        } else if let Expr::Lambda { params, ret, .. } = init.unlocated() {
                            // Record the closure's Func type so a subsequent call
                            // (closure_return_llvm_type) can determine the real
                            // return type — e.g. a custom enum → {i32,i64} — instead
                            // of defaulting the indirect call to i64. Without this,
                            // a let-bound `fn() -> Shape { .. }` is called as i64
                            // while the lambda body returns {i32,i64}, miscompiling
                            // the call and the match on its result.
                            let param_tys: Vec<Type> =
                                params.iter().map(|p| p.ty.clone()).collect();
                            let ret_ty = ret
                                .clone()
                                .unwrap_or_else(|| Type::Name("unit".to_string(), vec![]));
                            self.var_types
                                .insert(name.clone(), Type::Func(param_tys, Box::new(ret_ty)));
                        } else if let Expr::Record {
                            ty: Some(tn),
                            fields,
                            ..
                        } = init.unlocated()
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
                        } else if matches!(init.unlocated(), Expr::SetLiteral(_)) {
                            self.var_type_names.insert(name.clone(), "set".to_string());
                        } else if let Expr::List(list_elems) = init.unlocated() {
                            // D1: infer List<T> type from first element
                            if let Some(first) = list_elems.first() {
                                let elem_type = self.infer_object_type(first, vars);
                                if !elem_type.is_empty() {
                                    self.var_type_names
                                        .insert(name.clone(), format!("List<{}>", elem_type));
                                }
                            }
                        } else if let Expr::Index(_, _) = init.unlocated() {
                            // D1: infer element type via infer_object_type (handles List<T> stripping)
                            let elem_type = self.infer_object_type(init, vars);
                            if !elem_type.is_empty() {
                                self.var_type_names.insert(name.clone(), elem_type);
                            }
                        } else if let Expr::SliceExpr { target, .. } = init.unlocated() {
                            // 0.34.36 (audit wave-2 #6): a slice `xs[a .. b]`
                            // keeps the target's element type (List<T> →
                            // List<T>). Without this registration,
                            // `let sub = xs[1 .. 3]` (TOP-LEVEL body) left
                            // `sub` untyped, so `println(sub)` fell into the
                            // puts fast path and printed the list struct
                            // pointer as a C string (garbage). Mirror the
                            // source list's type so println dispatches to the
                            // list formatter. (Nested-block counterpart:
                            // block.rs compile_block.)
                            let target_type = self.infer_object_type(target, vars);
                            if target_type.starts_with("List") || target_type == "set" {
                                self.var_type_names
                                    .insert(name.clone(), target_type.clone());
                            }
                        } else if let Expr::Call(callee, call_args) = init.unlocated() {
                            if let Expr::Field(obj, method_name) = callee.unlocated() {
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
                                    } else if let Expr::Ident(flow_name) = obj.unlocated() {
                                        // Flow::transition — insert/remove may be flow
                                        // transition names, not Set operations.
                                        if let Some(flow) = self.flow_defs.get(flow_name) {
                                            let from_type = call_args
                                                .first()
                                                .map(|a| self.infer_object_type(a, vars))
                                                .unwrap_or_default();
                                            // 0.36.7: from-state args may be
                                            // flow-qualified (`flow::<name>::Fault`);
                                            // overload matching uses the bare name.
                                            let from_type = Self::bare_flow_state_name(&from_type);
                                            let t = flow.transitions.iter().find(|t| {
                                                t.name == *method_name && t.from_state == from_type
                                            }).or_else(|| {
                                                // 0.36.10 (裁决 6 follow-up): recover/reset on a
                                                // declared-faultable result var — register the
                                                // result under the Fault overload (runtime dispatch).
                                                if method_name == "recover" || method_name == "reset"
                                                    && matches!(call_args.first().map(|a| a.unlocated()), Some(Expr::Ident(v))
                                                        if matches!(self.multi_target_result_vars.get(v), Some(f) if f == flow_name))
                                                {
                                                    flow.transitions.iter().find(|t| {
                                                        t.name == *method_name && t.from_state == "Fault"
                                                    })
                                                } else {
                                                    None
                                                }
                                        });
                                            if let Some(t) = t {
                                                let from_state = t.from_state.clone();
                                                let to_states = t.to_states.clone();
                                                let fails = t.fails.clone();
                                                if let Some(to) = to_states.first() {
                                                    // 0.36.7 (裁决 1 跨 flow 补全,
                                                    // legacy leg): register the
                                                    // flow-qualified record name for
                                                    // the Fault sink so legacy field
                                                    // inference (`infer_object_type`)
                                                    // resolves `sf.last_state` against
                                                    // THIS flow's `flow::<name>::Fault`
                                                    // TypeDef (correct StateId/EventId
                                                    // field types), not the bare-name
                                                    // first-wins alias of another flow
                                                    // (wrong enum in native prints).
                                                    let var_ty = Self::transition_result_var_type(
                                                        flow_name, to,
                                                    );
                                                    self.var_type_names
                                                        .insert(name.clone(), var_ty);
                                                    self.track_flow_result_type(
                                                        name,
                                                        &from_state,
                                                        to,
                                                        fails,
                                                    );
                                                    // 0.36.10 (裁决 6 follow-up):
                                                    // declared-faultable multi-target
                                                    // result -> recover/reset-able.
                                                    if to_states.len() > 1 {
                                                        self.multi_target_result_vars.insert(
                                                            name.to_string(),
                                                            flow_name.to_string(),
                                                        );
                                                    }
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
                                        } else {
                                            // Q3 (rc-quality-gate-0.34.25a):
                                            // trait-impl methods on string
                                            // receivers (JsonExt::get_float …)
                                            // — declare the impl return type so
                                            // display/dispatch sees Result<…>.
                                            let impl_ret = self.infer_impl_method_return_type(
                                                &obj_type,
                                                method_name,
                                            );
                                            if !impl_ret.is_empty() {
                                                register_qualified_var_type(
                                                    &mut self.var_type_names,
                                                    name,
                                                    impl_ret,
                                                );
                                            }
                                        }
                                    } else if let Expr::Ident(flow_name) = obj.unlocated() {
                                        // Flow::transition(from, ...) → matching overload's to-state
                                        if let Some(flow) = self.flow_defs.get(flow_name) {
                                            let from_type = call_args
                                                .first()
                                                .map(|a| self.infer_object_type(a, vars))
                                                .unwrap_or_default();
                                            // 0.36.7: from-state args may be
                                            // flow-qualified (`flow::<name>::Fault`);
                                            // overload matching uses the bare name.
                                            let from_type = Self::bare_flow_state_name(&from_type);
                                            let t = flow.transitions.iter().find(|t| {
                                                t.name == *method_name && t.from_state == from_type
                                            }).or_else(|| {
                                                // 0.36.10 (裁决 6 follow-up): recover/reset on a
                                                // declared-faultable result var — register the
                                                // result under the Fault overload (runtime dispatch).
                                                if method_name == "recover" || method_name == "reset"
                                                    && matches!(call_args.first().map(|a| a.unlocated()), Some(Expr::Ident(v))
                                                        if matches!(self.multi_target_result_vars.get(v), Some(f) if f == flow_name))
                                                {
                                                    flow.transitions.iter().find(|t| {
                                                        t.name == *method_name && t.from_state == "Fault"
                                                    })
                                                } else {
                                                    None
                                                }
                                        });
                                            if let Some(t) = t {
                                                let from_state = t.from_state.clone();
                                                let to_states = t.to_states.clone();
                                                let fails = t.fails.clone();
                                                if let Some(to) = to_states.first() {
                                                    // 0.36.7 (裁决 1 跨 flow 补全,
                                                    // legacy leg): register the
                                                    // flow-qualified record name for
                                                    // the Fault sink so legacy field
                                                    // inference (`infer_object_type`)
                                                    // resolves `sf.last_state` against
                                                    // THIS flow's `flow::<name>::Fault`
                                                    // TypeDef (correct StateId/EventId
                                                    // field types), not the bare-name
                                                    // first-wins alias of another flow
                                                    // (wrong enum in native prints).
                                                    let var_ty = Self::transition_result_var_type(
                                                        flow_name, to,
                                                    );
                                                    self.var_type_names
                                                        .insert(name.clone(), var_ty);
                                                    self.track_flow_result_type(
                                                        name,
                                                        &from_state,
                                                        to,
                                                        fails,
                                                    );
                                                    // 0.36.10 (裁决 6 follow-up):
                                                    // declared-faultable multi-target
                                                    // result -> recover/reset-able.
                                                    if to_states.len() > 1 {
                                                        self.multi_target_result_vars.insert(
                                                            name.to_string(),
                                                            flow_name.to_string(),
                                                        );
                                                    }
                                                }
                                            }
                                        } else {
                                            // Q3: trait-impl method on a
                                            // non-flow receiver.
                                            let impl_ret = self.infer_impl_method_return_type(
                                                &obj_type,
                                                method_name,
                                            );
                                            if !impl_ret.is_empty() {
                                                register_qualified_var_type(
                                                    &mut self.var_type_names,
                                                    name,
                                                    impl_ret,
                                                );
                                            }
                                        }
                                    } else {
                                        // Q3: trait-impl method on a
                                        // non-flow/non-string receiver.
                                        let impl_ret = self
                                            .infer_impl_method_return_type(&obj_type, method_name);
                                        if !impl_ret.is_empty() {
                                            register_qualified_var_type(
                                                &mut self.var_type_names,
                                                name,
                                                impl_ret,
                                            );
                                        }
                                    }
                                }
                            } else if let Expr::Ident(func_name) = callee.unlocated() {
                                match func_name.as_str() {
                                    "keys" | "values" => {
                                        self.var_type_names
                                            .insert(name.clone(), "List<string>".to_string());
                                    }
                                    "Ok" => {
                                        let full = match call_args.first() {
                                            Some(arg) => {
                                                let inner = self.infer_object_type(arg, vars);
                                                if !inner.is_empty()
                                                    && self.type_defs.contains_key(&inner)
                                                {
                                                    format!("Result<{},i32>", inner)
                                                } else if !inner.is_empty() {
                                                    format!("Result<{},i32>", inner)
                                                } else {
                                                    "Result".to_string()
                                                }
                                            }
                                            None => "Result".to_string(),
                                        };
                                        self.var_type_names.insert(name.clone(), full);
                                    }
                                    "Err" => {
                                        self.var_type_names
                                            .insert(name.clone(), "Result".to_string());
                                    }
                                    "Some" => {
                                        // Prefer Option<Inner> when the payload type is known.
                                        let full = match call_args.first() {
                                            Some(arg) => {
                                                let inner = self.infer_object_type(arg, vars);
                                                if !inner.is_empty()
                                                    && inner != "Some"
                                                    && inner != "None"
                                                {
                                                    format!("Option<{}>", inner)
                                                } else {
                                                    "Option".to_string()
                                                }
                                            }
                                            None => "Option".to_string(),
                                        };
                                        self.var_type_names.insert(name.clone(), full);
                                    }
                                    "None" => {
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
                                                match ret_ty.unlocated() {
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
                                        } else if let Some(closure_ret) = self
                                            .var_types
                                            .get(func_name)
                                            .and_then(|t| match t.unlocated() {
                                                Type::Func(_, ret) => Some(ret.as_ref().clone()),
                                                _ => None,
                                            })
                                        {
                                            // Closure call: the callee is a let-bound
                                            // closure whose Func type was recorded at
                                            // its binding. Track the return type name
                                            // so field access / method dispatch on the
                                            // call result resolves correctly (otherwise
                                            // infer_object_type falls back to the
                                            // variable name and field access fails
                                            // E0707). Mirrors the named-func branch.
                                            if let Type::Name(tn, _) = closure_ret.unlocated() {
                                                let resolved =
                                                    self.substitute_type_params(&closure_ret);
                                                let type_name = self
                                                    .get_full_type_name(&resolved)
                                                    .unwrap_or_else(|| tn.clone());
                                                self.var_type_names.insert(name.clone(), type_name);
                                                self.var_types.insert(name.clone(), closure_ret);
                                            }
                                        } else if let Some(crate::ast::Type::Name(tn, _)) = self
                                            .extern_func_defs
                                            .get(func_name)
                                            .and_then(|ef| ef.ret.as_ref())
                                            .map(crate::ast::Type::unlocated)
                                        {
                                            self.var_type_names.insert(name.clone(), tn.clone());
                                        }
                                        // 0.35.11-fix: list-returning builtins
                                        // (map/filter/reverse/sort/range)
                                        // leave the binding untyped otherwise —
                                        // println(m) then puts the list struct
                                        // pointer as a C string. Derive the
                                        // display type from the source arg.
                                        if let Some(list_ty) = self.infer_list_builtin_return_type(
                                            func_name, call_args, vars,
                                        ) {
                                            self.var_type_names.insert(name.clone(), list_ty);
                                        }
                                        // G-41: Track return types for builtins and std
                                        // functions that return List<string>.
                                        match func_name.as_str() {
                                            "listdir" | "walk_dir" | "str_split" | "words"
                                            | "lines" | "split" | "sort_str" | "keys"
                                            | "values" => {
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
                                            "getenv" | "base64_decode" | "try_input_line" => {
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
                                            "map_new" => {
                                                self.var_type_names
                                                    .insert(name.clone(), "Map".to_string());
                                            }
                                            "map_set" | "map_remove" => {
                                                if let Some(val_arg) = call_args.get(2) {
                                                    let vt = self.infer_object_type(val_arg, vars);
                                                    if vt.starts_with('(')
                                                        || self.is_product_tuple_alias(&vt)
                                                    {
                                                        let resolved =
                                                            if self.is_product_tuple_alias(&vt) {
                                                                self.resolve_alias_type_name(&vt)
                                                            } else {
                                                                vt
                                                            };
                                                        self.var_type_names.insert(
                                                            name.clone(),
                                                            format!("Map<string, {}>", resolved),
                                                        );
                                                    } else if !Self::map_value_decodable_by_any(&vt)
                                                    {
                                                        // 0.39.136: narrow only for kinds the Any
                                                        // renderer cannot decode — see block.rs
                                                        // sibling sites.
                                                        self.var_type_names.insert(
                                                            name.clone(),
                                                            format!("Map<string, {}>", vt),
                                                        );
                                                    } else {
                                                        self.var_type_names.insert(
                                                            name.clone(),
                                                            "Map".to_string(),
                                                        );
                                                    }
                                                } else {
                                                    self.var_type_names
                                                        .insert(name.clone(), "Map".to_string());
                                                }
                                            }
                                            "set_new" | "set_insert" | "set_remove" => {
                                                self.var_type_names
                                                    .insert(name.clone(), "Set".to_string());
                                            }
                                            _ => {
                                                // Deep-eval 2026-08-09 (09_io_files
                                                // Result display): builtin
                                                // Result-returning calls
                                                // (read_file/input/write_file/…)
                                                // left the binding untyped —
                                                // infer_object_type fell back to
                                                // the variable name ("r1") and
                                                // println displayed the Result
                                                // struct as a bare tuple
                                                // `(true, "…", 0)`. Mirror the
                                                // compile_block builtin fallback.
                                                if crate::codegen::builtins::is_builtin(func_name) {
                                                    let obj_type =
                                                        self.infer_object_type(init, vars);
                                                    if !obj_type.is_empty()
                                                        && obj_type.as_str() != func_name.as_str()
                                                    {
                                                        self.var_type_names
                                                            .insert(name.clone(), obj_type);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        } else if let Expr::Turbofish(_func_name, turbo_type_args, _) =
                            init.unlocated()
                        {
                            if let Some(ta) = turbo_type_args.first() {
                                if let Type::Name(tn, args) = ta.unlocated() {
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
                                            self.var_type_names
                                                .insert(name.clone(), crate::core::fmt_type(ta));
                                        }
                                    } else {
                                        self.var_type_names.insert(name.clone(), tn.clone());
                                    }
                                }
                            }
                        }
                        // 0.35.23 deep-eval (mimi-make E0707): the
                        // top-level-body counterpart of the block.rs If/Block
                        // backfill — `let r0 = if n > 0 { parse_one(..) }
                        // else { Rule {..} }` in main's top level flows
                        // through THIS path (compile_func_body); without it
                        // `r0.target` fails E0707 because var_type_names
                        // never learns "Rule".
                        if !self.var_type_names.contains_key(name.as_str())
                            && matches!(init.unlocated(), Expr::If { .. } | Expr::Block(_))
                        {
                            let inferred = self.infer_object_type(init, vars);
                            if !inferred.is_empty() {
                                self.var_type_names.insert(name.clone(), inferred);
                            }
                        }
                        // Track list element type for nested List<List<T>> indexing
                        if let Some(decl_ty) = &ty {
                            self.register_list_elem_type(name, decl_ty);
                        }
                    }
                    // For tuple patterns, push the tuple type onto tuple_type_stack
                    // so that compile_pattern_bind can load the struct correctly
                    if let PatternKind::Tuple(sub_pats) = &pat.kind {
                        if !sub_pats.is_empty() {
                            // Try to infer tuple type from declared type or init expression
                            let tuple_ty = if let Some(Type::Tuple(elem_tys)) =
                                ty.as_ref().map(Type::unlocated)
                            {
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
                    // 0.36.10 (裁决 6 follow-up): aliasing a declared-faultable
                    // result var (`let x = failed`) keeps recover/reset-ability —
                    // the alias's slot holds the same __MultiTarget union, and
                    // the runtime tag dispatch reads it the same way. Runs for
                    // every variable-let; a no-op when the source is not a
                    // faultable result var.
                    if let PatternKind::Variable(alias_name) = &pat.kind {
                        if let Expr::Ident(src) = init.unlocated() {
                            if let Some(flow) = self.multi_target_result_vars.get(src).cloned() {
                                self.multi_target_result_vars
                                    .insert(alias_name.clone(), flow);
                            }
                        }
                    }
                    self.compile_pattern_bind(pat, val, vars)?;
                    // M2 (0.35.37): register capability variables AFTER
                    // pattern binding — the old check ran before
                    // compile_pattern_bind, vars.get(name) was always None,
                    // and `let c: cap X = ...` never entered cap_vars, so
                    // Stmt::Drop silently skipped the consume/release
                    // emission (CAP_TABLE leak, exactly-once broken).
                    if let PatternKind::Variable(name) = &pat.kind {
                        if let Some(Type::Cap(_) | Type::CapAtom(_)) =
                            ty.as_ref().map(Type::unlocated)
                        {
                            if let Some(&(cap_alloca, _)) = vars.get(name) {
                                self.register_cap(name, cap_alloca);
                            }
                        }
                    }
                    // Pop tuple type stack if we pushed it
                    if let PatternKind::Tuple(sub_pats) = &pat.kind {
                        if !sub_pats.is_empty() {
                            self.tuple_type_stack.pop();
                        }
                        if let Expr::Call(callee, _) = init.unlocated() {
                            if let Expr::Ident(func_name) = callee.unlocated() {
                                if func_name == "map_get" && sub_pats.len() == 2 {
                                    if let PatternKind::Variable(name) = &sub_pats[1].kind {
                                        self.var_type_names.insert(name.clone(), "any".to_string());
                                    }
                                }
                            }
                        }
                        // 0.1.8 Phase E: `let (ch0, ch1) = session_pair::<S>()`
                        // destructures two SessionChan endpoints. The runtime
                        // values are i64 handles, so without a type-name hint
                        // `ch.send` would be mistaken for a socket i64 handle.
                        let is_session_pair_init = match init.unlocated() {
                            Expr::Turbofish(n, _, _) => n == "session_pair",
                            Expr::Call(callee, _) => matches!(
                                callee.unlocated(),
                                Expr::Turbofish(n, _, _) if n == "session_pair"
                            ),
                            _ => false,
                        };
                        if is_session_pair_init {
                            for sub in sub_pats {
                                if let PatternKind::Variable(name) = &sub.kind {
                                    self.var_type_names
                                        .insert(name.clone(), "SessionChan".to_string());
                                }
                            }
                        }
                    }
                    if let PatternKind::Variable(name) = &pat.kind {
                        // 2026-08-06 (audit 1j): Set literals `{1, 2}` compile
                        // to an opaque i64 handle with no var_type_names entry,
                        // so `let s = {1, 2}; contains(s, x)` fell through to
                        // compile_contains ("expected a list"). Track the Set
                        // type name so the contains dispatch can route Set
                        // haystacks to mimi_set_contains. (audit 1l) Map
                        // literals get the same treatment so type_name(m)
                        // resolves instead of "unknown".
                        if matches!(init.unlocated(), Expr::SetLiteral(_)) {
                            self.var_type_names.insert(name.clone(), "Set".to_string());
                        }
                        if matches!(init.unlocated(), Expr::MapLiteral { .. }) {
                            self.var_type_names.insert(name.clone(), "Map".to_string());
                        }
                        // 0.1.8 Phase E: `let ch = session_open::<S>()` is also
                        // an i64 handle; register the SessionChan type name for
                        // method dispatch.
                        let is_session_open_init = match init.unlocated() {
                            Expr::Turbofish(n, _, _) => n == "session_open",
                            Expr::Call(callee, _) => matches!(
                                callee.unlocated(),
                                Expr::Turbofish(n, _, _) if n == "session_open"
                            ),
                            _ => false,
                        };
                        if is_session_open_init {
                            self.var_type_names
                                .insert(name.clone(), "SessionChan".to_string());
                        }
                        // 2026-08-06 (audit 1l): enum variant constructors
                        // (`let e = FileNotFound`) compile to a tagged
                        // i64/int value with no var_type_names entry —
                        // type_name(e) printed "unknown". Register the
                        // variant's owning type name.
                        if let Expr::Ident(variant_name) = init.unlocated() {
                            if let Some((owner, _)) = self.find_variant_owner(variant_name) {
                                self.var_type_names.insert(name.clone(), owner);
                            }
                        }
                        if let Expr::Ident(fn_name) = init.unlocated() {
                            if self.module.get_function(fn_name.as_str()).is_some() {
                                self.fn_ptr_var_names.insert(name.clone());
                                // 2026-08-06 (§7-#81): register the declared
                                // Func signature so fn-pointer calls recover
                                // the real return type (f64/struct) instead of
                                // hard-coded i64 — an i64 indirect call on an
                                // f64-returning callee read garbage from %rax.
                                if let Some(fdef) = self.func_defs.get(fn_name.as_str()) {
                                    let params: Vec<Type> =
                                        fdef.params.iter().map(|p| p.ty.clone()).collect();
                                    let ret = fdef.ret.clone().unwrap_or(Type::Infer);
                                    self.var_types
                                        .insert(name.clone(), Type::Func(params, Box::new(ret)));
                                }
                            }
                            if self.cap_type_names.contains(fn_name.as_str()) {
                                self.var_type_names.insert(name.clone(), fn_name.clone());
                                // 0.35.37 (exactly-once alignment): `let c =
                                // FileReadCap` (no type annotation) registers
                                // ONLY in var_type_names — never in cap_vars —
                                // so Stmt::Drop silently skipped the release
                                // and call-argument consumption had nothing to
                                // consume. Register the alloca now that
                                // compile_pattern_bind has run (vars holds it).
                                if let Some(&(cap_alloca, _)) = vars.get(name) {
                                    self.register_cap(name, cap_alloca);
                                }
                            }
                        }
                        // 0.35.14 (DX backlog #18): tuple fn-element extraction.
                        self.record_tuple_fn_elems(name, init);
                        self.register_tuple_index_fn_binding(name, init);

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
                        // Normalize i64 booleans (builtin predicates) to i1.
                        // H-22: zero constant at the condition's own width
                        // (icmp operands must match; hard-coded i64 zero vs a
                        // narrower int is invalid IR).
                        if iv.get_type().get_bit_width() == 1 {
                            iv
                        } else {
                            self.builder
                                .build_int_compare(
                                    inkwell::IntPredicate::NE,
                                    iv,
                                    iv.get_type().const_int(0, false),
                                    "cond_bool",
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("cond normalize: {}", e))
                                })?
                        }
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
                    let mut then_val = self.compile_block_last_val(then_, &mut then_vars)?;
                    // String literals produce raw C pointers; wrap so the merge
                    // phi and return see the canonical {ptr, i64} struct.
                    then_val = self.normalize_block_last_string(then_val, then_)?;
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
                        let mut v = self.compile_block_last_val(else_block, &mut else_vars)?;
                        v = self.normalize_block_last_string(v, else_block)?;
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
                    if let Expr::Ident(name) = expr.unlocated() {
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
                Stmt::Defer(block) => {
                    // 0.31.24: Register defer block for LIFO execution on scope exit
                    self.register_defer(block);
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
                    // Register compensation block for LIFO execution on error
                    // exit. 0.34.36 (cross-agent contract): registration at the
                    // statement's execution point (inline, no block pre-scan) —
                    // compensation fires only for faults after this statement.
                    self.register_comp(block);
                }
                Stmt::Arena(block) => {
                    // H-8: tail arena wrapper contributes the implicit value.
                    if is_tail {
                        let inner_vars = &mut vars.clone();
                        last_val = self.compile_block_last_val(block, inner_vars)?;
                        last_val = self.adjust_int_val(last_val, ret_type)?;
                        last_val = self.coerce_variant_value(last_val, ret_type, ret_ty_ast)?;
                        vars.extend(std::mem::take(inner_vars));
                    } else {
                        self.compile_arena_block(block, vars, "arena")?;
                    }
                }
                Stmt::Unsafe(block) => {
                    // H-8: tail unsafe wrapper contributes the implicit value.
                    if is_tail {
                        let inner_vars = &mut vars.clone();
                        last_val = self.compile_block_last_val(block, inner_vars)?;
                        last_val = self.adjust_int_val(last_val, ret_type)?;
                        last_val = self.coerce_variant_value(last_val, ret_type, ret_ty_ast)?;
                        vars.extend(std::mem::take(inner_vars));
                    } else {
                        // Unsafe: execute block (no restrictions in codegen)
                        self.compile_block(block, vars)?;
                    }
                }
                Stmt::IeeeFloat(block) => {
                    // v0.34.10a (SD-9): suspend finiteness trap inside.
                    self.ieee_depth += 1;
                    if is_tail {
                        let inner_vars = &mut vars.clone();
                        let r = self.compile_block_last_val(block, inner_vars);
                        self.ieee_depth -= 1;
                        let val = r?;
                        last_val = self.adjust_int_val(val, ret_type)?;
                        last_val = self.coerce_variant_value(last_val, ret_type, ret_ty_ast)?;
                        vars.extend(std::mem::take(inner_vars));
                    } else {
                        let r = self.compile_block(block, vars);
                        self.ieee_depth -= 1;
                        r?;
                    }
                }
                Stmt::Func(f) => {
                    if f.is_comptime {
                        // Comptime functions: skip codegen (interpreter-only)
                    } else {
                        // I-H13: nested func with free vars → closure capture
                        // (same ABI as lambda). Capture-free nested funcs still
                        // use a standalone LLVM function for dual-backend parity.
                        self.compile_nested_func_stmt(f, vars)?;
                    }
                }
                Stmt::Requires(..)
                | Stmt::Ensures(..)
                | Stmt::Invariant(..)
                | Stmt::Math(_)
                | Stmt::Ellipsis => {
                    // Skip contract-related statements in codegen
                }
                Stmt::Block(block) => {
                    // H-8: a tail bare block contributes the implicit return
                    // value; statement-position blocks discard it.
                    if is_tail {
                        let inner_vars = &mut vars.clone();
                        last_val = self.compile_block_last_val(block, inner_vars)?;
                        last_val = self.adjust_int_val(last_val, ret_type)?;
                        last_val = self.coerce_variant_value(last_val, ret_type, ret_ty_ast)?;
                        vars.extend(std::mem::take(inner_vars));
                    } else {
                        self.compile_block(block, vars)?;
                    }
                }
                Stmt::Pinned { expr, var, body } => {
                    // v0.34.3: synchronous pinned timeout abolished (clause 10);
                    // only the pin + body remain.
                    let val = self.compile_expr(expr, vars)?;
                    if let Some(v) = var {
                        let ty = val.get_type();
                        let alloca = self.build_alloca(ty, v)?;
                        self.build_store(alloca, val)?;
                        vars.insert(v.clone(), (alloca, ty));
                    }
                    self.compile_block(body, vars)?;
                }
                Stmt::IfLet {
                    pat,
                    init,
                    then_,
                    else_,
                } => {
                    // C2 (audit-syntax): desugar to match (see compile_if_let_stmt).
                    self.compile_if_let_stmt(pat, init, then_, else_, vars)?;
                }
                _ => {}
            }
        }
        Ok(ControlFlow::Continue(last_val))
    }

    /// 0.36.49 (Phase C): mirror of `simple.rs::collect_arg_cap_places` for
    /// returned expressions. Every capability variable reachable through a
    /// returned value is being moved out of this function; the legacy emitter
    /// must mark those places consumed to match the checker's transfer-on-return
    /// semantics.
    fn collect_expr_cap_places(
        expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
        out: &mut Vec<String>,
    ) {
        match expr.unlocated() {
            Expr::Ident(name) => {
                if vars.contains_key(name) {
                    out.push(name.clone());
                }
            }
            Expr::NamedArg(_, value) => Self::collect_expr_cap_places(value, vars, out),
            Expr::Tuple(values) => {
                for v in values {
                    Self::collect_expr_cap_places(v, vars, out);
                }
            }
            Expr::List(values) => {
                for v in values {
                    Self::collect_expr_cap_places(v, vars, out);
                }
            }
            Expr::SetLiteral(values) => {
                for v in values {
                    Self::collect_expr_cap_places(v, vars, out);
                }
            }
            Expr::Record { fields, .. } => {
                for field in fields {
                    Self::collect_expr_cap_places(&field.value, vars, out);
                }
            }
            Expr::Field(obj, _) => Self::collect_expr_cap_places(obj, vars, out),
            Expr::Index(base, index) => {
                Self::collect_expr_cap_places(base, vars, out);
                Self::collect_expr_cap_places(index, vars, out);
            }
            _ => {}
        }
    }

    /// Emit the implicit return at the end of a function: check for unconsumed
    /// capabilities, convert pointer-to-struct returns, clean up scopes, verify
    /// postconditions, and build the final return instruction.
    fn emit_implicit_return(
        &mut self,
        ret_type: BasicTypeEnum<'ctx>,
        ret_ty_ast: Option<&Type>,
        last_val: BasicValueEnum<'ctx>,
        _func_name: &str,
        vars: &HashMap<String, VarEntry<'ctx>>,
        expr: Option<&Expr>,
    ) -> MimiResult<()> {
        // 0.36.49 (Phase C): a capability reached via the implicit tail
        // expression is transferred to the caller, not leaked. Mark it
        // consumed so the legacy scope check does not demand an extra drop;
        // no runtime cap_consume is emitted here because ownership of the
        // returned handle moves out of this function.
        if let Some(expr) = expr {
            let mut returned_caps = Vec::new();
            Self::collect_expr_cap_places(expr, vars, &mut returned_caps);
            for name in returned_caps {
                if self.is_cap_var(&name) && !self.is_cap_consumed(&name) {
                    self.consume_cap(&name)?;
                }
            }
        }

        // Check for unconsumed capabilities before returning
        self.check_unconsumed_caps()?;

        // Transfer ownership of string return values before the heap cleanup below
        // frees local temporaries.
        let last_val = self.claim_string_return_value(last_val, ret_type, expr, vars)?;

        // Deep-eval 2026-08-09 (demos/07 custom Res segv): mirror emit_return's
        // L6 claim — a custom-enum-shaped return ({i32, i64}) may carry a
        // boxed payload that the heap cleanup below would otherwise free
        // before the ret, leaving the caller reading freed memory.
        self.claim_returned_enum_box(last_val, ret_type)?;

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
                                self.build_store(fp, null_ptr)?;
                            }
                        }
                    }
                    loaded
                }
            }
            _ => last_val,
        };
        let last_val = self.coerce_variant_value(last_val, ret_type, ret_ty_ast)?;

        // 0.35.20 (#6): claim returned List variables' data buffers — null out
        // the variable slot's data field AFTER the return value has been loaded
        // above, so the returned struct keeps the live pointer while the
        // scope-exit free below turns into free(null) (ownership transfers to
        // the caller). Placing this before the load returned a null data
        // pointer (empty-list display for chunks).
        self.claim_returned_lists(expr, vars);
        // 0.35.20 (#6): List *literals* escaping the return get a deep-copied
        // buffer (no named slot to null) — must run before the flush below.
        let last_val = self.claim_returned_list_literals(last_val, expr)?;

        // Pop scopes (discard compensations on normal exit)
        // A function owns exactly one shared-release frame. Popping only that
        // frame preserves the caller's registrations when codegen recursively
        // monomorphizes a callee while the caller is still being emitted.
        self.pop_shared_scope()?;
        self.flush_heap_scopes_to_boundary()?;
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
                        super::scope::ContractPhase::Ensures,
                    )?;
                }
            }
        }
        // 0.34.34 (O1-default prerequisite): the trailing return must NOT be
        // emitted when the body already terminated. Pre-fix this appended a
        // second terminator to the already-terminated block. Single-target
        // transitions survived because the stray ret was type-compatible
        // (dead, removed by the backend); multi-target transitions return
        // {i32 tag, i64 payload} while the stray value is an i64 — invalid
        // IR that O0 tolerated by luck and O1 turns into an LLVM abort
        // ("Cannot emit physreg copy instruction").
        if !self.block_has_terminator() {
            let mut last_val = self.adjust_int_val(last_val, ret_type)?;
            let last_val = self.load_return_value_if_needed(last_val)?;
            // GENERIC-RET-ALIGN: aggregate returns must match the declared
            // signature layout too (tuple slot widths) — see the explicit
            // return path for the full rationale.
            let mut last_val = last_val;
            if last_val.get_type() != ret_type {
                if let (BasicValueEnum::StructValue(sv), BasicTypeEnum::StructType(st)) =
                    (last_val, ret_type)
                {
                    last_val = self.align_struct_return(sv, st)?;
                }
            }
            self.build_return(Some(&last_val))?;
        }
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
        let effective_ret_override =
            if let Some(Type::ImplTrait(_)) = func.ret.as_ref().map(Type::unlocated) {
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
            if let Some(ty) = self.legacy_param_llvm_type(func, param) {
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

        // 0.35.23 deep-eval: the native entry `main` receives the C
        // (argc, argv) pair so the generated executable can seed the runtime
        // CLI args (mimi_args_init) — args()/cli_args() were EMPTY in every
        // `mimi build` binary before this (mimi-log, mimi-lint, mimi-kv,
        // mimichat all read CLI args; the VM got them via with_cli_args).
        // argc is declared i32 to match the C main ABI (SysV passes argc in
        // edi — reading a declared i64 would eat garbage high bits).
        let fn_type = if func.name == "main" {
            let main_params = [
                BasicMetadataTypeEnum::IntType(self.context.i32_type()),
                BasicMetadataTypeEnum::PointerType(self.context.ptr_type(AddressSpace::default())),
            ];
            match ret_type {
                BasicTypeEnum::IntType(t) => t.fn_type(&main_params, false),
                BasicTypeEnum::FloatType(t) => t.fn_type(&main_params, false),
                BasicTypeEnum::PointerType(t) => t.fn_type(&main_params, false),
                BasicTypeEnum::StructType(t) => t.fn_type(&main_params, false),
                BasicTypeEnum::ArrayType(t) => t.fn_type(&main_params, false),
                _ => self.context.i64_type().fn_type(&main_params, false),
            }
        } else {
            match ret_type {
                BasicTypeEnum::IntType(t) => t.fn_type(&metadata_params, false),
                BasicTypeEnum::FloatType(t) => t.fn_type(&metadata_params, false),
                BasicTypeEnum::PointerType(t) => t.fn_type(&metadata_params, false),
                BasicTypeEnum::StructType(t) => t.fn_type(&metadata_params, false),
                BasicTypeEnum::ArrayType(t) => t.fn_type(&metadata_params, false),
                _ => self.context.i64_type().fn_type(&metadata_params, false),
            }
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

    /// Legacy surface-AST function body compiler (fifth pass).
    ///
    /// Called by `compile_file_inner` for functions NOT compiled by the resolved
    /// native emitter. The skip guard (`count_basic_blocks != 0`) prevents
    /// double-emission of functions already handled by the resolved emitter.
    ///
    /// Permanent ineligible body classes: capturing lambdas, generics,
    /// async, extern ABI wrappers, view/mutate borrow params (non-self).
    pub(super) fn compile_func_legacy(&mut self, func: &FuncDef) -> MimiResult<()> {
        // V-11 (audit 2026-08-05) frame guard: nested-function shadows
        // registered while compiling this body (bare-name directory swap +
        // call redirect) must stay live through the WHOLE body — mirroring
        // the checker's deferred restores — but must not leak into
        // subsequently compiled functions. Snapshot on entry, restore on
        // every exit (including `?` error propagation).
        let saved_shadows = self.nested_shadow_symbols.clone();
        let saved_current_fn = std::mem::replace(&mut self.current_legacy_fn, func.name.clone());
        let result = self.compile_func_legacy_inner(func);
        self.restore_nested_shadow_frame(saved_shadows, saved_current_fn);
        result
    }

    /// Undo the shadows registered by one compile_func_legacy frame.
    /// Entries inherited from the enclosing frame (identical mangled symbol
    /// in the saved snapshot) are kept untouched; entries (re)registered by
    /// this frame restore the func_defs entry they displaced.
    fn restore_nested_shadow_frame(
        &mut self,
        saved_shadows: HashMap<String, (String, Option<FuncDef>)>,
        saved_current_fn: String,
    ) {
        for (name, (mangled, prior)) in &self.nested_shadow_symbols {
            let re_registered = saved_shadows
                .get(name)
                .map(|(saved_mangled, _)| saved_mangled != mangled)
                .unwrap_or(true);
            if re_registered {
                match prior {
                    Some(def) => {
                        self.func_defs.insert(name.clone(), def.clone());
                    }
                    None => {
                        self.func_defs.remove(name);
                    }
                }
            }
        }
        self.nested_shadow_symbols = saved_shadows;
        self.current_legacy_fn = saved_current_fn;
    }

    fn compile_func_legacy_inner(&mut self, func: &FuncDef) -> MimiResult<()> {
        // 0.35.24 (deep-eval): snapshot the caller's per-function variable type
        // tracking before the fresh-start clears below. When a caller body
        // (e.g. a generic `f`) is being emitted, its monomorphized callee
        // instances are compiled NESTED inside it (call-site instantiation);
        // without this snapshot the callee's clears wiped the caller's
        // registrations mid-body — `claim_returned_lists` then silently
        // skipped List vars after the nested call (`is_list_var` → None),
        // freeing an escaping buffer (latent use-after-free on `return xs`).
        let saved_var_types = std::mem::take(&mut self.var_types);
        let saved_var_type_names = std::mem::take(&mut self.var_type_names);
        let saved_list_elem_llvm_types = std::mem::take(&mut self.list_elem_llvm_types);
        let saved_type_map = std::mem::take(&mut self.type_map);
        let result = self.compile_func_legacy_clean(func);
        self.var_types = saved_var_types;
        self.var_type_names = saved_var_type_names;
        self.list_elem_llvm_types = saved_list_elem_llvm_types;
        self.type_map = saved_type_map;
        result
    }

    fn compile_func_legacy_clean(&mut self, func: &FuncDef) -> MimiResult<()> {
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
                self.compile_func_legacy(&body_func)?;
            }
            return self.compile_export_wrapper(func, &body_name);
        }

        let (function, ret_type) = self.declare_func(func)?;
        // Skip functions already compiled by the resolved native emitter,
        // unless they failed emission — those need recompilation.
        // 0.34.42: key the failed-set lookup on the ACTUAL LLVM symbol name.
        // The resolved emitter records failures by catalog qualified_name
        // (e.g. `string_char_at`), but legacy reaches this point with the
        // surface func.name (`char_at`) while declare_func returns the
        // mangled LLVM function. The old func.name-only lookup missed the
        // mismatch, left the partial stub untouched (count_basic_blocks may
        // even be 0 after bind_parameters-only emission), and the invalid
        // terminator-less function segfaulted LLVM's pass pipeline.
        let llvm_symbol = function.get_name().to_string_lossy().into_owned();
        if function.count_basic_blocks() != 0 {
            if self.resolved_failed_functions.contains(&func.name)
                || self.resolved_failed_functions.contains(&llvm_symbol)
            {
                // Delete all basic blocks from the function, keeping the
                // declaration intact. The legacy emitter will recompile the
                // body from scratch. We cannot delete-and-redeclare because
                // callers compiled by the resolved emitter hold LLVM value
                // references to this function, and deleting it would leave
                // dangling pointers.
                if std::env::var("MIMI_VERBOSE").is_ok() {
                    eprintln!(
                        "warning: function '{}' has partial blocks from the resolved \
                         emitter — clearing body and recompiling",
                        func.name
                    );
                }
                // Use inkwell's delete() to remove all blocks from the
                // function while keeping the declaration alive (so callers
                // compiled by the resolved emitter still have a valid
                // reference). We iterate get_basic_blocks() which returns a
                // snapshot — deleting the first block repeatedly until no
                // blocks remain.
                // H11 (0.35.37): the old loop did `let _ = bb.delete()` —
                // inkwell's delete returns Result<(), ()>, and an Err meant
                // the block stayed in the function while the loop kept
                // spinning on count_basic_blocks() > 0: a compiler hang.
                // Fail closed instead: report the deletion error and abort
                // the build, and guard against the (impossible-in-practice)
                // count>0-but-no-first-block divergence.
                // Use inkwell's delete() to remove all blocks from the
                // function while keeping the declaration alive (so callers
                // compiled by the resolved emitter still have a valid
                // reference).
                // H11 (0.35.37): the old loop did `let _ = bb.delete()` —
                // inkwell's delete returns Result<(), ()>, and an Err meant
                // the block stayed in the function while the loop kept
                // spinning on count_basic_blocks() > 0: a compiler hang.
                // Fail closed instead: report the deletion error and abort
                // the build, and guard against the (impossible-in-practice)
                // count>0-but-no-first-block divergence.
                //
                // 0.39.x stdlib matrix sweep (valgrind-pinned): one-at-a-time
                // `bb.delete()` left predecessor branches pointing at freed
                // blocks — destroying block N ran User destructors that wrote
                // into use-lists of blocks freed at N-1. That heap corruption
                // surfaced later as nondeterministic SIGSEGVs inside random
                // LLVM passes. Two-phase teardown, mirroring clear_partial_body
                // in codegen/resolved/mod.rs: erase all instructions first
                // (drops every cross-block edge), then delete empty blocks.
                // 0.39.x matrix sweep: delegates to the shared three-pass
                // teardown (valgrind-pinned; see clear_partial_body above).
                {
                    self.clear_partial_body(function);
                    if function.count_basic_blocks() > 0 {
                        return Err(CompileError::LlvmError(format!(
                            "H11: function '{}' still has {} basic blocks after the shared \
                             body clear — refusing to continue with a partial body",
                            func.name,
                            function.count_basic_blocks()
                        )));
                    }
                }
            } else {
                return Ok(()); // successfully compiled by resolved emitter
            }
        }
        // Set calling convention for extern "C" / extern "stdcall" etc.
        if let Some(ref abi) = func.extern_abi {
            let cc = crate::ffi::abi_to_llvm_call_conv(abi);
            function.set_call_conventions(cc);
        }
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        // 0.35.23 deep-eval: native entry — seed the runtime CLI args so
        // args()/cli_args() work in the generated executable. The declare_func
        // main signature carries (argc: i32, argv: ptr); pass argc as i32 to
        // the runtime's mimi_args_init(i32, ptr).
        if func.name == "main" {
            if let Some(args_init_fn) = self.module.get_function("mimi_args_init") {
                if let (Some(argc), Some(argv)) =
                    (function.get_nth_param(0), function.get_nth_param(1))
                {
                    let argc_i32 = argc.into_int_value();
                    self.build_call(
                        args_init_fn,
                        &[argc_i32.into(), argv.into()],
                        "mimi_args_init",
                    )?;
                }
            }
        }

        // v0.29.24: apply @max_children(N) process quota when compiling main.
        if func.name == "main" {
            if let Some(max) = self.max_children {
                if let Ok(set_fn) = self.get_runtime_fn("mimi_actor_set_max_children") {
                    let n = self.context.i64_type().const_int(max as u64, false);
                    self.build_call(set_fn, &[n.into()], "set_max_children")?;
                }
            }
        }

        // Push scopes for function body
        self.push_cap_scope();
        self.push_comp_scope();
        self.push_defer_scope();
        self.begin_function_heap_scope();
        self.push_shared_scope();

        let mut vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
        self.bind_func_params(func, function, &mut vars)?;

        // H4 (audit-codegen): inside a fallible multi-target transition, the
        // `self` parameter IS the from-state payload. Capture its slot so the
        // panic→Fault absorption epilogue can shadow persistent draft field
        // values into the Fault record (interp parity). The H2 lambda/nested-func
        // guards clear in_multi_target_transition before compiling standalone
        // functions, so this guard never fires outside the real transition body.
        if self.in_fallible_multi_target() {
            if let Some(&(slot, ty)) = vars.get("self") {
                self.fault_self_entry = Some((slot, ty));
            }
        }

        // Prepare and compile function contracts.
        self.prepare_func_contracts(func, &vars)?;
        self.snapshot_old_values(&vars)?;

        let ret_ty_ast = func.ret.as_ref();
        self.current_fn_ret_ty_ast = func.ret.clone();
        // Deep-eval 2026-08-09 (std/fs read_lines E0200): legacy Err
        // constructors inside a Result-returning function pad the Ok slot
        // with the Ok value-shape zero (consume in compile_err_constructor).
        // Without this, `match` arms constructing the same Result type split
        // layouts (`Ok(list)` → {i1,ptr,i64} vs `Err(str)` → {i1,i64,i64})
        // and the phi unification rejects the match.
        let saved_pending_result_ok_ty = self.pending_result_ok_ty.take();
        if let Some(ok_ty) = func.ret.as_ref().and_then(|r| match r.unlocated() {
            crate::ast::Type::Result(ok, _) => Some((**ok).clone()),
            crate::ast::Type::Name(n, args) if n == "Result" && args.len() == 2 => {
                Some(args[0].clone())
            }
            _ => None,
        }) {
            self.pending_result_ok_ty = Some(ok_ty);
        }
        let last_expr = func.body.last().and_then(|s| match s.unlocated() {
            Stmt::Expr(e) => Some(e),
            _ => None,
        });
        match self.compile_func_body(func, ret_type, &mut vars)? {
            ControlFlow::Break(()) => {
                // Early return: the return statement's flush already released
                // all heap scopes down to this function's boundary. Only the
                // boundary marker remains to be popped.
                self.end_function_heap_scope();
                self.pending_result_ok_ty = saved_pending_result_ok_ty;
                return Ok(());
            }
            ControlFlow::Continue(last_val) => {
                // Normal exit: execute defer blocks before implicit return
                self.pop_defer_scope(&mut vars)?;
                self.emit_implicit_return(
                    ret_type, ret_ty_ast, last_val, &func.name, &vars, last_expr,
                )?;
            }
        }
        self.pending_result_ok_ty = saved_pending_result_ok_ty;

        self.end_function_heap_scope();

        Ok(())
    }

    /// Shared three-pass teardown of a partially-emitted function body
    /// (0.39.x stdlib matrix sweep, valgrind-pinned). Deleting blocks or
    /// instructions in appearance order corrupts the heap: LLVM destruction
    /// is use-before-def, and loop back-edges put defs (latch) after their
    /// users (header phis). Safe sequence:
    ///   pass 1 — erase every PHI and every terminator (kills all cross-block
    ///            edges in both directions);
    ///   pass 2 — erase remaining instructions in REVERSE order (SSA dominance
    ///            guarantees users die before defs);
    ///   pass 3 — delete the now-empty, now-unreferenced blocks.
    pub(crate) fn clear_partial_body(&self, function: inkwell::values::FunctionValue<'ctx>) {
        let blocks = function.get_basic_blocks();
        for bb in &blocks {
            while let Some(instruction) = bb.get_first_instruction() {
                if instruction.get_opcode() == inkwell::values::InstructionOpcode::Phi {
                    instruction.erase_from_basic_block();
                } else {
                    break;
                }
            }
            if let Some(terminator) = bb.get_terminator() {
                terminator.erase_from_basic_block();
            }
        }
        for bb in &blocks {
            while let Some(instruction) = bb.get_last_instruction() {
                instruction.erase_from_basic_block();
            }
        }
        for bb in blocks {
            // SAFETY: blocks are empty and unreferenced after the two erase
            // passes; delete() removes them from the function and their
            // addresses are not used afterwards.
            unsafe {
                let _ = bb.delete();
            }
        }
    }

    /// Compile a generic function with concrete type arguments (monomorphization).
    ///
    /// 0.39.x matrix sweep: wraps the real work so that EVERY error exit — not
    /// only the tail — tears the partially-emitted instance body down before
    /// propagating. A half-built body left in the module poisons the LLVM pass
    /// pipeline (nondeterministic SIGSEGVs downstream).
    pub(super) fn compile_generic_func(
        &mut self,
        func: &FuncDef,
        type_map: &HashMap<String, crate::ast::Type>,
    ) -> MimiResult<()> {
        let mangled = Self::mangle_name(&func.name, type_map);
        match self.compile_generic_func_inner(func, type_map) {
            Ok(()) => Ok(()),
            Err(e) => {
                if let Some(function) = self.module.get_function(&mangled) {
                    let missing_terminators = function
                        .get_basic_blocks()
                        .iter()
                        .any(|bb| bb.get_terminator().is_none());
                    if missing_terminators {
                        // The declaration itself stays (callers may already hold
                        // references); only the poisoned body is removed.
                        self.clear_partial_body(function);
                    }
                }
                Err(e)
            }
        }
    }

    fn compile_generic_func_inner(
        &mut self,
        func: &FuncDef,
        type_map: &HashMap<String, crate::ast::Type>,
    ) -> MimiResult<()> {
        // 0.35.24 (deep-eval): this is the monomorphized-instance entry — a
        // callee instance is compiled NESTED inside the caller's body. The
        // fresh-start clears below must not destroy the caller's per-function
        // variable type tracking: without this snapshot, `claim_returned_lists`
        // silently skipped List vars after the nested call (`is_list_var` →
        // None) and the scope-exit free freed an escaping buffer (latent
        // use-after-free on `return xs`).
        let saved_var_types = std::mem::take(&mut self.var_types);
        let saved_var_type_names = std::mem::take(&mut self.var_type_names);
        let saved_list_elem_llvm_types = std::mem::take(&mut self.list_elem_llvm_types);
        // GENERIC-SHADOW-MONO-001: publish the definition under instantiation.
        let saved_current_generic_def = self.current_generic_def.take();
        self.current_generic_def = Some(func.clone());

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
        let effective_ret_override =
            if let Some(Type::ImplTrait(_)) = func.ret.as_ref().map(Type::unlocated) {
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
        self.begin_function_heap_scope();
        self.push_shared_scope();

        let mut vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
        self.bind_func_params(func, function, &mut vars)?;

        // 0.39.x matrix sweep (RESULT-MAPERR-ABI-001): register instantiated
        // PARAMETER types so method dispatch inside the instance sees the
        // concrete receiver shape ("Result<i32, string>", not bare "Result").
        // Without this, `result.map_err(f)` in std/result.mimi compiled with
        // the Err payload treated as a scalar i64 while the actual slot holds
        // a heap-boxed pointer for non-scalar E — the generated program
        // dereferenced the length as a pointer and crashed. (The map_err call
        // site additionally recovers E from the lambda's own annotation; this
        // registration covers every other combinator face.)
        for param in &func.params {
            let substituted = self.resolve_type(&param.ty);
            let ty_name = crate::core::fmt_type(&substituted);
            if ty_name.starts_with("Result<")
                || ty_name.starts_with("Option<")
                || ty_name.starts_with("List<")
                || ty_name.starts_with("Set<")
                || ty_name.starts_with("Map<")
            {
                self.var_type_names.insert(param.name.clone(), ty_name);
            }
        }

        // Prepare and compile function contracts.
        self.prepare_func_contracts(func, &vars)?;
        self.snapshot_old_values(&vars)?;

        let ret_ty_ast = func.ret.as_ref();
        self.current_fn_ret_ty_ast = func.ret.clone();
        let last_expr = func.body.last().and_then(|s| match s.unlocated() {
            Stmt::Expr(e) => Some(e),
            _ => None,
        });
        let last_val = self.compile_block_last_val(&func.body, &mut vars)?;

        self.emit_implicit_return(ret_type, ret_ty_ast, last_val, &func.name, &vars, last_expr)?;
        self.end_function_heap_scope();
        // 0.39.x stdlib matrix sweep (nondeterministic-SIGSEGV root cause #2,
        // valgrind/IR-diff pinned): some body shapes can finish "successfully"
        // without a terminator in the entry block (e.g. a tail expression that
        // silently emits nothing). A terminator-less function body poisons the
        // module — LLVM's pass pipeline dereferences garbage on it later
        // (LowerExpectIntrinsic crashed with a null instruction pointer), and
        // whether the poison exists at all depended on HashMap-ordered
        // emission. Fail closed instead: restore state, tear the partial body
        // down with the shared three-pass clear, and surface the error.
        let mangled_check = Self::mangle_name(&func.name, type_map);
        if let Some(function) = self.module.get_function(&mangled_check) {
            let missing_terminators = function
                .get_basic_blocks()
                .iter()
                .any(|bb| bb.get_terminator().is_none());
            if missing_terminators || function.count_basic_blocks() == 0 {
                self.clear_partial_body(function);
                self.var_types = saved_var_types;
                self.var_type_names = saved_var_type_names;
                self.list_elem_llvm_types = saved_list_elem_llvm_types;
                self.current_generic_def = saved_current_generic_def;
                self.type_map = prev_type_map;
                if let Some(bb) = saved_block {
                    self.builder.position_at_end(bb);
                }
                return Err(CompileError::Generic(format!(
                    "monomorphized instance '{}' was emitted without a terminating \
                     instruction — refusing to keep a poisoned body in the module",
                    mangled_check
                )));
            }
        }
        // Restore the caller's per-function variable type tracking (see the
        // snapshot comment at the top of the function).
        self.var_types = saved_var_types;
        self.var_type_names = saved_var_type_names;
        self.list_elem_llvm_types = saved_list_elem_llvm_types;
        self.current_generic_def = saved_current_generic_def;
        self.type_map = prev_type_map;
        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }
        Ok(())
    }
}
