<div align="center">

# Mimi 语言

**Flow-first、面向类型状态（Typestate-Oriented）的系统编程语言**

[![Version](https://img.shields.io/badge/version-0.1.6--dev-blue.svg)](https://github.com/ontonous/mimi)
[![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-5400%2B-brightgreen.svg)](#)
[![Semantics](https://img.shields.io/badge/semantics-Pre--1.0-orange.svg)](#)
[![Clippy](https://img.shields.io/badge/clippy-zero%20warnings-orange.svg)](#)

解释器 + LLVM 18 Codegen 双后端 · Z3 形式化验证 · 稀疏 Flow 状态机 · 线性资源 · Actor 模型 · 会话类型 · Component 边界

---

</div>

## 什么是 Mimi？

Mimi 是一门 **Flow-first、面向类型状态（Typestate-Oriented）** 的系统编程语言。核心洞见：**用业务逻辑状态机（Flow）平替生命周期标注与 `&mut self`**。每个资源的生命周期绑定到一个业务状态——编译器通过状态转移保证安全，而非借用检查。

Mimi 要求五个问题的答案直接从源码和类型中可见：

1. 这个业务对象当前处于**什么状态**？
2. 当前状态允许发生**哪些事件**？
3. 状态变化时资源和所有权**如何转移**？
4. 这个失败是局部返回、状态 Fault，还是并发对端 PeerFault？
5. 哪些错误可以在程序运行前被**拒绝**？

```mimi
flow Order {
    state Pending
    state Paid
    state Shipped

    transition pay(Pending, payment: Payment) -> Paid { ... }
    transition ship(Paid, tracking: Tracking) -> Shipped { ... }
}
```

**稀疏，而非稠密。** 你不需要声明 `pay(Paid)` 或 `ship(Pending)`。这些组合不是"缺失的矩阵格、自动补全到 Fault"——**它们在类型上不存在**。对 `Pending` 的订单调用 `ship` 是编译错误，不是运行时兜底。动态边界（网络、FFI、反序列化）产生类型化的 `UnexpectedEvent` 错误，不伪造业务边。

### 最小心智模型

| 构造 | 唯一职责 |
|------|---------|
| `func` | 无持久状态的同步计算与组合 |
| `flow` | 跨时间存在的业务状态及其合法变化 |
| `actor` | 邮箱、调度、隔离、监督；业务状态由 Flow 承载 |
| `protocol` | Flow 对外可见的静态状态拓扑投影 |
| `session` | 两个线性端点之间的消息顺序 |
| `Result<T, E>` | 同步、可恢复的失败 |
| `Fault` | Flow 不变量破坏或不可继续的状态故障 |
| `view / mutate / consume` | 只读、原位修改、所有权转移权限 |
| `requires / ensures` | 可动态检查或静态证明的合约 |
| `component / foreign` | 跨语言边界，带类型化所有权、错误和 effect |

Mimi 是生产编译后端。意图层设计使用 **MimiSpec**（`.mms`），通过 `mimi promote` 提升为 Mimi。

---

## 设计不变量

架构修正案（2026-07-25）经九轮外部盲审后确立了 10 条设计裁决：

> **⚠ 这是设计裁决，不是冻结的 API。** Mimi 是实验性早期语言，距离 1.0 遥远：破坏性更改可自由进行，"冻结"意味着**锚定**开发，而非**锁定**表面语法。裁决固定的是设计**取向**（稀疏优于稠密、禁止嵌套 Flow、无 WAL）——其表面拼写仍可 break。真正的长期资产只有设计思想与不变量套件（L1/L2/L3 + 双后端等价）。见 CHANGELOG.md。

| # | 不变量 | 含义 |
|---|--------|------|
| 1 | **Sparse 不可逆** | 不存在 dense 模式。未声明的 `(state, event)` 组合是编译错误。 |
| 2 | **禁止嵌套 Flow** | Flow payload 只持有普通数据——handle、原生类型、`shared`/`weak` 引用。永远不持有另一个 Flow 实例。 |
| 3 | **无 WAL** | 编译器不生成事务日志。数据一致性是业务逻辑的职责。 |
| 4 | **Recover = 原位复用** | Recover 保留内存句柄（XPU 上 1GB 张量重新分配是灾难）。不是事务回滚。 |
| 5 | **Generation 由逃逸分析决定** | 无显式语法。局部句柄：零开销。跨边界句柄：编译器 Lowering 时自动打包 Generation。 |
| 6 | **View/Mutate 是 1.0 必须** | `view`/`mutate` 借用用于纯函数参数传递。不是 nice-to-have。 |
| 7 | **Multi-target 是 1.0 必须** | `transition parse(Pending) -> Connected \| Rejected`，保留名义 state tag。 |
| 8 | **跨 FFI 失败 = Fault** | 跨 FFI 失败必须进入 Fault，绝不能 Rejected。无法撤销 C 已改变的外部状态。 |
| 9 | **`?` 之前禁止线性消费** | Checker 静态拦截：fallible 操作之前消费线性资源 → 编译错误。 |
| 10 | **无同步 Pinned 超时** | 可能挂死的 C 函数放到 ForeignTask（异步）。不存在同步看门狗。 |

> 完整修正案：13 条款 + 10 不变量。与白皮书冲突时以修正案为准。

---

## 特性

### Flow 核心

| 特性 | 状态 |
|------|------|
| `flow` / `state` / `transition` 声明、状态负载、转移分发 | ✅ |
| 稀疏转移图——未声明 `(state, event)` = 编译错误（`@sparse`） | ✅ |
| 类型化 Fault——per-flow `fault ErrorType` 声明 | ✅ |
| `return S{}` 终止符——唯一转移终止（become/stay 已按 ADR-001 删除） | ✅ |
| `fails E` 可回滚路径——`?` 返回 `Err((source, error))`，source generation 归还 | ✅ |
| Reset / Recover 系统动词（用户可覆盖） | ✅ |
| SystemTrace 溯源（`last_state`、`unexpected_event`、快照） | ✅ |
| 渐进模式——脚本 `main()` 经隐式 `flow Main { state Single }` 注入（真正的语义脱糖，spec §3.13） | ✅ |
| 多目标转移（`-> A \| B`，保留 state tag） | ✅（stable tagged-union ABI，0.34.15-16，ADR-002） |

### 线性安全与所有权

| 特性 | 状态 |
|------|------|
| Flow 状态 use-after-move 拒绝（E0423） | ✅ |
| 别名链、闭包捕获、集合/元组插入拒绝（E0427） | ✅ |
| CFG 级线性——`is_linear()` 对 Flow 状态的 dataflow 分析 | ✅ |
| Session 端点线性——scope exit（E0425）、use-after-alias（E0426） | ✅ |
| 线性资源的 shared/weak 包装拒绝 | ✅ |
| View/mutate 借用（纯函数参数传递） | ✅（0.34.13-14 闭合 + 0.34.25c place 文法 fail-closed，E0434/E0435） |
| 跨 turn exactly-once 资源追踪 | ✅ |
| Channel/Mutex/Atomic 类型级线性 | 📋（已知限制：builtin 整数 handle） |

### 并发

| 特性 | 状态 |
|------|------|
| `actor Name runs FlowName`——Actor 业务状态由 Flow 承载 | ✅（解释器；codegen 待实现） |
| 会话类型：`session` / `dual` / `end`，编译时 residual 检查 | ✅ |
| Protocol 接口抽象（保守投影子类型化） | ✅ |
| PeerFault 跨 Actor 传播 | ✅ |
| 邮箱背压自动治理 | ✅ |
| 生成配额控制（`@max_children(N)`） | ✅ |
| 多态广播（`Vec<Protocol>`） | ✅ |

### 合约与验证

| 特性 | 状态 |
|------|------|
| 函数体内 `requires:` / `ensures:` / `invariant:` | ✅ |
| Z3 SMT 求解器集成（`mimi verify`） | ✅ |
| 运行时合约断言（`mimi build --verify-contracts`） | ✅ |

### 双后端与类型系统

| 特性 | 状态 |
|------|------|
| Bytecode VM 解释器（0.1.3 起唯一解释器）+ LLVM 18 codegen（原生二进制）——L1 等价测试 | ✅ |
| Hindley-Milner 类型推断（undo trail + TypeScheme + zonk） | ✅ |
| 泛型 `<T: Bound>`、递归类型 | ✅ |
| 枚举 / 记录 / 元组，`match` 穷尽性，`while let` | ✅ |
| `Option<T>` / `Result<T, E>` / `?` 操作符 | ✅ |

### FFI、Comptime 与工具链

| 特性 | 状态 |
|------|------|
| `extern "C"`、`repr(C)`、多语言 bindgen（C/C++/Rust/Go/Node.js/Java/Python） | ✅ |
| `comptime func` + `quote!` AST 生成 | ✅ |
| LSP：补全、悬停、跳转定义、合约 lens | ✅ |
| 包管理器：`mimi.toml`、registry、git 依赖、依赖树 | ✅ |
| 交叉编译：`--target` 标志、共享库 `.so` 输出 | ✅ |
| Component IR + Native ABI + Wire Schema | ✅（0.1.1 Phase C；SDK conformance 全绿） |

---

## 快速开始

### 构建

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
        return Positive { count: self.count + 1 }
    }
    transition inc(Positive) -> Positive {
        return Positive { count: self.count + 1 }
    }
    transition reset(Positive) -> Zero {
        return Zero { count: 0 }
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

`Counter::inc(s0)` **消费** `s0`——transition 之后继续使用 `s0` 是编译错误（E0423）。每次 transition 产生状态的新 generation。

### 运行测试

```bash
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test
```

---

## 架构

### CheckedProgram：语义中枢

所有后端（解释器、LLVM codegen、Z3 验证器）消费唯一真值源：**CheckedProgram**。没有后端重查 AST 或重猜类型。

```
Source → Lexer → Parser → AST
  → HM 推断 → 类型检查器 → CheckedProgram
    → Typed Resolved IR（canonical 签名、目录、物化类型）
    → CFG（per-callable 控制流图）
    → Resource Analysis（线性资源动作）
      ↓
  ┌───────────┼───────────┐
  解释器        Codegen      验证器
  (from_checked) (compile_checked) (verify_checked)
```

**铁律**：后端不能回退 raw AST。声明层（签名、Flow 转移、Actor/Session、ownership、CFG）全量从 CheckedProgram 安装；函数体经 per-function dispatch 编译（resolved native emitter + 显式可观测 legacy arm）。`CheckedProgram::raw_ast()` 为 crate 内部，仅限 3 个永久 consumer（codegen 第五遍 / 解释器 / Z3 验证器）。

### 依赖主链

```
Span/Origin → HM → CFG/ownership → CheckedProgram/Resolved IR
  → Flow generation/turn → Actor/Session/resource → semantic trace
  → Verified Core
  → Component IR → Native ABI → Wire → Rust SDK / XPU FFI
```

### 核心抽象

| 抽象 | 位置 | 职责 |
|------|------|------|
| **CheckedProgram** | `src/core/checker/` | 唯一语义中枢：canonical 签名、目录、物化类型 |
| **Typed Resolved IR** | `src/core/resolved/` | ResolvedFunction / ResolvedFlow / ResolvedTransition / ResolvedActor |
| **HM 统一** | `src/core/unification.rs` | Undo trail + TypeScheme + zonk；泛型调用 fresh instantiate |
| **TypeFolder** | `src/core/type_folder.rs` | Binder-aware 类型折叠（SurfaceTy / InferTy / ZonkedTy / BackendTy） |
| **CFG** | `src/core/cfg/` | Per-callable 控制流图，stable-ID CallableCfg |
| **Resource Analysis** | `src/core/ownership.rs` | 线性资源 ledger（Introduce / Move / Drop / Return + borrow），canonical 动作类型 |
| **AstNodeMeta** | `src/span.rs` | SourceId + Span + AstOrigin；NodeIdBuilder 稳定身份 |

### 编译器内部 Flow 范式

编译器自身建立在 Flow 范式之上——每个前端模块都是状态机，签名为 `fn transition(self, event) -> Self`。五条铁律：禁止 `&mut self`、禁止 `Arc<Mutex<T>>`、禁止 `unsafe`、禁止 `transmute`/生命周期标注、禁止裸 `panic!`/`unwrap()`。Parser、Lexer、Loader、LSP、Verifier：严格 Flow。Interpreter、Core Checker：宽松 Flow。Codegen、Runtime、FFI：非 Flow（LLVM API / C 风格 / 文本生成）。

---

## 标准库（24 个模块）

| 模块 | 描述 |
|------|------|
| `prelude` | identity, clamp, lerp, compose, pipe, fail, assert_msg |
| `io` | print_line, input_line, print_format, IoOps trait |
| `fs` | read, write, exists, read_lines, write_lines, file_size |
| `strings` | split, join, replace_all, capitalize, reverse, truncate, pad |
| `collections` | sort, map, filter, reduce, partition, group_by, chunks, dedup |
| `maps` | get, set, merge, pick, omit, has_key, from_list, filter_keys |
| `set` | contains, insert, remove, to_list, is_empty |
| `json` | to_json, from_json, get_int, get_bool, get_string, JsonExt trait |
| `net` | TCP socket, HTTP fetch/fetch_post, `Result<T, NetError>` |
| `csv` | parse_csv, serialize_csv |
| `crypto` | sha256, base64_encode/decode, hex_encode/decode |
| `template` | render_template |
| `time` / `datetime` | timestamp, sleep_ms, duration, days_from_now, time_since |
| `env` | get_var, cli_args, has_var, get_int, get_float |
| `mymath` | gcd, lcm, factorial, fibonacci, is_prime, is_power_of_two |
| `array` | fill, slice, rotate, binary_search |
| `iter` | range, zip, enumerate, take, drop, chain |
| `random` | random_int, random_float, random_range |
| `text` | slugify, indent, wrap |
| `result` | unwrap, map, map_err, and_then, or_else |
| `testing` | assert_eq_int, assert_true, assert_approx_eq_float |
| `effects` | Stdlib effect 标注（类型检查器纯度约束） |
| `errors` | 类型化错误枚举（FsError, JsonError, CollectionError）+ From 协议 |

内建正则（始终可用）：`regex_match`、`regex_find`、`regex_replace`。

内建并发原语（始终可用）：`Mutex<T>`、`AtomicI32`/`AtomicI64`/`AtomicBool`、`Channel<T>`、`broadcast`。

---

## CLI 命令

| 命令 | 描述 |
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
| `mimi promote <path>` | 提升 `.mms` → `.mimi` |
| `mimi mms <files>` | 处理 MimiSpec 文件 |
| `mimi stats <path>` | 使用统计 |
| `mimi stat <path>` | 目录分析 |
| `mimi bindgen <path>` | 生成多语言 FFI 绑定 |
| `mimi emit-*-bindings` | 语言特定 FFI 绑定生成（C/C++/Rust/Go/Node.js/Java/Python） |

---

## 项目结构

```
mimi/
├── src/                        # Rust 编译器（366 文件，~305k LOC）
│   ├── main.rs                 # CLI 入口（clap derive）
│   ├── lib.rs                  # 库入口
│   ├── ast.rs                  # AST：FlowDef, StateDef, TransitionDef, ProtocolDef, ...
│   ├── span.rs                 # SourceId / Span / AstNodeMeta——稳定节点身份
│   ├── flow_matrix.rs          # 转移矩阵 + Fault 注入
│   ├── session.rs              # Session 类型对偶 + 顺序检查
│   ├── progressive.rs          # 脚本 → 隐式 flow Main { state Single }
│   ├── trace.rs                # Canonical 语义追踪（Transition / Fault / OwnershipTransfer）
│   ├── path_safety.rs          # 统一路径验证
│   ├── source_scan.rs          # 共享 SourceScanner（fmt/lint）
│   ├── parser/                 # Flow 解析器（严格 Flow 状态机）
│   ├── lexer/                  # Flow 词法分析器（严格 Flow 状态机）
│   ├── core/                   # 类型推断与检查 → CheckedProgram
│   │   ├── checker/            # 类型检查器 → CheckedProgram 语义中枢
│   │   ├── resolved/           # Typed Resolved IR（canonical 声明）
│   │   ├── unification.rs      # HM 统一（undo trail + TypeScheme）
│   │   ├── type_folder.rs      # Binder-aware 类型折叠
│   │   ├── cfg/                # Per-callable 控制流图
│   │   ├── ownership.rs        # 线性资源分析（canonical 动作）
│   │   └── infer/              # HM 类型推断 + 合约推导
│   ├── interp/                 # Bytecode VM（0.1.3 起唯一解释器）
│   │   └── bytecode/           # Bytecode 编译器 + VM + builtin 注册表
│   ├── codegen/                # LLVM 18 codegen（compile_checked）
│   │   └── builtins/           # 内建函数 codegen（io, string, json, ...）
│   ├── verifier/               # Z3 合约验证器（verify_checked）
│   ├── ffi/                    # 多语言绑定生成（7 种语言）
│   ├── lsp/                    # LSP 服务器（严格 Flow）
│   ├── loader/                 # 模块加载器（严格 Flow）
│   ├── runtime/                # Rust 运行时 + actor 邮箱 + profiler
│   ├── fmt.rs                  # 代码格式化器
│   ├── lint.rs                 # 静态分析器
│   ├── main/                   # CLI 子命令实现（24 个命令）
│   ├── diagnostic/             # 错误码与格式化
│   └── tests/                  # 4500+ 测试
├── std/                        # 标准库（24 个模块）
├── examples/                   # 示例程序（28 个）
├── demos/                      # 演示程序（23 个）
├── tests/real_world/           # MCDD 真实程序双后端套件（69 个程序）
├── scripts/                    # 构建与 CI 脚本
├── Cargo.toml
└── CHANGELOG.md
```

---

## 开发

### 前置条件

- **Rust** 1.75+
- **LLVM 18**（通过 `scripts/setup-llvm-wrapper.sh` 自动配置）
- **libffi**（FFI 支持）
- **Z3**（合约验证；由 `cargo build` 处理）

### 测试层级（IDD）

| 层级 | 测试 | 含义 |
|------|------|------|
| **L1** | `cargo test dual_` | 双后端等价性（解释器 == codegen） |
| **L2** | `cargo test typecheck::` | 类型系统健全性（坏代码被拒绝） |
| **L3** | `cargo test e2e_asan -- --ignored` | 内存安全（Valgrind/ASan/Miri） |

### 常用命令

```bash
# 全量测试
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test

# 双后端等价性（L1）
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test dual_

# 类型系统健全性（L2）
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test "typecheck::"

# 真实程序 MCDD 套件
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test real_world

# Clippy（零警告门禁）
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo clippy --all-targets -- -D warnings

# 格式化
LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo fmt
```

> **测试提示**：完成测试性能优化后，日常全量门禁为 `ulimit -v 20000000 && LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper cargo test -- --test-threads=4`（约 42 秒）。Z3 验证子集仍使用单线程；极端内存受限环境可退回 `--test-threads=1`。debug 模式仍可使用约 12 GB 内存。详见 [AGENTS.md](AGENTS.md)。

---

## 状态

**当前版本**：0.1.9-dev。0.1.7 已发布（2026-08-19）：Wave-3 基建诚实收口；0.1.8 门禁全绿（语义诚实 + 身份纯度：L1 spawn、生产 dual、Narrow、值 ABI、Flow S / Actor A / Session K）；0.1.9 进行中（linear T + 权限闭环）。尚未宣称 VM≡native。


### 文档索引与外部盲审

精选关键文档与九轮外部盲审索引均汇总于 CHANGELOG.md。


---


## 版本历史

### 1. 当前版本
- **0.1.9-dev**（当前）：在 0.1.8 语义诚实 + 身份纯度基础上，延续至 linear T + 权限闭环（0.1.9）。见 CHANGELOG.md。

### 2. 当前大版本（0.1.x）
- **0.1.0 → 0.1.8**：CheckedProgram 语义中枢、Typed Resolved IR、HM 统一、CFG/ownership、Bytecode VM 唯一解释器、Codegen 全量迁移、黄金文档 + 语法冻结（0.1.4）、  核心深度闭环（0.1.6）、Wave-3 基建诚实收口（0.1.7）。逐 minor 细节见 CHANGELOG.md。

### 3. PRE0.1（v0.7 – v0.30）
- v0.7（Z3 + FFI codegen）→ v0.30（止血，清零 15 项架构债务）。1863 commits、  66 个 `mimi-v*` tag。详细历史见 CHANGELOG.md。

> 完整变更日志：[CHANGELOG.md](CHANGELOG.md)。


---

## 许可证

[Apache License 2.0](LICENSE)

Copyright © 2026 ontonous
