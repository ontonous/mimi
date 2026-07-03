mod access;
mod construct;
mod helpers;
mod hof;
mod mutate;

use crate::codegen::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, PointerValue};

impl<'ctx> CodeGenerator<'ctx> {
    /// Extract a list pointer from a builtin argument.
    ///
    /// Most list values in user code are already alloca pointers, but when a
    /// list is passed by value (e.g. a function parameter or a lambda argument)
    /// it arrives as a struct value. This helper stores those into a temporary
    /// alloca so builtins that mutate or inspect the list can work uniformly.
    pub(in crate::codegen) fn require_list_pointer(
        &self,
        arg: BasicMetadataValueEnum<'ctx>,
        tmp_name: &str,
    ) -> MimiResult<PointerValue<'ctx>> {
        match arg {
            BasicMetadataValueEnum::PointerValue(pv) => Ok(pv),
            BasicMetadataValueEnum::StructValue(sv) => {
                let tmp = self.build_alloca(BasicTypeEnum::StructType(sv.get_type()), tmp_name)?;
                self.build_store(tmp, sv)?;
                Ok(tmp)
            }
            _ => Err(CompileError::TypeMismatch(format!(
                "{}: expected a list",
                tmp_name
            ))),
        }
    }
}
