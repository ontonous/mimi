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
- [文档要求与权威地图 / Documentation & Authority Map](#文档要求与权威地图--documentation--authority-map)
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

仓库统一通过 [`Makefile`](Makefile)、[`scripts/dev-env.sh`](scripts/dev-env.sh) 与 [`scripts/run-gates.sh`](scripts/run-gates.sh) 引导 LLVM 18 环境和门禁（默认安装 `cargo-nextest` 时 `make test` 会自动优先使用）。

```bash
# 查看所有日常命令
make help

# 1. 初始化 /tmp/llvm-wrapper（LLVM 18.1.3，可重复运行且幂等）
make setup

# 2. 快速离线静态与文档门禁（~15–20s：语言文档、V0.2 计划、核心 Python 测试等）
make gates-fast

# 3. 编译检查 / 格式化 / 严格 CI 静态门禁仪表盘
make check
make fmt
make gates       # 依次检查 Tier 1 + unsafe + fmt + MIR audits + clippy (-D warnings)

# 4. 运行测试（L1/L2 快速冒烟、Z3 单线程隔离、完整套件）
make test-quick
make test-z3
make test

# 5. 与 GitHub Actions 对齐的本地 CI 全流程
make ci
```

若需直接调用 `cargo`，请先 `source scripts/dev-env.sh` 或使用 `LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo ...`（与 `.github/workflows/ci.yml` 的 `--locked` 口径一致）。

> **Clippy 门禁说明**：CI（[`.github/workflows/ci.yml`](.github/workflows/ci.yml)）强制执行 `cargo clippy --locked --all-targets -- -D warnings`。在活跃开发分支中如遇历史/WIP warning，请区分本次引入的 lint 与基线问题，不要通过新增全局 `#[allow(...)]` 或放宽不变量来绕过门禁。详情参见 [工作流与代码库梳理报告](docs/developer-workflow-and-audit.md)。

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

## 文档要求与权威地图 / Documentation & Authority Map

### 文档权威层级（冲突时以上层/代码实况为准）

| 主题 | 权威来源 | 说明 |
|---|---|---|
| 当前包版本与特性开关 | [`Cargo.toml`](Cargo.toml) | 当前版本 `0.1.11-dev`，MSRV Rust `1.88`，LLVM `181.3.0` |
| 日常开发命令与 CI 门禁 | [`Makefile`](Makefile)、[`scripts/run-gates.sh`](scripts/run-gates.sh)、[`.github/workflows/ci.yml`](.github/workflows/ci.yml) | 本地与 CI 共享同一套门禁脚本 |
| 开发者工作流与现状审计 | [`docs/developer-workflow-and-audit.md`](docs/developer-workflow-and-audit.md) | Tier 0–3 流程、WIP 阻塞项与结构重构路线 |
| 语言规范与语法参考 | [`docs/language-spec.md`](docs/language-spec.md)、[`docs/syntax-reference.md`](docs/syntax-reference.md) | 受 `scripts/check_language_docs.py` 校验 |
| 运行时与内存模型 | [`ARCHITECTURE.md`](ARCHITECTURE.md) | C ABI runtime 布局、RC 与 handle 约束 |
| 0.2 规划与任务依赖 | [`devdocs/V0.2/README.md`](devdocs/V0.2/README.md)、[`devdocs/V0.2/plan.toml`](devdocs/V0.2/plan.toml) | 受 `devdocs/V0.2/tools/check_plan.py` 校验；`devdocs/archive/` 仅作历史记录 |

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
