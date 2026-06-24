<div align="center">

# 🧬 Mimi 语言

**带合约验证、结构化并发与线性能力的系统编程语言**

_A system programming language with contract verification, structured concurrency, and linear capabilities_

[![Version](https://img.shields.io/badge/version-0.23.0--dev-blue.svg)](https://github.com/ontos-hpc/mimi)
[![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-2127%20passed%20%7C%200%20failed-brightgreen.svg)](#)
[![Clippy](https://img.shields.io/badge/clippy-zero%20warnings-orange.svg)](#)

MimiSpec 生产编译后端 · Z3 形式化验证 · LLVM 原生编译 · 解释器 + Codegen 双后端

---

> **⚠️ 开发阶段声明 / Development Status**  
> Mimi 目前处于 **活跃开发阶段（pre-stable）**，版本号 `0.x` 意味着语言特性、API、标准库和 CLI 界面都可能发生较大变化，**尚不建议用于生产环境**。  
> 我们非常欢迎早期使用者测试、反馈问题并参与讨论——每一个 Issue、每一条建议都对项目至关重要。

</div>

---

## 目录 / Table of Contents

- [参与与社区 / Get Involved](#参与与社区--get-involved)
- [特性概览 / Features Overview](#特性概览--features-overview)
- [快速开始 / Quick Start](#快速开始--quick-start)
- [示例 / Examples](#示例--examples)
- [标准库 / Standard Library](#标准库--standard-library)
- [CLI 命令 / CLI Commands](#cli-命令--cli-commands)
- [项目结构 / Project Structure](#项目结构--project-structure)
- [版本历史 / Version History](#版本历史--version-history)
- [开发 / Development](#开发--development)
- [贡献 / Contributing](#贡献--contributing)
- [许可证 / License](#许可证--license)

---

## 参与与社区 / Get Involved

Mimi 正在快速演进，我们热切期待社区的反馈。无论你是编程语言爱好者、系统软件开发者，还是仅对合约驱动开发感到好奇，这里都有你的一席之地。

### 当前状态 / Current Status

- **语言核心** / Core language：类型系统、借用检查、并发模型等基础组件已成型，仍在不断打磨。
- **标准库** / Standard library：21 个模块可用，覆盖常用场景，接口可能还会调整。
- **工具链** / Toolchain：编译器、LSP、包管理器均可运行，尚未达到 1.0 级稳定性。
- **验证与编译后端** / Verification & codegen：LLVM 原生编译与 Z3 合约验证持续完善，部分高级验证场景可能还不完整。

### 你可以做什么 / How to Get Involved

| 方式 | 路径 |
|---|---|
| **报告 Bug** | 在 [Issues](https://github.com/ontos-hpc/mimi/issues) 中提交，请附上复现步骤、平台信息和最小可复现示例。 |
| **提出特性请求** | 通过 Issue 描述你的使用场景和期望行为。 |
| **改进文档** | 语法参考、标准库注释、示例程序——任何能让 Mimi 更易学的改动都欢迎。 |
| **贡献代码** | 阅读 [CONTRIBUTING.md](CONTRIBUTING.md)，从 `good first issue` 起步。 |
| **编写示例或教程** | 分享你的 Mimi 程序，帮助后来者理解语言特色。 |
| **参与讨论** | GitHub Issues & Discussions 区欢迎提问、分享经验或聊聊设计取舍。 |

### 何时稳定？/ When Will It Be Stable？

目前还没有固定的稳定版发布时间表。团队根据内部路线图和社区反馈逐步推进，阶段性目标记录在 [CHANGELOG.md](CHANGELOG.md) 和各版本里程碑中。如果你依赖某个特性或希望 API 尽早冻结，请在 Issue 中告诉我们——使用场景直接影响优先级。

> 💡 **提示**  
> 即便只是点个 Star，或者告诉朋友你在试用 Mimi，都是对开源社区的支持。

> 💡 **Tip**  
> Even just starring the repo or telling a friend you're trying Mimi makes a difference.

---

## 特性概览 / Features Overview

Mimi 是一套 **MimiSpec** 意图描述语言的生产编译后端，以**合约验证、结构化并发和线性能力**为核心差异化优势。

_Mimi is the production compiler backend for the **MimiSpec** intent-description language, differentiated by **contract verification, structured concurrency, and linear capabilities**._

| 特性 / Feature | 说明 / Description |
|---|---|
| **合约验证** | `requires`/`ensures` 前后置条件 + Z3 形式化验证 + 运行时断言 |
| **结构化并发** | `parasteps` 并行 + `spawn`/`await` + `on failure` LIFO 补偿 |
| **线性能力** | `cap` 类型级别资源追踪 + `Allocator` 自定义分配器 |
| **双后端** | 解释器（快速开发）+ LLVM 18 codegen（原生编译） |
| **借用检查** | `&T`/`&mut T`, 路径敏感, arena 逃逸检测, 重借用 |
| **引用计数** | `shared`/`local_shared`/`weak` 所有权模型 |
| **泛型与生命周期** | `<T: Clone>` 约束, 生命周期 elision, 递归类型 |
| **Option / Result** | `Option<T>` 全路径 + `Result<T, E>` + `?` 运算符 |
| **ADT + 模式匹配** | 枚举/记录/元组, `match` 穷尽性检查, `while let` |
| **FFI** | `extern "C"`, `repr(C)` 结构体直传, 回调, pybind11/C 头导出 |
| **async** | `async fn` → Future 状态机 + Executor 协作式调度 |
| **LSP** | 语言服务器: 补全、悬停、跳转、合约验证镜头 |
| **包管理** | `mimi.toml` + registry + git 依赖 + 依赖树 |
| **标准库** | 21 模块: io, fs, net, json, csv, crypto, regex, template 等 |
| **MimiSpec 集成** | `.mms` 解析, `mms{}` 占位符, 规则一致性检查 |
| **编译目标** | 原生 x86_64, 交叉编译 Windows, 共享库 `.so` |

---

## 快速开始 / Quick Start

### 从源码构建 / Build from Source

```bash
# 克隆 / Clone
git clone https://github.com/ontos-hpc/mimi
cd mimi

# 设置 LLVM 18 环境 / Setup LLVM 18
bash scripts/setup-llvm-wrapper.sh

# 编译 / Build
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo build --release

# 验证 / Verify
./target/release/mimi --version
```

### Hello World

```mimi
func greet(name: string) -> string {
    "Hello, " + name + "!"
}

func main() -> i32 {
    println(greet("世界"));
    0
}
```

```bash
./target/release/mimi run hello.mimi
# => Hello, 世界!
```

### 运行测试 / Run Tests

```bash
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo test
# 2127 passed, 0 failed, 21 ignored
```

---

## 示例 / Examples

### 函数与合约 / Function with Contracts

```mimi
pub func divide(a: i32, b: i32) -> i32 {
    requires: b != 0          // 前置条件: 除数不为零
    ensures:  result == a / b // 后置条件: 结果正确
    a / b
}
```

通过 `mimi build --verify-contracts` 启用 Z3 形式化验证。

### ADT 与模式匹配 / ADT & Pattern Matching

```mimi
type Tree<T> {
    Leaf(T)
    Node(Tree<T>, Tree<T>)
}

func depth<T>(t: Tree<T>) -> i32 {
    match t {
        Leaf(_) => 1,
        Node(l, r) => 1 + max(depth(l), depth(r)),
    }
}
```

### 并发与补偿 / Concurrency & Compensation

```mimi
func process() -> Result<i32, string> {
    let data = fetch_data()?;
    on failure { cleanup(data) }

    let result = compute(data)?;
    on failure { revert(result) }

    Ok(result)
}
```

### FFI 调用 / FFI Calls

```mimi
extern "C" {
    func strlen(s: string) -> i64;
    func puts(s: string) -> i32;
}

func main() {
    let len = strlen("Mimi");
    puts("Hello from Mimi FFI!");
}
```

> 更多示例见 [`examples/`](examples/) 目录（29 个 `.mimi` 程序）。

---

## 标准库 / Standard Library

| 模块 | 文件 | 功能 |
|---|---|---|
| `io` | `io.mimi` | 输入输出: `print_line`, `input_line` |
| `fs` | `fs.mimi` | 文件系统: `read`, `write`, `exists` |
| `strings` | `strings.mimi` | 字符串: `split`, `join`, `replace_all` |
| `collections` | `collections.mimi` | 集合操作: `sort`, `map`, `filter`, `reduce` |
| `maps` | `maps.mimi` | Map 操作: `get`, `set`, `merge`, `pick` |
| `set` | `set.mimi` | Set 操作: `contains`, `insert`, `remove` |
| `json` | `json.mimi` | JSON: `to_json`, `from_json`, 类型化反序列化 |
| `net` | `net.mimi` | 网络: TCP socket, HTTP fetch |
| `csv` | `csv.mimi` | CSV 解析与序列化 |
| `crypto` | `crypto.mimi` | 加密: SHA256, base64, hex |
| `template` | `template.mimi` | 字符串模板渲染 |
| `regex` | (builtins) | 正则匹配/查找/替换 |
| `time` / `datetime` | `time.mimi` / `datetime.mimi` | 时间戳/日期工具 |
| `env` | `env.mimi` | 环境变量/命令行参数 |
| `mymath` | `mymath.mimi` | 数学函数: gcd, lcm, is_prime |
| `random` | `random.mimi` | 随机数工具 |
| `text` | `text.mimi` | 文本: slugify, indent, wrap |
| `result` | `result.mimi` | Result 组合子 |
| `prelude` | `prelude.mimi` | 基础工具: clamp, pipe, compose |
| `testing` | `testing.mimi` | 测试断言 |

---

## CLI 命令 / CLI Commands

| 命令 | 说明 | Description |
|---|---|---|
| `mimi check <file>` | 类型检查 | Type check |
| `mimi run <file>` | 运行（类型检查 + 解释执行） | Run (type check + interpret) |
| `mimi build <file>` | 编译为原生可执行 | Compile to native binary |
| `mimi build --verify-contracts` | 编译并启用合约断言 | Build with contract assertions |
| `mimi test <file>` | 运行 `test_*` 函数 | Run test functions |
| `mimi fmt <files>` | 格式化代码 | Format code |
| `mimi lint <files>` | 静态分析 | Lint |
| `mimi verify <file>` | Z3 合约形式化验证 | Z3 formal verification |
| `mimi lsp` | 启动 LSP 服务器 | LSP server |
| `mimi init <name>` | 初始化项目 | Init project |
| `mimi add <name>` | 添加依赖 | Add dependency |
| `mimi remove <name>` | 移除依赖 | Remove dependency |
| `mimi install` | 安装依赖 | Install dependencies |
| `mimi update` | 更新依赖 | Update dependencies |
| `mimi list` | 列出依赖 | List dependencies |
| `mimi tree` | 显示依赖树 | Dependency tree |
| `mimi publish` | 发布到本地 registry | Publish to local registry |
| `mimi search <query>` | 搜索包 | Search packages |
| `mimi doc <file>` | 生成文档 | Generate docs |
| `mimi promote <file>` | `.mms` → `.mimi` 提升 | Promote MMS to Mimi |
| `mimi mms <files>` | 处理 MimiSpec | Process MimiSpec files |
| `mimi stats <file>` | 使用统计 | Usage stats |
| `mimi emit-c-headers <file>` | 导出 C 头文件 | Emit C headers |
| `mimi emit-py-bindings <file>` | 导出 Python 绑定 | Emit Python bindings |

---

## 项目结构 / Project Structure

```
mimi/
├── src/                   # Rust 源代码 (~88k 行, 285 文件)
│   ├── main.rs            # CLI 入口
│   ├── lib.rs             # 库入口
│   ├── ast.rs             # AST 定义
│   ├── parser/            # 解析器
│   ├── lexer/             # 词法分析
│   ├── core/              # 类型检查 & 推断
│   ├── interp/            # 解释器后端
│   ├── codegen/           # LLVM codegen 后端
│   ├── verifier/          # Z3 形式化验证器
│   ├── ffi/               # FFI 系统
│   ├── lsp/               # LSP 服务器
│   ├── contracts.rs       # 合约提取
│   ├── runtime/           # Rust 运行时 (~2.2k 行)
│   ├── fmt.rs             # 格式化器
│   ├── lint.rs            # 静态分析
│   ├── manifest.rs        # 包清单
│   ├── loader.rs          # 模块加载
│   └── tests/             # 测试套件
├── std/                   # 标准库 (21 模块)
├── examples/              # 示例 (29 个)
├── docs/                  # 文档
│   ├── adr/               # 架构决策记录
│   ├── syntax-reference.md
│   └── ...
├── scripts/               # 构建/测试脚本
├── benches/               # 基准测试
├── CHANGELOG.md           # 完整更新日志
├── CONTRIBUTING.md        # 贡献指南
├── CODE_OF_CONDUCT.md     # 行为准则
├── SECURITY.md            # 安全策略
└── LICENSE                # Apache-2.0
```

---

## 版本历史 / Version History

| 版本 | 亮点 | Highlight |
|---|---|---|
| **v0.23** 🔨 | Z3 深度修复 + 深度审查 | Z3 deep fix + internal audit |
| **v0.22** | 语言补全: Option/泛型嵌套/loop/管道符/LSP 增强 | Language completion |
| **v0.21** | Clippy 清零 + Codegen 缺口关闭 + 文档补齐 | Clippy zero + gap closure |
| **v0.20** | 结构化并发: Future/Waker/Executor/poll codegen | Structured concurrency |
| **v0.19** | 路径敏感 Borrow + 重借用 + 条件返回 | Path-sensitive borrow |
| **v0.18** | 泛型约束 + 生命周期 elision + 内置 trait | Generic bounds + lifetimes |
| **v0.17** | GEP 安全抽象 + 62 处 unsafe 消除 | GEP safety abstraction |
| **v0.16** | FFI 修复 + 效果系统 + 模式匹配穷尽 | FFI fix + effect system |
| **v0.15** | C runtime → Rust 运行时重写 | C runtime → Rust rewrite |
| **v0.14** | 诊断: 错误码 + Z3 调试输出 | Diagnostics: error codes + Z3 output |
| **v0.13** | 验证覆盖: 闭包/spawn/await/字符串 | Verification coverage |
| **v0.12** | FFI 零拷贝 + 加密/CSV/模板标准库 | FFI zero-copy + stdlib |
| **v0.11** | Windows 目标 + 网络标准库 | Windows target + net stdlib |
| **v0.10** | 后端对齐 + CI/CD | Backend alignment + CI/CD |
| **v0.9** | 安全: Arena 逃逸/写竞争检测 | Safety: arena/race detection |
| **v0.8** | 包管理 + 文档管道 | Package management + docs |
| **v0.7** | Z3 验证 + FFI codegen | Z3 verification + FFI codegen |

> 完整更新日志见 [CHANGELOG.md](CHANGELOG.md)。

---

## 开发 / Development

### 环境要求 / Prerequisites

- **Rust** 1.75+
- **LLVM 18**（可用 `scripts/setup-llvm-wrapper.sh` 自动配置）
- **libffi**（FFI 支持）
- **Z3**（合约验证，`cargo build` 自动处理）

### 命令速查 / Command Quick Reference

```bash
# 运行全量测试 / Run all tests
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo test

# L1 双后端等价性测试 / Dual-backend equivalence
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo test dual_

# L2 类型系统健全性测试 / Type system soundness
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo test typecheck::

# Clippy（零警告门禁）
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo clippy --deny warnings

# 格式化 / Format
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo fmt

# 基准测试 / Benchmarks
cargo bench
```

### 开发原则 / Principles

| 层级 | 测试类别 | 含义 |
|---|---|---|
| **L1** | 双后端等价性 | 解释器与 codegen 输出一致 |
| **L2** | 类型系统健全性 | 错误代码被正确拒绝 |
| **L3** | 内存安全 | Valgrind/ASan 零警告 |

---

## 贡献 / Contributing

我们热切欢迎各种形式的贡献。以下是你可能感兴趣的方式：

- **试用 & 反馈**：按快速开始构建项目、运行示例，把遇到的困惑或错误提交为 Issue。
- **文档与翻译**：修正拼写、补充注释、翻译章节，帮助 Mimi 触及更多开发者。
- **编写测试与示例**：贡献 `examples/` 下的小程序，或为已有特性撰写教程。
- **代码贡献**：查阅 [CONTRIBUTING.md](CONTRIBUTING.md) 了解编码规范与提交流程，从 `good first issue` 起步。
- **设计讨论**：在 Issue 区参与语言特性、API 设计、错误消息等方面的讨论——你的使用场景就是最好的设计依据。
- **社区建设**：回答他人提问，在社交媒体上分享项目，帮助构建友好包容的社群。

> 所有参与者须遵守[行为准则](CODE_OF_CONDUCT.md)。安全问题请通过[安全策略](SECURITY.md)私下报告。

---

## 许可证 / License

[Apache License 2.0](LICENSE)

版权所有 © 2026 ontonous