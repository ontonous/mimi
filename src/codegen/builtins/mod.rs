pub mod concurrency;
pub mod io;
pub mod json;
pub mod list;
pub mod map;
pub mod math;
pub mod network;
pub mod string;
pub mod time_env;

use crate::codegen::CallSiteValueExt;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::AddressSpace;

pub fn register_runtime<'ctx>(module: &Module<'ctx>, ctx: &'ctx Context) {
    let i8_ptr = ctx.ptr_type(AddressSpace::default());
    let i32 = ctx.i32_type();
    let i64 = ctx.i64_type();
    let void = ctx.void_type();

    register_libc(module, ctx, i8_ptr, i32, i64, void);
    register_map_record_fns(module, ctx, i8_ptr, i32, i64, void);
    register_string_fns(module, ctx, i8_ptr, i32, i64, void);
    register_regex_fns(module, ctx, i8_ptr, i32, i64, void);
    register_ffi_fns_defined_in_rust_ffi_rt_rs(module, ctx, i8_ptr, i32, i64, void);
    register_g5_refcounted_heap_allocation_for_shared_values_defined_in_mimi_rt_c(
        module, ctx, i8_ptr, i32, i64, void,
    );
    register_time_fns(module, ctx, i8_ptr, i32, i64, void);
    register_environment_cli_fns(module, ctx, i8_ptr, i32, i64, void);
    register_json_fns_stubs_for_codegen(module, ctx, i8_ptr, i32, i64, void);
    register_set_fns(module, ctx, i8_ptr, i32, i64, void);
    register_network_socket_fns(module, ctx, i8_ptr, i32, i64, void);
    register_mimifuture_mimiexecutor_poll_based_async_rt(module, ctx, i8_ptr, i32, i64, void);
    register_directory_path_fns(module, ctx, i8_ptr, i32, i64, void);
    register_process_advanced_file_operations(module, ctx, i8_ptr, i32, i64, void);
    register_binary_i_o_streaming_line_reading(module, ctx, i8_ptr, i32, i64, void);
    register_crypto_fns(module, ctx, i8_ptr, i32, i64, void);
    register_actor_concurrency_rt(module, ctx, i8_ptr, i32, i64, void);
    register_atomic_mutex_channel_rt(module, ctx, i8_ptr, i32, i64, void);
    register_quoted_ast_rt(module, ctx, i8_ptr, i32, i64, void);
}

fn register_libc<'ctx>(
    module: &Module<'ctx>,
    _ctx: &'ctx Context,
    i8_ptr: inkwell::types::PointerType<'ctx>,
    i32: inkwell::types::IntType<'ctx>,
    i64: inkwell::types::IntType<'ctx>,
    void: inkwell::types::VoidType<'ctx>,
) {
    module.add_function(
        "printf",
        i32.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], true), // CG-C7: return i32 (matches C int)
        Some(inkwell::module::Linkage::External),
    );

    module.add_function(
        "puts",
        i32.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );

    module.add_function(
        "malloc",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );

    module.add_function(
        "free",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );

    module.add_function(
        "strlen",
        i64.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );

    module.add_function(
        "strcmp",
        i32.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );

    module.add_function(
        "strcpy",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );

    module.add_function(
        "strcat",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );

    module.add_function(
        "memcpy",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );

    module.add_function(
        "realloc",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );

    // v0.28.13: push with exponential capacity growth
    module.add_function(
        "mimi_list_push_i64",
        void.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_list_push_grow",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );

    module.add_function(
        "fprintf",
        i32.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            true,
        ),
        Some(inkwell::module::Linkage::External),
    );

    module.add_function(
        "sprintf",
        i32.fn_type(
            // CG-C7: return i32 (matches C int, not i64)
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            true,
        ),
        Some(inkwell::module::Linkage::External),
    );

    module.add_function(
        "exit",
        void.fn_type(&[BasicMetadataTypeEnum::IntType(i32)], false),
        Some(inkwell::module::Linkage::External),
    );

    // Future-based spawn/await (replaces old pthread_create/join)
}

fn register_map_record_fns<'ctx>(
    module: &Module<'ctx>,
    ctx: &'ctx Context,
    i8_ptr: inkwell::types::PointerType<'ctx>,
    i32: inkwell::types::IntType<'ctx>,
    i64: inkwell::types::IntType<'ctx>,
    void: inkwell::types::VoidType<'ctx>,
) {
    // Map/Record runtime functions
    // MapHandle = i64 (pointer cast)
    module.add_function(
        "mimi_map_new",
        i64.fn_type(&[], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_map_destroy",
        void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_map_size",
        i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_map_has_key",
        i32.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_map_get",
        i64.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_map_set",
        void.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_map_remove",
        i32.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_map_from_list",
        i64.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default())),
                BasicMetadataTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default())),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_map_keys(handle) → MimiList* (i8*)
    module.add_function(
        "mimi_map_keys",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_map_values(handle) → MimiList* (i8*)
    module.add_function(
        "mimi_map_values",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_value_type_name",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
}

fn register_string_fns<'ctx>(
    module: &Module<'ctx>,
    ctx: &'ctx Context,
    i8_ptr: inkwell::types::PointerType<'ctx>,
    _i32: inkwell::types::IntType<'ctx>,
    i64: inkwell::types::IntType<'ctx>,
    _void: inkwell::types::VoidType<'ctx>,
) {
    // String runtime functions
    // MimiList* = i8* (opaque pointer to {i64, i8**} struct)
    // str_split(s, delim) → MimiList*
    module.add_function(
        "mimi_str_split",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // str_join(list*, sep) → i8* (heap-allocated string)
    module.add_function(
        "mimi_str_join",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_str_concat(a, b) → i8* (heap-allocated string)
    module.add_function(
        "mimi_str_concat",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // str_replace(s, from, to) → i8* (heap-allocated string)
    module.add_function(
        "mimi_str_replace",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_str_format(num_args, template, arg0..arg7) → i8* (heap-allocated string)
    module.add_function(
        "mimi_str_format",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_to_string_i64(i64) → i8* (heap-allocated string, Rust Display)
    module.add_function(
        "mimi_to_string_i64",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_to_string_f64(f64) → i8* (heap-allocated string, Rust Display)
    module.add_function(
        "mimi_to_string_f64",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::FloatType(ctx.f64_type())], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_str_clone(i8*, i64) → i64 (heap-allocated string handle for map storage)
    module.add_function(
        "mimi_str_clone",
        i64.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
}

fn register_regex_fns<'ctx>(
    module: &Module<'ctx>,
    _ctx: &'ctx Context,
    i8_ptr: inkwell::types::PointerType<'ctx>,
    i32: inkwell::types::IntType<'ctx>,
    i64: inkwell::types::IntType<'ctx>,
    void: inkwell::types::VoidType<'ctx>,
) {
    // Regex functions
    // mimi_regex_match(text, pattern) -> int (0 or 1)
    module.add_function(
        "mimi_regex_match",
        i32.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_regex_find(text, pattern) -> i8* (malloc'd match, empty on no match)
    module.add_function(
        "mimi_regex_find",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_regex_replace(text, pattern, replacement) -> i8* (malloc'd result)
    module.add_function(
        "mimi_regex_replace",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_regex_find_all(text, pattern) -> i8* (JSON array string)
    module.add_function(
        "mimi_regex_find_all",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_regex_capture_groups(text, pattern) -> i8* (JSON array string)
    module.add_function(
        "mimi_regex_capture_groups",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );

    // mimi_sort_f64_inplace(data: i8*, count: i64) -> void
    module.add_function(
        "mimi_sort_f64_inplace",
        void.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );

    // mimi_sort_str_inplace(data: *mut *mut c_char, count: i64) -> void
    // `data` is a pointer to an array of `*mut c_char` (i8**) — the list's
    // element buffer for `List<string>`. The function reorders the pointer
    // slots in place using lexicographic C-string comparison.
    module.add_function(
        "mimi_sort_str_inplace",
        void.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );

    // mimi_try_exit(payload): print error and exit(1) for ? operator
    module.add_function(
        "mimi_try_exit",
        void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );

    // mimi_try_exit_str(str, len): print string error and exit(1) for ? operator
    // Used when the error type is Result<T, string> to display the actual message.
    module.add_function(
        "mimi_try_exit_str",
        void.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );

    // mimi_match_panic(): abort on non-exhaustive match (CG-C1)
    module.add_function(
        "mimi_match_panic",
        void.fn_type(&[], false),
        Some(inkwell::module::Linkage::External),
    );
}

fn register_ffi_fns_defined_in_rust_ffi_rt_rs<'ctx>(
    module: &Module<'ctx>,
    _ctx: &'ctx Context,
    _i8_ptr: inkwell::types::PointerType<'ctx>,
    _i32: inkwell::types::IntType<'ctx>,
    i64: inkwell::types::IntType<'ctx>,
    void: inkwell::types::VoidType<'ctx>,
) {
    // FFI runtime functions (defined in Rust ffi/runtime.rs)
    // mimi_shared_retain(handle) -> handle
    module.add_function(
        "mimi_shared_retain",
        i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_shared_release(handle)
    module.add_function(
        "mimi_shared_release",
        void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
}

fn register_g5_refcounted_heap_allocation_for_shared_values_defined_in_mimi_rt_c<'ctx>(
    module: &Module<'ctx>,
    ctx: &'ctx Context,
    i8_ptr: inkwell::types::PointerType<'ctx>,
    i32: inkwell::types::IntType<'ctx>,
    i64: inkwell::types::IntType<'ctx>,
    void: inkwell::types::VoidType<'ctx>,
) {
    // G5: Refcounted heap allocation for shared values (defined in mimi_runtime.c)
    // mimi_rc_alloc(size: i64) -> i8*
    module.add_function(
        "mimi_rc_alloc",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_rc_retain(ptr: i8*)
    module.add_function(
        "mimi_rc_retain",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_rc_release(ptr: i8*)
    module.add_function(
        "mimi_rc_release",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_rc_weak_retain(ptr: i8*)
    module.add_function(
        "mimi_rc_weak_retain",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_rc_weak_release(ptr: i8*)
    module.add_function(
        "mimi_rc_weak_release",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_rc_upgrade(ptr: i8*) -> i8*
    module.add_function(
        "mimi_rc_upgrade",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_cap_check(cap, name) -> bool
    module.add_function(
        "mimi_cap_check",
        i32.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_cap_register(name) -> cap_id
    module.add_function(
        "mimi_cap_register",
        i64.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_cap_consume(cap, name) -> bool
    module.add_function(
        "mimi_cap_consume",
        i32.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );

    // F7: Tuple FFI serialization — serialize heterogeneous tuple to JSON string.
    // mimi_tuple_serialize(values: *const i64, count: i64, elem_types: *const i64) -> i8*
    module.add_function(
        "mimi_tuple_serialize",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default())),
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default())),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // F7: Tuple FFI deserialization — parse JSON array back to i64 values.
    // mimi_tuple_deserialize(json: i8*, count: i64, elem_types: *const i64, out_values: *mut i64) -> i64
    module.add_function(
        "mimi_tuple_deserialize",
        i64.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default())),
                BasicMetadataTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default())),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );

    // Thread pool for parasteps (replaces raw pthread_create)
    // mimi_pool_submit(fn_ptr: i8*, arg: i8*) -> void
    module.add_function(
        "mimi_pool_submit",
        void.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr), // fn_ptr
                BasicMetadataTypeEnum::PointerType(i8_ptr), // arg
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_pool_join_all() -> void: wait for all pool tasks to complete
    module.add_function(
        "mimi_pool_join_all",
        void.fn_type(&[], false),
        Some(inkwell::module::Linkage::External),
    );
}

fn register_time_fns<'ctx>(
    module: &Module<'ctx>,
    _ctx: &'ctx Context,
    _i8_ptr: inkwell::types::PointerType<'ctx>,
    _i32: inkwell::types::IntType<'ctx>,
    i64: inkwell::types::IntType<'ctx>,
    void: inkwell::types::VoidType<'ctx>,
) {
    // Time functions
    // mimi_now() -> i64 (unix timestamp in seconds)
    module.add_function(
        "mimi_now",
        i64.fn_type(&[], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_now_ms() -> i64 (unix timestamp in milliseconds)
    module.add_function(
        "mimi_now_ms",
        i64.fn_type(&[], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_sleep(ms: i64) -> void
    module.add_function(
        "mimi_sleep",
        void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
}

fn register_environment_cli_fns<'ctx>(
    module: &Module<'ctx>,
    _ctx: &'ctx Context,
    i8_ptr: inkwell::types::PointerType<'ctx>,
    _i32: inkwell::types::IntType<'ctx>,
    i64: inkwell::types::IntType<'ctx>,
    void: inkwell::types::VoidType<'ctx>,
) {
    // Environment/CLI functions
    // mimi_getenv(name: i8*) -> i8*
    module.add_function(
        "mimi_getenv",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_args_init(argc: i64, argv: i8**) -> void
    module.add_function(
        "mimi_args_init",
        void.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_args_count() -> i64
    module.add_function(
        "mimi_args_count",
        i64.fn_type(&[], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_args_list() -> MimiList*
    module.add_function(
        "mimi_args_list",
        i8_ptr.fn_type(&[], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_args_get(i: i64) -> i8*
    module.add_function(
        "mimi_args_get",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
}

fn register_json_fns_stubs_for_codegen<'ctx>(
    module: &Module<'ctx>,
    ctx: &'ctx Context,
    i8_ptr: inkwell::types::PointerType<'ctx>,
    i32: inkwell::types::IntType<'ctx>,
    i64: inkwell::types::IntType<'ctx>,
    _void: inkwell::types::VoidType<'ctx>,
) {
    // JSON functions (stubs for codegen)
    // mimi_is_valid_json(json_str: i8*) -> i32 (1 if valid, 0 if not)
    module.add_function(
        "mimi_is_valid_json",
        i32.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_from_json(json_str: i8*) -> i8* (heap-allocated validated JSON string, or NULL)
    module.add_function(
        "mimi_from_json",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // json_get_string(json: i8*, key: i8*) -> i8*
    module.add_function(
        "json_get_string",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // json_get_int(json: i8*, key: i8*) -> i64
    module.add_function(
        "json_get_int",
        i64.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // json_array_length(json: i8*) -> i64
    module.add_function(
        "json_array_length",
        i64.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // json_get_element(json: i8*, index: i64) -> i8*
    module.add_function(
        "json_get_element",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_json_as_i64(json: i8*) -> i64
    module.add_function(
        "mimi_json_as_i64",
        i64.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_json_as_f64(json: i8*) -> f64
    module.add_function(
        "mimi_json_as_f64",
        ctx.f64_type()
            .fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_json_as_bool(json: i8*) -> i64
    module.add_function(
        "mimi_json_as_bool",
        i64.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
}

fn register_set_fns<'ctx>(
    module: &Module<'ctx>,
    _ctx: &'ctx Context,
    i8_ptr: inkwell::types::PointerType<'ctx>,
    _i32: inkwell::types::IntType<'ctx>,
    i64: inkwell::types::IntType<'ctx>,
    void: inkwell::types::VoidType<'ctx>,
) {
    // ========== Set runtime functions ==========
    // mimi_set_new() -> i64 (handle)
    module.add_function(
        "mimi_set_new",
        i64.fn_type(&[], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_set_destroy(handle: i64)
    module.add_function(
        "mimi_set_destroy",
        void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_set_insert(handle: i64, value: i64) -> i64 (handle)
    module.add_function(
        "mimi_set_insert",
        i64.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_set_contains(handle: i64, value: i64) -> i64 (0/1)
    module.add_function(
        "mimi_set_contains",
        i64.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_set_remove(handle: i64, value: i64) -> i64 (handle)
    module.add_function(
        "mimi_set_remove",
        i64.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_set_size(handle: i64) -> i64
    module.add_function(
        "mimi_set_size",
        i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_set_to_list(handle: i64, out_len: *mut i64) -> *mut i64
    module.add_function(
        "mimi_set_to_list",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
}

fn register_network_socket_fns<'ctx>(
    module: &Module<'ctx>,
    ctx: &'ctx Context,
    i8_ptr: inkwell::types::PointerType<'ctx>,
    _i32: inkwell::types::IntType<'ctx>,
    i64: inkwell::types::IntType<'ctx>,
    _void: inkwell::types::VoidType<'ctx>,
) {
    // ========== Network / Socket functions ==========
    // mimi_socket(domain: i64, type: i64, protocol: i64) -> i64
    module.add_function(
        "mimi_socket",
        i64.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_connect(fd: i64, host: i8*, port: i64) -> i64
    module.add_function(
        "mimi_connect",
        i64.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_bind(fd: i64, port: i64) -> i64
    module.add_function(
        "mimi_bind",
        i64.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_listen(fd: i64, backlog: i64) -> i64
    module.add_function(
        "mimi_listen",
        i64.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_accept(fd: i64) -> i64
    module.add_function(
        "mimi_accept",
        i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_send(fd: i64, data: i8*, len: i64) -> i64
    module.add_function(
        "mimi_send",
        i64.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_recv(fd: i64, buf_size: i64, out_len: i64*) -> i8*
    module.add_function(
        "mimi_recv",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default())),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_close(fd: i64) -> i64
    module.add_function(
        "mimi_close",
        i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_http_get(url: i8*) -> i8*
    module.add_function(
        "mimi_http_get",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_http_post(url: i8*, body: i8*) -> i8*
    module.add_function(
        "mimi_http_post",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
}

fn register_mimifuture_mimiexecutor_poll_based_async_rt<'ctx>(
    module: &Module<'ctx>,
    _ctx: &'ctx Context,
    i8_ptr: inkwell::types::PointerType<'ctx>,
    i32: inkwell::types::IntType<'ctx>,
    i64: inkwell::types::IntType<'ctx>,
    void: inkwell::types::VoidType<'ctx>,
) {
    // ─── MimiFuture + MimiExecutor (poll-based async runtime) ───
    // mimi_future_alloc(result_size: i64) -> i8*
    module.add_function(
        "mimi_future_alloc",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_future_free(fut: i8*)
    module.add_function(
        "mimi_future_free",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_future_set_completed(fut: i8*)
    module.add_function(
        "mimi_future_set_completed",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_future_is_completed(fut: i8*) -> i32
    module.add_function(
        "mimi_future_is_completed",
        i32.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // MimiExecutor
    // mimi_executor_spawn(future: i8*, poll_fn: i8*) -> void
    module.add_function(
        "mimi_executor_spawn",
        void.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_executor_run() -> void
    module.add_function(
        "mimi_executor_run",
        void.fn_type(&[], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_spawn_future(future: i8*, poll_fn: i8*) -> i8*
    module.add_function(
        "mimi_spawn_future",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_await_future(future: i8*) -> void
    module.add_function(
        "mimi_await_future",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
}

fn register_directory_path_fns<'ctx>(
    module: &Module<'ctx>,
    _ctx: &'ctx Context,
    i8_ptr: inkwell::types::PointerType<'ctx>,
    _i32: inkwell::types::IntType<'ctx>,
    i64: inkwell::types::IntType<'ctx>,
    _void: inkwell::types::VoidType<'ctx>,
) {
    // ========== Directory & path runtime functions ==========
    // mimi_listdir(path: i8*) -> MimiList* (i8*)
    module.add_function(
        "mimi_listdir",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_is_dir(path: i8*) -> bool (i64)
    module.add_function(
        "mimi_is_dir",
        i64.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_is_file(path: i8*) -> bool (i64)
    module.add_function(
        "mimi_is_file",
        i64.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_path_join(a: i8*, b: i8*) -> i8*
    module.add_function(
        "mimi_path_join",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_path_ext(path: i8*) -> i8*
    module.add_function(
        "mimi_path_ext",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_path_basename(path: i8*) -> i8*
    module.add_function(
        "mimi_path_basename",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_path_dirname(path: i8*) -> i8*
    module.add_function(
        "mimi_path_dirname",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_walk_dir(path: i8*) -> MimiList* (i8*)
    module.add_function(
        "mimi_walk_dir",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_mkdir_p(path: i8*) -> bool (i64)
    module.add_function(
        "mimi_mkdir_p",
        i64.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_remove_file(path: i8*) -> bool (i64)
    module.add_function(
        "mimi_remove_file",
        i64.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
}

fn register_process_advanced_file_operations<'ctx>(
    module: &Module<'ctx>,
    _ctx: &'ctx Context,
    i8_ptr: inkwell::types::PointerType<'ctx>,
    _i32: inkwell::types::IntType<'ctx>,
    i64: inkwell::types::IntType<'ctx>,
    void: inkwell::types::VoidType<'ctx>,
) {
    // ========== Process & advanced file operations ==========
    // mimi_exec(cmd: i8*) -> i8* (MimiExecResult*)
    module.add_function(
        "mimi_exec",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_exec_free(res: i8*)
    module.add_function(
        "mimi_exec_free",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_exec_free_struct(res: i8*) — frees struct only, not strings
    module.add_function(
        "mimi_exec_free_struct",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_file_stat_free(res: i8*)
    module.add_function(
        "mimi_file_stat_free",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_file_stat(path: i8*, err_out: i8**) -> i8* (MimiStatResult*)
    module.add_function(
        "mimi_file_stat",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_append_file(path: i8*, content: i8*) -> i64
    module.add_function(
        "mimi_append_file",
        i64.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_set_env(key: i8*, value: i8*) -> i64
    module.add_function(
        "mimi_set_env",
        i64.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_exec_pipe(cmd: i8*) -> i8*
    module.add_function(
        "mimi_exec_pipe",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
}

fn register_binary_i_o_streaming_line_reading<'ctx>(
    module: &Module<'ctx>,
    _ctx: &'ctx Context,
    i8_ptr: inkwell::types::PointerType<'ctx>,
    _i32: inkwell::types::IntType<'ctx>,
    i64: inkwell::types::IntType<'ctx>,
    _void: inkwell::types::VoidType<'ctx>,
) {
    // ========== Binary I/O & streaming line reading ==========
    // mimi_read_file_partial(path: i8*, max_bytes: i64) -> i8*
    module.add_function(
        "mimi_read_file_partial",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_read_file_bytes(path: i8*) -> i8*
    module.add_function(
        "mimi_read_file_bytes",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_write_file_bytes(path: i8*, data: i8*) -> i64
    module.add_function(
        "mimi_write_file_bytes",
        i64.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_read_lines_json(path: i8*) -> i8*
    module.add_function(
        "mimi_read_lines_json",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
}

fn register_crypto_fns<'ctx>(
    module: &Module<'ctx>,
    _ctx: &'ctx Context,
    i8_ptr: inkwell::types::PointerType<'ctx>,
    _i32: inkwell::types::IntType<'ctx>,
    _i64: inkwell::types::IntType<'ctx>,
    _void: inkwell::types::VoidType<'ctx>,
) {
    // ========== Crypto runtime functions ==========
    // mimi_sha256(data: i8*) -> i8* (hex string)
    module.add_function(
        "mimi_sha256",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_base64_encode(data: i8*) -> i8*
    module.add_function(
        "mimi_base64_encode",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_base64_decode(data: i8*) -> i8*
    module.add_function(
        "mimi_base64_decode",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_lexer_tokenize(source: i8*) -> i8* (JSON array of tokens)
    module.add_function(
        "mimi_lexer_tokenize",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_parse_source(source: i8*) -> i8* (JSON AST)
    module.add_function(
        "mimi_parse_source",
        i8_ptr.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
}

pub fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "println" | "print" | "eprintln" | "assert" | "assert_eq" | "assert_ne"
        | "format"
        | "assert_approx_eq" | "range" | "len" | "to_string" | "abs" | "min" | "max"
        | "push" | "pop" | "sqrt" | "floor" | "ceil" | "round"
        | "int_to_string" | "float_to_string" | "string_to_int"
        | "exit" | "lexer" | "mms_parse" | "ast_eval"
        | "input" | "file_exists" | "read_file" | "write_file" | "char_code" | "chr" | "str_char_at"
        | "listdir" | "is_dir" | "is_file" | "path_join" | "path_ext" | "path_basename" | "path_dirname"
        | "walk_dir" | "mkdir_p" | "remove_file"
        | "exec" | "file_stat" | "append_file" | "set_env"
        | "exec_pipe"
        | "read_file_partial" | "read_file_bytes" | "write_file_bytes" | "read_lines_json"
        | "read_lines_json_builtin"
        | "sha256" | "base64_encode" | "base64_decode"
        | "str_contains" | "str_starts_with" | "str_ends_with"
        | "pow" | "random" | "pi"
        // v0.28.13 trigonometric and exponential
        | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "atan2"
        | "sinh" | "cosh" | "tanh"
        | "ln" | "log" | "log2" | "log10" | "exp" | "exp2" | "cbrt"
        | "str_parse_int" | "str_parse_float" | "to_int" | "to_float"
        | "str_index_of" | "str_repeat" | "str_trim"
        | "str_to_upper" | "str_to_lower" | "str_substring"
        | "contains" | "sum" | "reverse" | "flatten" | "sort" | "sort_f64" | "sort_str" | "zip" | "enumerate"
        | "str_split" | "str_join" | "str_replace"
        | "has_key" | "keys" | "values" | "map_new" | "map_get" | "map_set" | "map_remove" | "map_size" | "map_from_list"
        | "str_to_c_str" | "c_str_to_string"
        | "now" | "timestamp" | "now_ms" | "timestamp_ms" | "sleep"
        | "getenv" | "args"
        // v0.28.20 concurrency primitives
        | "atomic_i32_new" | "atomic_i32_load" | "atomic_i32_store"
        | "atomic_i32_fetch_add" | "atomic_i32_compare_exchange" | "atomic_i32_drop"
        | "atomic_i64_new" | "atomic_i64_load" | "atomic_i64_store"
        | "atomic_i64_fetch_add" | "atomic_i64_drop"
        | "atomic_bool_new" | "atomic_bool_load" | "atomic_bool_store" | "atomic_bool_drop"
        | "mutex_new" | "mutex_lock" | "mutex_get" | "mutex_set"
        | "mutex_unlock" | "mutex_drop"
        | "channel_new" | "channel_send" | "channel_recv"
        | "channel_try_recv" | "channel_drop"
        | "session_send" | "session_recv" | "session_close" | "session_open" | "session_pair"
        | "protocol_methods"
        | "actor_mailbox_depth" | "actor_is_muted" | "actor_set_mailbox_depth"
        | "actor_set_max_children" | "actor_spawn_count" | "actor_max_children"
        | "broadcast"
        | "spawn_detached"
        | "assert_state"
        | "inject_fault"
        // v0.29.44: shadow memory tagging
        | "shadow_alloc" | "shadow_tag" | "shadow_check" | "shadow_free"
        | "option_value_or"
        | "to_json" | "from_json"
        | "json_get_string" | "json_get_int" | "json_get_element" | "json_is_valid" | "json_array_length"
        // Network builtins
        | "socket" | "connect" | "bind" | "listen" | "accept"
        | "send" | "recv" | "close_fd"
        | "http_get" | "http_post"
        | "from_int"
        | "regex_match" | "regex_find" | "regex_replace"
        | "regex_find_all" | "regex_capture_groups"
    )
}

use super::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_builtin_call(
        &mut self,
        name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let libc_builtins: &[&str] = &[
            "println",
            "print",
            "eprintln",
            "assert",
            "assert_eq",
            "assert_ne",
            "assert_approx_eq",
            "input",
            "file_exists",
            "read_file",
            "write_file",
            "to_string",
            "int_to_string",
            "float_to_string",
            "pow",
            "random",
            "pi",
            "sqrt",
            "floor",
            "ceil",
            "round",
            "now",
            "timestamp",
            "now_ms",
            "timestamp_ms",
            "sleep",
            "getenv",
            "args",
            "from_int",
            // v0.28.13 math
            "sin",
            "cos",
            "tan",
            "asin",
            "acos",
            "atan",
            "atan2",
            "sinh",
            "cosh",
            "tanh",
            "ln",
            "log",
            "log2",
            "log10",
            "exp",
            "exp2",
            "cbrt",
        ];
        if self.no_std && libc_builtins.contains(&name) {
            self.require_std(name)?;
        }
        match name {
            "println" => self.compile_println(args),
            "print" => self.compile_print(args),
            "format" => self.compile_format(args),
            "eprintln" => self.compile_eprintln(args),
            "assert" => self.compile_assert(args),
            "assert_eq" => self.compile_assert_eq(args),
            "assert_ne" => self.compile_assert_ne(args),
            "assert_approx_eq" => self.compile_assert_approx_eq(args),
            "input" => self.compile_input(args),
            "file_exists" => self.compile_file_exists(args),
            "read_file" => self.compile_read_file(args),
            "write_file" => self.compile_write_file(args),
            "listdir" => self.compile_listdir(args),
            "is_dir" => self.compile_is_dir(args),
            "is_file" => self.compile_is_file(args),
            "path_join" => self.compile_path_join(args),
            "path_ext" => self.compile_path_ext(args),
            "path_basename" => self.compile_path_basename(args),
            "path_dirname" => self.compile_path_dirname(args),
            "walk_dir" => self.compile_walk_dir(args),
            "mkdir_p" => self.compile_mkdir_p(args),
            "remove_file" => self.compile_remove_file(args),
            "exec" => self.compile_exec(args),
            "exec_safe" => self.compile_exec_safe(args),
            "exec_pipe" => self.compile_exec_pipe(args),
            "file_stat" => self.compile_file_stat(args),
            "append_file" => self.compile_append_file(args),
            "set_env" => self.compile_set_env(args),
            "read_file_partial" => self.compile_read_file_partial(args),
            "read_file_bytes" => self.compile_read_file_bytes(args),
            "write_file_bytes" => self.compile_write_file_bytes(args),
            "read_lines_json" | "read_lines_json_builtin" => self.compile_read_lines_json(args),
            "option_value_or" => self.compile_option_value_or(args),
            "sha256" => self.compile_sha256(args),
            "base64_encode" => self.compile_base64_encode(args),
            "base64_decode" => self.compile_base64_decode(args),
            "to_string" | "int_to_string" | "float_to_string" => self.compile_to_string(args),
            "char_code" => self.compile_char_code(args),
            "chr" => self.compile_chr(args),
            "str_char_at" => self.compile_str_char_at(args),
            "str_contains" => self.compile_str_contains(args),
            "str_starts_with" => self.compile_str_starts_with(args),
            "str_ends_with" => self.compile_str_ends_with(args),
            "str_parse_int" | "string_to_int" => self.compile_str_parse_int(args),
            "to_int" => self.compile_to_int(args),
            "str_parse_float" => self.compile_str_parse_float(args),
            "to_float" => self.compile_to_float(args),
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
            // v0.28.13 trigonometric and exponential
            "sin" => self.compile_math_unary(args, "sin"),
            "cos" => self.compile_math_unary(args, "cos"),
            "tan" => self.compile_math_unary(args, "tan"),
            "asin" => self.compile_math_unary(args, "asin"),
            "acos" => self.compile_math_unary(args, "acos"),
            "atan" => self.compile_math_unary(args, "atan"),
            "atan2" => self.compile_math_binary(args, "atan2"),
            "sinh" => self.compile_math_unary(args, "sinh"),
            "cosh" => self.compile_math_unary(args, "cosh"),
            "tanh" => self.compile_math_unary(args, "tanh"),
            "ln" => self.compile_math_unary(args, "log"),
            "log" => self.compile_math_log(args),
            "log2" => self.compile_math_unary(args, "log2"),
            "log10" => self.compile_math_unary(args, "log10"),
            "exp" => self.compile_math_unary(args, "exp"),
            "exp2" => self.compile_math_unary(args, "exp2"),
            "cbrt" => self.compile_math_unary(args, "cbrt"),
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
            "json_array_length" => self.compile_json_array_length(args),
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
            "sort_f64" => self.compile_sort_f64(args),
            "sort_str" => self.compile_sort_str(args),
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
            "lexer" => self.compile_lexer(args),
            "mms_parse" => self.compile_parse(args),
            "from_int" => self.compile_from_int(args),
            "regex_match" => self.compile_regex_match(args),
            "regex_find" => self.compile_regex_find(args),
            "regex_replace" => self.compile_regex_replace(args),
            "regex_find_all" => self.compile_regex_find_all(args),
            "regex_capture_groups" => self.compile_regex_capture_groups(args),
            // v0.28.20 concurrency primitives
            "atomic_i32_new" => self.compile_atomic_i32_new(args),
            "atomic_i32_load" => self.compile_atomic_i32_load(args),
            "atomic_i32_store" => self.compile_atomic_i32_store(args),
            "atomic_i32_fetch_add" => self.compile_atomic_i32_fetch_add(args),
            "atomic_i32_compare_exchange" => self.compile_atomic_i32_compare_exchange(args),
            "atomic_i32_drop" => {
                self.compile_atomic_drop_helper("mimi_atomic_i32_drop", args)?;
                Ok(BasicValueEnum::IntValue(
                    self.context.i64_type().const_int(0, false),
                ))
            }
            "atomic_i64_new" => self.compile_atomic_i64_new(args),
            "atomic_i64_load" => self.compile_atomic_i64_load(args),
            "atomic_i64_store" => self.compile_atomic_i64_store(args),
            "atomic_i64_fetch_add" => self.compile_atomic_i64_fetch_add(args),
            "atomic_i64_drop" => {
                self.compile_atomic_drop_helper("mimi_atomic_i64_drop", args)?;
                Ok(BasicValueEnum::IntValue(
                    self.context.i64_type().const_int(0, false),
                ))
            }
            "atomic_bool_new" => self.compile_atomic_bool_new(args),
            "atomic_bool_load" => self.compile_atomic_bool_load(args),
            "atomic_bool_store" => self.compile_atomic_bool_store(args),
            "atomic_bool_drop" => {
                self.compile_atomic_drop_helper("mimi_atomic_bool_drop", args)?;
                Ok(BasicValueEnum::IntValue(
                    self.context.i64_type().const_int(0, false),
                ))
            }
            "mutex_new" => self.compile_mutex_new(args),
            "mutex_lock" => self.compile_mutex_lock(args),
            "mutex_get" => self.compile_mutex_get(args),
            "mutex_set" => self.compile_mutex_set(args),
            "mutex_unlock" => self.compile_mutex_unlock(args),
            "mutex_drop" => {
                self.compile_atomic_drop_helper("mimi_mutex_drop", args)?;
                Ok(BasicValueEnum::IntValue(
                    self.context.i64_type().const_int(0, false),
                ))
            }
            "channel_new" => self.compile_channel_new(args),
            "channel_send" => self.compile_channel_send(args),
            "channel_recv" => self.compile_channel_recv(args),
            // v0.29.34: session endpoint runtime — delegates to channel builtins.
            "session_send" => self.compile_session_send(args),
            "session_recv" => self.compile_session_recv(args),
            "session_close" => self.compile_session_close(args),
            "session_open" | "session_pair" => self.compile_session_open(args),
            "protocol_methods" => Ok(self.context.i64_type().const_int(0, false).into()),
            "actor_mailbox_depth" => self.compile_actor_mailbox_query(args, "mimi_actor_mailbox_depth"),
            "actor_is_muted" => self.compile_actor_mailbox_query(args, "mimi_actor_is_muted"),
            "actor_set_mailbox_depth" => self.compile_actor_set_mailbox_depth(args),
            "actor_set_max_children" => self.compile_actor_set_max_children(args),
            "actor_spawn_count" => self.compile_actor_spawn_count(),
            "actor_max_children" => self.compile_actor_max_children(),
            "broadcast" => self.compile_broadcast(args),
            // v0.29.37: spawn_detached — returns actor handle (i64), survives parent kill
            "spawn_detached" => {
                // For codegen, spawn_detached calls the same spawn path as regular spawn.
                // The detached flag is a runtime concept; in codegen we return a handle
                // just like regular spawn. The interp path handles the detached flag.
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            // v0.29.38: assert_state — test utility, returns unit (no-op in codegen)
            "assert_state" => {
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            // v0.29.38: inject_fault — test utility, returns Fault record (0 in codegen)
            "inject_fault" => {
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            // v0.29.44: shadow memory tagging builtins
            "shadow_alloc" => {
                // Delegates to mimi_shadow_alloc(size, tag, label) -> ptr
                self.compile_shadow_alloc(args)
            }
            "shadow_tag" => {
                self.compile_shadow_simple(args, "mimi_shadow_tag", 2)
            }
            "shadow_check" => {
                self.compile_shadow_simple(args, "mimi_shadow_check", 2)
            }
            "shadow_free" => {
                self.compile_shadow_simple(args, "mimi_shadow_free", 1)?;
                Ok(BasicValueEnum::IntValue(self.context.i64_type().const_int(0, false)))
            }
            "channel_try_recv" => self.compile_channel_try_recv(args),
            "channel_drop" => {
                self.compile_atomic_drop_helper("mimi_channel_drop", args)?;
                Ok(BasicValueEnum::IntValue(
                    self.context.i64_type().const_int(0, false),
                ))
            }
            "ast_eval" => {
                // ast_eval on a compile-time folded quote block:
                // quote! { 42 } evaluates directly to i64(42) at compile time,
                // so ast_eval just returns the argument value unchanged.
                if args.len() == 1 {
                    Ok(match args[0] {
                        BasicMetadataValueEnum::IntValue(iv) => BasicValueEnum::IntValue(iv),
                        BasicMetadataValueEnum::FloatValue(fv) => BasicValueEnum::FloatValue(fv),
                        BasicMetadataValueEnum::PointerValue(pv) => {
                            BasicValueEnum::PointerValue(pv)
                        }
                        BasicMetadataValueEnum::StructValue(sv) => BasicValueEnum::StructValue(sv),
                        BasicMetadataValueEnum::ArrayValue(av) => BasicValueEnum::ArrayValue(av),
                        BasicMetadataValueEnum::VectorValue(vv) => BasicValueEnum::VectorValue(vv),
                        BasicMetadataValueEnum::ScalableVectorValue(svv) => {
                            BasicValueEnum::ScalableVectorValue(svv)
                        }
                        BasicMetadataValueEnum::MetadataValue(_) => {
                            return Err(CompileError::BuiltinError(
                                "ast_eval: unexpected MetadataValue argument".to_string(),
                            ))
                        }
                    })
                } else {
                    Err(CompileError::WrongArgCount(
                        "ast_eval expects 1 argument".to_string(),
                    ))
                }
            }
            _ => Err(CompileError::BuiltinError(format!(
                "builtin '{}' not yet implemented in codegen",
                name
            ))),
        }
    }

    fn compile_lexer(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "lexer expects 1 argument (source string)".to_string(),
            ));
        }
        let source_ptr = self.extract_raw_str_ptr(&args[0]).map_err(|_| {
            CompileError::TypeMismatch("lexer: first arg must be a string".to_string())
        })?;
        let func = self
            .module
            .get_function("mimi_lexer_tokenize")
            .ok_or_else(|| "mimi_lexer_tokenize not declared".to_string())?;
        let result = self
            .builder
            .build_call(
                func,
                &[BasicMetadataValueEnum::PointerValue(source_ptr)],
                "lexer_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("lexer error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_lexer_tokenize returned void")?;
        self.wrap_c_string(result.into_pointer_value())
    }

    fn compile_parse(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "parse expects 1 argument (source string)".to_string(),
            ));
        }
        let source_ptr = self.extract_raw_str_ptr(&args[0]).map_err(|_| {
            CompileError::TypeMismatch("parse: first arg must be a string".to_string())
        })?;
        let func = self
            .module
            .get_function("mimi_parse_source")
            .ok_or_else(|| "mimi_parse_source not declared".to_string())?;
        let result = self
            .builder
            .build_call(
                func,
                &[BasicMetadataValueEnum::PointerValue(source_ptr)],
                "parse_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("parse error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_parse_source returned void")?;
        self.wrap_c_string(result.into_pointer_value())
    }

    /// G2: Convert an integer to an enum tag value.
    /// from_int(int_val, enum_type_name) -> i32 tag
    fn compile_option_value_or(
        &mut self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "option_value_or expects 2 arguments".to_string(),
            ));
        }
        let option_val = args[0];
        let default_val = match &args[1] {
            BasicMetadataValueEnum::IntValue(iv) => BasicValueEnum::IntValue(*iv),
            BasicMetadataValueEnum::StructValue(sv) => BasicValueEnum::StructValue(*sv),
            BasicMetadataValueEnum::PointerValue(pv) => BasicValueEnum::PointerValue(*pv),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "option_value_or: invalid default value type".to_string(),
                ))
            }
        };
        let default_ty = default_val.get_type();
        let current_fn = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("option_value_or: no current function".to_string())
        })?;
        let ok_bb = self.context.append_basic_block(current_fn, "opt_or_ok");
        let err_bb = self.context.append_basic_block(current_fn, "opt_or_err");
        let merge_bb = self.context.append_basic_block(current_fn, "opt_or_done");
        let result_alloca = self.build_alloca(default_ty, "opt_or_result")?;
        let (disc, payload) = match option_val {
            BasicMetadataValueEnum::StructValue(sv) => {
                let disc = self
                    .builder
                    .build_extract_value(sv, 0, "opt_disc")
                    .map_err(|e| CompileError::LlvmError(format!("extract disc: {}", e)))?
                    .into_int_value();
                let payload = self
                    .builder
                    .build_extract_value(sv, 1, "opt_payload")
                    .map_err(|e| CompileError::LlvmError(format!("extract payload: {}", e)))?;
                (disc, payload)
            }
            BasicMetadataValueEnum::PointerValue(pv) => {
                let loaded = self
                    .builder
                    .build_load(default_ty, pv, "opt_loaded")
                    .map_err(|e| CompileError::LlvmError(format!("load option: {}", e)))?;
                let disc = self
                    .builder
                    .build_extract_value(loaded.into_struct_value(), 0, "opt_disc")
                    .map_err(|e| CompileError::LlvmError(format!("extract disc: {}", e)))?
                    .into_int_value();
                let payload = self
                    .builder
                    .build_extract_value(loaded.into_struct_value(), 1, "opt_payload")
                    .map_err(|e| CompileError::LlvmError(format!("extract payload: {}", e)))?;
                (disc, payload)
            }
            _ => {
                return Err(CompileError::TypeMismatch(
                    "option_value_or: first arg must be an Option value".to_string(),
                ))
            }
        };
        self.builder
            .build_conditional_branch(disc, ok_bb, err_bb)
            .map_err(|e| CompileError::LlvmError(format!("cond branch: {}", e)))?;
        self.builder.position_at_end(ok_bb);
        self.build_store(result_alloca, payload)?;
        self.build_br(merge_bb)?;
        self.builder.position_at_end(err_bb);
        self.build_store(result_alloca, default_val)?;
        self.build_br(merge_bb)?;
        self.builder.position_at_end(merge_bb);
        self.build_load(default_ty, result_alloca, "opt_or_val")
    }

    fn compile_from_int(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.is_empty() {
            return Err(CompileError::WrongArgCount(
                "from_int expects at least 1 argument (int)".to_string(),
            ));
        }
        let val = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "from_int: first arg must be integer".to_string(),
                ))
            }
        };
        // Truncate i64 to i32 for enum tag
        let i32_ty = self.context.i32_type();
        let tag = self
            .builder
            .build_int_truncate(val, i32_ty, "from_int_trunc")
            .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?;
        Ok(tag.into())
    }
}

/// Register runtime functions for v0.28.19 Actor real concurrency.
///
/// - `mimi_actor_spawn(fields_ptr: i8*, fields_size: i64, dispatch_fn: i8*) -> i8*`
/// - `mimi_actor_id(handle: i8*) -> i64`
/// - `mimi_actor_current_id() -> i64`
/// - `mimi_actor_call(handle: i8*, method_id: i32, args_ptr: i8*, args_size: i64, result_ptr: i8*) -> i64`
/// - `mimi_actor_drop(handle: i8*)`
/// - `mimi_actor_fault(handle: i8*)` — v0.29.11 mailbox short-circuit
/// - `mimi_actor_is_faulted(handle: i8*) -> i32`
fn register_actor_concurrency_rt<'ctx>(
    module: &Module<'ctx>,
    _ctx: &'ctx Context,
    i8_ptr: inkwell::types::PointerType<'ctx>,
    i32: inkwell::types::IntType<'ctx>,
    i64: inkwell::types::IntType<'ctx>,
    void: inkwell::types::VoidType<'ctx>,
) {
    // mimi_actor_spawn(fields_ptr: i8*, fields_size: i64, dispatch_fn: i8*) -> i8*
    module.add_function(
        "mimi_actor_spawn",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_actor_id(handle: i8*) -> i64
    module.add_function(
        "mimi_actor_id",
        i64.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_actor_current_id() -> i64
    module.add_function(
        "mimi_actor_current_id",
        i64.fn_type(&[], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_actor_call(handle: i8*, method_id: i32, args_ptr: i8*, args_size: i64, result_ptr: i8*) -> i64
    module.add_function(
        "mimi_actor_call",
        i64.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i32),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_actor_drop(handle: i8*)
    module.add_function(
        "mimi_actor_drop",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_actor_fault(handle: i8*) — v0.29.11 Fault absorption short-circuit
    module.add_function(
        "mimi_actor_fault",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_actor_is_faulted(handle: i8*) -> i32
    module.add_function(
        "mimi_actor_is_faulted",
        i32.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // v0.29.21 mailbox backpressure
    module.add_function(
        "mimi_actor_set_mailbox_depth",
        void.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_actor_mailbox_depth",
        i64.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_actor_is_muted",
        i32.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // v0.29.24 spawn quota
    module.add_function(
        "mimi_actor_set_max_children",
        void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_actor_spawn_count",
        i64.fn_type(&[], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_actor_max_children",
        i64.fn_type(&[], false),
        Some(inkwell::module::Linkage::External),
    );
    // v0.29.25 broadcast
    module.add_function("mimi_session_pair", i64.fn_type(&[], false), Some(inkwell::module::Linkage::External));
    module.add_function("mimi_session_lo", i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false), Some(inkwell::module::Linkage::External));
    module.add_function("mimi_session_hi", i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false), Some(inkwell::module::Linkage::External));
    module.add_function(
        "mimi_actor_set_method_names",
        void.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_actor_method_id",
        i32.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_broadcast",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_broadcast_free",
        void.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
}

// v0.28.20 — concurrent primitive runtime declarations.
//
// Most primitives take/return i64; the channel `try_recv` returns -1 on
// failure (no message available). All return types follow Rust's stdlib
// conventions so the interp path (which calls the same runtime functions)
// produces identical results.
fn register_atomic_mutex_channel_rt<'ctx>(
    module: &Module<'ctx>,
    _ctx: &'ctx Context,
    _i8_ptr: inkwell::types::PointerType<'ctx>,
    i32: inkwell::types::IntType<'ctx>,
    i64: inkwell::types::IntType<'ctx>,
    void: inkwell::types::VoidType<'ctx>,
) {
    // ----- AtomicI32 -----
    // mimi_atomic_i32_new(value: i32) -> i64
    module.add_function(
        "mimi_atomic_i32_new",
        i64.fn_type(&[BasicMetadataTypeEnum::IntType(i32)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_atomic_i32_load",
        i32.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_atomic_i32_store",
        void.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::IntType(i32),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // fetch_add returns the previous value (i32).
    module.add_function(
        "mimi_atomic_i32_fetch_add",
        i32.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::IntType(i32),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_atomic_i32_compare_exchange",
        i32.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::IntType(i32),
                BasicMetadataTypeEnum::IntType(i32),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_atomic_i32_drop",
        void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );

    // ----- AtomicI64 -----
    module.add_function(
        "mimi_atomic_i64_new",
        i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_atomic_i64_load",
        i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_atomic_i64_store",
        void.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_atomic_i64_fetch_add",
        i64.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_atomic_i64_drop",
        void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );

    // ----- AtomicBool (stored as i32 with 0/1) -----
    module.add_function(
        "mimi_atomic_bool_new",
        i64.fn_type(&[BasicMetadataTypeEnum::IntType(i32)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_atomic_bool_load",
        i32.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_atomic_bool_store",
        void.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::IntType(i32),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_atomic_bool_drop",
        void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );

    // ----- Mutex -----
    module.add_function(
        "mimi_mutex_new",
        i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_mutex_lock",
        i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_mutex_get",
        i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_mutex_set",
        void.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_mutex_unlock",
        void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_mutex_drop",
        void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );

    // ----- Channel -----
    module.add_function(
        "mimi_channel_new",
        i64.fn_type(&[], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_channel_send",
        void.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i64),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_channel_recv",
        i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_channel_try_recv",
        i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_channel_drop",
        void.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
        Some(inkwell::module::Linkage::External),
    );
}

// v0.28.21 — Runtime QuotedAst (malloc + tagged union)
//
// The `mimi_quote_*` functions let codegen construct runtime QuotedAst
// nodes via heap-allocated MimiQuotedAst structs. Expr::Quote blocks
// that cannot be folded to constants at compile time emit calls to
// these functions to build the AST at runtime, then pass the resulting
// pointer to `ast_eval`.
fn register_quoted_ast_rt<'ctx>(
    module: &Module<'ctx>,
    _ctx: &'ctx Context,
    i8_ptr: inkwell::types::PointerType<'ctx>,
    i32: inkwell::types::IntType<'ctx>,
    i64: inkwell::types::IntType<'ctx>,
    void: inkwell::types::VoidType<'ctx>,
) {
    // mimi_quote_new_leaf(tag: i32, value: i64) -> i8*
    module.add_function(
        "mimi_quote_new_leaf",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i32),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_quote_new_node(tag: i32, child0: i8*, child1: i8*, extra: i64) -> i8*
    module.add_function(
        "mimi_quote_new_node",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i32),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_quote_new_list(tag: i32, children: i8**, len: i64) -> i8*
    module.add_function(
        "mimi_quote_new_list",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(i32),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_quote_drop(node: i8*)
    module.add_function(
        "mimi_quote_drop",
        void.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // mimi_quote_tag(node: i8*) -> i32
    module.add_function(
        "mimi_quote_tag",
        i32.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    // Accessors
    module.add_function(
        "mimi_quote_data0",
        i64.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_quote_data1",
        i64.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_quote_data2",
        i64.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_quote_argc",
        i32.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false),
        Some(inkwell::module::Linkage::External),
    );
    module.add_function(
        "mimi_quote_list_child",
        i8_ptr.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                BasicMetadataTypeEnum::IntType(i64),
            ],
            false,
        ),
        Some(inkwell::module::Linkage::External),
    );
}
