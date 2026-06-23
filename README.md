# Mimi 语言

**Mimi** 是一门带合约验证、结构化并发和线性能力的系统编程语言，是 MimiSpec 意图描述的生产编译后端。

基于双后端架构（**解释器** + **LLVM codegen**）和 **Z3 定理证明器**，Mimi 将函数契约（`requires`/`ensures`）、并行步骤（`parasteps`/`on failure`）和线性能力（`cap`）作为一等语言构造，保障程序正确性。

- **合约验证**：编译期 Z3 形式化验证 + 运行时断言
- **结构化并发**：`parasteps` 并行 + `on failure` 补偿
- **线性能力**：`cap` 类型级别资源追踪
- **双后端**：解释器快速开发 + LLVM 原生编译
- **FFI 零开销**：`extern "C"` 跨语言调用，`repr(C)` 结构体直传
- **包管理**：`mimi.toml` + registry + git 依赖

---

## 快速开始

### 从源码构建

```bash
git clone https://github.com/ontonous/mimi
cd mimi
bash scripts/setup-llvm-wrapper.sh
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo build --release
```

### Hello World

```mimi
func greet(name: string) -> string {
    "Hello, " + name + "!"
}

func main() -> i32 {
    println(greet("World"));
    0
}
```

```bash
./target/release/mimi run hello.mimi
```

---

## 特性

| 特性 | 说明 |
|------|------|
| 类型系统 | `i32`/`i64`/`f64`/`bool`/`string`/`unit`/`nothing`，泛型 `<T>`，`newtype`，类型别名 |
| ADT + 模式匹配 | 枚举、记录、元组，`match` 穷尽性检查 |
| 函数与闭包 | `func` 命名函数，`fn` 匿名闭包，一等函数类型 `func(T) -> U` |
| 合约 | `requires`/`ensures` 前后置条件，`old()` 入口快照，Z3 形式化验证 |
| 结构化并发 | `parasteps` + `spawn`/`await`（pthread 线程池），`on failure` LIFO 补偿 |
| 线性能力 | `cap` 类型级别能力系统，`Allocator` 自定义分配器 |
| 借用检查 | `&T`/`&mut T`，路径敏感分析，arena 逃逸检测 |
| 引用计数 | `shared`/`local_shared`/`weak` 所有权模型 |
| Move 语义 | Copy trait，use-after-move 检测 |
| FFI | `extern "C"` 声明，`repr(C)` 结构体直传，回调支持，json 序列化，pybind11/C header 导出 |
| 错误处理 | `Result<T, E>` + `?` 运算符 |
| 标准库 | 19 个模块：io、fs、strings、collections、net、json、maps、time、datetime、env、crypto、csv、template 等 |
| 包管理 | `mimi.toml`，依赖解析，本地 registry，git 依赖 |
| MimiSpec 集成 | `.mms` 解析、promote、`mms {}` 块、规则一致性检查 |
| 编译目标 | LLVM 18 原生编译，交叉编译（Windows），no_std，共享库 `.so` |
| LSP | 语言服务器协议支持，悬停、补全、跳转定义、合约镜头 |

---

## CLI 命令

| 命令 | 说明 |
|------|------|
| `mimi check <file>` | 类型检查 |
| `mimi run <file>` | 类型检查并运行 |
| `mimi test <file>` | 运行 `test_*` 测试函数 |
| `mimi build <file>` | 编译为本地可执行文件 |
| `mimi build <file> --verify-contracts` | 编译并验证合约 |
| `mimi fmt <files...>` | 格式化 |
| `mimi lint <files...>` | 静态分析 |
| `mimi verify <file>` | Z3 合约形式化验证 |
| `mimi lsp` | 启动 LSP 服务器 |
| `mimi init <name>` | 初始化项目 |
| `mimi add <name>` | 添加依赖 |
| `mimi remove <name>` | 移除依赖 |
| `mimi install` | 安装依赖 |
| `mimi update` | 更新依赖 |
| `mimi list` | 列出依赖 |
| `mimi tree` | 显示依赖树 |
| `mimi publish` | 发布到本地 registry |
| `mimi search <query>` | 搜索包 |
| `mimi doc <file>` | 生成文档 |
| `mimi promote <file>` | `.mms` → `.mimi` 提升 |
| `mimi mms <files>` | 处理 MimiSpec 文件 |
| `mimi stats <file>` | 使用统计 |
| `mimi emit-c-headers <file>` | 导出 C 头文件 |
| `mimi emit-py-bindings <file>` | 导出 Python pybind11 绑定 |

---

## 项目结构

```
mimi/
├── src/
│   ├── main.rs              # CLI 入口
│   ├── lib.rs               # 库入口
│   ├── ast.rs               # AST 类型定义
│   ├── parser/              # 解析器
│   ├── lexer/               # 词法分析
│   ├── core/                # 类型检查
│   ├── interp/              # 解释器
│   ├── codegen/             # LLVM codegen
│   ├── verifier/            # Z3 形式化验证
│   ├── ffi/                 # FFI 契约系统
│   ├── diagnostic/          # 诊断系统
│   ├── lsp/                 # LSP 服务器
│   ├── fmt.rs               # 格式化器
│   ├── lint.rs              # 静态分析
│   ├── contracts.rs         # 合约提取
│   ├── manifest.rs          # 包清单
│   ├── loader.rs            # 模块加载
│   ├── lockfile.rs          # 锁定文件
│   ├── pkg_registry.rs      # 包注册表
│   ├── pkg_resolve.rs       # 依赖解析
│   ├── runtime/             # C 运行时
│   └── tests/               # 测试套件 (2,011 个)
├── std/                     # 标准库 (19 模块)
├── examples/                # 示例程序 (28 个)
├── demos/                   # 演示程序 (23 个)
├── devdocs/                 # 内部开发文档
├── gramma/                  # 语法形式化定义
├── benches/                 # 基准测试
├── readme/                  # 详细文档
├── Cargo.toml
├── Makefile
└── LICENSE                  # Apache-2.0
```

---

## 语法示例

### 函数与合约

```mimi
pub func divide(a: i32, b: i32) -> i32 {
    requires: b != 0
    ensures: result == a / b
    a / b
}
```

### ADT 与模式匹配

```mimi
type Tree<T> {
    Leaf(T)
    Node(Tree<T>, Tree<T>)
}

func depth<T>(t: Tree<T>) -> i32 {
    match t {
        Leaf(_) => 1,
        Node(l, r) => 1 + max(depth(l), depth(r))
    }
}
```

### 并发与补偿

```mimi
func process() -> Result<i32, string> {
    let data = fetch_data()?;
    on failure { cleanup(data) }

    let result = compute(data)?;
    on failure { revert(result) }

    Ok(result)
}
```

### FFI 调用

```mimi
extern "C" {
    func strlen(s: string) -> i64;
    func puts(s: string) -> i32;
}

func main() {
    let len = strlen("Hello");
    puts("Hello from Mimi FFI!");
}
```

---

## 版本

当前版本: **v0.7.0** | 语言规范: v1.0.0-rc.1

| 版本 | 亮点 |
|------|------|
| v0.7 | Z3 验证、FFI 零拷贝、HTTP codegen、标准库扩展 (csv/crypto/template) |
| v0.6 | Windows 目标、Actor 模型、正则内置函数、字符串合约运行时断言 |
| v0.5 | Parasteps pthread codegen、合约验证、CI/CD 完整化 |
| v0.4 | 错误系统 `Diagnostic` 化、Arena 逃逸检测、写-写竞争检测 |
| v0.3 | 包管理、文档生成管道、双后端基线 |
| v0.2 | 基础语言特性、LLVM codegen、合约系统 |
| v0.1 | 初始原型、解释器、类型检查器 |

完整更新日志见 [CHANGELOG.md](CHANGELOG.md)。

---

## 开发

```bash
# 运行测试
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo test

# L1 双后端等价性测试
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo test dual_

# L2 类型系统健全性测试
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo test typecheck::

# 格式化
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo fmt

# Lint
LLVM_SYS_180_PREFIX=/tmp/llvm-wrapper cargo clippy --deny warnings
```

---

## 许可证

Apache-2.0
