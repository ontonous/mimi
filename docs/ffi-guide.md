# Mimi FFI 开发者指南

本指南面向需要在 Mimi 与其他语言（C/C++/Rust/Go/Python/Node.js/Java）之间建立互操作的开发者。

## 1. 两种 FFI 方向

| 方向 | Mimi 语法 | 用途 |
|---|---|---|
| Mimi → C | `extern "C" { func foo() }` | Mimi 调用 C 共享库函数 |
| C → Mimi | `extern "C" func foo() { ... }` | 将 Mimi 函数导出为 C ABI，供其他语言调用 |

## 2. 导出 Mimi 函数

```mimi
extern "C" func add(a: i32, b: i32) -> i32 {
    a + b
}
```

编译为共享库：

```bash
mimi build --shared math.mimi -o libmath.so
```

`--shared` 会生成位置无关代码并导出 `extern "C"` 函数符号。

## 3. 生成绑定

```bash
mimi bindgen math.mimi -o bindings
```

绑定生成器会同时读取 `extern "C" { ... }` 声明和 `extern "C" func ...` 导出函数，为 7 种目标语言生成桥接代码。

## 4. 类型映射速查

| Mimi 类型 | C ABI | Rust 绑定 | Go 绑定 | Python 绑定 |
|---|---|---|---|---|
| `i32` / `i64` / `bool` | `int64_t` | `i64` / `c_longlong` | `int64` | `int` |
| `f64` | `double` | `f64` / `c_double` | `float64` | `float` |
| `string` | `char*` | `String` / `*mut c_char` | `string` | `str` |
| `#[repr(C)] record` | `struct X` | `MimiX` | `X` | `X` |
| `func(...) -> ...` | 函数指针 | `unsafe extern "C" fn(...)` | `func(...)` | `Callable[..., Any]` |
| `c_shared T` | `int64_t` handle | `i64` | `int64` | `int` |

## 5. 内存所有权

### 5.1 字符串

- **Borrowed**：C 侧不释放，Mimi 保留所有权。绑定生成器通常不自动释放。
- **Owned/Transfer**：C 侧获得所有权，必须调用 `mimi_string_free` 释放。

### 5.2 `#[repr(C)]` record

按值传递，不涉及堆分配。绑定生成器会为目标语言生成 layout-compatible 的结构体，字段顺序与类型必须严格一致。

### 5.3 shared / cap

`c_shared T` 和 `Cap` 以不透明 handle（`int64_t`）穿越边界。handle 由 Mimi 运行时表管理，详细生命周期见 `src/ffi/runtime.rs`。

## 6. 回调

当前状态：

- ✅ Rust：生成 `unsafe extern "C" fn` 函数指针类型，可直接传递 Rust 函数。
- ✅ C++：使用 `std::function` + thread-local slot + C trampoline。
- ✅ Go：使用类型别名 + `//export` trampoline + package-level slot。
- ✅ Python：使用 `std::function` + thread-local slot + C trampoline，`.pyi` 输出 `Callable`。
- ✅ Node.js：使用 N-API env/ref slot + thread-local 存储 + C trampoline，`.d.ts` 输出具体函数签名。
- ⏳ Java：生成类型签名，但 idiomatic 闭包包装仍在开发中。

## 7. 错误处理

- FFI 合约违反会触发运行时错误处理程序（`mimi_runtime_set_error_handler`）。
- Python / C++ 绑定将错误转换为语言级异常。
- 对于 `errno` 函数（如 `open`、`read`），Python 绑定会自动检查 `errno` 并抛出 `OSError`。

## 8. 示例

完整可编译示例见 `examples/ffi/` 与 `demos/`：

- `examples/ffi/math.mimi` — 含标量、字符串、结构体、回调的综合示例。
- `demos/c_ffi_layer/` — C 调用 Mimi。
- `demos/rust_ffi/` — Rust 调用 Mimi。
- `demos/python_ffi/` — Python 调用 Mimi。
