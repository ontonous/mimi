# Mimispec 依赖预审计

> 审计日期: 2026-06-24
> 用途: 规划 v0.24 自举的导入点替换优先级

## 总览

当前的 `mimispec` crate 在 6 个源文件中共有 20 处导入点。

## 导入点清单

### 1. `src/ast.rs:297` — AST 类型依赖（深层）

```rust
ast: Option<mimispec::ast::File>,
```

MmsBlock 的 AST 字段引用 mimispec 的 AST 类型。这是最深层的依赖——Mimi 的 AST 包含 mimispec 的 AST。

**替换方案**: 将 `MmsBlock.ast` 改为 `Option<MimiMmsAst>`，其中 `MimiMmsAst` 是 Mimi 自身定义的 mimispec AST 镜像类型。

**优先级**: P1（最优先，其他替换依赖于此）

---

### 2. `src/parser/parse_stmt.rs:231-245` — MmsBlock 解析（独立）

```rust
fn try_parse_mimispec_with_timeout(content: &str) -> Option<mimispec::ast::File> {
    // 100ms 超时保护
    let result = mimispec::parse(&content_owned);
}
```

MmsBlock 内容通过 mimispec 解析器解析。有 100ms 超时保护。

**替换方案**: 调用 Mimi 实现的 mimispec 解析器（FFI 或直接调用）。

**优先级**: P2（可独立替换，不依赖其他模块）

---

### 3. `src/doc_core.rs:169-305` — 文档生成（最巨量）

约 10 处导入点，覆盖 parse、AST 遍历、render。

```rust
use mimispec::parse;
use mimispec::ast::*;
use mimispec::render::render_file;
fn append_fragment_markdown(frag: &mimispec::ast::Fragment, out: &mut String);
fn expr_to_string(expr: &mimispec::ast::Expr) -> String;
fn compare_op_to_str(op: mimispec::ast::CompareOp) -> &'static str;
fn atom_to_string(atom: &mimispec::ast::Atom) -> String;
```

这是**替换工作量最大**的文件。完整的 mimispec AST 遍历 + render 逻辑约 120 行。

**替换方案**: 
1. 先定义 Mimi 端的 mimispec AST 镜像类型
2. 重写 `append_fragment_markdown`, `expr_to_string`, `compare_op_to_str`, `atom_to_string`
3. 替换 `render_file` 调用

**优先级**: P3（可最后替换，不影响其他模块）

---

### 4. `src/main/mms.rs:4-65` — LaTeX 渲染（CLI 边界）

```rust
use mimispec::latex::render_file_latex;
let result = mimispec::parse(&source);
Some(mimispec::render::render_file(&result.file))
```

仅 CLI 层的 mms 命令使用。

**替换方案**: 
1. parse 调用 → Mimi 解析器
2. render_file_latex / render_file → Mimi 端实现（或保留只在 CLI 层）

**优先级**: P4（仅 CLI，最简单的边界）

---

### 5. `src/interp/builtins/mimispec_runtime.rs:9-33` — 运行时内置函数

```rust
match mimispec::tokenize(source) { ... }
let result = mimispec::parse(source);
```

提供 Mimi 代码中可直接调用的 mimispec 处理函数。

**替换方案**: 
1. 重写 tokenize/parse 的 FFI 调用到 Mimi 解析器

**优先级**: P2（可独立替换）

---

### 6. `src/interp/builtins/mod.rs:13` — 模块声明

```rust
pub(crate) mod mimispec_runtime;
```

**替换方案**: 随 `mimispec_runtime.rs` 的替换自动处理。

**优先级**: P5（被动替换）

---

## 替换优先级排序

| 优先级 | 文件 | 工作量 | 依赖关系 |
|--------|------|--------|---------|
| P1 | `src/ast.rs` | 小 | 阻断 P3 |
| P2 | `src/parser/parse_stmt.rs` | 小 | 独立 |
| P2 | `src/interp/builtins/mimispec_runtime.rs` | 中 | 独立 |
| P3 | `src/doc_core.rs` | 大 | 依赖 P1 |
| P4 | `src/main/mms.rs` | 中 | 依赖 P1+P3 |
| P5 | `src/interp/builtins/mod.rs` | 极小 | 被动 |

## Phase 1 双轨解析策略

1. 定义 `MimiMmsAst` — Mimi 端的 mimispec AST 镜像（覆盖 mimispec::ast::File 及其子类型）
2. 实现 `mimi_parse_mimispec(source: &str) -> MimiMmsAst` — 在解析器中添加 mimispec 解析分支
3. 在 `parse_stmt.rs` 中并行运行 Rust 版 + Mimi 版解析器，diff 结果
4. 逐步切换各调用点

## Phase 2 替换顺序

```
1. src/ast.rs (P1) — 定义镜像类型
2. src/parser/parse_stmt.rs (P2) — 替换解析
3. src/interp/builtins/mimispec_runtime.rs (P2) — 替换内置函数
4. src/doc_core.rs (P3) — 替换文档生成
5. src/main/mms.rs (P4) — 替换 CLI
6. 删除 Cargo.toml 中的 mimispec 依赖
```
