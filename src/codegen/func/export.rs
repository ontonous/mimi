//! C ABI symbol naming and layout helpers for `extern "C"` functions and `#[repr(C)]` records.

use crate::ast::{Field, Type, TypeDefKind};
use crate::codegen::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::BasicTypeEnum;
use inkwell::AddressSpace;

impl<'ctx> CodeGenerator<'ctx> {
    pub(crate) fn export_body_symbol(name: &str) -> String {
        // `$` cannot appear in a Mimi identifier, so a source declaration
        // cannot collide with this generated symbol. Keep the C-visible name
        // exclusively on the conversion wrapper.
        format!("$mimi.export.body.{name}")
    }

    /// C-layout LLVM struct type for a repr(C) record.
    pub(crate) fn c_sty_for_reprc_record(
        &self,
        name: &str,
    ) -> MimiResult<inkwell::types::StructType<'ctx>> {
        let td = self.type_defs.get(name).ok_or_else(|| {
            CompileError::LlvmError(format!("repr(C) record '{}' not found", name))
        })?;
        let fields = match &td.kind {
            TypeDefKind::Record(fields) => fields.clone(),
            _ => {
                return Err(CompileError::LlvmError(format!(
                    "'{}' is not a record",
                    name
                )))
            }
        };
        self.c_layout_struct_type(&fields)
    }

    /// Map a Mimi type to the LLVM type used at the C ABI boundary for
    /// exported functions.
    fn c_abi_llvm_type(&self, ty: &Type) -> MimiResult<BasicTypeEnum<'ctx>> {
        match ty.unlocated() {
            Type::Name(name, _) => match name.as_str() {
                "i32" => Ok(BasicTypeEnum::IntType(self.context.i32_type())),
                "i64" => Ok(BasicTypeEnum::IntType(self.context.i64_type())),
                "f64" => Ok(BasicTypeEnum::FloatType(self.context.f64_type())),
                "bool" => Ok(BasicTypeEnum::IntType(self.context.i8_type())),
                "string" => Ok(BasicTypeEnum::PointerType(
                    self.context.ptr_type(AddressSpace::default()),
                )),
                "unit" => Ok(BasicTypeEnum::IntType(self.context.i64_type())),
                _ => {
                    if self.repr_c_record_names.contains(name.as_str()) {
                        let td = self.type_defs.get(name.as_str()).ok_or_else(|| {
                            CompileError::LlvmError(format!("unknown repr(C) record '{}'", name))
                        })?;
                        if let TypeDefKind::Record(fields) = &td.kind {
                            Ok(BasicTypeEnum::StructType(
                                self.c_layout_struct_type(fields)?,
                            ))
                        } else {
                            Err(CompileError::LlvmError(format!(
                                "'{}' is not a record",
                                name
                            )))
                        }
                    } else if self.record_type_names.contains(name.as_str()) || name == "List" {
                        // Non-repr(C) records and lists cross the boundary as JSON strings.
                        Ok(BasicTypeEnum::PointerType(
                            self.context.ptr_type(AddressSpace::default()),
                        ))
                    } else if name == "Map" || name == "Set" {
                        // Opaque runtime handles (i64).
                        Ok(BasicTypeEnum::IntType(self.context.i64_type()))
                    } else {
                        Err(CompileError::LlvmError(format!(
                            "type '{}' has no C ABI representation",
                            name
                        )))
                    }
                }
            },
            Type::Func(_, _) | Type::ExternFunc(_, _) => Ok(BasicTypeEnum::PointerType(
                self.context.ptr_type(AddressSpace::default()),
            )),
            Type::Tuple(_) => Ok(BasicTypeEnum::PointerType(
                self.context.ptr_type(AddressSpace::default()),
            )),
            _ => Err(CompileError::LlvmError(format!(
                "type '{}' has no C ABI representation",
                crate::core::fmt_type(ty)
            ))),
        }
    }

    /// Build a C-layout LLVM struct type for a list of record fields.
    fn c_layout_struct_type(
        &self,
        fields: &[Field],
    ) -> MimiResult<inkwell::types::StructType<'ctx>> {
        let mut field_tys = Vec::new();
        for f in fields {
            field_tys.push(self.c_abi_llvm_type(&f.ty)?);
        }
        Ok(self.context.struct_type(&field_tys, false))
    }
}
