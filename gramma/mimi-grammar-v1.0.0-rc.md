# Mimi 语法规范 v1.0.0-rc

> ⚠ **文档状态（0.34.33 标注）**：本文档是 v1.0.0-rc 时代的语法快照，**不再权威**。
> 0.1.4（0.34）语法冻结后的权威来源为 `docs/language-spec.md`（规范）+
> `devdocs/v0.34/golden/syntax-reference.golden.md`（parser 实况 EBNF，渲染副本
> `docs/syntax-reference.md`）。本文件含多处已废止内容：`do`/`become`/`stay` 已删除、
> `steps`/`ui`/`binds` 已移出关键字表（tokenize 为 Ident）、`flow` 已是 Mimi 一等关键字、
> 关键字总数现为 80。保留作历史参考，勿据其生成代码。

**Mimi**（文件后缀 `.mimi`）是面向 Intent-as-Code 的系统编程语言。

本文档定义 Mimi v1.0.0-rc 的完整**词法**与**语法**。它直接反映解析器实现（`mimi/src/parser/`、`mimi/src/lexer.rs`、`mimi/src/ast.rs`），是语法层面的权威参考。（⚠ 该权威性声明已被 0.34 金标准治理取代，见上方状态注。）

### 权威性与边界说明

1. **Parser 为准**：lexer 仅做分词，它保留某个字符串为关键字不代表 Mimi parser 已支持该语法。本文档以 `src/parser/` 实际分支为最终权威。（0.34 起该角色由 golden EBNF 承担。）
2. **Mimi vs MimiSpec**：`.mimi` 是 Mimi 生产代码；`.mms` 是 MimiSpec 草图。~~`flow`、`ui`、`steps`、`binds`、`and`、`or` 等字符串被 lexer 识别，但仅用于 `mms { ... }` 块内的 MimiSpec 语法，Mimi parser 本身不处理。~~（**过期**：`flow`/`and`/`or` 现为 Mimi 关键字并参与解析；`ui`/`steps`/`binds` 已移出关键字表。现状以 keywords.rs:92-177 为准。）
3. **文档更新**：本文档已清理旧版本中混入的 MimiSpec 或非 Mimi 关键字（如 `error`、`exit`、`done`、`mod`、`iflet`、`@import`）。

> 设计哲学、类型系统、内存模型等高层规范见 [`mimi/docs/mimi.md`](../docs/mimi.md)。

---

## 1. 词法结构

### 1.1 字符集

源文件为 UTF-8 编码。标识符与字符串中的字符集为 Unicode（由 lexer 直接读取字节序列）。

空白字符：空格（`0x20`）、制表符 TAB（`0x09`）、换行 LF（`0x0A`）、回车 CR（`0x0D`）。

### 1.2 注释

```ebnf
Comment       ::= '//' { any_char_until_newline }
BlockComment  ::= '/*' { any_char } '*/'
```

- `//` 到行尾；独占一行时被 lexer 视为逻辑空行（影响 `rule` 附着链）。
- `/* */` 可以嵌套。

### 1.3 关键字

Mimi 以 **parser 实际分支**为最终权威。lexer 中保留的某些字符串仅用于识别，不代表 Mimi parser 已支持其语法。

#### Mimi 关键字（parser 已实现）

以下关键字在 Mimi parser 中有实际处理分支，可在 `.mimi` 生产代码中使用：

```
module   type    newtype  func    actor   trait   impl
if       else    for      while   in      return  break   continue
drop     let     mut      ref     on      failure parasteps
spawn    await   async   arena    unsafe  alloc
shared   local_shared  weak
cap      where   pub     extern  use
comptime quote   match   desc    rule    requires ensures
math     old     mms     fn      true    false
unit     i32     i64     f64     bool    string   nothing
```

#### MimiSpec 保留字（lexer 识别，Mimi parser 不处理）

以下字符串被 lexer 识别为关键字，但 **仅在 `mms { ... }` 块内作为 MimiSpec 语法使用**。在 Mimi parser 中它们没有对应分支，直接写在 `.mimi` 顶层或语句位置会报错：

```
flow     ui      steps    binds
and      or
```

> 例如 `flow OrderLifecycle: ...` 和 `ui Panel binds Model: ...` 应写在 `mms { ... }` 块内，由 MimiSpec 解析器处理；Mimi 只负责识别 `mms {}` 块边界。

#### 已移除/非 Mimi 关键字

以下关键字在旧文档或 MimiSpec 中出现，但 **不是 Mimi lexer/parser 的关键字**，不可在 `.mimi` 中使用：

```
error    exit    done    mod     iflet   @import
```

### 1.4 符号（Punctuation / Operators）

```
(    )    [    ]    {    }
+    -    *    /    %
==   !=   <    >    <=   >=
=    :    ::   ,    ;    ..
->   =>   |    &    !
.    ..   ...  ?    $    $$
```

> `@import` 是 MimiSpec 的导入语法，Mimi 使用 `use` 关键字。

### 1.5 字面量

```ebnf
IntLiteral     ::= ['-' | '+'] Digit { Digit | '_' }
FloatLiteral   ::= ['-' | '+'] Digit { Digit | '_' } '.' Digit { Digit | '_' }
                 | ['-' | '+'] Digit { Digit | '_' } ('.' Digit { Digit | '_' })? ('e' | 'E') ['-' | '+'] Digit { Digit }
StringLiteral  ::= '"' { string_char | escape } '"'
BoolLiteral    ::= 'true' | 'false'
UnitLiteral    ::= '(' ')'
CharLiteral    ::= "'" (char | escape) "'"   ;; 当前版本未完整实现，推荐使用 string
escape         ::= '\\' ('n' | 't' | 'r' | '"' | '\\' | '0')
Digit          ::= '0'..'9'
```

数字字面量允许 `_` 分隔符（如 `1_000_000`），不影响值。

字符串字面量禁止隐式跨行：未闭合引号遇到换行报错。需要物理换行使用 `\n`。

### 1.6 标识符

```ebnf
Ident          ::= (Letter | '_') { Letter | Digit | '_' }
Letter         ::= Unicode 字母字符
```

### 1.7 缩进与花括号

Mimi 支持两种块标记方式：

| 方式 | 语法 | 用途 |
|------|------|------|
| **花括号体** | `{ Stmt; Stmt; ... }` | L4 生产代码的标准形式 |
| **缩进体** | `:\n {Indent} Stmt\n {Dedent}` | `.mms` 草图模式兼容（MimiSpec） |

缩进单位必须为 **4 空格整倍数**。禁止使用 Tab。

花括号体与缩进体可以在同一函数中混合使用（契约前缀用缩进、实现用花括号）：

```mimi
func foo(x: i32) -> i32 {
    requires: x > 0
    ensures: result > 0

    x + 1
}
```

### 1.8 意图后缀（Commitment）

加在关键字、标识符或字符串末尾，无空格：

```ebnf
Commitment     ::= '$' | '$$' | '?' | '??'
                 | '$?' | '$$?' | '$??' | '$$??'
```

顺序固定：**先锁定，后不确定**。`?$` / `?$$` / `??$` / `??$$` 非法。

| 后缀 | 含义 |
|------|------|
| `?` | 不确定 / 请求 AI 审视 |
| `??` | 完全委托 AI |
| `$` | 设计锁定：AI 不得修改 |
| `$$` | 强锁定：需人类显式解锁 |
| `$?` / `$$?` | 锁定但请 AI 审视 |
| `$??` / `$$??` | 锁定但 AI 可决定保留 |

---

## 2. 语法（EBNF）

### 2.1 文件结构

```ebnf
File           ::= { Import } { Item }

Import         ::= 'use' Path ';'

Path           ::= Ident { '::' Ident }
```

> `@import "..."` 是 MimiSpec（`.mms`）的导入语法；Mimi（`.mimi`）使用 `use`。

一个文件可以有零或多个 `Import`（必须位于所有 `Item` 之前），零或多个顶层 `Item`。

### 2.2 顶层项（Items）

```ebnf
Item           ::= ModuleDef
                 | TypeDef
                 | NewtypeDef
                 | FuncDef
                 | ActorDef
                 | TraitDef
                 | ImplDef
                 | ExternBlock
                 | CapDef
                 | DescStmt
                 | RuleStmt
```

> `FlowDef` / `UiDef` 不是 Mimi 顶层项；它们属于 MimiSpec 语法，应写在 `mms { ... }` 块内。

#### 2.2.1 模块

```ebnf
ModuleDef      ::= 'module' Ident Commitment? '{' { Item } '}'
                 | 'module' Ident Commitment? ':'                         ;; 缩进体（草图模式）
                   Indent { Item } Dedent
```

#### 2.2.2 类型定义

```ebnf
TypeDef        ::= TypeAnnotation
                 | 'type' Ident Commitment? [ '<' GenericParams '>' ] '{' RecordBody '}'
                 | 'type' Ident Commitment? [ '<' GenericParams '>' ] '{' EnumBody '}'

NewtypeDef     ::= 'newtype' Ident Commitment? '=' Type ';'

TypeAnnotation ::= 'type' Ident Commitment? [ '<' GenericParams '>' ] '=' Type ';'

GenericParams  ::= Ident { ',' Ident } [ ',' ]

RecordBody     ::= Field { ',' Field } [ ',' ]

Field          ::= Ident ':' Type

EnumBody       ::= Variant { ',' Variant } [ ',' ]

Variant        ::= Ident
                 | Ident '(' Type { ',' Type } ')'             ;; Tuple payload
                 | Ident '{' Field { ',' Field } '}'           ;; Record payload
```

#### 2.2.3 函数定义

```ebnf
FuncDef        ::= [ 'pub' ] [ 'async' ] 'func' Ident Commitment?
                   [ '<' GenericParams '>' ]
                   '(' [ Params ] ')' [ '->' Type ]
                   [ 'where' WhereClause ]
                   FunctionBody

Params         ::= Param { ',' Param } [ ',' ]

Param          ::= [ 'mut' ] Ident ':' Type

WhereClause    ::= Ident ':' Ident { '+' Ident }                ;; TypeParam: Trait1 + Trait2

FunctionBody   ::= ContractPrefix BlockBody
                 | ContractPrefix                              ;; 草图模式：仅有契约，无实现体

ContractPrefix ::= { RequiresClause | EnsuresClause | MathClause | DescClause }

BlockBody      ::= '{' { Stmt } '}'

RequiresClause ::= 'requires' ':' Expression
EnsuresClause  ::= 'ensures' ':' Expression
MathClause     ::= 'math' ':' Indent { MathStmt } Dedent
DescClause     ::= 'desc' StringLiteral
```

#### 2.2.4 Actor 定义

```ebnf
ActorDef       ::= 'actor' Ident Commitment? '{'
                     { ActorField | FuncDef }
                   '}'

ActorField     ::= [ 'mut' ] Ident ':' Type [ '=' Expression ]
```

Actor 的方法定义与普通 `FuncDef` 相同，但隐式携带 `self: &ActorType` 参数。

#### 2.2.5 Trait / Impl 定义

```ebnf
TraitDef       ::= 'trait' Ident Commitment?
                   [ '<' GenericParams '>' ]
                   '{' { TraitMethod } '}'

TraitMethod    ::= 'func' Ident '(' [ Params ] ')' [ '->' Type ] ';'

ImplDef        ::= 'impl' TraitPath 'for' Ident '{' { FuncDef } '}'

TraitPath      ::= Ident [ '<' Type { ',' Type } '>' ]        ;; TraitName<ConcreteType>
```

#### 2.2.6 Extern 块

```ebnf
ExternBlock    ::= 'extern' StringLiteral '{' { ExternFunc } '}'

ExternFunc     ::= 'func' Ident '(' [ Params ] ')' [ '->' Type ] ';'
```

#### 2.2.7 Cap 定义

```ebnf
CapDef         ::= 'cap' Ident Commitment? [ '=' CapComposition ] ';'

CapComposition ::= Ident '+' Ident                            ;; cap A = B + C
```

#### 2.2.8 Flow / Ui（MimiSpec 保留字）

`flow`、`ui`、`steps`、`binds` 不是 Mimi 顶层项或语句关键字。它们属于 MimiSpec，仅在 `mms { ... }` 块内有效：

```mimi
mms {
    flow OrderLifecycle:
        New to Pending

    ui OrderPanel binds order:
        stack "订单面板":
            "支付" desc "按钮"
}
```

Mimi parser 只负责识别 `mms { ... }` 块边界，块内内容由 MimiSpec 解析器处理。

### 2.3 语句（Statements）

```ebnf
Stmt           ::= LetStmt
                 | AssignStmt
                 | IfStmt
                 | ForStmt
                 | WhileStmt
                 | ReturnStmt
                 | BreakStmt
                 | ContinueStmt
                 | ExprStmt
                 | BlockStmt
                 | ParastepsStmt
                 | OnFailureStmt
                 | ArenaStmt
                 | UnsafeStmt
                 | AllocStmt
                 | DropStmt
                 | SharedLetStmt
                 | MmsBlockStmt
                 | DescStmt
                 | RequiresStmt
                 | EnsuresStmt
                 | MathStmt
                 | EmptyStmt
```

#### 2.3.1 变量声明

```ebnf
LetStmt        ::= 'let' [ 'mut' ] Pattern [ ':' Type ] [ '=' Expression ]

SharedLetStmt  ::= 'let' 'shared' Ident [ ':' Type ] '=' Expression
                 | 'let' 'local_shared' Ident [ ':' Type ] '=' Expression
                 | 'let' 'weak' Ident [ ':' Type ] '=' Expression
```

#### 2.3.2 赋值

```ebnf
AssignStmt     ::= LValue '=' Expression

LValue         ::= Ident
                 | LValue '.' Ident
                 | LValue '[' Expression ']'
```

#### 2.3.3 控制流

```ebnf
IfStmt         ::= 'if' Expression BlockBody [ 'else' ( BlockBody | IfStmt ) ]

ForStmt        ::= 'for' Pattern 'in' Expression BlockBody

WhileStmt      ::= 'while' Expression BlockBody

ReturnStmt     ::= 'return' Expression? ';'?

BreakStmt      ::= 'break' Expression? ';'?

ContinueStmt   ::= 'continue' ';'?

BlockStmt      ::= BlockBody
```

#### 2.3.4 并发与补偿

```ebnf
ParastepsStmt  ::= 'parasteps' [ StringLiteral ] BlockBody

OnFailureStmt  ::= 'on' 'failure' BlockBody

SpawnExpr      ::= 'spawn' Expression

AwaitExpr      ::= 'await' Expression
```

#### 2.3.5 内存区域

```ebnf
ArenaStmt      ::= 'arena' BlockBody

UnsafeStmt     ::= 'unsafe' BlockBody

AllocStmt      ::= 'alloc' Ident ':' Type '{' Expression '}' BlockBody
```

#### 2.3.6 其他语句

```ebnf
DropStmt       ::= 'drop' '(' Expression ')'

ExprStmt       ::= Expression ';'?

MmsBlockStmt   ::= 'mms' '{' { any_char } '}'                   ;; 元数据块，编译器忽略内容

DescStmt       ::= 'desc' StringLiteral

RequiresStmt   ::= 'requires' ':' Expression

EnsuresStmt    ::= 'ensures' ':' Expression

MathStmt       ::= 'math' ':' Indent { MathLine } Dedent

EmptyStmt      ::= ';'
```

### 2.4 模式（Patterns）

```ebnf
Pattern        ::= Wildcard
                 | Variable
                 | LiteralPattern
                 | ConstructorPattern
                 | TuplePattern
                 | ArrayPattern
                 | SlicePattern

Wildcard       ::= '_'

Variable       ::= Ident

LiteralPattern ::= IntLiteral | FloatLiteral | BoolLiteral | StringLiteral | UnitLiteral

ConstructorPattern ::= Ident '(' [ Pattern { ',' Pattern } ] ')'

TuplePattern   ::= '(' [ Pattern { ',' Pattern } [ ',' ] ] ')'

ArrayPattern   ::= '[' [ Pattern { ',' Pattern } [ ',' ] ] ']'

SlicePattern   ::= '[' Pattern { ',' Pattern } [ ',' ] '..' [ Pattern ] ']'
```

### 2.5 表达式（Expressions）

```ebnf
Expression     ::= LogicalOrExpr

LogicalOrExpr  ::= LogicalAndExpr { '||' LogicalAndExpr }

LogicalAndExpr ::= EqualityExpr { '&&' EqualityExpr }

EqualityExpr   ::= ComparisonExpr { ('==' | '!=') ComparisonExpr }

ComparisonExpr ::= AddExpr { ('<' | '>' | '<=' | '>=') AddExpr }

AddExpr        ::= MulExpr { ('+' | '-') MulExpr }

MulExpr        ::= UnaryExpr { ('*' | '/' | '%') UnaryExpr }

UnaryExpr      ::= ( '-' | '!' ) UnaryExpr
                 | 'not' UnaryExpr
                 | PostfixExpr

PostfixExpr    ::= PrimaryExpr { PostfixOp }

PostfixOp      ::= '.' Ident                                  ;; Field access / method call
                 | '[' Expression ']'                          ;; Index
                 | '(' [ Expression { ',' Expression } ] ')'   ;; Function call
                 | '::' '<' Type { ',' Type } '>'              ;; Turbofish
                 | '?'                                         ;; Try (short-circuit error propagation)

PrimaryExpr    ::= Literal
                 | Ident
                 | '(' Expression ')'
                 | BlockBody
                 | IfExpr
                 | MatchExpr
                 | SpawnExpr
                 | AwaitExpr
                 | 'quote' '{' { any_char } '}'
                 | 'comptime' BlockBody
                 | RecordExpr
                 | '...'                                       ;; Placeholder
```

#### 2.5.1 If 表达式

```ebnf
IfExpr         ::= 'if' Expression BlockBody [ 'else' ( BlockBody | IfExpr ) ]
```

`if` 在语句位置时不产生值；在表达式位置时最后一个分支必须存在。

#### 2.5.2 Match 表达式

```ebnf
MatchExpr      ::= 'match' Expression '{'
                     { MatchArm ','? }
                   '}'

MatchArm       ::= Pattern [ 'if' Expression ] '=>' Expression
```

模式匹配要求穷尽性检查（编译器实现中）。

#### 2.5.3 Record 表达式

```ebnf
RecordExpr     ::= Ident '{'
                     [ RecordField { ',' RecordField } [ ',' ] ]
                   '}'

RecordField    ::= Ident ':' Expression
```

### 2.6 类型表达式

```ebnf
Type           ::= TypeOrExpr

TypeOrExpr     ::= BaseType
                 | NamedType
                 | TupleType
                 | FuncType
                 | RefType
                 | OptionalType
                 | CapType
                 | ImplTraitType
                 | InferType

BaseType       ::= 'i32' | 'i64' | 'f64' | 'bool' | 'string' | 'unit'

NamedType      ::= Ident [ '<' Type { ',' Type } '>' ]        ;; e.g. List<i32>, Result<T, E>

TupleType      ::= '(' Type { ',' Type } [ ',' ] ')'

FuncType       ::= [ 'fn' ] '(' [ Type { ',' Type } ] ')' '->' Type   ;; v1.0 仅内部使用

RefType        ::= '&' [ 'mut' ] Type
                 | '&' [ 'mut' ] '[' Type ']'

OptionalType   ::= Type '?'                                     ;; T? 等价于 Option<T>

CapType        ::= Ident                                        ;; cap 名称作为类型名

ImplTraitType  ::= 'impl' Ident { '+' Ident }                  ;; impl Trait1 + Trait2

InferType      ::= '_'                                          ;; ⚠️ 当前未实现

ArrayType      ::= '[' Type ';' IntLiteral ']'                  ;; 定长数组 [T; n]
                 ;; | '[' Type ']'                                ;; 切片 [T] 当前未实现
```

---

## 3. 运算符优先级（从高到低）

| 优先级 | 类别 | 运算符 | 结合性 |
|--------|------|--------|--------|
| 1 | 后缀 | `.` `[]` `()` `::<>` `?` | 左 |
| 2 | 一元 | `-` `!` `not` | 右 |
| 3 | 乘除 | `*` `/` `%` | 左 |
| 4 | 加减 | `+` `-` | 左 |
| 5 | 比较 | `<` `>` `<=` `>=` | 左 |
| 6 | 相等 | `==` `!=` | 左 |
| 7 | 逻辑与 | `&&` | 左 |
| 8 | 逻辑或 | `\|\|` | 左 |

---

## 4. 内置函数

以下函数在语言运行时中直接提供，无需 `import`：

### 4.1 I/O

```
print(s: string)          — 打印字符串，不换行
println(s: string)        — 打印字符串，换行
eprintln(s: string)       — 打印到 stderr
input() -> string         — 从 stdin 读取一行
```

### 4.2 集合

```
len(xs) -> i64            — 返回长度
push(xs, val)             — 追加元素（List）
pop(xs) -> T              — 弹出末尾元素
contains(xs, val) -> bool — 检查包含
map(xs, fn) -> List       — 映射（需闭包）
filter(xs, fn) -> List    — 过滤
reduce(xs, init, fn) -> T — 归约
sort(xs)                  — 排序
reverse(xs)               — 反转
```

### 4.3 文件系统

```
read_file(path) -> string        — 读取文件内容
write_file(path, content)        — 写入文件
file_exists(path) -> bool        — 检查文件是否存在
```

### 4.4 字符串

```
str_char_at(s, i) -> string      — 取第 i 个字符
str_substring(s, start, end)     — 子串
str_trim(s) -> string            — 去除首尾空白
str_to_upper(s) -> string        — 转大写
str_to_lower(s) -> string        — 转小写
str_contains(s, sub) -> bool     — 包含子串
str_split(s, sep) -> List        — 分割
str_join(list, sep) -> string    — 连接
```

### 4.5 数学

```
abs(x) -> T                      — 绝对值
min(a, b) -> T                   — 最小值
max(a, b) -> T                   — 最大值
sqrt(x) -> f64                   — 平方根
pow(base, exp) -> f64            — 幂
floor(x) -> f64                  — 向下取整
ceil(x) -> f64                   — 向上取整
round(x) -> f64                  — 四舍五入
random() -> f64                  — [0, 1) 随机数
pi() -> f64                      — 圆周率
```

### 4.6 类型转换

```
to_string(x) -> string           — 转字符串
to_int(s) -> i64                 — 字符串转整数
to_float(s) -> f64               — 字符串转浮点
int_to_string(i) -> string       — 整数转字符串
float_to_string(f) -> string     — 浮点转字符串
string_to_int(s) -> i64          — 字符串转整数（别名）
```

### 4.7 断言与退出

```
assert(cond)                     — 断言条件为真
assert_eq(a, b)                  — 断言相等
assert_ne(a, b)                  — 断言不等
exit(code: i32)                  — 以退出码终止进程
```

### 4.8 元数据

```
type_name(x) -> string           — 运行时类型名称
type_fields(T) -> List           — 类型字段列表（v1.1）
```

---

## 5. 编译指示

### 5.1 文件级别

```ebnf
Pragma         ::= '#[' Ident [ '(' PragmaArgs ')' ] ']'

PragmaArgs     ::= StringLiteral | Ident | IntLiteral
```

内置编译指示：

```
#[derive(Debug)]      — 自动生成 to_string()
#[derive(Clone)]      — 自动生成 clone()
#[derive(Eq)]         — 自动生成 eq()
```

### 5.2 `allow` 属性

```ebnf
#[allow(dead_code)]
```

抑制特定警告。

---

## 6. 版本信息

- **规范版本**: v1.0.0-rc
- **对应实现**: mimi v0.7.0
- **最后更新**: 2026-06-17（已按 parser 实现校正关键字边界）

---

## 附录 A：关键字完整列表

### Mimi 关键字（parser 已实现）

| 类别 | 关键字 |
|------|--------|
| 声明 | `module` `type` `newtype` `func` `actor` `trait` `impl` `cap` |
| 契约 | `desc` `rule` `requires` `ensures` `math` `old` |
| 控制流 | `if` `else` `for` `while` `in` `match` `return` `break` `continue` |
| 并发 | `parasteps` `spawn` `await` `async` `on` `failure` |
| 内存 | `arena` `unsafe` `alloc` `ref` `mut` `shared` `local_shared` `weak` |
| 权限 | `cap` `drop` |
| 元编程 | `comptime` `quote` `fn` |
| 模块 | `use` `pub` `extern` |
| 字面量/类型 | `true` `false` `unit` `i32` `i64` `f64` `bool` `string` `nothing` |
| 逻辑 | `not` |
| 其他 | `let` `where` `mms` `with` |

### MimiSpec 保留字（lexer 识别，Mimi parser 不处理）

这些关键字在 `mms { ... }` 块内由 MimiSpec 解析器使用：

```
flow     ui      steps    binds
and      or
```

### 已移除/非 Mimi 关键字

以下字符串不是 Mimi 关键字，也不应在 `.mimi` 文件中使用：

```
error    exit    done    mod     iflet   @import   ~
```
