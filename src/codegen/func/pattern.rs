use crate::ast::*;
use crate::error::{CompileError, MimiResult};
use inkwell::types::BasicTypeEnum;
use inkwell::values::BasicValueEnum;
use std::collections::HashMap;

use super::CodeGenerator;
use super::VarEntry;

impl<'ctx> CodeGenerator<'ctx> {
    /// Recursive pattern matching binding for let statements.
    /// Walks the pattern tree and binds variables by extracting from the compiled value.
    pub(in crate::codegen) fn compile_pattern_bind(
        &mut self,
        pat: &Pattern,
        val: BasicValueEnum<'ctx>,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        match pat {
            Pattern::Wildcard => Ok(()),
            Pattern::Variable(name) => {
                let llvm_ty = val.get_type();
                let alloca = self.builder.build_alloca(llvm_ty, name)
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(alloca, val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                vars.insert(name.clone(), (alloca, llvm_ty));
                Ok(())
            }
            Pattern::Literal(lit) => {
                let lit_val = self.compile_literal_expr(lit, &HashMap::new())
                    .map_err(|e| CompileError::LlvmError(format!("literal pattern compile error: {}", e)))?;
                let eq = match (&val, &lit_val) {
                    (BasicValueEnum::IntValue(a), BasicValueEnum::IntValue(b)) => {
                        self.builder.build_int_compare(inkwell::IntPredicate::EQ, *a, *b, "pat_lit_eq")
                            .map_err(|e| CompileError::LlvmError(format!("icmp error: {}", e)))?
                    }
                    _ => return Err(CompileError::LlvmError(
                        "literal pattern: type mismatch".to_string()
                    )),
                };
                let bool_ty = self.context.bool_type();
                let assert_fn = self.module.get_function("mimi_runtime_assert")
                    .unwrap_or_else(|| {
                        let i8_ty = self.context.i8_type();
                        let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
                        let fn_ty = self.context.void_type().fn_type(&[
                            inkwell::types::BasicMetadataTypeEnum::IntType(bool_ty),
                            inkwell::types::BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        self.module.add_function("mimi_runtime_assert", fn_ty, None)
                    });
                let msg = self.builder.build_global_string_ptr("pattern literal match failed", "pat_lit_msg")
                    .map_err(|e| CompileError::LlvmError(format!("global str error: {}", e)))?;
                self.builder.build_call(assert_fn, &[
                    inkwell::values::BasicMetadataValueEnum::IntValue(eq),
                    inkwell::values::BasicMetadataValueEnum::PointerValue(msg.as_pointer_value()),
                ], "pat_lit_assert")
                    .map_err(|e| CompileError::LlvmError(format!("assert call error: {}", e)))?;
                Ok(())
            }
            Pattern::Constructor(_name, sub_patterns) => {
                if sub_patterns.is_empty() {
                    return Ok(());
                }
                // Load struct value if we have a pointer
                let struct_val = match val {
                    BasicValueEnum::PointerValue(pv) => {
                        // We need the struct type - use a default layout
                        let i64_ty = self.context.i64_type();
                        let struct_ty = self.context.struct_type(&[
                            BasicTypeEnum::IntType(self.context.i32_type()),
                            BasicTypeEnum::IntType(i64_ty),
                        ], false);
                        let loaded = self.builder.build_load(struct_ty, pv, "ctor_loaded")
                            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                        match loaded {
                            BasicValueEnum::StructValue(sv) => sv,
                            _ => return Err(CompileError::LlvmError(
                                "constructor pattern: expected struct from pointer".to_string()
                            )),
                        }
                    }
                    BasicValueEnum::StructValue(sv) => sv,
                    _ => return Err(CompileError::LlvmError(
                        "constructor pattern requires a struct value".to_string()
                    )),
                };
                if sub_patterns.len() == 1 {
                    let payload = self.builder.build_extract_value(struct_val, 1, "ctor_payload")
                        .map_err(|e| CompileError::LlvmError(format!("extract payload error: {}", e)))?;
                    self.compile_pattern_bind(&sub_patterns[0], payload, vars)?;
                } else {
                    for (i, sub_pat) in sub_patterns.iter().enumerate() {
                        let field_val = self.builder.build_extract_value(struct_val, (i + 1) as u32, &format!("ctor_field_{}", i))
                            .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))?;
                        self.compile_pattern_bind(sub_pat, field_val, vars)?;
                    }
                }
                Ok(())
            }
            Pattern::Tuple(sub_patterns) => {
                // Load struct value if we have a pointer
                let struct_val = match val {
                    BasicValueEnum::PointerValue(pv) => {
                        // Create a struct type from the tuple type stack
                        let struct_ty = self.tuple_type_stack.last()
                            .ok_or_else(|| CompileError::LlvmError("tuple_type_stack empty for tuple pattern".to_string()))?;
                        let loaded = self.builder.build_load(*struct_ty, pv, "tuple_pat_loaded")
                            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                        match loaded {
                            BasicValueEnum::StructValue(sv) => sv,
                            _ => return Err(CompileError::LlvmError(
                                "tuple pattern: expected struct from pointer".to_string()
                            )),
                        }
                    }
                    BasicValueEnum::StructValue(sv) => sv,
                    _ => return Err(CompileError::LlvmError(
                        "tuple pattern requires a tuple value".to_string()
                    )),
                };
                for (i, sub_pat) in sub_patterns.iter().enumerate() {
                    let field_val = self.builder.build_extract_value(struct_val, i as u32, &format!("tuple_pat_field_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))?;
                    self.compile_pattern_bind(sub_pat, field_val, vars)?;
                }
                Ok(())
            }
            Pattern::Array(sub_patterns) => {
                let list_ptr = match val {
                    BasicValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::LlvmError(
                        "array pattern requires a list pointer".to_string()
                    )),
                };
                let list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(self.context.i64_type()),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let data_gep = self.gep().build_struct_gep(list_ty, list_ptr, 1, "list.data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let data_ptr = self.builder.build_load(
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                    data_gep, "data"
                ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
                    .into_pointer_value();
                let data_i64 = self.builder.build_bit_cast(data_ptr,
                    self.context.i64_type().ptr_type(inkwell::AddressSpace::default()), "data_i64")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                    .into_pointer_value();
                for (i, sub_pat) in sub_patterns.iter().enumerate() {
                    let idx = self.context.i64_type().const_int(i as u64, false);
                                        let elem_ptr = unsafe {
                        self.gep().build_gep(self.context.i64_type(), data_i64, &[idx], &format!("pat_elem_{}", i))
                    }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                    let elem = self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), elem_ptr, &format!("pat_elem_val_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                    self.compile_pattern_bind(sub_pat, elem, vars)?;
                }
                Ok(())
            }
            Pattern::Slice(sub_patterns, rest) => {
                self.compile_pattern_bind(&Pattern::Array(sub_patterns.clone()), val, vars)?;
                if let Some(rest_pat) = rest {
                    self.compile_pattern_bind(rest_pat, val, vars)?;
                }
                Ok(())
            }
        }
    }
}
