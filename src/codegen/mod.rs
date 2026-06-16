#![allow(dead_code, deprecated)]

pub mod types;
pub mod builtins;

use crate::ast::*;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::targets::{CodeModel, InitializationConfig, RelocMode, Target, TargetMachine};
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use inkwell::OptimizationLevel;
use std::collections::HashMap;
use std::path::Path;

pub struct CodeGenerator<'ctx> {
    pub context: &'ctx Context,
    pub module: Module<'ctx>,
    pub builder: Builder<'ctx>,
    loop_break: Option<inkwell::basic_block::BasicBlock<'ctx>>,
    loop_continue: Option<inkwell::basic_block::BasicBlock<'ctx>>,
    type_defs: HashMap<String, crate::ast::TypeDef>,
    type_llvm: HashMap<String, BasicTypeEnum<'ctx>>,
}

type VarEntry<'ctx> = (inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>);

impl<'ctx> CodeGenerator<'ctx> {
    pub fn new(context: &'ctx Context, module_name: &str) -> Self {
        let module = context.create_module(module_name);
        let builder = context.create_builder();
        builtins::register_runtime(&module, context);
        Self { context, module, builder, loop_break: None, loop_continue: None, type_defs: HashMap::new(), type_llvm: HashMap::new() }
    }

    pub fn compile_file(&mut self, file: &File) -> Result<(), String> {
        // First pass: collect type definitions
        for item in &file.items {
            match item {
                Item::Type(t) => {
                    self.register_type_def(t)?;
                }
                Item::Module(m) => {
                    for inner in &m.items {
                        if let Item::Type(t) = inner {
                            self.register_type_def(t)?;
                        }
                    }
                }
                _ => {}
            }
        }
        // Second pass: register extern functions and compile user functions
        for item in &file.items {
            match item {
                Item::ExternBlock(block) => {
                    self.register_extern_block(block)?;
                }
                Item::Func(f) if !f.is_comptime => self.compile_func(f)?,
                Item::Module(m) => {
                    for inner in &m.items {
                        match inner {
                            Item::ExternBlock(block) => {
                                self.register_extern_block(block)?;
                            }
                            Item::Func(f) if !f.is_comptime => self.compile_func(f)?,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn register_extern_block(&mut self, block: &crate::ast::ExternBlock) -> Result<(), String> {
        for ef in &block.funcs {
            let mut param_tys = Vec::new();
            for p in &ef.params {
                let ty = types::mimi_type_to_llvm(self.context, &p.ty)
                    .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                param_tys.push(types::basic_to_metadata(self.context, ty));
            }
            let ret_ty = match &ef.ret {
                Some(ty) => types::mimi_type_to_llvm(self.context, ty)
                    .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type())),
                None => BasicTypeEnum::IntType(self.context.i64_type()),
            };
            let fn_type = match ret_ty {
                BasicTypeEnum::IntType(t) => t.fn_type(&param_tys, false),
                BasicTypeEnum::FloatType(t) => t.fn_type(&param_tys, false),
                BasicTypeEnum::PointerType(t) => t.fn_type(&param_tys, false),
                BasicTypeEnum::StructType(t) => t.fn_type(&param_tys, false),
                BasicTypeEnum::ArrayType(t) => t.fn_type(&param_tys, false),
                _ => self.context.i64_type().fn_type(&param_tys, false),
            };
            self.module.add_function(&ef.name, fn_type, Some(inkwell::module::Linkage::External));
        }
        Ok(())
    }

    fn register_type_def(&mut self, t: &crate::ast::TypeDef) -> Result<(), String> {
        let llvm_ty = match &t.kind {
            crate::ast::TypeDefKind::Record(fields) => {
                let mut field_tys = Vec::new();
                for f in fields {
                    let ty = types::mimi_type_to_llvm(self.context, &f.ty)
                        .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                    field_tys.push(ty);
                }
                BasicTypeEnum::StructType(self.context.struct_type(&field_tys, false))
            }
            crate::ast::TypeDefKind::Enum(_variants) => {
                // Enum representation: i32 tag + union of largest variant payload
                let tag_ty = BasicTypeEnum::IntType(self.context.i32_type());
                let payload_ty = BasicTypeEnum::IntType(self.context.i64_type());
                // For simplicity, use i64 as payload storage
                BasicTypeEnum::StructType(self.context.struct_type(&[tag_ty, payload_ty], false))
            }
            crate::ast::TypeDefKind::Alias(ty) | crate::ast::TypeDefKind::Newtype(ty) => {
                types::mimi_type_to_llvm(self.context, ty)
                    .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()))
            }
        };
        self.type_llvm.insert(t.name.clone(), llvm_ty);
        self.type_defs.insert(t.name.clone(), t.clone());
        Ok(())
    }

    fn compile_func(&mut self, func: &FuncDef) -> Result<(), String> {
        let ret_type = match &func.ret {
            Some(ty) => types::mimi_type_to_llvm(self.context, ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type())),
            None => BasicTypeEnum::IntType(self.context.i64_type()),
        };

        let mut param_types = Vec::new();
        for param in &func.params {
            if let Some(ty) = types::mimi_type_to_llvm(self.context, &param.ty) {
                param_types.push(ty);
            }
        }

        let metadata_params: Vec<_> = param_types.iter().map(|t| types::basic_to_metadata(self.context, *t)).collect();

        let fn_type = match ret_type {
            BasicTypeEnum::IntType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&metadata_params, false),
            _ => self.context.i64_type().fn_type(&metadata_params, false),
        };

        let function = self.module.add_function(&func.name, fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        let mut vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
        for (i, param) in func.params.iter().enumerate() {
            if let Some(ty) = types::mimi_type_to_llvm(self.context, &param.ty) {
                let alloca = self.builder.build_alloca(ty, &param.name)
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(alloca, function.get_nth_param(i as u32).expect("param index matches function signature"))
                    .map_err(|e| format!("store error: {}", e))?;
                vars.insert(param.name.clone(), (alloca, ty));
            }
        }

        let mut last_val: BasicValueEnum = self.context.i64_type().const_int(0, false).into();
        for stmt in &func.body {
            match stmt {
                Stmt::Expr(expr) => {
                    last_val = self.compile_expr(expr, &vars)?;
                }
                Stmt::Return(Some(expr)) => {
                    let val = self.compile_expr(expr, &vars)?;
                    self.builder.build_return(Some(&val)).map_err(|e| format!("return error: {}", e))?;
                    return Ok(());
                }
                Stmt::Return(None) => {
                    self.builder.build_return(None).map_err(|e| format!("return error: {}", e))?;
                    return Ok(());
                }
                Stmt::Let { pat, init: Some(init), .. } => {
                    let val = self.compile_expr(init, &vars)?;
                    let name = match pat {
                        Pattern::Variable(n) => n.clone(),
                        _ => continue,
                    };
                    let ty = val.get_type();
                    let alloca = self.builder.build_alloca(ty, &name)
                        .map_err(|e| format!("alloca error: {}", e))?;
                    self.builder.build_store(alloca, val)
                        .map_err(|e| format!("store error: {}", e))?;
                    vars.insert(name, (alloca, ty));
                }
                Stmt::Assign { target: Expr::Ident(name), value } => {
                    let val = self.compile_expr(value, &vars)?;
                    if let Some(&(alloca, _)) = vars.get(name) {
                        self.builder.build_store(alloca, val)
                            .map_err(|e| format!("store error: {}", e))?;
                    }
                }
                Stmt::If { cond, then_, else_ } => {
                    let cond_val = self.compile_expr(cond, &vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        return Err("if condition must be boolean".into());
                    };

                    let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                    let then_bb = self.context.append_basic_block(function, "then");
                    let else_bb = self.context.append_basic_block(function, "else");
                    let merge_bb = self.context.append_basic_block(function, "ifcont");

                    self.builder.build_conditional_branch(cond_bool, then_bb, else_bb)
                        .map_err(|e| format!("branch error: {}", e))?;

                    // Then block
                    self.builder.position_at_end(then_bb);
                    self.compile_block(then_, &mut vars)?;
                    if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }

                    // Else block
                    self.builder.position_at_end(else_bb);
                    if let Some(else_block) = else_ {
                        self.compile_block(else_block, &mut vars)?;
                    }
                    if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }

                    // Continue at merge
                    self.builder.position_at_end(merge_bb);
                }
                Stmt::While { cond, body } => {
                    let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                    let loop_bb = self.context.append_basic_block(function, "loop");
                    let body_bb = self.context.append_basic_block(function, "loopbody");
                    let merge_bb = self.context.append_basic_block(function, "loopcont");

                    // Jump to loop condition check
                    self.builder.build_unconditional_branch(loop_bb)
                        .map_err(|e| format!("branch error: {}", e))?;

                    // Loop condition
                    self.builder.position_at_end(loop_bb);
                    let cond_val = self.compile_expr(cond, &vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        return Err("while condition must be boolean".into());
                    };
                    self.builder.build_conditional_branch(cond_bool, body_bb, merge_bb)
                        .map_err(|e| format!("branch error: {}", e))?;

                    // Loop body
                    self.builder.position_at_end(body_bb);
                    let old_break = self.loop_break.take();
                    let old_continue = self.loop_continue.take();
                    self.loop_break = Some(merge_bb);
                    self.loop_continue = Some(loop_bb);
                    self.compile_block(body, &mut vars)?;
                    if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }
                    self.loop_break = old_break;
                    self.loop_continue = old_continue;

                    // Continue after loop
                    self.builder.position_at_end(merge_bb);
                }
                Stmt::For { var, iterable, body } => {
                    // For loop: compile to while loop over index
                    let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                    let iterable_val = self.compile_expr(iterable, &vars)?;
                    let list_ptr = match iterable_val {
                        BasicValueEnum::PointerValue(pv) => pv,
                        _ => return Err("for loop requires a list".into()),
                    };

                    // Get list length (first element of list struct)
                    let list_struct_ty = inkwell::types::BasicTypeEnum::StructType(
                        self.context.struct_type(&[
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                        ], false)
                    );
                    let list_len_gep = self.builder.build_struct_gep(
                        list_struct_ty,
                        list_ptr,
                        0,
                        "list.len"
                    ).map_err(|e| format!("gep error: {}", e))?;
                    let list_len = self.builder.build_load(
                        BasicTypeEnum::IntType(self.context.i64_type()),
                        list_len_gep,
                        "len"
                    ).map_err(|e| format!("load error: {}", e))?;

                    // Create index variable
                    let idx_alloca = self.builder.build_alloca(self.context.i64_type(), "idx")
                        .map_err(|e| format!("alloca error: {}", e))?;
                    self.builder.build_store(idx_alloca, self.context.i64_type().const_int(0, false))
                        .map_err(|e| format!("store error: {}", e))?;

                    // Loop condition: idx < len
                    let loop_bb = self.context.append_basic_block(function, "forloop");
                    let body_bb = self.context.append_basic_block(function, "forbody");
                    let merge_bb = self.context.append_basic_block(function, "forcont");

                    self.builder.build_unconditional_branch(loop_bb)
                        .map_err(|e| format!("branch error: {}", e))?;

                    self.builder.position_at_end(loop_bb);
                    let idx_val = self.builder.build_load(
                        BasicTypeEnum::IntType(self.context.i64_type()),
                        idx_alloca,
                        "idx"
                    ).map_err(|e| format!("load error: {}", e))?;
                    let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err("index must be i64".into()); };
                    let len_iv = if let BasicValueEnum::IntValue(iv) = list_len { iv } else { return Err("length must be i64".into()); };
                    let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx_iv, len_iv, "cmp")
                        .map_err(|e| format!("cmp error: {}", e))?;
                    self.builder.build_conditional_branch(cmp, body_bb, merge_bb)
                        .map_err(|e| format!("branch error: {}", e))?;

                    // Body
                    self.builder.position_at_end(body_bb);
                    let old_break = self.loop_break.take();
                    let old_continue = self.loop_continue.take();
                    self.loop_break = Some(merge_bb);
                    self.loop_continue = Some(loop_bb);

                    // Get list data pointer
                    let data_gep = self.builder.build_struct_gep(
                        list_struct_ty,
                        list_ptr,
                        1,
                        "list.data"
                    ).map_err(|e| format!("gep error: {}", e))?;
                    let data_ptr = self.builder.build_load(
                        BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                        data_gep,
                        "data"
                    ).map_err(|e| format!("load error: {}", e))?;
                    let data_pv = if let BasicValueEnum::PointerValue(pv) = data_ptr { pv } else { return Err("data must be pointer".into()); };

                    // GEP to get element
                    let elem_ptr = unsafe {
                        self.builder.build_gep(
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            data_pv,
                            &[idx_iv],
                            "elem"
                        )
                    }.map_err(|e| format!("gep error: {}", e))?;
                    let elem = self.builder.build_load(
                        BasicTypeEnum::IntType(self.context.i64_type()),
                        elem_ptr,
                        "elem_val"
                    ).map_err(|e| format!("load error: {}", e))?;

                    // Bind loop variable
                    let elem_alloca = self.builder.build_alloca(BasicTypeEnum::IntType(self.context.i64_type()), var)
                        .map_err(|e| format!("alloca error: {}", e))?;
                    self.builder.build_store(elem_alloca, elem)
                        .map_err(|e| format!("store error: {}", e))?;
                    vars.insert(var.clone(), (elem_alloca, BasicTypeEnum::IntType(self.context.i64_type())));

                    // Compile body
                    self.compile_block(body, &mut vars)?;

                    // Increment index
                    if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
                        let next_idx = self.builder.build_int_add(idx_iv, self.context.i64_type().const_int(1, false), "next_idx")
                            .map_err(|e| format!("add error: {}", e))?;
                        self.builder.build_store(idx_alloca, next_idx)
                            .map_err(|e| format!("store error: {}", e))?;
                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }

                    self.loop_break = old_break;
                    self.loop_continue = old_continue;
                    self.builder.position_at_end(merge_bb);
                }
                Stmt::Break(_) => {
                    if let Some(target) = self.loop_break {
                        self.builder.build_unconditional_branch(target)
                            .map_err(|e| format!("break error: {}", e))?;
                        // Create unreachable block for subsequent statements
                        let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                        let unreachable = self.context.append_basic_block(function, "unreachable");
                        self.builder.position_at_end(unreachable);
                    } else {
                        return Err("break outside of loop".into());
                    }
                }
                Stmt::Continue => {
                    if let Some(target) = self.loop_continue {
                        self.builder.build_unconditional_branch(target)
                            .map_err(|e| format!("continue error: {}", e))?;
                        let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                        let unreachable = self.context.append_basic_block(function, "unreachable");
                        self.builder.position_at_end(unreachable);
                    } else {
                        return Err("continue outside of loop".into());
                    }
                }
                _ => {}
            }
        }

        self.builder.build_return(Some(&last_val)).map_err(|e| format!("return error: {}", e))?;
        Ok(())
    }

    fn compile_block(
        &mut self,
        block: &Block,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> Result<(), String> {
        for stmt in block {
            match stmt {
                Stmt::Expr(expr) => {
                    self.compile_expr(expr, vars)?;
                }
                Stmt::Return(Some(expr)) => {
                    let val = self.compile_expr(expr, vars)?;
                    self.builder.build_return(Some(&val)).map_err(|e| format!("return error: {}", e))?;
                    return Ok(());
                }
                Stmt::Return(None) => {
                    self.builder.build_return(None).map_err(|e| format!("return error: {}", e))?;
                    return Ok(());
                }
                Stmt::Let { pat, init: Some(init), .. } => {
                    let val = self.compile_expr(init, vars)?;
                    let name = match pat {
                        Pattern::Variable(n) => n.clone(),
                        _ => continue,
                    };
                    let ty = val.get_type();
                    let alloca = self.builder.build_alloca(ty, &name)
                        .map_err(|e| format!("alloca error: {}", e))?;
                    self.builder.build_store(alloca, val)
                        .map_err(|e| format!("store error: {}", e))?;
                    vars.insert(name, (alloca, ty));
                }
                Stmt::Assign { target: Expr::Ident(name), value } => {
                    let val = self.compile_expr(value, vars)?;
                    if let Some(&(alloca, _)) = vars.get(name) {
                        self.builder.build_store(alloca, val)
                            .map_err(|e| format!("store error: {}", e))?;
                    }
                }
                Stmt::If { cond, then_, else_ } => {
                    let cond_val = self.compile_expr(cond, vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        return Err("if condition must be boolean".into());
                    };

                    let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                    let then_bb = self.context.append_basic_block(function, "then");
                    let else_bb = self.context.append_basic_block(function, "else");
                    let merge_bb = self.context.append_basic_block(function, "ifcont");

                    self.builder.build_conditional_branch(cond_bool, then_bb, else_bb)
                        .map_err(|e| format!("branch error: {}", e))?;

                    self.builder.position_at_end(then_bb);
                    self.compile_block(then_, vars)?;
                    if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }

                    self.builder.position_at_end(else_bb);
                    if let Some(else_block) = else_ {
                        self.compile_block(else_block, vars)?;
                    }
                    if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }

                    self.builder.position_at_end(merge_bb);
                }
                Stmt::Break(_) | Stmt::Continue => {}
                _ => {}
            }
        }
        Ok(())
    }

    fn compile_expr(
        &self,
        expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match expr {
            Expr::Literal(lit) => match lit {
                Lit::Int(n) => Ok(self.context.i64_type().const_int(*n as u64, true).into()),
                Lit::Float(f) => Ok(self.context.f64_type().const_float(*f).into()),
                Lit::Bool(b) => Ok(self.context.bool_type().const_int(*b as u64, false).into()),
                Lit::Unit => Ok(self.context.i64_type().const_int(0, false).into()),
                Lit::String(s) => {
                    let global = self.builder.build_global_string_ptr(s, "str")
                        .map_err(|e| format!("string error: {}", e))?;
                    Ok(global.as_pointer_value().into())
                }
                Lit::FString(_) => Ok(self.context.i64_type().const_int(0, false).into()),
            },
            Expr::Ident(name) => {
                if let Some(&(alloca, ty)) = vars.get(name) {
                    self.builder.build_load(ty, alloca, name)
                        .map_err(|e| format!("load error: {}", e))
                } else {
                    Err(format!("undefined variable '{}'", name))
                }
            }
            Expr::Binary(op, lhs, rhs) => {
                let l = self.compile_expr(lhs, vars)?;
                let r = self.compile_expr(rhs, vars)?;
                self.compile_binop(*op, l, r)
            }
            Expr::Unary(op, inner) => {
                let v = self.compile_expr(inner, vars)?;
                match op {
                    UnOp::Neg => {
                        if let BasicValueEnum::IntValue(iv) = v {
                            let zero = self.context.i64_type().const_int(0, true);
                            Ok(self.builder.build_int_sub(zero, iv, "neg")
                                .map_err(|e| format!("neg error: {}", e))?.into())
                        } else if let BasicValueEnum::FloatValue(fv) = v {
                            let zero = self.context.f64_type().const_float(0.0);
                            Ok(self.builder.build_float_sub(zero, fv, "fneg")
                                .map_err(|e| format!("neg error: {}", e))?.into())
                        } else {
                            Err("negation requires numeric type".into())
                        }
                    }
                    UnOp::Not => {
                        if let BasicValueEnum::IntValue(iv) = v {
                            Ok(self.builder.build_not(iv, "not")
                                .map_err(|e| format!("not error: {}", e))?.into())
                        } else {
                            Err("not requires boolean type".into())
                        }
                    }
                    _ => Err(format!("unsupported unary operator {:?}", op)),
                }
            }
            Expr::Call(callee, args) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    self.compile_call(name, args, vars)
                } else {
                    Err("only direct function calls supported in codegen".into())
                }
            }
            Expr::Match(scrutinee, arms) => {
                let scrutinee_val = self.compile_expr(scrutinee, vars)?;
                let scrutinee_iv = if let BasicValueEnum::IntValue(iv) = scrutinee_val {
                    iv
                } else {
                    return Err("match scrutinee must be integer (enum tag)".into());
                };

                let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let merge_bb = self.context.append_basic_block(function, "matchcont");
                let mut else_bb = self.context.append_basic_block(function, "matchelse");

                let mut incoming_vals = Vec::new();
                let mut incoming_bbs = Vec::new();

                // Build if-else chain for each arm
                for (i, arm) in arms.iter().enumerate() {
                    let arm_bb = self.context.append_basic_block(function, &format!("arm{}", i));

                    match &arm.pat {
                        Pattern::Wildcard | Pattern::Variable(_) => {
                            // Always matches - jump to arm body
                            self.builder.position_at_end(else_bb);
                            self.builder.build_unconditional_branch(arm_bb)
                                .map_err(|e| format!("branch error: {}", e))?;
                        }
                        Pattern::Literal(lit) => {
                            self.builder.position_at_end(else_bb);
                            let lit_val = match lit {
                                Lit::Int(n) => self.context.i64_type().const_int(*n as u64, true),
                                Lit::Bool(b) => self.context.bool_type().const_int(*b as u64, false),
                                Lit::Unit => self.context.i64_type().const_int(0, false),
                                _ => return Err("unsupported match literal type".into()),
                            };
                            let cmp = self.builder.build_int_compare(
                                inkwell::IntPredicate::EQ,
                                scrutinee_iv,
                                lit_val,
                                "cmp",
                            ).map_err(|e| format!("cmp error: {}", e))?;
                            let next_bb = if i < arms.len() - 1 {
                                self.context.append_basic_block(function, &format!("next{}", i))
                            } else {
                                merge_bb
                            };
                            self.builder.build_conditional_branch(cmp, arm_bb, next_bb)
                                .map_err(|e| format!("branch error: {}", e))?;
                            else_bb = next_bb;
                        }
                        Pattern::Constructor(name, _) => {
                            // Constructor pattern: compare tag (name hash as i64 for now)
                            self.builder.position_at_end(else_bb);
                            let tag_val = self.context.i64_type().const_int(
                                name.bytes().fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64)),
                                false,
                            );
                            let cmp = self.builder.build_int_compare(
                                inkwell::IntPredicate::EQ,
                                scrutinee_iv,
                                tag_val,
                                "cmp",
                            ).map_err(|e| format!("cmp error: {}", e))?;
                            let next_bb = if i < arms.len() - 1 {
                                self.context.append_basic_block(function, &format!("next{}", i))
                            } else {
                                merge_bb
                            };
                            self.builder.build_conditional_branch(cmp, arm_bb, next_bb)
                                .map_err(|e| format!("branch error: {}", e))?;
                            else_bb = next_bb;
                        }
                        _ => return Err(format!("unsupported pattern in codegen: {:?}", arm.pat)),
                    }

                    // Arm body
                    self.builder.position_at_end(arm_bb);
                    let mut local_vars = vars.clone();
                    // Bind variables from pattern
                    match &arm.pat {
                        Pattern::Variable(name) => {
                            let alloca = self.builder.build_alloca(
                                BasicTypeEnum::IntType(self.context.i64_type()),
                                name,
                            ).map_err(|e| format!("alloca error: {}", e))?;
                            self.builder.build_store(alloca, scrutinee_iv)
                                .map_err(|e| format!("store error: {}", e))?;
                            local_vars.insert(name.clone(), (alloca, BasicTypeEnum::IntType(self.context.i64_type())));
                        }
                        Pattern::Constructor(_, inner_patterns) => {
                            // For constructor patterns, bind inner variables
                            // For now, assume single inner variable
                            for inner_pat in inner_patterns {
                                if let Pattern::Variable(name) = inner_pat {
                                    let alloca = self.builder.build_alloca(
                                        BasicTypeEnum::IntType(self.context.i64_type()),
                                        name,
                                    ).map_err(|e| format!("alloca error: {}", e))?;
                                    self.builder.build_store(alloca, scrutinee_iv)
                                        .map_err(|e| format!("store error: {}", e))?;
                                    local_vars.insert(name.clone(), (alloca, BasicTypeEnum::IntType(self.context.i64_type())));
                                }
                            }
                        }
                        _ => {}
                    }
                    let arm_val = self.compile_expr(&arm.body, &local_vars)?;
                    incoming_vals.push(arm_val);
                    incoming_bbs.push(arm_bb);
                    self.builder.build_unconditional_branch(merge_bb)
                        .map_err(|e| format!("branch error: {}", e))?;
                }

                // Unreachable else block (should not be reached if match is exhaustive)
                self.builder.position_at_end(else_bb);
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                // Merge block - use phi to select the right value
                self.builder.position_at_end(merge_bb);
                if incoming_vals.is_empty() {
                    return Err("empty match expression".into());
                }
                let ty = incoming_vals[0].get_type();
                let phi = self.builder.build_phi(ty, "match.result")
                    .map_err(|e| format!("phi error: {}", e))?;
                let phi_refs: Vec<_> = incoming_vals.iter().zip(incoming_bbs.iter())
                    .map(|(v, bb)| (v as &dyn inkwell::values::BasicValue, *bb))
                    .collect();
                phi.add_incoming(&phi_refs);
                Ok(phi.as_basic_value())
            }
            Expr::Record { ty, fields } => {
                // Create a record value
                let type_name = ty.as_deref().unwrap_or("unknown");
                let llvm_ty = self.type_llvm.get(type_name)
                    .ok_or_else(|| format!("unknown type '{}'", type_name))?;
                if let BasicTypeEnum::StructType(sty) = llvm_ty {
                    let alloca = self.builder.build_alloca(*sty, type_name)
                        .map_err(|e| format!("alloca error: {}", e))?;
                    // Store field values
                    for (i, field) in fields.iter().enumerate() {
                        let val = self.compile_expr(&field.value, vars)?;
                        let gep = self.builder.build_struct_gep(*sty, alloca, i as u32, &field.name)
                            .map_err(|e| format!("gep error: {}", e))?;
                        self.builder.build_store(gep, val)
                            .map_err(|e| format!("store error: {}", e))?;
                    }
                    Ok(alloca.into())
                } else {
                    Err(format!("type '{}' is not a struct", type_name))
                }
            }
            Expr::Field(obj, field_name) => {
                let obj_val = self.compile_expr(obj, vars)?;
                match obj_val {
                    BasicValueEnum::PointerValue(pv) => {
                        // Try to determine the struct type from the pointer
                        // We need to look up the type from the AST or type annotations
                        // For now, try to find the type name from the object expression
                        let type_name = match obj.as_ref() {
                            Expr::Ident(name) => {
                                // Look up the variable's type in type_llvm
                                vars.get(name).map(|(_, ty)| ty)
                            }
                            Expr::Record { ty: Some(name), .. } => {
                                self.type_llvm.get(name)
                            }
                            _ => None,
                        };
                        if let Some(BasicTypeEnum::StructType(sty)) = type_name {
                            // Find field index by looking up the type definition
                            let type_name_str = match obj.as_ref() {
                                Expr::Ident(_name) => {
                                    // Try to find the type from type_defs
                                    self.type_defs.iter().find(|(_, td)| {
                                        matches!(&td.kind, TypeDefKind::Record(fields) if fields.iter().any(|f| &f.name == field_name))
                                    }).map(|(n, _)| n.clone())
                                }
                                Expr::Record { ty: Some(name), .. } => Some(name.clone()),
                                _ => None,
                            };
                            if let Some(tn) = type_name_str {
                                if let Some(td) = self.type_defs.get(&tn) {
                                    if let TypeDefKind::Record(fields) = &td.kind {
                                        if let Some(idx) = fields.iter().position(|f| &f.name == field_name) {
                                            let gep = self.builder.build_struct_gep(*sty, pv, idx as u32, field_name)
                                                .map_err(|e| format!("gep error: {}", e))?;
                                            let field_ty = types::mimi_type_to_llvm(self.context, &fields[idx].ty)
                                                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                                            return self.builder.build_load(field_ty, gep, field_name)
                                                .map_err(|e| format!("load error: {}", e));
                                        }
                                    }
                                }
                            }
                            // Fallback: try field name as index (for anonymous structs)
                            if let Ok(idx) = field_name.parse::<u32>() {
                                let gep = self.builder.build_struct_gep(*sty, pv, idx, field_name)
                                    .map_err(|e| format!("gep error: {}", e))?;
                                return self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), gep, field_name)
                                    .map_err(|e| format!("load error: {}", e));
                            }
                        }
                        // Fallback: return 0 placeholder
                        Ok(self.context.i64_type().const_int(0, false).into())
                    }
                    _ => Err("field access on non-struct type".to_string()),
                }
            }
            Expr::List(elems) => {
                // Create a list struct: { i64 len, i64* data }
                let count = elems.len() as u64;
                let len_val = self.context.i64_type().const_int(count, false);
                // Allocate array
                let sizeof_i64 = self.context.i64_type().const_int(8, false);
                let alloc_size = self.builder.build_int_mul(len_val, sizeof_i64, "alloc_size")
                    .map_err(|e| format!("mul error: {}", e))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let data_ptr = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let data_ptr_i64 = self.builder.build_bit_cast(data_ptr,
                    self.context.i64_type().ptr_type(inkwell::AddressSpace::default()),
                    "data_ptr_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                // Store each element
                for (i, elem) in elems.iter().enumerate() {
                    let val = self.compile_expr(elem, vars)?;
                    let iv = match val {
                        BasicValueEnum::IntValue(iv) => iv,
                        _ => return Err("list elements must be i64 for now".into()),
                    };
                    let idx = self.context.i64_type().const_int(i as u64, false);
                    let elem_ptr = unsafe {
                        self.builder.build_gep(self.context.i64_type(), data_ptr_i64, &[idx], "elem")
                    }.map_err(|e| format!("gep error: {}", e))?;
                    self.builder.build_store(elem_ptr, iv)
                        .map_err(|e| format!("store error: {}", e))?;
                }
                // Create list struct
                let list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(self.context.i64_type()),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let list_alloca = self.builder.build_alloca(list_ty, "list")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let len_gep = self.builder.build_struct_gep(list_ty, list_alloca, 0, "list_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(len_gep, len_val)
                    .map_err(|e| format!("store error: {}", e))?;
                let data_gep = self.builder.build_struct_gep(list_ty, list_alloca, 1, "list_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let data_void_ptr = self.builder.build_bit_cast(data_ptr,
                    self.context.ptr_type(inkwell::AddressSpace::default()), "data_void")
                    .map_err(|e| format!("bitcast error: {}", e))?;
                self.builder.build_store(data_gep, data_void_ptr)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(list_alloca.into())
            }
            Expr::Index(obj, idx_expr) => {
                // list[i] - load from array
                let obj_val = self.compile_expr(obj, vars)?;
                let idx_val = self.compile_expr(idx_expr, vars)?;
                match obj_val {
                    BasicValueEnum::PointerValue(pv) => {
                        let idx_iv = match idx_val {
                            BasicValueEnum::IntValue(iv) => iv,
                            _ => return Err("index must be i64".into()),
                        };
                        // Assume it's a list struct and get data pointer
                        let list_ty = self.context.struct_type(&[
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                        ], false);
                        let data_gep = self.builder.build_struct_gep(list_ty, pv, 1, "list.data")
                            .map_err(|e| format!("gep error: {}", e))?;
                        let data_ptr = self.builder.build_load(
                            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                            data_gep, "data")
                            .map_err(|e| format!("load error: {}", e))?
                            .into_pointer_value();
                        let data_ptr_i64 = self.builder.build_bit_cast(data_ptr,
                            self.context.i64_type().ptr_type(inkwell::AddressSpace::default()),
                            "data_i64")
                            .map_err(|e| format!("bitcast error: {}", e))?
                            .into_pointer_value();
                        let elem_ptr = unsafe {
                            self.builder.build_gep(self.context.i64_type(), data_ptr_i64, &[idx_iv], "elem")
                        }.map_err(|e| format!("gep error: {}", e))?;
                        self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), elem_ptr, "elem_val")
                            .map_err(|e| format!("load error: {}", e))
                    }
                    _ => Err("index requires a list/array pointer".into()),
                }
            }
            _ => Err(format!("unsupported expression in codegen: {:?}", expr)),
        }
    }

    fn compile_binop(
        &self,
        op: BinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match op {
            BinOp::Add => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_add(l, r, "add").map_err(|e| format!("add error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_add(l, r, "fadd").map_err(|e| format!("add error: {}", e))?.into()),
                _ => Err("add requires same numeric types".into()),
            },
            BinOp::Sub => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_sub(l, r, "sub").map_err(|e| format!("sub error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_sub(l, r, "fsub").map_err(|e| format!("sub error: {}", e))?.into()),
                _ => Err("sub requires same numeric types".into()),
            },
            BinOp::Mul => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_mul(l, r, "mul").map_err(|e| format!("mul error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_mul(l, r, "fmul").map_err(|e| format!("mul error: {}", e))?.into()),
                _ => Err("mul requires same numeric types".into()),
            },
            BinOp::Div => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_signed_div(l, r, "div").map_err(|e| format!("div error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_div(l, r, "fdiv").map_err(|e| format!("div error: {}", e))?.into()),
                _ => Err("div requires same numeric types".into()),
            },
            BinOp::Mod => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_signed_rem(l, r, "rem").map_err(|e| format!("rem error: {}", e))?.into()),
                _ => Err("mod requires integer types".into()),
            },
            BinOp::EqCmp => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::EQ, l, r, "eq").map_err(|e| format!("cmp error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_compare(inkwell::FloatPredicate::OEQ, l, r, "feq").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("eq requires same types".into()),
            },
            BinOp::NeCmp => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::NE, l, r, "ne").map_err(|e| format!("cmp error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_compare(inkwell::FloatPredicate::ONE, l, r, "fne").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("ne requires same types".into()),
            },
            BinOp::Lt => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SLT, l, r, "lt").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("lt requires integer types".into()),
            },
            BinOp::Gt => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SGT, l, r, "gt").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("gt requires integer types".into()),
            },
            BinOp::Le => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SLE, l, r, "le").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("le requires integer types".into()),
            },
            BinOp::Ge => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SGE, l, r, "ge").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("ge requires integer types".into()),
            },
            BinOp::And => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_and(l, r, "and").map_err(|e| format!("and error: {}", e))?.into()),
                _ => Err("and requires boolean types".into()),
            },
            BinOp::Or => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_or(l, r, "or").map_err(|e| format!("or error: {}", e))?.into()),
                _ => Err("or requires boolean types".into()),
            },
            _ => Err(format!("unsupported binary operator {:?}", op)),
        }
    }

    fn compile_call(
        &self,
        name: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let mut compiled_args = Vec::new();
        for arg in args {
            compiled_args.push(self.compile_expr(arg, vars)?);
        }

        let metadata_args: Vec<_> = compiled_args.iter().map(|v| {
            match v {
                BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
                BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
                BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
            }
        }).collect();

        // Dispatch builtins
        if builtins::is_builtin(name) {
            return self.compile_builtin_call(name, &metadata_args);
        }

        if let Some(function) = self.module.get_function(name) {
            let call = self.builder.build_call(function, &metadata_args, "call")
                .map_err(|e| format!("call error: {}", e))?;
            Ok(call.try_as_basic_value().left().unwrap_or(
                self.context.i64_type().const_int(0, false).into()
            ))
        } else {
            Err(format!("undefined function '{}' in codegen", name))
        }
    }

    fn compile_builtin_call(
        &self,
        name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match name {
            "println" => {
                if args.is_empty() {
                    return Err("println expects at least 1 argument".into());
                }
                // For string args: call puts
                // For integer args: call printf with "%ld\n"
                let fmt_str = match args[0] {
                    BasicMetadataValueEnum::PointerValue(_) => {
                        // String arg - use puts
                        let puts = self.module.get_function("puts")
                            .ok_or_else(|| "puts not declared".to_string())?;
                        self.builder.build_call(puts, args, "puts_call")
                            .map_err(|e| format!("puts error: {}", e))?;
                        return Ok(self.context.i64_type().const_int(0, false).into());
                    }
                    BasicMetadataValueEnum::IntValue(_) => "%ld\n",
                    BasicMetadataValueEnum::FloatValue(_) => "%f\n",
                    _ => "%p\n",
                };
                let fmt_global = self.builder.build_global_string_ptr(fmt_str, "fmt")
                    .map_err(|e| format!("fmt error: {}", e))?;
                let mut printf_args = vec![
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ];
                printf_args.extend_from_slice(args);
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &printf_args, "printf_call")
                    .map_err(|e| format!("printf error: {}", e))?;
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "print" => {
                if args.is_empty() {
                    return Err("print expects at least 1 argument".into());
                }
                let fmt_str = match args[0] {
                    BasicMetadataValueEnum::PointerValue(_) => "%s",
                    BasicMetadataValueEnum::IntValue(_) => "%ld",
                    BasicMetadataValueEnum::FloatValue(_) => "%f",
                    _ => "%p",
                };
                let fmt_global = self.builder.build_global_string_ptr(fmt_str, "fmt")
                    .map_err(|e| format!("fmt error: {}", e))?;
                let mut printf_args = vec![
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ];
                printf_args.extend_from_slice(args);
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &printf_args, "printf_call")
                    .map_err(|e| format!("printf error: {}", e))?;
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "eprintln" => {
                if args.is_empty() {
                    return Err("eprintln expects at least 1 argument".into());
                }
                let fmt_str = match args[0] {
                    BasicMetadataValueEnum::PointerValue(_) => "%s\n",
                    BasicMetadataValueEnum::IntValue(_) => "%ld\n",
                    BasicMetadataValueEnum::FloatValue(_) => "%f\n",
                    _ => "%p\n",
                };
                let fmt_global = self.builder.build_global_string_ptr(fmt_str, "efmt")
                    .map_err(|e| format!("efmt error: {}", e))?;
                let mut printf_args = vec![
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ];
                printf_args.extend_from_slice(args);
                // Use fprintf(stderr, ...)
                let _stderr = self.module.get_global("stderr")
                    .map(|g| g.as_pointer_value())
                    .unwrap_or_else(|| {
                        // Fallback: just use printf
                        self.module.get_function("printf").unwrap().as_global_value().as_pointer_value()
                    });
                // For simplicity, use printf for stderr too (not ideal but functional)
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &printf_args, "eprintf_call")
                    .map_err(|e| format!("eprintf error: {}", e))?;
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "assert" => {
                if args.len() != 1 {
                    return Err("assert expects 1 argument".into());
                }
                let cond = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("assert requires boolean/i64 argument".into()),
                };
                let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let ok_bb = self.context.append_basic_block(function, "assert_ok");
                let fail_bb = self.context.append_basic_block(function, "assert_fail");
                self.builder.build_conditional_branch(cond, ok_bb, fail_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                self.builder.position_at_end(fail_bb);
                let fmt_global = self.builder.build_global_string_ptr("assertion failed\n", "assert_msg")
                    .map_err(|e| format!("fmt error: {}", e))?;
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &[
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ], "assert_printf")
                    .map_err(|e| format!("printf error: {}", e))?;
                let exit_fn = self.module.get_function("exit")
                    .ok_or_else(|| "exit not declared".to_string())?;
                self.builder.build_call(exit_fn, &[
                    BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(1, false)),
                ], "assert_exit")
                    .map_err(|e| format!("exit error: {}", e))?;
                self.builder.build_unconditional_branch(ok_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                self.builder.position_at_end(ok_bb);
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "assert_eq" => {
                if args.len() != 2 {
                    return Err("assert_eq expects 2 arguments".into());
                }
                let a = args[0];
                let b = args[1];
                let eq = match (a, b) {
                    (BasicMetadataValueEnum::IntValue(l), BasicMetadataValueEnum::IntValue(r)) => {
                        self.builder.build_int_compare(inkwell::IntPredicate::EQ, l, r, "cmp")
                            .map_err(|e| format!("cmp error: {}", e))?
                    }
                    (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => {
                        self.builder.build_float_compare(inkwell::FloatPredicate::OEQ, l, r, "cmp")
                            .map_err(|e| format!("cmp error: {}", e))?
                    }
                    _ => return Err("assert_eq requires same types".into()),
                };
                let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let ok_bb = self.context.append_basic_block(function, "aeq_ok");
                let fail_bb = self.context.append_basic_block(function, "aeq_fail");
                self.builder.build_conditional_branch(eq, ok_bb, fail_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                self.builder.position_at_end(fail_bb);
                let fmt_global = self.builder.build_global_string_ptr("assertion failed: values not equal\n", "aeq_msg")
                    .map_err(|e| format!("fmt error: {}", e))?;
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &[
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ], "aeq_printf")
                    .map_err(|e| format!("printf error: {}", e))?;
                let exit_fn = self.module.get_function("exit")
                    .ok_or_else(|| "exit not declared".to_string())?;
                self.builder.build_call(exit_fn, &[
                    BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(1, false)),
                ], "aeq_exit")
                    .map_err(|e| format!("exit error: {}", e))?;
                self.builder.build_unconditional_branch(ok_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                self.builder.position_at_end(ok_bb);
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "assert_ne" => {
                if args.len() != 2 {
                    return Err("assert_ne expects 2 arguments".into());
                }
                let a = args[0];
                let b = args[1];
                let ne = match (a, b) {
                    (BasicMetadataValueEnum::IntValue(l), BasicMetadataValueEnum::IntValue(r)) => {
                        self.builder.build_int_compare(inkwell::IntPredicate::NE, l, r, "cmp")
                            .map_err(|e| format!("cmp error: {}", e))?
                    }
                    (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => {
                        self.builder.build_float_compare(inkwell::FloatPredicate::ONE, l, r, "cmp")
                            .map_err(|e| format!("cmp error: {}", e))?
                    }
                    _ => return Err("assert_ne requires same types".into()),
                };
                let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let ok_bb = self.context.append_basic_block(function, "ane_ok");
                let fail_bb = self.context.append_basic_block(function, "ane_fail");
                self.builder.build_conditional_branch(ne, ok_bb, fail_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                self.builder.position_at_end(fail_bb);
                let fmt_global = self.builder.build_global_string_ptr("assertion failed: values are equal\n", "ane_msg")
                    .map_err(|e| format!("fmt error: {}", e))?;
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &[
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ], "ane_printf")
                    .map_err(|e| format!("printf error: {}", e))?;
                let exit_fn = self.module.get_function("exit")
                    .ok_or_else(|| "exit not declared".to_string())?;
                self.builder.build_call(exit_fn, &[
                    BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(1, false)),
                ], "ane_exit")
                    .map_err(|e| format!("exit error: {}", e))?;
                self.builder.build_unconditional_branch(ok_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                self.builder.position_at_end(ok_bb);
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "range" => {
                if args.len() != 2 {
                    return Err("range expects 2 arguments".into());
                }
                let start = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("range start must be i64".into()),
                };
                let end = match args[1] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("range end must be i64".into()),
                };
                // Create a list struct: { i64 len, i64* data }
                // For simplicity in codegen, we use a runtime-allocated array
                let len_val = self.builder.build_int_sub(end, start, "range_len")
                    .map_err(|e| format!("sub error: {}", e))?;
                // Allocate array: len * sizeof(i64)
                let sizeof_i64 = self.context.i64_type().const_int(8, false);
                let alloc_size = self.builder.build_int_mul(len_val, sizeof_i64, "alloc_size")
                    .map_err(|e| format!("mul error: {}", e))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let data_ptr = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let data_ptr_i64 = self.builder.build_bit_cast(data_ptr,
                    self.context.i64_type().ptr_type(inkwell::AddressSpace::default()),
                    "data_ptr_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                // Fill the array: for i in 0..len: data[i] = start + i
                let i64_ty = self.context.i64_type();
                let idx_alloca = self.builder.build_alloca(i64_ty, "idx")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let loop_bb = self.context.append_basic_block(function, "range_loop");
                let body_bb = self.context.append_basic_block(function, "range_body");
                let exit_bb = self.context.append_basic_block(function, "range_exit");
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                // Loop condition
                self.builder.position_at_end(loop_bb);
                let idx = self.builder.build_load(i64_ty, idx_alloca, "idx")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, len_val, "cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(cmp, body_bb, exit_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                // Body: data[idx] = start + idx
                self.builder.position_at_end(body_bb);
                let elem_val = self.builder.build_int_add(start, idx, "elem_val")
                    .map_err(|e| format!("add error: {}", e))?;
                let elem_ptr = unsafe {
                    self.builder.build_gep(i64_ty, data_ptr_i64, &[idx], "elem_ptr")
                }.map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(elem_ptr, elem_val)
                    .map_err(|e| format!("store error: {}", e))?;
                // idx++
                let next_idx = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next_idx")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(idx_alloca, next_idx)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                // Exit: create list struct { len, data* }
                self.builder.position_at_end(exit_bb);
                let list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let list_alloca = self.builder.build_alloca(list_ty, "list")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let len_gep = self.builder.build_struct_gep(list_ty, list_alloca, 0, "list_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(len_gep, len_val)
                    .map_err(|e| format!("store error: {}", e))?;
                let data_gep = self.builder.build_struct_gep(list_ty, list_alloca, 1, "list_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let data_void_ptr = self.builder.build_bit_cast(data_ptr,
                    self.context.ptr_type(inkwell::AddressSpace::default()), "data_void")
                    .map_err(|e| format!("bitcast error: {}", e))?;
                self.builder.build_store(data_gep, data_void_ptr)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(list_alloca.into())
            }
            "len" => {
                if args.len() != 1 {
                    return Err("len expects 1 argument".into());
                }
                match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => {
                        // Could be a string or list. Assume list struct { len, data* }
                        let list_ty = self.context.struct_type(&[
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                        ], false);
                        let len_gep = self.builder.build_struct_gep(list_ty, pv, 0, "list.len")
                            .map_err(|e| format!("gep error: {}", e))?;
                        let len = self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), len_gep, "len")
                            .map_err(|e| format!("load error: {}", e))?;
                        Ok(len)
                    }
                    _ => Err("len expects a list or string pointer".into()),
                }
            }
            "to_string" | "int_to_string" => {
                if args.len() != 1 {
                    return Err("to_string expects 1 argument".into());
                }
                match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => {
                        // Allocate 21 bytes for i64 string representation
                        let alloc_size = self.context.i64_type().const_int(21, false);
                        let malloc_fn = self.module.get_function("malloc")
                            .ok_or_else(|| "malloc not declared".to_string())?;
                        let buf = self.builder.build_call(malloc_fn, &[
                            BasicMetadataValueEnum::IntValue(alloc_size),
                        ], "malloc_call")
                            .map_err(|e| format!("malloc error: {}", e))?
                            .try_as_basic_value().left()
                            .ok_or("malloc returned void")?
                            .into_pointer_value();
                        let fmt_global = self.builder.build_global_string_ptr("%ld", "int_fmt")
                            .map_err(|e| format!("fmt error: {}", e))?;
                        let sprintf_fn = self.module.get_function("sprintf")
                            .ok_or_else(|| "sprintf not declared".to_string())?;
                        self.builder.build_call(sprintf_fn, &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                            BasicMetadataValueEnum::IntValue(iv),
                        ], "sprintf_call")
                            .map_err(|e| format!("sprintf error: {}", e))?;
                        // Return as string struct { ptr, len }
                        let strlen_fn = self.module.get_function("strlen")
                            .ok_or_else(|| "strlen not declared".to_string())?;
                        let str_len = self.builder.build_call(strlen_fn, &[
                            BasicMetadataValueEnum::PointerValue(buf),
                        ], "strlen_call")
                            .map_err(|e| format!("strlen error: {}", e))?
                            .try_as_basic_value().left()
                            .ok_or("strlen returned void")?;
                        let string_ty = self.context.struct_type(&[
                            BasicTypeEnum::PointerType(self.context.i8_type().ptr_type(inkwell::AddressSpace::default())),
                            BasicTypeEnum::IntType(self.context.i64_type()),
                        ], false);
                        let str_alloca = self.builder.build_alloca(string_ty, "str")
                            .map_err(|e| format!("alloca error: {}", e))?;
                        let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                            .map_err(|e| format!("gep error: {}", e))?;
                        self.builder.build_store(ptr_gep, buf)
                            .map_err(|e| format!("store error: {}", e))?;
                        let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                            .map_err(|e| format!("store error: {}", e))?;
                        self.builder.build_store(len_gep, str_len)
                            .map_err(|e| format!("store error: {}", e))?;
                        Ok(str_alloca.into())
                    }
                    _ => Err("to_string: unsupported type".into()),
                }
            }
            "abs" => {
                if args.len() != 1 {
                    return Err("abs expects 1 argument".into());
                }
                match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => {
                        // abs(x) = x < 0 ? -x : x
                        let zero = self.context.i64_type().const_int(0, true);
                        let neg = self.builder.build_int_sub(zero, iv, "neg")
                            .map_err(|e| format!("neg error: {}", e))?;
                        let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, iv, self.context.i64_type().const_int(0, false), "is_neg")
                            .map_err(|e| format!("cmp error: {}", e))?;
                        let result = self.builder.build_select(cmp, neg, iv, "abs_val")
                            .map_err(|e| format!("select error: {}", e))?;
                        Ok(result)
                    }
                    BasicMetadataValueEnum::FloatValue(_fv) => {
                        // Use fabs
                        let fabs_fn = self.module.get_function("fabs")
                            .or_else(|| {
                                // Declare fabs if not present
                                let fabs_ty = self.context.f64_type().fn_type(
                                    &[inkwell::types::BasicMetadataTypeEnum::FloatType(self.context.f64_type())], false);
                                Some(self.module.add_function("fabs", fabs_ty, Some(inkwell::module::Linkage::External)))
                            }).unwrap();
                        let call = self.builder.build_call(fabs_fn, args, "fabs_call")
                            .map_err(|e| format!("fabs error: {}", e))?;
                        Ok(call.try_as_basic_value().left().unwrap())
                    }
                    _ => Err("abs requires numeric type".into()),
                }
            }
            "sqrt" => {
                if args.len() != 1 {
                    return Err("sqrt expects 1 argument".into());
                }
                let sqrt_fn = self.module.get_function("sqrt")
                    .or_else(|| {
                        let sqrt_ty = self.context.f64_type().fn_type(
                            &[inkwell::types::BasicMetadataTypeEnum::FloatType(self.context.f64_type())], false);
                        Some(self.module.add_function("sqrt", sqrt_ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                let call = self.builder.build_call(sqrt_fn, args, "sqrt_call")
                    .map_err(|e| format!("sqrt error: {}", e))?;
                Ok(call.try_as_basic_value().left().unwrap())
            }
            "min" | "max" => {
                if args.len() != 2 {
                    return Err("min/max expects 2 arguments".into());
                }
                let a = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("min/max requires integer types".into()),
                };
                let b = match args[1] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("min/max requires integer types".into()),
                };
                let pred = if name == "min" {
                    inkwell::IntPredicate::SLT
                } else {
                    inkwell::IntPredicate::SGT
                };
                let cmp = self.builder.build_int_compare(pred, a, b, "cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let result = self.builder.build_select(cmp, a, b, "minmax")
                    .map_err(|e| format!("select error: {}", e))?;
                Ok(result)
            }
            "exit" => {
                if args.len() != 1 {
                    return Err("exit expects 1 argument".into());
                }
                let code = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("exit code must be integer".into()),
                };
                let exit_fn = self.module.get_function("exit")
                    .ok_or_else(|| "exit not declared".to_string())?;
                self.builder.build_call(exit_fn, &[
                    BasicMetadataValueEnum::IntValue(code),
                ], "exit_call")
                    .map_err(|e| format!("exit error: {}", e))?;
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "push" => {
                // push(list, elem) - simplified: just return list pointer (no real mutation in codegen yet)
                if args.len() != 2 {
                    return Err("push expects 2 arguments".into());
                }
                // TODO: real push implementation with realloc
                match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => Ok(pv.into()),
                    _ => Err("push requires a list pointer".into()),
                }
            }
            "pop" => {
                if args.is_empty() {
                    return Err("pop expects 1 argument".into());
                }
                // TODO: real pop implementation
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "floor" | "ceil" | "round" => {
                if args.len() != 1 {
                    return Err("floor/ceil/round expects 1 argument".into());
                }
                let fn_name = match name {
                    "floor" => "floor",
                    "ceil" => "ceil",
                    _ => "round",
                };
                let c_fn = self.module.get_function(fn_name)
                    .or_else(|| {
                        let ty = self.context.f64_type().fn_type(
                            &[inkwell::types::BasicMetadataTypeEnum::FloatType(self.context.f64_type())], false);
                        Some(self.module.add_function(fn_name, ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                let call = self.builder.build_call(c_fn, args, &format!("{}_call", fn_name))
                    .map_err(|e| format!("{} error: {}", fn_name, e))?;
                Ok(call.try_as_basic_value().left().unwrap())
            }
            _ => Err(format!("builtin '{}' not yet implemented in codegen", name)),
        }
    }

    pub fn emit_ir(&self) -> String {
        self.module.print_to_string().to_string()
    }

    pub fn compile_to_object(&self, output_path: &Path) -> Result<(), String> {
        Target::initialize_native(&InitializationConfig::default())
            .map_err(|e| format!("failed to initialize target: {}", e))?;
        let target = Target::from_name("x86-64")
            .ok_or("failed to find x86-64 target")?;
        let tm = target.create_target_machine(
            &TargetMachine::get_default_triple(),
            "x86-64",
            TargetMachine::get_host_cpu_features().to_string().as_str(),
            OptimizationLevel::Aggressive,
            RelocMode::Default,
            CodeModel::Default,
        ).ok_or("failed to create target machine")?;

        tm.write_to_file(&self.module, inkwell::targets::FileType::Object, output_path)
            .map_err(|e| format!("failed to write object file: {}", e))
    }
}
