use crate::codegen::CallSiteValueExt;
use crate::codegen::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_len(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 {
                    return Err(CompileError::WrongArgCount("len expects 1 argument".to_string()));
                }
                match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => {
                        if self.pending_len_is_string {
                            // String: use strlen
                            let strlen_fn = self.module.get_function("strlen")
                                .ok_or_else(|| "strlen not declared".to_string())?;
                            let len = self.builder.build_call(strlen_fn, &[
                                BasicMetadataValueEnum::PointerValue(pv),
                            ], "strlen")
                                .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
                                .try_as_basic_value_opt()
                                .ok_or("strlen returned void")?;
                            Ok(len)
                        } else {
                            // List struct { i64 len, i8* data }: read first field
                            let list_ty = self.list_struct_type();
                            let len_gep = self.gep().build_struct_gep(list_ty, pv, 0, "list.len")
                                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                            let len = self.builder.build_load(self.context.i64_type(), len_gep, "len")
                                .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                            Ok(len)
                        }
                    }
                    BasicMetadataValueEnum::StructValue(sv) => {
                        if self.pending_len_is_string {
                            // String struct {i8*, i64} — field 1 is the length directly
                            self.builder.build_extract_value(sv, 1, "str_len")
                                .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))
                        } else {
                            Err(CompileError::TypeMismatch("len: expected a string or list".to_string()))
                        }
                    }
                    _ => Err(CompileError::TypeMismatch("len expects a list or string pointer".to_string())),
                }

    }

    pub(in crate::codegen) fn compile_contains(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err(CompileError::WrongArgCount("contains expects 2 arguments".to_string())); }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("contains: first arg must be a list".to_string())),
                };
                let elem_val = args[1];
                let i64_ty = self.context.i64_type();
                // Get list length and data
                let list_len = self.load_list_len(list_ptr)?;
                let data_ptr = self.load_list_data_i64(list_ptr)?;
                // Loop through list elements
                let function = self.current_function().ok_or_else(|| "codegen: no current function for contains loop".to_string())?;
                let loop_bb = self.context.append_basic_block(function, "contains_loop");
                let body_bb = self.context.append_basic_block(function, "contains_body");
                let found_bb = self.context.append_basic_block(function, "contains_found");
                let done_bb = self.context.append_basic_block(function, "contains_done");
                let idx_alloca = self.builder.build_alloca(i64_ty, "ci")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(loop_bb);
                let idx = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), idx_alloca, "idx")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "cmp")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(body_bb);
                                let elem_ptr = unsafe {
                    self.gep().build_gep(i64_ty, data_ptr, &[idx], "elem")
                }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let elem = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "elem_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                let eq = match (elem, elem_val) {
                    (BasicValueEnum::IntValue(a), BasicMetadataValueEnum::IntValue(b)) => {
                        self.builder.build_int_compare(inkwell::IntPredicate::EQ, a, b, "eq")
                            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                    }
                    _ => return Err(CompileError::TypeMismatch("contains: element comparison only supports i64 for now".to_string())),
                };
                let inc_bb = self.context.append_basic_block(function, "contains_inc");
                self.builder.build_conditional_branch(eq, found_bb, inc_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // Next iteration
                self.builder.position_at_end(inc_bb);
                let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                self.builder.build_store(idx_alloca, next)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // Found
                self.builder.position_at_end(found_bb);
                self.builder.build_unconditional_branch(done_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // Done: phi(true, false)
                self.builder.position_at_end(done_bb);
                let phi = self.builder.build_phi(i64_ty, "result")
                    .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
                phi.add_incoming(&[
                    (&i64_ty.const_int(1, false), found_bb),
                    (&i64_ty.const_int(0, false), loop_bb),
                ]);
                Ok(phi.as_basic_value())

    }

}
