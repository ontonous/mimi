pub mod io;
pub mod string;
pub mod math;
pub mod time_env;
pub mod json;
pub mod list;
pub mod map;
pub mod network;

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
    // mimi_map_keys(handle) → MimiList* (i8*)
    module.add_function("mimi_map_keys",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External));
    // mimi_map_values(handle) → MimiList* (i8*)
    module.add_function("mimi_map_values",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
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
    // G5: Refcounted heap allocation for shared values (defined in mimi_runtime.c)
    // mimi_rc_alloc(size: i64) -> i8*
    module.add_function("mimi_rc_alloc",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External));
    // mimi_rc_retain(ptr: i8*)
    module.add_function("mimi_rc_retain",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External));
    // mimi_rc_release(ptr: i8*)
    module.add_function("mimi_rc_release",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External));
    // mimi_rc_weak_retain(ptr: i8*)
    module.add_function("mimi_rc_weak_retain",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External));
    // mimi_rc_weak_release(ptr: i8*)
    module.add_function("mimi_rc_weak_release",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External));
    // mimi_rc_upgrade(ptr: i8*) -> i8*
    module.add_function("mimi_rc_upgrade",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
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

    // F7: Tuple FFI serialization — serialize heterogeneous tuple to JSON string.
    // mimi_tuple_serialize(values: *const i64, count: i64, elem_types: *const i64) -> i8*
    module.add_function("mimi_tuple_serialize",
        i8_ptr.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i64.ptr_type(AddressSpace::default())),
            BasicMetadataTypeEnum::IntType(i64),
            BasicMetadataTypeEnum::PointerType(i64.ptr_type(AddressSpace::default())),
        ], false),
        Some(inkwell::module::Linkage::External));
    // F7: Tuple FFI deserialization — parse JSON array back to i64 values.
    // mimi_tuple_deserialize(json: i8*, count: i64, elem_types: *const i64, out_values: *mut i64) -> i64
    module.add_function("mimi_tuple_deserialize",
        i64.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::IntType(i64),
            BasicMetadataTypeEnum::PointerType(i64.ptr_type(AddressSpace::default())),
            BasicMetadataTypeEnum::PointerType(i64.ptr_type(AddressSpace::default())),
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
    // mimi_is_valid_json(json_str: i8*) -> i32 (1 if valid, 0 if not)
    module.add_function("mimi_is_valid_json",
        i32.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External));
    // mimi_from_json(json_str: i8*) -> i8* (heap-allocated validated JSON string, or NULL)
    module.add_function("mimi_from_json",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External));
    // json_get_string(json: i8*, key: i8*) -> i8*
    module.add_function("json_get_string",
        i8_ptr.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
        ], false),
        Some(inkwell::module::Linkage::External));
    // json_get_int(json: i8*, key: i8*) -> i64
    module.add_function("json_get_int",
        i64.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
        ], false),
        Some(inkwell::module::Linkage::External));
    // json_get_element(json: i8*, index: i64) -> i8*
    module.add_function("json_get_element",
        i8_ptr.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::IntType(i64),
        ], false),
        Some(inkwell::module::Linkage::External));

    // ========== Network / Socket functions ==========
    // mimi_socket(domain: i64, type: i64, protocol: i64) -> i64
    module.add_function("mimi_socket",
        i64.fn_type(&[
            BasicMetadataTypeEnum::IntType(i64),
            BasicMetadataTypeEnum::IntType(i64),
            BasicMetadataTypeEnum::IntType(i64),
        ], false),
        Some(inkwell::module::Linkage::External));
    // mimi_connect(fd: i64, host: i8*, port: i64) -> i64
    module.add_function("mimi_connect",
        i64.fn_type(&[
            BasicMetadataTypeEnum::IntType(i64),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::IntType(i64),
        ], false),
        Some(inkwell::module::Linkage::External));
    // mimi_bind(fd: i64, port: i64) -> i64
    module.add_function("mimi_bind",
        i64.fn_type(&[
            BasicMetadataTypeEnum::IntType(i64),
            BasicMetadataTypeEnum::IntType(i64),
        ], false),
        Some(inkwell::module::Linkage::External));
    // mimi_listen(fd: i64, backlog: i64) -> i64
    module.add_function("mimi_listen",
        i64.fn_type(&[
            BasicMetadataTypeEnum::IntType(i64),
            BasicMetadataTypeEnum::IntType(i64),
        ], false),
        Some(inkwell::module::Linkage::External));
    // mimi_accept(fd: i64) -> i64
    module.add_function("mimi_accept",
        i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External));
    // mimi_send(fd: i64, data: i8*, len: i64) -> i64
    module.add_function("mimi_send",
        i64.fn_type(&[
            BasicMetadataTypeEnum::IntType(i64),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::IntType(i64),
        ], false),
        Some(inkwell::module::Linkage::External));
    // mimi_recv(fd: i64, buf_size: i64, out_len: i64*) -> i8*
    module.add_function("mimi_recv",
        i8_ptr.fn_type(&[
            BasicMetadataTypeEnum::IntType(i64),
            BasicMetadataTypeEnum::IntType(i64),
            BasicMetadataTypeEnum::PointerType(i8_ptr.ptr_type(AddressSpace::default())),
        ], false),
        Some(inkwell::module::Linkage::External));
    // mimi_close(fd: i64) -> i64
    module.add_function("mimi_close",
        i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External));
    // mimi_http_get(url: i8*) -> i8*
    module.add_function("mimi_http_get",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External));
    // mimi_http_post(url: i8*, body: i8*) -> i8*
    module.add_function("mimi_http_post",
        i8_ptr.fn_type(&[
            BasicMetadataTypeEnum::PointerType(i8_ptr),
            BasicMetadataTypeEnum::PointerType(i8_ptr),
        ], false),
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
        | "has_key" | "keys" | "values" | "map_new" | "map_get" | "map_set" | "map_remove" | "map_size" | "map_from_list"
        | "str_to_c_str" | "c_str_to_string"
        | "now" | "timestamp" | "now_ms" | "timestamp_ms" | "sleep"
        | "getenv" | "args"
        | "to_json" | "from_json"
        | "json_get_string" | "json_get_int" | "json_get_element" | "json_is_valid"
        // Network builtins
        | "socket" | "connect" | "bind" | "listen" | "accept"
        | "send" | "recv" | "close_fd"
        | "http_get" | "http_post"
        | "from_int"
    )
}


use super::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};


impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_builtin_call(
        &self,
        name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let libc_builtins: &[&str] = &[
            "println", "print", "eprintln",
            "assert", "assert_eq", "assert_ne", "assert_approx_eq",
            "input", "file_exists", "read_file", "write_file",
            "to_string", "int_to_string", "float_to_string",
            "pow", "random", "pi", "sqrt", "floor", "ceil", "round",
            "now", "timestamp", "now_ms", "timestamp_ms", "sleep",
            "getenv", "args", "from_int",
        ];
        if self.no_std && libc_builtins.contains(&name) {
            self.require_std(name)?;
        }
        match name {
            "println" => self.compile_println(args),
            "print" => self.compile_print(args),
            "eprintln" => self.compile_eprintln(args),
            "assert" => self.compile_assert(args),
            "assert_eq" => self.compile_assert_eq(args),
            "assert_ne" => self.compile_assert_ne(args),
            "assert_approx_eq" => self.compile_assert_approx_eq(args),
            "input" => self.compile_input(args),
            "file_exists" => self.compile_file_exists(args),
            "read_file" => self.compile_read_file(args),
            "write_file" => self.compile_write_file(args),
            "to_string" | "int_to_string" | "float_to_string" => self.compile_to_string(args),
            "str_char_at" => self.compile_str_char_at(args),
            "str_contains" => self.compile_str_contains(args),
            "str_starts_with" => self.compile_str_starts_with(args),
            "str_ends_with" => self.compile_str_ends_with(args),
            "str_parse_int" | "to_int" | "string_to_int" => self.compile_str_parse_int(args),
            "str_parse_float" | "to_float" => self.compile_str_parse_float(args),
            "str_index_of" => self.compile_str_index_of(args),
            "str_repeat" => self.compile_str_repeat(args),
            "str_trim" => self.compile_str_trim(args),
            "str_to_upper" => self.compile_str_to_upper(args),
            "str_to_lower" => self.compile_str_to_lower(args),
            "str_substring" => self.compile_str_substring(args),
            "str_split" => self.compile_str_split(args),
            "str_join" => self.compile_str_join(args),
            "str_replace" => self.compile_str_replace(args),
            "str_to_c_str" => self.compile_str_to_c_str(args),
            "c_str_to_string" => self.compile_c_str_to_string(args),
            "abs" => self.compile_abs(args),
            "sqrt" => self.compile_sqrt(args),
            "min" | "max" => self.compile_min_max(args, name),
            "floor" | "ceil" | "round" => self.compile_floor_ceil_round(args, name),
            "pow" => self.compile_pow(args),
            "random" => self.compile_random(args),
            "pi" => self.compile_pi(args),
            "now" | "timestamp" => self.compile_now(args),
            "now_ms" | "timestamp_ms" => self.compile_now_ms(args),
            "sleep" => self.compile_sleep(args),
            "getenv" => self.compile_getenv(args),
            "args" => self.compile_args(args),
            "exit" => self.compile_exit(args),
            "to_json" => self.compile_to_json(args),
            "from_json" => self.compile_from_json(args),
            "json_is_valid" => self.compile_is_valid_json(args),
            "json_get_string" => self.compile_json_get_string(args),
            "json_get_int" => self.compile_json_get_int(args),
            "json_get_element" => self.compile_json_get_element(args),
            "range" => self.compile_range(args),
            "len" => self.compile_len(args),
            "push" => self.compile_push(args),
            "pop" => self.compile_pop(args),
            "contains" => self.compile_contains(args),
            "sum" => self.compile_sum(args),
            "reverse" => self.compile_reverse(args),
            "flatten" => self.compile_flatten(args),
            "sort" => self.compile_sort(args),
            "enumerate" => self.compile_enumerate(args),
            "zip" => self.compile_zip(args),
            "map_new" => self.compile_map_new(args),
            "map_size" => self.compile_map_size(args),
            "has_key" => self.compile_has_key(args),
            "map_get" => self.compile_map_get(args),
            "map_set" => self.compile_map_set(args),
            "map_remove" => self.compile_map_remove(args),
            "map_from_list" => self.compile_map_from_list(args),
            "keys" => self.compile_map_keys(args),
            "values" => self.compile_map_values(args),
            "socket" => self.compile_socket(args),
            "connect" => self.compile_connect(args),
            "bind" => self.compile_bind(args),
            "listen" => self.compile_listen(args),
            "accept" => self.compile_accept(args),
            "send" => self.compile_send(args),
            "recv" => self.compile_recv(args),
            "close_fd" => self.compile_close_fd(args),
            "http_get" => self.compile_http_get(args),
            "http_post" => self.compile_http_post(args),
            "lexer" | "parse" => {
                Err(CompileError::BuiltinError(format!("'{}' is a runtime-only function, not available in codegen", name)))
            }
            "from_int" => self.compile_from_int(args),
            _ => Err(CompileError::BuiltinError(format!("builtin '{}' not yet implemented in codegen", name))),
        }
    }

    /// G2: Convert an integer to an enum tag value.
    /// from_int(int_val, enum_type_name) -> i32 tag
    fn compile_from_int(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() < 1 {
            return Err(CompileError::WrongArgCount("from_int expects at least 1 argument (int)".to_string()));
        }
        let val = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err(CompileError::TypeMismatch("from_int: first arg must be integer".to_string())),
        };
        // Truncate i64 to i32 for enum tag
        let i32_ty = self.context.i32_type();
        let tag = self.builder.build_int_truncate(val, i32_ty, "from_int_trunc")
            .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?;
        Ok(tag.into())
    }
}
