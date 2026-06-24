# Mimi 语法参考

> 本文档描述 Mimi 语言的完整语法，可作为自举实现的语法底本。
> 版本: v0.22.4-dev
> 数据来源: `src/lexer/`, `src/parser/`, `src/ast.rs`

## 1. 词法

### 1.1 注释

```
// 行注释（到行尾）
```

块注释 `/* ... */` 支持嵌套。

### 1.2 标识符

```
identifier ::= [a-zA-Z_][a-zA-Z0-9_]*
```

- 以字母或 `_` 开头
- 后续字符为字母、数字或 `_`
- 关键字不可作为裸标识符使用（见 §1.4）

### 1.3 字面量

```
integer     ::= decimal | hex | binary | octal
decimal     ::= [0-9]([0-9_]*[0-9])?    // 十进制（支持下划线分隔）
hex         ::= "0" ("x" | "X") hex_digit+
binary      ::= "0" ("b" | "B") ("0" | "1" | "_")+
octal       ::= "0" ("o" | "O") ("0"-"7" | "_")+
float       ::= digit+ "." digit+ ([eE][+-]? digit+)?
string      ::= "\"" string_char* "\""
fstring     ::= "f" "\""
                 ( string_char
                 | "{" expr "}"     // 插值表达式
                 | "\\{" / "\\}"   // 转义大括号
                 )*
                 "\""
bool        ::= "true" | "false"
unit        ::= "()"
```

**转义序列**（字符串和 f-string 通用）：

| 序列 | 含义 |
|------|------|
| `\n` | 换行 |
| `\t` | 制表符 |
| `\r` | 回车 |
| `\\` | 反斜杠 |
| `\"` | 双引号 |
| `\{` | 左大括号（仅 f-string 中需要） |
| `\}` | 右大括号（仅 f-string 中需要） |
| `\0` | 空字符 |
| `\xHH` | 十六进制字节 |
| `\uHHHH` | 四位数 Unicode |
| `\u{H...}` | Unicode 码点（花括号形式） |

注：没有独立的 `char` 字面量类型。`'a'` 在词法上是 `Tick` 标记，用于生命周期。

### 1.4 关键字

```
module    type      func      fn        actor
newtype   let       mut       ref       shared
local_shared  weak  weak_local    c_shared  c_borrow
c_borrow_mut  raw_string  arena   alloc   cap
trait     impl      dyn       where     extern
if        else      for       in        while
return    break     continue  match     use
pub       drop      await     async     unsafe
spawn     steps     parasteps quote     comptime
failure   requires  ensures   invariant math
desc      rule      old       mms       with
and       or        not       true      false
unit      nothing
```

`i32`, `i64`, `f64`, `bool`, `string` 是预置类型名（以标识符形式存在，非独立关键字）。

### 1.5 运算符标记

```
+   -   *   /   %   **          // 算术运算符
=                                     // 赋值
==  !=  <   >   <=  >=               // 比较
&&  ||  !                            // 逻辑
&   |   ^   ~   <<   >>              // 位运算
+=  -=  *=  /=  &=  |=  ^=          // 复合赋值
->          // 箭头（函数返回值）
=>          // 胖箭头（match arm）
..          // 范围 / 切片
...         // 省略号（sketch 模式占位 / variadic）
?           // 问号（错误传播）
@           // at（cap 模式标注 / 属性）
#           // hash（属性开头）
$           // dollar（仅限 $( 用于 quote 插值）
```

### 1.6 标点标记

```
(   )   {   }   [   ]   :   ::   ;   ,   .   '
```

### 1.7 特殊标记

```
INDENT   DEDENT   NEWLINE   EOF
```

`INDENT`/`DEDENT` 仅在 sketch 模式 (`.mms`) 中使用，基于 4 空格缩进。

## 2. 类型系统

### 2.1 基本类型

| 类型 | 描述 | 内存 |
|------|------|------|
| `i32` | 32 位有符号整数 | 4 字节 |
| `i64` | 64 位有符号整数 | 8 字节 |
| `f64` | 64 位浮点数 | 8 字节 |
| `bool` | 布尔值 (`true` / `false`) | 1 字节 |
| `string` | UTF-8 字符串（赋值拷贝语义） | 指针 + 长度 |
| `nothing` | 底类型（不可达 / 错误类型） | 0 字节 |

### 2.2 复合类型

| 类型 | 示例 | 描述 |
|------|------|------|
| 元组 | `(i32, string)` | 异质序列 |
| 列表 | `[i32]` | 同质动态数组 |
| 固定数组 | `[i32; 4]` | 编译期定长数组 |
| 切片 | `&[T]` | 不可变切片视图 |
| 函数 | `func(i32) -> bool` | 函数类型（注意是 `func` 关键字） |
| 外部函数 | `extern "C" fn(i32) -> bool` | C ABI 函数指针 |
| Option | `i32?` 或 `Option<i32>` | 可选值（`?` 后缀语法糖） |
| Result | `Result<i32, string>` | 结果类型 |
| Set | `Set<T>` | 唯一元素集合（字面量 `{1, 2, 3}`） |
| impl Trait | `impl Clone + Default` | 不透明返回类型 |
| dyn Trait | `dyn Clone` | 运行时 trait 对象（胖指针） |

### 2.3 引用类型

```
&'a T         // 不可变引用（生命周期可选）
&'a mut T     // 可变引用（生命周期可选）
*T            // 裸指针（不可变）
*mut T        // 裸指针（可变）
```

生命周期用 `'name` 表示（`'a`, `'b` 等）。

### 2.4 共享 / 弱引用

```
shared T             // 原子引用计数（跨线程，Arc<RwLock<T>>）
local_shared T       // 非原子引用计数（单线程，Rc<RefCell<T>>）
weak T               // 共享弱引用
weak_local T         // 局部共享弱引用
c_shared T           // C 兼容共享句柄
c_borrow T           // C 兼容不可变借用
c_borrow_mut T       // C 兼容可变借用
raw_string           // C 原始字符串（C 须通过 mimi_string_free_raw 释放）
```

### 2.5 分配器

```
Allocator             // 分配器类型
alloc(System) { ... } // 系统分配器
alloc(Arena) { ... }  // Arena 区域分配器
alloc(Bump) { ... }   // Bump 分配器
```

### 2.6 C 兼容类型

```
CBuffer<T>            // C 缓冲类型（自动 malloc/free）
*T                    // 裸只读指针
*mut T                // 裸可变指针
extern "C" fn(Args...) -> Ret  // C 函数指针
#[repr(C)]            // C 兼容布局属性（枚举/结构体）
#[repr(transparent)]  // 透明包装属性
```

### 2.7 用户定义类型

```
// 枚举
type Option<T> {
    Some(T)
    None
}

// 记录（结构体）
type Point {
    x: f64,
    y: f64,
}

// 枚举变体可含元组或记录 payload
type Tree<T> {
    Leaf(T),
    Node { left: Tree<T>, right: Tree<T> },
}

// 类型别名
type MyInt = i32

// 强包装类型（newtype）
newtype UserId = i32

// C 兼容联合（仅 #[repr(C)]）
#[repr(C)]
type Value = union {
    int_val: i32,
    float_val: f64,
}
```

### 2.8 泛型

```
func identity<T>(x: T) -> T { x }

// 泛型约束
func clone<T: Clone>(x: T) -> T { ... }

// 多重约束
func process<T: Clone + Default>(x: T) -> T { ... }

// where 子句
func map<T, U>(x: T, f: func(T) -> U) -> U where T: Clone { ... }

// 类型泛型
type Container<T> {
    value: T,
}
```

## 3. 表达式

### 3.1 运算符优先级（从低到高）

| 优先级 | 运算符 | 结合性 | 说明 |
|--------|--------|--------|------|
| 1 | `\|\|` | 左 | 逻辑或 |
| 2 | `&&` | 左 | 逻辑与 |
| 3 | `==` `!=` `..` | 左 | 相等比较和范围 |
| 4 | `<` `>` `<=` `>=` | 左 | 大小比较 |
| 5 | `\|` | 左 | 按位或 |
| 6 | `^` | 左 | 按位异或 |
| 7 | `&` | 左 | 按位与 |
| 8 | `<<` `>>` | 左 | 移位 |
| 9 | `+` `-` | 左 | 加减 |
| 10 | `*` `/` `%` | 左 | 乘除取模 |
| 11 | `**` | **右** | 幂运算 |
| — | postfix | — | 函数调用/字段/索引/切片 |
| — | unary | — | 负号/取反/引用/解引用 |

### 3.2 一元运算符

```
-expr       // 取负
!expr       // 逻辑非
not expr    // 逻辑非（关键字形式）
&expr       // 不可变引用
&mut expr   // 可变引用
*expr       // 解引用
```

### 3.3 字面量 / 标识符

```
42              // 整数
-1              // 负整数
3.14            // 浮点数
true            // 布尔真
false           // 布尔假
()              // 单元值
"hello"         // 字符串
f"x = {x}"      // 格式化字符串（插值）
[1, 2, 3]       // 列表
{"a": 1, "b": 2} // Map 字面量（键必为字符串）
{1, 2, 3}       // Set 字面量（≥2 元素，{expr} 仍为块）
x               // 变量引用
```

### 3.4 后置运算

```
func_name(arg1, arg2)       // 函数调用
obj.method(arg1)            // 方法调用
record.field                // 字段访问
tuple.0                     // 元组索引
array[i]                    // 索引
array[start..end]           // 切片
array[..end]                // 半开切片
array[start..]              // 半开切片
expr?                       // 错误传播（Try）
```

### 3.5 副作用表达式

```
{ stmt; stmt; expr }       // 块表达式（最后表达式为值）
if cond { ... } else { ... }  // if 表达式
match expr { ... }          // 模式匹配
```

### 3.6 闭包（Lambda）

```
fn(param: Type) -> RetType { body }
fn(x: i32) -> i32 { x + 1 }
```

闭包可以捕获环境变量。捕获变量通过引用计数（`shared`）在闭包和外部作用域间共享。

### 3.7 元组 / 列表 / 记录

```
(1, "hello", true)          // 元组
[1, 2, 3]                   // 列表字面量
{"a": 1, "b": 2}           // Map 字面量
{1, 2, 3}                   // Set 字面量
Point { x: 1.0, y: 2.0 }    // 记录构造
Point { x, y }              // 记录构造（字段简写）
[expr for var in iter]       // 列表推导
[expr for var in iter if cond]  // 带过滤的列表推导
```

### 3.8 其他表达式

```
// 范围表达式
start..end

// 快照（用于 ensures 后置条件）
old(expr)

// Arena 块表达式
arena { stmt; expr }

// Comptime 块（编译时执行）
comptime { stmt; expr }

// Quote 块（编译时 AST 生成）
quote! { stmt; stmt }

// Quote 插值
$(expr)

// Turbofish（显式泛型实例化）
func_name::<Type>(args)

// 类型信息
type_name(expr)     // 运行时类型名（字符串）
type_info(Type)     // 类型元数据

// 异步 / 并发
spawn expr          // 生成异步任务
await expr          // 等待 Future
```

## 4. 语句

### 4.1 Let 绑定

```
let x = expr              // 不可变绑定
let mut x = expr          // 可变绑定
let x: Type = expr        // 带类型标注
let ref x = expr          // Arena 引用绑定
let (a, b) = expr         // 解构绑定
let x;                    // 声明而不初始化（后需赋值）
```

### 4.2 赋值

```
x = expr                // 变量赋值
x.field = expr          // 字段赋值
x[i] = expr             // 索引赋值
x += expr               // 复合赋值（+= -= *= /= &= |= ^=）
```

### 4.3 共享绑定

```
shared x = expr              // 原子引用计数
local_shared x = expr        // 局部引用计数
weak x = expr                // 弱引用（从 shared 升级）
weak_local x = expr          // 局部弱引用
```

所有共享绑定可选类型标注：`shared x: Type = expr`。

### 4.4 控制流

```
if cond { ... } else { ... }       // if/else
if cond { ... } else if ...        // 链式 elif（else 内嵌 if）
while cond { ... }                 // while 循环
for var in iterable { ... }        // for 循环
break                              // 跳出循环
break expr                         // 带值 break
continue                           // 继续循环
return expr                        // 函数返回
```

### 4.5 函数声明

```
// 普通函数
func name(param: Type) -> RetType {
    body
}

// 公有函数
pub func add(x: i32, y: i32) -> i32 { x + y }

// 泛型函数
func first<T>(list: [T]) -> T { list[0] }

// 带 where 子句
func clone<T>(x: T) -> T where T: Clone { ... }

// 带效果（能力声明）
func write_file(path: string, data: string) with FileIO { ... }

// 异步函数
async func fetch(url: string) -> string { ... }

// Comptime 函数（编译时执行，仅在解释器中可用）
comptime func generate_code() -> func(i32) -> i32 { ... }

// 外部导出（不修改符号名，C ABI）
extern "C" func my_export(x: i32) -> i32 { x + 1 }
```

隐式返回：函数体最后一个表达式的值作为返回值。

### 4.6 模块和导入

```
use path::to::module              // 导入模块
module Name { ... }               // 模块定义
pub func / pub type / pub actor   // 公有可见性
```

### 4.7 类型声明

```
type Name { Variant1, Variant2(Type) }       // 枚举
type Name { field1: Type, field2: Type }     // 记录
type Alias = ExistingType                    // 别名
newtype Name = ExistingType                  // 强包装
#[repr(C)] type Name { ... }                 // C 兼容布局
type Name = union { ... }                    // C 兼容联合
```

### 4.8 Trait / Impl

```
// Trait 定义
trait Clone<T> {
    func clone(x: T) -> T;
}

// Impl 实现
impl<T: Clone> Clone<T> for MyType {
    func clone(x: MyType) -> MyType { ... }
}
```

### 4.9 Actor

```
actor Counter {
    mut count: i32 = 0

    func increment() -> i32 {
        count = count + 1
        count
    }
}
```

### 4.10 Capability 声明

```
// 简单能力
cap FileIO

// 组合能力
cap FileIO + NetworkIO
// 或
cap FileIO = FileRead + FileWrite
```

### 4.11 Extern 块（FFI 导入）

```
extern "C" {
    // 基本 FFI
    func puts(s: string) -> i32

    // 带 cap mode 标注（传递能力）
    func send(cap@ conn: c_shared Connection, data: raw_string)

    // 带借用的 cap mode
    func read(& buf: CBuffer<u8>) -> i32

    // 合约（前置/后置条件）
    func divide(a: i32, b: i32) -> i32
        requires: b != 0
        ensures: result * b == a

    // Variadic 函数
    func printf(fmt: raw_string, ...) -> i32

    // 无 panic 保护（catch_unwind + 信号处理）
    #[no_panic]
    func unsafe_ffi_call(ptr: *mut T) -> i32
}
```

### 4.12 合约

```
requires: expr     // 前置条件
ensures: expr      // 后置条件（可用 old(expr) 引用入口快照）
invariant: expr    // 循环不变量
math: {            // 数学公式块（供 Z3 验证器使用）
    expr1
    expr2
}
```

### 4.13 Comptime / Quote

```
comptime func gen() { ... }      // 编译时函数
comptime { stmt; expr }          // 编译时块表达式
quote! { stmt; expr }            // AST 引用（仅在 interpreter 中可用）
$(expr)                          // Quote 插值
```

### 4.14 元数据

```
desc "描述文本"                    // 描述元数据
rule "规则文本"                    // 规则元数据（MimiSpec 层）
mms { 意图内容 }                   // MimiSpec 块
```

### 4.15 并发语句

```
parasteps { ... }                          // 并行步骤块
spawn expr                                 // 生成并发任务
await expr                                 // 等待异步任务完成
on failure { ... }                         // 补偿块（parasteps 失败时执行）
```

### 4.16 其他语句

```
unsafe { ... }          // 不安全块
arena { ... }           // Arena 区域块（表达式形式也有）
alloc(System) { ... }   // 指定分配器
alloc(Arena) { ... }
alloc(Bump) { ... }
drop(expr)              // 释放能力
{ ... }                 // 嵌套块
...                     // 占位符（仅 sketch 模式）
```

## 5. 模式匹配

### 5.1 模式语法

```
pattern ::= "_"                        // 通配符
          | identifier                 // 变量绑定
          | literal                    // 字面量（42, "hello", true）
          | Name(args...)              // 构造器模式
          | Name { field: pat, ... }   // 记录构造器模式
          | (p1, p2)                   // 元组模式
          | [p1, p2]                   // 数组模式
          | [p1, ..rest]               // 切片模式（rest 绑定剩余元素）
```

### 5.2 Match 表达式

```
match expr {
    Pattern1 if guard => body,     // 带守卫（if 条件）
    Pattern2 => body,
    _ => default_body,
}
```

- Guard 是 `if` 后跟任意表达式
- Match arm body 可以是块 `{ ... }` 或单个表达式
- 臂间用逗号分隔

### 5.3 Let 解构

```
let (a, b) = tuple_expr
let [x, y, z] = list_expr
let Some(value) = optional_expr
```

## 6. 属性

```
#[derive(Trait1, Trait2)]    // 自动派生
#[repr(C)]                   // C 兼容内存布局
#[repr(transparent)]         // 透明包装（与内部类型同布局）
#[no_panic]                  // 外部队块免 panic 包装
```

## 7. 顶层程序结构

```
// 源文件结构
file ::= import* item*

import  ::= "use" path ";"                    // 路径： ident "::" ident ...
item    ::= attr* vis? func_def
          | attr* vis? type_def
          | attr* vis? newtype_def
          | attr* vis? actor_def
          | cap_def
          | trait_def
          | impl_def
          | extern_block
          | "comptime" func_def               // comptime 函数
          | "async" func_def                  // 异步函数
          | "unsafe" extern_block             // 免 passport 类型检查

vis     ::= "pub"
attr    ::= "#" "[" Ident ( "(" attr_args ")" )? "]"
```

### 7.1 函数定义

```
func_def ::= "func" ident generic_params?
             "(" params? ")"
             ("->" type)?
             ("where" ident ":" bound ("+" bound)*)?
             ("with" ident ("," ident)*)?
             block

params ::= param ("," param)*
param  ::= "mut"? ident ":" type
```

### 7.2 类型定义

```
type_def      ::= "type" ident generic_params? ("=" type | block)
newtype_def   ::= "newtype" ident generic_params? "=" type ";"

generic_params ::= "<" generic_param ("," generic_param)* ">"
generic_param  ::= ident (":" bound ("+" bound)*)?
```

### 7.3 外部块

```
extern_block ::= "extern" string_literal? "{" extern_func* "}"
extern_func  ::= ("func" | "fn") ident
                 "(" extern_param? ("," extern_param)* (",")? "..."? ")"
                 ("->" type)?
                 ("requires" ":" expr)?
                 ("ensures" ":" expr)?
                 ";"

extern_param ::= ("cap" "@" | "&")? ident ":" type
```

### 7.4 Capability 定义

```
cap_def ::= "cap" ident (
              ";"
            | "+" ident ";"
            | "=" ident ("+" ident)* ";"
            )
```

## 8. 内置函数

### 8.1 标准库模块

| 模块 | 文件 |
|------|------|
| prelude | `std/prelude.mimi` |
| io | `std/io.mimi` |
| fs | `std/fs.mimi` |
| strings | `std/strings.mimi` |
| collections | `std/collections.mimi` |
| mymath | `std/mymath.mimi` |
| net | `std/net.mimi` |
| maps | `std/maps.mimi` |
| json | `std/json.mimi` |
| time | `std/time.mimi` |
| datetime | `std/datetime.mimi` |
| env | `std/env.mimi` |
| testing | `std/testing.mimi` |
| random | `std/random.mimi` |
| text | `std/text.mimi` |
| result | `std/result.mimi` |
| regex | 内置（builtins） |
| crypto | `std/crypto.mimi` |
| csv | `std/csv.mimi` |
| template | `std/template.mimi` |
| set | `std/set.mimi` |

### 8.2 内置函数

正则表达式（无需导入）：

```
regex_match(text: string, pattern: string) -> bool
regex_find(text: string, pattern: string) -> string
regex_replace(text: string, pattern: string, replacement: string) -> string
```

List/Map 方法（通过隐式 trait 调用）：

```
list.len() -> i32
list.push(value)
list.pop() -> T
list.insert(index: i32, value)
list.remove(index: i32) -> T
map.keys() -> [K]
map.values() -> [V]
```

## 9. 编译模式

### 9.1 Production 模式 (`.mimi`)

- 使用 `{ }` 大括号界定块
- 不需要 `...` 占位符
- 标准语法

### 9.2 Sketch 模式 (`.mms`)

- 使用缩进（4 空格）和 `INDENT`/`DEDENT` 界定块
- 函数/类型头后使用 `:` 而非 `{`
- 允许 `...` 占位符表示未实现代码
- 用于 MimiSpec 渐进式开发

### 9.3 模式关键字对应关系

| Production | Sketch |
|-----------|--------|
| `func name() { ... }` | `func name():` + 缩进体 |
| `type Name { ... }` | `type Name:` + 缩进体 |
| `module Name { ... }` | `module Name:` + 缩进体 |

## 10. 执行模型

### 10.1 三条执行路径

| 命令 | 处理流程 |
|------|----------|
| `mimi run` | 解析 → 类型检查 → 解释器执行 |
| `mimi build` | 解析 → 类型检查 → LLVM codegen |
| `mimi verify` | 解析 → Z3 验证（跳过 comptime 求值） |

### 10.2 Comptime 执行

- `comptime func` 在 `main()` 之前执行，结果缓存
- `quote! { ... }` 仅在解释器 (`mimi run`) 中可用
- `$(expr)` 在 quote 块中插值编译期表达式
- Codegen (`mimi build`) 排除所有 comptime 函数

### 10.3 合约验证

- `requires:` / `ensures:` 在函数体顶部声明
- `--verify-contracts` 编译为运行时断言
- Z3 验证器操作原始 AST，不展开 comptime
- `old(expr)` 在 ensures 中捕获函数入口值

### 10.4 并发模型

- `spawn` 生成异步任务，返回 Future
- `await` 阻塞直到 Future 完成
- `async func` 编译为状态机（poll-based）
- `parasteps` 并行执行块内语句
- 运行时使用协作式调度（单线程 executor）

## 11. 语法约定速查表

### 11.1 语句结束

```
stmt ::= ... ";"                     // 显式分号结束
       | ... (换行于 })              // 右大括号后隐式结束
```

### 11.2 关键字是否保留

所有关键字不可用作标识符（但以 `Ident` token 形式存在的 `i32`, `i64`, `f64`, `bool`, `string` 可用作类型名前缀）。

### 11.3 运算符别名

```
"and"   ≡ "&&"
"or"    ≡ "||"
"not"   ≡ "!"
```
