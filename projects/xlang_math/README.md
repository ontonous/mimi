# xlang_math — 跨语言 FFI 调用验证项目

该项目把 Mimi 模块 `xmath.mimi` 编译为共享库，并用 C、Rust、Python 真实调用，以暴露绑定生成器与运行时的实际 issue。

## 构建

```bash
make
```

需要：
- `mimi` 已构建在 `../../target/debug/mimi`
- LLVM wrapper 已按 `AGENTS.md` 设置（`LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper`）
- `rustc`、`gcc`、`g++`、`python3` + `pybind11`

## 运行

```bash
make test
```

## 当前已验证的正确行为

- `add(i32, i32) -> i32`：C / Rust / Python 均返回 5
- `greet(string) -> string`：返回 `"Hello, Mimi"`，且字符串被正确释放

## 已暴露的未修复问题

### 1. `#[repr(C)]` record 传值 ABI 不一致

- `point_sum(Point) -> i32` 与 `make_point(i32, i32) -> Point` 在 C / Rust / Python 中均返回错误值。
- 反汇编显示 Mimi 编译器把 struct 参数展开为多个 `int64` 寄存器参数，并把 struct 返回值拆到 `rax:rdx`，但 C 头文件/绑定按正常 struct-by-value 声明，导致 ABI 不匹配。
- 这属于 codegen 层的 L1 双端等价性问题。

### 2. callback 调用 ABI / 参数传递错误

- `apply_callback(f: func(i32, i32) -> i32, x: i32) -> i32` 在 C / Rust / Python 中均返回垃圾值。
- 绑定生成器现已能生成签名正确的 C 函数指针，但 Mimi 编译器对 `func(...)` 外部参数的调用似乎按闭包 ABI 处理，参数寄存器使用不正确。
- 这也属于 codegen 层的 L1 问题。

### 3. 运行时库分发

- 绑定代码链接 `-lmimi_runtime`，但 `mimi build --shared` 并不产出 `libmimi_runtime.so`。
- 本项目用 `runtime_shim/` 小 crate 把 `mimi::runtime` 的 C ABI 符号重新导出为 `libmimi_runtime_shim.so`，客户端再链接它。
- 长期应让工具链直接产出运行时共享库或把运行时静态链接进用户共享库。

## 文件说明

| 文件 | 说明 |
|------|------|
| `xmath.mimi` | Mimi 源码（标量、字符串、结构体、回调） |
| `c_main.c` | C 调用示例 |
| `rust_main.rs` | Rust 调用示例 |
| `python_test.py` | Python 调用示例 |
| `runtime_shim/` | 导出 Mimi runtime C ABI 的 cdylib |
| `bindings/` | `mimi bindgen` 生成的绑定文件 |
