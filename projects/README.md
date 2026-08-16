# Mimi 项目集

本目录同时包含两类项目：

1. **跨语言 FFI 验证项目**：在真实 C / Rust / Python 调用端暴露
   Mimi FFI 绑定生成器与 codegen 的 ABI/内存/类型问题。
2. **0.1.7 手写 dogfood 工程**：刻意覆盖 Flow / Session / 合约 / 线性，
   为特性裁决和 DX 宣言提供真实工程证据。

## 0.1.7 Dogfood 项目

| 项目 | 覆盖场景 | 验证 |
|------|----------|------|
| `mimi-taskq` | Flow 任务生命周期 + Session 线性握手 + 合约 | `mimi check/run/test/build` 全通过 |
| `mimi-ledger` | Flow 账户状态 + Session 审计通道 + 合约 | `mimi check/run/test/build` 全通过 |

## 跨语言 FFI 验证项目

| 项目 | 覆盖场景 | 状态 |
|------|----------|------|
| `xlang_math` | 标量、字符串、#[repr(C)] struct-by-value、回调 | 全部正常（v0.28.18） |
| `xlang_strings` | 字符串参数/返回值、内存所有权、i32 返回 | 全部正常（v0.28.18） |
| `xlang_callback` | i32/bool 回调在各种语言中的调用 | 通过跨线程 callback 修复（v0.28.18） |

## 共享运行时 shim

`runtime_shim/` 把 `mimi::runtime` 的 C ABI 符号导出为 `libmimi_runtime_shim.so`，
各项目链接它。长期应让工具链直接产出运行时共享库或静态链接运行时。

## 使用方式

每个项目目录下：

```bash
make      # 构建 .so、生成绑定、编译调用端
make test # 运行 C / Rust / Python 调用测试
```

需要 `mimi` 已构建在 `../target/debug/mimi` 并按 `AGENTS.md` 设置 LLVM wrapper。
