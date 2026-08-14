# Mimi 语法参考（EBNF）

> **Authority**: 本文档是 `devdocs/v0.34/golden/syntax-reference.golden.md`（parser 实况 EBNF）
> 的渲染副本。语法以 golden 为准；本文档 0.34.5 起由 golden 重新生成。
> **Semantic authority**: `docs/language-spec.md`（extracted from `devdocs/pre-0.1/`）。
> When this file and `language-spec.md` conflict on semantics, `language-spec.md` prevails.
>
> **Status tags**: Each production is tagged `[stable]`, `[experimental]`, `[removed]`, or `[not-yet-implemented]`.
> See `docs/language-support.toml` for 9-dimension capability matrix.
>
> Version: v0.1.5-dev (2026-08-13, synced from golden — 0.35.39 僵尸关键字裁撤)
> Implementation: v0.1.5-dev (internal sprint 0.35.X)
> Data sources: `src/lexer/`, `src/parser/`, `src/ast.rs`, `devdocs/v0.34/golden/syntax-reference.golden.md`
> 渲染例外（与 golden 的有意差异）：golden 的标题/引言元块与 §7 差异台账不进入本副本；
> §7 台账见 golden 原文（0.34.33 起差异归零）。

---
## 1. 词法

### 1.1 注释

| 形式 | 状态 | 坐标 |
|------|------|------|
| `// ...` 行注释 | ✅ | flow.rs 词法器 |
| `#` 裸行注释（后随非 `[`） | ✅ | flow.rs:883-891（LX-H2） |
| `#[ ... ]` 属性 | ✅ | 见 §7 属性（`#` + `[` 被词法器显式排除出注释） |

### 1.2 字面量

| 字面量 | 语法 | 坐标 |
|--------|------|------|
| 整数 | `123` / `0x1F` / `0b1010` / `0o17`，允许 `_` 分隔 | parse_expr.rs:234-254（模式同款 parse_pattern，pattern.rs:120-140） |
| 浮点 | `3.14`（允许 `_`） | parse_expr.rs:255-264 |
| 字符串 | `"..."`，转义 `\n \t \r \0 \\ \" \{ \}` | parse_stmt.rs:748-767 |
| f-string | `f"text {expr} text"`，空插值 `f"{}"` 非法 | parse_stmt.rs:655-777（LX-H8） |
| 布尔 | `true` / `false` | parse_expr.rs:278-287 |
| 单元 | `()`（表达式与类型两处均归一化为 `unit`） | parse_expr.rs:288-292；parse_type.rs:139-140 |

### 1.3 关键字（67 个 `=> TokenKind` 映射：64 硬关键字 + and/or/not 软关键字，keywords.rs:81-147；0.35.39 实测）

```
module type func fn actor newtype let const mut ref
shared weak arena cap trait impl dyn where extern
if else for fault fails in while return reset recover break continue
match use pub drop defer await unsafe spawn parasteps
quote comptime failure requires ensures invariant math old
flow state transition protocol pinned persistent view mutate
session dual end and or not loop as
true false unit
```

v0.34.2 变更（golden-document.md §1.1/§1.3/§1.4）：
- **移出关键字表**：`subflow`（条款 2）、`steps`（MimiSpec-only）、`consume`（随 delegate 死）——现在 tokenize 为 Ident。
- **软关键字化**：`and`/`or`/`not` 仍 tokenize 为运算符 kind，但**不是硬关键字**（绑定位置可作标识符）。
- `delegate` **已软化为标识符**（tokenize 为 Ident，keywords.rs:242 测试断言；`let delegate = 5` / `func delegate()` 合法）；仅语句起始位置保留条款 2 拒绝诊断（parse_stmt.rs:131，与 `on` 同模式，parse_stmt.rs:182）。
- [建议] 再审查：`reset`/`recover`（仅系统注入 transition 名）、`nothing`。
- **v0.34.11 已删除**：`become`/`stay`（ADR-001，golden-document.md §1.2）——tokenize 为 Ident。
- **v0.34.27 已删除**：`do`（语言评估：`do { X }` ≡ `{ X }`，零表达力；golden-document.md §1.3 修正）——tokenize 为 Ident。当前 **67 个** `=> TokenKind` 映射（实测 keywords.rs:81-147，含 and/or/not 软关键字映射；其中 64 个硬关键字，`is_keyword_kind` 判定）。
- **v0.35.39 已删除**（僵尸关键字裁撤，13 个）：`c_shared`/`c_borrow`/`c_borrow_mut`/`local_shared`/`weak_local`/`raw_string`/`nothing`(token)/`alloc`/`async`(top-level)/`with`/`desc`/`rule`/`mms`——关键字表 80 → 67，共享收敛为 `shared`/`weak` 二态；`Type::Nothing` 保留为语义残差类型（无关键字）。

### 1.4 软关键字（pattern 位置可作绑定名，pattern.rs:196-212）

`old view mutate persistent and or not session dual end`

---

## 2. 类型语法（parse_type.rs）

```
Type := PostfixType { '?' }
PostfixType := TypeAtom { '<' TypeList '>' } [ '->' Type ]      (* 仅 allow_func 时 *)
TypeAtom :=
      Ident                    (* 命名类型，含 Result/List/Option 等，parse_type.rs:81-85 *)
    | '_'                      (* Type::Infer，:77-80 *)
    | 'CBuffer<' Type '>'      (* :70-76 *)
    | '&' [ 'mut' ] Type           (* Ref/RefMut，:94-121；v0.34.4 ADR-004 删 '\'' lifetime *)
    | '&' [ 'mut' ] '[' Type ']'   (* Slice，:109-113 *)
    | '(' { Type ',' } ')'     (* 空 → unit，:123-144 *)
    | 'shared' Type            (* :145-149 *)
    | 'weak' Type              (* :155-159 *)
    | '*' [ 'mut' ] Type       (* RawPtr/RawPtrMut，:184-196 *)
    | 'cap' Ident              (* :197-212 *)
    | 'func' '(' { Type ',' } ')' [ '->' Type ]      (* :213-237 *)
    | 'extern' '"C"' ( 'fn' | 'func' ) '(' { Type ',' } ')' [ '->' Type ]  (* :238-275 *)
    | 'impl' Ident { '+' Ident }                     (* :276-310 *)
    | 'dyn' Ident { '+' Ident }                      (* :311-345 *)
    | '[' Type ';' size ']'    (* 定长数组，:346-379 *)
```

[事实] 类型参数只允许加在命名类型上（parse_type.rs:35-45）。
[事实] `func(T) -> U` 类型接受；裸 `fn` 类型仅存在于 `extern "C" fn(...)` 模式（parse_type.rs:242-253）——对应 golden-document.md ADR-003（保留现状）。
[事实] v0.34.4（ADR-004）：显式生命周期 `&'a T` **已删除**——lexer 拒绝 `'`（scan.rs + flow.rs），仅 elision `&T`/`&mut T`（Type::Ref Option 字段保留，恒 None）。

### 2.1 类型定义（parse_type.rs:389-610）

```
TypeDef := 'type' Ident [ generics ] '=' ...
         | 'type' Ident [ generics ] '{' record-or-enum '}'
Newtype := 'newtype' Ident [ generics ] '=' Type ';'

union  := 'type' Ident '=' 'union' '{' record-fields '}'     (* :443-463 *)
record := '{' { Ident ':' Type (','|换行) } '}'
enum   := '{' { Variant } '}'
Variant := Ident [ '(' { Type ',' } ')' | '{' record-fields '}' ]
```

### 2.2 泛型参数（top_level.rs:818-852）

```
generics := '<' { Ident [ ':' Ident { '+' Ident } ] ',' } '>'
```

---

## 3. 模式语法（pattern.rs:68-220）

```
Pattern := Ident                              (* Variable *)
         | '_'                                (* Wildcard，:114-115 *)
         | Ident '(' { Pattern ',' } ')'      (* Constructor tuple，:75-91 *)
         | Ident '{' { Ident [ ':' Pattern ] ',' } '}'  (* Constructor record，:92-113 *)
         | Int | String | true | false        (* Literal，:120-152 *)
         | '(' { Pattern ',' } ')'            (* Tuple，:153-167 *)
         | '[' { Pattern ',' } [ '..' [ Ident ] ] ']'   (* Array/Slice+rest，:168-195 *)
         | 软关键字 → Variable（old/view/mutate/persistent/and/or/not/session/dual/end）
```

---

## 4. 语句语法（parse_stmt.rs）

### 4.1 语句分发（parse_stmt.rs:17-296）

```
Stmt := 'let' [ 'mut' ] [ 'ref' ] Pattern [ ':' Type ] [ '=' Expr ] ';'      (* :496-546 *)
      | 'const' Ident [ ':' Type ] '=' Expr ';'                              (* :496-501 *)
      | 'return' [ Expr ] ';'                                                (* :548-560 *)
      | 'break' [ Expr ] ';' | 'continue' ';'                                (* :21-39 *)
      | 'if' Expr '{' Block '}' [ 'else' ('if' ... | '{' Block '}') ]       (* :562-591 *)
      | 'while' [ 'let' Pattern '=' Expr ] Expr '{' Block '}'                (* :593-614 *)
      | 'loop' '{' Block '}'                                                 (* :616-622 *)
      | 'for' Pattern 'in' Expr '{' Block '}'                                (* :624-637；ast.rs For.var: Pattern。v0.34.3 起绑定为 Pattern（`(k, v)` 解构）；0.34.24 起解释器与 native codegen 均支持单标识符与 tuple 解构（audit-syntax C2，880384bc） *)
      | 'arena' '{' Block '}' ';'                                            (* :298-305 *)
      | 'unsafe' '{' Block '}' ';'                                           (* :307-314 *)
      | ('shared'|'weak') Ident [ ':' Type ] '=' Expr ';'                     (* :456-494 *)
      | '...' ';'                                                            (* sketch-only，:80-91 *)
      | 'drop' '(' Expr ')' ';'                                              (* :92-99 *)
      | 'defer' '{' Block '}' ';'      (* :100-107；任意作用域出口 LIFO 执行，
                                         0.36.15 与 on failure 一并由 resolved
                                         发射器以登记+出口发射实现——无
                                         'defer failure' 表面，见 language-spec
                                         §4.6 0.36.15 修正 *)
      | 'parasteps' '{' Block '}' ';'                                        (* :108-115 *)
      | 'func' FuncDef ';'                                                   (* :116-120 *)
      | 'delegate' ... — **v0.34.1 已拒绝**（条款 2 诊断，parse_stmt.rs:139-160）
      | 'pinned' '(' Expr ')' [ '|' Ident '|' ] '{' Block '}'   (* :180-216；v0.34.3 timeout 字段删除 *)
      | 'if' 'let' Pattern '=' Expr '{' Block '}' [ 'else' ( 'if' ... | '{' Block '}' ) ]  (* v0.34.3 Stmt::IfLet *)
      | 'on' 'failure' '{' Block '}'   (* :217-224；失败出口补偿（Err/Fault/panic），
                                         于语句执行点登记，0.34.36 无 block 预扫 *)
      | Target ('=' | '+=' | '-=' | '*=' | '/=' | '&=' | '|=' | '^=') Expr ';'  (* 复合赋值→desugar 为 Assign+Binary，:226-294 *)
      | Expr ';'                                                             (* :290-293 *)
```

[事实] `pinned(expr, timeout = N)` 硬错误："abolished by architecture amendment clause 10. Async FFI timeout (spawn_foreign_task) is planned for 0.2..."（parse_stmt.rs:162-168）。
[事实] v0.34.3：`Stmt::Pinned.timeout` 字段删除（parser 恒拒绝 timeout），仅 pin + body 保留。
[事实] v0.34.3：`if let` 为 Stmt::IfLet（非 desugar 到 match——pattern 绑定需 then 块可见），bytecode 完整执行，codegen E0700（golden-document.md §1.3）。
[事实] `stay { payload }` 带 payload 形式**不解析** — **v0.34.11 整词删除**（ADR-001）：`stay`/`become` tokenize 为 Ident，语法位置使用直接解析失败。
[事实] 复合赋值 desugar：`x += e` → `Assign(x, x + e)`，RHS 中 x 标记 Desugared（parse_stmt.rs:263-289）。

### 4.2 块内合约子句（parse_stmt.rs:803-861）

```
'requires' ':' Expr ';'*     (* 在块内任意位置，:803-817 *)
'ensures'  ':' Expr ';'*
'invariant' ':' Expr ';'*
'math' ':' '{' { Expr ';' } '}' ';'    (* :844-861 *)
```

---

## 5. 表达式语法（parse_expr.rs）

### 5.1 二元运算符优先级（parse_expr.rs:38-59，含 pipe 特例）

| 优先级 | 运算符 | 结合性 | Token |
|-------|--------|--------|-------|
| pipe | `\|>` | 左 | PipeArrow（parse_expr.rs:28-35，desugar 为调用；仅表达式管道——转移目标分隔符 `\|>` 已 v0.34.1 拒绝） |
| 1 | `or` / `\|\|` | 左 | OrOr / Or |
| 2 | `and` / `&&` | 左 | AndAnd / And |
| 3 | `==` / `!=` / `..` | 左 | EqEq / Ne / DotDot（Range 仅 parse_expr_inner；slice 起点用 with-`..` 变体 :76-124） |
| 4 | `<` `>` `<=` `>=` | 左 | Lt Gt Le Ge |
| 5 | `\|` | 左 | BitOr |
| 6 | `^` | 左 | BitXor |
| 7 | `&` | 左 | BitAnd |
| 8 | `<<` `>>` | 左 | Shl Shr |
| 9 | `+` `-` | 左 | Plus Minus |
| 10 | `*` `/` `%` | 左 | Star Slash Percent |
| 11 | `**` | **右** | Pow |

[事实] `..` 作为二元运算符被解析为 `Binary(BinOp::Range)`（parse_expr.rs:47），`Expr::Range` variant 全仓零构造（golden-document.md §1.1）。

### 5.2 一元运算符（parse_expr.rs:126-175）

```
Unary := '-' Expr | '!' Expr | 'not' Expr
       | '&' [ 'mut' ] Expr      (* Ref/RefMut *)
       | '*' Expr                (* Deref *)
       | 'old' '(' Expr ')'      (* 软关键字，后随 '(' 才是快照，:159-172 *)
```

### 5.3 表达式主（parse_expr.rs:230-454）

```
Primary := Int | Float | String | FString | true | false | unit
         | '(' { Expr ',' } ')'            (* 单元素 = 分组 *)
         | '[' { Expr ',' } [ '..' Expr ] ']'      (* List/range，parse_bracket_primary *)
         | 'match' Expr '{' MatchArm '}'   (* :331-343 *)
         | 'spawn' Expr (prec 12)          (* :344-349 *)
         | 'await' Expr (prec 12)          (* :350-355 *)
         | 'arena' '{' Block '}'           (* :356-363 *)
         | 'comptime' '{' Block '}'        (* :364-371 *)
         | 'quote' [ '!' ] '{' Block '}'   (* :372-382 *)
         | '$(' Expr ')'                   (* QuoteInterpolate，:383-394 *)
         | 'fn' '(' Params ')' [ '->' Type ] '{' Block '}'   (* Lambda 闭包，:400-420 *)
         | '{' map-literal | set-literal | Block '}'          (* :421-436 *)
         | Ident | 关键字即标识符（is_keyword_token 兜底，:437-443）
         | 'if' Expr '{' Block '}' [ 'else' ... ]  (* if-expr，:177-228 *)
```

```
MatchArm := Pattern [ 'if' Expr ] '=>' ( '{' Block '}' | Expr ) [ ',' ]   (* pattern.rs:6-66 *)
```

### 5.4 后缀运算（parse_expr.rs:456-531）

```
Postfix := Primary { Call | Field | IndexOrSlice | Try | OptionalChain }
Call := '(' Args ')'
Args := [ { ('mutate'|'view') * /* 跳过，borrow 模式由参数声明决定，:541-547 */ Ident '=' Expr /* 命名参数 */ | Expr } ',' ]
Field := '.' Ident | '.' Int                  (* 元组索引 *)
IndexOrSlice := '[' Expr ']' | '[' [Expr] '..' [Expr] ']'    (* :588-632 *)
Try := '?'                                    (* 后缀 loop 内，:488-493 *)
OptionalChain := '?.' Ident | '?.' Int        (* :462-487 *)
Cast := ... 'as' Type                         (* 收尾，:523-529 *)
```

---

## 6. 顶层语法（top_level.rs）

```
Item := [ 'pub' ] [ Attributes ] (
          'comptime' 'func' FuncDef            (* :191-198 *)
        | 'func' FuncDef                       (* :207-211 *)
        | 'module' Ident '{' { 'use' ... } Items '}'   (* :694-718，parse_module *)
        | 'type' ... / 'newtype' ...           (* §2.1 *)
        | 'actor' Ident [ 'runs' Ident ] '{' { Fields | Funcs } '}'   (* :611-692 *)
        | 'const' Ident [ ':' Type ] '=' Expr ';'      (* :230-250 *)
        | 'cap' Ident [ '+' Ident | '=' Ident { '+' Ident } ] ';'     (* :324-354 *)
        | 'trait' Ident generics '{' { 'func' Name generics '(' Params ')' [ '->' Type ] ';' } '}'   (* :356-400 *)
        | 'impl' generics Ident [ '<' Types '>' ] 'for' Type '{' { FuncDef } '}'    (* :402-459 *)
        | 'unsafe' 'extern' [ '"C"' ] '{' ExternFuncs '}'      (* :265-273 *)
        | 'extern' '"C"' 'func' FuncDef        (* Mimi → C 导出，:282-306 *)
        | 'extern' [ '"C"' ] '{' ExternFuncs '}'  (* C → Mimi 导入，:461-609 *)
        | 'flow' ...                          (* §6.1 *)
        | 'protocol' ...                      (* §6.2 *)
        | 'session' ...                       (* §6.3 *)
)
```

```
FuncDef := 'func' Ident generics '(' Params ')' [ '->' Type ]
           [ 'where' { Ident ':' Ident { '+' Ident } ',' } ]
           '{' Block '}'
Params := { [ 'mut' ] Ident ':' [ ('view'|'mutate') ] Type [ '=' Expr ] ',' }   (* :854-908 *)
```

[事实] v0.34.18c（§4.2）：`with` 效果子句**已废除**——parser 拒绝（top_level.rs:774-781，"the `with` effect clause was abolished ... Remove `with ...`"），`with` 保留为 reserved 关键字（负测试）。原 `[ 'with' Ident { ',' Ident } ]` 产生式删除；E0254 双点死代码清理。spec §2.7 仅删 Effect 部分（0.34.18c 完成）。

### 6.1 Flow（top_level.rs:920-1252）

```
Flow := 'flow' Ident generics
        { '@sparse' | '@mailbox' ['(' ['depth' '='] Int ')'] | '@max_children' ['(' ['children' '='] Int ')'] }
        '{'
          { 'impl' Ident ';' }                (* :1048-1054 *)
        | [ 'persistent' ] 'state' Ident [ '{' { Ident ':' Type ',' } '}' ] ';'   (* :1254-1287 *)
        | 'transition' Ident '(' FromIdent [ ',' { [ 'mut' ] Ident ':' [ ('view'|'mutate') ] Type } ] ')'
          '->' Ident { '|' Ident }            (* v0.34.1：仅 `|` 多目标分隔符；`|>` 已拒绝 *)
          [ 'fails' Type ]                    (* :1358-1364 *)
          [ '{' Block '}' | ';' ]             (* body 为裸 block :1381-1385；v0.34.27 do 已删 *)
        | 'fault' Type ';'                    (* per-Flow 单一 typed error，:1216-1222 *)
        '}'
```

[事实] v0.34.1：`@transactional` **已拒绝**（条款 3 诊断，top_level.rs:1061-1070）；`metadata_shadow_fields`/`transactional_fields` FlowDef 字段删除（ast.rs）。
[事实] v0.34.18b（条款 1 sparse-irreversible）：`@dense` **已删除**——parser 拒绝（top_level.rs:935-939，compile error E0211 而非运行时 Fault）；flow_matrix N×M 注入删除；16 @dense 测试迁移（删 4/转 5/迁移 6）。
[事实] v0.34.1：`delegate` 已拒绝（条款 2 诊断）。`|>` 转移分隔符已拒绝。
[事实] `fault Variant { ... }` 变体块语法全仓零匹配——仅 `fault Type`（golden-document.md §3.2）。
[事实] v0.34.3：`for` 绑定为 Pattern（`for (k, v) in m` 解构；单标识符 = Pattern::Variable）——ast.rs For.var。

### 6.2 Protocol（top_level.rs:1389-1468）

```
Protocol := 'protocol' Ident generics '{'
              { 'state' Ident [ '{' Ident ':' Type '}' ] ';' }   (* 单一 payload 字段 *)
            | { 'transition' Ident '(' Ident ')' '->' Ident ';' }
            '}'
```

### 6.3 Session（top_level.rs:1476-1542）

```
Session := 'session' Ident '=' SessionType ';'
SessionType := '!' Type '.' SessionType      (* Send *)
             | '?' Type '.' SessionType      (* Recv *)
             | 'dual' '(' SessionType ')'    (* :1514-1521 *)
             | 'end'                         (* :1522-1525 *)
             | Ident                         (* 命名引用 *)
```

### 6.4 属性（top_level.rs:64-189）

```
Attributes := { '#[' 'derive' '(' ('Debug'|'Clone'|'Eq') { ',' } ')' ']'    (* Copy/Default 预留拒绝 *)
             | '#[' 'repr' '(' ('C'|'transparent') ')' ']'
             | '#[' 'no_panic' ']'          (* 仅 extern 块 *)
             | '#[' 'errno' ']'             (* 仅 extern 块，SD-3 实现为扁平属性 *)
             | '#[' 'errno' ']'             (* extern 块内 per-function，:491-514 *)
             }
```

[事实] 属性约束：derive/repr 仅限 type/newtype；no_panic/errno 仅限 extern 块（top_level.rs:163-189）。
[事实] 未知属性硬错误（top_level.rs:153-159）——PR-H2。

---
## 7. 与 golden 的差异

无差异（0.34.33 起由 golden 重新生成）。差异台账维护在
`devdocs/v0.34/golden/syntax-reference.golden.md` §7。

---
## 附录 A：产生式坐标速查

| 产生式 | 坐标 |
|--------|------|
| 二元优先级表 | parse_expr.rs:38-59 |
| pipe desugar | parse_expr.rs:28-35 |
| 后缀 loop（call/field/`?.`/try） | parse_expr.rs:456-531 |
| slice/index | parse_expr.rs:588-632 |
| f-string 插值 | parse_stmt.rs:655-777 |
| 语句分发 | parse_stmt.rs:17-296 |
| 块内合约子句 | parse_stmt.rs:803-861 |
| 模式 | pattern.rs:68-220 |
| match arms | pattern.rs:6-66 |
| 类型 | parse_type.rs:6-387 |
| 类型定义/union/newtype | parse_type.rs:389-610 |
| func 签名（where/with） | top_level.rs:720-816 |
| 泛型参数 | top_level.rs:818-852 |
| 参数（view/mutate/默认值） | top_level.rs:854-908 |
| flow 定义 | top_level.rs:920-1252 |
| transition 定义 | top_level.rs:1289-1387 |
| 属性 | top_level.rs:64-189 |
| extern 块 | top_level.rs:461-609 |
| actor | top_level.rs:611-692 |
| protocol | top_level.rs:1389-1468 |
| session | top_level.rs:1476-1542 |
| 关键字表 | keywords.rs:92-177 |
