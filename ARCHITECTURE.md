# Mimi Runtime Architecture

> 500-word runtime architecture overview (0.31.22 盲审修正)

## Overview

The Mimi runtime is a C ABI-compatible library (`libmimi_runtime.a`) that provides:
- Reference-counted memory management (RC)
- String and collection operations
- Networking and file I/O
- Concurrency primitives (Mutex, Channel, Atomic)
- FFI support for extern "C" functions

## Memory Management

### Reference Counting (RC)

Objects are allocated with a hidden `RcHeader` prefix:

```
[RcHeader: strong | weak | alloc_size][user data...]
```

- `mimi_rc_alloc(size)` — allocate RC object
- `mimi_rc_retain(ptr)` — increment strong count
- `mimi_rc_release(ptr)` — decrement strong count, free when zero
- `mimi_rc_weak_retain/release` — weak reference management
- `mimi_rc_upgrade(ptr)` — upgrade weak to strong (returns null on failure)

**0.31.22 RC ABA Fix**: `mimi_rc_upgrade` now temporarily increments weak count during upgrade to prevent use-after-free races.

### Unified Allocator

**0.31.22**: `mimi_alloc(size)` / `mimi_free(ptr)` wrappers replace direct `libc::malloc/free`:
- Default: `libc::malloc/free` (C ABI compatible)
- `#[cfg(miri)]`: Rust allocator (Miri can detect errors)

## Strings

Strings are C-style null-terminated (`*mut c_char`):
- `mimi_string_new(str)` — allocate from Rust string
- `mimi_string_free(ptr)` — free string
- **Known limitation**: Binary data with `\0` is truncated (fat pointer planned for Phase C)

## Collections

### List

`MimiList` uses a hidden capacity header:
```
[capacity: i64][elements...]
```
- `mimi_list_new()` / `mimi_list_free()`
- `mimi_list_push/pop/get/set`
- **0.31.23 planned**: Typed storage (de-stringification)

### Map/Set

Global handle registry with integer handles:
- `mimi_map_new()` → handle (i64)
- `mimi_map_get/set/remove`
- **Phase C planned**: Fat pointer (de-global-lock)

## Concurrency

Built-in functions (not std::sync):
- `mutex_new()` / `mutex_lock()` / `mutex_unlock()`
- `channel_new()` / `channel_send()` / `channel_recv()`
- `atomic_i32_new()` / `atomic_i32_load()` / `atomic_i32_store()`

**Phase C planned**: Opaque pointer + Generation (thread-safety enforcement)

## Networking

Built-in functions with error interception:
- `socket()` / `bind()` / `listen()` / `accept()`
- `connect()` / `send()` / `recv()` / `close_fd()`
- **0.31.22**: Errors return `InterpError` instead of -1 sentinel

## FFI

extern "C" functions use passport types:
- `CBuffer<T>` — C-compatible buffer
- `c_shared T` / `c_borrow T` / `c_borrow_mut T` — ownership modes
- `raw_string` — ownership transfer (C must free)

## Error Handling

- Contract violations → `abort()` (async-signal-safe)
- Environment failures → `Result<T, E>` or error codes
- **0.31.24 planned**: Typed errors (FsError, JsonError, etc.)

## Optimization Levels

- **O0** (default): No optimization, fastest compile
- **O1** (`MIMI_OPT=1`): Safe optimizations (inline, DCE, SROA)
- **O2** (`MIMI_OPT=2`): **Experimental** — may trigger LLVM bugs
- **O3**: Not recommended

## Thread Safety

- RC operations use atomic CAS (Acquire/Release ordering)
- Global handle registry uses `Mutex` (Phase C: remove)
- Thread-local storage for shadow maps (deprecated)

## Deprecated/Removed

- **Shadow MTE** (0.31.22): Software memory tagging removed (performance overhead)
- **Pinned timeout** (0.31.21): Synchronous timeout abolished (use ForeignTask)
- **WAL/@transactional** (0.31.21): Transaction log removed (Recover = in-place reuse)

## Future Work

- **Phase C**: Component IR, Native ABI, fat pointers, opaque handles
- **0.31.23**: List typed storage, Value clone elimination
- **0.31.24**: Error algebra, defer LIFO, comptime purity