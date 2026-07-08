# ADR 002: Mimi 合约系统设计与 Z3 验证架构

## 状态

已实施（v0.9+），持续演进中。

## 上下文

Mimi 的核心差异化在于编译期合约验证。需要一种与语言类型系统深度集成的合约机制，而非独立的断言库。涉及的关键设计决策：

- 可验证的合约语法（requires/ensures/invariant）vs 传统 inline `assert`
- Z3 求解器集成策略：表达式编码、跨函数传播、反例生成
- 非平凡语言特性（字符串、并发、闭包）在 SMT 编码中的处理界限

## 决策

### 1. 合约语法：requires / ensures / invariant

选择声明式前置/后置/不变量语法而非内联断言，理由：

| 方面 | `requires x > 0` | `assert(x > 0)` |
|------|------------------|------------------|
| Z3 静态验证 | 可直接编码为约束 | 需从 AST 推断 |
| 调用者侧推理 | 可作为公开契约传播 | 仅在被调用者体内可见 |
| 反例生成 | 可区分先决条件 vs 后置条件 | 无法区分 |
| 文档价值 | 自文档化 | 实现细节 |

**`result` 标识符**：后置条件中 `result` 自动绑定到函数返回值，无需额外变量。

**`old(expr)` 标识符**：后置条件中 `old(x)` 引用函数入口处 `x` 的初始值，与 Z3 编码中的 `old_x` 变量对应。

### 2. Z3 集成架构：表达式编码

Mimi 编译器通过 `z3` Rust crate 调用 Z3 C API。编码规则如下：

#### 2.1 类型映射

| Mimi 类型 | Z3 排序 | 示例 |
|-----------|---------|------|
| `i32` / `i64` | `Int` | `x + 1` → `x + 1` |
| `f64` | `Real` | `x / 2.0` → `/ x 2.0` |
| `bool` | `Bool` | `x && y` → `and(x, y)` |
| `string` | 三变量编码 + 可选 `String` | 见 §5 |
| 枚举/类型标签 | `Int` | `Option::Some(0)` → 具体整数 |

#### 2.2 字段与记录编码（struct encoding）

对 `record/p.field` 的字段访问使用**即时生成 Z3 变量**而非理论排序：

```
s:p → 字段访问时按需创建 Z3 变量 `s_p`（get_or_create_int/get_or_create_real）
```

这意味着每条验证路径中字段都被视为自由变量——等价性是因过程内有限约束建立的，而非通过结构性。

```
// 例：
requires: p.x > 0
// Z3: 断言 p_x > 0，其中 p_x 为 Z3Int 变量
```

函数调用编码为不透明变量，通过 `call_var_key` 生成确定性键名：

```
fn call_var_key(name, args) → "call_{name}_{arg1}_{arg2}_{...}"
double(x) → "call_double_x"
```

#### 2.3 if / match 编码

- `if cond { A } else { B }` → Z3 `ite(cond, A_z3, B_z3)`
- `match expr { P1 => A, P2 => B, _ => C }` → 嵌套 `ite` 链，模式守卫（guard）以 `and` 连接
- 仅支持整数/布尔字面量模式匹配；构造子模式（constructor patterns）不编码

### 3. 三种验证状态

| 状态 | Z3 结果 | 含义 | 伪代码路径 |
|------|---------|------|-----------|
| `Verified` | `Sat + Unsat(ensures)` | 前置条件可满足，所有后置条件成立 | `check(pre)` 返回 Sat → `push + check(pre && !ens)` 返回 Unsat |
| `Failed` | `Sat + Sat(ensures)` | 前置条件可满足，但存在违反了后置条件的输入 | Z3 返回模型，提取反例 |
| `Unknown` | `Unknown` | 求解器超时/崩溃/无法编码 | 静默跳过，不报错也不可验证 |

**Unknown 的处理**：
- Z3 不可用时（libz3 未安装）：所有函数返回 `Unknown`，使用 `mock_verify_file` 模拟
- 超时（默认 5000ms）：`check_safe()` 捕获 `catch_unwind`，重建求解器，返回 `Unknown`
- 编码失败（如不可编码的 Lambda）：`expr_to_z3_*` 返回 `None`，调用方跳过断言

**反例生成**：`Failed` 状态时从 Z3 模型提取变量赋值（int/real），关联到违反的后置条件索引，构建 `Diagnostic` 错误消息。

### 4. 跨模块 ensures 传播

调用者须能依赖被调用者的后置条件，否则跨函数验证将无效。

#### 机制（`assert_callee_ensures_in_expr` + `assert_callee_ensures_in_block`）

```
1. collect_func_defs(items)  // 预扫描所有文件项，建立名→函数定义映射
2. 对函数体内每个 Expr::Call(Ident(name), args):
   a. 在 func_defs 中查找 name
   b. 提取 callee 的所有 ensures 表达式
   c. 用 call 的实参替换 ensures 中的形参（substitute_call）
   d. 对替换后的 ensures 调用 expr_to_z3_bool
   e. 将结果 Z3 布尔值断言到 solver 中
```

同时覆盖**非尾部调用**：`let y = double(x); y` 中 `double(x)` 的 ensures 通过 `assert_callee_ensures_in_block` 扫描 let/assign/if 中的调用而传播。

#### 下界值（lower bound）

对于有返回类型但无显式返回表达式的函数（如死循环），`result` 绑定到 0 以防止后置条件空洞地通过。

### 5. 字符串理论映射（P1.2）

字符串使用 Z3 Seq 理论编码，同时维护整数辅助变量以兼容旧路径：

#### 变量注册（每个 `string` 类型参数 `s`）

| Z3 变量 | 类型 | 含义 |
|---------|------|------|
| `s` | `Z3String` | 原生 Z3 字符串理论变量 |
| `s_len` | `Z3Int` | `s` 的长度 = `s.length()` |
| `s_ne` | `Z3Bool` | `s != ""` = `s.ne(empty)` |
| `s`（Int 映射） | `Z3Int` | 遗留的整型编码（保持兼容） |

#### 一致性约束

```python
solver.assert(s.length() == s_len)
solver.assert(s_ne == (s != ""))
```

#### 支持的操作

| Mimi 操作 | Z3 编码 |
|-----------|---------|
| `s == t`（字符串相等） | `encode_string_eq` → `s.eq(t)` |
| `s != ""`（判空） | `s.ne(empty)` |
| `len(s)` | `s.length()`（或直接返回 `s_len` 变量）|
| `contains(s, pat)` | `s.contains(pat)` |
| `starts_with(s, pat)` | `s.prefix(pat)` |
| `ends_with(s, pat)` | `s.suffix(pat)` |
| `char_at(s, i)` | `s.at(i)` |

#### 编码位置

- `resolve_string_expr`: `expr.rs:646-670`
- `encode_string_eq`: `expr.rs:674-678`
- 一致性约束（`s.length() == s_len`）：`func.rs:333-363`

### 6. Spawn / Await 编码（P1.1）

Spawn/Await 在验证中被视作**透明包装器**：验证器穿透它们直接编码内部表达式。

```
Expr::Spawn(inner) → self.expr_to_z3_int/inner, vars  // 直接递归
Expr::Await(inner) → self.expr_to_z3_bool/inner, vars  // 直接递归
```

这意味着：
- `spawn f(x)` 在 Z3 中等价于 `f(x)`（并发语义不可见）
- `await f(x)` 在 Z3 中等价于 `f(x)`（同步语义不可见）
- 验证器不建模线程创建、数据竞争、死锁

此选择是刻意的：SMT 求解器不适合托管并发正确性；`parasteps` 的写-写竞争检测由独立的静态 lint（W005）承担。

### 7. 局限性

#### 不可编码的特性

| 特性 | 原因 | 编码行为 |
|------|------|---------|
| `Expr::Lambda`（闭包） | 函数值无法编码为 SMT 项 | 返回 `None`，合约被视为 Unknown |
| `Expr::Comprehension` | 列表推导依赖未建模的集合语义 | 返回 `None` |
| `Expr::Record`（记录构造） | 无对应的结构性 Z3 排序 | 跳过 |
| `Expr::Index`（索引） | 动态数组访问超出 SMT 范围 | 跳过 |
| 字符串返回类型的函数 | `result` 被编码为 Int/Real，字符串返回函数不可验证 | `result` 绑定到 0 |

#### 已解决的局限性

| 过去问题 | 修复 |
|---------|------|
| 非尾部调用中的 ensures 未传播 | `assert_callee_ensures_in_block` 覆盖所有语句位置 |
| Z3 parse 错误被静默忽略 | `parse_errors` 向量收集并在诊断中报告（E0500）|
| 合约 `old(x)` 无对应 Z3 变量 | `old_{name}` 变量在 `verify_func` 中注册 |

### 8. 源文件结构参考

| 文件 | 职责 |
|------|------|
| `mod.rs` | 入口：`verify_source` / `verify_file` / `is_z3_available` |
| `ctx.rs` | `Verifier` 结构体（solver + func_defs + let_subst）、`VerificationResult`、`VerifStatus`、`Z3VarMap` |
| `func.rs` | `verify_func` / `verify_extern_func`：断言布局、反例提取、跨模块 ensures 传播 |
| `expr.rs` | `expr_to_z3_int` / `expr_to_z3_real` / `expr_to_z3_bool`、字符串理论映射、match 编码 |
| `helpers.rs` | `block_tail_expr` / `extract_body_return` / `extract_string_empty_cmp` / `parse_contract_expr` |
| `ffi.rs` | `verify_ffi_call_sites`：extern 函数调用点的前置条件验证 |
| `tests.rs` | 验证器测试 |

### 9. 关键设计决策

| 决策 | 选择 | 替代方案 | 理由 |
|------|------|---------|------|
| 求解器 | Z3（via `z3` Rust crate） | CVC5 / Bitwuzla / 自制求解器 | 成熟度、`Seq` 理论（字符串）、API 稳定性 |
| 记录编码 | 按需字段变量（非结构性） | Z3 数据排序 | 避免构造/解构复杂性，过程内足够 |
| 超时处理 | `catch_unwind` + 求解器重建 | `interrupt()` + 复用 | Z3 C API 崩溃后不安全，重建可确保状态干净 |
| 跨函数推理 | 在当前边界内联合采样 | 各函数独立验证，连接 requires/ensures | 允许验证器推断中间值的约束关系 |
| Unknown 策略 | 保守跳过 | 假定所有不变量成立 | 避免误报 |

## 后果

### 正面

- 可对具有 requires/ensures/invariant 的函数进行编译期静态验证
- 交叉函数验证：调用者可信任被调用者的 ensures 约束
- 字符串相等、包含、前缀/后缀通过 Z3 Seq 理论可验证
- 反例诊断包含具体的输入值和违反的后置条件
- 求解器崩溃/超时时优雅降级至 Unknown

### 负面

- Lambda 和列表推导不参与合约验证，隐式绕过检查
- 字符串丰富的后置条件仍受 Z3 Seq 理论性能限制
- 对 `shared` 参数的合约验证使用抽象堆模型，不足以捕获堆数据结构的性质
- 并发语义（spawn/await）在验证中被完全抽象，补充 lint（W005）只能检测写-写竞争，不能证明无竞争
- 求解器 5000ms 超时对大型函数可能不足，LSP 中可通过 `set_timeout` 动态调整

### 未来工作

- Lambda 闭包合约：符号式闭包摘要（v0.15 规划）
- `#[ignore]` 清零：泛型边界 + 网络测试（v0.15）
- 字符串返回类型的函数验证：需要扩展 `result` 变量支持 Z3String
