# Mimi 语言参考实现

本目录是 **Mimi**（`.mimi`）语言的 Rust 参考实现。它独立于同仓库的 `mimispec/`（MimiSpec 草图解析器），但共享同一设计哲学：从草图到生产的渐进光谱、意图锁、契约元数据。

## 目录

- `src/` — 编译器/解释器源码
- `mimispec/` — 用 MimiSpec（`.mms`）编写的实现架构与模块规格
- `examples/` — 示例 Mimi 程序

## 架构

实现按阶段分层：

| 模块 | 职责 | 当前状态 |
|------|------|----------|
| `syntax` | lexer、parser、AST | v0.2：支持 `.mimi` 生产模式与 `.mms` 草图模式；新增 ADT、match、tuple、list |
| `core` | 名称解析、类型检查、借用/所有权检查、HIR Lowering | v0.2：基础名称解析 + 类型检查（含 ADT 与 match） |
| `runtime` | 解释器、actor 运行时、`parasteps`/Saga | v0.2：树遍历解释器支持 ADT、match、tuple、list |
| `driver` | `mimi check` / `run` / `build` CLI | v0.2：`check` 自动识别 `.mimi`/`.mms`，`run` 先做类型检查 |

## v0.1 里程碑目标（已完成）

一个**可运行**的最小 Mimi 子集：

- 模块、函数、基本类型（`i32`、`f64`、`bool`、`string`、`unit`）
- `let`、`return`、表达式、`if`/`else`、`while`、`for`
- 函数调用与顶层函数
- `mimi check file.mimi` 语法检查
- `mimi run file.mimi` 调用 `main()` 解释执行

## v0.2 里程碑目标（当前）

- 名称解析：重复定义、未定义变量/函数/构造函数检查
- 基础静态类型检查：返回类型、参数类型、变量初始化、`if`/`while` 条件、`for` 迭代器、运算符类型
- `mimi run` 在执行前先进行类型检查
- 内置函数签名：`println`、`assert`、`range`
- `.mms` 草图模式统一解析：`mimi check file.mms` 自动使用 sketch lexer/parser
- ADT / `match` / `newtype`
- 元组 `(T, U)`、列表 `[T]` 与 let 模式解构
- 记录类型构造与字段访问
- 内置函数：`println`、`assert`、`range`、`sqrt`

未实现（后续里程碑）：

- Move / 借用 / 生命周期
- `actor`、`parasteps`、`on failure`
- `cap` 线性能力
- 形式化契约检查

## 构建

```bash
cd mimi
cargo build --release
./target/release/mimi --help
```

## 使用示例

```bash
# 生产模式：语法 + 类型检查
mimi check examples/hello.mimi

# 生产模式：解释执行
mimi run examples/hello.mimi

# 草图模式：仅解析检查
mimi check mimispec/architecture.mms

# ADT + match 示例
mimi run examples/shapes.mimi

# 记录类型示例
mimi run examples/records.mimi

# newtype 示例
mimi run examples/newtype.mimi
```

## 设计原则

1. **先做小、先做能跑**：每个里程碑必须能 `cargo build` 并通过示例程序。
2. **MMS 驱动架构**：`mimispec/` 下的 `.mms` 文件描述模块边界，Rust 代码负责实现。
3. **与 mimispec 保持边界**：不直接修改 `mimispec/`；Mimi 有自己的 lexer/parser，但可参考其错误恢复与意图后缀处理。
4. **锁信息透传**：AST 保留 `Commitment` 字段，供后续 IDE/Subagent 检查使用。
