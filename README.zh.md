<div align="center">

# Mimi 语言

**面向类型状态（Typestate-Oriented）的系统编程语言 — Flow 状态机、合约验证与结构化并发**

[![Version](https://img.shields.io/badge/version-0.30.0--dev-blue.svg)](https://github.com/ontonous/mimi)
[![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-3100+%20passed-brightgreen.svg)](#)
[![Flow](https://img.shields.io/badge/flow-v0.29%20complete-orange.svg)](#)
[![Clippy](https://img.shields.io/badge/clippy-zero%20warnings-orange.svg)](#)

解释器 + LLVM 18 Codegen 双后端 · Z3 形式化验证 · 面向类型状态 · Flow 状态机 · 协议/会话类型 · Actor 模型

---
</div>

---

## 什么是 Mimi？

Mimi 是一门**面向类型状态（Typestate-Oriented）** 的系统编程语言。其核心洞见：**用业务控制流抽象（Flow/状态机）平替生命周期标注与 `&mut self`**。每个资源的生命周期绑定到一个业务状态——编译器通过状态转移保证安全，而非借用检查。

```mimi
flow Door {
    state Open   { opened_at: i64 }
    state Closed { locked: bool }

    transition open(Closed) -> Open {
        do { return Open { opened_at: timestamp() } }
    }
    transition close(Open { opened_at }) -> Closed {
        do { return Closed { locked: false } }
    }
    transition lock(Closed) -> Closed {
        do { return Closed { locked: true } }
    }
}
```

编译器自动补全转移矩阵——每个未定义的 (state, event) 对自动生成 `→ Fault`。无遗漏状态，无遗忘转移。

---

## 特性

| 分类 | 特性 | 状态 |
|------|------|------|
| **Flow** | `flow`/`state`/`transition` 声明、状态负载、转移分发 | ✅ v0.29.9 |
| **Flow** | 转移矩阵自动补全（+1 Fault 兜底） | ✅ v0.29.10 |
| **Flow** | Fault 吸收态 + 资源自动析构 | ✅ v0.29.11 |
| **Flow** | SystemTrace 溯源信息（`last_state`、`unexpected_event`、快照） | ✅ v0.29.12 |
| **Flow** | Reset / Recover 系统动词（Fault→根状态，保留持久化字段） | ✅ v0.29.13 |
| **Flow** | Persistent 持久化负载 + `@transactional` WAL 回滚 | ✅ v0.29.14 |
| **Flow** | `delegate view/mutate/consume`（三级权限委托） | ✅ v0.29.15 |
| **Flow** | `pinned { timeout }` FFI 内存锚定 | ✅ v0.29.16 |
| **Flow** | Subflow 同步嵌套（深度优先析构） | ✅ v0.29.17 |
| **Flow** | Protocol 接口抽象（保守投影子类型化） | ✅ v0.29.18 |
| **Flow** | 会话类型：`session`/`dual`/`end`，编译时线性检查 | ✅ v0.29.19 |
| **Flow** | PeerFault 跨 Actor 传播 | ✅ v0.29.20 |
| **Flow** | Mailbox 背压自动治理 | ✅ v0.29.21 |
| **Flow** | 渐进式 Typestate（脚本→隐式 `flow Main { state Single }`） | ✅ v0.29.22 |
| **Flow** | `view`/`mutate` 局部借用（零开销 GEP 传参） | ✅ v0.29.23 |
| **Flow** | Spawn 配额控制（`@max_children(N)`） | ✅ v0.29.24 |
| **Flow** | 多态广播（`Vec<Protocol>`） | ✅ v0.29.25 |
| **Flow** | Protocol methods、session_pair、lifecycle | ✅ v0.29.27–31 |
| **合约** | `requires:` / `ensures:` / `invariant:` 函数内声明 | ✅ |
| **合约** | Z3 SMT 求解器集成（`mimi verify`） | ✅ |
| **合约** | 运行时合约断言（`mimi build --verify-contracts`） | ✅ |
| **Actor** | `actor` 关键字、可变字段、mailbox 分发、worker 线程 | ✅ |
| **双后端** | 解释器（快速开发）+ LLVM 18 codegen（原生编译） | ✅ |
| **泛型** | `<T: Bound>` 类型参数、递归类型 | ✅ |
| **ADT** | 枚举/记录/元组、`match` 穷尽检查、`while let` | ✅ |
| **Option/Result** | `Option<T>` / `Result<T, E>` / `?` 运算符 | ✅ |
| **FFI** | `extern "C"`、`repr(C)`、多语言 bindgen（C/C++/Rust/Go/Node.js/Java/Python） | ✅ |
| **Comptime** | `comptime func` + `quote!` AST 生成 | ✅ |
| **LSP** | 补全、悬停、跳转定义、合约检查镜头 | ✅ |
| **包管理** | `mimi.toml` 清单、registry、git 依赖、依赖树 | ✅ |
| **交叉编译** | `--target` 标志、共享库 `.so` 输出 | ✅ |

---

## 快速开始

### 从源码构建

```bash
git clone https://github.com/ontonous/mimi
cd mimi
bash scripts/setup-llvm-wrapper.sh
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo build --release
```

### Hello, Flow

```mimi
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }

    transition inc(Zero) -> Positive {
        do { return Positive { count: self.count + 1 } }
    }
    transition inc(Positive) -> Positive {
        do { return Positive { count: self.count + 1 } }
    }
    transition reset(Positive) -> Zero {
        do { return Zero { count: 0 } }
    }
}

func main() -> i32 {
    let s0 = Zero { count: 0 }
    let s1 = Counter::inc(s0)
    let s2 = Counter::inc(s1)
    println(s2.count)   // 2
    let s3 = Counter::reset(s2)
    println(s3.count)   // 0
    0
}
```

```bash
./target/release/mimi run counter.mimi
# => 2
# => 0
```

### 运行测试

```bash
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test
```

---

## 标准库（24 模块）

| 模块 | 文件 | 功能 |
|------|------|------|
| `prelude` | `prelude.mimi` | identity、clamp、lerp、compose、pipe、fail、assert_msg |
| `io` | `io.mimi` | print_line、input_line、print_format、IoOps trait |
| `fs` | `fs.mimi` | read、write、exists、read_lines、write_lines、file_size |
| `strings` | `strings.mimi` | split、join、replace_all、capitalize、reverse、truncate、pad |
| `collections` | `collections.mimi` | sort、map、filter、reduce、partition、group_by、chunks、dedup |
| `maps` | `maps.mimi` | get、set、merge、pick、omit、has_key、from_list、filter_keys |
| `set` | `set.mimi` | contains、insert、remove、to_list、is_empty |
| `json` | `json.mimi` | to_json、from_json、get_int、get_bool、get_string、JsonExt trait |
| `net` | `net.mimi` | TCP socket、HTTP fetch/fetch_post、NetError |
| `csv` | `csv.mimi` | parse_csv、serialize_csv |
| `crypto` | `crypto.mimi` | sha256、base64_encode/decode、hex_encode/decode |
| `template` | `template.mimi` | render_template |
| `regex` | (builtins) | regex_match、regex_find、regex_replace |
| `time` / `datetime` | `time.mimi` / `datetime.mimi` | timestamp、sleep_ms、duration、days_from_now、time_since |
| `env` | `env.mimi` | get_var、cli_args、has_var、get_int、get_float |
| `mymath` | `mymath.mimi` | gcd、lcm、factorial、fibonacci、is_prime、is_power_of_two |
| `array` | `array.mimi` | fill、slice、rotate、binary_search |
| `iter` | `iter.mimi` | range、zip、enumerate、take、drop、chain |
| `random` | `random.mimi` | random_int、random_float、random_range |
| `text` | `text.mimi` | slugify、indent、wrap |
| `result` | `result.mimi` | unwrap、map、map_err、and_then、or_else |
| `testing` | `testing.mimi` | assert_eq_int、assert_true、assert_approx_eq_float |

内置并发原语（全局可用）：`Mutex<T>`、`AtomicI32`/`AtomicI64`/`AtomicBool`、`Channel<T>`、`broadcast`。

---

## CLI 命令

| 命令 | 说明 |
|------|------|
| `mimi check <path>` | 类型检查，完整错误报告 |
| `mimi run <path>` | 运行（解释执行），可选 `--verify-contracts` / `--profile` / `--watch` |
| `mimi test <path>` | 运行 `test_*` 函数，支持 `--filter` 和 `--verbose` |
| `mimi build <path>` | 编译为原生二进制（LLVM）。`--emit-ir`、`--shared`、`--target`、`--verify-contracts` |
| `mimi fmt <files>` | 格式化代码（`--check` 用于 CI） |
| `mimi lint <files>` | 静态分析（`--fail-on-warnings`） |
| `mimi verify <path>` | Z3 形式化验证 |
| `mimi lsp` | 启动 LSP 服务器（stdin/stdout） |
| `mimi init [name]` | 初始化 `mimi.toml` |
| `mimi add <name>` | 添加依赖（`--version`、`--git`、`--path`） |
| `mimi remove <name>` | 移除依赖 |
| `mimi install` | 安装依赖（`--frozen`、`--offline`） |
| `mimi update` | 更新依赖 |
| `mimi list` | 列出依赖 |
| `mimi tree` | 显示依赖树 |
| `mimi publish` | 发布到本地 registry |
| `mimi search <query>` | 搜索包 |
| `mimi doc <path>` | 生成文档 |
| `mimi promote <path>` | 升级 `.mms` → `.mimi` |
| `mimi mms <files>` | 处理 MimiSpec 文件 |
| `mimi stats <path>` | 使用统计 |
| `mimi stat <path>` | 目录分析 |
| `mimi bindgen <path>` | 生成多语言 FFI 绑定 |
| `mimi emit-c-headers` / `emit-py-bindings` / `emit-rust-bindings` / `emit-go-bindings` / `emit-node-bindings` / `emit-cpp-bindings` / `emit-java-bindings` | 各语言 FFI 绑定生成 |

---

## 项目结构

```
mimi/
├── src/                       # Rust 编译器（323 文件，~172k LOC）
│   ├── main.rs                # CLI 入口（clap derive）
│   ├── lib.rs                 # 库入口
│   ├── ast.rs                 # AST：FlowDef、StateDef、TransitionDef、ProtocolDef……
│   ├── flow_matrix.rs         # 转移矩阵 + Fault 自动补全（+1 兜底）
│   ├── session.rs             # 会话类型对偶化 + 顺序检查
│   ├── progressive.rs         # 脚本 → 隐式 flow Main { state Single }
│   ├── parser/                # Flow 解析器（严格 Flow 状态机）         ✅ v0.29.0
│   ├── lexer/                 # Flow 词法分析器（严格 Flow 状态机）     ✅ v0.29.1
│   ├── core/                  # 类型推断与检查（宽松 Flow）             ✅ v0.29.8
│   ├── interp/                # 解释器（宽松 Flow）                     ✅ v0.29.6
│   ├── codegen/               # LLVM 18 codegen（via inkwell）
│   │   └── builtins/          # 内置函数 codegen（io、string、json……）
│   ├── verifier/              # Z3 合约验证器（严格 Flow）              ✅ v0.29.7
│   │   └── flow.rs            # 验证器本身是 Flow 状态机
│   ├── ffi/                   # 多语言绑定生成（7 种语言）
│   ├── lsp/                   # LSP 服务器（严格 Flow）                ✅ v0.29.5
│   ├── loader/                # 模块加载器（严格 Flow）                 ✅ v0.29.4
│   ├── runtime/               # Rust 运行时 + actor mailbox + profiler
│   ├── fmt.rs                 # 代码格式化器
│   ├── lint.rs                # 静态分析器
│   ├── main/                  # CLI 子命令实现
│   ├── diagnostic/            # 错误码与格式化
│   └── tests/                 # 3100+ 测试，96 个模块
├── std/                       # 标准库（24 模块）
├── examples/                  # 示例程序（28+）
├── demos/                     # 演示程序（23+）
├── devdocs/                   # 设计文档：白皮书、Flow 草案、ADR
├── scripts/                   # 构建与 CI 脚本
├── Cargo.toml
└── CHANGELOG.md
```

---

## 架构：Flow 范式

编译器本身构建在它要编译的同一 Flow 范式之上——每个模块都是一个状态机：

| 模块 | Flow 度 | 状态 |
|------|---------|------|
| Parser | 严格 Flow | ✅ v0.29.0（454 LOC） |
| Lexer | 严格 Flow | ✅ v0.29.1（970 LOC） |
| Loader | 严格 Flow | ✅ v0.29.4 |
| LSP | 严格 Flow | ✅ v0.29.5 |
| Verifier | 严格 Flow | ✅ v0.29.7 |
| Core Checker | 宽松 Flow | ✅ v0.29.8 |
| Interpreter | 宽松 Flow | ✅ v0.29.6 |
| Codegen | 非 Flow（LLVM API） | N/A |
| Runtime | 非 Flow（C 风格 unsafe） | N/A |
| FFI | 非 Flow（文本生成器） | N/A |

**Flow 范式五条铁律：**
1. 禁止 `&mut self` — 使用 `fn transition(self, event) -> Self`
2. 禁止 `Arc<Mutex<T>>` — 使用 `enum + transition`
3. Flow 模块中禁止 `unsafe`
4. 禁止 `transmute` 和生命周期标注
5. 禁止裸 `panic!`/`unwrap()`/`expect()` — 返回 `Result<Self, Error>`

---

## 开发

### 环境要求

- **Rust** 1.75+
- **LLVM 18**（可用 `scripts/setup-llvm-wrapper.sh` 自动配置）
- **libffi**（FFI 支持）
- **Z3**（合约验证，`cargo build` 自动处理）

### 测试层级

| 层级 | 测试 | 含义 |
|------|------|------|
| **L1** | `cargo test dual_` | 双后端等价性（interp == codegen） |
| **L2** | `cargo test typecheck::` | 类型系统健全性（错误代码被拒绝） |
| **L3** | `cargo test e2e_asan -- --ignored` | 内存安全（Valgrind/ASan/Miri） |

### 常用命令

```bash
# 全量测试
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test

# 双后端等价性（L1）
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test dual_

# 类型系统健全性（L2）
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test "typecheck::"

# 真实世界 MCDD 测试套件
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test real_world

# Clippy（零警告门禁）
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo clippy --all-targets -- -D warnings

# 格式化
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo fmt
```

> **内存注意**：`cargo test` debug 模式可能使用 ~12 GB 内存。内存受限环境请使用 `ulimit -v 20000000` 和 `--test-threads=1`。详见 [AGENTS.md](AGENTS.md)。

---

## 版本历史

| 版本 | 亮点 |
|------|------|
| **v0.30.0** | **止血清零（Hemostasis）**：0 新 Feature — 架构债务修复（sprintf→snprintf、路径安全、malloc 检查、values_equal、build_unreachable、fmt tokenization） |
| **v0.29.41** | 白皮书冻结：全部 38 项能力完成 ✅ |
| **v0.29.37** | Actor 生命周期：SystemKill 级联 + `spawn detached` |
| **v0.29.34** | Session 双端运行时：send/recv/close 推进端点 |
| **v0.29.32** | Pinned 协作式看门狗：`pinned { timeout }` |
| **v0.29.25** | Flow 多态广播、session_pair、mutate 转发 |
| **v0.29.18** | Protocol 接口抽象（保守投影子类型化） |
| **v0.29.14** | Persistent 持久化负载 + `@transactional` WAL 回滚 |
| **v0.29.9** | Flow 语言基座：`state`/`transition` 双后端 |
| **v0.29.0–8** | 编译器内部 Flow 架构替换（Parser→Lexer→Loader→LSP→Interp→Verifier→Checker） |
| **v0.28.37** | Feature Bugs 清零 — v0.28 最终版 |
| **v0.28.0** | 使用驱动：7 语言 FFI、profiler、bindgen、包管理器 |
| **v0.27** | 安全审计：P0/P1/P2/P3（arena、FFI、JSON、runtime） |
| **v0.24** | 结构化并发状态机 |
| **v0.20** | Future/Waker/Executor/poll codegen |
| **v0.15** | C runtime → Rust 运行时重写 |
| **v0.7** | Z3 验证 + FFI codegen |

> 完整更新日志见 [CHANGELOG.md](CHANGELOG.md)。

---

## 许可证

[Apache License 2.0](LICENSE)

版权所有 © 2026 ontonous
