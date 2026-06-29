# xlang_callback — 回调跨语言 FFI 验证

验证 Mimi `extern "C"` 函数接收 C 回调并在不同语言中调用回调的行为。

## 构建与运行

```bash
make
make test
```

## 当前已验证的正确行为

- 无：当前所有回调路径在 C / Rust / Python 中均返回垃圾值。

## 已暴露的未修复问题

### 1. 回调参数/返回值类型宽度与 ABI

- `map_int(f: func(i32) -> i32, x: i32) -> i32` 中，绑定生成器已能生成 `int32_t (*)(int32_t)`，
  但 Mimi codegen 把 `func` 外部参数当作 Mimi 闭包 `{fn_ptr, env_ptr}` 处理，导致 C 函数指针被错误解包。

### 2. 返回 `bool` 的回调

- `filter_int(f: func(i32) -> bool, x: i32) -> bool` 同时暴露 `bool` 作为 extern 返回类型以及
  回调返回类型的 ABI 问题。C 头文件把函数返回值声明为 `bool`（`int8_t`），但编译后的共享库
  可能使用 `i1` 或 `i64`，具体行为未定义。
