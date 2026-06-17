use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::AddressSpace;

pub fn register_runtime<'ctx>(module: &Module<'ctx>, ctx: &'ctx Context) {
    let i8_ptr = ctx.i8_type().ptr_type(AddressSpace::default());
    let i32 = ctx.i32_type();
    let i64 = ctx.i64_type();
    let void = ctx.void_type();

    module.add_function("printf",
        i32.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], true),
        Some(inkwell::module::Linkage::External));

    module.add_function("puts",
        i32.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External));

    module.add_function("malloc",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External));

    module.add_function("free",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External));

    module.add_function("strlen",
        i64.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External));

    module.add_function("strcmp",
        i32.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
        ], false),
        Some(inkwell::module::Linkage::External));

    module.add_function("strcpy",
        i8_ptr.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
        ], false),
        Some(inkwell::module::Linkage::External));

    module.add_function("strcat",
        i8_ptr.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
        ], false),
        Some(inkwell::module::Linkage::External));

    module.add_function("memcpy",
        i8_ptr.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::IntType(i64),
        ], false),
        Some(inkwell::module::Linkage::External));

    module.add_function("realloc",
        i8_ptr.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::IntType(i64),
        ], false),
        Some(inkwell::module::Linkage::External));

    module.add_function("fprintf",
        i32.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
        ], true),
        Some(inkwell::module::Linkage::External));

    module.add_function("sprintf",
        i64.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
        ], true),
        Some(inkwell::module::Linkage::External));

    module.add_function("exit",
        void.fn_type(&[BasicMetadataTypeEnum::IntType(i32)], false),
        Some(inkwell::module::Linkage::External));
}

pub fn is_builtin(name: &str) -> bool {
    matches!(name,
        "println" | "print" | "eprintln" | "assert" | "assert_eq" | "assert_ne"
        | "assert_approx_eq" | "range" | "len" | "to_string" | "abs" | "min" | "max"
        | "push" | "pop" | "sqrt" | "floor" | "ceil" | "round"
        | "int_to_string" | "float_to_string" | "string_to_int"
        | "exit" | "lexer" | "parse"
    )
}
