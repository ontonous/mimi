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

    // pthread support for parasteps
    // pthread_create(pthread_t*, void*, void* (*)(void*), void*) -> int
    // We use i8* for the function pointer (cast at call site)
    module.add_function("pthread_create",
        i32.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i64.ptr_type(AddressSpace::default())),  // pthread_t*
            BasicMetadataTypeEnum::PointerType(i8_ptr),  // attr (NULL)
            BasicMetadataTypeEnum::PointerType(i8_ptr),  // start_routine (as i8*, cast at callsite)
            BasicMetadataTypeEnum::PointerType(i8_ptr),  // arg
        ], false),
        Some(inkwell::module::Linkage::External));
    module.add_function("pthread_join",
        i32.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i64.ptr_type(AddressSpace::default())),  // pthread_t
            BasicMetadataTypeEnum::PointerType(i8_ptr),  // retval (NULL)
        ], false),
        Some(inkwell::module::Linkage::External));

    // Map/Record runtime functions
    // MapHandle = i64 (pointer cast)
    module.add_function("mimi_map_new",
        i64.fn_type(&[], false),
        Some(inkwell::module::Linkage::External));
    module.add_function("mimi_map_destroy",
        void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External));
    module.add_function("mimi_map_size",
        i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External));
    module.add_function("mimi_map_has_key",
        i32.fn_type(&[
            BasicMetadataTypeEnum::IntType(i64),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
        ], false),
        Some(inkwell::module::Linkage::External));
    module.add_function("mimi_map_get",
        i64.fn_type(&[
            BasicMetadataTypeEnum::IntType(i64),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
        ], false),
        Some(inkwell::module::Linkage::External));
    module.add_function("mimi_map_set",
        void.fn_type(&[
            BasicMetadataTypeEnum::IntType(i64),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::IntType(i64),
        ], false),
        Some(inkwell::module::Linkage::External));
    module.add_function("mimi_map_remove",
        i32.fn_type(&[
            BasicMetadataTypeEnum::IntType(i64),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
        ], false),
        Some(inkwell::module::Linkage::External));
    module.add_function("mimi_map_from_list",
        i64.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i64.ptr_type(AddressSpace::default())),
            BasicMetadataTypeEnum::PointerType(i64.ptr_type(AddressSpace::default())),
            BasicMetadataTypeEnum::IntType(i64),
        ], false),
        Some(inkwell::module::Linkage::External));
    module.add_function("mimi_value_type_name",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External));
}

pub fn is_builtin(name: &str) -> bool {
    matches!(name,
        "println" | "print" | "eprintln" | "assert" | "assert_eq" | "assert_ne"
        | "assert_approx_eq" | "range" | "len" | "to_string" | "abs" | "min" | "max"
        | "push" | "pop" | "sqrt" | "floor" | "ceil" | "round"
        | "int_to_string" | "float_to_string" | "string_to_int"
        | "exit" | "lexer" | "parse"
        | "input" | "file_exists" | "read_file" | "write_file" | "str_char_at"
        | "str_contains" | "str_starts_with" | "str_ends_with"
        | "pow" | "random" | "pi"
        | "str_parse_int" | "str_parse_float" | "to_int" | "to_float"
        | "str_index_of" | "str_repeat" | "str_trim"
        | "str_to_upper" | "str_to_lower" | "str_substring"
        | "contains" | "sum" | "reverse" | "flatten" | "sort" | "zip" | "enumerate"
        | "str_split" | "str_join" | "str_replace"
        | "has_key" | "map_new" | "map_get" | "map_set" | "map_remove" | "map_size" | "map_from_list"
        | "type_name" | "type_fields" | "type_variants"
        | "str_to_c_str" | "c_str_to_string"
    )
}
