# xlang_strings — 字符串跨语言 FFI 验证

验证 Mimi `extern "C"` 字符串参数/返回值在 C / Rust / Python 中的行为与内存所有权。

## 构建与运行

```bash
make
make test
```

## 当前已验证的正确行为

- `greet(name: string) -> string`：C / Rust / Python 均返回 `"Hello, Mimi"`。

## 已暴露的未修复问题

### 1. `i32` 标量 ABI 宽度不匹配

- `char_count(s: string) -> i32` 的 C 头文件目前生成 `int64_t char_count(const char* s)`，
  而 Mimi 源码声明返回 `i32`。实际编译出的共享库对 `i32` 返回值使用 `i64` 寄存器，
  小数值时恰好正确，但存在 L1 不一致风险。

### 2. 字符串所有权边界模糊

- `greet` / `join` 返回 `string`，绑定生成器将其映射为 `char*` 并要求调用方 `mimi_string_free`。
- 需要更系统地验证 C/Rust/Python 端释放后无 use-after-free 或重复释放。
