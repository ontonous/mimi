# Mimi 所有权模型与 C ABI 的双栈边界设计

> **状态**：设计草案  
> **版本**：v0.2.0  
> **最后更新**：2026-06-17  
> **相关文档**：`ffi-glue.md`、`design-decisions.md`、`readme/10-ffi.md`

---

## 1. 问题陈述

Mimi 的所有权模型与 C ABI 存在根本性的运行时语义冲突：

| Mimi 侧 | C ABI 侧 |
|---|---|
| `shared T` 是带引用计数和锁的容器 | C 只认识 `T*` 或 `void*` |
| `&T` / `&mut T` 受借用检查器约束 | C 指针可被任意复制、长期保存 |
| `cap` 是线性能力，不可复制伪造 | C 侧只是整数，无法验证 |
| `string` 是 Mimi 堆对象 | C 侧是 `*const c_char` 或 `*mut c_char` |
| 形式化合约 `requires`/`ensures` | C 函数没有合约概念 |

当前实现（解释器路径的 `call_extern` 与代码生成路径的 `register_extern_block`）把 Mimi 内部对象“砸平”成 C 指针，导致：

1. `shared T` 被传为指向 Mimi 内部 `Value` 的裸指针，引用计数语义丢失；
2. `&T` / `&mut T` 在 C 侧无法保证独占性或生命周期；
3. `cap` 越过 ABI 后变成不可验证的整数；
4. 解释器与代码生成对 `shared`/借用的处理不一致。

本设计采用**双栈策略**：明确区分 Mimi 应用层运行时与 C ABI 外部边界，在两者之间建立一个自动的、可验证的**边界翻译层**。

---

## 2. 双栈架构

```text
┌─────────────────────────────────────┐
│         Mimi 应用层（Mimi 堆）        │
│  shared T, &T, &mut T, cap, string   │
│  引用计数, 借用检查, 线性能力, 合约    │
│  requires / ensures / math           │
└──────────────┬──────────────────────┘
               │
               │  边界翻译层（自动生成 wrapper）
               │  - 类型转换
               │  - 生命周期保持
               │  - cap 验证
               │  - 形式化合约 wrapper
               ▼
┌─────────────────────────────────────┐
│         C ABI 外部边界               │
│  *T, *mut T, #[repr(C)] 类型         │
│  裸指针, 手动内存布局                │
│  无 Mimi 所有权/借用语义             │
└─────────────────────────────────────┘
```

核心原则：

1. **Mimi 安全对象不直接出境**。`shared T`、`&T`、`&mut T`、`string` 等类型不能作为 extern 函数参数直接出现。
2. **C 侧只接收它能理解的形态**：标量、原始指针、`#[repr(C)]` 记录、不透明句柄。
3. **边界转换由编译器自动生成的 wrapper 完成**，不是用户手写 `unsafe`。
4. **wrapper 是形式化验证的边界**：Z3/SMT 在 wrapper 上验证 Mimi 不变量，而不需要验证 C 函数内部。

---

## 3. 边界类型护照

Mimi 类型出境前必须转换成对应的“护照类型”。

### 3.1 类型映射表

| Mimi 类型 | 出境护照类型 | C ABI 看到 | 所有权语义 |
|---|---|---|---|
| `shared T` | `c_shared T` | `MimiSharedHandle*` | C 可 retain/release，参与引用计数 |
| `&T` | `c_borrow T` | `*const T` | 与 Mimi 借用生命周期绑定，不可逃逸 |
| `&mut T` | `c_borrow_mut T` | `*mut T` | 独占借用，调用期间锁定 |
| `string`（borrow） | `string.as_c_str()` | `*const c_char` | Mimi 保留所有权，C 不释放 |
| `string`（transfer） | `string.into_raw()` | `*mut c_char` | 所有权转移给 C，C 负责释放 |
| `cap` | `MimiCap` | `i64` | 不透明句柄，可 check/consume |
| 记录/枚举 | `#[repr(C)] Record` | 按 C 布局 | 值拷贝或按指针传递 |
| `*T` / `*mut T` | `*T` / `*mut T` | C 原始指针 | 无 Mimi 所有权，需用户显式管理 |

### 3.2 `c_shared T`

`c_shared T` 是 `shared T` 出境时的引用计数兼容视图。

```mimi
let data: shared Buffer = shared Buffer { ... };

extern "C" {
    fn process_buffer(buf: c_shared Buffer) -> i32;
}

// wrapper 内部自动完成：
// 1. retain shared
// 2. 生成 MimiSharedHandle*
// 3. 调用 C 函数
// 4. release shared
let rc = process_buffer(data);
```

C 侧看到的头文件：

```c
typedef struct MimiShared MimiShared;
MimiShared* mimi_shared_retain(MimiShared*);
void mimi_shared_release(MimiShared*);

int32_t process_buffer(MimiShared* buf);
```

### 3.3 `c_borrow T` / `c_borrow_mut T`

借用出境时，编译器根据被借用对象的来源选择**静态证明**或**运行时跟踪**策略。

#### 3.3.1 静态借用：栈上局部变量

当被借用的是栈上局部变量时，编译器可以静态证明 C 函数返回后指针不再有效。无需运行时代价。

```mimi
let mut buf: Buffer = Buffer { ... };

extern "C" {
    fn read_into_buffer(buf: c_borrow_mut Buffer) -> i32;
}

let n = read_into_buffer(&mut buf);
```

codegen 生成：

```llvm
; alloca + noalias 标注
%buf = alloca %Buffer
%ptr = bitcast %Buffer* %buf to i8*
call void @read_into_buffer(i8* noalias %ptr)
```

- `noalias` 告知 LLVM C 函数不会保存指针；
- 栈帧本身保证对象在调用期间存活；
- **零运行时开销**。

#### 3.3.2 动态借用：`shared T` 或堆对象

当被借用的是 `shared T` 或动态分配对象时，需要在调用期间防止对象被释放或修改。此时引入轻量级运行时标记：

```mimi
let data: shared Buffer = shared Buffer { ... };

extern "C" {
    fn inspect_buffer(buf: c_borrow Buffer) -> i32;
}

let n = inspect_buffer(&data);
```

wrapper 逻辑：

```rust
// 伪代码
fn __mimi_wrapper_inspect_buffer(shared: &shared Buffer) -> i32 {
    // 获取读锁/借用标记
    let guard = shared.borrow();
    let ptr = guard.as_ptr();
    let result = unsafe { __ffi_inspect_buffer(ptr) };
    // guard drop，释放借用标记
    result
}
```

性能说明：

- 栈变量借用：**零运行时开销**；
- `shared T` 借用：一次锁/原子操作获取借用标记，调用结束后释放；
- 对高频 FFI（如每帧调用图形 API），建议优先使用栈变量借用或 `c_shared T` 长期持有，避免频繁短周期借用。

#### 3.3.3 C 侧保存指针的后果

`c_borrow` / `c_borrow_mut` 传递给 C 的指针**不得在 C 返回后被保存或使用**。一旦 C 保存了指针，后续访问属于 C 侧的未定义行为。

如果 C 需要长期访问对象，应使用 `c_shared T`。

### 3.4 性能模型

不同护照类型的运行时开销：

| 护照类型 | 栈变量来源 | shared/堆来源 | 备注 |
|---|---|---|---|
| `c_borrow T` | 零开销 | 一次 borrow 标记 | 静态证明 vs 运行时跟踪 |
| `c_borrow_mut T` | 零开销 | 一次 borrow 标记 | 静态证明 vs 运行时跟踪 |
| `c_shared T` | N/A | 引用计数 +1/-1 | 可长期持有 |
| `string.as_c_str()` | 零开销 | 零开销 | 仅确保 null terminator |
| `string.into_raw()` | 转移所有权 | 转移所有权 | C 必须释放 |
| `*T` / `*mut T` | 零开销 | 零开销 | 无 Mimi 保护 |

设计建议：

- 高频短周期 FFI（如图形 API 每帧调用）优先使用栈变量 `c_borrow` 或 `c_shared` 长期持有；
- 避免频繁创建/销毁 `c_shared` 句柄；
- `c_borrow` 来自 `shared T` 时，借用标记的成本是一次锁/原子操作，通常可忽略，但在极高频场景下应测量。

### 3.5 `string` 边界

`string` 默认不转移所有权：

```mimi
extern "C" {
    fn strlen(s: string) -> i32;  // 实际传 *const c_char
}

let n = strlen("hello".into());  // OK，Mimi 保留所有权
```

如果需要转移所有权：

```mimi
extern "C" {
    fn save_string(s: raw string) -> i32;  // 传 *mut c_char，C 负责释放
}

let n = save_string("hello".into().into_raw());
```

### 3.5 `*T` / `*mut T`

原始指针类型是 Mimi 的“已离开安全区”类型，用户显式管理：

```mimi
let ptr: *mut u8 = malloc(1024);
// ... 使用 ptr ...
free(ptr);
```

`shared T.as_ptr()`、`&T.as_ptr()`、`&mut T.as_mut_ptr()` 也返回原始指针，但这是**显式降级**，不是隐式转换。

---

## 4. extern 块语法扩展

### 4.1 默认约束

`extern "C"` 块中的参数类型必须属于以下类别：

1. 标量：`i32`、`i64`、`f64`、`bool`
2. 原始指针：`*T`、`*mut T`
3. 护照类型：`c_shared T`、`c_borrow T`、`c_borrow_mut T`、`raw string`
4. `#[repr(C)]` 记录或枚举
5. `cap`（映射为 `MimiCap`）

否则编译器报错。

### 4.2 带合约的 extern 声明

extern 合约分为两类：

1. **纯逻辑合约**：对参数和返回值的约束，可被 Z3/SMT 验证。
2. **效应合约**：对 C 函数副作用的声明，由 `cap` 承载，不进入形式化验证，但进入能力检查。

```mimi
extern "C" {
    requires: path.len > 0
    ensures: result.is_ok() ==> result.value.len >= 0
    fn read_file(path: string, cap: FileReadCap) -> Result<string, string>;
}
```

- `requires` / `ensures` 约束 wrapper 能看到的逻辑属性；
- `cap: FileReadCap` 声明该函数具有“读取文件”的效应。

编译器自动生成的 wrapper：

```mimi
fn __mimi_wrapper_read_file(path: string, cap: FileReadCap) -> Result<string, string> {
    requires: path.len > 0
    ensures: result.is_ok() ==> result.value.len >= 0

    // cap 验证
    assert cap.check("FileReadCap");
    cap.consume("FileReadCap");

    // 边界转换
    let c_path = path.as_c_str();
    let c_cap = cap.as_handle();

    // 调用 C
    let c_result = __ffi_read_file(c_path, c_cap);

    // 结果转换
    c_result.into_result_string()
}
```

用户调用 `read_file(...)` 时，实际调用的是 wrapper。

#### 4.2.1 可验证与不可验证的边界

Z3/SMT 验证的是 wrapper 的**逻辑转换**，而非 C 函数内部行为：

- ✅ 可验证：参数类型转换保持值不变、返回值的逻辑属性、`cap` 是否正确消费、引用计数是否平衡。
- ❌ 不可验证：C 函数是否遵守契约、C 函数是否有未声明的副作用、C 函数是否访问了不该访问的内存。

因此，extern 合约需要用户明确信任 C 函数的行为。`cap` 系统把这种信任显式化：你持有 `FileReadCap` 才允许调用 `read_file`，但 `read_file` 内部具体做了什么，Mimi 无法证明。

文档应明确告知用户：

> “Mimi 验证的是你写的包装器的逻辑，而非 C 函数内部。C 函数的正确性需要单独保证。”

---

## 5. FFI 运行时库 `libmimi_ffi_rt`

C 侧需要链接一个极小的 Mimi FFI 运行时库，提供统一的边界原语。

### 5.1 C 头文件

```c
#ifndef MIMI_FFI_RT_H
#define MIMI_FFI_RT_H

#include <stdint.h>
#include <stdbool.h>

// 共享句柄（不透明）
typedef struct MimiShared MimiShared;
MimiShared* mimi_shared_retain(MimiShared* handle);
void mimi_shared_release(MimiShared* handle);
void* mimi_shared_get_ptr(MimiShared* handle);

// 线性能力句柄
typedef int64_t MimiCap;
bool mimi_cap_check(MimiCap cap, const char* name);
bool mimi_cap_consume(MimiCap cap, const char* name);

// string 辅助
const char* mimi_string_as_c_str(void* mimi_string);
char* mimi_string_into_raw(void* mimi_string);
void* mimi_string_from_raw(char* c_str);
void mimi_string_free_raw(char* c_str);

#endif
```

### 5.2 解释器路径使用运行时库

解释器不再自己把 `Value::Shared` 转成裸指针，而是调用 `libmimi_ffi_rt` 的对应函数：

```rust
Value::Shared(arc) => {
    let handle = mimi_ffi_rt::shared_create(arc);
    c_args.push(handle as i64);
}
```

### 5.3 代码生成路径使用运行时库

codegen 在 extern 调用前后插入 `mimi_shared_retain` / `mimi_shared_release` 等调用。

---

## 6. cap 的跨边界认证

当前 `cap` 越过 FFI 后变成普通整数，C 侧无法验证。新设计中：

1. Mimi 运行时维护全局 `CapTable`；
2. 每个 cap 实例分配唯一 `i64` id；
3. 传过 FFI 时只传 id；
4. C 侧调用 `mimi_cap_check(id, "CapName")` 验证；
5. C 侧调用 `mimi_cap_consume(id, "CapName")` 消费。

```mimi
cap FileReadCap;

extern "C" {
    fn read_file(path: string, cap: FileReadCap) -> string;
}
```

生成的 C 函数实际签名：

```c
const char* read_file(const char* path, MimiCap cap);
```

C 实现：

```c
const char* read_file(const char* path, MimiCap cap) {
    if (!mimi_cap_check(cap, "FileReadCap")) {
        return NULL; // 或返回错误
    }
    // ... 执行读取 ...
    mimi_cap_consume(cap, "FileReadCap"); // 如果 Mimi 侧是 move 语义
    return result;
}
```

Mimi 侧支持两种 cap 传递模式：

- `cap: CapName`：move 语义，调用后消费；
- `cap &fh: CapName`：borrow 语义，调用后归还。

---

## 7. 解释器与 codegen 统一

当前两条路径对 `shared` / 借用的处理不一致。新设计要求它们共享同一张 FFI 契约表。

### 7.1 共享契约表

每个 extern 函数在编译期生成一个 `FfiContract`：

```rust
struct FfiContract {
    name: String,
    param_kinds: Vec<FfiParamKind>,
    ret_kind: FfiRetKind,
    cap_consumptions: Vec<String>,
}

enum FfiParamKind {
    Scalar,
    StringBorrow,
    StringTransfer,
    SharedPointer,
    BorrowPointer { mutable: bool },
    RawPointer { mutable: bool },
    Cap { name: String, mode: CapMode },
    ReprCRecord(String),
}
```

解释器和 codegen 都根据这张表执行 marshalling。

### 7.2 测试矩阵

每个 FFI 特性需要在解释器和 codegen 两条路径上都有测试：

| 测试场景 | 解释器 | codegen |
|---|---|---|
| `i32` / `i64` / `f64` / `bool` | ✅ | ✅ |
| `string.as_c_str()` | ✅ | ✅ |
| `string.into_raw()` | ✅ | ✅ |
| `shared T` → `c_shared T` | ✅ | ✅ |
| `c_shared T` retain/release | ✅ | ✅ |
| `&T` → `c_borrow T` | ✅ | ✅ |
| `&mut T` → `c_borrow_mut T` | ✅ | ✅ |
| `cap` check/consume | ✅ | ✅ |
| `#[repr(C)]` 记录 | ✅ | ✅ |
| `requires`/`ensures` wrapper | ✅ | ✅ |

---

## 8. 实施路线图

### 阶段 0：现状止损

- 收紧 `call_extern` 允许参数类型，禁止 `shared`、`&T`、`&mut T`、记录、列表直接传 extern；
- 明确 `string` 默认 borrow，`into_raw()` 显式转移；
- `cap` 传 FFI 时改为不透明句柄。

### 阶段 1：新增边界类型

- 在 AST/类型系统中新增：`*T`、`*mut T`、`c_shared T`、`c_borrow T`、`c_borrow_mut T`、`raw string`；
- parser 支持新类型；
- type checker 在 extern 块中强制使用护照类型。

### 阶段 2：自动生成 wrapper

- 为每个 extern 函数生成 Mimi wrapper；
- wrapper 负责参数/结果转换、cap 验证、生命周期保持；
- 用户调用实际走到 wrapper。

### 阶段 3：FFI 运行时库

- 实现 `libmimi_ffi_rt`；
- 解释器和 codegen 都链接并调用它；
- codegen 生成 `shared` / 借用的运行时结构。

### 阶段 4：形式化验证边界

- 把 extern 函数的 `requires`/`ensures` 生成到 wrapper；
- Z3/SMT 在 wrapper 上验证边界转换的正确性；
- 明确区分可验证的逻辑合约与不可验证的效应合约；
- 提供 `--verify-ffi` 模式。

### 阶段 5：`#[repr(C)]` 泛型与 C 头生成

- 为 extern 块中的泛型类型触发单态化；
- `mimi build --emit-c-headers` 输出每个实例化的 C 布局；
- extern 块内禁止未实例化的泛型参数。

### 阶段 6：异步 FFI 回调

- 支持 `extern "C" fn` 函数指针类型；
- 手动 trampoline 把 C 回调转发到 Actor；
- v1.1 自动生成 Actor trampoline。

### 阶段 7：语法显式化与废弃旧行为

- `extern "C"` 默认只接受护照类型；
- 旧的危险 FFI 用法产生编译错误；
- 提供 `unsafe extern` 块作为最后的逃生口。

---

## 9. 与现有文档的关系

| 文档 | 关系 |
|---|---|
| `readme/10-ffi.md` | 需要同步更新 extern 语法、类型映射、cap 传递模式 |
| `ffi-glue.md` | 需要补充双栈模型、wrapper 生成、`libmimi_ffi_rt` 设计 |
| `design-decisions.md` | 需要把本设计作为一项核心设计决策纳入 |
| `readme/03-memory.md` | 需要补充 `c_shared` / `c_borrow` 等边界类型与 shared/借用的关系 |

---

## 10. 关键设计决策记录

### 决策 1：是否让 C 直接操作 Mimi 引用计数？

**结论：不暴露内部结构，只提供不透明句柄 + retain/release 回调。**

理由：暴露 `Arc`/`Rc` 内部会绑定 ABI 到 Mimi 实现细节，不透明句柄更稳定。

### 决策 2：`cap` 在 C 侧是否可消费？

**结论：默认只读检查；消费必须显式声明。**

```mimi
fn read_file(path: string, cap: FileReadCap) -> string;      // move
fn check_file(path: string, cap &fh: FileReadCap) -> bool;   // borrow
```

### 决策 3：是否保留当前“简单 FFI”？

**结论：逐步废弃，提供迁移 shim。**

阶段 0 发出警告，阶段 5 变为错误。需要 `unsafe extern` 作为临时逃生口。

### 决策 4：wrapper 由谁生成？

**结论：编译器自动生成，对用户透明。**

用户写 `extern "C" { fn foo(...); }`，编译器生成 `__mimi_wrapper_foo`，用户调用的是 wrapper。

---

## 11. 边界场景决策

### 11.1 `c_shared T` 在 C 侧被 long-term retain 时，Mimi 侧如何感知？

**结论：不引入跨语言 GC。**

`c_shared T` 的语义是“C 持有强引用，阻止 Mimi 释放该对象”。它本质上是 Mimi 引用计数系统的一个外部根。只要引用计数 > 0，对象就不释放。

可能的情况：

- C 长期持有句柄，Mimi 侧所有引用已消失 → 对象变成只有 C 持有的孤岛；
- 这是**逻辑泄漏**，不是内存不安全；
- 用户可通过 `mimi_shared_is_unique(handle)` 查询是否只剩 C 持有；
- Debug 模式下，运行时可扫描 `c_shared` 句柄表，报告“某句柄仍被 C 持有”的警告，帮助发现泄漏。

循环引用：Mimi 内部已有 `weak`。C 持有的句柄若是强引用，循环引用由 C 侧程序员避免。

### 11.2 `c_borrow_mut` 跨越 FFI 后，C 保存指针并在借用结束后使用，能否检测？

**结论：无法在运行时零成本检测。定义为 C 侧未定义行为。**

一旦指针传递给 C，C 可以将其复制到任意位置。Mimi 运行时无法追踪 C 侧所有指针副本。

策略：

- 文档明确标注：`c_borrow` / `c_borrow_mut` 的指针不得在 C 返回后使用；
- Debug 模式可提供昂贵的内存页保护（如写时复制），但不推荐在 v1.0 实现；
- 静态分析 C 代码超出 Mimi 项目范围；
- 若 C 需要长期访问，应使用 `c_shared T`。

### 11.3 `#[repr(C)]` 记录的泛型如何处理？

**结论：必须单态化。**

Mimi 的泛型模型是编译时单态化。`#[repr(C)]` 泛型记录用于 extern 时，必须为每个具体类型生成唯一的 C 布局。

示例：

```mimi
#[repr(C)]
type Pair<T> {
    first: T,
    second: T,
}

extern "C" {
    fn sum_pair(p: Pair<i32>) -> i32;
}
```

codegen 触发单态化，生成 C 头：

```c
struct Pair_i32 {
    int32_t first;
    int32_t second;
};

int32_t sum_pair(struct Pair_i32 p);
```

限制：

- extern 块内不允许未实例化的泛型参数；
- 泛型 extern 函数由调用点触发单态化；
- `mimi build --emit-c-headers` 输出所有实例化的 C 类型定义。

### 11.4 异步 FFI 调用（C 回调 Mimi）如何设计？

**结论：引入 `extern "C" fn` 函数指针类型，回调投递到 Actor 消息队列。**

示例：

```mimi
extern "C" {
    fn register_callback(cb: extern "C" fn(i32), ctx: *mut unit);
}
```

安全模型：

- C 回调可能发生在任意线程，不能直接执行 Mimi 代码；
- 编译器自动生成 trampoline，把 C 回调转换为向指定 Actor 发送消息；
- 用户注册时绑定一个 Actor 句柄：

```mimi
actor MyActor {
    func on_data(val: i32) { ... }
}

register_callback(MyActor.on_data_callback());
```

v1.0 最小实现：

- 支持 `extern "C" fn` 类型；
- 允许用户手动编写 trampoline 把 C 回调转发到 Actor；
- 自动生成 Actor trampoline 可作为 v1.1 特性。

---

*本设计为 Mimi FFI 的演进方向。具体实现应以阶段 0 为起点，逐步推进。*
