# Mimi FFI 双栈边界开发路线图

> **版本**：v0.1.0  
> **最后更新**：2026-06-17  
> **依据**：`ffi-ownership-abi.md` v0.2.0

---

## 目标

把 `ffi-ownership-abi.md` 中的设计落地到 Mimi 编译器与运行时。核心目标：

1. 禁止 Mimi 内部对象（`shared`、`&`、`&mut`、记录等）直接穿越 C ABI；
2. 引入边界护照类型（`*T`、`*mut T`、`c_shared T`、`c_borrow T` 等）；
3. 统一解释器与 codegen 两条 FFI 路径；
4. 为 extern 函数生成带 cap 验证和形式化合约的 wrapper；
5. 提供 `libmimi_ffi_rt` 运行时库。

---

## 阶段 0：现状止损（已完成）

### 目标

在解释器路径立即收紧 `call_extern`，禁止已确认不安全的类型直接传 extern。

### 改动的文件

- `mimi/src/interp/call.rs`
- `mimi/src/tests/extern_calls.rs`
- `mimi/src/tests/ffi_safety.rs`（新建）

### 具体任务

1. 重写 `call_extern` 的参数转换逻辑。
2. 只允许以下参数类型：
   - `Value::Int`
   - `Value::Float`
   - `Value::Bool`
   - `Value::String`（按 borrow 传递：`CString::as_ptr()`，调用后释放）
   - `Value::Cap`（暂按 i64 传递，v0.3 引入 CapTable）
3. 明确禁止：
   - `Value::Shared`
   - `Value::LocalShared`
   - `Value::WeakShared`
   - `Value::WeakLocal`
   - `Value::Ref`
   - `Value::RefMut`
   - `Value::List`
   - `Value::Tuple`
   - `Value::Record`
   - `Value::Variant`
   - `Value::Closure`
   - `Value::Actor`
   - `Value::Future`
   - `Value::ArenaRef`
   - `Value::Array`
   - `Value::Slice`
4. 提供清晰的错误信息，引导用户使用未来的护照类型。

### 验收标准

- `cargo test` 在 `mimi/` 目录下全部通过。
- 新增测试：尝试传 `shared`、`&T`、`&mut T`、记录给 extern 函数，必须失败并返回明确错误。
- 现有 extern 调用测试（如 `extern_func_not_found_in_nonexistent_lib`）仍然通过。

---

## 阶段 1：新增 FFI 护照类型到 AST/类型系统（已完成）

### 目标

在语言层面引入边界类型，让用户能显式表达“我要把 Mimi 对象安全地传给 C”。

### 新增 AST 类型

- `Type::RawPtr(Box<Type>)` —— `*T`
- `Type::RawPtrMut(Box<Type>)` —— `*mut T`
- `Type::CShared(Box<Type>)` —— `c_shared T`
- `Type::CBorrow(Box<Type>)` —— `c_borrow T`
- `Type::CBorrowMut(Box<Type>)` —— `c_borrow_mut T`

### 改动的文件

- `mimi/src/ast.rs`
- `mimi/src/lexer.rs`（新关键字/类型名）
- `mimi/src/parser/parse_type.rs`
- `mimi/src/core/mod.rs`（resolve_type）
- `mimi/src/core/infer_expr.rs`
- `mimi/src/codegen/types.rs`
- `mimispec` 解析器（若涉及语法变更）
- `mimispec-vscode` 语法高亮
- `mimispecref` 规范文档

### 具体任务

1. ✅ 在 AST 中新增类型节点。
2. ✅ lexer 识别 `c_shared`、`c_borrow`、`c_borrow_mut`、`*` 作为类型前缀。
3. ✅ parser 支持 `*T`、`*mut T`、`c_shared T` 等语法。
4. ✅ type checker 在 extern 块中强制：参数类型必须是标量、原始指针、护照类型、`cap`、或 `#[repr(C)]` 记录；同时禁止护照类型出现在普通函数、类型别名、记录/枚举、actor、trait/impl 签名中。
5. ✅ codegen 将护照类型映射到 C ABI：
   - `*T` / `*mut T` → `T*`
   - `c_shared T` → `MimiSharedHandle*`（i8*）
   - `c_borrow T` / `c_borrow_mut T` → `T*`

### 验收标准

- ✅ 解析器能正确解析所有新类型。
- ✅ type checker 对非法 extern 参数报错，并拒绝在非 extern 位置使用护照类型。
- ✅ codegen 能生成正确的 LLVM 声明。
- ✅ 新增单元测试覆盖每种类型（`mimi/src/tests/ffi_safety.rs`）。

---

## 阶段 2：自动生成 extern wrapper（已完成）

### 目标

为每个 extern 函数生成 Mimi wrapper，负责边界转换、cap 验证、生命周期保持。

### 改动的文件

- `mimi/src/ffi/`（新建 `contract.rs`、`mod.rs`）
- `mimi/src/interp/mod.rs`
- `mimi/src/interp/call.rs`
- `mimi/src/codegen/mod.rs`

### 具体任务

1. ✅ 新建 `FfiContract` / `FfiArgContract` / `FfiRetContract`，统一描述 extern 函数的参数/返回转换规则。
2. ✅ 解释器路径：用户调用 extern 函数名时，按 `FfiContract` 执行参数 marshalling 与返回转换（wrapper 层）。标量、string borrow、cap、passport 类型均接入 contract；非法类型返回与 Stage 0 兼容的 FFI safety 错误。
3. ✅ codegen 路径：为每个 extern 函数生成 wrapper（原 extern 符号改为 `__mimi_extern_<name>`，用户可见的 `<name>` 变为内部 wrapper）。当前 wrapper 对标量类型直接转发，为后续 passport 类型的边界转换预留了入口。
4. ✅ 在 wrapper 中实现 passport 类型（`c_shared`、`c_borrow` 等）的完整 marshalling。
5. ✅ cap 验证与生命周期保持接入 wrapper。

### 验收标准

- ✅ 用户写 `extern "C" { fn foo(x: i32); }` 并调用 `foo(1)`，解释器与 codegen 均走到 wrapper。
- ✅ wrapper 的行为与直接调用 C 函数一致（标量路径已验证）。
- ✅ 解释器和 codegen 对 passport 类型的 wrapper 行为一致。

---

## 阶段 3：FFI 运行时库 `libmimi_ffi_rt`（已完成）

### 目标

实现 C 侧运行时库，统一处理 shared 句柄、cap 验证、string 转换。

### 具体任务

1. ✅ 用 Rust 实现 FFI 运行时函数（`src/ffi/runtime.rs`）：
   - `mimi_shared_retain/release/get_ptr`
   - `mimi_cap_check/consume`
   - `mimi_string_as_c_str/into_raw/from_raw/free_raw`
2. ✅ 解释器路径通过 `FfiContract` 调用运行时函数。
3. ✅ codegen 在 extern wrapper 中插入运行时调用（retain/release/cap_check/cap_consume）。

### 验收标准

- ✅ C 头文件声明运行时函数（`mimi emit-c-headers` 输出）。
- ✅ cap 验证在 codegen 侧生效（LLVM IR 包含 `mimi_cap_check` 调用）。
- ✅ string 转换通过运行时管理。

---

## 阶段 4：形式化验证边界

### 目标

把 extern 合约接入 Z3/SMT 验证，但明确只验证 wrapper 逻辑。

### 改动的文件

- `mimi/src/verifier.rs`
- `mimi/src/contracts.rs`

### 具体任务

1. 从 extern 声明提取 `requires`/`ensures`。
2. 为 wrapper 生成 SMT-LIB 查询。
3. 验证参数转换、返回值属性、cap 消费、引用计数平衡。
4. 明确报错：C 函数内部不可验证。

### 验收标准

- 简单 extern 合约能通过验证。
- 复杂副作用合约不被验证器接受，提示用户。

---

## 阶段 5：`#[repr(C)]` 泛型单态化与 C 头生成

### 目标

处理泛型类型在 extern 边界上的 C 布局。

### 改动的文件

- `mimi/src/codegen/mod.rs`
- `mimi/src/main.rs`（新增 `--emit-c-headers`）

### 具体任务

1. 为 extern 调用点触发泛型单态化。
2. 为每个实例化生成唯一的 C 结构体名（如 `Pair_i32`）。
3. `mimi build --emit-c-headers` 输出 `.h` 文件。
4. extern 块内禁止未实例化的泛型参数。

### 验收标准

- `Pair<i32>` 在 extern 中使用时生成 `struct Pair_i32`。
- C 头文件可被 C 程序直接 include。

---

## 阶段 6：异步 FFI 回调

### 目标

支持 C 回调 Mimi，安全地投递到 Actor。

### 改动的文件

- `mimi/src/ast.rs`（新增 `extern "C" fn` 类型）
- `mimi/src/parser/parse_type.rs`
- `mimi/src/core/infer_expr.rs`
- `mimi/src/interp/actor.rs`
- `mimi/src/codegen/mod.rs`

### 具体任务

1. 新增 `Type::ExternCFn(args, ret)`。
2. parser 支持 `extern "C" fn(...)` 类型。
3. 允许用户手动编写 trampoline 把 C 回调转发到 Actor。
4. v1.1 自动生成 Actor trampoline。

### 验收标准

- 能注册 C 回调并接收回调事件。
- 回调线程安全地投递到 Actor 消息队列。

---

## 阶段 7：语法显式化与废弃旧行为

### 目标

让危险 FFI 用法无法编译通过。

### 改动的文件

- `mimi/src/core/mod.rs`
- `mimi/src/diagnostic/codes.rs`

### 具体任务

1. extern 块默认只接受护照类型。
2. 旧的危险用法产生编译错误（如 `shared T` 直接作为 extern 参数）。
3. 提供 `unsafe extern` 块作为最后的逃生口。

### 验收标准

- 危险用法被编译器拒绝。
- `unsafe extern` 可以绕过检查，但产生警告/需审计。

---

## 当前优先级

| 优先级 | 阶段 | 理由 |
|---|---|---|---|
| ✅ 已完成 | 阶段 0-3 | 类型系统、wrapper、运行时库均已实现 |
| P3 | 阶段 4 | 形式化验证（Z3 SMT）已有基础实现，需完善 |
| P4 | 阶段 5 + 6 | 泛型单态化与 C 头生成、异步 FFI 回调 |
| P5 | 阶段 7 | 废弃旧行为，关闭逃生口 |

---

## 当前状态

所有核心 FFI 安全基础设施（阶段 0-3）已完成。解释器与 codegen 两条路径均已接入 passport 类型、cap 验证、合约检查。`mimi verify` 命令使用 Z3 SMT 求解器验证 requires/ensures 合约。

---

> ⏳ **历史归档**：本文档已整合至 `mimi/docs/ffi-ownership-abi.md`（FFI 设计权威文档，§8 实施路线图）。保留以供历史参考。
