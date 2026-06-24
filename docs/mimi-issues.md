# Mimi 语言当前问题清单

> 审计日期: 2026-06-24 (updated to v0.22.4-dev)
> 审计背景: v0.22 尝试用 Mimi 实现 mimispec 解析器（7,290 行 Rust → Mimi）过程中发现

## 一、类型系统缺陷

### 1.1 ~~缺少 `Option<T>`~~ ✅ v0.22 已修复

**症状**: 无法表达"可能有值也可能没有"的语义。
**影响**: ⚫ 严重。每个可选字段都需要 `has_xxx: bool` + 默认值 workaround，污染所有 API。
**位置**: `src/core/ast.rs` 有 `Type::Option(Box<Type>)` 类型。
**修复**: v0.22 补齐 `Option<T>` 的类型推断、codegen、interpreter 全路径，L1 双后端测试通过。

### 1.2 `Result<T, E>` 不完整

**症状**: 
- `Result` 在 type checker 中已存在
- `?` 运算符存在（compile_try_expr 双后端实现）
**影响**: 🟢 低。基本场景可正常工作。

### 1.3 ~~不支持递归类型~~ ✅ v0.22 已修复

**症状**: 无法定义 `type Expr { Call(name: string, args: List<Expr>) }`。
**影响**: ⚫ 严重。任何树形结构（AST、JSON）都无法用原生类型表达。
**原因**: 类型检查器在 `check_type` 时对自引用类型不做特殊处理。
**修复**: v0.22 在类型检查器中支持 Record/Union/Enum 自引用类型定义，L1 测试通过。

### 1.4 泛型 `List<T>` 嵌套（codegen 有限制）

**症状**: `List<List<i32>>` 类型标注通过，codegen 嵌套索引已修复（v0.22）。
**影响**: 🟢 低。解释器可用，codegen 列表元素 type-erased 为 i64。
**注意**: v0.22 完成类型推断 + codegen 链式索引修复 + 解释器支持。

### 1.5 ~~不支持函数类型作为参数~~ ✅ v0.22 已修复

**症状**: `func(List<T>, i32) -> (Any, i32)` 在高阶函数中解析失败。
**修复**: v0.22 添加闭包结构体包装 + List&lt;T&gt; 形参 ABI 转换，双后端 L1 测试通过。

---

## 二、字符串/字符处理不足

### 2.1 ~~没有字符操作~~ ✅ v0.22 已补齐

**症状**: 所有字符操作通过 `char_at(s, i)` 返回单字符 string。
**修复**: v0.22 添加 `char_code(s, i) -> i64`, `chr(i) -> i64`，`str_index_of` 返回 `Option<i32>`。

### 2.2 ~~缺少正则表达式匹配~~ ✅ 已存在

- `regex_match` / `regex_find` / `regex_replace` 作为内置函数已存在

### 2.3 `str_starts_with` 不是字符级操作

**症状**: 无法高效地逐个字符遍历字符串并做模式匹配。
**影响**: 🟢 低。可以用 `char_at` / `char_code` + 索引循环替代。

---

## 三、语法/Parser 限制

### 3.1 ~~缩进敏感 + 没有行延续~~ ✅ v0.22.1 已修复

**症状**: 长表达式不能用 `||` 续行，必须写在一行。
**影响**: ⚪→✅。
**修复**: v0.22.1 添加了反斜杠 `\` 行延续，支持跨行拼接。

### 3.2 ~~不支持 `use module::function` 导入单个函数~~ ⬜ 仍待实现

**症状**: 必须 `use module` 然后 `module::function()`。
**影响**: 🟢 低。只是语法糖缺失，不影响功能。
**计划**: v0.22.5

### 3.3 `else if` 解析问题

**症状**: `if ... { } else if ... { }` 在某些嵌套场景下解析失败。
**影响**: 🟢 低。大部分场景可用。

---

## 四、标准库不完整

### 4.1 ~~缺少集合类型~~ ✅ v0.22.1/v0.22.3 已修复

| 缺失类型 | 状态 | 说明 |
|----------|------|------|
| `Map<K, V>` 字面量 | ✅ v0.22.1 | `{"key": value}` 双后端支持 |
| `Set<T>` | ✅ v0.22.3 | `{1, 2, 3}` 字面量 + 操作，codegen stub |
| 迭代器 | 🟢 低 | `for i in range` |

### 4.2 JSON 序列化/反序列化

- `to_json` / `from_json` 已存在
- `from_json::<T>` 类型化反序列化 v0.22.2 已支持 (i32/f64/string/bool/List/Option/Record/Enum)
- `Record` 类型的字段访问无法在 Mimi 中动态进行（静态类型限制，非 bug）
- codegen 路径返回 graceful error

### 4.3 正则表达式

- `regex_match` / `regex_find` / `regex_replace` 作为内置函数可用

---

## 五、Codegen/运行时问题

### 5.1 LLVM codegen 对复杂类型的支持不稳定

- `List<List<T>>` 嵌套在 codegen 路径已修复（v0.22 chained indexing fix）
- 列表元素在 codegen 中 type-erased 为 i64，嵌套列表索引已通过 L1 测试
- codegen stub: Set 操作 / from_json::&lt;T&gt; / sort_f64 / sort_str / lexer/parse/ast_eval

### 5.2 解释器与 codegen 行为不完全一致

- 某些边缘情况（复杂泛型、嵌套调用）双后端行为不同
- 已知 11 个 `dual_gap_*` 已在 v0.21 关闭，但可能还有未发现的差距

---

## 六、开发者体验

### 6.1 错误消息不够友好

- 某些解析错误缺乏源码位置信息
- 缺少 "did you mean?" 建议

### 6.2 ~~缺少字符串插值~~ ✅ 已有 f-string

- `f"hello {name}"` 语法已支持（f-string 插值）

---

## 影响评估矩阵

| 问题 | 严重性 | 对自举的阻塞程度 | 修复难度 |
|------|--------|-----------------|---------|
| ~~缺少 `Option<T>`~~ (✅ v0.22) | ⚫→✅ | **已解决** | 中 |
| ~~不支持递归类型~~ (✅ v0.22) | ⚫→✅ | **已解决** | 高 |
| 泛型嵌套 (🟡 codegen 列表元素限 i64) | 🟢 | 不阻塞（解释器可用） | 中 |
| ~~函数类型作参数~~ ✅ v0.22 | ⚪→✅ | 已解决（闭包包装 + ABI 转换） | 中 |
| ~~字符串能力~~ (✅ `chr`/`char_code`/`str_index_of`) | 🟢→✅ | 已解决 | 低 |
| ~~无行延续~~ (✅ v0.22.1 `\` 续行) | 🟢→✅ | 已解决 | 低 |
| ~~Map 字面量~~ (✅ v0.22.1 `{"k": v}`) | 🟢→✅ | 已解决 | 中 |
| ~~Set 集合~~ (✅ v0.22.3 字面量 + 操作) | 🟢→✅ | 已解决 | 中 |
| ~~JSON 类型化~~ (✅ v0.22.2 `from_json::<T>`) | 🟢→✅ | 已解决 | 中 |
| codegen stub ×4 | 🟢 | 不阻塞（解释器可用） | 低 |

## 结论

自举（用 Mimi 写 mimispec 解析器）v0.22 后**可行性显著提升**：

1. ~~**缺少 `Option<T>`**~~ — ✅ v0.22 已补齐全路径
2. ~~**不支持递归类型**~~ — ✅ v0.22 类型检查器支持自引用
3. **泛型嵌套** — 🟡 类型推断通过，codegen 列表元素限 i64，interp 中 List<List<Atom>> 可用
4. ~~**字符串能力**~~ — ✅ `chr`/`char_code`/`str_index_of` 补齐
5. ~~**Map/Set/JSON**~~ — ✅ v0.22.1~v0.22.3 逐一补齐
6. ~~**行延续**~~ — ✅ v0.22.1 `\` 续行
7. ✅ **高阶函数参数** — 双后端 L1 测试通过

剩余限制：
- Codegen 列表元素仅支持标量类型（`List<List<T>>` 嵌套索引在 codegen 中不可用）
- codegen stub ×4（Set/from_json/sort/lexer）仅在解释器中可用
- `file_size` 仍读取全文件（vs stat 优化）

v0.22 推进后，mimispec 解析器的核心阻塞点（Option<T>、递归类型、集合类型、字符串操作）均已清除。
