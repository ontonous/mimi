# Mimi FFI 类型映射矩阵

> **Authority**: `docs/language-spec.md` §7 (Component Boundary, Native Cross-boundary Types).
> This document describes the current transitional FFI type mapping.
> The 1.0 target uses typed Component IR (`.mimiabi`) with `ffi view/mutate/owned/shared/handle` ABI modes.
> Version: v0.30.0
> Last updated: 2026-07-17

## 1. 类型映射表

| Mimi 类型 | C | C++ | Rust | Go | Node.js (N-API) | Java (JNI) | Python | TypeScript |
|-----------|---|-----|------|----|-----------------|------------|--------|-----------|
| `i32` | `int32_t` | `int32_t` | `i32` | `int32` | `number` | `int` | `int` | `number` |
| `i64` | `int64_t` | `int64_t` | `i64` | `int64` | `number` | `long` | `int` | `number` |
| `f64` | `double` | `double` | `f64` | `float64` | `number` | `double` | `float` | `number` |
| `bool` | `int` (0/1) | `bool` | `bool` | `bool` | `boolean` | `boolean` | `bool` | `boolean` |
| `string` | `char*` | `std::string` | `&str`/`String` | `string` | `string` | `String` | `str` | `string` |
| `unit` | `void` | `void` | `()` | (empty) | `undefined` | `void` | `None` | `void` |
| `*T` | `void*` | `void*` | `*const c_void` | `unsafe.Pointer` | `number` | `long` | `int` | `number` |
| `*mut T` | `void*` | `void*` | `*mut c_void` | `unsafe.Pointer` | `number` | `long` | `int` | `number` |
| `List<T>` | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| `Map<K,V>` | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| `Result<T,E>` | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| `Option<T>` | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Record | `void*` (handle) | `void*` | `*mut MimiX` | `unsafe.Pointer` | `number` | `long` | `int` | `number` |
| Enum | `int64_t` (disc) | `int64_t` | `i64` | `int64` | `number` | `long` | `int` | `number` |
| Callback | `void*` (fn ptr) | `void*` | `*const c_void` | `unsafe.Pointer` | `Function` | `long` | `callable` | `Function` |

> ❌ = 不能直接跨 FFI 边界，需要序列化为 JSON/string

## 2. 字符串传递规则

| 方向 | C | C++ | Rust | Go | Node.js | Java |
|------|---|-----|------|----|---------|------|
| Mimi → 外部 | `char*` (调用者释放) | `MimiString` (RAII) | `CString` → `*const c_char` | `C.CString` → `defer free` | `napi_get_value_string_utf8` | `GetStringUTFChars` |
| 外部 → Mimi | `char*` (Mimi 拥有) | `const std::string&` | `&str` / `CString` | `string` | `napi_create_string_utf8` | `NewStringUTF` |
| 释放 | `mimi_string_free(ptr)` | `~MimiString()` 自动 | `CString::drop()` | `C.free()` | 自动 GC | `ReleaseStringUTFChars` |

## 3. 错误传播机制

| 语言 | 错误传播方式 |
|------|------------|
| C | 返回负值/null，检查 errno |
| C++ | 返回 `MimiString`（空 = 错误）或抛异常 |
| Rust | 返回 `Result<T, E>`，使用 `?` 传播 |
| Go | 返回 `(value, error)` 元组 |
| Node.js | N-API 抛异常，JS 侧 try/catch |
| Java | JNI 抛 `RuntimeException`，Java 侧 try/catch |
| Python | 返回 `None` 或抛 `RuntimeError` |
| TypeScript | 返回类型包含 `null`，检查后使用 |

## 4. 内存管理边界

| 规则 | 说明 |
|------|------|
| **Mimi 分配，Mimi 释放** | `mimi_string_free()` 释放 Mimi 堆字符串 |
| **C 分配，C 释放** | `malloc`/`free` 或 `libc::free` |
| **RAII 封装** | C++ `MimiString`、Rust `CString` 自动释放 |
| **GC 语言** | Go/Node.js/Java 通过 finalizer 或 `defer` 释放 |
| **Handle 表** | `c_shared`/`c_borrow` 类型通过引用计数管理 |

## 5. 使用示例

### 5.1 `mimi bindgen` 一键生成

```bash
mimi bindgen mylib.mimi -o bindings/
# 生成: mylib.h, mylib.hpp, mylib.rs, mylib.go, mylib_napi.c, mylib.d.ts, mylib_jni.c, Mylib.java
```

### 5.2 单语言生成

```bash
mimi emit-c-headers mylib.mimi -o mylib.h
mimi emit-cpp-bindings mylib.mimi -o mylib.hpp
mimi emit-rust-bindings mylib.mimi -o mylib.rs
mimi emit-go-bindings mylib.mimi -o mylib.go
mimi emit-node-bindings mylib.mimi -o mylib_napi.c --ts mylib.d.ts
mimi emit-java-bindings mylib.mimi -o mylib_jni.c --java Mylib.java
mimi emit-py-bindings mylib.mimi -o mylib.cpp
```

### 5.3 编译共享库

```bash
mimi build mylib.mimi --shared -o libmylib.so
# 然后链接生成的绑定代码
```
