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

    // String runtime functions
    // MimiList* = i8* (opaque pointer to {i64, i8**} struct)
    // str_split(s, delim) → MimiList*
    module.add_function("mimi_str_split",
        i8_ptr.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
        ], false),
        Some(inkwell::module::Linkage::External));
    // str_join(list*, sep) → i8* (heap-allocated string)
    module.add_function("mimi_str_join",
        i8_ptr.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
        ], false),
        Some(inkwell::module::Linkage::External));
    // str_replace(s, from, to) → i8* (heap-allocated string)
    module.add_function("mimi_str_replace",
        i8_ptr.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
        ], false),
        Some(inkwell::module::Linkage::External));

    // mimi_try_exit(payload): print error and exit(1) for ? operator
    module.add_function("mimi_try_exit",
        void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External));

    // FFI runtime functions (defined in Rust ffi/runtime.rs)
    // mimi_shared_retain(handle) -> handle
    module.add_function("mimi_shared_retain",
        i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External));
    // mimi_shared_release(handle)
    module.add_function("mimi_shared_release",
        void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External));
    // mimi_cap_check(cap, name) -> bool
    module.add_function("mimi_cap_check",
        i32.fn_type(&[
            BasicMetadataTypeEnum::IntType(i64),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
        ], false),
        Some(inkwell::module::Linkage::External));
    // mimi_cap_register(name) -> cap_id
    module.add_function("mimi_cap_register",
        i64.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i8_ptr),
        ], false),
        Some(inkwell::module::Linkage::External));
    // mimi_cap_consume(cap, name) -> bool
    module.add_function("mimi_cap_consume",
        i32.fn_type(&[
            BasicMetadataTypeEnum::IntType(i64),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
        ], false),
        Some(inkwell::module::Linkage::External));

    // Thread pool for parasteps (replaces raw pthread_create)
    // mimi_pool_submit(fn_ptr: i8*, arg: i8*) -> void
    module.add_function("mimi_pool_submit",
        void.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i8_ptr),  // fn_ptr
            BasicMetadataTypeEnum::PointerType(i8_ptr),  // arg
        ], false),
        Some(inkwell::module::Linkage::External));
    // mimi_pool_join_all() -> void: wait for all pool tasks to complete
    module.add_function("mimi_pool_join_all",
        void.fn_type(&[], false),
        Some(inkwell::module::Linkage::External));

    // Time functions
    // mimi_now() -> i64 (unix timestamp in seconds)
    module.add_function("mimi_now",
        i64.fn_type(&[], false),
        Some(inkwell::module::Linkage::External));
    // mimi_now_ms() -> i64 (unix timestamp in milliseconds)
    module.add_function("mimi_now_ms",
        i64.fn_type(&[], false),
        Some(inkwell::module::Linkage::External));
    // mimi_sleep(ms: i64) -> void
    module.add_function("mimi_sleep",
        void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External));

    // Environment/CLI functions
    // mimi_getenv(name: i8*) -> i8*
    module.add_function("mimi_getenv",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External));
    // mimi_args_init(argc: i64, argv: i8**) -> void
    module.add_function("mimi_args_init",
        void.fn_type(&[
            BasicMetadataTypeEnum::IntType(i64),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
        ], false),
        Some(inkwell::module::Linkage::External));
    // mimi_args_count() -> i64
    module.add_function("mimi_args_count",
        i64.fn_type(&[], false),
        Some(inkwell::module::Linkage::External));
    // mimi_args_get(i: i64) -> i8*
    module.add_function("mimi_args_get",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External));

    // JSON functions (stubs for codegen)
    // mimi_to_json(value_ptr: i8*) -> i8* (heap-allocated JSON string)
    module.add_function("mimi_to_json",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External));
    // mimi_from_json(json_str: i8*) -> i8* (heap-allocated Value pointer)
    module.add_function("mimi_from_json",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
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
        | "now" | "timestamp" | "now_ms" | "timestamp_ms" | "sleep"
        | "getenv" | "args"
        | "to_json" | "from_json"
    )
}
