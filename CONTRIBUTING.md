# 贡献指南 / Contributing Guide

感谢你对 Mimi 语言的关注！本文档指引你如何参与贡献。

_Thank you for your interest in Mimi! This document guides you through contributing._

---

## 目录 / Table of Contents

- [行为准则 / Code of Conduct](#行为准则--code-of-conduct)
- [如何贡献 / How to Contribute](#如何贡献--how-to-contribute)
- [开发流程 / Development Process](#开发流程--development-process)
- [提交规范 / Commit Convention](#提交规范--commit-convention)
- [测试要求 / Testing Requirements](#测试要求--testing-requirements)
- [文档要求 / Documentation Requirements](#文档要求--documentation-requirements)
- [问题报告 / Issue Reporting](#问题报告--issue-reporting)

---

## 行为准则 / Code of Conduct

本项目遵循 [贡献者公约 2.1](CODE_OF_CONDUCT.md)。所有参与者须遵守。

_This project adheres to the [Contributor Covenant 2.1](CODE_OF_CONDUCT.md). All participants are expected to uphold this code._

---

## 如何贡献 / How to Contribute

### 🐛 报告 Bug / Report a Bug

1. 搜索已有 [Issues](https://github.com/ontonous/mimi/issues) 确认非重复
2. 使用 Bug 报告模板创建 Issue
3. 提供最小可复现示例

### 💡 提议功能 / Suggest a Feature

1. 先讨论大方向（可在 Discussion 中）
2. 使用功能请求模板创建 Issue
3. 说明设计思路和使用场景

### 🔧 提交代码 / Submit Code

1. Fork 仓库
2. 创建特性分支：`git checkout -b feat/my-feature`
3. 按 [开发流程](#开发流程--development-process) 完成修改
4. 确保所有测试通过
5. 提交 Pull Request

### 📝 完善文档 / Improve Documentation

文档改进始终欢迎！包括：
- 修复拼写/语法错误
- 补充示例
- 完善 API 文档
- 翻译

---

## 开发流程 / Development Process

本项目采用 **Invariant-Driven Development (IDD)**，三层不变量严格遵循：

| 层级 | 名称 | 测试类别 | 提交前必须 |
|---|---|---|---|
| L1 | 双后端等价性 | `cargo test dual_` | ✅ 全部通过 |
| L2 | 类型系统健全性 | `cargo test typecheck::` | ✅ 全部通过 |
| L3 | 内存安全 | Valgrind/ASan | ✅ 零警告 |

### 新增功能流程

```
1. 编写 L1 双后端测试（可暂时 #[ignore]）
2. 在解释器中实现 → L1 通过
3. 在 codegen 中实现 → L1 仍然通过
4. 添加 L2 健全性测试（应拒绝不当用法）
5. 运行 L3 内存检查
6. 提交
```

### 修复 Bug 流程

```
1. 编写重现 Bug 的 L1/L2 测试（应失败）
2. 修复代码
3. 测试通过
4. 思考该 Bug 属于哪类问题，补充回归测试
5. 提交
```

### 构建与测试

```bash
# 首次构建
bash scripts/setup-llvm-wrapper.sh
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo build

# 运行测试
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo test

# Clippy（零通过门禁）
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo clippy --deny warnings

# 格式化
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo fmt -- --check
```

---

## 提交规范 / Commit Convention

提交信息格式：

```
<type>(<scope>): <简短描述>

<详细描述（可选）>

<不变量类别>: L1 / L2 / L3
测试: <测试名> (<文件路径>)
```

### 类型 / Types

| 类型 | 说明 |
|---|---|
| `feat` | 新功能 |
| `fix` | 修复 Bug |
| `test` | 添加/修改测试 |
| `docs` | 文档变更 |
| `refactor` | 重构 |
| `chore` | 基础设施/工具链 |
| `security` | 安全修复 |

### 示例

```
fix(codegen): 修复 match guard 在代码生成中缺失

Guard 条件在 compile_match_expr 中被静默忽略，
导致第一个匹配的 arm 无论 guard 如何都被选中。

L1 测试: dual_match_guard (src/tests/dual_backend.rs)
类别: 模式匹配语义
```

### 阶段化提交 / Phased Commits

禁止将"补测试"与"修代码"混在同一次提交中：

```text
COMMIT A: test(idd): 补充 XXX 的 L1/L2 测试（部分 #[ignore]）
COMMIT B: fix(codegen): 实现 XXX，解除 #[ignore]
COMMIT C: docs: 更新文档
```

---

## 测试要求 / Testing Requirements

### 测试命名规范

| 测试类型 | 命名前缀 | 位置 |
|---|---|---|
| 双后端等价 | `dual_*` | `src/tests/` |
| 类型检查 | `typecheck_*` | `src/tests/` |
| FFI 契约 | `ffi_*` | `src/tests/` |
| 编译时元编程 | `adv_comptime_*` / `adv_quote_*` | `src/tests/` |
| 内存安全 | `e2e_valgrind_*` / `e2e_asan_*` | `src/tests/` |

### 已知差距（Known Gaps）

- 若测试因当前实现限制而暂时失败，须标记 `#[ignore = "原因"]`
- 禁止静默容忍差距
- CI 默认运行不能失败；被忽略测试只在 `--ignored` 时运行

---

## 文档要求 / Documentation Requirements

### 标准库文档

每个 `std/*.mimi` 中的 `pub func` 必须在紧上方用 `// 描述` 格式添加单行注释：

```mimi
// 计算字符串长度
pub func length(s: string) -> i32 { ... }
```

修改标准库后，运行：

```bash
python3 scripts/gen_stdlib_docs.py
```

---

## 问题报告 / Issue Reporting

- 使用 GitHub Issues 跟踪 Bug 和功能请求
- 使用 [Issue 模板](.github/ISSUE_TEMPLATE/) 创建
- 对于合约验证相关问题，请附上 Mimi 源代码和 `mimi verify` 的输出
- 安全问题请通过 [SECURITY.md](SECURITY.md) 中的方式私下报告

---

再次感谢你的贡献！❤️

_Thank you for contributing! ❤️_
