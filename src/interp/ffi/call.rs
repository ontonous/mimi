// SD-4: fork() isolation removed. Signal guard (signal_guard.rs) replaces it.
// fork() in multi-threaded processes is POSIX UB (locked mutexes in other
// threads stay locked in the child). Signal guards are in-process, thread-safe,
// and don't require child process creation.
//
// 0.33 (Phase D/0.33.20): the raw libffi call helpers previously defined in
// this module (`call_ffi_raw` / `call_ffi_raw_struct` / `call_ffi_direct`)
// moved to `ffi_runtime.rs` so both the tree-walker interpreter and the
// bytecode VM share the exact same C ABI call path.
