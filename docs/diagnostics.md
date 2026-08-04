# Mimi 诊断输出契约（Diagnostics Contract）

> 状态：**实现约定（0.34.34 起），待 1.0 冻结为 `[stable]`**。
> 配套文档：错误码登记见 `docs/error-codes.md`（码一经分配永不复用）。
> 设计裁决（2026-08-05，设计者）：诊断消费方以机器/AI 优先；rustc 风格图形化
> （gutter ` | `、caret `^^^`、` --> ` 箭头、空行）为冗余装饰——caret 携带的列区间
> 信息由坐标区间无损替代。保持最高信息量，不设字面字符禁令。

## 1. 范围

本契约覆盖 **stderr 文本诊断**这一机器可读表面：

| 发布面 | 契约内 | 说明 |
|---|---|---|
| checker / parser 诊断（`mimi check/build/run` 编译期） | ✅ | `format_diagnostic`（src/diagnostic/format.rs） |
| 合约违规内嵌消息（`--verify-contracts` 编译产物） | ✅ | codegen/scope.rs `build_contract_violation_message` |
| 算术 trap / runtime abort（编译产物运行期） | ✅ | runtime `mimi_trap_*` / `mimi_runtime_abort` |
| bytecode VM 运行时错误（`mimi run`） | ✅ | interp 错误渲染 |
| LSP 诊断 | ❌ | 结构化协议（JSON-RPC），自有规范，不受文本契约约束 |
| `mimi verify`（Z3）输出 | ❌ | 独立工具表面 |

**契约形状**：单行致密格式。`SEVERITY[CODE] LOCATION MESSAGE`，可选字段以
` | ` 串联（`src:` / `note:` / `help:` / `hint:`）。一行一条诊断，无装饰行。

## 2. 文法（normative）

```text
diagnostic      = severity [ "[" code "]" ] [ SP location ] SP message
                  *( SP "|" SP field )
severity        = "error" | "warning" | "note" | "help"
code            = "E" digit+ | "W" digit+          ; 见 error-codes.md
location        = path ":" line ":" col-range
col-range       = column "-" column                ; 单行区间（end_col 排他）
                | column "-" line ":" column       ; 跨行：起列 - 止行:止列
                | column                           ; 无终点信息
field           = src-field | note-field | help-field | hint-field
src-field       = "src:" SP source-line            ; ≤200 字符，超长以 " ..." 截断
note-field      = "note:" SP message [ SP "@" SP location ]
help-field      = "help:" SP message
hint-field      = "hint:" SP message               ; 处置指引（如何关闭/规避）
```

规则：

1. `code` 缺失时不渲染括号（禁止 `error[]` 空括号形态）。
2. `col-range` 的区间端点语义与 AST `Span` 一致（`end_col` 排他）。区间信息
   是 caret 下划线的无损替代。
3. 字段顺序固定：`src` → `note`（可多条）→ `help`/`hint`。机器解析按
   `" | "` 切分后按前缀归类；前缀集合为本节封闭枚举。
4. 颜色（ANSI）仅在 TTY 且未设 `NO_COLOR` 时出现，**不属于契约内容**——
   消费方应按 `strip_ansi` 等价方式剥离。管道/CI/AI 场景保证无色。

## 3. 各类消息的必选字段

| 类别 | 形态示例 | 必选 | 现状 |
|---|---|---|---|
| checker 诊断 | `error[E0208] f.mimi:3:5-14 cannot assign to immutable variable 'x' \| src: x = x + 1 \| help: use 'let mut'` | severity、code、location、message | ✅ |
| parser 诊断 | `error f.mimi:2:13 expected expression after '=' in let binding \| src: let x = ;` | severity、location、message | ⚠ code 缺失（G3） |
| 合约违规（编译产物） | `[mimi] [E0808] requires condition failed for 'div': b != 0 @ f.mimi:2:15-21 \| hint: rebuild without --verify-contracts to disable contract checking.` | `[mimi]` 前缀、code E0808、phase 措辞、owner、渲染合约文本、`@`+location、hint | ✅ |
| 合约违规（VM） | `bytecode runtime error: [E0808] requires condition failed for 'div': false [main] (line 11)` | code、phase 措辞、owner、运行时值、调用函数、行号 | ✅ |
| 算术 trap | `[E0802] integer overflow in addition` + 可用逃生舱时附 `Hint: use wrapping_*...` | code、message | ⚠ 无位置（G1） |
| runtime abort（越界/OOM/字符串范围等） | `[mimi] list index out of bounds: index read (idx >= len)` | `[mimi]` 前缀、message | ⚠ 无位置（G1） |
| VM 运行时错误 | `bytecode runtime error: [E0800] index 9 out of bounds (len 3) [main] (line 3)` | code、message、函数、行号 | ✅ |

合约消息措辞双后端对齐：`[E0808] {requires|ensures} condition failed for
'<owner>'`。两端信息互补（codegen：合约源文本+声明位置；VM：运行时求值
+调用位置），属**允许的不对称**（G2），双方都满足各自必选字段。

## 4. 不变量（绝对禁令——禁的是说谎与泄漏，不是排版）

1. **禁止内部结构泄漏**：任何诊断不得含编译器内部类型/值的 Debug 形态
   （`AstNodeMeta`、`Located {`、`SourceId(`、AST variant 名等）。表达式必须
   以人类可读形态渲染（expr_render），禁止 `{:?}` dump。
2. **禁止事实性误导**：类别标签与处置指引必须真实。诊断不得携带与实际来源
   不符的类别前缀（如纯 Mimi 合约标为 FFI），不得指引错误的关闭开关。
3. **错误码不复用**：见 `docs/error-codes.md` 红线。
4. **坐标必须精确**：location 来自真实 Span，不得丢失、近似或指向错误节点。
   （dx-backlog CO-H2"类型错误指向函数声明而非表达式"属本条违例，登记 0.1.5。）

**明确不设禁**：` --> `、`^^^`、`|` gutter、换行等**格式字符本身**。它们是
格式政策而非不变量——用户源码（字符串字面量、文件路径）可合法包含任意字符，
字面禁令对正常内容脆弱；且未来 opt-in 人类富渲染模式（如 `--format=rich`）
可合法重新使用 caret/多行排版而不违反致密默认契约。契约的致密性由正向形状
断言保障（码+消息+坐标+字段共居单行），不由字符黑名单保障。

## 5. 已知缺口与登记（0.34.34 审计）

| # | 缺口 | 性质 | 处置 |
|---|---|---|---|
| G1 | codegen trap/OOB/OOM 消息无位置（VM 有函数+行） | 表达力缺失 | 登记 1.x：编译期嵌入位置（与合约消息同技术） |
| G2 | VM/codegen 合约消息互补不对称 | 允许分歧 | 本契约 §3 明文 |
| G3 | parser 错误无稳定码（裸 `error`） | 码系缺口 | 登记 0.1.5：分配 parse 错误码段 |
| G4 | lint 独立格式（`path: [severity] message`，无码无坐标） | 未收敛 | 登记 0.1.5：接入 format_diagnostic |
| G5 | resolved emitter 列表越界不触发 E0800（静默 OOB 读/写） | P0"错误缺失" | 修复登记（独立于本契约，属 soundness） |
| G6 | note 位置继承主诊断文件标签（跨文件 note 未启用） | 有效简化 | 跨文件 note 启用时须按 note span 的 source_id 解析标签 |

## 6. 版本

- 0.34.34（commit e628b9db / cbfaff3c）：致密格式落地 + 契约断言正向化。
- 1.0：冻结为 `[stable]`，写入 language-spec 错误模型章节交叉引用。
- 冻结后：修改机器语义（字段集、文法、必选字段）须提升契约版本；
  新增可选字段（如 G1 的位置字段）为兼容变更。
