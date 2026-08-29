# Changelog

## [Unreleased] — 0.1.10-dev

### 0.40.1.7 — F-003：E0723 门禁对 record 包裹普通 List 的过度 fail-closed 收窄

0.40.1.6（F-002）把 E0723 门禁延伸到 record 字段递归判定，但引入**过度拒绝（over-reach）**：
record 字段路径用独立的 `nested_list_rt_owns` 把 `List<i32>` 当成所有权空洞 fail-closed，
而同一门禁的顶层/表层路径（`list_elem_owns_unclaimed_heap` → `nested_inner_owns_unclaimed`）
对裸 `List<i32>` 返回一直放行。两者不一致 —— 返回 `Wrap { items: List<i32> }` 被 native 拒绝，
但返回裸 `[1,2,3]` 却正常。

根因（`src/codegen/resolved/mod.rs`）：`owns_unclaimed_heap_rt` 的 `List` 臂调用了与表层策略
**不一致**的 `nested_list_rt_owns`（`Primitive`/`Tuple`/`builtin:type:*` 元素一律判为空洞），
而顶层策略对单层级 `List<X>`（X 为 scalar / `string` / tuple / record / 泛型参数）是「外层数据
数组被认领、单层安全」、仅 `List<List<X>>` / `List<Set>` / `List<Map>` 才是真空洞。

修复：删除 `nested_list_rt_owns`，改为在 record 字段路径复用与顶层**完全一致**的判定——
新增 `list_elem_owns_unclaimed_rt` / `nested_inner_owns_rt`（ResolvedType 镜像），使
「包裹普通 `List<X>` 的 record 返回」与「裸 `List<X>` 返回」等价放行。同时修正 E0723 诊断文案
（旧文案只提 "Set/Map payload"，实际触发还含嵌套非 string `List`）。

不变量类别: L1（双后端等价，移除误导性 fail-closed 分歧）/ L2（类型系统健全性）
测试: `tests/real_world/regression_record_list_field_return_native_allowed.mimi`、
`src/tests/audit_fix_codegen_expr1.rs::audit_expr1_record_list_field_return_native_allowed`
（负向仍由 `audit_expr1_native_heap_aggregate_record_field_return_fails_closed_e0723` 覆盖：
`List<List<i32>>` 字段 record 返回依旧 E0723 fail-closed）

### 0.40.1.8 — F-004：闭包捕获堆集合逃逸 native 静默 UAF → fail-closed（E0723）

所有权移交第 4 边界（§1.3）漏网：闭包捕获堆值（`List`/含 heap record）逃逸后 native
静默 UAF（VM 读到原值、native 读脏数据/悬垂指针，编译通过、L3/L1 最危险静默形态）。
根因：`allocate_closure_env`（lambda.rs）按值复制捕获变量进 env struct，enclosing scope
退出 `free_heap_allocs` 释放数据数组 → env 悬垂指针；direct-return 有「逃逸置 NULL」但
闭包捕获无等价认领。

0.40.1 采用 **fail-closed**（返回捕获堆值的闭包报 `E0723`），与 F-001/F-003 哲学一致；
正确 move 所有权转移属 A2 glue（0.40.3），不在本轮引入新 deep-copy/claim 启发式。

修复要点（双后端返回站点均用**精确 Mimi 类型**判定，弃用 LLVM 布局启发式）：

- **resolved native emitter**：`ResolvedStmtKind::Return` 臂对返回 `ResolvedLambda` 的每个
  `captures` local 取 `body.locals[cap].ty`（ResolvedTypeId），经
  `resolved_type_owns_heap_collection` 递归判定是否 transitively 拥有堆集合
  （`builtin:type:List` / `Set` / `Map`，并递归 `Option`/`Result`/`Tuple`/`Newtype`/
  `Array`/`Slice`/`CBuffer`）。标量/string/闭包/record 不命中（record-含-list 走 F-005
  E0700）。
- **legacy native emitter**：`claim_string_return_value`（func.rs）调用
  `returned_closure_captures_heap`（lambda.rs），对返回 lambda 的 free var 取
  `self.var_type_names[name]` 的 Mimi 类型名串，经 `mime_type_name_owns_heap_collection`
  （`contains("List<"/"Set<"/"Map<")`）判定。
- **关键根因修复**：`compile_subset`（resolved/mod.rs:791 `Err(e)` 臂）原本**吞掉所有
  per-function 错误**并静默回退 legacy，导致 resolved 路径的 E0723 被吞 → 重新 emit 坏 IR。
  改为 `if e.code() == "E0723" { return Err(e); }` 让该错误上抛、被 `compile.rs` 的 E0723
  升级逻辑捕获为硬错误，**不再被 fallback 绕过**。

诊断文案同步收窄：`E0723` 现仅针对 `List`/`Set`/`Map`（及 transitively 持有的容器），不再
误提 string/record（二者属 F-005 E0700 边界，非 F-004）。

不变量类别: L1（双后端等价，移除误导性的静默 UAF 分歧）/ L3（内存安全，消除逃逸闭包悬垂读）
测试: `src/tests/audit_fix_codegen_expr1.rs::audit_expr1_closure_capture_heap_return_fails_closed_e0723`
（负向回归：返回捕获标量/字面量 string 的闭包仍双后端 MATCH，不误伤）

### 0.40.1.9 — F-005：闭包返回 record/nominal 的 native ABI 误降级 i64 → 正确 struct ABI（双后端等价）

所有权移交盲区一 §1.3 第 4 边界的独立衍生项（与 F-004 闭包捕获堆值 UAF 无关）：
`func make() -> func() -> P { ... }` 后 `main` 调 `let f = make(); f()` —— native 把闭包返回值
静默降级为 i64，致 `f().a` 字段访问在 native 报 `E0700 field access requires a struct or actor
type, got "i64"`，而 VM 正常返回 `3`（L1 分歧，VM 接受 / native 拒绝的「一方更好」违例，内核卡 §5
禁止放行）。

根因（`src/codegen/func.rs`）：`let name = func_name(args)` 的返回类型登记
`match ret_ty.unlocated()` 只处理 `Type::ImplTrait` 与 `Type::Name`，`func() -> P` 属 `Type::Func`，
落入 `_ => {}` 被**静默丢弃** —— `f` 的 `var_types` 条目始终空缺。闭包调用点
（`src/codegen/expr/call/simple.rs:6361`）经 `closure_return_llvm_type(var_types["f"])` 取不到返回
LLVM 类型 → `emit_closure_call`（`src/codegen/expr/call/closure.rs:91`）把间接调用默认成 i64。
标量/string 闭包仅靠值巧合过关（小整数塞得进 i64、string 结构同槽），tuple/Option/Result 闭包则被
静默截断到 i64（此前无编译错误、仅值错）。

修复：在 `func.rs` 的 `let`-绑定返回类型登记补 `Type::Func` / `Type::ExternFunc` 分支，把 `f` 的
`func() -> R` 类型写入 `var_types` / `var_type_names`，使调用点经既有单源 `closure_return_llvm_type`
推导出正确的返回 LLVM 类型（record→struct、tuple/Option/Result→对应 struct、scalar/string→原类型）。
非新增启发式 / 类型白名单 / 形状枚举（仅补全既有 `match` 的穷尽性，复用既有单源类型 lowering），
不触 0.40.1 红线（deep-copy/claim 冻结）。

不变量类别: L1（双后端等价，消除「一方更好」分歧；closure 返回 record/tuple/Option/Result 现 VM≡native）
测试: `src/tests/dual_backend.rs::dual_closure_returns_record`、
`dual_closure_returns_record_captured`、`dual_closure_returns_record_param`、
`dual_closure_returns_composite`（均双后端 run+build+exec MATCH）

### 0.40.1.10 — F-006：inline 闭包调用作为列表字面量元素时 native 元素类型误降级 i64（L1 分歧，E0700）

F-005 (0.40.1.9) 的同胞缺陷：同一「闭包返回类型未传播到使用 `infer_object_type` 的上下文」家族。
`func mk(a,b) -> func() -> P { ... }` 后 `let f = mk(1,2); let g = mk(3,4); let ps = [f(), g()];`
native 在 `ps[0].a` 报 `E0700 field access requires a struct or actor type, got "i64"`，VM 正常返回 `5`
（L1 分歧，VM 接受 / native 拒绝的「一方更好」违例）。

根因（`src/codegen/expr.rs:859` `infer_object_type` 的 `Expr::Call(Ident(name), args)` 臂）：该臂只处理
**具名函数调用**（`func_defs` / `infer_call_return_type_name`）、内建与 `Some`/`None`/`Ok`/`Err` 构造器，
**从不查 `var_types[name]` 的闭包局部变量**。对 `f()`（局部闭包变量调用）它落到末尾 `name.clone()`
返回变量名 `"f"` → 列表字面量元素类型推断为 `i64`，`ps: List<i64>`，`ps[0]` 读成 `i64` → `.a` E0700。
（注：`let p = f(); [p, ...]` 因 `p` 是 `let`-绑定、其 `var_types` 已由 F-005 填为 `P` 而正常；命名函数
inline 调用 `[mk(1), mk(2)]` 走 `func_defs` 正常 —— 唯独「inline 闭包调用」中招，精确定位此臂。）

修复：在该臂**最前**加查 `self.var_types.get(name)`，`Type::Func(_, ret)` / `Type::ExternFunc(_, ret)`
直接 `return crate::core::fmt_type(ret)`（闭包返回类型名）。函数只在 `func_defs`、从不在 `var_types`，
故对具名函数调用为零副作用；纯复用既有 `var_types` 单源，非新增启发式 / 类型白名单 / 形状枚举，
不触 0.40.1 红线（deep-copy/claim 冻结）。分歧直接消除（非 fail-closed，无新增 E 码）。

不变量类别: L1（双后端等价）
测试: `src/tests/dual_backend.rs::dual_closure_in_list_literal_record`、
`dual_closure_in_list_literal_tuple`、`dual_closure_in_list_literal_option`、
`dual_closure_in_list_literal_scalar`（均双后端 run+build+exec MATCH）

### 0.40.1.11 — F-007：元组字面量含 record/tuple 元素时 native 元素类型误判 `any`（L1 分歧，E0707）

F-005 (0.40.1.9) / F-006 (0.40.1.10) 的同胞缺陷：同一「容器元素类型未登记到
`var_type_names`」家族在 **元组字面量** 上的变体。
`type P { a: i32, b: i32 }` 后 `let t = (P { a: 1, b: 2 }, P { a: 3, b: 4 });`
native 在 `t.0.a` 报 `E0707 type 'any' is not a struct`，VM 正常返回 `5`
（L1 分歧，VM 接受 / native 拒绝的「一方更好」违例）。

根因（`src/codegen/func.rs` let 绑定类型名登记 else-if 链，约 line 2705 一带）：
该链有 `List`/`Index`/`Slice`/`Record`/`Set`/`Lambda`/`String` 分支，但**没有
`Expr::Tuple` 分支**。对推断型 `let t = (...)`（pattern `ty` 为 `None`，不进
`ty` 分支的 `get_full_type_name` 路径），`var_type_names["t"]` 始终空缺。
`infer_object_type` 的 `Expr::TupleIndex` 臂（expr.rs:1129）会解析 `"(A, B)"` 取
元素类型——但前提是元组变量的类型名已登记；否则 `obj_ty` 为空、落入
`"any".to_string()`（expr.rs:1172），`t.0` 认成 `any` → `t.0.a` E0707。

修复：在 else-if 链补 `Expr::Tuple(_)` 分支，登记
`var_type_names[name] = self.infer_object_type(init, vars)`（即 `"(A, B,…)"`，
`infer_object_type` 的 `Expr::Tuple` 臂已正确渲染，纯复用、无新启发式 /
类型白名单 / 形状枚举，不触 0.40.1 红线）。分歧直接消除（非 fail-closed，无新增 E 码）。
`let t = (f(), g())`（inline 闭包调用作元组元素，F-006 同族）一并修复。

不变量类别: L1（双后端等价）
测试: `src/tests/dual_backend.rs::dual_tuple_of_record_literals`、
`dual_tuple_of_closure_returned_records`（均双后端 run+build+exec MATCH）

> 本轮同扫发现 **F-008（待下一轮 MODE-2）**：**推导式（comprehension）产出
> record/tuple 元素** 在 native 仍 `E0707 must produce i64-compatible value`
> （`src/codegen/expr/record.rs:952` `emit_comprehension_store`），而 VM 接受
> （`[P { a: x, b: x } for x in xs]` / `[(x, x) for x in xs]` 双后端分歧；标量
> 推导式正常）。根因：`emit_comprehension_loop`/`allocate_comprehension_output`
> 把输出写死成 `i64` 槽（`list_len * 8` 字节），不支持 struct 元素堆打包
> （list 字面量经 `coerce_to_list_storage` 已支持）。非一行补丁，需让推导式
> 产出 `List<P>`（复用 `coerce_to_list_storage` 堆打包）；或若过大则降级为
> 带 E 码的 fail-closed（两端一致拒绝）。排入队列，下一轮 MODE-2 处理。

### Pain-point 修复（PAIN_LOG P1–P3）

从 `docs/PAIN_LOG.md` 抽取的真实痛点，本轮在 0.1.10-dev 评估并修复：

- **P1 (E0247) 整数字面量默认 i32 + 双向强制**：已在前序提交解决（`infer_expr.rs`
  字面量默认 i32、对 i64/i32 字段双向弹性强制）。本次仅补充回归验证：
  `100` 同时可赋 `i64` 与 `i32` 字段，VM 与 codegen 双后端一致。无代码改动。
- **P2 (E0402) `state Fault` 与系统 Fault sink 冲突诊断**：修正建议语法——
  旧提示错误地写成 `fault <ErrorType> { ... }`，实际语法为
  `fault <ErrorType>;`（`src/core/checker/items.rs`）。
- **P3 (E0221) `state.method(args)` 方法式 flow 转移调用**：`t.start()` 此前报错
  “type 'Idle' has no method 'start'”。已补完 desugar 为 `Flow::method(state, args)`：
  - 类型检查：`src/core/infer/call/method.rs`（`flow_transition_for_state_method`）。
  - Resolved 降级（单一事实源）：`src/core/ir/lower.rs` 新增
    `lower_state_method_transition_call`，接收者按 `.callee.inner` 编目、事件参数保留
    原 `.argument.i` 角色，规避 `TOOL-RESOLUTION-001`。
  - LLVM codegen：`src/codegen/expr/call/method.rs`（`flow_for_state`）。
  - Bytecode VM：`src/interp/bytecode/compiler.rs`（flow-state 编目 +
    转移键预注册，使 `main` 在 Pass 4 之前即可识别转移）。
  - 回归夹具：`tests/real_world/flow_method_call.mimi`（双后端 run+build+exec 通过）。

> P4（LLVM codegen 剥为可选 Cargo Feature、VM 为默认 runtime）已确认方案：
> `default = []`，`inkwell`/`z3` 改为 `optional`，新增 `llvm`/`verify` feature，
> `mimi build`/`mimi verify` 仅在对应 feature 下可用。本轮回溯到此，待 0.1.10 内实施。

### 0.40.1.3 — 深拷贝盲区 fail-closed（E0723）

Sprint 0.40.1（A3 在途 WIP 收口）微 sprint：原生（LLVM）后端对深拷贝/所有权移交
盲区由静默透传改为 fail-closed，新增编译错误 `E0723`
（`src/diagnostic/codes.rs` + `docs/error-codes.md` 已登记）。

盲区来源：`devdocs/v0.40/blind-spots-evaluation-2026-08-29.md` §1.3-3/4 指出，遗留
`func.rs` `deep_copy_returned_value` / `type_owns_heap` 路径对 `Set<_>` / `Map<_,_>` 返回
载荷返回 `false`，不触发深拷贝/所有权移交，返回句柄别名已释放堆。此外 `List<List<X>>`
（X 为具体非 string 类型）内层 list 数据数组同样未被认领。

实施（`src/codegen/compile.rs` `compile_checked` 顶层致命门禁，先于任何发射、对所有函数
生效，仅 native 后端；`mimi run`/VM 不受影响）：

- 触发：`Set` / `Map` 顶层返回；`List<List<X>>` 且 X 为具体非 string 类型（`i32`/嵌套
  `List`/`Set`/`Map`/`tuple` 等）。
- 不触发（避免回退）：顶层 record 返回与 `List<List<string>>` 已由 resolved emitter
  安全移交（已验证 `make_outer`/`LogEntry` 双后端正确）；泛型 `List<List<T>>`
  （`chunks`/`group_by`/`wrap`）按泛型参数视为未定，不拦；`List<Record{heap}>` 不拦。
- 回归测试：`src/tests/audit_fix_codegen_expr1.rs`
  `audit_expr1_native_heap_aggregate_return_fails_closed_e0723`（native 触发 E0723，
  VM 路径同程序仍正常运行，证明 fail-closed 仅限 native）。

### 0.40.1.5 — F-001：record 字段深拷贝盲区 fail-closed（同一 E0723 门禁扩展）

MODE-1 排查（5 步）复现评估 §1.3-3/4 的 record 返回 UAF 洞的变体：顶层 record 返回
本身由 resolved emitter 安全移交（已验证 `make_outer`/`LogEntry`/`p_rec_list` 等双后端
正确），但 **record 字段** 含具体嵌套非 string 列表（`List<List<X>>`）/ `Set` / `Map`
时，内层堆载荷未被认领，原生（LLVM）后端读出悬垂数据，而 VM 正确 —— 真实 L1 分歧
（P0）。最小复现 `type Wrap { mats: List<List<i32>> }`：VM `2 2 1 4` vs native
`2 2 38834 1480490477`。

根因：0.40.1.3 的顶层 E0723 门禁只查 `function.ret` 顶层；record 返回名（`Type::Name`
无字段参数）被排除，故「把洞包进 record 字段」即绕过 fail-closed。record 字段类型不在
扁平 `Type::Name` 中，而存于 resolved 类型目录（`CheckedProgram::resolved_field_types`
按字段 `NodeId` 索引 + `resolved_types()` 的 `ResolvedType` 树）。

修复（沿用 0.40.1.3 的 fail-closed _gate，不新增 deep-copy 启发式）：将
`native_return_owns_unclaimed_heap` 的 nominal catch-all 改为经 `program.type_def` +
`field_ids` + `resolved_field_types` 解析字段 `ResolvedType` 并递归判定
（`owns_unclaimed_heap_rt` / `nested_list_rt_owns` 遍历 `ResolvedType` 树，处理
`builtin:type:` 限定名前缀与 `ResolvedType::Primitive` 非 `Nominal` 标量表示）。

- 触发：record 任意字段为 `Set`/`Map`，或具体嵌套非 string `List<List<X>>`（X 为
  `i32`/嵌套 `List`/`Set`/`Map`/`tuple` 等 concrete 非 string）；record-in-record
  递归穿透。
- 不触发（避免回退）：`List<List<string>>`、record 字段为 `string`/标量/`List<Record>`、
  泛型 `List<List<T>>`、顶层 record 返回（已验证安全移交）。
- 回归测试：`src/tests/audit_fix_codegen_expr1.rs`
  `audit_expr1_native_heap_aggregate_record_field_return_fails_closed_e0723`。
- 门禁全绿：`dual_` 1056 / `typecheck::` 112 / `codegen_e2e` 206 均 0 failed；
  `cargo fmt --check` 与 `cargo clippy` 本文件无新增告警。

### 0.40.1.6 — F-002：`List.concat` 原生双释放修复（stdlib `ListExt` 方法）

MODE-1 排查（5 步）复现评估 §6.3 的 stdlib 同名双实现/原生回路隐患的更严重变体：
原生（LLVM）后端对 `concat` **双释放**（glibc `free(): double free detected in
tcache 2`，rc=134），VM 正常返回。最小复现 `use std::collections; concat([1,2],[3,4])`：
VM 打印 `4`；native 双释放崩溃。

根因：`std/collections.mimi` 的 `ListExt::concat` 方法原实现
`let mut ct_result: List<T> = self`，把接收者 `self` 的堆缓冲**别名**进局部
`ct_result`；原生 `push` 按 §6.1 做精确 realloc，释放该共享缓冲，而方法 epilogue
仍对 `self` 调用 free → **二次释放**。相邻形状对拍：`concat([1,2,3],[])`、
`concat(["a","b"],["c"])`、普通 `let b = a`（非 receiver）均正常，唯非空 `List<i32>`
的 `self`-别名路径触发 —— 即方法 receiver 别名后被 realloc 释放、epilogue 再释放。

修复（不新增任何 deep-copy/claim 启发式，遵循 A2 落地前的过渡冻结红线）：改为从空
列表复制（与同文件 sibling `dedup`/`remove_at` 完全一致的模式），`self` 与 `ys` 各
自由各自 scope epilogue 释放，`ct_result` 拥有独立缓冲被返回 —— 消除别名与双释放。

- 触发形态：`concat(xs, ys)` 当 xs、ys 均为非空 `List<i32>`（或其他非 string 元素）。
- 不触发（已对拍）：空尾参、`string` 元素、`let b = a` 普通别名。
- 回归夹具：`tests/real_world/regression_concat_native_double_free.mimi`（双后端
  run+build+exec 均输出 `4`，native 不再双释放；由 `tests/real_world_cli.rs` 自动发现）。
- 不变量类别：L3（内存安全）/ L1（双后端等价）。

### L1 双后端等价性修复（flow-state `match` 原生后端崩溃）

双后端差异实测发现一处真实 L1 分裂：原生后端对 flow 状态机/转移结果的
`match` 在两类形状下崩溃 `error[E0722]`，而 VM 正常。根因是原生 `match`
发射器的单态 flow 快速路径（`emit_static_flow_match`）只接受“所有臂均为
`state:` 构造器”，一旦遇到常见的 `… | _ =>` 兜底臂就退回通用路径，而通用路径
会把 flow 状态当作带判别标签的枚举去查变体目录（flow 状态是裸记录，无判别
标签），于是硬报错；第二类形状是 `fails` 转移的 `Result<state, E>` 臂
`Ok(B { n })` 内层 `state:` 构造器子模式在 `bind_pattern` 中没有解构路径。

本轮**架构性修复**（非补丁式掩盖）：

- `is_static_flow` 门禁放宽为允许通配/绑定兜底臂，使单态 flow 匹配整体走
  正确路径（flow 状态无判别标签，本就不该走枚举路径）。
- `emit_static_flow_match` 改为**按臂序、判定守卫**的活性分析：运行时值恒为
  静态态，第一个能匹配该态的臂（构造器 / 通配 / 绑定）为生效臂；含守卫的生效
  臂保留 fallthrough（守卫可失败）；被前置无守卫臂抢先的臂视为静态死臂但仍编译
  （其 body 不可达）。新增 `bind_static_flow_arm_live` / `bind_static_flow_arm_dead`
  辅助，分别绑定真实字段与哨兵字段。
- `bind_pattern` 的 `Constructor` 臂新增 `state:` 变体分支：把 flow 状态记录按
  字段名/下标解构，递归绑定每个子模式（mirror `bind_flow_arm_variables`），覆盖
  `Ok(B { n })` / `Err(state)` 等内层 flow 状态构造器。

回归夹具（双后端 run+build+exec 通过）：

- `tests/real_world/flow_state_match_single_dual_backend.mimi`（单态态 + `_` 兜底）。
- `tests/real_world/flow_state_match_fail_result_dual_backend.mimi`
  （`fails` 转移 `Result<state, E>` 内层 `state:` 解构）。
- `tests/real_world/flow_state_match_fieldless_dual_backend.mimi`
  （无字段态臂 + 前置 `_` 兜底，覆盖臂序正确性）。

不变量类别: L1
测试: flow_state_match_single_dual_backend / flow_state_match_fail_result_dual_backend
/ flow_state_match_fieldless_dual_backend
（`tests/real_world/run_suite.py` 自动双后端校验，全量 lib 测试 5698 通过）

### 四大阻断项复检 — FFI 闭合（M-004 数据符号导出 + M-001 导出命名）

复检确认 M-010（struct-by-value ABI SIGSEGV）、N-2/M-011②（fn 字段调用）、
M-001 前缀（`__mimi_extern_`）均已在 0.1.10-dev 先行提交中解决。本轮收口剩余两项
真正阻断组件消费的阻断项：

- **M-004（`extern "C" const` 数据符号导出）**：此前 `const` 仅作内联常量，从不发射
  LLVM 全局量，导致 `--shared` 动态符号表里查无组件数据入口（如 `clap_entry`）。
  新增 `extern "C" const NAME: T = V` 语法（mirror `extern "C" func` 导出约定）：
  - AST：`Item::Const` 增加 `extern_abi: Option<String>` 字段
    （`src/ast.rs`）。
  - 解析：`extern "C" const` 分支落入 `parse_const(pub_, Some(abi))`
    （`src/parser/top_level.rs`）。
  - codegen：`compile_file_inner` 对 `extern_abi` 常量发射 `External` 链接全局量 +
    初始化器（标量 i32/i64/f64/bool + string `{ptr,len}` 结构）；`u_` 命名空间
    pass 只重命名函数，数据符号保持干净源名（`src/codegen/compile.rs`）。
  - 计算式/复合常量（如 `2 + 3`、record）**显式报错**（E0713），不静默产零残桩。
  - 验证：`nm -D` 可见 `D ENTRY`；C 端 `dlsym("ENTRY")` 读到 `42`；`mimi run` 双后端
    一致。回归：`ffi_m004_data_symbol_export` / `ffi_m004_computed_const_rejected`。
- **M-001(b)（`--shared` 导出函数保持裸名）**：`compile_to_object` 此前对 shared 构建
  仍套用 `u_` 命名空间 pass，使 `extern "C" func mul` 在 `.so` 中暴露为 `u_mul`，
  C 端 `dlsym("mul")` 失败。现 `compile_to_object` 依据 `codegen.shared` 决定是否
  套用该 pass（`src/codegen/mod.rs`），与测试 helper `compile_to_object_shared`
  行为对齐。验证：`nm -D` 可见 `T mul`（非 `u_mul`）；C 端 `dlsym("mul")` 调用返回
  正确值。回归：`ffi_m001b_shared_clean_export_names`。
- **M-001(a)（libc 名导入与运行时预声明解耦 —— 架构级闭合）**：此前用户以 `i32`
  窄化声明 libc 函数（如 `func strlen(s: string) -> i32`）时，与运行时
  `register_libc` 预声明的同名 `i64` 签名碰撞触发 E0713；只能以 `i64` 声明才不报错。
  根因是「用户 FFI 名」与「运行时内建 libc helper」共用同一符号命名空间。本轮按
  **架构级解耦**（非补丁式掩盖）修复：
  - `declare_extern_and_wrapper`（`src/codegen/registry/funcs.rs`）在签名不匹配时
    **不再一律 E0713**：新增 `reuse_compatible_libc_helper` 判定——当模块内已存在的
    声明恰好是运行时 libc helper（如 `strlen: i64 (ptr)`）时，**复用该现有声明**，
    因为它链接到同一个 C 符号；用户调用点按各自声明的 Mimi 类型消费（返回整型宽度
    收窄等价于 C 原型语义）。约束：变参标志、固定参数个数、以及**每个参数类型**
    必须严格一致（避免 wrapper 的 `call` 实参与被调方 ABI 不符）；**仅返回值整型
    宽度允许不同**（如 `i64`↔`i32` 收窄）。
  - 由此 `func strlen(s: string) -> i32` 直接复用运行时预声明的 `strlen: i64 (ptr)`，
    链接到 C 库 `strlen`，返回 `i64` 后在 wrapper 内截断为 `i32`——用户导入与运行时
    预声明彻底解耦，且**不改动任何 codegen 调用点 / 运行时包裹**（零回归面）。变参
    libc 名（`printf`/`snprintf`/`sprintf`/`fprintf`）保持裸名（运行时变参调用直接
    链接 libc）。真正不兼容的导入（参数类型不符 / 返回浮点不符等）仍正确 E0713 拒绝
    （负向用例已手验）。
  - 验证：用户 `func strlen(s: string) -> i32` 在 native 后端编译/链接/运行返回正确
    长度（11）；参数类型不符 / 返回浮点不符等真正不兼容的导入仍正确 E0713 拒绝
    （负向用例已手验）。FFI 套件 148 全绿，含此前因 rename 方案回归的
    `dual_*_option_set_product_tuple` 三例。
    回归：`extern_block_libc_name_import_native_i32`。

## [0.1.9] - 2026-08-28

### stdlib 合并唯一名（0.39.136 破坏性，迁移注记）

### stdlib 合并唯一名（0.39.136 破坏性，迁移注记）

跨模块同名自由函数曾使任何同时 `use` 双方（如 `std::maps` +
`std::strings`）的程序在合并层硬失败。按全局唯一裸名纪律收敛：

- 移除冗余垫片（方法/内建等价物保留可用性）：
  maps::is_empty → `m.is_empty()`；set 的 size/is_empty/contains/
  remove/to_list 自由函数 → set_* 前缀族；json 的 get_int/get_float/
  has_key → 内建 json_get_int / json_get_float / json_has_key；
  mymath::random_int → `use std::random` 的 random_int；
  datetime::sleep_ms → time::sleep_ms。
- 改名：text::is_blank/count_lines → text_is_blank/text_count_lines；
  env::set → env::set_var（对齐 get_var 风格）。
- maps 的 omit/filter 实现改为内联成员判断，不再依赖裸 contains 名字
  解析（内置与 stdlib 同名的历史陷阱，§5.2 先例的正式化）。

> 0.1.8 已发布（2026-08-22，tag `0.1.8`；0.38.122 收官：lib 5559/0/7、
> real_world 31/0、stress 62/0、dispatch 核语料 0 fallback）。
> 0.1.9-dev 进行中：线性种类 + 权限闭环 + 可写验收。
> 路线 `devdocs/v0.39/README.md`；裁决 `devdocs/kernel-final-verdict-2026-08-18.md`
> Q2-L/Q6/Q10。
>
> **0.1.9 发布就绪（0.39.128，草稿）**：八项交付锁达成 + Q10 基建就绪（Q2 P/L、Q6、语义、
> E0439、教核、Q10 基建、MutexGuard、dogfood）；lib **5663/0/7**、verifier 201/0、
> dispatch check + `--zero` 全语料 0 fallback、Valgrind 18/0、ASan 55/0、TSan 34/0、
> stress 62/0、real_world 31/0、dogfood 全绿。已知延后债：clippy/unsafe-safety
> （0.38 同策略）。**待用户授权切 release tag**。终测记录
> `devdocs/v0.39/quad-final-0.39.md`。

### 0.39.141 — 深度可用性对拍：A1 宽度残留闭合 + 动态 map 确定性 + 浮点/fn 类型修复（L1 修复族）

深度可用性探针（双后端对拍 + 跨进程稳定性实测）驱动的五族修复：

- **A1 宽度模型残留闭合（VM 常量折叠）**：`compile_binary_op` 折叠门控改用与守卫
  路径同一谓词（`binop_is_i32_width`）。修复前仅"声明式 i32 位"挂起折叠——在无锚定
  位置（println 实参、调用实参、尾返回、条件）未注解字面量对按 i64 全宽折叠：
  `println(2147483647 + 1)` VM 打印 2147483648 而 native E0802 trap；
  `println(1 << 33)` 打印 2^33 而 native 掩码后为 2；`println(1024 >> 40)` 0 vs 4。
  现在 i32 宽度下 Pow/Shl/Shr 一律走 Mask/Wrap，Add/Sub/Mul 越界折叠拒走快路落
  CheckI32 trap，所有表达式位置与 codegen 宽度执法逐位一致。审计台账 §9-#10/
  宽度模型 A1 残留从"⏸ 延期"改判"✅ 分歧面已闭合"；audit_fix_vm.rs 两处
  "ADJUDICATED—DEFERRED" 注记更新为 HISTORICAL-CLOSED。
- **动态 map 迭代确定性（双运行时）**：`keys()`/`values()`/`fields()` 键序收敛为
  **键排序**。HashMap 每进程随机种子 → 修复前同一二进制每次运行键序都不同
  （native 实测 5 连跑 4 种顺序），且 VM/native 互异。native `mimi_map_collect`
  与 VM 三内建同步排序；与既有 `mimi_map_to_json_*`（本就排序）及 Value Display
  对齐。
- **未类型化 map 静默句柄输出（resolved 发射器）**：`to_json(m)`/`println(m)`
  （`map_new()` 动态映射）在 resolved 管线落入 `compile_to_json` 整数臂——native
  打印裸句柄 `4294967299` 而 VM 序列化真 JSON。三层根修：(1) resolved 发射器在
  Bind/参数绑定处回填 `var_type_names`（此前为空，legacy 共享的类型名分发全部失明，
  也是既有 to_json 测试全靠 legacy fallback 掩盖的根因）；(2) resolved Call 对
  `Record`/`Map` 实参路由类型化 map 序列化器；(3) 新增 `mimi_map_to_json_any`
  （Component IR 注册 + LLVM 声明），按 `mimi_any_to_string` 启发式渲染 Any 值。
- **legacy map 提示窄化收口**：`block.rs`×2/`func.rs` 三处同构注册点统一走
  `map_value_decodable_by_any`——仅 f64/f32/bool/容器/元组值保留 `Map<string,T>`
  窄提示（Any 启发式不可解码），i32/i64/string 值回落裸 `Map` 走 Any 渲染；
  修复混合 int/string 链按"最后一次插入静态提示"误渲染（`{"age":""}`）。
- **浮点符号与除零码 parity**：一元负 float 改真 `fneg`（原 `0.0-x` 丢负零：
  `println(-0.0)` VM "-0" vs native "0"，resolved 与 legacy 双路径修复）；float
  除零除数补前置守卫报 **E0801 "division by zero"**（原 native 报 E0813；小步语义
  §3 "E0801 (zero divisor)" 不限于整数，VM 同判）。
- **`fn(T) -> U` 类型拼写落地（spec §6.1）**：parse_type 增加 `TokenKind::Fn` 臂与
  `func` 同构 lower 到 `Type::Func`——此前 spec 明文列出的函数类型拼写在参数/注解
  位置一律解析错误。

回归收编：`dual_literal_fold_overflow_traps_unanchored_*` /
`dual_literal_shift_amount_masked_in_unanchored_position` /
`dual_literal_pow_wraps_unanchored_position` /
`dual_i64_annotation_escape_hatch_stays_full_width`（含 i64 目的地注解不拓宽
子表达式的 trap parity 锁）、`dual_prod_map_keys_values_order_deterministic`、
`dual_prod_untyped_map_to_json_and_println`、`dual_fn_type_spelling_in_params_and_annotations`、
`dual_float_negative_zero_display`、`dual_float_div_by_zero_trap_code_parity` +
real_world `probe_q15.mimi`（CLI 级双后端输出对拍）。已知边界登记：未类型化 map 的
f64/bool 异构值受句柄类型擦除限制仍需窄提示（单类型链精确）。

 门禁：lib **5669/0/7**、dual 1030 全绿、golden 重生成（新增两条 runtime 声明）、
fmt 干净。

### 0.39.142+ — JSON 序列化双后端 parity 全闭合 + E0722 跨发射器泛型 ABI 根治（L1 修复族）

0.39.141 之后的 to_json 序列化族与 E0722 跨发射器泛型 ABI 族修复（双后端对拍驱动，
全部收编 real_world / `dual_` 回归）：

- **to_json 递归序列化器单一真源（L1 parity）**：native `to_json` 改为镜像 VM
  `value_to_json`（`mimi_value_to_json` 递归序列化器，3aaba88f），消除两套实现长期
  漂移；新增专用 `mimi_to_json_f64`（按小步语义 §3 浮点确定性渲染）+ 嵌套 record
  递归（7ba4734f），修复前 native 浮点/嵌套 record 与 VM 不一致。
- **嵌套容器正确序列化**：`Option<List<X>>`（d7da1a85）、`Result<Option<List<X>>>`
  （d7da1a85）、`List<Option<X>>`（232fd890）经递归序列化器正确展开，修复前 native
  输出裸句柄或非法 JSON。
- **自定义 enum / Set / Map 递归序列化**：自定义 enum 经递归序列化器路由
  （b46d1fd4）；Set/Map 递归序列化受容器元素 ABI 修复门控（49807761），修复前
  复合值序列化缺失。
- **透明类型别名准入 resolved native slice（36f5a81e）**：`type Id = i64` 等透明别名
  此前在 `to_json` 路径以内部诊断 E0700 fail-closed；现与底层原始类型同权（承接
  0.39.140 透明别名同权，补齐序列化面）。
- **map-literal 值类型推断（a865f1c8）**：动态 map 字面量值类型此前未推断，resolved
  管线 `to_json(m)` 落整数臂输出裸句柄；现与 VM parity（承接 0.39.141 未类型化 map
  序列化根修）。
- **E0722 跨发射器泛型 ABI 根治（9b9a153a + ba58c607）**：根因 = resolved emitter
  Call 臂对带 `type_arguments` 的泛型调用、及复合 T 容器（`List<(T,T)>` 等非标量列表
  元素）在跨发射器 ABI 发生类型擦除 → E0722（0.39.124 / 0.39.135 类缺口）。修复：
  (1) 泛型实例化签名用具体实参类型替换（route-a legacy 单态兜底收敛为根治）；
  (2) 非标量列表元素表示跨发射器统一（ba58c607）。6 个 E0722 靶测
  （`dual_generic_linear_option_flip_cap_ok` / `linear_kind_infection_nested_dual_backend`
  / `dual_container_destructure_tuple_alias` 等）现零 E0722 硬错、native≡VM。
- **修复史注记（防回退）**：`478a2c1a` 的"放宽 slice 准入"式修复曾引入 6 项 L1 回归
  （usability-ledger Round 56），经 `74d4a130` revert，最终由 `9b9a153a` / `ba58c607`
  的根因修复替代，零回归。`usability-ledger.md` Round 32–56 的 E0722 调查台账随之闭环。
- **清理**：移除 codegen 遗留 DBG `eprintln!` 调试输出（220df6e8）。

门禁：`dual_` 套件全绿（含 6 个 E0722 靶测 + to_json 嵌套/递归回归）；`dispatch_stat
check --zero` 维持全语料 0 fallback（透明别名 + map 推断使更多 callable 入 resolved
slice，E0722 形态不再触发）。

### 0.39.140 — 泛型调用返回 ABI/所有权归一 + actor 容器返回 + 透明别名同权（L1 修复族）

双后端对拍驱动的三族修复（探针实测驱动，全部收编 real_world 回归）：

- **泛型调用返回 ABI**：resolved Call 臂对带 type_arguments 的泛型调用按
  ABI 安全性分流——小整数/bool/char/unit 走快速路径，string/f64/i128/
  nominal/tuple 转 legacy 单态化实例。修复 `first<T>(xs: List<T>) -> T`
  返回 string 段错误、返回 f64 静默错译（裸比特当 i64）。
- **legacy 实例所有权归一**：聚合体返回的串形叶改运行时探针
  （owned 转移、借用堆拷贝）；override 返回的 `List<string>` /
  `List<List<string>>` 元素载荷无条件私有化；嵌套列表字面量头堆打包
  （原为栈 alloca 地址逃逸）+ 内层数组登记随嵌套上交外层容器。
  修复 `(T,i32)`/`Result<T,string>` free abort 与 `List<List<T>>`
  悬垂段错误。
- **actor 方法容器返回**：未注解 bind 的 actor 方法结果接入声明类型登记
  （List<string> 不再退化为裸 i64 句柄读取）；epilogue 对局部变量尾表达式
  按声明类型兜底认领；`self.<list 字段> = [..]` 转移 RHS 数组所有权进
  持久字段。
- **透明类型别名同权**：`type Id = i64` 在 let 字面量绑定 / cast 目标 /
  泛型实例化一致性 / 复合载荷位置与底层原始类型同权（此前以内部诊断
  TOOL-RESOLUTION-001 或 E0700 fail-closed）。`newtype X = T` 不透明语义
  保持不变。

回归：`core_generics_return_abi.mimi`（七形状）、
`concurrency_actor_return_list.mimi`、`core_type_alias_transparency.mimi`
（real_world 106/106 interp、105/105 build+exec）；lib 5659/0/7；
golden IR 按预期演进重生成（21 项）。

### 0.39.139 — `module` 关键字退役 + 内联 module AST 管道全删（选项 C 收官）

0.39.137 §6.14 裁决 → 0.39.138 检查期硬拒绝 → 本版完成完全退役：

- **关键字退役**：`module` 从关键字表删除（62 词 / 59 硬），重新成为
  普通标识符（`let module = 42` 合法；正向回归锁在位）。
- **解析期定向拒绝**：item 位形 `module Ident {` 由解析器给出 E0445
  （ParseError 增加可选 code 字段），诊断含模块名 + 迁移指引；拒绝臂
  消费整个块 + 恢复循环尊重子解析器消费进度——单条报错零级联噪声。
- **AST 管道全删**：`Item::Module`/`ModuleDef` 变体、parser parse_module
  管道与 DEPTH_MAX_MODULE 深度预算、checker `module_path` 字段与全部
  下降臂、resolved 的 `module: &str` 参数穿线 / `qualify()` /
  `ResolvedItemKind::Module`、cfg/ir/lower/verifier(5 文件)/LSP(5 文件)/
  loader/lint/doc_core/progressive/flow_matrix/codegen/interp/stats 的
  全部 descent 臂。净删 ~1000 行。
- **测试台账**：11 个 0.39.138 墓碑测试正式删除（被测机制不复存在）；
  3 个内联 module codegen/mms 测试删除；E0445 测试迁移到解析层合同；
  LSP hover/definition/symbols 的 module 测试改锁现存声明面；关键字
  计数锁更名 60/57。ignored 回落 18→7。

门禁：lib **5659/0/7**、real_world_cli + trap_semantics 绿、探针 16/17、
fmt/check_language_docs 干净；CLI 实证 E0445 单条 + `module` 标识符正例。

### 0.39.138 — 内联 module 硬拒绝（E0445，选项 B 落地）

0.39.137 §6.14 评估的"内联 `module` 死面"按裁决收紧为检查期硬拒绝：

- **E0445（新错误码）**：inline `module` 块在 decl 收集期 fail-loud，
  诊断带迁移指引（pub 项移入独立 .mimi 文件 + `use` 裸名合并）。
  check 阶段跳过下降——只报最外层一条，无级联噪声。
- **动因**：死面静默放行声明、调用点才以三种误导性诊断爆炸
  （裸名 E0401 / 限定 E0400+E0221 / 模块内互调 E0207 unknown），
  违反 §4.10 Fail-fast；对 Rust 先验的 AI 代码生成是最坏 DX 组合。
- **测试台账（§14.4）**：11 个纯模块机器件测试（resolved qualified-id ×5、
  audit41 NodeId ×2、audit9 actor-wrap、V-3 qualified-keys、FFI walker
  descent、verify_module_nested）挂 `#[ignore]` 并登记退役里程碑——被测
  机制随 pre-1.0 选项 C（语法+关键字+AST 管道整体删除）一并退役；
  v1_2 三个用例重写为 E0445 合同断言（含嵌套单条报错形状锁定）。
  ignored 计数 7→18。
- **pre-1.0 门禁立案**：`module` 关键字退役（63→62）与 AST 管道删除
  为 1.0 API 冻结前必做清理项。

门禁：lib **5666/0/18**、real_world_cli + trap_semantics 绿、探针 16/17、
fmt/check_language_docs 干净。

### 0.39.137 — 模块系统裁决落地（spec §6.14）+ VM 僵尸管道删除 + csv::get 改名

**评估结论**：模块系统"文档 vs 实现"之争复核后实为伪命题——AGENTS §5.2
记载的 merge 语义与实现一致，真正缺口是 spec 零覆盖。三项落地：

- **spec §6.14 `[stable]`**：文件级 merge 模型成文——`use` 裸名合并、
  duplicate fail-loud、`M::f(...)` 前缀调用不支持、`::` 保留给 Flow 转移边、
  命名自描述约定（stdlib 既有实践）、内联 `module` 为非契约死面。
- **VM 僵尸管道删除**：compiler.rs 的 module-qualified 分发分支 +
  `build_qualified_path`/`collect_module_funcs`/`compile_module_funcs`
  全部移除。该路径对任何通过 checker 的程序不可达（CLI 全路径拒绝：
  内联 E0400/E0221、文件 TOOL-RESOLUTION-001），系 tree-walker 时代化石；
  native codegen 从未编译内联模块，删除后两后端一致。
- **csv::get → csv::cell**：消除与 maps::get 的异语义同名陷阱
  （csv+maps 共导入此前必然撞 duplicate 错误）；stdlib 碰撞矩阵全扫登记
  （其余 13 对为同语义互斥导入对，spec §6.14 记录处理约定）。

测试重述：v1_2 四个 T303/T304 时代用例绕过 checker 直达 VM、锁定僵尸行为
（其一注释自认"type checker may not fully support qualified calls yet"），
按真实合同重写为 fail-closed 断言（E0400/E0401/E0220 拒绝面 + merge 主路径
+ duplicate loader 层错误）；std_csv 夹具同步 cell 改名。

门禁：lib **5677/0/7**、real_world_cli + trap_semantics 绿、探针 16/17、
fmt/check_language_docs/gen_stdlib_docs 干净。

### 0.39.136 — trap 语义双后端对齐 + unit 载荷容器 ABI 修复

**Trap 有序退出（0.39.135 遗留 #1）**：native 的全部用户可见 trap 出口
（`mimi_fail`、E0801/E0802 算术、E0800 转移 miss、E0813 float、pattern
assert、inject_fault、assert_state、match_panic）从 `std::process::abort()`
（SIGABRT rc=134 + 丢弃缓冲 stdout）改为共享 `trap_exit()`——`fflush(NULL)`
冲刷所有流后 `_exit(1)`，镜像 VM 的干净退出。两个 POSIX 异步信号安全原语，
保持 RT-C3 约束；OOM 分配路径保留 abort。探针 p15/p16 由 NO 转 YES
（**矩阵 16/17**，唯一余项为 VM-FFI-lib 设计边界）。

**unit 载荷容器 ABI（0.39.135 遗留 #2 深挖）**：`Result<(), E>` /
`Option<()>` 全形状修复。根因链：`mimi_type_to_llvm(unit)=None` 毒化整个
容器 lowering → legacy `declare_func` 回退 i64 签名 → resolved 发射器把
结构体返回体装进 i64 函数（**无效 IR：签名与终止子类型不符**）→ 调用方按
聚合=i64 指针约定 inttoptr 解引用垃圾（段错误 rc=139）。修复三处：
- `container_payload_llvm` 哨兵（unit→i64，镜像 resolved 显式 ABI），同时
  接入自由函数与 `CodeGenerator::llvm_type_for` 两套 lowering（后者优先真实
  lowering、unit 兜底，防记录载荷被未知名→i64 兜底劫持）；
- resolved `declare_callable` 复用已声明符号时校验签名兼容，不匹配 fail-loud
  E0722 拒绝发射（堵住此类无效 IR 整类）；
- `Option<()>` 显示特判：`Some(())`/`None()` 对齐 VM 与既有锁定约定。

修复波及面顺带闭合：`match Result<(), E>` 的 `Ok(_)` 臂（此前 E0713）、
跨函数调用、注解/推断两种绑定形态。附带发现并确认非缺陷：裸 `write_file`
双后端正常（0.39.135 记录的 E0722 形态不复现）；`use std::fs` 后须用
非限定名调用包装函数（模块前缀语法不支持，文档问题另案）。

门禁：lib **5677/0/7**（含 unit 家族 4 项新回归 + trap 双测）、
trap_semantics 集成测试 2 项（二进制级 stdout 保留锁）、real_world_cli 绿、
fmt 干净。

**探针收编（e18 类盲区闭合）**：0.39.135 全特性评测的 p01–p14 正向探针
入册 `tests/real_world/probe_p*.mimi`——real_world_cli_suite 对全部程序做
VM/native 输出级对拍（TC-C5/L1），内核卡 e18"编译通过但输出分歧"类缺陷
从此被 CI 门禁拦截，不再依赖 dispatch 门禁的仅编译成功判定。trap 两项由
tests/trap_semantics.rs 锁定；FFI 探针维持设计边界登记。并发探针连跑三次
无 flake。

### 0.39.135 — 可用性修复：全特性真实可用性探针驱动的四项 P0 双后端分歧 + E0444

AI 全特性评测（17 正向探针 + 6 负例 + 合约验证面，双后端对拍）发现并修复：

- **P0-1/2 newtype 透明化（VM 侧）**：构造器恒等化（对齐 codegen
  `registry/types.rs` identity 注册）+ 解构模式直绑 scrutinee + `.0` 标量投影
  恒等。修复三处分歧：注解解包 `let v: i32 = u` VM E0800 崩溃、
  `println(u)` 印 `UserId(42)` vs native `42`、newtype 实参进标量形参崩溃。
- **P0-3 actor 方法返回 string（native）**：mailbox 结果打包在
  `emit_actor_method_epilogue` 缺失 `claim_string_return_value`——隐式尾表达式
  的 concat 缓冲在作用域清理解放后才返回（悬垂 {ptr,len}），mailbox 调用方
  观察到空串而 VM 打印真值；**内核卡 e18 正例本身即此形状**，dispatch 门禁
  只验编译成功未查输出等价的盲区由此暴露。
- **P0-4 泛型单态化记录按值 ABI（native）**：mono 实例签名按值收
  `{fields}`，调用点却传 alloca 指针——LLVM 把地址位重解释为字段数据，
  `pass<Plain>(p); println(q.v)` 输出垃圾/段错误。泛型调用点对具名聚合
  参数补加载（字符串除外，仍走 C 指针包装路径）。
- **语义对齐 runs-Flow actor 方法分发（VM）**：mailbox worker 此前把
  runs-flow actor 的全部消息路由进转移分发，普通方法
  （`func ping()`）死于 "no transition" 而 native 直呼方法。改为方法名
  匹配优先、未匹配回落转移分发（与 codegen 编译期解析一致）。
- **E0444（新错误码）：session 协议载荷必须整数标量（i32/i64）**。
  f64/string 载荷此前通过检查但在 VM 运行期 E0800 / native 静默错包；
  现于声明处 fail-closed。带环引用守卫（A=B;B=A 不再爆栈）。
- **回归收编**：`src/tests/usability_fixes.rs` 七项 L1 双后端测试 +
  E0444 负例锁。

裁决记录：Free 类型实例化 `linear T` 泛型是**合法单态化**
（`linear_kind_monomorphization_multi_instantiation` 锁定 cap+i32 双实例），
内核卡 §2 的 "Free→线性位置 → E0432" 指具体线性签名位置而非泛型实例化——
反向 kind 检查已实现并撤销。已知边界维持：trap 双后端退出码语义
（VM rc=1 vs native SIGABRT）、裸 `write_file` 内建 resolved 委托硬错
（stdlib 包装可用）、VM FFI 需 MIMI_FFI_LIB。

门禁：lib **5671/0/7**（含 7 项新回归）、fmt 绿。

### 0.39.131 — 语义 F4 修复：小步语义补齐锁清单覆盖
- `docs/spec/small-step-semantics.md` 新增四节（253→330 行，仍 ≤20 页）：
  - §8 消耗/构造（线性值由构造器产生、被恰一个合法消费者消解，L 账本镜像）；
  - §9 纪元在逃逸点（本地 self-loop 剥 epoch 无税；Channel/FFI/mailbox 必须显式
    打包，裸 Flow 被 check 拒；旧句柄 typed stale）；
  - §10 Flow `fails E` 与 Fault（回滚归还 source generation；Fault 是独立系统态，
    非第二个 Err；`?` 走回滚道）；
  - §11 view/mutate 作用域（同步调用动态作用域只读/排他，不消耗线性值，
    跨边界不可借）。
- 原 §8（Determinism）重编号为 §12。语义锁覆盖清单由缺四项→全达成。
- docs-check 绿；verifier 201/0 保持。

### 0.39.134 — 三版本深度自查（0.1.7 / 0.1.8 / 0.1.9）
- **0.1.7**：7/7 DoD 锁逐项实证——dispatch 0 fallback、soak（stress 62/0+28/0）、
  native 并发+TSan 34/0、**10M native event storm 实跑 1 passed（0.20s）**、
  边缘出清（quote! 两负测试 + protocol "removed in 0.1.7 Phase E"）、dogfood、
  文档诚实。**无缺口**。
- **0.1.8**：10/10 锁逐项实证——L1 spawn（deadlock parity：VM/native 都必须
  hang 禁顺序假成功）、Narrow 4 边界 E0442、Q8（B-STR-001 embedded_nul +
  B-HANDLE-001 handle_lease 5 测试绿）、Q1 S（flow_epoch 9 + bare-flow-across-
  channel 拒绝）、Q4 A（E0402 实测）、Q5 K（session 方法化 VM+native）、Q3 M
  （move-rest + Free 读不耗 self）、Q9（kernel-card 分层）、mms 拒绝。**无缺口**。
- 结论：0.1.7/0.1.8 扎实（真实测试背书）；0.1.9 为本轮草率 sprint（6 缺口已修）。
- 报告：`devdocs/v0.39/self-review-0.1.7-8-9.md`（54 行）。

### 0.39.133 — 自查 F2/F3 收尾 + cap resolved 缺口实证（诚实记录）
- **F2**：`src/core/checker/linear_blackbox.rs` → **`linear_kind.rs`**（名实相符：
  模块是 linear 种类检查实现，非旧「调用点黑盒」；`mod linear_kind`）。README
  Q2 L 措辞修正（「删除主体」→「调用点黑盒退役 + 体分析引擎重构为
  linear_kind_body_sound」）。
- **F3 补强（.gitignore 修复）**：`examples/kernel-card/` 此前被 `examples/*`
  忽略从未真正入仓（F3 提交声称 tracked 实为未跟踪——自查暴露）。已加
  `!examples/kernel-card/` 使 20 例真入仓。
- **cap resolved 缺口实证**：尝试把 `cap C`（Capability）准入 resolved slice
  （opaque i64，镜像 SystemToken）——裸 cap 直通（e12/e13）与 `List<cap>` 非泛型
  组合（e19）可 resolved，但 `identity<linear T>(List<cap>/Option<cap>)`
  （容器经泛型直通）在 resolved 的 i64 擦除槽硬 E0722（0.39.124 类缺口）→
  6 个 P 合同/dual 测试回归。**决策：回退 cap 准入**；组合锁由 contract_p 双后端
  （legacy，VM/native 一致）满足。正例集改用已入 resolved 的 SystemToken/
  MutexGuard 演示线性种类（e12/e13/e19 全 resolved）。
- **结果**：lib **5664/0/7**、dispatch --zero 全绿（20 例 kernel-card 全 resolved）、
  dogfood 全绿；cap 容器经泛型直通留已知缺口跟踪（0.39.124 类）。

### 0.39.130 — 教核 F3 修复：内核卡补 20 个手写正例
- **`devdocs/kernel-card.md` §7**：新增「手写正例集（20 例）」表——覆盖
  func/while/for/记录/枚举+match/newtype/Flow+transition/Flow fails/Result+`?`/
  Option/view+mutate/`linear T` 直通/`linear drop T`/SystemToken 恰一次/能力门禁
  I/O/MutexGuard/Session/actor runs Flow/List 线性感染/contracts 有界。
- **`examples/kernel-card/`**（新增，tracked）：e01-e20 完整可运行源码，每例
  独立 `mimi check` 全过；代表性 7 例（07/12/13/16/17/18/20）native 编译通过。
- 教核锁「20 个手写正例」由缺失→达成；自查 F3 关闭。

### 0.39.128 — Phase G：全门禁收口记录
- **功能门禁全绿补跑**：`cargo check` rc=0；stress **62/0**（28 ignored）+ heavy
  **28/0**；real_world **31/0**；real_world_cli **2/0**——与 0.38 基线一致。
- **已知延后债（不影响功能门禁，与 0.38 同策略）**：
  - clippy -D warnings：~258 missing_safety_doc（既有 unsafe 债，不 mass-edit）；
  - unsafe-safety gate：168 unsafe 无 SAFETY 注释（基线 37，非新债）。
- quad-final-0.39.md 补全：stress/heavy/real_world/real_world_cli 数字 + 已知债
  说明。纯门禁/文档切片。

### 0.39.127 — Phase G：发布前安全套件补跑（Valgrind/ASan/TSan）
- **Valgrind**：`e2e_valgrind_ --include-ignored` **18/0**（与 0.38 同基线）。
- **ASan**（nightly build-std）：lexer 1 + parser fuzz 10 + property 44 =
  **55 passed / 0 failed**（detect_leaks=1 halt_on_error=1）。
- **TSan**（nightly build-std）：future 6 + FFI 17 + channel 11 =
  **34 passed / 0 failed**。
- quad-final-0.39.md 更新：dispatch `--zero` 全绿 + valgrind + sanitizer 数字。
- 纯门禁/文档切片（无行为变更）；lib 5663/0/7 保持。

### 0.39.126 — Phase G：Result/Option `unwrap` 进入 resolved slice → dispatch --zero 全绿
- **实现**：`result.unwrap` / `option.unwrap` 加入 resolved native emitter
  （镜像 legacy `compile_unwrap_expect`）：Some/Ok → payload；None/Err →
  `mimi_try_exit(0)`（noreturn）→ unreachable。退役 0.1.8 的 eligibility 硬化
  拒绝（该拒绝曾把含 unwrap 的函数整体降级 legacy）。
- **效果**：
  - std_env 从 3 resolved-skips → **0**（result.unwrap 根因闭合）；
  - `dispatch_stat check --zero` **全语料 0 fallback、rc=0**（此前 std_env/std_net
    登记已知回退）——152 个 .mimi 全部 emitter=resolved；
  - unwrap Ok → 值（VM/native 同 `7`）；unwrap Err → 双后端均 trap（非零退出，
    message 文本可异、退出码一致，dual 语义等价）。
- **测试**：phase_g 新增 `phase_g_result_unwrap_resolved_slice`（Ok 双后端 +
  Err 双后端 trap）。
- **门禁**：lib **5663/0/7**、make test-dogfood 全绿、dispatch check 与 --zero
  均 rc=0。

### 0.39.125 — Phase G：quad-final-0.39 终测框架（草稿）
- **`devdocs/v0.39/quad-final-0.39.md`**（68 行，镜像 0.38 结构）：0.1.9 终测记录
  草稿——九项交付锁（Q2 P/L、Q6、语义、E0439、教核、Q10、MutexGuard、dogfood）
  + 测试结果表 + 生产 dual/静默回退 + 已知限制 + 发布建议。
- **当前数字**：lib 5662/0/7、verifier 201/0、phase_d 25/0、phase_e 5/0、
  phase_f 2/0、phase_g 1/0、bin CLI 30/0、dogfood 全绿、dispatch-core 全 resolved。
- **发布建议**：功能门禁全绿 → 可进入发布流程，但 tag/冻结动作待用户授权；
  发布前补跑 ASan/TSan/Valgrind + make ci-full。
- 纯文档切片（草稿，非发布）。

### 0.39.124 — 已知缺口调查：resolved `linear T`+Free 实例化（跨发射器泛型 ABI）
- **触发**：core 函数（含线性类型）+ 以 **Free 类型**（如 string）实例化
  `linear T` 泛型调用 → resolved 侧按 legacy 泛型前向声明的类型擦除 i64 槽做
  参数强制转换（string→i64）→ E0722 硬错（fail-closed，非静默）。
- **根因**：跨发射器泛型 ABI——legacy 泛型前向声明对类型参数用 i64 槽，resolved
  main 调用时参数类型不匹配且不可强制。属 monomorphization 类型擦除缺口。
- **已排除**：`linear T` 单独以 string 实例化（无 SystemToken）可 resolved 编译
  （iso4 过）；SystemToken 单独用（无 linear T）可 resolved（iso2/iso3 过）；
  仅 **core 函数内**二者组合触发。
- **正确形态（已采用）**：`linear T` 承载真正线性值（SystemToken/cap）——mimi-ledger
  custody 已改为 token 经管线整体转移，resolved 全绿（fallback 0.000）。
- **处置**：记录为已知限制（loud E0722，非静默错误），留独立切片修跨发射器泛型
  类型擦除；不阻塞 0.1.9。
- 工作树干净；lib 5662/0/7 保持。

### 0.39.123 — Phase G：SystemToken 进入 resolved native slice（权限闭环）
- **缺口**：dispatch 门禁显示 dogfood/ledger 的 SystemToken 函数走 legacy 回退——
  根因 `nominal type 'builtin:type:SystemToken' is not a record or enum in the
  resolved native slice`（函数级整体回退）。
- **修复**：
  - `src/codegen/resolved/eligibility.rs`：SystemToken 加入不透明句柄放行
    （同 SessionChan/actor/Future）；
  - `src/codegen/resolved/types.rs`：`builtin:type:SystemToken` 降为 opaque i64
    句柄（同 Mutex/Channel/MutexGuard）；
  - 内置函数（make_token/token_id/read_file_guarded/get_env_guarded/http_get_guarded/
    token_channel_*）codegen 分发早已就绪 → 补上类型降级后完整走 resolved。
- **结果**：examples/dogfood 46/46、mimi-ledger 52/52 **fallback 0.000**（原
  0.065/0.038）；phase_d 25/0 全绿；lib **5662/0/7**；make test-dogfood 全绿。
- **顺带修正**：mimi-ledger `custody<linear T>` 原以 Free 字符串实例化——语义上
  linear T 应承载真正线性值，改为 SystemToken 经管线整体转移（也规避 resolved
  `linear T`+Free 实例化的已知缺口，见下）。
- **已知后续**（不阻塞）：resolved `linear T` 以 Free 类型（如 string）实例化仍
  E0722（数值转换串位）——真实缺口，留独立切片修 monomorphization 类型擦除。

### 0.39.122 — Phase G：生产 dogfood 接入线性种类（mimi-ledger）
- **mimi-ledger**（projects/mimi-ledger/src/main.mimi，生产 dogfood）接入
  线性种类，非玩具：
  - `custody<linear T>`：账户 id 经线性泛型管线**整体转移**（Phase A-C 种类
    系统，替代 blackbox）；
  - `audit_ledger(acct, t)`：SystemToken **能力门禁审计**——每次审计恰一次
    消费一枚 token（token_id + drop），返回确定性摘要（不打印进程级计数器，
    保证可复跑）；
  - `test_linear_kinds()`：`mimi test` 新增用例，4/4 passed。
- **门禁**：`make test-dogfood`（mimi-taskq + mimi-ledger + mimichat +
  mimichat-modern check/test/build/run）全绿；lib **5662/0/7** 全绿。
- 生产 dogfood 现含 linear T + SystemToken 真实用法，满足 Phase G「须用
  linear T 或 cap 至少一处非玩具」于**生产项目**层面达成。

### 0.39.121 — Phase G 首切片：dogfood 线性能力管线（非玩具）
- **dogfood**（examples/dogfood/linear_guarded_backup.mimi，tracked）：带
  SystemToken 能力门禁的"受保护备份"流水线——非玩具真实逻辑：
  - `linear T` 泛型直通 `transfer<linear T>`：token 经泛型管线整体转移
    （Phase A-C 种类系统教科书用例，删除 blackbox 后由种类系统表达）；
  - `stage(t)` 保持 token 线性原样返回；`read_guarded` 以 SystemToken 为门禁
    恰一次消费 read_file_guarded（Err 分支）；get_env_guarded("PATH") 成功分支；
  - MutexGuard 恰一次 unlock（共享备份计数）。
  - VM≡native 同输出：`mutex-guard-ok / guarded-read-rejected / guarded-env-ok`。
- **守卫**（src/tests/phase_g.rs，tracked）：dogfood check + 双后端等价断言——
  线性/能力/guard 组合回归必红。
- 满足 Phase G「须用 linear T 或 cap 至少一处非玩具」交付项。
- lib **5662/0/7** 全绿（含 phase_g 1 例）。

### 0.39.115 — Phase F 评测 SOP + 失败聚类 + 修复钩子接口
- **修复钩子接口**（eval/repair_hook.example.sh）：retry_loop 的修复槽——
  `--task --candidate --diagnostic --round --next` 契约；默认 no-op 拷贝，真实
  模型替换该脚本即可接入（本环境 ollama 可用但无本地模型、离线拉不到 → 接口
  就绪、模型留待可拉取环境）。
- **失败模式聚类**（eval/cluster_failures.py）：对失败任务按 round-1 候选跑
  `mimi check` 提取错误码，聚类为 check/parse、check/E0256、escape、semantic
  等；escape 源码级优先。对抗集 4 题正确归类。
- **评测 SOP**（eval/README.md）：冻结 → 生成候选（只发任务描述）→ 跑 harness
  /retry → 聚类 → 填报告；指标口径与任务折叠说明；已知边界（离线无模型、
  token_id 计数器依赖规避）。
- Phase F 工具链完备：单题门禁/批量/自动重试/修复钩子/聚类/报告，全部入版本库
  eval/；`src/tests/phase_f.rs` 参考解守卫保持绿。
- 纯工具/文档切片（无 Rust 行为变更）。

### 0.39.114 — Phase F 自动重试 driver + 工具链入版本库
- **retry_loop.sh**：逐轮自动重试 driver——round 1 跑初始候选；semantic 未过且
  未达 max_rounds 时读预置 round_N+1 修复（或接真实 LLM 修复钩子）继续；达上限
  干净停止。t02 泄漏轨迹 round1（E0256）→ round2（token_id 消费）PASS 复现。
- **首份正式报告**：`devdocs/v0.39/phase-f-report.md`（基建验证 001）——冻结清单
  + 四场景 CSV（基线/对抗/修复轨迹/自动重试）+ 聚合表 + 失败模式 + 结论。
- **工具链入版本库**：评测 harness/任务集/冻结从 gitignored `devdocs/mimi-eval/`
  迁至 **tracked `eval/`**（scripts/ 风格，25+ 文件已 tracked；gitignore 仅盖
  devdocs/）——聚合逻辑已修 3 次 bug，必须受版本控制。报告仍按路线落 devdocs/。
- 聚合场景指标：基线 6/6 全过、对抗正确捕获、修复轨迹 avg_fix_rounds=2.00、
  自动重试 round2 PASS。

### 0.39.113 — Phase F 修复轮迭代 + 轨迹指标校准
- **聚合修复**（run_eval.sh）：任务按 `tXX.N.mimi` 剥离 round 后缀折叠为单一任务；
  `first_check` 取各任务 **round-1** 行（此前误取最后一轮 → 修复轨迹下 first_check
  错归零）；`semantic`/`escape` 取最后一轮行；未通过计 max_fix_rounds。
- **修复轨迹演示**（devdocs/mimi-eval/repair-trajectory-demo.md）：hand-authored
  bad→good 证明 avg_fix_rounds 可测量——
  - 轨迹 1（t02 线性泄漏）：round1 E0256（help 直接给出修复方向）→ round2 用
    `token_id(t)` 消费 → 全绿；
  - 轨迹 2（t05 错误输出 4≠5）：round1 check 过 semantic 不过 → round2 改回 → 全绿；
  - 集合指标：tasks=2, first_check=0.50, semantic=1.00, avg_fix_rounds=2.00。
- **三场景回归**：基线（参考解）全 1.00；对抗集（语法/泄漏/mms{}/错输出）正确捕获；
  修复轨迹（2 轮）正确聚合。harness 具备可复跑、可判别、可测修复轮的完整能力。
- 纯基建/文档切片。

### 0.39.112 — Phase F 任务集质量自检 + harness 修复
- **harness 修复**：逃生舱检测移到 check 之前——escape-abuse 是**源码级**判定
  （用了 mms{}/thread_local 即记滥用），不受 check 成败影响（t03 用 mms{} 即使
  parse 失败也记 escape=1）。
- **对抗自检**：4 道典型失败候选验证 harness 分门别类正确捕获——t01 语法
  （check=0）、t02 线性泄漏（check=0）、t03 mms{}（escape=1）、t05 错误输出
  （check=1 semantic=0 dual=1）。基线保持 6/6 全过。
- **参考解守卫**（`src/tests/phase_f.rs`，tracked）：6 道参考解嵌入断言
  check + 双后端等价 + 无逃生舱构造——编译器回归使评测基线失效必红。
- **t02 反脆弱**：token_id 输出依赖进程级计数器（并行测试下非 1）→ 参考解改为
  token 消费后打印固定 marker `t02_linear ok`（线性纪律仍由 check 强制：
  泄漏 E0256）。修复 phase_f 在全量并行下的偶发失败。
- lib **5661/0/7** 全绿（含 phase_f 2 例）。

### 0.39.111 — Phase F 评测基建（任务集 + harness + 冻结 + 基线）
- **任务集**（devdocs/mimi-eval/tasks/，6 题，覆盖内核卡 §1 全类别）：
  t01 Flow / t02 线性种类 / t03 Session / t04 actor runs Flow / t05 失败分层 /
  t06 对照 CRUD——每题配规范正例（VM≡native 双后端验证过）与期望输出。
- **评测 harness**（devdocs/mimi-eval/eval_harness.sh + run_eval.sh）：对单个候选跑
  check / semantic（输出比对）/ dual（VM≡native）/ escape-hatch（mms{}、
  thread_local cap），输出可复跑 CSV 行（task,round,check,semantic,dual,
  escape_abuse,first_check_ok）。
- **聚合脚本**：首次 check 率、语义测试率、平均修复轮数、逃生舱滥用率。
- **冻结清单**（freeze.toml）：编译器版本、内核卡 SHA、模型/采样、最大修复轮次
  （默认 5）；escape-hatch 定义与内核卡同步（cap 是内核线性值，不记滥用）。
- **基线冒烟**：参考解当候选 → tasks=6，first_check=1.00，semantic=1.00，
  avg_fix_rounds=1.00，escape_abuse=0.00——harness 产出可复跑数字。
- **内核卡修正**：`cap` 声明式能力厘清为「内核线性值 / linear T 载体（I/O 权限
  走 SystemToken）」，非出核。
- 报告模板：devdocs/v0.39/phase-f-report-template.md。
- 纯基建/文档切片；lib 门禁不受影响。

### 0.39.84 — Phase E 收尾回归 + 文档同步
- **回归**：四项交付全门禁复跑——E0439（verifier 201/0）、MutexGuard（phase_e
  5/0 + 全部 mutex 11/0）、小步语义/内核卡（docs 门禁绿）、**lib 5659/0/7 全绿**。
- **文档同步**：
  - `docs/error-codes.md`：E0256/E0304 扩为「linear resource」（覆盖 SystemToken /
    MutexGuard / Flow 状态 / Session 端点 / linear drop T）；E0439 更新为
    0.39.80-80b 闭口说明（有界 Proven/无界 Disproven 双引擎一致，残余分歧 fail-closed）；
  - `docs/spec/small-step-semantics.md`：trap 码对齐实现——溢出 E0802、
    零除 E0801（原误标 E0801/E0800）。
- 无行为变更（纯文档 + 回归）。

### 0.39.83 — 发布内核卡（唯一 AI 合同）
- **交付**：`devdocs/kernel-card.md`（68 行）——Mimi 唯一 AI 合同，一页覆盖：
  - 内核清单（func/let/if/match/while/for、type/newtype、flow+state+transition、
    Result/Option/?、fails E、view/mutate、线性两件套+SystemToken、MutexGuard、
    Session；`mms{}`/`cap`/thread-local cap 出核）；
  - 线性种类（Free/linear/linear drop，E0432）与恰一次契约表（E0304/E0256）；
  - 错误码契约（E0304/E0256/E0432/E0439/E0800/E0801/E0242）；
  - 双后端等价（VM≡native）与验证口径（有界 Proven/无界 Disproven，0.39.80-80b）。
- **接线**：AGENTS.md §0 显著指针 + 0.1.9 路线行更新；README.md 文档表新增条目；
  v0.39 执行文档指向内核卡。
- 纯文档切片（+ AGENTS/README 指针），无行为变更。

### 0.39.82 — 小步语义文档（规范性附录 mimi-small-step-1）
- **交付**：`docs/spec/small-step-semantics.md`（252 行）——内核 `K` 的小步
  操作语义，挂 `language-spec.md` §11 规范性附录：
  - 内核语法（值/表达式/块/语句）；机器整数算术（i32/i64，SD-7 trap 语义：
    溢出 E0801、零除/MIN÷-1 E0800）；
  - 确定性小步规则 + 求值上下文（左到右、最内层先、严格实参）；
  - 短路 `&&`/`||`、let/if/match/块/return/函数应用；
  - **线性资源账本**（exactly-once）：introduce/move/transfer/release 转换 +
    E0304（二次移动）/E0256（泄漏）不变式；
  - 确定性 + 双引擎与 trap 行为的一致性（E0439 fail-closed，0.39.80-80b）。
- **接线**：language-spec.md 新增 §11 附录指针（prose 歧义处以小步规则为准）。
- 纯文档切片，无行为变更。

### 0.39.81 — MutexGuard 恰一次 unlock（线性 guard）
- **实现**：MutexGuard 改为**线性资源**（move-only、必须恰一次消费）：
  - checker `is_linear_surface_type` + ir `nominal_is_linear` 认
    `builtin:type:MutexGuard` → CFG 追踪 guard；
  - `mutex_unlock(g)` **消费** guard（唯一合法释放）；`mutex_get(g)`/`mutex_set(g,v)`
    **借用** guard（读取/写入后仍需解锁，CFG borrow-skip）。
- **语义**（Phase E「恰一次 unlock」达成）：
  - 双重 unlock → **E0304**（guard 已消费）；guard 泄漏（未 unlock 返回）→ **E0256**；
  - guard 可整体转移（move / 跨函数）后由新绑定解锁；双后端一致。
- **测试**：`src/tests/phase_e.rs` 5 例（lock/get/set/unlock 双后端、双重解锁
  E0304、泄漏 E0256、move 后解锁、跨函数解锁）。
- **兼容**：既有 dual_mutex 3 例 + 全部 mutex 6 例保持（均恰一次 unlock 模式）。
- lib 5659/0/7 全绿。

### 0.39.80b — E0439 i32 歧义全闭（实证 + 测试加固）
- **实证**：0.39.80 修 `&&` 编码后，逐引擎核对 i32 溢出两个方向：
  - 有界 `x>=0 && x<=1000, x*2` → resolved **Proven**、flow **Proven** → 合并
    **Proven，无 E0439**；
  - 无界 `x*2` → resolved **Disproven**、flow **Disproven**（VIR 体算术 definedness，
    §11-#46）→ 合并 **Disproven，无 E0439**。
  → **E0439 i32 方向歧义全闭**：两引擎在有界/无界双向一致。
- **测试**：`dual_engine_unbounded_i32_overflow_fails_closed` 加固为
  `dual_engine_unbounded_i32_overflow_agrees_fail_closed`（断言 Disproven **且**
  无 E0439）。verifier 201/0、lib 5654/0/7 全绿。

### 0.39.80 — E0439 方向收敛：resolved 引擎修复 `&&` 前置编码（Phase E 首切片）
- **问题**：`resolved_to_z3_bool` 的 `LogicalAnd`/`LogicalOr` 分支嵌套在整数比较
  臂内——仅当两操作数都编码为 Int 才可达。布尔比较操作数（如 `x >= 0 && x <= 1000`）
  导致整个 requires 静默丢弃（encoding_failures++），求解器视 x 无约束 → 溢出 VC
  找到负 x 反例 → **有界 i32 合同被 resolved 引擎误判 Disproven**，与 flow 引擎
  Proven 相歧 → E0439（0.34.44 锁定的 i32 反向歧义根因）。
- **修复**：`resolved_to_z3_bool` 在 Binary 臂顶部新增布尔逻辑分支——LogicalAnd/Or
  先按布尔操作数编码。有界 i32（`x>=0 && x<=1000`, `x*2`）resolved 现 **Proven**，
  与 flow 一致、无 E0439。
- **语义保持**：无界 i32 溢出仍 fail-closed（flow 无界/assume defined vs resolved
  checked → E0439 + Disproven，不静默 Proven）。
- **测试**：`dual_engine_divergence_on_i32_definedness_is_fail_closed` 改为
  `dual_engine_agrees_on_bounded_i32_definedness_no_divergence`（Proven 无 E0439）；
  新增 `resolved_engine_encodes_conjunctive_precondition`、`dual_engine_unbounded_
  i32_overflow_fails_closed`。
- lib 5654/0/7 全绿（verifier 201/0）。

### 0.39.79 — 废止 thread-local cap 重新注册协议（迁移标记）
- **裁决**：legacy thread-local cap 协议（R-4：`mimi_cap_register/check/consume/
  drop`，跨线程须"发 id+name 重注册"）**废止为 legacy/迁移标记**，0.1.9 内保持
  兼容不删。推荐能力模型 = **SystemToken**（0.39.71-78）：
  - `make_token()` → 进程级唯一（非 thread-local）；
  - `TokenChannel` / actor mailbox → 跨任务/跨线程转移，checker/CFG 线性恰一次；
  - `read_file_guarded`/`get_env_guarded`/`http_get_guarded` → 收 cap std API。
- **落地**：`capability.rs` 头注释标 DEPRECATED 并指向 SystemToken；跨线程告警
  消息补迁移指向（行为不变，无测试断言旧消息）。`phase-d-plan.md` §8 记录裁决。
- **测试**：`src/tests/phase_d.rs` 25 例（+1 推荐能力路径端到端双后端）。
- lib 5652/0/7 全绿。

### 0.39.78 — actor mailbox token 面（SystemToken 跨 mailbox 转移）
- **实现**：SystemToken 单独开面——可作 actor 方法参数（transfer-in）与返回值
  （transfer-out），走 TokenChannel 同款转移模型：
  - `items.rs` AUD-4 放行 SystemToken（新增 `is_system_token_type` 判定）；
    SessionChan 等其余线性面禁令不动（E0432 保持）。
  - 调用点消费旧绑定（E0304）、方法体持全新义务（须恰一次消费 E0256）。
- **语义验证**（双后端）：
  - actor 方法收 SystemToken 参数并消费 → VM/native 直通；
  - actor 方法返回 SystemToken（give）→ VM/native 直通；
  - 调用后旧 token 复用 → E0304；方法体泄漏参数 → E0256；
  - SessionChan 跨 mailbox → E0432（禁令保持）。
- **测试**：`src/tests/phase_d.rs` 24 例（+5：param 双后端、return 双后端、
  sender E0304、param leak E0256、SessionChan E0432）。
- lib 5651/0/7 全绿。

### 0.39.77 — Phase D 阶段收尾：迁移标记 + 交接文档
- **迁移标记**（`phase-d-plan.md` §6）：fs/net/env 推荐收 cap 路径
  （read_file_guarded / http_get_guarded / get_env_guarded）vs 既有不收 cap
  面（标迁移方向，0.1.9 内保持兼容不删）。
- **交接状态**（§7）：Phase D 0.39.71-76 全部完成（token 唯一 id → 线性
  SystemToken → TokenChannel 跨任务 move → 泛型/容器集成 → fs/net/env 收 cap
  API 各一条）；lib 5646/0/7；phase_d 19 例。待续：0.39.77 起迁移收尾、
  mailbox token 面、thread-local 废止、Phase E/F/G。
- **技术债/已知限制**：token 唯一性进程内；收 cap 现为调用消费语义（借用变体
  后续）；网络 I/O 受 SSRF 保护（cap 面以类型/线性面为证）。
- 纯文档切片，无行为变更。

### 0.39.76 — 收 cap 的 net API：http_get_guarded（fs/net/env 各一条达成）
- **实现**：`http_get_guarded(url, t: SystemToken) -> string`（net 域收 cap）。
  t 为 SystemToken 能力门禁，被调用消费；运行时忽略（线性由 checker/CFG 保证）；
  复用 http_get 核心（interp wrapper + codegen compile_http_get_guarded 复用）。
- **至此 Phase D「fs/net/env 至少各一条 API 收 cap」达成**：
  fs `read_file_guarded` / net `http_get_guarded` / env `get_env_guarded`。
- **语义**：无 token → E0242；调用后旧 token 复用 → E0304；check 通过。
  网络 I/O 在本环境被 SSRF 保护阻断，运行时接线以 native 可编译为证。
- **测试**：`src/tests/phase_d.rs` 19 例（+3：check 通过、缺 token E0242、
  消费后复用 E0304）。
- lib 5646/0/7 全绿。

### 0.39.75 — 收 cap 的 fs/env API（SystemToken 能力门禁）
- **实现**（fs + env 两域各一条收 cap API）：
  - `read_file_guarded(path, t: SystemToken) -> Result<string,string>`（fs）
  - `get_env_guarded(name, t: SystemToken) -> Result<string,string>`（env）
  - t 为 SystemToken 能力门禁，被调用**消费**（每次授权一次受保护操作）；
    运行时忽略（能力由 checker/CFG 线性保证）；复用 read_file/getenv 核心。
- **接线**：checker（builtins 列表/arity + infer 双臂）+ interp（wrapper + 注册）
  + codegen（compile_*_guarded 复用核心 + 分发 + 可调用列表）。
- **语义**：无 token → E0242；调用后旧 token 绑定再使用 → E0304（已消费）；
  双后端一致。
- **迁移**：既有不收 cap 的 `read_file`/`getenv` 保持；guarded 变体为收 cap
  推荐路径（文档标注迁移方向）。
- **测试**：`src/tests/phase_d.rs` 16 例（+4：read_file_guarded 双后端、
  get_env_guarded 双后端、缺 token E0242、消费后复用 E0304）。
- lib 5643/0/7 全绿。

### 0.39.74 — SystemToken × 线性泛型 + 容器组合集成（token 基础收口）
- **集成验证**：SystemToken 与 Phase C 线性泛型面无缝协作——
  - `linear T` 泛型直通（transfer-only）双后端；
  - `List<SystemToken>` 构建 + 定向头提取（`v[0]`）+ 整体 drop 双后端；
  - `List<SystemToken>` 整体经 `linear T` 直通后仍可提取元素；
  - Free-T 泛型 → E0432（种类不匹配，token 线性面正确）。
- token 成为首个真实线性能力消费者，Phase C `linear T` 面 + Phase D token 面
  交汇闭环。
- **测试**：`src/tests/phase_d.rs` 12 例（+4 集成）。纯测试切片，无生产代码变更。
- lib 5639/0/7 全绿（首轮 parallel flake：loader_std_strings_plus_fs_merge 单测
  通过、重跑全绿，确认非本轮回归）。

### 0.39.73 — TokenChannel：SystemToken 跨通道转移（跨任务 move）
- **实现**：`token_channel_new() -> TokenChannel`、`token_channel_send(ch, t)`、
  `token_channel_recv(ch) -> SystemToken`——SystemToken 经通道整体转移：
  - checker：TokenChannel 进 builtin_type_names；infer 三臂（new 返 TokenChannel、
    send 检 TokenChannel+SystemToken、recv 返 SystemToken）；
  - resolved：TokenChannel 进 BUILTIN_NOMINALS（builtin:type:TokenChannel）；
  - interp/codegen：透传 channel i64 柄（token_channel_* 复用 channel_* 运行时）。
- **语义裁决**：TokenChannel **可 Copy 共享**（同 Channel）——只有 SystemToken
  线性。跨任务 move + 旧端失效由 SystemToken 承担：send 消费 t（move 入通道，
  旧绑定死 → E0304），recv 返回全新 SystemToken 义务（须消费）。同一通道可多
  token 往返。
- **CFG 实证**：初版把 TokenChannel 当线性 → `let ch2 = ch` 移动 + 借用通道
  的 liveness 检查缺口（tch6 假通过）；改可 Copy 后语义自洽（通道共享合法，
  token 线性独立）。
- **测试**：`src/tests/phase_d.rs` 8 例（新增 send/recv 双后端、use-after-send
  E0304、通道共享 Copy）。
- lib 5635/0/7 全绿。

### 0.39.72 — 线性 SystemToken 能力类型（move-only，旧端失效）
- **实现**：`make_token()` 现返回 **线性 `SystemToken`**（非 Copy、move-only），
  `token_id(t: SystemToken) -> i64` 消费 t 取唯一 id：
  - checker：`SystemToken` 进 `builtin_type_names`；`is_linear_surface_type` 识别；
  - infer：make_token → `Type::Name("SystemToken")`；token_id 臂（消费 t、返 i64）；
  - resolved：`SystemToken` 进 BUILTIN_NOMINALS（`builtin:type:SystemToken`）；
  - ir：`nominal_is_linear()` 认 `builtin:type:SystemToken` → CFG 作为线性资源追踪；
  - interp/codegen：token_id 透传 i64 柄；make_token 双后端仍返唯一 id。
- **语义（旧端失效）**：move 后旧绑定再使用 → E0304；弃置不消费 → E0304/E0256；
  显式 drop → 消费；跨函数整体转移 → 直通。均双后端验证。
- **命名裁决**：内置 token 型命名 **SystemToken**，避开用户 `cap Token` 声明
  （audit r4 别名 fail-closed 回归完好，E0407 保持）。
- **测试**：`src/tests/phase_d.rs` 5 例（唯一性双后端、drop、use-after-move E0304、
  弃置拒、跨函数 move 双后端）。
- lib 5632/0/7 全绿。

### 0.39.71 — Phase D 启动：`make_token()` 全局唯一 token id（cap/std 首切片）
- **规划**：`devdocs/v0.39/phase-d-plan.md`（0.39.71-90 切片清单、语义决策、
  爆炸半径、风险护栏）。
- **实现**：`make_token() -> i64` 全局唯一 token id——每次调用返回进程内单调
  计数器的不同 id：
  - checker `builtins.rs` 注册（is_builtin_callable + arity 0）；
  - infer `simple.rs` 零参工厂臂（返回 i64，与 channel_new 同款）；
  - interp `misc.rs` `builtin_make_token`（`TOKEN_COUNTER` AtomicU64）；
  - codegen `compile_make_token` + native runtime `mimi_make_token`
    （`MIMI_TOKEN_COUNTER` AtomicU64）。
- **语义决策**：唯一性 ≠ 线性——`i64` 是 Copy 型。线性 token 能力类型
  （move-only、跨任务 move、旧端失效）为 0.39.72-73 切片。
- **测试**：`src/tests/phase_d.rs` 2 例——唯一性（VM+native 双后端 `1,2,3,distinct`）、
  普通 i64 可用性。
- lib 5629/0/7 全绿。

### 0.39.64 — Phase C 收尾：文档/注释终态化 + L 合同定稿
- `dual_backend.rs` 旧 0.36.39「线性黑盒直通」注释块替换为 **Phase C 终态语义**
  （显式种类 `linear T`/`linear drop T`、Free T+线性一律 E0432、E0841 保留 +
  自递归支持、方法路径无洞）；测试内残留 "black-box" 措辞更新。
- `phase-a-plan.md` §8 BLACKBOX-REC-001 → ✅ 0.39.60 已修（附自递归信任说明）。
- `phase-c-plan.md` §6 **Phase C 结论**定稿：保留 E0841（P0-6 负结果）、终态语义
  表、达到效果、删 blackbox 主体降级长期项。
- lib 5627/0/7 全绿（本轮纯文档/注释，无行为变更）。

### 0.39.63 — P0-6 实证为负：CFG 不可替换 E0841（删 blackbox 策略重定）
- **实证**（0.39.63，测试先行）：修 `resource_lower::is_linear` 使 CFG 识别
  `linear T`/`linear drop T` binder 为线性资源（P0-6），并写正负集（CFG 把
  linear T 参数 Introduce 为资源 / Free T 不追踪）。立即被 **3 个承载性模式**
  拦下：
  1. match 通配消费（`match o { Some(x) => sink_g(x), _ => 0 }`）→ 假 E0256
     （CFG 把 `Option<T>` 当单资源，match 消解元素不映射回 o）；
  2. self-递归转移（`count_down<linear T>(x,n){ if n<=0 {x} else {count_down(x,n-1)} }`）
     → 假 E0304（CFG 视递归调用重复消费 / 仅部分路径消费）。
- **结论**：CFG 的线性追踪未覆盖 match 解构消费 + 递归直通——**不能**作为
  blackbox E0841 的替换。已**回退 P0-6**（恢复保守 GenericParameter→false）。
- **策略重定（0.39.63-70）**：保留 E0841 定义时体校验（sound + 已支持自递归，
  0.39.60）；blackbox 调用点路径已退役（0.39.59）；"删 blackbox 主体"从 Phase C
  路线图中**降级为长期项**（需 CFG match 解构 + 递归支持，超出 0.1.9 风险预算）。
  Phase C 剩余 = 回归 + 文档 + L 合同定稿。
- lib 5627/0/7 全绿（回退后）。

### 0.39.62 — trait 方法调用点线性实参种类检查（E0432 覆盖方法，收口 0.39.61 遗留）
- **实证**：trait 方法 dispatch（method.rs `type_methods` 路径）完全绕过 linear-arg
  检查——Free-T 泄漏方法体 + 线性实参静默弃值（0.39.61 登记的 pre-existing 洞）。
- **修复**：`check_method_linear_arg_kind`——trait 方法实参循环里对线性实参执行
  与 simple.rs / impl 方法同款规则：
  - Free-T 方法 + 线性实参 → E0432（种类不匹配 + 迁移提示）；
  - `linear T`/`linear drop T` 方法 kind 兼容放行（drop + SessionChan → E0432）；
  - 具体线性参数方法（`x: cap FileReadCap`）跳过（concrete 追踪处理）。
  - 隐式 self 偏移 + 简单 key（trait_args 空）；泛型 impl 多义 key 取不到 →
    保守 fail-closed。
- **测试**：`linear_kind_trait_method_free_t_linear_rejected`、
  `linear_kind_trait_method_free_t_session_rejected`（均 E0432）；既有 concrete
  cap 方法、linear T 方法（双后端）不回归。
- 至此方法路径 E0841（定义时）+ E0432（调用点）与顶层函数一致，方法线性安全
  无洞。
- lib 5627/0/7 全绿。

### 0.39.61 — 方法级 `linear T` 定义时体校验（E0841 覆盖方法，修 soundness 洞）
- **实证**：`linear T` 方法体（`impl Wrap for Rec { func leak<linear T>(x) -> i32 { 0 } }`）
  静默弃值——方法完全绕过 E0841（func_generics 未注册方法泛型 + check_func 只处理
  顶层函数）与调用点 E0432（方法调用 generics 空 → 检查跳过）。
- **修复**：
  - `Item::Impl` 方法泛型注册入 `func_generics`（含 kind：`linear T`/`linear drop T`）；
  - `check_linear_kind_param_bodies` 提取共享（func.rs），impl 方法路径调用；
  - 隐式 self 偏移：funcs 签名 self@0，AST params 无 self → funcs_index = index + offset
    （顶层 0 / 隐式 self 方法 1）；
  - 定义时分析改用 `linear_kind_body_sound`（直接给 AST 参数 + 体，不经
    `find_func_def_ast`——它只命中顶层函数）。
- **测试**：`linear_kind_method_leaky_body_rejected`、`linear_kind_method_partial_path_rejected`
  （均 E0841）；既有 linear T 方法直通（双后端）不回归。
- **遗留（已登记，非本切片引入）**：Free-T 方法调用点 E0432 仍被绕过（trait 方法
  dispatch 早退，不经 generic 参数检查）——Free-T 泄漏方法体 + 线性实参仍可静默
  弃值（pre-existing）。需专门切片修 trait 方法 dispatch 路径。
- lib 5625/0/7 全绿。

### 0.39.60 — BLACKBOX-REC-001 关闭：线性种类自递归 + P0-6 实证（Phase C 前置）
- **自递归信任**（BLACKBOX-REC-001 关闭）：`call_transfer` 中若 visiting 守卫含
  `{callee}#` 前缀（正在分析自身），则委托给自身的递归分支视为 transfer-out——
  基例（非递归路径）仍由外层分析强制消费，按归纳健全。此前 fail-closed 误报
  `count_down<linear T>(x,n){ if n<=0 {x} else {count_down(x,n-1)} }`。
- **正负集**：`linear_kind_self_recursion_*` 3 例——transfer 自递归（双后端）、
  `linear drop T` 自递归 + drop 基例（双后端）、递归基例弃置 → E0841。
- **P0-6 实证（关键发现，塑造 0.39.62-65 删除策略）**：`resource_lower::is_linear`
  对 `GenericParameter` 返回 **false（保守）**——CFG **不**把 `linear T` 参数当线性
  资源追踪。故 blackbox 的 E0841 **非冗余**：删 blackbox 定义时路径前必须先修
  P0-6（让 CFG 认识 linear 种类参数，E0256 兜底泄漏/弃置），否则无声漏检。
- lib 5623/0/7 全绿。

### 0.39.59 — Free `T` + 线性实参一律 E0432（退役调用点体分析，Phase C 切片）
- **语义**：`linear T`（transfer-only）/ `linear drop T`（drop-tolerant）是接线性
  实参的**唯二显式种类**。Free `T` + 线性实参 → 一律 E0432（种类不匹配 + 迁移
  提示），不再做调用点 blackbox 体分析。P 合同不变（仍 E0432）。
- **实现**：
  - simple.rs / method.rs：Free T + 线性实参路径从 `generic_linear_blackbox_sound`
    改为无条件 E0432「Free generic parameter `T` may only instantiate to non-linear
    types (kind mismatch)…」。
  - 定义时 E0841 修复：`param_type_refs_linear_kind` **不深入函数类型**——可调用值
    非线性资源，`foldT<T>(xs, f: func(T)->i32)` 的 f 不再被误检 E0841（0.39.59
    实证：22 例 dual_backend 迁移时 foldT/host 3 例因此失败）。
- **测试迁移**（Free T + 线性 → 新种类）：
  - contract_p：identity 系列 → `identity<linear T>`；dropit 升格 L 合同（E0432）；
    新增 `p_contract_free_t_linear_always_rejected`。
  - linear_kind：`linear_kind_free_t_linear_rejected_linear_t_passes` 取代旧
    unmarked-pass-through；`pass_list<linear T>`。
  - dual_backend：22 例 `dual_generic_linear_*` → `linear T`（转移）/ `linear drop T`
    （drop-tolerant，foldT/host 3 例）。
  - drop_face：6 正集迁 `linear drop T`/`linear T`；partial-path 负集改 E0841。
- lib 5620/0/7 全绿。退役调用点体分析 = blackbox 仅剩定义时 E0841 一项职责。

### 0.39.58 — `linear drop T` 种类（Phase C drop 面裁决 = 候选 (a) 落地）
- **决策**（0.39.57 正负集完成后）：drop-tolerant 泛型面用**显式新种类**
  `linear drop T` 表达（可 drop 亦可转移；实例化必须可 drop），非精简 blackbox。
  完成 drop_face 负集（SessionChan drop → E0432；单路径 drop → E0432）——
  原 `channel_new()` 返回可 drop 的 `Channel<i64>`（非 Session）系误报，已澄清。
- **实现**：
  - AST `GenericKind::LinearDrop` + parser `linear drop T`（`linear` 后接 `drop`
    关键字；`drop` 是 TokenKind::Drop 非 Ident）。
  - checker：`linear_kind_generic_names` 并入 LinearDrop（kind 兼容放行）；
    `param_uses_linear_drop_kind` 区分 drop-tolerant；定义时 E0841 用
    allow_drop=true（可 drop 但每路径必须消费）。
  - 调用点：`linear drop T` 实例化 SessionChan（或任意嵌套）→ E0432（可 drop
    约束违反，提示改用 `linear T` transfer-only）。
- 正集：drop cap（双后端）、transfer（双后端）；负集：SessionChan 实例化拒。
  linear_kind 现 39 例。
- 意义：drop 面成为显式种类，为 0.39.59-61 Free-T 退役 + 0.39.62-65 blackbox
  删除铺平（22 个 dual_generic_linear_* 的 `sink_g<T>{drop}` 可迁 `linear drop T`）。

### 0.39.56 — Phase C 规划 + Drop 面正负集（阶段过渡切片）
- **Phase C 规划**：`devdocs/v0.39/phase-c-plan.md`（0.39.56–70 切片清单、语义
  决策、爆炸半径、风险护栏）。
- **实证裁决（0.39.57 前）**：曾试推「Free T + 线性实参一律 E0432」切片，被
  **22 个 `dual_generic_linear_*` 测试**拦下——`sink_g<T>{ drop(x) }` 在泛型
  循环/if-let/match 内消费线性元素是承载性真实模式。**结论**：Phase C 首切片
  不是 Free-T 退役，而是先裁决「整体 drop 可表达性」（(a) `linear drop T` vs
  (b) 精简 drop-only 泛型面）。已回退代码改动、修订计划。
- **Drop 面正集落地**（0.39.56）：`src/tests/drop_face.rs` 6 例固化 drop-tolerant
  泛型面（简单 drop / 循环消费 / if-let / match 通配 / 容器整转 / let-sink），
  双后端全绿——作为 0.39.57 裁决的基线。

### 0.39.37 — SET-REMOVE-CODEGEN-001 闭合：resolved codegen 全部 Set 方法（Phase B 切片 7）
- **根因**：Set 方法（`s.size()`/`s.remove(v)`/`s.is_empty()`/`s.contains(v)`/
  `s.insert(v)`/`s.to_list()`）在 resolved lowering 以
  `ResolvedCallee::Builtin("builtin.method.set.*")` 到达 codegen，但只有
  `ProtocolMethod` 形式才走到 `emit_builtin_set_protocol_method` → Builtin 形式
  全落 `compile_builtin_call` 兜底 → E0709（`s.size()`/`s.remove(1)` 皆然，
  checker/VM/legacy 均正常）。
- **修复**：
  1. resolved `ResolvedCallee::Builtin` 臂增 `builtin.method.set.*` 分支，路由到
     set 协议处理器（镜像 `builtin.method.string.*`/`session.*` 模式）。
  2. set 值实参按 `mimi_set_*` 的 `(i64,i64)` 签名做位宽扩到 i64（`s.remove(1)`
     的 i32 字面量曾致 LLVM 签名不匹配 E0713）。
- 效果：resolved codegen 下 `s.size()`/`s.remove(1)`/inline `is_empty(s.remove(1))`
  等全部原生通过；`mimi build`（resolved）与 VM 等价。
- 锁定：`set_method_matrix_resolved_dual_backend`（linear_kind 现 36 例；to_list
  无序显示按 `len` 断言，跨后端不锁元素序）。

### 0.39.36 — `is_empty` Map/Set codegen（Phase B 切片 6, E0700 部分收口）
- `is_empty(map)`/`is_empty(set)` 原生 codegen 收口：Map 与 Set 运行期都是裸 i64
  handle，无法凭值区分 → 新增 `pending_is_empty_kind`（调用点按推断类型分类
  map/set，legacy `infer_object_type` + resolved `resolved_type_display_name` 双路
  设置），`compile_is_empty` 按 kind 调 `mimi_map_size`/`mimi_set_size` 判空。
- 回归：`is_empty(map_new())`→empty、非空 map→nonempty、`is_empty({1,2})` set→
  nonempty（修复前误走 mimi_map_size → 原生错报 empty）。
- 锁：`is_empty_map_codegen_dual_backend` / `is_empty_set_codegen_dual_backend`
  （linear_kind 现 35 例）。lib 全绿。
- **登记 SET-REMOVE-CODEGEN-001（待修）**：resolved codegen 下 `s.remove(1)` 结果
  再喂自由 builtin（is_empty/len）→ E0709（set.remove callee Builtin vs
  ProtocolMethod 分叉；checker/VM/legacy 均正常），详见 phase-a-plan §9。

### 0.39.35 — 深化单态化锁定 + BLACKBOX-REC-001 登记（Phase B 切片 5）
- 锁定（全部双后端等价）：
  - 跨函数链 `wrap<linear T>{id(x)}`（cap 直通）；
  - trait 方法 + `linear T`（`r.keep(x)`，keep: T->T）；
  - 容器过链 `wrap2<linear T>(List<T>) { id(x) }`（List<cap> 直通）；
  - SessionChan 直通 `echo<linear T>`（transfer-only 通道转移）。
  → linear_kind 现 33 例。
- **登记 BLACKBOX-REC-001（Phase C 修）**：泛型线性递归被 blackbox fail-closed 误拒
  （`count_down<linear T>(x,n){ if n<=0 {x} else {count_down(x,n-1)} }` 语义健全但
  E0841/E0432）。名字级 blackbox 不追踪递归 → 属 Phase C「换黑盒」收口项，详见
  phase-a-plan §8。

### 0.39.34 — RECORD-LIN-001 修复：用户记录含 cap 字段按线性追踪（Phase B 切片 4, HIGH 闭合）
- **根因**（上轮登记）：`is_linear` 对用户记录走 `Nominal.is_linear` 标志（由
  `nominal_is_linear()` 决定，只认 `state:`/SessionChan）→ 含 cap 字段的记录
  is_linear=false → `let r = Plain { data: c }; drop(r); drop(c)` 双消费被接受
  （健全性缺口）；`drop(r)` 单独用又 E0256 泄漏。
- **修复**：
  1. `resolved/mod.rs`：`compute_linear_record_names` — 遍历 Record/Union type_def，
     经 `field_ids` + `resolved_field_types` 递归判字段线性（cycle-guarded），得
     线性记录 qualified_name 集。
  2. `cfg/resource_lower.rs`：`analyze_resolved_bodies` / `ActionEmitter` 增
     `linear_record_names`；`is_linear` Nominal 臂改为「线性记录名 ∪ 线性实参」，
     用户记录（如 `Plain`）与 `List<cap>`/`Tuple<cap>` 同权追踪。
  3. `codegen/expr/record.rs`：记录字面量构造移动 cap 字段（`consume_cap` +
     `is_cap_consumed` 幂等守卫，与 0.39.32 列表字面量同款）→ 原生 E0303 关闭。
- 效果：`drop(r); drop(c)` / `drop(c)`（移入后）→ E0304；`bind→consume` 双后端绿。
- 回归：`record_lin_cap_field_double_drop_rejected` /
  `record_lin_cap_field_use_after_move_rejected` /
  `record_lin_cap_field_bind_then_consume_dual`（linear_kind 现 29 例）。

### 0.39.33 — 双线性参数单态化锁定 + RECORD-LIN-001 登记（Phase B 切片 3）
- 锁定：`swap2<linear T, U>`（T=cap, U=i32）整体转移双后端等价（42）——
  `linear_kind_two_linear_params_dual_backend`（linear_kind 现 26 例）。
- **登记 RECORD-LIN-001（HIGH，Phase C/D 修）**：用户记录含 cap 字段不被线性追踪
  （`nominal_is_linear` 只认 state:/SessionChan）→ `let r = Plain { data: c }; drop(r);
  drop(c)` 双消费被接受（健全性缺口）。根因 + 修复位置见 `devdocs/v0.39/phase-a-plan.md §7`。
  对照：`List<cap>`/`Tuple<cap>` 正确（is_linear 显式处理）；`linear T` 记录在泛型面
  由 0.39.9-14 覆盖，具体面 cap 记录逃逸在 is_linear 处。
- 临时缓解：记录 cap 走 List/Tuple/Map，或具体化函数直收 cap 参数。

### 0.39.32 — 列表字面量 cap 移动：绑定 `let fs = [c]` 后 drop 不再假 E0303（Phase B 切片 2）
- **根因**：legacy codegen 的 cap 追踪器在 `compile_list_expr` 不标记元素消费
  （仅调用实参可达收集处理内联 `sink([c])`）→ 绑定 `let fs = [c]; drop(fs)` 的
  `c` 永不消费 → 原生 E0303；checker（CFG Move 在列表字面量处）与 VM 均正确。
- **修复**：
  - `store_list_elements`（record.rs）：列表字面量移动时消费元素可达 cap
    （复用 `collect_arg_cap_places`）。
  - 幂等守卫：`scope.rs` 新增 `is_cap_consumed`；调用实参 / 方法实参 / 返回
    表达式的可达收集改为 `is_cap_var && !is_cap_consumed` 才消费——避免
    `sink([c])` / `pass_list([c])` 在构造已消费后二次 `CapConsumed`。
- 回归：`linear_kind_list_literal_cap_binding_drop_dual`、
  `linear_kind_list_literal_inline_call_arg_no_double_consume`、
  `linear_kind_list_literal_binding_through_generic_chain`（linear_kind 现 25 例）。

### 0.39.31 — `is_empty` 自由 builtin codegen 补齐（E0700 关闭，Phase B 切片 1）
- LEN-READ-001 的 checker 豁免开放了 `is_empty(线性容器)`，但 codegen 一直缺
  （E0700「not yet implemented in codegen」，因 builtin 分发无 `is_empty` 分支）。
- **落地**：
  - `compile_is_empty`（list/access.rs）：List（指针/结构体读 len==0）、String
    （C 串首字节 / {ptr,i64} 结构体 byte_len==0）；Map/Set 是裸 i64 handle 值层
    无法区分 → 保持 Unsupported（无回归）。
  - `builtins/mod.rs` 分发加 `is_empty`；legacy（simple.rs）与 resolved（mod.rs）
    调用点对 `is_empty` 也设 `pending_len_is_string`（与 len 同款）。
- 回归：`linear_container_read_is_empty_then_drop` 升级全双后端；
  `is_empty_free_builtin_codegen_dual_backend`（线性 List/空 List/String 三态
  VM/native 等价）。linear_kind 现 22 例。

### 0.39.21–30 — 单态化前置 + drop glue 对齐（Phase A 切片 6，收尾）
- 验证 + 锁定（线性 kind 不改变单态化/drop 机制，机制本身类型驱动）：
  - **单态化**：`swap2<linear T>` 同一泛型双实例化（cap + i32）VM/Resolved 双后端
    等价（输出 30）——`linear_kind_monomorphization_multi_instantiation`。
  - **drop glue 恰好一次**：`List<cap>` 3 元素 / `Box<cap>` 记录整体转移后 drop，
    双后端等价——`linear_kind_drop_glue_once_infected_list` /
    `linear_kind_drop_glue_once_infected_record`。
  - **blackbox×kind 审计**：全库仅 5 个 `generic_linear_blackbox_sound` 调用点
    （simple.rs×2 / method.rs×2 调用点 + func.rs 定义时），均正确区分——
    `linear T` 定义时 transfer-only（allow_drop=false）且调用点 kind 放行；
    Free `T` 仅走调用点 blackbox（E0432 迁移）。无矛盾。
- linear_kind 现 21 例。

### 0.39.17–20 — 错误码完善：E0432 保 P 合同迁移码 + 消息带 `linear T` 迁移提示（Phase A 切片 5）
- **设计决策**：E0432 是 P 合同冻结迁移码（contract_p 12 例 + dual_backend 30+ 例 +
  audit_fix 全锁），re-code 到 E0842 会撕裂冻结合同——故不引入 E0842 分码，改为
  在 E0432 消息内追加迁移提示（三选一改写建议 + 原因）。
- 落地：`simple.rs` / `method.rs` 调用点 blackbox 拒绝消息改为——
  「非整体转移（线性值在 callee 内泄漏/弃置）。迁移：① 声明 `linear T` +
  transfer-only 体（pass T through）；② 具体函数签名直收线性型；③ 保
  pass-through/drop-only 泛型体」。
- 回归：`p_contract_e0432_message_carries_migration_hint`（contract_p 现 13 例）。

### 0.39.16 — LEN-READ-001 修复：线性容器读指标（len/is_empty/map_size/keys）按借用不消费 (Phase A 切片 4)
- **根因**：自由只读指标 builtin（`len(fs)`/`is_empty(fs)`/`map_size(m)`/`keys(m)`）在
  `resource_lower` 通用调用实参消费循环被当作 Move → 线性容器被 `len(fs)` 消费，
  `let n = len(fs); drop(fs)` 假 E0304 双消费；而方法形 `fs.len()`（Permission::View）
  不受影响，形成语义分裂。
- **修复**（`resource_lower.rs` Call 臂）：`ResolvedCallee::Builtin` 名在
  `len | is_empty | map_size | keys` 集合 → 跳过整组实参消费（按借用，与 View 方法同款）。
  变换/消费 builtin（push/pop/map_set…）不受豁免，行为不变。
- 回归：`linear_container_read_len_then_drop`（len 双后端）、
  `linear_container_read_is_empty_then_drop`（checker 级；is_empty 尚缺 codegen）。
  linear_kind 现 18 例。

### 0.39.15 — M-ARG-001 修复：方法实参线性转移（View/Mutate 只借 receiver，非 self 实参仍移动）(Phase A 切片 3)
- **根因**：`implicit_self_param` 给方法默认 `Mutate` 借 self → `ResolvedCall.permission
  = Mutate`，`resource_lower` 的调用实参消费循环整组跳过 View/Mutate 调用 → `r.take(c)`
  （take: cap 参数）在调用方从不消费 `c` → 假 E0256；`linear T` 方法接收者同样受阻。
- **修复**（`resource_lower.rs` Call 臂）：View/Mutate 只跳过**接收者**（arguments[0]），
  其余非 self 实参仍 `Move` 进 callee。自由调用 permission=None/Consume → 行为不变。
  对 Mutate 容器变换（0.36.47/0.36.48 专用分支已 Move 全部线性 `Load` 实参）额外
  跳过这些实参，避免 E0304 双消费（`dual_linear_cap_method_arg_transfer_ok` 回归锁定）。
- 回归：`method_arg_cap_transfer_consumes_at_caller` +
  `linear_kind_method_receiver_with_linear_arg_dual_backend`（linear_kind 现 16 例）。
- 效果：`r.take(c)`（concrete cap）、`r.pass(c)`（linear T）现 check + 双后端同跑。

### 0.39.10–14 — 感染：容器/记录字段 kind 流锁定（Phase A 切片 2）
- `linear T` 经容器/记录/嵌套整体转移的**感染**行为锁定（0.39.9 的
  `param_uses_linear_kind` 用 `type_any` 递归天然覆盖容器/记录/Option 嵌套）：
  - 正例（双后端同跑）：`pass_list<linear T>(xs: List<T>)`、
    `pass_box<linear T>(b: Box<T>)`（`type Box<linear T>` 记录字段感染）、
    `pass_opt<linear T>(o: Option<T>)`、`pass_nest<linear T>(xs: List<Box<T>>)`。
  - 反例（定义时 E0841）：`take<linear T>(b: Box<T>) -> T { b.data }` 记录字段投影、
    `first<linear T>(xs: List<T>) { xs[0] }` 容器投影。
  - 回归 `linear_kind` 现 14 例。
- **登记两既有 checker gap**（非本版本引入，独立切片修，见 phase-a-plan §6）：
  M-ARG-001 方法实参线性转移缺（`r.take(c)` → E0256）；LEN-READ-001 线性容器
  `len()` 读借用后 drop 判 E0304。二者不影响本切片交付，但 0.1.9 Phase D
  （cap/std 接线）前必修。

### 0.39.9 — `linear T` 种类语义：定义时 transfer-only 体校验 + 调用点 kind 放行（Phase A 切片 1）
- `linear T` 从纯语法记录升级为**显式线性种类**：
  - **定义时**（`check_func`）：对引用 `linear T` 的参数位置跑 `linear_blackbox`
    transfer-only（allow_drop=false）体校验一次；不健全 → 新错误码 **E0841**
    （投影 / 弃置 / `drop(T)` 在函数定义即拒，span=函数）。理由：`T` 可能
    实例化为 Session，drop(T) = E0425 弃置。
  - **调用点**（`simple.rs` / `method.rs`）：`linear T` 参数对线性实参 kind 兼容
    **直接放行**（不再依赖调用点 blackbox）；Free `T` 仍走迁移 blackbox
    （P 合同不删直通）。
  - 语义分化可观察：`pass<linear T>{ x }` 双后端同跑；`sink<linear T>{0}` /
    `dropit<linear T>{ drop(x);0 }` / `first<linear T>{ xs[0] }` 定义时 E0841。
- 新增 E0841 到 codes.rs（const + describe + 注册）。
- 回归：`linear_kind` 现 9 例（含 4 个定义时反例 + 调用点正例）；`contract_p`
  12 例（Free 迁移合同）不变；lib 全量绿。
- 本地规划：`devdocs/v0.39/phase-a-plan.md` 切片 0.39.9 完成。

### 0.39.2 — P 合同扩展 + `linear T` 种类语法（0.1.9 Phase 0/Phase A 基础）
- **P 合同扩展**（`src/tests/contract_p.rs`，现 12 例）：正例覆盖嵌套线性容器
  `identity(List<cap>)` / `identity(Option<cap>)` / `identity(List<List<cap>>)`
  （双后端同跑）+ 泛型体整体 `drop(x)`；反例覆盖元组投影 `t.0`、
  `Option::unwrap` 解包（均须 E0432 拒）。
- **`linear T` 种类语法**（Phase A slice 0.39.2，`src/tests/linear_kind.rs`）：
  - `GenericParam` 新增 `kind: GenericKind`（`Free` 默认 / `Linear` 显式）。
  - `linear` 是**上下文软关键字**：仅 generic 参数位置识别为 kind 标记，
    其余位置仍是普通标识符（沿用 `parasteps`/`fault`/`reset`/`recover` 先例，
    不加 TokenKind、不动关键字计数钉）。
  - `func pass<linear T>(x: T) -> T { x }` 现可解析 + check + 双后端同跑；
    `linear T` 投影 `xs[0]` 仍 E0432 拒。
  - 语义强制（Free/Linear 种类不匹配、`List<T>`/记录字段感染、删特判网）属
    后续 Phase A 切片；当前行为仍由 `linear_blackbox` 支配（整体转移可过）。
- 本地记录：`devdocs/v0.39/contract-p.md`。

### 0.39.0 — P 合同冻结：整体转移合同规范正负例（0.1.9 Phase 0）
- 把最终裁决 Q2 的整体转移句子钉成规范测试（`src/tests/contract_p.rs`）：
  **整体转移才允许未标 kind 的 `T` 接线性实参**。
  - 正例 `identity(cap)`：check 过 + 双后端（VM + compile_checked native）同跑
    （`p_contract_identity_cap_passes_check` / `..._dual_backend_runs`）。
  - 反例 `first(xs[0])`（元素提取）、`discard`（泛型体弃置线性实参）：
    必须 E0432 拒（`p_contract_first_element_extraction_rejected` /
    `p_contract_discard_rejected`）。
- 现状：当前 `linear_blackbox` 已满足本合同（identity 过、first/discard 拒）。
  换实现（上 `linear T` 种类、删特判网、单态化、drop glue）时本组必须仍绿。
- 本地合同记录：`devdocs/v0.39/contract-p.md`。

### 0.38.122 — 23 项历史失败基线清零 + 门禁修复（0.1.8 收官）

### 0.38.115 — codegen_mod F1：逃逸 List<string> claim 跨嵌套 scope 持久化 (L3)
- 修复 `codegen/mod.rs` 堆 scope 释放中 `claimed_returned_string_lists` /
  `claimed_returned_string_list_lists` 的不对称：此前每次嵌套 scope flush
  （`free_heap_allocs` / `emit_frees_for_top_scope`）都 `mem::take` 抽干且从不
  恢复，而对称的 closure-env claim（`claimed_returned_envs`）会恢复；free 循环还
  把空字符串列表 claim 传给 `emit_guarded_scope_free`，导致外层 scope 注册的逃逸
  `List<string>` 在任意内层 scope 弹出后丢失守卫，可能被释放 → UAF / 双释放。
- 修复：使字符串列表 claim 与 env claim 对称——存入局部变量、传给 guarded free、
  并在 `heap_allocs.len() > 1` 时恢复（直到函数级 scope 弹出）。严格更保守（只会
  增加守卫，绝不减少）。
- 新增 `MIMI_ASAN=1` AddressSanitizer 验证通道（`src/main/build.rs`，门控环境变量）：
  为 runtime `rustc` 加 `-Z sanitizer=address`、为最终 `cc` 链接加
  `-fsanitize=address`，使原生编译的 Mimi 程序可在 ASan 下检验堆安全。正常构建
  不受影响。
- 回归：`audit_codegen_mod_f1_escaped_list_string_through_loop` /
  `audit_codegen_mod_f1_escaped_list_string_through_inner_block`
  （`src/tests/audit_fix_codegen_mod.rs`）双后端锁定逃逸 `List<string>` 跨嵌套
  scope 完整交付。

### 0.38.116 — interp F2：FFI 回调 string 参数默认借用（不再 free）(L3)
- 修复 `src/interp/ffi/helpers.rs` 的 `compute_arg_free_mask`：此前对**所有**
  `string` / `CBuffer` 回调参数返回 `true`，导致 trampoline 无条件 `libc::free`
  传入的 C 字符串指针。C 回调几乎总是传**借用**的 `const char*`（字符串字面量、
  静态缓冲、`strdup` 后库仍持有的指针），free 它们即堆破坏 / 崩溃（HIGH）。
- 修复：回调入站方向（C→Mimi）无 borrow/owned 区分，安全默认是**借用**——
  解码已把字节拷进 `Arc<String>`，Mimi 持有自己的值、绝不 free C 侧指针。
  `compute_arg_free_mask` 现恒返回全 `false`；未来应以显式 owned 标记
  （`string_owned` / `#[transfer]`）恢复精确释放。严格更安全（只消除崩溃，
  不引入双释放）。
- 回归探针 `test_callback_str`（runtime `ffi_test.rs`）：用 `.rodata` 静态字面量
  调回调，pre-fix 触发 `free(): invalid pointer` / SIGABRT，post-fix 回调正确
  收到 `"borrowed_static"` 且程序不崩。
   `interp_ffi_callback_static_string_not_freed`
   （`src/tests/ffi_interp_e2e.rs`）双端（pre/post 已证伪）锁定。

### 0.38.117 — interp F3：重入 FFI 崩溃的 `FfiGuard` 泄漏→死锁修复 (L3)
- 修复 `src/interp/ffi/ffi_runtime.rs` `call_extern` 在受保护 FFI 调用期间持有的
  RAII 资源（核心为 `ffi_guards: Vec<FfiGuard>`，内含 `RwLock` 读/写锁守卫）在
  **重入 FFI 崩溃**时泄漏的问题。机制：当 C 回调 → Mimi → 嵌套 FFI（B）在外层
  受保护调用（A）的 `siglongjmp` 边界内崩溃时，外层 `call_guarded` 的
  `siglongjmp` 跳过 B 的整个栈帧（含其 `ffi_guards`），`RwLock` 守卫永不释放 →
  后续访问该 `Value` 死锁（HIGH）。
- 修复：新增线程局部「崩溃清理栈」`CRASH_CLEANUP`（`signal_guard.rs`：
  `crash_cleanup_push` / `crash_cleanup_base` / `crash_cleanup_drain_above` /
  `CrashCleanupPopper`）。`call_extern` 在崩溃危险区前把 `ffi_guards` 装箱推入该栈，
  正常退出由 `CrashCleanupPopper` 弹出释放；重入崩溃时由外层 `call_guarded` 的恢复
  路径 `crash_cleanup_drain_above(base)` 排空被跳过帧（index ≥ base）的资源。
  `base` 边界确保外层帧自身资源不被误释放（绝不双释放）。
- 主路径（非重入）本就安全：`ffi_guards` 位于 `call_extern` 局部、`call_guarded`
  闭包之外，`call_extern` 帧不被 `siglongjmp` 跳过，正常 drop。
- 回归：`crash_cleanup_drains_skipped_reentrant_frame` /
  `crash_cleanup_preserves_outer_frame_resources`
  （`signal_guard.rs` 白盒测试）分别锁定「被跳过帧资源在恢复时释放」与「外层帧
  资源不被误释放」。既有 11 个 `signal_guard` 测试 + 92 个 `ffi_` 测试全绿。

### 0.38.118 — core_resolved F1：`from_flow_acc` 后缀回退防误绑 (L2 consistency)
- 修复 `src/core/resolved/mod.rs` `from_flow_acc` 的 checker→resolved 签名对接
  workaround：旧实现用 `find_map` 在 `m::A::run` → `A::run` → `run` 的**逐步缩短
  后缀**上取首个命中，且**仅以参数量**做护栏，导致「同后缀 + 同参数量 + 不同类型」
  的签名被静默误绑（错误函数的签名流入 resolved IR 与下游 codegen/interp）——一个
  soundness 漏洞（审计 `devdocs/audit0820/core_resolved.md` F1，HIGH）。
- 现状核实：checker 与 resolved 目录现已对所有 callable 种类（普通函数、actor 方法、
  transition 合成方法、impl 方法、嵌套函数）达成一致**模块限定键**，故在合法程序中
  精确键恒为权威命中，旧 `find_map` 实际等价于精确命中（误绑在合格程序中是 latent 的）。
  但脆弱的回退本身仍是隐患，须消除。
- 修复：将解析抽取为 `resolve_zonked_signature`（含 `ZonkedResolution` 枚举），语义：
  * 节点 id 命中（嵌套 callable）/ 精确限定名命中 → 权威，静默应用；
  * 后缀回退仅在**恰好一个**候选存在时采用；多个不同后缀候选 → **fail-closed**
    （`TOOL-RESOLUTION-001`）而非 last-wins 误绑；
  * 单一后缀命中仍须与声明**返回类型一致**（两边均为具体类型时）才应用，否则
    fail-closed，杜绝同后缀异类型误绑。
- 回归：`audit_core_resolved_f1.rs` 内 4 个 `resolve_zonked_signature` 白盒单测
  （模糊后缀→失败闭合、精确命中权威、单一后缀接受、嵌套节点 id 命中）+ 1 个
  `core_resolved_f1_actor_method_vs_same_named_top_level_dual`（actor 方法 `A::run`
  必须绑定自身 transition 签名 `T`，而非同名顶层 `run() -> i32`；resolved IR 直接
  断言 + VM 运行时锁定）。

### 0.38.119 — CHK-F02：`Any` 收窄为单向（bottom）unify，消除调用点类型混淆 (L2)
- 修复 `src/core/unification.rs` 的 `Any` 双向 unify（`(Any, _) | (_, Any) => Ok`）：
  旧实现让 `Any` 同时充当 top 与 bottom，任何 `Any` 类型的值都能静默冒充任意具体类型
  （审计 `devdocs/audit0820/*` CHK-F02，HIGH 类型混淆）。
- 收窄为**单向（bottom）**：`Any` 在左侧（`(Any, T)`）恒 `Ok`——可向下流入任意具体类型，
  故 `map_get` 返回的 `(bool, Any)` 仍可当具体值用、异构 map/set 的 `Any` 形参仍接受任意实参；
  **唯一被拒绝的方向是把具体值沉入 `Any` 槽**：`(T_concrete, Any)` → `Err`，即 `Any` 类型的值
  不能再传给期望具体类型的形参/绑定（调用点类型混淆闭环）。`Any` 仍与类型变量 / `Infer` /
  自身 / `_` 统一，内部推断不受影响。
- 取舍：pure TOP-only（`Any` 作 supertype、拒绝 `(Any, T)`）曾考虑，但因 stdlib 异构
  map/set 在 `Record` 无类型表示上同时用 `Any` 作形参与返回值，top-only 会破坏
  `set(m, k, concrete)` 及整个 maps/set 模块；bottom-only 是唯一保留该设计且 sound 的单向语义。
- 回归：`src/core/unification.rs` 内 `chk_f02_any_is_bottom_only` /
  `checked_unify_rejects_nested_any_sink`（原 `checked_unify_allows_nested_any` 反向，因它
  原编码了双向漏洞）/`checked_never_allows_escape_on_either_side`（仅保留 `_`/`Infer` 拒绝）；
  `src/tests/audit_chk_f02.rs` 三测：`chk_f02_any_value_rejected_as_concrete_param`、
  `chk_f02_any_value_rejected_as_concrete_let_binding`（PoC：Any 值沉入具体类型被拒）、
  `chk_f02_any_flows_down_as_concrete_value`（伴随：Any 仍可向下当具体值用，map_get 习惯保留）。

### 0.38.121 — VER-F1 复核：假证在当前树不可复现，落锁 sound 行为回归 (L2 诚实)
 - `audit0820` VER-F1（CRITICAL）主张验证器 `old(param)==param` 无条件下断言且
   守门只拦 checked 算术，普通 `x=y` 绕过 → `ensures` 假证。实测多组 `mutate` 参数
   RMW + `old()` 合约 PoC：标量 `x=x&1/x|1/x^7/x<<1`（建模→`SolverUnknown`）、
   `x=-x`（守门→`NotInTrustedSubset`）、记录字段交换 `p=Rec{a:p.b,b:p.a}` +
   `old(p.a)`（不可建模→`NotInTrustedSubset`）、`x=x+1`（checked 守门）、
   `x=x`（恒等→正确 `Proven`）；审计原 PoC `x.sorted()` 依赖已删除的 List 方法、
   自由函数 RMW 被 checker 拒。结论：验证器现已（a）符号执行标量赋值、（b）对不可
   建模构造 fail-closed，具体假证在 0.1.8 当前树**不可复现**，疑似已被后续验证器
   升级缓解。
 - 落锁：`ver_f1_no_false_proof_on_changing_reassignment` 断言任何"改变值的重赋值
   + old() 合约"绝不返回 `Proven`；`ver_f1_identity_reassignment_is_correctly_proven`
   控制组确认恒等重赋值可正确 `Proven`（`src/tests/audit_ver_f1.rs`）。回归若复现
   "赋值被忽略" 即被捕获。

### 0.38.122 — 23 项历史失败基线清零（stale 测试预期 + golden 重生成）(测试面)
- 收口 `audit0820` 有意修复后遗留的 23 项陈旧测试预期，`cargo test --lib` 从
  `5536 passed / 23 failed / 7 ignored` 清零为 **`5559 passed / 0 failed / 7 ignored`**
  （= 5566 总数，基线名实相符）。四类：
  1. **codegen_golden ×20**：`codegen_expr F1`（0.38 wave-2，bcf9e590）把字符串
     字面量从裸 `i8*`+运行期 `strlen` 改为编译期长度 + `{ptr,i64}` fat 表示，20 份
     golden `.ir` 未跟随。用 `UPDATE_GOLDEN=1` 重生成对齐（双后端行为测试全绿，
     证明 codegen 输出正确，纯表示层对齐）。
  2. **ownership_checker_tracks_actor_method_capabilities**：ACT-F2（86f6c49c）
     把线性 cap 进 actor 邮箱从「方法末 E0256 未消费」升级为「邮箱边界 E0432 硬拒」，
     断言同步 E0256→E0432。
  3. **fix5_session_residuals_do_not_bleed_between_methods**：ACT-F2 使
     `SessionChan` 不能进 actor 邮箱（E0432），改用普通函数验证每函数会话残差
     隔离（`leaky`→E0425 点名 ch1；`clean` 完成协议无 ch2）。
  4. **rust_binding_smoke**：FFI-01（8e55e6a8）已声明 `ffi_raw::malloc`，断言笔误
     `*mut c_void)` 改为生成输出一致的 `*mut c_void;`。
- 全部为既有测试预期修正，零编译器/运行时行为变更；`real_world`/stress/dogfood
  不涉及本切片改动面（仅 lib 内测试与 golden）。

### 0.38.120 — audit 复核：parser F-01 为 false-positive（可选链左结合即正确语义）
- `a?.b.c` 解析为 `Field(OptionalChain(a,"b"),"c")` = `(a?.b).c`，与
  JS/TS/C#/Swift 可选链语义一致，`?.` 在 `.c` 处短路、后续 `.` 为非可选访问。
  审计 F-01 判定此为"误解析"、主张 `OptionalChain(Field(a,"b"),"c")` 反为非标准
  （会使 `.c` 误入可选链），照做=回归。同 §0 之 ACT-F1/RT-H2 推翻为误报。
- 落锁：`audit_fix_parser_optional_chain_dot_assoc_left_to_right` 固定该结合性，
  防止未来误"修复"回退。无行为变更。

### 0.38.50 — TransitionEpoch 生命周期闭环
- 暴露 `flow_drop(handle)` 语言 builtin，贯通 checker、Bytecode VM、Resolved/native
  codegen 与 Component ABI；跨边界 Flow 句柄现在可以显式释放，不再只能依赖进程结束
  回收。
- 回归覆盖：`flow_drop_is_a_registered_language_builtin` 保持 builtin 可从 Mimi 调用；
  `flow_drop_production_dual_stale_after_drop` 在生产 dual（checked 解释器 + 编译
  `compile_checked` native）路径锁定 drop 后旧句柄返回 `EPOCH_ERR_STALE`（2）。

### 0.38.91–110 — Phase E Session K + 拆 mimispec
- SessionChan 方法表面：`ch.send(v)` / `ch.recv()` / `ch.close()` 在 checker、
  resolved/legacy native、bytecode VM 三路全等。tuple 解构的
  `session_pair::<S>()` 端点在 bytecode 与 legacy codegen 的类型跟踪中识别为
  `SessionChan`，不再误路由到 socket 的 `send`/`recv`。
- 负测试 `session_method_check_order_violation_rejected`；生产 dual
  `dual_session_method_roundtrip`（resolved + legacy + VM stdout `42/84`）。
- 自由函数 `session_send` / `session_recv` / `session_close` 保留但发出
  `W014` 弃用诊断，提示迁移到方法表面。
- 拆 MimiSpec：`mms{}` 由 parser 硬拒绝（不再 trivia 消费）；删除外部
  `mimispec` crate 依赖、`mimi mms` 命令、`src/main/mms.rs`、`std/mimispec/`；
  `mimi doc` 对 `.mms`/`mms` 输出给出 removed 错误。`mms_integration` 改锁硬拒绝。
- 文档同步：`docs/language-spec.md` §6.6/§6.9、`docs/syntax-reference.md` §6.3、
  README CLI 表更新为 0.1.8 移除态。

### 0.38.111+ — Phase F move-rest + 稀疏诊断/失败分层
- move-rest 端到端：`return Target { f: e, ..self }` 在 parser、checker、
  resolved IR、resolved/legacy native、Bytecode VM 同语义；rest 只移动未
  显式写出的字段，整份 rest 拷贝后覆写显式字段。`Op::UpdateRecord` /
  `ResolvedExprKind::Record.rest` 支持该动作。
- 负测试：显式 `self.f` 与 `..self` 同时出现时拒绝（`E0256`，线性字段不拷）；
  `move_rest_three_fields_self_loop` 锁 bytecode + resolved native + legacy
  native 三路 `2 2 3`。
- sparse 拒绝诊断列出当前状态合法事件：`no flow transition ...; legal events
  from S: a, b`。`flow_sparse_undefined_event_rejected` 断言新信息。
- 文档：language-spec §3.4 记录 move-rest、§2.3 明确 `fails E` 回滚与 `Fault`
  系统状态是两条通道（诊断不把 Fault 称作第二个 Err）；syntax-reference 5.3
  记录 `..expr` 语法。

### 0.38.126+ — Phase G 终测
- lib 全量 **5529 passed / 0 failed / 7 ignored**；real_world 31、real_world_cli 2、
  stress 62、dogfood（taskq/ledger/mimichat/mimichat-modern）全绿。
- ASan 44、TSan 11、Valgrind 18 均通过；生产 dual 核语料 100% resolved、零 fallback。
- 修复 resolved emitter 两处终测问题：builtin `PeerFault` 字段名查找缺失、
  unit 函数裸 `return` 误发 `ret void`（应 `ret i64 0`，与 legacy/0.35.23 对齐）。
- `devdocs/v0.38/quad-final-0.38.md` 记录终测结果与发布建议。

### 0.38.71–90 — Phase D 入口 A（actor 业务 mut 关闭）
- `E0402` 扩到无 `runs Flow` 的业务 actor：用户可见 `mut` 字段一律非法；
  诊断给出单状态 Flow + transition 改写骨架。
- actor 字段在每实例上可读写，但业务状态变更必须落到 Flow transition；
  保留 `actor Name runs FlowName` 唯一业务 actor 形态。
- 迁移：仓库 dogfood / demo / real_world 不再教 `BankAccount { mut balance }`
  逃生舱；相关已迁 Flow 一体化。

### 0.38.26–45 — Phase B 值 ABI
- `List<string>` 元素改为长度感知 `{ptr, len}`（`MimiStr` 盒 + `string_abi=2`），
  与裸 `string` 观察等价。`str_split` 走 `mimi_str_split_ll`（ptr+len）；
  `str_join` 读 fat 槽。嵌入 NUL / 空串 / 多字节 UTF-8 / 嵌套
  `List<List<string>>` 生产 dual 锁。
- 旧 C-string 元素 ABI（`string_abi=0` 或非 `MSTR` 魔数）由
  `mimi_list_read_string` / `mimi_str_join` 拒绝，禁止静默按 NUL 截断。
  `mimi_list_string_abi_version() == 2`。
- Map/Set 句柄改为 `HandleGeneration` + per-op lease（名称不是 Flow
  `TransitionEpoch`）。destroy：停新 lease → 等归零 → 世代 +1。
  销毁/过期句柄是 typed error（`mimi_handle_last_error` /
  `mimi_map_try_size`），不是 UAF。测试
  `map_lease_destroy_waits_active_op` / `map_stale_generation_is_typed_error`
  及 Set 孪生。
- 关闭 `devdocs/v0.37/known-blockers-architecture.md` 的 B-STR-001 /
  B-HANDLE-001。

### 0.38.46–49 — Phase C Flow TransitionEpoch
- 每个 Flow 值概念上携带 `TransitionEpoch`；裸 Flow record 不得跨
  Channel / FFI / actor mailbox（新码 E0443），必须先 `flow_pack`。
  本地 self-loop（clause 5.1 silent stay）剥离 epoch、不产 pack 税。
- runtime 入口：`flow_pack` / `flow_epoch` / `flow_check_epoch` /
  `flow_bump_epoch` / `flow_unpack` / `flow_pack_count` /
  `flow_epoch_last_error`。旧 epoch 的 peer 收到 typed stale 错误
  （`EPOCH_ERR_STALE`），不是静默别名/UAF。
- 测试：`epoch_rejects_bare_flow_on_channel` /
  `epoch_rejects_bare_flow_in_extern` / `epoch_rejects_bare_flow_in_mailbox`
  （E0443），`flow_epoch_pack_roundtrip` /
  `flow_epoch_stale_is_typed_error` /
  `flow_epoch_recover_bump_is_new_epoch`，以及生产 dual
  `flow_epoch_channel_stale_rejected` / `flow_epoch_local_self_loop_no_tax`。
- `lookup_variant_name` 增加 `state:Flow::State` 变体回退，resolved 静态 Flow
  match 可处理单目标/guard Flow 状态匹配。

### 0.38.16–25 — Phase A Narrow
- checker 单一谓词 `TaskBoundaryKind::may_cross_task_boundary`：`view` /
  `mutate` / `&T`/`&mut T` 默认不得进入 spawn、Channel 元素、Future 捕获、
  actor mailbox。同步 `func` 参数（含 DSP `mutate List`）不是任务边界。
- 新码 E0442；负测试 `narrow_rejects_view_across_spawn` /
  `narrow_rejects_mutate_in_channel` / `narrow_rejects_ref_in_future_env` /
  `narrow_rejects_view_in_mailbox`。
- actor mailbox 测试不再对已返回的方法结果写 `await`：`await` 只接合
  Future（spawn 任务），同步方法结果直接取值。

### 0.38.0–15 — Phase 0 L1 spawn/await 同语义
- VM `spawn`/`await` 发 `Op::Spawn` / `Op::Await`：朴素 OS 线程 + join，与
  native `mimi_spawn_future` 同观察。删除 0.1.7「sequential fallback 即设计」
  注释；成功路径不再 compile-inner-then-Mov / await-as-eval。
- 生产 dual：`dual_assert_prod!` = `check` + 装 CheckedProgram 的 interp +
  `compile_checked` + 跑产物。核 spawn/Flow 证据走此路径，不靠 legacy
  `compile_file`。
- Resolved 对 Flow / Session / spawn / 线性核 callee 的 emit 失败改为硬错误
  （函数名 + 原因），禁止静默 Legacy 降级。`dispatch_stat.py check --core`
  只扫核语料。
- 新测试：`dual_spawn_channel_same_completion`（顺序 spawn 过不了）、
  `dual_spawn_deadlock_is_deadlock`（互等必须 hang，不得假成功）、
  `dual_production_checked_path_spawn`、
  `dispatch_core_flow_zero_legacy_fallback`。

## [0.1.7] — 2026-08-19

> **Wave-3 基建诚实收口已发布**。
> 0.1.7 里程碑：默认语料 dispatch 0 fallback、确定性 Drop-glue、native 真线程、
> Phase E 边缘出清（`quote!` / `$(...)` / `protocol` / `impl Protocol`）、
> Component ABI/Wire CLI、dogfood 与高压套件。
> **不宣称**内核愿景已闭合、不宣称 VM≡native、不宣称 Flow 世代已实现。
> 终测报告 `devdocs/v0.37/quad-final-0.37.135.md`。下一版本 0.1.8。

### 0.37.136 — chore(release): 0.1.7
- Cargo.toml / Cargo.lock：`0.1.7-dev` → `0.1.7`
- CHANGELOG / README 同步为已发布
- 附注 tag `0.1.7`

### 0.37.135 — Phase F 终测与文档收口
- 新增 `devdocs/v0.37/quad-final-0.37.135.md`，按 §4 逐条登记「不宣称」
- README / AGENTS / gap-audit 从「开发进行中」改为「收口完成，待切 tag」

### 0.37.130 — Wave-3 基建诚实收口 + native map from_list
- 新增 `devdocs/v0.37/quad-final-0.37.135.md`：按 §4 列出全部「不宣称」
- native `map_from_list` 按 `List<(string, Any)>` 的 `{ {ptr, len}, value }`
  元组槽解码，不再把 string 长度误读成 map 值
- ABI 声明的 C 字符串 map key 允许字节对齐（不再要求 8 字节对齐）
- 双后端钉住 `from_list` / `to_list` / `get` / `set` 往返（stdout 同为最终 size）
- Fault 表面：§3.12 为 0.1.7 权威；§4.3 rich variant 明确 deferred 0.2

### 0.37.129 — 0.1.7/0.1.8/0.1.9 宏观重锚
- 新增 `devdocs/kernel-roadmap-0.1.7-0.1.9.md`、`devdocs/v0.38/README.md`、
  `devdocs/v0.39/README.md`
- 0.1.7 DoD 改为诚实收口：不宣称 VM≡native、不宣称 Flow 世代已实现
- 0.1.8 锁定为语义诚实 + 身份纯度（S/A/K、值 ABI、拆 mimispec）
- 0.1.9 锁定为 `linear T` + cap std + 内核卡 + AI 评测
- `feature-design-review` §6「等 dogfood 再裁」废止

### 0.37.128 — .mimiabi 全语言 Bindgen 接入
- 新增 `mimi abi emit-go` / `emit-node` / `emit-py` / `emit-java` / `emit-cpp`
  - 通过 ComponentIR → AST 适配层，让旧语言后端都能以 `.mimiabi` 为统一输入
  - Node 支持 `--ts-output`；Java 支持 `--java-output`
  - 与 `emit-c` / `emit-rust` 共同覆盖 7 大语言 Bindgen 入口
- 新增 CLI 单测：core `.mimiabi` 同时生成 Go/Node/Python/Java/C++ 输出
- `MimiAbiType` / `MimiAbiTypeRef` 从 `mimi::component` 公开导出

### 0.37.127 — 用户源码 ABI 到 C/Rust 生成端到端测试
- 新增 CLI 单测：`mimi abi export` → `mimi abi emit-c` / `emit-rust`
- 验证用户 `Point` struct 从 `.mimi` 源码进入 `.mimiabi` 并出现在 C Header 与 Rust bindings

### 0.37.126 — 用户源码 ABI 导出包含类型定义
- `mimi abi export` 现在同时导出用户类型
  - repr(C) Record → AbiStruct
  - 无 payload Enum → AbiEnum
  - Alias / Newtype → AbiAlias
- 单测覆盖：导出结果包含用户 `Point` struct

### 0.37.125 — 用户源码 Component ABI 导出
- 新增 `mimi abi export <source.mimi> [-o out]`
  - 从 `.mimi` 源码提取 extern/exported 函数
  - 叠加 core runtime ABI 后导出为标准 `.mimiabi` JSON
  - 可将用户组件与本地 C/Rust 绑定生成、版本握手检查无缝衔接
- 新增 1 个 CLI 单测：导出结果同时包含 user import、user export 与 core runtime export

### 0.37.124 — 全量门禁一键复跑脚本
- 新增 `scripts/check_all_gates.sh`
  - 依次执行 fmt / clippy / lib tests / dispatch-zero / stress smoke / stress heavy /
    dogfood / bin CLI tests / real-world CLI
  - 任一失败即停止并返回非零
- 便于 0.1.7 最终门禁复核与 CI 统一入口

### 0.37.123 — ABI 版本握手检查 CLI
- 新增 `mimi abi check <file>`：将给定 `.mimiabi` 与当前 core runtime ABI 比对
  - 输出所有变更与 breaking 摘要
  - 存在 breaking change 时失败退出，用于 Native ABI 版本握手
- 新增 2 个 CLI 单测：当前 core ABI 自检通过、导出重命名触发 breaking 失败

### 0.37.122 — Wire Schema 校验 CLI
- 新增 `mimi wire validate-schema <file>`：读取并校验 WireSchema JSON
  - 检查重复字段名/索引、索引连续 0-based 等语义
- 新增 2 个 CLI 单测：clean schema 通过、non-contiguous index 拒绝

### 0.37.121 — ABI CLI stdin/stdout 支持
- `mimi abi * -` 可从 stdin 读取 `.mimiabi` JSON
- `-o -` 可将 JSON/生成代码写到 stdout
- 支持 `mimi abi core -o - | mimi abi validate -` 管道工作流

### 0.37.120 — Wire CLI stdin/stdout 支持
- `mimi wire encode -` / `mimi wire decode -`：从 stdin 读取输入
- `-o -`：将二进制输出写到 stdout（默认 stdout）
- 便于 shell 管道直接进行 Wire 封包/解包

### 0.37.119 — Component `.mimiabi` CLI
- 新增 `mimi abi core [-o out]`：导出 core runtime Component ABI 为 `.mimiabi` JSON
- 新增 `mimi abi validate <file>`：校验 format version / enum / 语义字段
- 新增 `mimi abi hash <file>`：打印 BLAKE3 内容哈希
- 新增 `mimi abi diff <old> <new>`：输出 breaking/non-breaking 变更摘要
- 新增 `mimi abi emit-c <file>` / `mimi abi emit-rust <file>`：从 `.mimiabi` 直接生成
  Component IR 驱动的 C Header 与 Rust FFI 绑定
- 新增 5 个 CLI 单测：core 导出/校验 roundtrip、坏 JSON 拒绝、identical diff、
  emit-c/emit-rust、clap 解析
- README CLI 表补充 `mimi abi`

### 0.37.118 — Wire Schema CLI 接入
- 新增 `mimi wire encode <payload> [-o out]`：将原始 payload 包装为二进制 Wire Envelope
- 新增 `mimi wire decode <envelope> [-o out]`：解包并校验 Wire Envelope，输出原始 payload
- 支持 stdout/文件输出，校验 magic/version/length/trailing data
- 新增 3 个 CLI 单测：roundtrip、corrupt reject、clap 子命令解析
- README CLI 表补充 `mimi wire encode|decode`

### 0.37.117 — 0.1.7 Final Gate Evidence 快照
- 新增 `docs/0.1.7-final-gate-evidence.md`
- 汇总后端零回退、Valgrind/ASan/TSan、10M fuzz、10M event storm、
  stress-heavy、dogfood、LSP 亚 10ms、Phase E 终检证据
- 24h soak runner 已就绪，实际长时间运行在最终 Nightly 阶段执行

### 0.37.116 — Nightly 24h Soak 运行脚本
- 新增 `scripts/soak_nightly.sh`
  - 默认执行 `MIMI_SOAK_SECONDS=86400` 的 native 内存稳定 soak
  - 输出与日志自动落到 `devdocs/soak-<duration>s-<timestamp>.log`
  - 支持 `MIMI_SOAK_SECONDS=900` 等短浸泡验证
- soak 测试增加结束摘要：`duration_secs / samples / baseline_kb / peak_kb / growth_kb`

### 0.37.115 — ASan / TSan Nightly Sanitizer 复核通过
- 使用 `cargo +nightly test -Z build-std --target x86_64-unknown-linux-gnu`
  + `RUSTFLAGS=-Z sanitizer=address` 复核：
  - lexer 基础测试通过
  - `tests::fuzz::target_parser::` 10 项通过
  - `tests::property::` 44 项通过
- 使用同一 nightly build-std + `RUSTFLAGS=-Z sanitizer=thread` 复核：
  - `runtime::future::tests::` 5 项通过
  - `ffi::runtime::tests::` 15 项通过
  - `tests::actor_concurrent::` 11 项通过
- 未发现 ASan 内存错误、泄漏或 TSan 数据竞争
- 新增 `scripts/sanitize_ci.sh` 一键复跑：
  - `scripts/sanitize_ci.sh asan`
  - `scripts/sanitize_ci.sh tsan`
  - `scripts/sanitize_ci.sh both`

### 0.37.114 — Native 长时间内存稳定 Soak 门禁
- 新增 `#[ignore]` 重载 soak 测试 `stress_soak_native_memory_stability_heavy`
  - 编译为 native 二进制后运行无限分配循环（临时 List 反复分配/释放）
  - 每 500ms 采样 `/proc/<pid>/status` VmRSS，判定峰值增长不超过有界阈值
  - 默认 5s；`MIMI_SOAK_SECONDS=86400` 可驱动 24h Nightly soak
- 验证：5s 与 60s soak 均通过，RSS 无失控增长
- 新增 `build_native_only` 供长驻 native 进程测试复用

### 0.37.113 — Makefile fuzz 目标统一 LLVM 前缀
- `test-fuzz-quick` / `test-fuzz` / `test-fuzz-ci` 现在与其它门禁一致，
  自动使用 `LLVM_SYS_181_PREFIX`，避免单独手动设置环境变量
- `make test-fuzz-quick` 验证通过

### 0.37.112 — Valgrind 全量成员复核通过
- 运行 `cargo test --lib e2e_valgrind_ -- --test-threads=1`
- 18 个 Valgrind 覆盖项全部通过：
  string/list/recursion/closure/fault-heap-cleanup/large-struct-return/
  shared/weak/parasteps/spawn/arena 等
- 覆盖 compiler-native 路径堆分配、引用计数、Fault 回滚与并发 spawn 内存安全

### 0.37.111 — Wire Fuzzing 10,000,000 次迭代
- 新增 `#[ignore]` 重载 fuzz 门禁 `stress_wire_fuzz_10m_no_panic`
  - 对随机/截断二进制 Wire Envelope 与 WireType 解码循环执行 10,000,000 次
  - 只允许安全拒绝或成功解码，禁止 panic/崩溃
- 实测 10M 次迭代约 8s 内完成
- `make test-stress-heavy` 可直接纳入该 10M 重载门禁

### 0.37.110 — LSP 10k 行项目亚 10ms 补全/悬停
- 优化 `compute_completion`：一次 `parse_with_recovery` 结果复用到 top/type/
  module/impl 分支，避免对大文件重复克隆 AST
- 新增 `#[ignore]` 重载性能门禁
  `e2e_perf_10k_line_completion_sub_10ms`
  - 合成 10,003 行 / 267KB 单函数文件
  - 实测 hover 中位数 ≈ 6.9ms、completion 中位数 ≈ 6.4ms（debug 构建）
  - 断言 hover 与 completion 中位数 < 10ms
- `cargo test --lib lsp_e2e` 常规 7 passed / 1 ignored；ignored 重载门禁通过

### 0.37.109 — Phase E：移除 Protocol 表面语法
- `protocol` 从关键字表移除，恢复普通标识符能力
  - `protocol` 声明与 Flow 内 `impl ProtocolName` 由 parser 拒绝并给出 0.1.7
    Phase E 迁移错误
- 删除 `parse_protocol_def` 与 Flow 内 impl 收集路径
- 删除 AST `Item::Protocol` / `ProtocolDef` / `FlowDef.impl_protocols`
- parser recovery 同步 token 移除 `TokenKind::Protocol`
- 清理 CheckedProgram / verifier / codegen / interpreter 中的 protocol 目录、
  conformance 投影与 resolved/checked 访问器
- 测试迁移：
  - 移除 protocol 语法测试（flow_features、resolved）与
    `tests/real_world/flow_protocol.mimi`
  - 新增 `protocol_syntax_removed_at_parser`
  - flow lexer 关键字表改断言 `protocol` 为普通标识符
- 文档同步：`docs/syntax-reference.md` 关键字表 62 → 61、删除 §6.2 Protocol
  产生式；`docs/language-spec.md` §3.9/§6.5 标记 surface removed；
  `docs/phase-e-status-0.1.7.md` 标记 protocol 删除完成
- 门禁：`cargo test --lib` 5421 passed / 6 ignored、clippy/fmt clean、
  `make test-stress` 62 passed / 26 ignored、real_world_cli 通过

### 0.37.108 — Phase E：移除 quote 语法面
- `quote` 从关键字表移除，恢复普通标识符能力
  - `func quote()` / `let quote = 1` 合法
- parser 不再产生 `Expr::Quote` / `Expr::QuoteInterpolate`
  - `quote! { ... }` 与 `$(...)` 现在给出 0.1.7 Phase E 迁移错误
  - 提示改用 `comptime { ... }` 常量折叠
- 删除 `parse_quote_block` 与 quote 专用 parser 路径
- 测试迁移：
  - quote 正用例迁移到 `comptime` 等价路径
  - SD-7/SD-9 常量折叠回归改为 `comptime` 双后端验证
  - 新增 `quote_syntax_removed_at_parser` / `quote_interpolation_removed_at_parser`
  - 新增 `quote_is_ordinary_identifier_now`
- 文档同步：`docs/syntax-reference.md` 关键字表 63 → 62、§5.3 移除 quote 产生式；
  `docs/phase-e-status-0.1.7.md` 标记 quote 删除完成
- `cargo test --lib`：5450 passed / 6 ignored


### 0.37.107 — Flow 10,000,000 事件风暴原生重载
- 新增 `stress_flow_event_storm_10m_native_heavy`
- 使用 tail-recursive Flow driver 在每次递归传入新的线性 state 绑定，
  checker 接受该写法，native codegen 将尾递归收紧为循环
- 单 Flow 实例连续完成 10,000,000 次状态转移，实测约 12ms
  - 验证 DoD 状态机承压：10,000,000 events，零乱序、零状态撕裂
- 新增 PR 门禁级 `stress_flow_event_storm_native_smoke`：原生 100,000 次事件
- `make test-stress` 覆盖 100K smoke，`make test-stress-heavy` 覆盖 10M heavy

### 0.37.106 — build-race 便捷门禁 + nested-spawn 文档
- Makefile 新增 `make test-build-race`
  - 只跑 `stress_parallel_mimi_build_no_archive_race`
  - 单独验证并发 `mimi build` 不再共享 archive
- readme/04-concurrency §4.1 补充 nested-spawn 上下文说明
  - 专用 nested-spawn 线程也视为 worker 上下文
  - 深层链式/二叉 fan-out 不会回排有界 pool

### 0.37.105 — 并发 mimi build archive 竞态回归测试
- 新增 `tests/stress/build_concurrency.rs`
  - `stress_parallel_mimi_build_no_archive_race`
  - 同一输出目录下并行启动 4 个 `mimi build`
  - 全部构建成功且 4 个 binary 输出 `race-ok`
- 固定 0.37.103 的 per-build runtime archive 修复，防止回归
- `make test-stress` 将自动覆盖该路径

### 0.37.104 — AtomicBool CAS 自旋锁进入 real_world 语料
- 新增 `tests/real_world/concurrency_atomic_bool_spinlock.mimi`
  - 4 个 parasteps worker × 20 次锁内递增，校验 80
  - AtomicBool CAS 自旋拿锁 + AtomicI64 计数 + store 放锁
- VM 与 compiled binary 均通过
- `make test-dispatch-zero`：151 语料，聚合 fallback_rate=0.0000

### 0.37.103 — mimi build 使用每次构建独立的 runtime archive 路径
- 修复并发 `mimi build` 共享同一个 `libmimi_runtime.a` 导致的
  flaky 构建失败：
  - `failed to map object file: memory map must have a non-zero length`
  - `failed to open object file: No such file or directory`
- runtime archive 改到 per-build `tmp_dir` 下，多个进程不再互相覆盖
- 4 路并发 `mimi build` 同一输出目录验证全部成功
- real_world_cli 不再因并发/残留 archive 偶发失败

### 0.37.102 — 扩展 nested-spawn 覆盖：二叉 fanout + real_world 语料
- `tests/stress/real_spawn.rs` 新增：
  - `stress_native_nested_spawn_fanout_smoke`：深度 6，2^6 叶子，校验 64
  - `stress_native_nested_spawn_fanout_heavy`：深度 10，2^10 叶子，校验 1024
- native fanout heavy 约 94ms，验证嵌套 spawn 可二叉发散且无 pool 回压死锁
- 新增 `tests/real_world/concurrency_nested_spawn.mimi`
  - outer → inner 两层嵌套 spawn
  - VM 输出 44，compiled binary 输出 44

### 0.37.101 — 修复深层嵌套 spawn 链死锁 + 原生回归压力
- **根因**：从专用 nested-spawn 线程再次 `spawn` 时未标记 `IN_WORKER`，
  深链会重新排队回有界 Worker Pool；当所有 pool worker 都在 await 子任务时
  互相等待 → 死锁
- **修复**：`src/runtime/future.rs` 中 dedicated nested-spawn 线程同样标记
  `IN_WORKER = true`，后续嵌套 spawn 继续使用专用线程
- 新增：
  - `stress_native_nested_spawn_chain_smoke`：16 层，输出 16
  - `stress_native_nested_spawn_chain_heavy`：128 层，输出 128，约 17ms
- 该回归测试在修复前 native 二叉会 hang；修复后通过

### 0.37.100 — 原生 AtomicBool CAS 自旋锁压力测试
- `tests/stress/real_spawn.rs` 新增：
  - `stress_native_atomic_bool_lock_smoke`：8 workers × 50 次锁内递增，校验 400
  - `stress_native_atomic_bool_lock_heavy`：32 workers × 500 次，校验 16000
- worker 用 `atomic_bool_compare_exchange(flag, false, true)` 自旋拿锁，
  `atomic_bool_store(flag, false)` 放锁，`AtomicI64` 计总数
- native heavy 约 12.7ms，验证 AtomicBool CAS 在高竞争下无丢失更新

### 0.37.99 — Flow Event Storm 重载提升至 5,000 转移
- `stress_flow_event_storm_heavy` 从 2,000 → 5,000 次连续 Flow 转移
- 保持 `mimi run` 路径，验证 VM 下线性 Flow 链的 5k 深度可用性
- heavy 门禁在 `make test-stress-heavy` 中验证

### 0.37.98 — channel_send 断开可观测性
- `mimi_channel_send` 不再静默吞掉 mpsc `send` 错误
- 当 receiver 已 drop 时输出
  `[mimi runtime] channel send: channel disconnected: ...`
- 与 `channel_recv` 的 H12 断开日志对称
- `make test-stress` 仍 57 passed

### 0.37.97 — AtomicI64/AtomicBool CAS 双后端等价测试
- `src/tests/dual_backend.rs` 新增：
  - `dual_atomic_i64_compare_exchange`
  - `dual_atomic_bool_compare_exchange`
- 分别覆盖成功/失败 CAS 与值保持路径
- VM 与 resolved codegen+LLVM native 双后端输出一致
- `cargo test --lib "tests::dual_backend::dual_atomic"` 8 passed

### 0.37.96 — Component IR 补注册 Map→JSON 运行时导出
- `src/component/gen.rs` 新增：
  - `mimi_map_to_json_i64`
  - `mimi_map_to_json_string`
  - `mimi_map_to_json_bool`
  - `mimi_map_to_json_f64`
  - `mimi_map_to_json_f64_serde`
- 消除 dogfood 构建中在 mimichat 出现 5 次
  `get_runtime_fn("mimi_map_to_json_*") not in Component IR registry` 警告
- `make test-dogfood` check/test/build/run 全绿且无 registry 警告

### 0.37.95 — syntax-reference 标注 quote/protocol Phase E 判死注记
- `docs/syntax-reference.md` §5.3 / §6：
  - `quote` / `quote!` / `$(...)` 标注 0.1.7 Phase E 已裁决删除
  - `protocol` 声明 / `impl P` 标注 0.1.7 Phase E 已裁决删除
- 当前 parser 仍兼容，删除提交属于 0.1.7 收尾
- 与 feature-design-review #1/#2 保持一致

### 0.37.94 — 原生 AtomicI64 CAS 竞争压力测试
- `tests/stress/real_spawn.rs` 新增 `stress_native_atomic_i64_cas_smoke` / `heavy`
  - 8 个 parasteps workers × 50 次 CAS，校验 400
  - 32 workers × 500 次 CAS，校验 16000
- worker 使用 `load` + `compare_exchange` 自旋无锁递增共享计数器
  - 高压下不丢更新，native 重载约 9ms
- 与 AtomicI32 fetch-add / Mutex 互斥保护互补：覆盖 AtomicI64 CAS 竞争路径

### 0.37.93 — AtomicBool compare_exchange 内置原语补齐
- runtime 新增 `mimi_atomic_bool_compare_exchange(handle, expected, desired) -> i32`
- checker / bytecode VM / resolved codegen / component export 全链路接入
  `atomic_bool_compare_exchange`
- 入参接受 bool 或 0/1 i32，成功返回 1，失败返回 0
- 新增 `tests/real_world/concurrency_atomic_bool_compare_exchange.mimi`
  - 成功/失败两条路径，VM 与 compiled binary 均通过
- readme/04-concurrency AtomicBool 方法表补 `compare_exchange`

### 0.37.92 — AtomicI64 compare_exchange 内置原语补齐
- runtime 新增 `mimi_atomic_i64_compare_exchange(handle, expected, desired) -> i32`
- checker / bytecode VM / resolved codegen / component export 全链路接入
  `atomic_i64_compare_exchange`
- 与 AtomicI32 CAS 语义一致：成功返回 1，失败返回 0
- 新增 `tests/real_world/concurrency_atomic_i64_compare_exchange.mimi`
  - 成功/失败两条路径，VM 与 compiled binary 均通过
- readme/04-concurrency AtomicI64 方法表补 `compare_exchange`

### 0.37.91 — spec §7.4 出清：ffi slice/slice_mut/buffer 判死移除
- `docs/language-spec.md`：
  - `ffi slice<T>` / `ffi slice_mut<T>` / `ffi buffer<T>` 标记 **REMOVED (0.1.7)**
  - 注释从「未实现（0.2 评估）」升级为「已判死删除（0.1.7 Phase E）」
- 保留 `ffi view/mutate/owned/shared/weak/handle/str/owned_str/c_str` 稳定面
- 后续 Phase E 继续处理 quote / Protocol / Effect lattice / 影子内存

### 0.37.90 — 原生 10,000 spawn/await 循环重载
- 新增 `stress_native_spawn_await_tenk`
  - `for i in range(0, 10000)` 内 `spawn id(i)` + `await`
  - 校验 `0+...+9999 = 49995000`
  - native 运行约 93ms
- 与 10k Channel worker 互补：覆盖 spawn+await 单任务高频路径

### 0.37.89 — 原生 10,000 Channel worker 并发达 Phase C DoD 量级
- 新增 `stress_native_channel_workers_tenk`
  - `mimi build` + native 执行
  - 循环内生成 10,000 个 `spawn send_id(ch, i)` 任务
  - 主线程接收 10,000 个值并校验 `0+...+9999 = 49995000`
- 有界 Worker Pool 下 native 运行仅需约 15ms，无死锁/丢消息
- Phase C DoD：「10,000 并发 Task + Channel 传递」已有直接重载证据

### 0.37.88 — Makefile 增加 real_world / real_world_cli 便捷门禁
- `make test-realworld`：`cargo test --test real_world`（4 threads）
- `make test-realworld-cli`：`cargo test --test real_world_cli`（1 thread）
- 与 stress/dispatch/dogfood 目标并列，便于本地按需跑双后端真实语料

### 0.37.87 — AtomicI32 compare_exchange 进入 real_world 语料
- 新增 `tests/real_world/concurrency_atomic_compare_exchange.mimi`
  - 成功 CAS 返回 1，失败 CAS 返回 0，值保持 9
  - 45/45 eligible，0% fallback
- `make test-dispatch-zero`：148 语料，聚合 fallback_rate=0.0000

### 0.37.86 — AtomicI64 进入 real_world 语料
- 新增 `tests/real_world/concurrency_atomic_i64.mimi`
  - store/load/fetch_add/drop 全链路
  - 45/45 eligible，0% fallback
- `make test-dispatch-zero`：147 语料，聚合 fallback_rate=0.0000

### 0.37.85 — AtomicBool 进入 real_world 语料
- 新增 `tests/real_world/concurrency_atomic_bool.mimi`
  - atomic_bool_new(false) → store(true) → load 校验
  - 45/45 eligible，0% fallback
- `make test-dispatch-zero`：146 语料，聚合 fallback_rate=0.0000

### 0.37.84 — 原生 Mutex 互斥保护压力进入 stress 套件
- `tests/stress/real_spawn.rs` 新增 native Mutex 用例
  - `stress_native_mutex_protected_smoke`：8 workers × 50 次 protected inc，校验 400
  - `stress_native_mutex_protected_heavy`：32 workers × 500 次，校验 16000
- 每个 worker 通过 `mutex_lock/get/set/unlock` 更新同一 `Mutex<i64>`
- parasteps join 后读取最终值，验证互斥且无丢失更新

### 0.37.83 — 原生 Atomic fetch-add 并发压力进入 stress 套件
- `tests/stress/real_spawn.rs` 新增 native 原子递增用例
  - `stress_native_atomic_fetch_add_smoke`：8 workers × 100 次，校验 800
  - `stress_native_atomic_fetch_add_heavy`：32 workers × 1000 次，校验 32000
- parasteps 块尾 join 后读取并校验 `AtomicI32`
- smoke 与 heavy 均通过，验证真线程下的 fetch-add 原子性

### 0.37.82 — 原生 parasteps + Channel 高压重载进入 stress 套件
- `tests/stress/real_spawn.rs` 新增 native 路径用例（真正走 `mimi build` 编译）
  - `stress_native_parasteps_channel_smoke`：8 个 spawn 发送，校验 sum=28
  - `stress_native_parasteps_channel_heavy`：64 个 spawn 发送，校验 sum=2016
- 与既有 `real_world_cli` 的 build+exec 不同，该用例可在 stress 套件里直接跑
- 验证：smoke 与 heavy 均通过

### 0.37.81 — readme/04-concurrency 补完 Channel/Mutex/Atomic 内置原语文档
- 新增第 5 节「并发原语」
  - Channel：`channel_new/send/recv/try_recv/drop`
  - Mutex：`mutex_new/lock/get/set/unlock/drop`
  - Atomic：`AtomicI32/AtomicI64/AtomicBool` 常用原语
- 每类附可直接运行的 `func main` 示例
- 后续章节号顺延修复（On Failure / Parasteps+On Failure / 共享状态 / 并发模式）

### 0.37.80 — channel_try_recv 进入 real_world 语料
- 新增 `tests/real_world/concurrency_channel_try_recv.mimi`
  - 空 Channel 上 `channel_try_recv` 返回 `-1`
  - send 后 `try_recv` 取回 `7`
  - 44/44 eligible，0% fallback
- `make test-dispatch-zero`：145 语料，聚合 fallback_rate=0.0000

### 0.37.79 — parasteps 结构化并发 + Channel 管线进入 real_world 语料
- 新增 `tests/real_world/concurrency_parasteps_channel.mimi`
  - `parasteps` 内 spawn 两个任务向共享 `Channel<i64>` 发送
  - 块结束隐式 join 后，主线程从 Channel 接收并校验 `1+2 == 3`
  - 44/44 eligible，0% fallback
- `make test-dispatch-zero`：144 语料，聚合 fallback_rate=0.0000

### 0.37.78 — clippy --all-targets -D warnings 清零 + worker_loop 简化
- `src/runtime/future.rs`：去掉 `let was_worker = ...` / `let _ = was_worker` 的
  unit 值样板，`IN_WORKER.with` 直接执行 poll + release 并恢复线程标记
- `src/codegen/expr.rs`：把 guarded `unwrap()` 改为带语义说明的 `expect()`，
  满足 unwrap_used 策略
- 验证：
  - `cargo clippy --all-targets -- -D warnings`：0 error
  - `cargo test --lib`：5481 passed
  - `make test-stress`：53 passed；`make test-stress-heavy`：16 passed

### 0.37.77 — std::errors 语料扩展覆盖 JSON/Collection/Math/Net 字符串载荷
- `tests/real_world/std_errors.mimi` 增加：
  - `Json(ParseError("bad"))`
  - `Collection(IndexOutOfBounds(1, 4))`
  - `Math(DivisionByZero)`
  - `Net(ConnectionFailed("refused"))`
- 全部走 `app_error_to_string` 并在 compiled binary 与 VM 双后端校验
- `make test-dispatch-zero`：143 语料，聚合 fallback_rate=0.0000

### 0.37.76 — Channel + 真实 spawn 协作进入 real_world dispatch 语料
- 新增 `tests/real_world/concurrency_channel_workers.mimi`
  - 4 个真实 spawn 任务通过 `Channel<i64>` 回传数据
  - 主线程统一接收并校验 `0+1+2+3 == 6`
  - 44/44 eligible，0% fallback
- `make test-dispatch-zero`：143 语料，聚合 fallback_rate=0.0000

### 0.37.75 — 自定义枚举 string 载荷加入 real_world dispatch 固定回归
- 新增 `tests/real_world/custom_enum_string_payload.mimi`
  - `Label("hello")` / `Count(7)` 两种载荷均按 enum ABI 正确解码
  - 45/45 eligible，0% fallback
- `make test-dispatch-zero`：142 语料，聚合 fallback_rate=0.0000

### 0.37.74 — resolved 自定义枚举单字符串载荷：按 enum 装箱 ABI 解码

- 真实 CLI 全量回归发现 `std::errors` 编译产物把 `Fs(NotFound("x"))` 显示成乱码
- 根因：自定义枚举单字段若为 string，构造侧按 compact enum ABI 将
  `{ptr,len}` 装箱为堆指针存入 i64 payload；但 resolved 的 ctor 绑定
  复用了 `convert_list_elem_i64`，把该指针误当裸 C 字符串处理
- 修复：单字段自定义枚举解码时检测到 string shape，改为 inttoptr + load
  恢复 `{ptr,len}` 结构
- 连带修复：`std_collections.mimi` 编译二进制从 exit 160 恢复为 0
- 验证：
  - `real_world_cli_suite` 全量通过（此前 2 个 codegen 失败）
  - `cargo test --lib`：5481 passed
  - `make test-stress`：53 passed；`make test-stress-heavy`：16 passed
  - `make test-dispatch-zero`：fallback_rate=0.0000
  - `make test-dogfood`：全绿

### 0.37.73 — stress 并发规模上探：1000 spawn/await heavy 用例
- 新增 `stress_concurrency_scale_thousand`
  - 一次程序内 1000 个真实 spawn + 1000 个 await，校验总和 `499500`
- `make test-stress-heavy`：16 heavy 全通过（含 1000 规模 Task 并发）
- Worker pool 在千级任务下保持稳定

### 0.37.72 — parasteps 结构化并发进入 real_world dispatch 语料

- 新增 `tests/real_world/concurrency_parasteps.mimi`
  - 在 parasteps 内 spawn 两个任务，块尾 join 前校验两个结果
  - 44/44 eligible，0% fallback
- `make test-dispatch-zero`：141 语料，聚合 fallback_rate=0.0000

### 0.37.71 — 真实 Task 轻量调度器首片：有界 Worker Pool + 嵌套专用线程

- `mimi_spawn_future` 不再为每个 Future 新建一个独立 OS 线程
  - 顶层 spawn 提交到全局有界 Worker Pool（`available_parallelism` 上限 8）
  - 池内 Worker 复用，减少高并发 spawn 的线程数与 pthread 栈开销
- 嵌套 spawn 安全策略：池内任务再 spawn 时走专用线程，避免“所有 worker
  阻塞 await 子任务而子任务排不到 worker”的死锁
- 新增回归 `bounded_pool_nested_spawn_does_not_deadlock`
- 结果：
  - `cargo test --lib`：5481 passed
  - `make test-stress`：53 passed / 0 failed
  - `make test-stress-heavy`：15 heavy 全通过（含 500 spawn/await、500 channel worker）
  - `make test-dispatch-zero`：聚合 fallback_rate=0.0000
  - `make test-dogfood`：全绿

### 0.37.70 — std::errors 进入 resolved slice：支持同一类型的多个 `From` 重载

- 此前 `funcs` 只用 `类型_方法` 作键，两个 `impl From<X, AppError> for AppError`
  会互相覆盖，触发 E0252/E0402，导致 `std::errors` 无法检查通过
- 现在 impl 方法槽键改为：无泛型实例时保持历史 `Type_method`（如 `List_head`）；
  有具体 trait 参数时追加稳定后缀（如 `AppError_from_From<FsError,AppError>`）
- 同步修复 checker 的 trait 签名比较：先对 `From<T,U>` 的 `T/U` 代入具体 impl
  args，再比较返回/参数类型，避免把 `From<FsError,AppError>` 的 `from(_: FsError)`
  与 `From<MyError,AppError>` 的 `from(_: MyError)` 误判为重复
- `std::errors` 改用标准 `std::strings::to_string`，进入 resolved module 白名单
- 新增真实语料 `tests/real_world/std_errors.mimi`：54/54 eligible
- 验证：
  - `make test-dispatch-zero`：140 语料，聚合 fallback_rate=0.0000
  - `cargo test --lib`：5480 passed
  - `make test-stress`：53 passed / 0 failed
  - `make test-dogfood`：全绿

### 0.37.69 — resolved 返回归属探针纳入 Slot，修复 Records 深层返回内存泄漏

- `heap_probe_candidates` 此前只扫描 `HeapEntry::Ptr`，跳过 `HeapEntry::Slot`
  - `let a = "... " + "..."` 会把 concat 的临时 `Ptr` 注册转成 `a` 变量槽的
    `Slot`；返回所有权探针看不到它，于是把返回容器里的字符串判为“未拥有”
    并再 heap-copy 一份
  - 因为返回路径 `drain_heap_scope` 不会释放原 Slot，每次调用都泄漏原始字符串
- 现在 `heap_probe_candidates` 也把 `Slot` 对应的结构体指针字段加载进候选集
  - 返回 `Record<List<string>>` / 嵌套 Record / `List<List<string>>` 时，
    字符串数据指针能命中已注册槽位，不再发生“复制后漏原串”
- 结果：
  - `stress_soak_resolved_heap_record_string_list_return_ownership_smoke` 通过
  - `stress_soak_resolved_nested_heap_record_string_list_return_ownership_smoke` 通过
  - `stress_soak_resolved_list_of_string_list_return_ownership_smoke` 通过
  - `make test-stress`：53 passed / 15 ignored，0 failed
  - `make test-dispatch-zero`：聚合 fallback_rate=0.0000
  - `make test-dogfood`：全绿
  - `cargo test --lib`：5480 passed

### 0.37.68 — future await 由固定旋转上限改为 Condvar 阻塞等待

- `mimi_await_future` 旧的实现是在 `completed` 上 `yield_now` 旋转，
  超过 1_000_000 次直接 `process::abort()`
  - 高压/长时任务下只要调度稍慢就会误杀进程，违反 0.1.7 高压可靠性目标
- 改为全局 `AWAIT_LOCK` + `AWAIT_CONDVAR`：
  - `mimi_await_future` 在锁内检查原子 completed，未完成则 `condvar.wait`
  - `mimi_future_set_completed` 在同一把锁内 CAS 并 `notify_all`
  - 单一 condvar 不引入 per-future 堆分配，保持 Valgrind clean
- 新增回归 `future_await_blocks_until_completion_without_spin_abort`
- 门禁：
  - 所有 `spawn` 相关单测 45 个通过
  - `e2e_valgrind_spawn_basic` / `e2e_valgrind_spawn_multiple` 通过
  - `cargo test --lib` 5480 passed

### 0.37.66 — loader impl 去重键纳入 trait 参数

- 修复模块合并时多个 `impl From<A, Target> for Target` / `impl From<B, Target> for Target`
  被误判为同一个 `impl:From:Target` 重复项的问题
- `item_name` 对 `Item::Impl` 的 dedup key 从 `(trait, type)` 扩展为
  `(trait, trait_args, type)`，例如：
  `impl:From:FsError,AppError:AppError` 与
  `impl:From:JsonError,AppError:AppError` 不再冲突
- 同步修改 `src/loader/flow.rs` 的同一 dedup 逻辑
- loader 测试 45 个全部通过

### 0.37.67 — DynamicAnyPack 进入 resolved native slice，std::set/maps Any 包装全开

- `resolve eligibility.require_conversion` 接受 `CheckedConversionKind::DynamicAnyPack`
  - resolved emitter 早已实现 concrete → Any 的 i64/ptr box ABI（窄整数 sext 到 i64），
    只是 eligibility 未放行导致 Any 包装函数在 dispatch 统计中被判 fallback
- `tests/real_world/std_set.mimi` 加入 `insert` / `contains` / `remove` Any 包装，55/55
- `tests/real_world/std_maps.mimi` 改为 `std::maps` 包装 + Any：
  `set` / `get` / `get_or_default` / `to_list` / `remove`，73/73
- dispatch 139 语料聚合 fallback_rate=0.0000
- `cargo test --lib` 5479 passed

### 0.37.65 — `std::fs` 进入 resolved module 白名单 + StatResult/ExecResult 布局

- 修复 `std::fs` 的 resolved 盲区根因：`builtin:type:StatResult`（以及同类
  `ExecResult`）此前未在 resolved eligibility/types 中登记为内置 record
  - `src/codegen/resolved/eligibility.rs` 接受 `ExecResult` / `StatResult`
  - `src/codegen/resolved/types.rs` 补齐 LLVM 布局：
    - `ExecResult { exit_code: i32, stdout: string, stderr: string }`
    - `StatResult { size: i64, modified: i64, is_file: bool, is_dir: bool }`
  - `std::fs` 的三个 `StatResult` 相关函数（`stat` / `file_size` /
    `string_file_size`）由 legacy fallback 转为 resolved
- 默认 module-body 白名单加入 `fs`
- `tests/real_world/std_fs.mimi` 改为真实 `use std::fs`，覆盖
  `write` / `read` / `exists` / `stat` 包装函数
- dispatch 语料维持 139，`std_fs` eligible=61/61 fallback=0；
  聚合 fallback_rate=0.0000；dogfood 全绿；`cargo test --lib` 5479 passed

### 0.37.64 — `std::set` resolved 自递归修复 + 进入 module 白名单

- 修复 resolved native 下 `SetExt` 方法的自递归 trampoline：
  - `std::set` 的合成 impl 函数（`Set_size` / `Set_insert` / ...）其函数体
    是 `self.size()` / `self.insert(...)`，resolved ProtocolMethod 把该调用
    再次指向同一个 `Set_xxx` 符号，生成 `Set_size -> Set_size -> ...` 自递归
  - 现在协议方法解析对内置 `Set` 方法直接调用 `mimi_set_*` runtime，
    与 legacy `compile_set_method` 的内置优先级一致
  - 覆盖 `size` / `len` / `is_empty` / `contains` / `insert` / `remove` /
    `to_list`，含 i64→i32 截断、bool 比较、list struct 组装
- eligibility 接受 `ContainerErase`：typed `Set<T>`/`List<T>` 传给 bare
  `Set`/`List` 参数只改变类型 id、不改变 LLVM 布局，resolved emitter 已按
  identity 处理
- 默认 module-body 白名单加入 `set`
- `tests/real_world/std_set.mimi` 改为真实 `use std::set`，覆盖非 Any 包装面
  `size` / `is_empty` / `to_list`；Any 包装面（insert/contains/remove）仍由
  DynamicAnyPack 迁移负责
- 门禁：dispatch 语料维持 139，`std_set` eligible=55/55 fallback=0，
  聚合 fallback_rate=0.0000；dogfood 全绿；`cargo test --lib` 5479 passed

### 0.37.63 — `std::testing` 进入 resolved module 白名单 + 独立语料

- 默认 module-body 白名单加入 `testing`，`std::testing` 断言函数进入
  resolved native 切片
- 新增 `tests/real_world/std_testing.mimi`，覆盖
  `assert_true` / `assert_false` / `assert_eq_int` / `assert_ne_int` /
  `assert_eq_string` / `assert_eq_bool` / `assert_approx_eq_float`
- dispatch 语料 138→139，`std_testing` eligible=50/50 fallback=0；
  聚合 fallback_rate 维持 0.0000

### 0.37.62 — `std::array` / `std::json` / `std::maps` 进入 resolved module 白名单

- 默认 module-body 白名单加入 `array`、`json`、`maps`；对应标准库模块函数
  进入 resolved native 切片
- `tests/real_world/std_maps.mimi` 改为实际 `use std::maps` 并覆盖
  `new` / `size` / `has_key` / `remove` 等非 Any 包装函数；故意避开
  `get`/`set` 的 `Any` 参数转换面（DynamicAnyPack 后续独立迁移）
- `tests/real_world/std_json.mimi` 追加 `use std::json` 头，模块函数随语料
  进入 resolved 检查
- 新增 `tests/real_world/std_array.mimi`，覆盖 `array_new` /
  `array_set` / `array_get` / `array_reverse` / `array_concat` /
  `array_contains`
- dispatch 语料 137→138，`std_array` 59/59、`std_json` 65/65、
  `std_maps` 73/73 全部 zero fallback；聚合 fallback_rate 维持 0.0000

### 0.37.61 — `std::text` 进入 resolved module 白名单 + 独立语料

- 在 `to_float` i32 修复与 0.37.59 字符串所有权修复的双重闭环后，
  `std::text` 的模块函数（含 `TextExt for string` impl）已可全部走
  resolved native 切片
- `src/codegen/resolved/eligibility.rs` 默认 module-body 白名单加入 `text`
- 新增独立语料 `tests/real_world/std_text.mimi`，覆盖：
  `is_blank` / `is_numeric` / `slugify` / `indent_text` / `wrap_text` /
  `camel_to_snake`；其中 `camel_to_snake` 同时锁住 0.37.59 的
  `"" + cts_ch` 原生加固
- dispatch 语料 136→137，`std_text` eligible=57/57 fallback=0；聚合
  fallback_rate 维持 0.0000

### 0.37.60 — `std::random` 进入 resolved module 白名单 + `to_float` i32 直接整型转换

- 修复 native `to_float` 对 i32/i16 等窄整型的错误路径：此前所有 IntValue
  都走 `mimi_any_to_float(i64)` 的 Any 句柄启发式，i32 直接传给 i64 签名
  产生非法 LLVM IR，负 i32 还会被当作 usize 句柄输出超大 float
  - 现在 i64 仍走 Any 启发式；i32 及更窄整型改为 `sitofp` 直接转换，与
    Bytecode VM 的 `Value::Int → as f64` 语义对齐
  - `emit_any_to_int` / `emit_any_to_float` 增加统一 i64 参数提升，确保任何
    宽度的整型句柄调用都不会生成签名不匹配 IR
- 根因闭环：`std::random` 的 `random_int` / `random_choice` 等函数此前因
  `mimi_any_to_float(i32)` IR 非法被 resolved per-function verify 拒绝，
  导致 generic impl 的 `List_random_choice` 在链接期 undefined
- `src/codegen/resolved/eligibility.rs` 默认 module-body 白名单加入 `random`；
  `std::random` 全套模块函数进入 resolved native 切片
- 新增独立语料 `tests/real_world/std_random.mimi`，覆盖：
  `random_bool` / `random_int` / `random_float` / `random_choice` /
  `random_sample` / `shuffle` / `random_remove_ith`；
  eligible=53/53 fallback=0
- 新增 Rust 回归 `e2e_resolved_to_float_promotes_i32_handle`，锁定
  `to_float(7)` 与 `to_float(-3)` 的 native resolved 输出
- 门禁：dispatch 语料 135→136，聚合 fallback_rate=0.0000；dogfood 4 工程
  全绿；`cargo test --lib` 5479 passed

### 0.37.59 — resolved native 字符串所有权修复 + `std::text` camel_to_snake 原生加固

- 修复 resolved native codegen 在循环内字符串临时值生命周期错误：
  - `w = w + ch` / f-string / string-returning call 赋给局部变量时，不再把
    新堆串保留在“本次循环体”堆作用域末尾释放；改为把所有权转移到变量槽
    （函数根作用域），避免 `free_heap_allocs` 提前释放刚存入变量的字符串
  - `let ch = str_char_at(...)` 等 string-temp binding 同样执行所有权转移
  - `w = ch` 这类字符串变量到变量的赋值改为深拷贝目标数据，避免目标与
    per-iteration 堆槽位别名而在循环体退出后被释放
- 回归：新增 `e2e_native_string_build_loop_ownership`，覆盖 `w = w + ch`
  循环拼串与 `w = ch` 取末字符；`checked_codegen_compile_and_run` 验证
  native resolved slice 行为
- `std/text.mimi` `camel_to_snake` 原生加固：大写开新词时使用
  `"" + cts_ch` 建立独立堆字符串，替代直接别名循环局部的 `cts_ch`
  （`FooBar` → `foo_bar`、`HelloWorld` → `hello_world`；`mimi run` 与原生
  binary 输出一致）
- 门禁：`make test-dispatch-zero` 135 条语料聚合 fallback_rate=0.0000；
  `make test-dogfood` 4 工程全绿；`cargo test --lib flow_` 411 passed 与
  `cargo test --lib dual_` 999 passed

### 0.37.58 — `std::iter` 进入 resolved module 白名单 + 独立语料

- `src/codegen/resolved/eligibility.rs` 默认 module-body 白名单加入 `iter`，
  `std::iter` 模块函数进入 resolved native 切片
- 新增 `tests/real_world/std_iter.mimi` 独立语料，覆盖：
  `iter_range` / `iter_zip` / `iter_enumerate` / `iter_take` /
  `iter_drop` / `iter_chain` / `iter_repeat` / `iter_reversed` /
  `iter_count` / `iter_unique`
- dispatch 语料 134→135，`std_iter` eligible=54/54 fallback=0；聚合
  fallback_rate 维持 0.0000

### 0.37.57 — stress 原生多客户端 TCP echo 回归锁

- `tests/stress/mod.rs` 新增 `build_and_run_native` helper：构建 Mimi 原生
  binary 并运行；专门用于真实线程并发与阻塞 I/O 场景（`mimi run` 的 bytecode
  VM 仍按设计顺序执行 spawn/await，不适合服务器/客户端并发用例）
- 新增 `tests/stress/net_concurrency.rs::stress_native_multi_client_tcp_echo`：
  将 mimichat-modern 的并发多客户端 TCP echo 形状沉淀为独立 stress 回归，
  原生 binary 验证 3 客户端与 server 内部 3 个 echo handler 全部成功
- 该压力用例覆盖：嵌套 spawn、Channel 就绪同步、并发 socket 生命周期与
  多 task await

### 0.37.56 — mimichat-modern 并发多客户端 TCP echo 服务态

- mimichat-modern net 服务态从单 echo 升级为并发多客户端：
  - `server_echo` 用 `Channel<i64>` 对外广播监听就绪信号
  - 接受 3 个连接后，为每个 client socket `spawn echo_handler(client)`
  - 一个 server task 内部再派生多个 echo handler，验证真实线程并发
  - 主流程同时 `spawn` 3 个 `client_echo`，全部 `await` 后检查 4 个 task 结果
- 原生 binary 运行输出 `net: 0/0/0/0`，证明并发网络 task 全部成功
- 该扩展作为 Phase C「真实结构化并发运行时」的 dogfood 承压切片：
  嵌套 spawn、Channel 就绪同步、多路并发 socket 生命周期均由 real-thread
  runtime 完成

### 0.37.55 — Flow EventId 裸变体作用域收紧 + mimichat-modern TCP 服务态

- dogfood 新证据：在 Flow transition 名为 `accept` 时，普通函数里的网络
  builtin `accept(fd)` 被误编译为 Flow 的 EventId 枚举构造器（struct 值参与
  `if client < 0`），native build 报误导性 E0700「lt requires same numeric types」
- 根因：每个 Flow transition 都是 EventId 变体；legacy codegen 的
  `nominal_variant_enum` 无作用域回退会在普通函数编译期把同名变体当作裸构造器
- 修复：
  - Flow StateId/EventId 裸变体构造只在 Flow transition body 编译期间生效
    （`current_flow_name` 非空）
  - `compile_flow` 结束后清空 `current_flow_name` / `current_persistent_fields`
    / `current_from_state`，避免 Flow 编译状态泄漏到后续普通函数
- 新增 Rust 回归测试：`e2e_flow_transition_named_accept_does_not_shadow_builtin_accept`
- mimichat-modern 扩展为真实 TCP 服务态：`server_echo` / `client_echo` 基于
  `use std::net` 包装层（`tcp_listen` / `tcp_accept` / `tcp_connect` /
  `tcp_send` / `tcp_recv`），通过 `spawn` + `await` 在原生 binary 中完成一次
  echo 往返；`make test-dogfood` 与 `make test-dispatch-zero` 保持全绿
- `src/codegen/resolved/eligibility.rs` 的默认 module-body 白名单加入 `net`，
  `std::net` 包装函数进入 resolved native 切片；mimichat-modern dispatch 从
  55/71 提升到 71/71，继续零回退
- 新增独立语料 `tests/real_world/std_net.mimi`：`std::net` 全套 wrapper
  进入真实世界回归集，dispatch 语料 133→134，`std_net` 61/61 零回退

### 0.37.54 — Flow transition body 本地 List 显式注解 resolved 支持修复

- mimichat-modern dogfood 暴露：Flow transition 内 `let xs: List<string> = []`
  被 resolved typed-body lowering 以 TOOL-RESOLUTION-001 拒绝
- resolved 构建现在会持久化 transition 拥有的全部显式类型注解，与普通函数
  行为对齐；local annotation / checked conversion 可进入 type_operands 表
- mimichat-modern 恢复真实 `List<string> transcript` 状态字段，通过
  `join` / `accept` 拷贝传递，成为该修复的活体回归用例
- 新增 Rust 回归测试 `flow_transition_local_list_annotation_resolved`

### 0.37.53 — mimichat-modern dogfood 工程落地（Phase D 扩展载体首片）

- 新增手写现代 dogfood 工程 `projects/mimichat-modern`：
  - Flow `ChatSession`：加入/收消息/离开的状态机切片
  - Actor `RoomService`：房间与服务计数状态
  - 真实线程 `Channel<i64>` worker：`spawn send_id(ch, id)` / `await`
  - 大 payload `ChatMessage` 记录（含 `List<string>` flags）
- `make test-dogfood` 扩至 4 个工程：taskq + ledger + mimichat +
  mimichat-modern，check/test/build/run 全绿
- `scripts/dispatch_stat.py` 语料扩至 133 条；mimichat-modern
  53/53 全部 resolved，聚合 fallback_rate=0.0000

### 0.37.52 — mimichat 真实工程回归纳入 dogfood + dispatch 零回退语料

- `projects/mimichat`（v0.28 时代 61 项测试 / 230 个函数）加入
  `make test-dogfood`：check / test / build / 原生运行全绿
- `scripts/dispatch_stat.py` 语料加入 `projects/mimichat/src`：
  230/230 全部走 resolved，`legacy_fallback=0`
- 全语料扩至 132 条，聚合 fallback_rate=0.0000
- 作用：既保住 v0.28 真实工程的长期可编译回归，也为旧工程与 0.1.7
  新 dogfood 工程建立同一套零回退/可运行门禁

### 0.37.51 — E0256/E0304 资源来源诊断（Phase D 可用性切片 1）

- `ResourceFact` 现在携带 `introduced_span`：线性资源在 `let`/绑定引入时记录来源位置
- E0256（未消费/可能未消费）与 E0304（消费后再次移动/重复消费/use-after-move）
  自动附带 `note: resource 'x' introduced here` 来源锚点
- CFG join 传播来源位置：资源跨分支合并后仍可指回最初引入点
- 新增回归断言：E0256 必须带 introduced-note，避免未来回归丢来源

### 0.37.50 — 并发原语名义类型化（Phase C 切片 1）+ dogfood dispatch 语料接入

- 并发原语从裸 `i64` handle 升级为名义类型（运行时表示仍为 i64）：
  - `AtomicI32` / `AtomicI64` / `AtomicBool` / `Mutex<T>` / `MutexGuard<T>` / `Channel<T>`
  - 工厂/操作 builtin 在 checker 侧返回并消费对应名义类型
  - 新增 handle family 交叉使用检查：`atomic_i32_load(channel_new())`、
    `mutex_get(mutex_new(0))` 等混用现在于 `mimi check` 阶段报 E0242
  - 新增 value slot 类型检查：`atomic_i32_store(a, "x")`、
    `channel_send(c, true)`、`mutex_set(g, true)` 等载荷类型错误同样 E0242
  - `AtomicI64` 与 `Channel` 的 i64 数据面保持不变；MutexGuard 的线性深整合
    仍按计划留到 0.1.8（Phase C 范围铁律）
  - resolved native slice 将新名义类型降为 opaque i64，eligibility 同步放行
    新名义类型使并发原语程序保持零 legacy 回退；双后端等价保持
  - 新增 `tests/real_world/concurrency_nominal.mimi`：显式
    `AtomicI32` / `Mutex<i64>` / `Channel<i64>` 参数与调用
- `scripts/dispatch_stat.py` 语料纳入两个 0.1.7 dogfood 工程：
  - `projects/mimi-taskq/src/main.mimi`
  - `projects/mimi-ledger/src/main.mimi`
  - `make test-dispatch-zero` 全语料 131 条，聚合 fallback_rate=0.0000
- 新增 checker 正/负测试：合法名义类型表面可编译，跨 handle family 混用拒绝
- `tests/stress/real_spawn.rs` 的 channel worker 源码同步迁移到
  `Channel<i64>` 参数，`make test-stress` 52 项冒烟全绿
- 全量 `dual_` 999 项通过，`make test-dogfood` 通过

### 0.37.49 — 特性设计评估复盘落档 + 0.1.7 排期增补 + 0.1.8 规划锚定

- 新增 `devdocs/v0.37/feature-design-review-0.37.md`：三证据流特性设计评估复盘
  （保留/修改/砍除/元结论 + 裁决排期矩阵），含证据修正记录——projects/ 旧工程
  全部为 v0.28 时代 Flow 诞生前产物（git 时间线核实），使用频率论据作废，价值侧
  判断改判"证据真空"；根因定性为 dogfooding 管线干涸，**0.37.48 已由
  mimi-taskq / mimi-ledger 重启**（首个数据点：未出现被字段拷贝/嵌套调用劝退，
  transition 人体工学维持待验假设，0.1.8 裁决）。
- 0.1.7 排期增补（`devdocs/v0.37/README.md`，sprint 总量 125→135）：
  - Phase C 插入「并发原语名义类型化」切片（0.37.53–55 前置）：Mutex/Channel/
    Atomic 裸 i64 handle → 名义类型 + 专用 checker 规则；SD-2 重评第一步，
    不进泛型线性黑盒，MutexGuard 线性深整合留 0.1.8；
  - Phase D dogfood 扩展载体：重写 mimichat（actor/channel/net 服务态 +
    大 payload Flow）；DoD 新增验收维度——四大支柱各 ≥1 处非生成手写使用
    （0.37.48 已达成，Phase D 复核）；
  - Phase E 增补「边缘特性判死」（与既有"选择性解冻"对称）：quote 语法面
    （含 `$(...)` 插值）、Protocol 表面语法、Effect lattice 残余判死删除；
    spec 纸面特性出清（`ffi slice`/`ffi buffer`/MTE 标注 removed）；
  - Phase 编号顺延：C 53–79 / D 80–105 / E 106–120 / F 121–135。
- AGENTS.md §13.1 版本表锚定 0.1.8（内部 0.38.x：真实使用闭环 + 特性收缩
  裁决，规划中），指向 feature-design-review-0.37.md 裁决输入。

### 0.37.48 — 0.1.7 手写 dogfood：Flow / Session / 合约 / 线性真实验收靶

- 新增 `projects/mimi-taskq`：
  - Flow：任务生命周期 `Pending -> Running -> Completed/Failed`
  - Session：`Handoff = !i32 . ?i32 . !i32 . end` 类型化线性握手
  - 合约：`next_task_id` / `enqueue` / `priority_score` 的 requires/ensures
  - 线性：`SessionChan` 双端点严格按协议顺序消费并 close
  - 内置 3 个 `test_*`：flow / session / contracts 均通过 `mimi test`
- 新增 `projects/mimi-ledger`：
  - Flow：账户 `Active -> Frozen -> Active` 生命周期
  - Session：一次性 `Audit = !i32 . ?i32 . end` 审计通道
  - 合约：余额存取款的 requires/ensures
  - 内置 3 个 `test_*` 全通过
- 新增 `make test-dogfood` 门禁：
  - 两个项目依次执行 `mimi check` / `mimi test` / `mimi build` / 原生运行
- 目的：
  - 解除特性裁决的证据真空
  - 作为 DX 宣言的真实可读验收靶
  - 与高压 soak 互补的功能性 dogfood
- 当前结论：手写工程没有出现被字段拷贝/嵌套调用劝退的情况；
  随后再依据更多工程证据决定 transition 人体工学糖与 linear shirink 取舍。

### 0.37.47 — resolved 返回 `List<List<string>>` 递归所有权与字符串字面量复制

- 新增 `HeapEntry::StringListListData`：
  - 返回的 `List<List<string>>` 在 caller scope 退出时递归释放
  - 每个外层元素是内层 List 的 heap box 句柄
  - 依次释放内层每个 string、内层 data 数组、内层 box，最后释放外层 data 数组
- `track_returned_heap_pointers` / `claim_returned_heap_pointers` 识别 `List<List<string>>`：
  - caller 普通返回循环中不再泄漏内层字符串与 box（修复前 100k 次 RSS 约 416 MiB，修复后约 3 MiB）
  - 早退返回时通过 `claimed_returned_string_list_lists` 运行时遍历检查，跳过内层 box / data / string 指针的释放，避免 use-after-free
- `ensure_returned_heap_strings_owned` 升级为语义感知：
  - `List<string>` / `List<List<string>>` 的字面量字符串元素会在返回前堆拷贝
  - 修复返回 `["a","b"]`、`[["a","b"],["c","d"]]` 等字面量容器时 caller 对 `.rodata` 调用 `free` 崩溃的问题
- 新增 soak 测试：
  - `stress_soak_resolved_string_list_literal_return_ownership_smoke`
  - `stress_soak_resolved_list_of_string_list_return_ownership_smoke`
  - `stress_soak_early_return_list_of_string_list_ownership_smoke`
- resolved 44/44 编译，0 legacy 回退

### 0.37.46 — resolved 返回记录嵌套用户 Record 中的 `List<string>`

- `track_returned_heap_pointers` / `claim_returned_heap_pointers` 增加递归语义派生：
  - 通过 `TypeDef` 字段 display 反查 canonical `ResolvedTypeId`
  - Record 字段递归时继续携带子字段的 resolved type 上下文
  - 因此 `Outer { inner: Inner }` + `Inner { items: List<string> }` 也能把内层字符串列表注册为 `StringListData`
- 修复普通返回与早退返回嵌套 `Record<List<string>>` 时约 202 MiB 的累积泄漏：
  - 100k 次返回 `Outer { inner: Inner { items: [两段 1000 字符字符串] } }`
  - RSS 修复后约 3 MiB
- 新增 soak 测试：
  - `stress_soak_resolved_nested_heap_record_string_list_return_ownership_smoke`
  - `stress_soak_early_return_nested_heap_record_string_list_ownership_smoke`
- resolved 44/44 编译，0 legacy 回退

### 0.37.45 — resolved 早退 `Record<List<string>>` / `List<string>` 元素级所有权转移

- 修复早退返回 `List<string>` 或含 `List<string>` 字段的 Record 时的 double free：
  - callee 的早退 flush 会释放循环体局部字符串 data；若返回值内嵌这些字符串，caller 后续再释放会 double free
  - 新增 `claimed_returned_string_lists` 与 `claim_returned_string_list`
  - `flush_heap_scopes_to_boundary` 现在对每个待释放指针做运行时 “claimed string list 成员检查”
  - 命中已转交字符串列表元素的指针跳过 free，同时原列表 data 指针继续走普通 claim
- 新增 soak 测试：
  - `stress_soak_early_return_heap_record_string_list_ownership_smoke`
    - 100k 次早退返回含两个 1000 字节字符串元素的 `Bag { items: List<string> }`
    - RSS < 128 MiB
- resolved 44/44 编译，0 legacy 回退

### 0.37.44 — resolved 返回 `Record<List<string>>` / `List<string>` 的元素级释放

- 新增 `HeapEntry::StringListData` 与 `register_returned_string_list`：
  - 返回的 `List<string>` 不再只注册 data 数组
  - caller scope 退出时运行时循环逐元素调用 `mimi_string_free`
  - 最后释放 data 数组本身
- `track_returned_heap_pointers` 增加语义感知：
  - 顶层 `List<string>` 返回走 StringListData
  - Record 直接字段若为 `List<string>` 也走 StringListData
  - 避免把字符串 data 当成普通 data 数组释放导致泄漏或错误
- 新增 soak 测试：
  - `stress_soak_resolved_heap_record_string_list_return_ownership_smoke`
    - 100k 次返回含两个 1000 字节字符串元素的 `Bag { items: List<string> }`
    - RSS < 128 MiB（修复前约 202 MiB）
- resolved 44/44 编译，0 legacy 回退

### 0.37.43 — resolved 含堆 Record 返回值的嵌套 String 所有权修复

- 修复函数返回 `Record { name: string, nums: List<i32> }` 时：
  - 如果 `name` 指向 `.rodata` 字符串字面量，callee 直接返回后 caller 会在 scope 退出时 `free` 非堆指针 → `munmap_chunk(): invalid pointer`
  - 新增 `ensure_returned_heap_strings_owned`：递归遍历返回结构体的 String 叶子，对每个叶子调用 `claim_resolved_string_return`，把 `.rodata` 字面量转换为 caller 可安全释放的堆拷贝
  - 同时接入早退路径：早退返回含堆 Record 前先完成嵌套 String 所有权转换，再 claim 所有堆指针叶子
- 新增 soak 测试：
  - `stress_soak_resolved_heap_record_return_ownership_smoke`：100k 次返回含 807 字符 String + 1000 元素 List 的 Record，RSS < 512 MiB
  - `stress_soak_early_return_heap_record_ownership_smoke`：早退返回同类 Record，100k 次循环，RSS < 512 MiB
- resolved 44/44 编译，0 legacy 回退

### 0.37.42 — resolved 调用返回值的堆所有权与早退 List 返回修复

- 修复 resolved 早退路径把 `return xs` 的 List data 在返回前 free 的 use-after-free：
  - 新增 `claim_returned_heap_pointers`：提取返回值中所有堆指针叶子并作为“已转交”guard
  - `flush_heap_scopes_to_boundary` 对这些指针执行 skip-free
  - IR 验证：早退分支不再直接 `free(list_data)` 后再 `ret`
- 修复 resolved 函数调用返回值的 caller 侧堆注册缺失：
  - 新增 `track_returned_heap_pointers`：对返回 String / List / 含堆字段 Record 的每个指针叶子调用 `register_heap_alloc`
  - 此前 caller 每轮循环丢弃旧返回值会导致 List/String 数据无限累积
  - 现在 caller scope 退出时统一释放
- 新增 soak 测试：
  - `stress_soak_resolved_list_return_ownership_smoke`：100k 次返回 1000 元素 List，RSS < 512 MiB（修复前约 785 MiB）
  - `stress_soak_resolved_string_return_ownership_smoke`：200k 次返回 1000 字符 String，RSS < 128 MiB
  - `stress_soak_early_return_list_ownership_smoke`：早退返回 1000 元素 List，100k 次循环，RSS < 512 MiB
- resolved 44/44 编译，0 legacy 回退

### 0.37.41 — real-thread spawn 支持 List<含堆字段 Record/Struct> 实参

- 将 `List<Record>` 深拷贝从纯标量元素扩展到含 String / List<i32|f64> 字段的元素：
  - 运行时逐元素深拷贝每个 record 的堆叶子路径
  - 每元素 String 逐个 `mimi_str_clone`
  - 每元素标量 List 深拷贝 data 数组
  - 重建 record 后写入新 box 与新 data 数组
- worker 后逐元素释放：
  - 所有克隆 String
  - 所有克隆 List data
  - record box
  - 外层 data 数组
- 新增测试：
  - `stress_real_spawn_heap_record_list_smoke`：两个 `Inner { name, nums }` 元素 → `9`
  - `stress_real_spawn_heap_record_list_matches_eager_semantics_smoke`：20 次循环，real/eager 均输出 `180`
  - `stress_real_spawn_heap_record_list_heavy`（ignored）：100 个 real-thread spawn，输出 `900`
- resolved 44/44 编译，0 legacy 回退

### 0.37.40 — real-thread spawn 支持 List<纯标量 Record/Struct> 实参

- 新增 `List<Record>` worker 实参支持（当前覆盖纯标量 Record 元素）：
  - 运行时逐元素从源列表 box 中 `load` Record 结构
  - 为每个元素分配新 box 并整值复制，写入新的 data 数组
  - worker 结束后释放所有新 box 与新 data 数组
  - 调用方原始列表及其 box 不受影响
- 修正检测：List 整体不再被误判为“含堆 Struct 实参”，避免空路径导致 resolved 验证 panic
- 修正实参安全门：允许 `is_list && elem_size == 0 && is_struct_list`
- 新增测试：
  - `stress_real_spawn_scalar_struct_list_smoke`：`List<Point>`，`[1+2, 3+4]` → `10`
  - `stress_real_spawn_scalar_struct_list_matches_eager_semantics_smoke`：20 次循环，real/eager 均输出 `200`
  - `stress_real_spawn_scalar_struct_list_heavy`（ignored）：100 个 real-thread spawn，输出 `1000`
- resolved 44/44 编译，0 legacy 回退

### 0.37.39 — real-thread spawn 支持嵌套含堆字段 Record/Struct 实参

- 将 Record/Struct 深拷贝信息从“顶层字段索引”升级为“递归字段路径”：
  - 收集 `[[嵌套字段...], 叶子字段]` 路径，支持任意深度的 String / List<i32|f64> 叶子
  - spawn 现场按路径逐叶深拷贝并自底向上重建嵌套 struct
  - worker 结束后按路径递归取出所有克隆 String/List data 并释放
- 用例：`Outer { inner: Inner { name: string, nums: List<i32> }, tag: i32 }`
  - `Inner` 的 `name` 与 `nums` 都深拷贝到 worker env，`tag` 按值复制
- 新增测试：
  - `stress_real_spawn_recursive_heap_struct_smoke`：`len("hi") + len([1,2,3]) + 10` → `15`
  - `stress_real_spawn_recursive_heap_struct_matches_eager_semantics_smoke`：20 次循环，real/eager 均输出 `300`
  - `stress_real_spawn_recursive_heap_struct_heavy`（ignored）：100 个 real-thread spawn，输出 `1500`
- resolved 44/44 编译，0 legacy 回退

### 0.37.38 — 早退 return 路径的确定性堆释放

- resolved 的每个早期 `return` 现在在 `ret` 前调用 `flush_heap_scopes_to_boundary`：
  - 释放函数边界内所有已注册堆分配（含循环体局部 List/String）
  - 不弹出编译期作用域栈，仍由 `end_function_heap_scope` 统一平衡簿记
  - 返回值所有权由 `claimed_returned_envs` 保持，不误释放返回给调用方的堆数据
- 修复循环内 `return` 未释放本次迭代堆分配的问题：
  - 之前 break/continue 已即时释放，return 路径仍会推迟到函数尾（且实际上没有发射 free）
  - 现在与 break/continue 同类路径全部即时、确定性释放
- 新增 `stress_soak_early_return_loop_drop_smoke`：
  - 80,000 次函数调用，每次循环内构造 1000 元素 List 后早退
  - 若早退泄漏该 List，预计累计 >640 MiB；native RSS 门限 512 MiB，实测通过

### 0.37.37 — real-thread spawn 支持嵌套纯标量 Record/Struct 实参

- 递归识别“无堆结构”：
  - 新增 `llvm_type_has_heap`，判断 struct 是否传递包含指针字段
  - Record 字段即使嵌套了多个 Record，只要最终全是标量，就被视为 scalar-like
  - spawn 现场直接按值复制到 worker env，无需深拷贝/释放
- 同时修复 resolved `resolve_type_display` 对显式用户 Record 名的解析：
  - 除 `id.as_str()` 与 `format!("{ty:?}")` 外，也匹配 `NominalTypeId` 的原名与 `type:` 前缀
  - `Point` / `Line` 这类嵌套 Record 字段显示名不再导致 legacy 回退
- 新增测试：
  - `stress_real_spawn_nested_scalar_struct_smoke`：`Line { a: Point{}, b: Point{} }` → `4`
  - `stress_real_spawn_nested_scalar_struct_matches_eager_semantics_smoke`：20 次循环，real/eager 均输出 `80`
  - `stress_real_spawn_nested_scalar_struct_heavy`（ignored）：100 个 real-thread spawn，输出 `400`

### 0.37.36 — real-thread spawn 支持含 List 字段的 Record/Struct 实参

- 扩展 Record/Struct worker 实参深拷贝到 `List<i32|f64>` 字段：
  - 标量字段按值复制
  - String 字段 `mimi_str_clone`
  - List 字段深拷贝 data 数组（`len * 8` memcpy），写入新 list 结构
  - worker 后释放所有克隆的 String 与 List data
- 同时修复 resolved record 类型构建对 `List<T>` 字段的显示名解析：
  - `resolve_type_display` 现在识别 `List<...>` 并返回统一 list ABI `{i64, ptr}`
  - 含 List 字段的 Record 不再回退 legacy；本次用例 resolved 44/44
- 新增测试：
  - `stress_real_spawn_heap_struct_list_deep_copy_smoke`：`Bag { items: [1,2,3], label: "hi" }` → `8`
  - `stress_real_spawn_heap_struct_list_matches_eager_semantics_smoke`：20 次循环，real/eager 均输出 `160`
  - `stress_real_spawn_heap_struct_list_heavy`（ignored）：100 个 real-thread spawn，输出 `800`

### 0.37.35 — real-thread spawn 支持含 String 字段的 Record/Struct 实参

- 新增含 `string` 字段的 Record/Struct worker 实参支持：
  - 标量字段按值复制，字符串字段逐个 `mimi_str_clone` 深拷贝
  - worker 读完实参后释放所有克隆的字符串字段
- 识别条件：struct 字段全为标量或规范 String 形态 `{ptr, i64}`，且至少一个 String 字段
- 新增测试：
  - `stress_real_spawn_heap_struct_string_deep_copy_smoke`：`User { name: "hello", age: 5 }` → `10`
  - `stress_real_spawn_heap_struct_string_matches_eager_semantics_smoke`：20 次循环，real/eager 均输出 `200`
  - `stress_real_spawn_heap_struct_string_heavy`（ignored）：100 个 real-thread spawn，输出 `1000`
- 真实 spawn 实参覆盖扩展到含堆指针字段的用户定义 Record；resolved 44/44 编译，0 legacy 回退

### 0.37.34 — break/continue 路径的循环体即时释放

- 新增 `CodeGenerator::emit_frees_for_top_scope`：
  - 只发射当前最内层堆作用域的 free，不弹出编译期作用域栈
  - 供 break/continue 提前离开迭代时使用，同时保持后续分支/回边簿记平衡
- resolved 的 `break` / `continue` 现在先释放本轮循环体堆分配，再跳转：
  - 避免了提前退出路径把循环局部 List/堆分配推迟到函数退出
  - 运行时路径互斥：break 分支的 free 与正常回边的 free 不会在同一次迭代同时执行
- 新增 `stress_soak_loop_break_continue_drop_smoke`：
  - break：`for i in 0..100` 中创建 List 后在 `i==3` break，输出 `0`
  - continue：偶数迭代跳过，奇数迭代累加 List 长度，输出 `10`

### 0.37.33 — real-thread spawn 支持 List<List<string>> 实参深拷贝

- 扩展嵌套 List 深拷贝到内层为 String：
  - `List<List<string>>` 三层/四层所有权全部由 worker env 接管：
    - 外层 data 数组
    - 内层 List box
    - 内层 data 数组
    - 每个字符串元素
  - spawn 现场内层为 String 时逐元素 `strlen` + `mimi_str_clone`
  - worker 结束后内层先逐个 `mimi_string_free`，再释放内层 data/box、外层 data
- 新增测试：
  - `stress_real_spawn_nested_string_list_deep_copy_smoke`：`[["a","bb"],["ccc","dddd",""]]` → `10`
  - `stress_real_spawn_nested_string_list_matches_eager_semantics_smoke`：20 次循环，real/eager 均输出 `200`
  - `stress_real_spawn_nested_string_list_heavy`（ignored）：100 个 real-thread spawn，输出 `1000`
- 真实 spawn 实参覆盖扩展到三层嵌套 String 容器；resolved 44/44 编译，0 legacy 回退

### 0.37.32 — real-thread spawn 支持 List<List<i32|f64>> 实参深拷贝

- 新增 `List<List<T>>`（T 为 i32/f64）worker 实参支持：
  - spawn 现场按两层深拷贝：
    - 外层 data 数组整体新建
    - 每个内层 List 深拷贝 data 缓冲并重新堆装箱，写入新外层
  - worker 读完实参后按两层释放：内层 data、内层 box、外层 data
- 新增测试：
  - `stress_real_spawn_nested_list_deep_copy_smoke`：`[[1,2],[3,4,5],[6]]` → `21`
  - `stress_real_spawn_nested_list_matches_eager_semantics_smoke`：20 次循环，real/eager 均输出 `420`
  - `stress_real_spawn_nested_list_heavy`（ignored）：100 个 real-thread spawn，输出 `2100`
- 真实 spawn 实参覆盖扩展到双层嵌套标量 List；resolved 44/44 编译，0 legacy 回退

### 0.37.31 — real-thread spawn 支持 List<string> 实参深拷贝

- 新增 `List<string>` worker 实参支持：
  - 在 spawn 现场为每个字符串元素执行 `strlen` + `mimi_str_clone`，写入新的 data 数组
  - worker 内读完实参后，运行时循环逐个 `mimi_string_free`，再 `free` data 数组
  - 与 String 实参、标量 List 实参同一套“worker 深拷贝 → 用完释放”所有权模型
- 新增 `stress_real_spawn_string_list_deep_copy_smoke`：
  - `spawn total_len(["hello", "world", "!"])` → `11`
  - resolved 44/44 编译，0 legacy 回退
- 新增 real/eager 语义一致性及 heavy：
  - `stress_real_spawn_string_list_matches_eager_semantics_smoke`：20 次循环，两个后端均输出 `120`
  - `stress_real_spawn_string_list_heavy`（ignored）：200 个 real-thread spawn，输出 `1200`
- 真实线程 spawn 的实参覆盖扩展到：String / List<i32・f64> / List<string> / 标量 Tuple/Struct

### 0.37.30 — 循环体内 List 确定性即时释放 + 嵌套 List push 深拷贝

- 修复 `push(rows, row)` 传入 `List<T>` 值（StructValue 形态）时只浅拷贝
  `{len, data}` 结构、与源列表共享 data 缓冲的问题
  - 现在按 `PointerValue` 形态同样深拷贝内层 data 数组，再堆装箱写入外层
  - `tricky_nested_loop_list` 等把循环内 List 移交外部容器的用例保持正确
- 为 `for range`、`for list`、`while`、`loop` 的循环体添加每轮堆作用域：
  - 每次迭代结束立即 `free_heap_allocs()`，释放本轮循环局部 List/堆分配
  - 提前 return/break 路径用 `drain_heap_scope()` 平衡编译期簿记
  - 不再把循环内临时 List 的释放推迟到整个函数退出
- 原生二进制 soak 保持绿色：20 万次 List<String> 循环 RSS 远低于 512 MiB

### 0.37.29 — 原生二进制 List/String 内存 soak

- 新增 `build_and_run_native_with_max_rss_kb` 压力工具：
  - `mimi build` 产出原生可执行文件
  - `/usr/bin/time -v` 只统计运行期 RSS，不含编译器/JIT 进程头部开销
- 新增 `stress_soak_native_list_string_loop_memory`：
  - 20 万次循环创建/丢弃 `["a", "b", "c"]`
  - 原生二进制输出 `0`，max RSS 低于 512 MiB（实测远低于该上限）
- 说明：此前 `mimi run` 的 RSS 数字混入了编译器/LLVM 进程内存；
  原生二进制 soak 更准确反映运行期堆行为。

### 0.37.28 — 真实线程 spawn + Channel 数据面打通

- 新增 Channel 工作线程压力：
  - 50 个 real-thread worker 各自 `channel_send` 到同一 channel
  - main 顺序 `channel_recv` 50 次，输出 `1225`
  - heavy：500 个 real-thread worker，输出 `124750`
- 接收顺序不确定但总和确定，验证真实并发数据面可用
- 全部程序 resolved 44/44 编译，无 legacy 回退

### 0.37.27 — 真实/eager 语义一致性验证

- 新增 `stress_real_spawn_matches_eager_semantics_smoke`
  - 同一 `spawn_sum_source(100)` 分别用默认真实线程与 `MIMI_EAGER_SPAWN`
    编译运行，两个输出必须一致
  - both 4950，语义一致
- 通过该测试发现并修正测试自身对 0..99 求和的错误期望（5050→4950）

### 0.37.26 — 真实 spawn 堆返回结果链路验证

- 新增 `stress_real_spawn_list_return_smoke`：worker 返回 `List<i32>`
  后由调用方 `await` 读取，确认堆结果从真实线程传回调用方可用
- 此前已覆盖 String 返回；现在标量、String、List 三类真实 spawn 返回都
  有独立压力冒烟

### 0.37.25 — 真实 spawn 支持标量元组/Struct 实参

- 纯标量字段的 Tuple/Struct（字段全为 Int/Float）作为 `spawn f(arg)` 实参时，
  可直接按值复制到 worker env，不需要堆深拷贝，无共享引用风险
- 非标量字段的 Tuple/Struct（含 String/List/指针/嵌套容器）仍回退 eager
- 新增 `stress_real_spawn_scalar_tuple_arg_smoke`

### 0.37.24 — 真实 spawn 支持标量 List 实参深拷贝

- `List<i32>`/`List<f64>` 等标量元素 List 作为 `spawn f(list)` 实参时：
  - 调用点深拷贝整个数据缓冲（`len * elem_size` memcpy）
  - worker 线程调用目标后 `free` 数据缓冲
  - 不会与调用方共享底层 list data，避免 use-after-free
- 非标量元素/嵌套容器 List 仍安全回退 eager
- 新增 `stress_real_spawn_list_scalar_deep_copy_smoke`
- 新增 `MIMI_EAGER_SPAWN=1` 逃生口 smoke 测试

### 0.37.23 — Future 句柄指针化，resolved `spawn`/`await` main 全链路真实线程

本轮修正了长期阻断 resolved main 使用真实 spawn 的 ABI 错位：
- `builtin:type:Future` 在 resolved 类型层从 i64 改为 opaque `ptr`
  - 与 legacy 的 future 句柄 ABI 一致
  - `let t = spawn f(...)` 绑定不再发生 Pointer→i64 非法转换
  - resolved main 编译从 43/44 提升到 44/44，spawn/await 不再回退 legacy
- 真实 spawn 参数深拷贝：
  - `String` 实参在调用点通过 `mimi_str_clone` 深拷贝到 worker env
  - poll 线程调用目标后由 `mimi_string_free` 释放副本，避免调用方
    释放字符串后子线程仍读取的 use-after-free
  - 其他 Struct/Array 实参仍安全回退 eager
- `mimi_string_free` 注册进 resolved/legacy 共享 runtime 声明表
- 更新 21 个 golden IR 基线（新增 `declare void @mimi_string_free(ptr)`）

### 0.37.22 — 真实 spawn 参数安全边界

真实线程 env 目前只安全地复制标量与不透明指针（i32/f64/actor handle 等）。
`String`/`List`/闭包等堆指针容器若作为实参会等待深拷贝 ABI 后再启用实线程，
当前安全地退回 eager 路径，避免调用方在子线程读取前释放参数堆内存。
- 新增测试覆盖 string 参数 spawn 仍可正常执行（eager 回退）
- actor+spawn 混合、500 spawn/await 压力仍走真实线程

### 0.37.21 — Phase C：真实线程 spawn 成为默认路径

`spawn f(args...)` 的直接命名函数调用现在默认由真实工作线程执行：
- 移除 `MIMI_REAL_SPAWN=1` 实验门；默认尝试真实线程。
- 保留 `MIMI_EAGER_SPAWN=1` 作为旧 eager 同步求值的调试逃生口。
- 非直接命名函数调用、含 View/Mutate 借用参数的调用仍走 eager 路径。
- 全量验证在默认 real-spawn 模式下通过：
  - `cargo test --lib`：5474 passed / 0 failed / 6 ignored
  - `cargo test --test real_world`：31 passed / 0 failed
  - `make test-stress-heavy`：5 passed（500 spawn/await、
    2000 actor+spawn 混合、2000 actor 链等）
- 全语料 codegen 仍保持 aggregated 0 fallback。

### 0.37.20 — Phase C 前哨：真实线程 spawn（实验开关）

为 resolved 增加 `MIMI_REAL_SPAWN=1` 实验路径，把常见的
`spawn f(args...)` 从 eager 同步求值升级为真实线程执行：

- **适用范围**：spawn 操作数为直接命名函数调用，且参数不含
  View/Mutate 借用；非该形态继续走 eager/synchronous 路径。
- **实现**：
  - 在调用点把参数值堆分配到 env，存入 future 数据区
  - 生成 `void(ptr future)` poll 函数，在线程内加载参数、调用目标
    函数、写回结果、`mimi_future_set_completed`
  - 调用运行时 `mimi_spawn_future` 启动真实线程，`await` 复用现有
    `mimi_await_future`
- **压力测试**：
  - `tests/stress/real_spawn.rs`：`MIMI_REAL_SPAWN=1` 下 50/500 个
    real-thread spawn/await 全部通过
  - actor+spawn 混合 200 组在 real-spawn 下输出仍为 19900
- 默认未开启，保持原有 eager 行为与全语料确定性。

### 0.37.19 — CI: 零回退硬门禁

按 Phase A DoD 补上“任何新 Legacy 分发都会被拦截”的硬门禁：

- `scripts/dispatch_stat.py check --zero`
  - 在原有“无静默回退率上升”基础上，额外要求每个语料程序的
    `legacy_fallback == 0`
  - 新程序即使首次纳入，只要仍有 legacy 回退也会失败
- Makefile 新增 `test-dispatch-zero` 目标，便于 CI 调用
- 当前全量 120 个成功语料全部 0 fallback，门禁通过

### 0.37.18 — Stress: Actor + Spawn/Await 混合链路

新增 `tests/stress/actor_spawn.rs`，把 actor mailbox 与 spawned future
放在同一个压力用例里跑：

- `stress_actor_spawn_mixed_smoke`：200 组 `spawn task(w, i)` /
  `await t`，常驻
- `stress_actor_spawn_mixed_heavy`：2000 组，`--ignored`
- 计算结果 `sum(0..200)=19900` / `sum(0..2000)=1999000`
- 两者均通过

### 0.37.17 — Phase A: 默认全语料同样 0 fallback

继续清掉最后两类回退：

- **User 限定名符号**：资格检查只对 non-User 的 `::` 或 non-User 泛型
  函数保持 `generics/qualified`；User-origin 的 actor 方法（如
  `Counter::increment`、`BankAccount::deposit`）进入 resolved 切片。
  - `demos/13_actors.mimi`：54/54 eligible、0 fallback、运行正确
- **模块函数**：默认 `MIMI_RESOLVED_MODULE_BODIES` 允许表加入 `result`，
  使 `examples/guess.mimi` 中来自 `std::result` 的 14 个模块函数进入
  resolved。
  - `examples/guess.mimi`：74/74 eligible、0 fallback
- **全语料更新**：
  - 默认：`legacy_fallback 40→0`、fallback_rate 0.00686→0.0；
    `total_functions 5832 / eligible 5832`
  - Reachable 维持 345/345 eligible、0 fallback
- `classify --check` 已刷新并保持通过。

### 0.37.16 — Phase A: 捕获闭包与 reduce 全量进入 resolved

本轮实现 resolved 捕获闭包（capturing lambda）并补上 `reduce` 内联循环，
把默认全语料回退从 402 压缩到 40：

- **闭包环境**：`emit_lambda` 现在为捕获变量堆分配 `{fields...}` env，
  闭包体从 env_ptr 加载捕获值；闭包结构仍为 `{fn_ptr, env_ptr}`，与
  legacy ABI 一致。
  - 支持直接捕获、返回闭包两种路径
  - `make_adder` / 直接 `base` 捕获均 0 fallback 且运行正确
- **门禁**：`ResolvedExprKind::Lambda` 不再拒绝非空 `captures`；捕获
  local 必须存在于当前 resolved frame，否则 fail-closed。
- **reduce**：新增 `emit_resolved_reduce`，对
  `reduce(list, fn, init)` 生成列表循环 + 闭包间接调用，使
  `List_count`（Trait 方法）也能由 resolved 发射，不再出现
  `List_count` undefined symbol。
- **验证程序**：
  - `/tmp` 捕获简单/返回闭包用例：0 fallback、输出正确
  - `a1_verification.mimi` / `std_collections.mimi`：编译运行通过
- **全语料更新**：
  - 默认：`legacy_fallback 402→40`、fallback_rate 0.0696→0.00686；
    `unsupported_type (capturing lambda)` 362→0
  - 剩余：`generics/qualified` 26、`module/source_id` 14
- `classify --check` 已刷新并保持通过。

### 0.37.15 — Phase A: reachable 全语料 resolved 0 fallback

移除 method-level generic trait method 的 resolved 门禁拒绝：这类调用现在
进入 resolved ProtocolMethod 路径，若某个具体方法无法由 resolved 发射器
处理，仍会由 per-function dispatch 自动回退 legacy monomorphization。

- **门禁**：Call 分支不再因 `trait_method_generic_count > 0` 直接拒绝。
- **效果**：
  - `map_list`（`xs.map(f)`）在 `a1_verification.mimi` /
    `std_collections.mimi` 中 0 fallback
  - **reachable 全语料 345/345 eligible、0 fallback、0 emit_failed**
- **全语料更新**：
  - reachable：`legacy_fallback 2→0`、fallback_rate 0.0058→0.0
  - 默认：`legacy_fallback 406→402`、fallback_rate 0.0696→0.0689；
    `generics/qualified` 30→26
- `classify --check` 已刷新并保持通过。

### 0.37.14 — Phase A: 用户泛型函数以 opaque i64 擦除进入 resolved

本轮把用户侧泛型函数（非 stdlib 非方法级泛型 trait 方法）放入 resolved
per-function 切片，并采用 opaque i64 擦除：

- **门禁**：`eligible_function_ids_with_stats` 只对 qualified 或
  非 User origin 的泛型函数继续记 `generics/qualified`；User-origin
  泛型函数进入逐函数 eligibility。
- **类型层**：`ResolvedType::GenericParameter` 在
  `require_scalar_type` 中放行，并在 `types.rs` 降低为 opaque i64。
- **效果**：
  - `demos/03_functions.mimi`：8/8 eligible、0 fallback
  - `tests/real_world/hm_core.mimi`：6/6 eligible、0 fallback
  - `tests/real_world/std_prelude.mimi`：5/5 eligible、0 fallback
  - 运行输出全部正确（identity/choose/pair/singleton/apply/swap 等）
- **全语料更新**：
  - reachable：`legacy_fallback 14→2`、fallback_rate 0.0406→0.0058；
    剩余 2 个为 method-level generic trait method（map/filter 等）
  - 默认：`legacy_fallback 1465→406`、fallback_rate 0.2512→0.0696；
    `generics/qualified` 1420→30；`unsupported_type` 40→362
- `classify --check` 已刷新并保持通过。

### 0.37.13 — Stress: Actor 邮箱链高压用例

新增 `tests/stress/actor_stress.rs`，覆盖 resolved `mimi_actor_call`
路径的连续调用稳定性：

- `stress_actor_mailbox_inc_chain_smoke`：200 次顺序 `inc()`，常驻
- `stress_actor_mailbox_inc_chain_heavy`：2000 次顺序 `inc()`，`--ignored`
- 两者均通过，重负载耗时约 0.8s

### 0.37.12 — Phase A: comptime 块进入 resolved 切片

`comptime { expr }` 此前因 `COMPTIME-PURE-001` backend requirement 被
resolved 门禁整体拒绝。该 requirement 只声明“应在编译期求值”；本轮让
resolved 发射器以运行时求值方式处理纯 comptime 块，消除最后一个
非泛型 reachable 回退：

- **门禁**：仅当表达式为 `Comptime` 且全部 requirement 均为
  `COMPTIME-PURE-001 / comptime.evaluate` 时放行；其他 backend
  requirement 仍 fail-closed。
- **发射器**：`Comptime(block)` 与嵌套 `Block` 共享内联块求值路径。
- **验证程序**：
  - `tests/real_world/meta_comptime_quote.mimi`：reachable 1/1
    eligible、0 fallback，运行退出 0
- **全语料更新**：
  - reachable：`legacy_fallback 15→14`、fallback_rate 0.0435→0.0406；
    `unsupported_expression` 1→0
  - 默认：`legacy_fallback 1466→1465`、fallback_rate 0.2514→0.2512；
    `unsupported_expression` 1→0
- `classify --check` 已刷新并保持通过。
- 当前 reachable 剩余回退全部为 `generics/qualified`（14 个函数）。

### 0.37.11 — Phase A: Spawn/Await future 处理进入 resolved 切片

本轮为 resolved 切片加入 `spawn` / `await` 的 eager future 路径，
把剩余回退压缩到 `generics/qualified + 1`：

- **门禁**：`require_expr` 接受 `ResolvedExprKind::Spawn` 与
  `ResolvedExprKind::Await`。
- **发射器**：
  - `emit_spawn()`：在当前线程同步求值 spawn 表达式，将结果写入
    future 数据区，并立即 `mimi_future_set_completed`；返回 future
    指针。该路径暂不生成独立 poll 线程，值与 ABI 与 legacy future
    布局保持一致。
  - `emit_await()`：调用 `mimi_executor_run` / `mimi_await_future`，
    从 offset 16 加载结果，并 `mimi_future_free` 释放 future。
- **验证程序**：
  - `examples/parasteps_on_failure_test.mimi`：reachable 6/6 eligible、
    0 fallback，运行退出 0
  - `tests/real_world/concurrency_spawn_await.mimi`：reachable 2/2
    eligible、0 fallback，运行退出 0
- **全语料更新**：
  - reachable：`legacy_fallback 17→15`、fallback_rate 0.0493→0.0435；
    `unsupported_expression` 3→1
  - 默认：`legacy_fallback 1468→1466`、fallback_rate 0.2517→0.2514；
    `unsupported_expression` 3→1
- `classify --check` 已刷新并保持通过。

### 0.37.10 — Phase A: Actor 邮箱方法调用进入 resolved 切片

本轮补上 resolved 发射器的 `mimi_actor_call` 邮箱 ABI，使所有以
actor 方法调用为主的测试程序完全脱离 legacy 回退：

- **类型层**：actor handle 从临时 i64 调整为 opaque pointer，与 legacy
  actor runtime 的 `i8*` ABI 对齐。
- **门禁**：`require_expr` 的 Call 分支接受 `ResolvedCallee::ActorMethod`。
- **发射器**：
  - 新增 `emit_actor_method_call()`：复用 `actor_method_ids` /
    `actor_defs` / `actor_abi_type_for` / `actor_abi_slot_size`，
    将参数压入固定 `i8` blob，调用 `mimi_actor_call`，再按方法返回类型
    从结果 blob 读回。
  - `actor_abi_type_for` / `actor_abi_slot_size` 提升为
    `pub(in crate::codegen)` 供 resolved 子模块复用。
- **验证程序**（reachable 全部 0 fallback）：
  - `demos/13_actors.mimi`、`examples/actor_full_test.mimi`
  - `examples/validation_concurrency.mimi`、`tests/real_world/concurrency_actor.mimi`
  - `tests/real_world/flow_actor_lifecycle.mimi`、`flow_broadcast.mimi`
  - `flow_delegate_channel.mimi`、`flow_mailbox_bp.mimi`
  - `flow_producer_mute.mimi`、`flow_test_sandbox.mimi`
- **全语料更新**：
  - reachable：`legacy_fallback 28→17`、fallback_rate 0.0812→0.0493；
    `unsupported_expression` 14→3
  - 默认：`legacy_fallback 1479→1468`、fallback_rate 0.2536→0.2517；
    `unsupported_expression` 14→3
- `classify --check` 已刷新并保持通过。

### 0.37.9 — Phase A: Actor/Future opaque 与自定义 Ok/Err `?` 传播进入 resolved

本轮同时推进了异步/并发的类型门禁和错误传播，把 resolved 回退推进到
10% 以下：

- **Actor/Future opaque**：
  - `require_scalar_type` 接受 `actor:*` 与 `builtin:type:Future` 为
    opaque handle；`types.rs` 将其降低为 i64。
  - 使 `flow_spawn_quota.mimi` 完全 resolved（1 个函数 0 fallback），
    其余 actor 程序从 unsupported_type 移到更精确的 unsupported_expression。
  - reachable 的 `unsupported_type` 清零。
- **自定义 Ok/Err 枚举 `?`**：
  - eligibility 接受含 `Ok`/`Err` 的用户 Nominal 枚举作为 Try 内类型。
  - `emit_try` 新增 `TryInnerKind::CustomEnum` 路径：Err 变体直接构造
    当前函数的 Err 返回值并 `return`，Ok 从 i64 payload 槽恢复目标值；
    保留 defer/on-failure 清理语义。
  - `demos/07_error_handling.mimi`、`core_try_operator.mimi`、
    `try_operator.mimi`、`error_propagation_test.mimi`、
    `nested_on_failure_test.mimi`、`on_failure_test.mimi` 全部
    reachable 0 fallback。
- **全语料更新**：
  - reachable：`legacy_fallback 37→28`、fallback_rate 0.1072→0.0812；
    `unsupported_type` 14→0；`unsupported_expression` 22→14
  - 默认：`legacy_fallback 1488→1479`、fallback_rate 0.2551→0.2536；
    `unsupported_type` 54→40；`unsupported_expression` 9→14
- `classify --check` 已刷新并保持通过。

### 0.37.8 — Phase A: shared/weak Ownership 注解进入 resolved 切片

`core_shared_weak.mimi` 的 `main` 因 `Ownership { kind: Shared, ... }`
和 `OwnershipWrap` 转换被门禁拒绝。Ownership 是编译期注解：shared/weak
值在 LLVM 层与目标类型共用同一表示。本轮补齐：

- **类型层**：`ResolvedType::Ownership` 降低为 inner target 类型。
- **门禁**：`require_scalar_type()` 接受 Ownership 并递归校验目标；
  `require_conversion()` 接受 `OwnershipWrap` / `OwnershipDowngrade` /
  `OwnershipRead`。
- **发射器**：`apply_conversion()` 将上述 Ownership 转换按 identity 处理。
- **验证程序**：
  - `tests/real_world/core_shared_weak.mimi`：reachable 1/1 eligible、
    0 fallback，运行退出 0
- **全语料更新**：
  - reachable：`legacy_fallback 38→37`、fallback_rate 0.1101→0.1072；
    `unsupported_type` 15→14
  - 默认：`legacy_fallback 1489→1488`、fallback_rate 0.2553→0.2551；
    `unsupported_type` 55→54
- `classify --check` 已刷新并保持通过。

### 0.37.7 — Phase A: 引用/借用表达式（`ref`、`&mut`、`*`) 进入 resolved 切片

本轮实现 resolved 发射器的引用指针 ABI，覆盖 `ref_type_test` 与
`ownership_cfg` 的借用/解引用场景：

- **类型层**：`ResolvedType::Reference` 降低为 opaque pointer；eligibility
  接受引用的标量目标类型；`require_root_place` 接受 `Deref` 投影。
- **绑定层**：`bind_pattern` / `require_binding_pattern` 接受
  `by_reference: Some` 绑定，引用本地保存指针值。
- **表达式层**：
  - `BorrowShared` / `BorrowMutable`：对于 `Load(place)` 返回其存储地址；
    对于 rvalue 临时分配 alloca 并返回其地址。
  - `Dereference`：通过指针加载目标类型值。
  - `root_place` 的 `Deref` 投影支持 `*ptr = ...` 写路径。
- **验证程序**：
  - `examples/ref_type_test.mimi`：reachable 2/2 eligible、0 fallback，
    运行返回 42
  - `tests/real_world/ownership_cfg.mimi`：reachable 7/7 eligible、
    0 fallback，运行退出 0
- **全语料更新**：
  - reachable：`legacy_fallback 43→38`、fallback_rate 0.1246→0.1101；
    `match_pattern` 1→0；`unsupported_type` 19→15
  - 默认：`legacy_fallback 1494→1489`、fallback_rate 0.2562→0.2553；
    `unsupported_type` 59→55
- `classify --check` 已刷新并保持通过。

### 0.37.6 — Phase A: PeerFault 内置记录进入 resolved 切片

`tests/real_world/flow_peer_fault.mimi` 的 `main` 由于
`builtin:type:PeerFault` 未在 eligibility 的 builtin 可切片名单中而整体回退。
`checker` 与 `resolved` 的类型表已经具备 PeerFault 的字段 schema，
`types.rs` 尚未给出对应 LLVM 布局。本轮补齐：

- **eligibility 门禁**：`require_scalar_type()` 接受
  `builtin:type:PeerFault`，与 SystemTrace/MemoryDump/PanicPayload 一致。
- **resolved/types.rs**：新增 PeerFault LLVM 布局
  `{ string peer_id, string reason }`，与 checker/legacy 记录布局对齐。
- **验证程序**：
  - `tests/real_world/flow_peer_fault.mimi`：reachable 1/1 eligible、
    0 fallback，运行退出 0
- **全语料更新**：
  - reachable：`legacy_fallback 44→43`、fallback_rate 0.1275→0.1246；
    `unsupported_type` 20→19
  - 默认：`legacy_fallback 1495→1494`、fallback_rate 0.2563→0.2562；
    `unsupported_type` 60→59
- `classify --check` 已刷新并保持通过。

### 0.37.5 — Phase A: DynamicAny/map 值句柄进入 resolved 切片

`std_maps.mimi` 的 `main` 因为 `type DynamicAny { capability: "type.dynamic_value" }`
被 eligibility 门禁拒绝，实际上 `types.rs` 已经把它降低为不透明 `i64`
句柄（与 Map/Set/Record 句柄、运行时 map value box ABI 一致），
`apply_conversion` 也已支持 `DynamicAnyPack`。本轮补齐：

- **eligibility 门禁**：`require_scalar_type()` 接受 `ResolvedType::DynamicAny`
  作为 per-function 切片可标量类型，与 `types.rs` / `apply_conversion` 对齐。
- **验证程序**：
  - `tests/real_world/std_maps.mimi`：reachable 1/1 eligible、0 fallback，
    运行退出 0
- **全语料更新**：
  - reachable：legacy_fallback 45→44、fallback_rate 0.1304→0.1275；
    `unsupported_type` 21→20
  - 默认：legacy_fallback 1496→1495、fallback_rate 0.2565→0.2563；
    `unsupported_type` 61→60
- `classify --check` 已刷新并保持通过。

### 0.37.4 — Phase A: newtype 构造绑定模式进入 resolved 切片

在 0.37.3 支持构造表达式后，剩余 `match_pattern` 主要来自
`let UserId(v) = ...` 这类 newtype 解构绑定。本轮补齐：

- **resolved 发射器**：`bind_pattern()` 新增 Constructor 分支；newtype
  构造绑定直接复用值并递归绑定子模式。
- **eligibility 门禁**：`require_binding_pattern()` 允许 Constructor 模式
  递归校验，与 match 模式保持一致。
- **验证程序**：
  - `examples/newtype.mimi`：reachable 2/2 eligible、0 fallback，运行返回 42
  - `demos/11_advanced.mimi`：reachable 11/11 eligible、0 fallback，运行退出 0
  - `tests/real_world/core_newtype.mimi`：reachable 2/2 eligible、0 fallback
- **全语料更新**：
  - reachable：legacy_fallback 47→45、fallback_rate 0.1362→0.1304；
    `match_pattern` 3→1
  - 默认：legacy_fallback 1498→1496、fallback_rate 0.2569→0.2565；
    `match_pattern` 3→1
- `classify --check` 已刷新并保持通过。

### 0.37.3 — Phase A: 用户 newtype/枚举构造表达式进入 resolved 切片

在 `MIMI_REACHABLE_DISPATCH=1` 实验揭露出 `unsupported_expression` 主体为
用户构造表达式后，本轮直接补齐 resolved 对构造调用的支持：

- **resolved 发射器**：`ResolvedExprKind::Call` 新增
  `ResolvedCallee::Constructor` 分支。
  - newtype 构造（如 `UserId(42)`）按值恒等包装发射，底层 LLVM 值直接透传；
  - 单载荷/无载荷自定义枚举变体复用既有 `emit_custom_enum_ctor()`
    （`{i32 tag, i64 payload}`）；
  - 多载荷枚举变体（如 `Rect(w, h)`）仍安全回退 legacy，不产出错误 ABI。
- **eligibility 门禁**：`require_expr` 的 Call 允许 `ResolvedCallee::Constructor(_)`，
  并对参数递归校验。
- **验证程序**：
  - `examples/newtype.mimi`：可编译且运行返回 42（与默认一致）
  - `tests/real_world/core_newtype.mimi`：reachable 2/2 eligible、0 fallback
  - `tests/real_world/core_enums_match.mimi`：reachable 2/2 eligible、0 fallback，
    运行退出 0（修复前一版错误 ABI 的段错误）
  - `demos/11_advanced.mimi`：reachable 11 函数仅剩 1 个模式匹配回退
- **全语料 reachable 更新**：120/128 可编译程序成功，aggregate
  legacy_fallback 57→47、fallback_rate 0.1652→0.1362；
  剩余 47 个回退拆解为 `unsupported_type` 21、`unsupported_expression` 9、
  `generics/qualified` 14、`match_pattern` 3。
- **默认路径同步受益**：默认 aggregate 4334/5832 eligible、
  legacy_fallback 1508→1498、fallback_rate 0.2586→0.2569；
  classify 中 `unsupported_expression` 19→9；`classify --check` 已刷新并保持通过。

### 0.37.2 — Phase A: 可达函数 dispatch 实验（MIMI_REACHABLE_DISPATCH=1）

针对 `generics/qualified` 占 94.2% 回退的现状，新增一个不改变默认行为的
实验路径：`MIMI_REACHABLE_DISPATCH=1` 时，dispatch 统计/eligible 集合只
处理从入口 `main` 可达的函数。

- **调用图构建**：`eligibility.rs` 新增 `reachable_function_ids()`，基于
  `CheckedProgram::call_sites()` 从所有 `main` 入口做保守 BFS；找不到入口时
  回退为“全函数可达”，保证实验统计不会丢真实代码目标。
- **实测效果**（单程序）：
  - `tests/real_world/std_datetime.mimi`：72 函数/11 回退 → 4 函数/0 回退
  - `tests/real_world/std_collections.mimi`：真实使用的 `map_list` /
    `filter_list` / `reduce_list` 等在可达集合内仍保留为 `generics/qualified`
  - `tests/real_world/hm_core.mimi`：5 个真实调用的泛型函数仍是回退，证明该
    实验不会把“必须 legacy 的已用泛型”误判为可消除
- **全语料 reachable 结果（构造支持前）**：120/128 可编译程序成功，
  aggregate total 5832→345、legacy_fallback 1508→57、
  fallback_rate 0.2586→0.1652；剩余 57 个回退拆解为
  `unsupported_expression` 19、`unsupported_type` 21、
  `generics/qualified` 14、`match_pattern` 3。这个结果说明 prelude 泛型
  虽然是最显眼的跳过项，但真实可达回退的主体已转向表达式/类型后端缺口。
- **脚本入口**：`scripts/dispatch_stat.py report --reachable` /
  `check --reachable` / `sample --reachable` 增加同一实验路径的可视化。
- 在 0.37.2 阶段默认路径与既有基线完全不变，`classify --check` 零漂移。
  （0.37.3 的构造表达式支持随后同步改善了默认路径。）

### 0.37.0 — Phase 0：高压测试基建启动 + fallback 根因分类（初始）

0.1.7 战役启动，先建立可自动化的 Phase 0 基础设施：

- **高压测试 Harness**：新增 `tests/stress.rs` + `tests/stress/` 模块族，提供
  `mimi run` 子进程驱动、临时目录、耗时统计与 Flow 链式事件/并发 spawn
  源生成器。按规范拆分为 `event_storm.rs`、`chaos_fault.rs`、
  `soak_memory.rs`、`concurrency_scale.rs`、`fuzz_parser.rs`、`fuzz_json.rs`、`fuzz_wire.rs`。冒烟门禁：200 次 Flow 转移、
  标准库畸形 JSON 0 Panic、50 个 spawn/await、5 轮重复 Flow 链浸泡；
  另附 2000 次 Flow 转移与 500 个 spawn/await 的 `#[ignore]` 重载用例；
  `soak_memory.rs` 增加基于 `/usr/bin/time -v` 的 Max RSS 采样冒烟，
  10 万次 List 分配/释放循环零异常；`fuzz_parser.rs` 加入截断/变异源文件
  的 parser 崩溃冒烟（0 Panic / 0 SIGSEGV）；`fuzz_json.rs` 增加 16 类畸形
  JSON 批量拒绝冒烟；`fuzz_wire.rs` 增加 1000 轮随机二进制解码冒烟；
  `chaos_fault.rs` 增加 IEEE 除零错误通道捕获冒烟。
  Makefile 新增 `test-stress` / `test-stress-heavy` / `test-stress-fuzz` 入口。
  实测 `cargo test --test stress` 9 passed / 2 ignored（约 1.2s），
  `--ignored` 全部重载通过（约 4.3s）。
- **Legacy 回退根因分类工具**：`scripts/dispatch_stat.py` 新增 `classify`
  子命令，直接消费 `devdocs/v0.34/golden/dispatch-baseline.json`，将现有
  `skip_reasons` 映射为 `generics/qualified`、`module/source_id`、
  `unsupported_type`、`unsupported_expression`、`match_pattern` 五大类。
  结果持久化为 `devdocs/v0.37/dispatch-fallback-root-causes.json`；
  支持 `--check` 门禁，校验已生成清单与当前基线一致。
- **当前基线拆解**：1508 个 Legacy 回退中，泛型/限定名 1420（94.2%）、
  不支持类型 61（4.0%）、不支持表达式 19（1.3%）、模块函数体 source_id 5
  （0.3%）、模式匹配边界 3（0.2%），为 Phase A 按根因攻坚提供精确清单。
- **MIMI_VERBOSE 采样工具**：`scripts/dispatch_stat.py sample` 可指定
  `--program` / `--limit` / `--output` 批量编译语料并解析
  `resolved skip '<name>': reason` 行，聚合高频 skip 函数与根因；
  已用 20 程序采样验证，确认 `generics/qualified` 主要由 prelude 的 11 个
  泛型函数（identity/const_val/swap/compose/pipe/tap/flip/apply/konst/
  eq/not_eq）在几乎每个程序中重复出现造成。

### 0.37.1 — Phase A prep：模块函数体默认 allowlist 扩展（回退率 0.2750→0.2586）

0.1.7 Phase A 第一个编译后端攻坚入口：将此前只能通过实验性
`MIMI_RESOLVED_MODULE_BODIES=1` 开启的模块函数体 resolved 编译，扩大为
生产默认 allowlist。

- **全语料实验**：`MIMI_RESOLVED_MODULE_BODIES=1` 跑完 120/120 个可编译
  语料，0 emit_failed、0 崩溃；随后在默认路径下复测得到一致聚合数据。
- **默认 allowlist 扩展**：`src/codegen/resolved/eligibility.rs` 的默认
  `prelude,mymath,strings,collections` 扩展为
  `prelude,mymath,strings,collections,datetime,crypto,csv,env,io,template,time,main`；
  `main` 覆盖依赖包入口模块（如 `mylib`）。
- **回退率变化**：aggregate eligible 4228→4324，legacy_fallback
  1604→1508，fallback_rate 0.2750→0.2586；原先 81 个
  `module file (source_id mismatch)` 中有 76 个被默认转正。剩余
  `examples/guess.mimi` 的 5 个来自 `std/result` 的模块项在全局 lift 下
  仍以 `unsupported type` 形式保持同量回退，因此不额外加入 allowlist。
- 本地已重新生成 `dispatch-baseline.json` 与
  `dispatch-fallback-root-causes.json` 快照。

## [0.1.6] — 2026-08-16

> **核心深度闭环（Deep over Broad）——逐支柱"重设计 → 锚定 → 挣绿"已发布**。
> 0.1.6 里程碑：失败归属（Fault nominal）、状态语义（Actor mut）、抽象
> （Protocol/Session）、线性系统（泛型×线性 + Session lowering）、语法重设计
> （Phase D 定案）；四支柱均"定案 + 挣绿"，Phase E 加固与 Phase F 终测/文档重锚
> 完成。全量门禁：5474 lib + 31+1 real_world/cli 全绿、clippy --all-targets 零警告、
> fmt 0 diff、dispatch 基线 120 成功 / 8 跳过；终测报告
> `devdocs/v0.36/quad-final-0.36.114.md`，已知边界
> `devdocs/v0.36/known-boundaries-0.1.6.md`。

### 0.36.115 — Phase F：0.1.6 终测报告 + 文档重锚（quad-final）

0.1.6 终测与文档重锚收口：

- **全量门禁**：`cargo test --lib` 5474 passed / 0 failed / 6 ignored；
  `cargo test --test real_world` + `--test real_world_cli` 31 + 1 passed；
  6 项 ASAN/工具 ignored 复跑 6 passed；`cargo fmt --check` 0 diff；
  `cargo clippy --all-targets -- -D warnings` 0 警告；语言文档门禁与
  edge isolation 门禁全绿。
- **dispatch 基线完整 report**：`scripts/dispatch_stat.py report` 跑完
  128 个语料条目，120 成功 / 8 跳过（FFI 外部依赖 fixture +
  interpreter-only `flow_test_macros`），无超时；聚合
  total=5832 / eligible=4228 / legacy=1604 / fallback=0.275034294；
  JSON 快照已存 `devdocs/v0.36/dispatch-report-0.36.114.json`。
- **生产路径证据**：新增 `dual_production_checked_path_smoke`，把
  `compile_checked` 生产路径与 VM、E2E native 三方对拍纳入双后端证据面。
- **文档重锚**：golden syntax-reference 与 language-support.toml 版本统一为
  0.1.6-dev；spec §3.10 Session residual 已闭环表述替换旧 experimental；
  README.md / README.zh.md / AGENTS 版本表同步 0.1.6-dev 终测状态。
- **新增交付**：`devdocs/v0.36/quad-final-0.36.114.md`（终测报告）、
  `devdocs/v0.36/known-boundaries-0.1.6.md`（Wave-3 / 0.2 / 1.x 已知边界固化）。

### 0.36.114 — Phase E：clippy 全 targets 零警告收口


将 clippy 检查范围扩展到全部 targets 后，发现测试代码中 1 处
`clippy::needless_borrow`：

- `dual_backend.rs` 里 `check_source(&src)` 改为 `check_source(src)`；
- 测试语义不变，回归 `dual_linear_cap_method_arg_double_use_rejected`
  通过。

修复后 `cargo clippy --all-targets -- -D warnings` 输出零警告。

### 0.36.113 — Phase E：clippy 全lib零警告收口

`cargo clippy --lib -- -D warnings` 暴露 1 处 `clippy::ptr_arg`：

- `sort_diagnostics` 参数 `&mut Vec<Diagnostic>` 改为 `&mut [Diagnostic]`；
- 调用点 `&mut errors` 自动借用到 slice，无需其他改动；
- 排序逻辑与行为不变，`check_program_diagnostics_are_source_sorted` 回归通过。

修复后 `cargo clippy --lib -- -D warnings` 输出零警告。

### 0.36.112 — Phase E：0.1.6 全面查缺补漏/代码审查收尾

本轮完成全量验证与台账收口：

- 全量 `cargo test --lib`：**5473 passed / 0 failed / 6 ignored**；
- `--ignored` 的 6 项 ASAN 回归单独运行也全部通过；
- `cargo fmt --check`、`check_language_docs.py`、`check_edge_isolation.py`
  全绿；
- 工作树干净。

审计台账中已修/已按设计闭合/已按裁决延后项均已落到代码注释与 CHANGELOG；
0.1.6 不再有未记录的活缺口。Wave-3 结构项继续作为后续路线保留。

### 0.36.111 — Phase E：按裁决收尾宽度模型与 actor dispatch ABI 台账（§9-#10、§10-#22）

两项均已有明确裁决，本次补齐台账闭合标注：

- §9-#10（i32 字面量折叠 wrap-vs-trap）：按 §16 V-6 裁决归入宽度模型 A1
  族，测试改为 exact-value 家族保持双后端一致性；
- §10-#22（actor dispatch 固定 256B）：编译期/运行期 size check + N-4
  钳制已诚实拒绝超限；>256B 动态化是 dispatch ABI 契约变更，按冻结纪律
  延至 1.x。

`audit_fix_vm.rs` 补 §9-#10 闭合说明；`runtime/actor.rs` 补 §10-#22
闭合说明。

### 0.36.110 — Phase E：闭合 access/record 替换深度上限静默回退（§2-#17）

§2-#17 后半项：access 侧 `substitute_type_params` 超过 `MAX_SUBST_DEPTH`
后静默返回 `ty.clone()`，release 下可能弱化类型推断而不报错。

本次将 access 与 record 两处统一改为返回 `Type::TyErr`（poison）：

- debug 构建仍由 `mimi_debug_assert!` 立即 ICE，提供可定位的编译器缺陷信号；
- release 构建不再静默弱化，`TyErr` 会沿类型流传播为显式类型错误；
- 新增两个回归：debug 断言 panic；release 断言返回 `TyErr`。

### 0.36.109 — Phase E：按设计闭合 future/RC Arc 固有边界台账（§10-#23/#26）

§10-#23/#26 记录 weak-retain 无法防御完全释放的悬垂调用，以及 RC 竞态依赖
调用方纪律。当前实现已有：

- future `mimi_future_free` 先 retain 拒绝再写 header，彻底移除 UAF write；
- RC weak_retain/upgrade 采用 CAS + Acquire/Release 两阶段，对存活对象
  ABA-safe；
- release 下溢/悬垂调用仍只能通过“调用方必须持有引用”这一 Arc-class
  前置约定拒绝；裸指针热路径没有 handle registry，任何更低成本的
  `Weak<T>` 兼容实现都必须保留相同读取即触达的原则。

该条按设计闭合到 Arc 固有边界，不视为 0.1.6 可消除缺陷。`future.rs` 与
`runtime/mod.rs` 补审计闭合标注。

### 0.36.108 — Phase E：按证据闭合 shadow_mte“死模块/零尺寸 UB”台账（§10-shadow_mte）

台账登记 `shadow_mte.rs` 为死模块且 size-0 layout alloc 可能 UB。核对
当前实现后该登记已过时：

- 模块不是死代码：interp/codegen builtins 均注册 `shadow_alloc` /
  `shadow_tag` / `shadow_check` / `shadow_free`；
- `tests/real_world/flow_shadow_memory.mimi` 与 `flow_features` 均有
  双后端/解释器回归；
- size=0 分配不 deref，`Layout::from_size_align` 与同 layout `dealloc`
  合法；
- 新增 `shadow_alloc_zero_size_is_safe` 回归，覆盖 alloc/tag/check/free
  全路径。

因此该条按证据闭合，更新模块头部状态说明。

### 0.36.107 — Phase E：按设计闭合 G-5 残留（Deref place 冲突近似）

台账 G-5 后半项为 `Place::conflicts_with` 对 Deref place 无视 base 一律
返回 true，可能产生 E0415 误报。该行为是有意的保守近似：

- 当前无法从 place 形状可靠判断 `*p.a` 与 `*p.b` 是否指向同一对象；
- 一律重叠是 fail-closed 方向：宁可误报，也不漏掉潜在的 aliased borrow；
- 发散函数前半项已由既有修复闭合。

该条按设计闭合，`core/ownership.rs` 补注释。

### 0.36.106 — Phase E：按设计闭合 Go 回调 ABI user-data 根本解台账（Wave-3）

台账登记“Go 回调 ABI user-data 根本解”为 Wave-3 项。当前 0.1.6 采用
per-slot mutex 快照缓解：

- cgo trampoline 在锁内快照回调，锁外调用，避免死锁；
- 并发覆盖已被 `audit_fix_bind_go` 回归覆盖；
- 跨线程/异步回调由全局 store + deregister 生命周期处理。

“把 user-data 直接编入 C 函数指针 ABI”属于 Wave-3 根本解，不阻塞 0.1.6。
该条按设计闭合，`ffi/go_bind.rs` 补模块级说明。

### 0.36.105 — Phase E：按设计闭合 codegen list 元素 struct box 泄漏台账（§6-#68）

§6-#68 记录 `struct_to_i64` 的 struct box 未注册到 `heap_allocs` 导致
长期存活泄漏。该行为是当前列表所有权模型的有意折衷：

- 直接注册会在返回 / in-place 修改列表时造成 use-after-free 或
  double-free；
- `from_json` list 路径已有同款注释（call/method.rs）；
- 进程终止回收保证 codegen 正确性，以内存卫生为代价。

该条按设计闭合到 Wave-3 列表所有权模型，不视为 0.1.6 安全/正确性阻塞。
`resolved/mod.rs` 与 `expr/record.rs` 两处注释改为闭合说明。

### 0.36.104 — Phase E：按设计闭合 Wire schema 台账（Wave-3）

台账登记“Wire schema 接线或删除”为 Wave-3 项。核对当前实现：

- `src/component/wire.rs` 已实现 `WireEnvelope` / `WireType` / 字段编解码；
- 模块内有完整 round-trip、truncated、长度溢出、非 UTF-8、handle depth
  等错误路径测试；
- 未接 CLI/传输层仅属 Wave-3 集成目标，不再是“未实现或未决定”的活缺口。

该条按设计闭合，`component/wire.rs` 补模块级说明。

### 0.36.103 — Phase E：按设计闭合三引擎等价矩阵台账（Wave-3）

台账登记“三引擎等价矩阵（legacy/resolved/VM 逐特性）”为 Wave-3 基建。
对 0.1.6 而言，当前证据基础已经具备：

- `dual_backend.rs` 全量双后端差分：VM + LLVM codegen；
- `bytecode_equiv_smoke` / property differential 覆盖 VM 与 codegen；
- resolved IR 作为唯一前端产物，legacy 路径已不是 0.1.6 交付面。

完整 legacy/resolved/VM 三引擎逐特性矩阵仍属 Wave-3 基建，按设计不在
0.1.6 范围内闭合。`dual_backend.rs` 补模块级说明。

### 0.36.102 — Phase E：按设计闭合 bindgen 未全量落地 Component IR 台账（§12-#57）

§12-#57 原记录生产 bindgen 绕过 Component IR，可能各自解析裸 AST 导致
ABI 漂移。核对当前实现：

- `mimi bindgen` 先经 `checked_component_input` 做类型检查 + 组件边界
  校验，解析错误/不支持类型在生成前 fail-loud；
- 7 个后端统一消费 `resolved_extern_funcs` / `resolved_type_defs`，不存在
  各后端各自解析原始 AST；
- returns_errno 传播已修；导出适配器亦有明确归属。

完全迁移到 `ComponentIr` 仍属 Wave-3 架构目标，但不再是 0.1.6 正确性
阻塞。该条按设计闭合，`main/bindgen.rs` 补模块级说明。

### 0.36.101 — Phase E：按设计闭合 LSP A6 消费端债务（§13-#73）

A6 记录“LSP 文本搜索→AST 位置未迁移”。经过 0.35.15 + 0.36.88–0.36.98
多轮落地，语义定位类消费端已全部迁移到 AST span / AST 遍历：

- definition / symbols / folding / hierarchy / references line range：
  AST span；
- code lens 引用计数：`count_ast_references`（0.36.96）；
- references / document highlight / rename：整词字节扫描 + `non_code_byte_ranges`
  排除，且 rename/highlight 本就属于源码改写操作，不是语义定位查询；
- inlay：AST 遍历 + span 锚点（0.36.98）。

剩余文本扫描均为行文本上下文获取或改写扫描，不再属于“未迁移 AST 位置”的
语义债务。该条按设计闭合，`lsp/references.rs` 补模块级说明。

### 0.36.100 — Phase E：按证据闭合回调槽 TLS / 跨线程台账（§12-#65）

§12-#65 原记录“回调槽 TLS 模型矛盾；`mimi_shared_get_ptr` header 生命周期谎言”。
其中 C header 寿命描述已在 0.36.99 修正；回调/TLS 侧核对当前实现已完整落地：

- `CALLBACK_GLOBAL_STORE`：全局回调生命周期，任何线程可从已注册表查找；
- `CALLBACK_FILE`：跨线程/异步回调使用进程级 File 创建临时 Interpreter；
- `mimi_callback_deregister`：显式注销 + active-count 等待，避免释放后回调；
- Go 侧 per-slot mutex（audit_fix_bind_go）防止并发覆盖；
- `ffi_interp_e2e` 已有跨线程/异步回调回归。

该条按证据闭合，`interp/ffi/callback.rs` 补审计闭合标注。

### 0.36.99 — Phase E：修正 C header `mimi_shared_get_ptr` 生命周期描述（§12-#65 部分闭合）

台账 §12-#65 指出 C header 对 `mimi_shared_get_ptr` 注释为“返回指针仅在
handle 存活期间有效”，但 runtime 实际返回**堆拷贝**，由调用方用
`mimi_value_free` 释放：

- handle 销毁后指针仍可安全读取；
- 旧注释会诱导调用方错误地随 handle 一起释放或过早释放。

本次修正：

- `c_header.rs` 注释改为“返回堆分配拷贝，调用方拥有并需 `mimi_value_free`”；
- `test_header_contains_runtime_api_declarations` 增加注释断言；
- 新增 `shared_c_api_get_ptr_copy_survives_handle_release`，验证 release
  后拷贝仍可读，随后由调用方释放。

### 0.36.98 — Phase E：document highlight 排除非代码区 + inlay 深入块容器

本次在同一批 LSP 消费端继续收敛：

1. **document highlight**
   `compute_document_highlight` 的使用扫描此前与 `compute_references` 一致，
   未排除注释/字符串；现复用 `non_code_byte_ranges`，注释/字符串中的同名
   单词不再产生 Text 高亮。新增
   `document_highlight_excludes_comments_strings` 回归。

2. **inlay hints 递归**
   `collect_hints_from_block` 原先只递归 If/While/For/IfLet/WhileLet/
   IeeeFloat，`Block` / `Loop` / `Arena` / `Unsafe` / `Defer` /
   `OnFailure` / `Parasteps` / `Pinned` 内的 let 类型提示与参数提示会
   静默缺失。本次补齐这些块容器递归，并新增
   `inlay_hints_recurses_into_block_wrappers` 回归（嵌套 block/loop 中的
   let 均产生提示）。

### 0.36.97 — Phase E：references 排除注释/字符串引用位（A6 消费端改进）

`compute_references` 的使用扫描此前依赖整词字节扫描，但未套用
`non_code_byte_ranges`，导致注释和字符串字面量中的同名文本也被当作引用
位置返回。

本次在 usage scan 中复用与 rename / code lens 相同的 SourceScanner 区域
纪律：

- 注释、字符串、块注释中的同名文本不再出现在 references 结果；
- 定义位置跳过逻辑保持不变；
- 新增 `references_exclude_comments_strings` 回归：只有真实调用行进入结果。

### 0.36.96 — Phase E：code lens 引用计数迁移到 AST（A6 关键消费端闭合）

A6 债务的 code lens 计数消费端此前仍是文本扫描（尽管已整词 + 排除非代码区）。
本次新增 `lsp/symbols.rs::count_ast_references`，用 AST 递归遍历统计：

- `Expr::Ident` / `Type::Name` / 构造器 / record 类型名 / turbofish 等；
- 覆盖 func/type/trait/actor/impl/flow/protocol/session 等顶层结构；
- 天然不含注释、字符串、文档文本。

`compute_code_lens` 四个计数位置（func/type/trait/actor）均已切换到 AST，
保留 `+1` 维持历史“定义 + 引用”显示口径。旧的文本计数函数降为
`#[cfg(test)]` 仅作 SourceScanner 路径单测。

新增 `code_lens_ast_reference_count_excludes_comments_strings`，同时验证：
- 注释/字符串中的同名文本不计入；
- 函数体调用计入；
- 类型注解与构造器中的类型名计入。

### 0.36.95 — Phase E：补强 LSP 引用计数回归（字符串/块注释排除）

扩展 `audit2_tool_count_text_references_whole_word`：

- 行注释中的 `foo` 不计入；
- 字符串字面量中的 `foo` 不计入；
- 块注释中的 `foo` 不计入；
- 整词边界与一行多出现仍保持。

该回归钉死 0.36.89 的 `non_code_byte_ranges` 排除契约。

### 0.36.94 — Phase E：闭合 unification 逃逸口边界 TODO（v0.31-type-engine）

`core/unification.rs` 保留 `TODO(#v0.31-type-engine)`，提议把 `_` / `Any`
逃逸口的统一限制到顶层推断边界，并增加 E0431 边界泄漏检查。

审计后按设计闭合该 TODO：

- `_` 只会由 parser 在 let-init 位置产生；
- `Any` 已从用户语法移除（golden §2.4），只服务内部 artifact 路径；
- 未复现用户可见的逃逸穿过函数调用/字段访问边界；
- E0431 仍保留作为未来发现边界泄漏时的错误码。

本次把 TODO 改为设计说明，不做未验证的新限制（避免假拒绝）。

### 0.36.92 — Phase E：文档化 codegen list 元素 struct box 所有权/泄漏契约（§6-#68）

台账 §6-#68 记录 `struct_to_i64` 对列表元素 struct box 使用
`malloc_or_abort` 但不注册到 `heap_allocs`，存在长期存活泄漏。

审计确认该泄漏是当前列表所有权模型的**有意折衷**：

- 列表 data 缓冲区的所有权已有 scope-exit 回收和返回移交；
- 元素级 struct box 不属于单一 scope，注册到 `heap_allocs` 会在返回
  /in-place 修改列表时造成 use-after-free 或 double-free 风险；
- 当前模型以进程终止回收为代价换取 codegen 正确性，与
  `from_json` list 的注释一致。

该条仍保持打开（Wave-3 列表所有权模型），但本次为两处 `struct_to_i64`
补上明确的审计归属与折衷说明，避免未来误注册导致更严重内存错误。

### 0.36.91 — Phase E：闭合 nested mutate place 写回 TODO（M3）

`interp/bytecode/compiler.rs` 的 TODO(M3) 记录：nested field place（如
`o.inner.value`）作为 mutate 实参时，单层 Field writeback 覆盖不到，会
静默丢弃。旧注释还称 checker 未做 mutate-arg place 验证。

核对当前代码后该条为**过时台账**：

- Q5（0.34.25c）已让 checker 拒绝 nested mutate place（E0434）；
- `flow_features::mutate_nested_place_rejected_q5` 回归覆盖；
- 因此不存在可被接受的 nested place 进入后端并静默丢失的路径。

本次把 TODO(M3) 注释改为闭合说明，并指出后端只实现被 checker 允许的
单层 `Field(Ident, _)` 形状。

### 0.36.90 — Phase E：删除 VM 未使用的 `field_index` 骨架 TODO

`interp/bytecode/compiler.rs` 中 `field_index` 方法只有一个占位实现
（`todo: resolve from CheckedProgram type definitions; return 0`），且全仓库
没有任何调用点。该骨架是早期 bytecode 布局遗留，保留只会误导读者以为
字段索引参与编译。

本次删除该方法及 TODO。

### 0.36.89 — Phase E：LSP 引用计数排除注释/字符串内容（A6 部分改进）

继续收敛 `lsp/symbols.rs::count_text_references`：

- 0.36.88 已改为整词扫描；
- 本次复用 `non_code_byte_ranges`，让 code lens 的引用计数不再把注释和
  字符串字面量里的同名文本当作代码引用；
- 与 rename 使用同一套 SourceScanner 区域纪律，跨行注释/多行字符串也能正确跳过。

新增/更新 `audit2_tool_count_text_references_whole_word` 断言：注释中的
`foo` 不计入。

### 0.36.88 — Phase E：LSP 引用计数改为整词扫描（A6 部分改进）

`lsp/symbols.rs::count_text_references`（code lens 引用计数）原先用
`line.contains(name)` 逐行子串匹配：

- `foo` 会把 `foobar` 也算进去；
- 一行内多次出现只算 1 次。

本次改为复用 `find_word_occurrences` 的整词扫描：

- 尊重标识符边界，`foo` 不再匹配 `foobar`；
- 一行内多次出现分别计数；
- 仍保留文本扫描的 A6 债务剩余（注释/字符串仍会被计数），已注释说明。

新增 `audit2_tool_count_text_references_whole_word` 回归。

### 0.36.87 — Phase E：闭合 JSON parser 审计 TODO（已知语义分歧全部关闭）

`runtime/mod.rs` 原保留 `TODO(#audit-wave2-json-parser-unify)`，指向手写
parser 与 serde_json 的若干边缘分歧。经过 0.36.73 / 0.36.83 两轮收紧，
原始登记的分歧已全部关闭：

- 前导零 `01` / `-01` 拒绝；
- 溢出指数 `1e999` 因非有限拒绝；
- `inf` / `nan` 不会被 number 路径接受；
- 未知转义与裸控制字符在 permissive / strict 字符串路径均拒绝；
- strict validator 已作为 `mimi_is_valid_json` / `mimi_from_json` 的前置。

结构上用 serde 完全替换手写 parser 仍属于 Wave-3 架构目标，但不再是
audit-wave2 的开放 TODO 标记。本次移除该 TODO 及 `json_get_int` 的引用。

### 0.36.86 — Phase E：按设计闭合 spawn/await 顺序求值台账（§9-#15 / B-8）

台账 §9-#15 / B-8 记录 `spawn` / `await` 当前编译为顺序求值，`Op::Spawn`
不发射。这是已知的并发运行时设计形状（Phase D / Wave-3），不是 0.1.6
安全或正确性阻塞：

- 当前语义等价于普通调用/求值，行为确定且不产生悬空并发；
- 真正并发运行时需同步落地 `Op::Spawn` 发射、调度器、生命周期等，属于
  结构性 Wave-3 工作。

本次在 `compiler.rs` 的 spawn/await 编译分支补注释标记该设计决策。

### 0.36.85 — Phase E：按设计闭合 match guard 恒真 CFG 建模台账（G-3）

台账 G-3 记录 `cfg/resolved_lower.rs` 将 match guard 按恒真建模，不分叉
控制流，消费路径高估。该方向是 fail-closed（安全方向），不是 0.1.6
阻塞项：

- 误把 guard-false 路径也计入消费只会更保守地拒绝，不会放行危险程序；
- 精确的 guard-true / guard-false 分叉属于 1.x CFG 增强。

本次在 `resolved_lower.rs` 的 lower_match guard 位置补注释说明设计决策。

### 0.36.84 — Phase E：闭合未解析操作数 `is_int` 误报台账（§2-#20）

台账 §2-#20 记录 `infer/access.rs` 与 `operator.rs` 的 `is_int` 对未解析
`TypeVar` 返回 false，可能误报。长时间审计未复现用户可见假阳性。

该行为按设计闭合：

- `is_int` 只认具体 `i32` / `i64`；
- 若 `TypeVar` 其后统一为整数，inference 会在用户可见调用点前解析；
- 若仍保持未解析，拒绝 index/range 属于 fail-closed（比静默假定整数安全）。

本次在 `src/core/helpers.rs::is_int` 补注释说明该契约。

### 0.36.83 — Phase E：runtime JSON 字符串 token 拒绝未知转义/裸控制符（serde 对齐）

`runtime/mod.rs` 的 permissive JSON 扫描器此前对字符串 token 过宽：
- 未知转义（`"\q"`）被当作普通字符保留；
- 裸控制字符（U+0000..U+001F）被当作普通字符接受。

这会让直接 C ABI 访问器在 `json_get_*` 上比 VM / serde_json 更宽容。

本次收紧：
- `JsonParser::parse_string` 与 `json_get_inner` 的 key 解析均对未知转义
  返回解析失败；
- 未转义控制字符同样返回解析失败；
- 合法转义（`\n`、`\t`、`\"` 等）保持不变。

新增 `json_accessors_reject_unknown_escapes_and_raw_controls` 回归，
直接覆盖 permissive `json_get_inner` 路径。

### 0.36.82 — Phase E：按设计闭合 LSP 无文本 fallback 精确性台账（X-11）

台账 X-11 🟡 指出 `lsp/position.rs` 的无文本回退把 char 列当 UTF-16，lossy。

核对当前实现后确认这是不可消除的固有边界：

- 精确转换必须扫描行文本并累加每个字符的 UTF-16 长度；
- 若调用方未提供文本，Span 本身不携带足够信息还原 UTF-16 列；
- 生产路径（diagnostic/state）都优先喂文本；`span_to_range` 只作为
  无法取得源码文本时的兜底，且已文档化 lossy。

该条目按设计闭合：不是“待修 bug”，而是有界 fallback 契约。本次补注释说明。

### 0.36.81 — Phase E：闭合 `#[ignore] verify_unsatisfiable_requires` 台账（§11-#49）

台账 §11-#49 记录 verifier 测试仍有一处旧 `mms{}` 合同写法未迁移。核对
`src/tests/v1_2_verification.rs` 后确认早已改写：

- 原测试使用 `mms { "requires: ..." }`，Mimi 并不从 `mms{}` 提取合同；
- 当前版本改为顶层 `requires:` / `ensures:`，矛盾前置条件断言为 Failed；
- 另在 verifier/tests.rs 也有顶层写法覆盖。

该条为过时台账，本次补注释标记已闭合。

### 0.36.80 — Phase E：闭合值位置 Ident 导入顺序解析台账（§5-#51）

台账 §5-#51 与已闭合的 §4-#6 同源：值位置 ident 使用全函数目录短名猜测解析，
不镜像 checker 导入顺序。0.36.70 已通过实测证明 loader 在合并导入模块时拒绝
重复 bare item，因此用户无法构造“同名不同模块同时进入作用域”的导入顺序分歧；
唯一短名匹配 + fail-closed ambiguity guard 已足够。

本次仅在 `lower.rs` 对应注释中补充 §5-#51 closed 标记，无行为变更。

### 0.36.79 — Phase E：闭合 std 私有 helper 跨 use 不可见台账（§1.6 机制）

台账记录了 std 模块私有 helper 无法跨 `use` 对消费者可见的问题：当 pub
函数体调用私有 helper 时，loader 只携带 pub 项，导致 `E0401`。

该问题此前已通过“把私有 helper 内联/提升为 pub 共享函数”落地：

- `strings.mimi`：`is_ws_char` 内联进 `trim_left` / `trim_right`
  （`audit2_std_trim_left_right_dual` 双端守卫）；
- `random.mimi`：`remove_at` 提升为 pub 自由函数 `random_remove_ith`
  （0.36.65，`audit2_std_random_sample_shuffle_semantics_dual` 双端守卫）。

具体的 audit 缺口已闭环；loader 是否引入“闭包传递私有项”是更广的架构特性，
不阻塞 0.1.6 审计。本次在 stdlib 回归注释中标记该机制已闭合。

### 0.36.78 — Phase E：闭合 cap/dyn/builtin 方法探测二次降低台账（R-5）

台账 R-5 称：cap/dyn 方法探测会先降低 receiver 再回退，导致同一 AST 二次
降低、伪 identity collision。核对 `lower.rs` 后确认已通过“先查 checked
node type、后降低 receiver”修复：

- `lower_dyn_method_call`：先用 `node_types` 判断 receiver 是否为 `dyn Trait`，
  非 dyn 直接回退，不再预降 receiver；
- `lower_cap_method_call`：先确认 receiver 为 Capability，非 cap 直接回退；
- `lower_builtin_method_call`：先用 checked receiver type 查 builtin，
  非 builtin 直接走真实 method 路径。

三处都保留“降低后类型与 checked 类型一致”的二次校验。该条为过时台账。

### 0.36.77 — Phase E：闭合 resolved 诊断顺序非确定性台账（§4-#43）

台账 §4-#43 称 `resolved/mod.rs` 通过 HashMap 迭代产生诊断，顺序可能不确定。
核对 `src/core/mod.rs` 后确认已修复：

- `check_program` / `check_program_strict` 的所有错误返回路径都调用
  `sort_diagnostics`；
- 排序键为：源位置（start_line, start_col）→ message → code；
- `core/resolved/tests.rs::check_program_diagnostics_are_source_sorted`
  已回归钉死“多诊断必须按源码顺序输出”。

该条为过时台账，本次补注释标记已闭合。

### 0.36.76 — Phase E：闭合 `has_cross_boundary_ops` 漏 Defer/OnFailure/Parasteps 台账（§4-#39）

台账 §4-#39 称 `has_cross_boundary_ops` 漏了 Defer / OnFailure / Parasteps，
可能把“仅在这些包装块内发生跨边界操作”的 transition 误判为 silent。

核对 `src/core/resolved/mod.rs` 当前实现后确认已经覆盖：

```rust
Stmt::Block(stmts)
| Stmt::Arena(stmts)
| Stmt::Unsafe(stmts)
| Stmt::IeeeFloat(stmts)
| Stmt::Defer(stmts)
| Stmt::OnFailure(stmts)
| Stmt::Parasteps(stmts) => stmts.iter().any(stmt_has_cross_boundary),
```

即这些包装块内的 `session_send` / `channel_send` / `emit` / FFI 调用等
跨边界操作都会阻止 silent transition。该条为过时台账，本次仅补注释标记。

### 0.36.75 — Phase E：闭合 async ensures body-result 类型台账（R-3 / §5-LOW）

台账 `R-3 / §5-LOW` 记录：async 函数 `ensures` 的 `result` local 标成
`Future<T>` 而不是 body 实际返回的 `T`。审计确认 lower.rs 已处理该点：

- `body_result_type` 显式解包 `Future<T>`，给 async body 的尾/return 使用内层 `T`；
- `Stmt::Ensures` 使用 `self.body_result` 插入 `result` local，与 body 结果类型一致；
- 当前 parser 不再产生 `is_async: true` 的常规函数（`top_level.rs` 恒为 false，
  async 由 ForeignTask/历史路径另行表达），该差异已不可由用户程序直接构造。

故该条为过时台账，本次仅在代码注释中标记已闭合，不引入行为变更。

### 0.36.74 — Phase E：闭合 Packed enum payload box 泄漏 TODO（L6 已实现）

`src/codegen/registry/types.rs` 的 Packed enum 构造路径仍残留 Long TODO，
写的是“该 box 永不释放，每个 boxed-enum 构造泄漏一次”。审计确认该 TODO
已被 0.35.x 的 L6 单所有权生命周期实现关闭：

- 本地消费：`simple.rs` enum_ctor 调用点通过 `register_heap_box` 登记 box，
  作用域退出时 `free_heap_allocs` 释放；
- 逃逸返回：`claim_returned_enum_box` 跳过 callee 侧释放，caller 通过
  `HeapEntry::EnumBox`（按运行期 tag 条件释放）重新登记；
- 已有双端回归覆盖 local/return/lambda 场景。

本次将旧 TODO 压缩为已实现说明，并指向对应测试：
`enum_packed_payload_box_no_leak_no_double_free_dual_backend` 与
`lambda_returning_boxed_enum_dual_backend`。

### 0.36.73 — Phase E：runtime JSON 数字解析拒绝前导零（serde 对齐）

`runtime/mod.rs` 的手写 JSON parser 原先接受 `"01"` / `"-01"` 这类
number token，而 serde_json / VM 端将其判为非法 JSON。这是 JSON parser
unify TODO 中登记的具体分歧之一。

本次在 `JsonParser::parse_number` 增加前导零拒绝：

- `01`、`-01` → 非法 JSON
- `0`、`-0.5`、`0e1` → 仍为合法 JSON

并新增 `runtime_core_json_rejects_leading_zero_numbers` 回归，直接断言
`mimi_is_valid_json` 行为。

### 0.36.72 — Phase E：清理 `audit_fix_io` 过时 Result 形状对齐 TODO

`io_fix_input_*` 上方残留 `TODO(#audit-wave2): full Result<string,string>
shape alignment`，但该对齐已在 §8-#86 完成：checker、VM、codegen 三端一致将
`input()` 定为 `string`，EOF 用空串哨兵，`input_line` 再基于空串返回 Err。

本次删除过时 TODO，并更新注释指向已完成的形状对齐与现有测试职责。

### 0.36.71 — Phase E：闭合 scope-aware nested-name TODO（checker 本身已合并同名嵌套调用）

`lower.rs` 对分支块/闭包体内嵌套函数的裸名调用使用 NodeId 排序的确定性选择，
并保留一个“未来做 scope-aware 嵌套名解析”的 TODO。审计确认：

- checker 的裸名表本身按 bare name 注册，同名的嵌套 callable 在 checker 层
  就共享同一签名/名称槽位；
- 因此不存在 checker 层可镜像的 scope-aware 分发语义；
- 后端按自己的非作用域表解析是既有语义，强制逐分支解析反而会过度约束；
- `audit11_same_name_nested_helpers_in_disjoint_branches_compile` 已覆盖
  同签名同名嵌套函数在不相交分支下的编译/VM 行为。

结论：无需实现 scope-aware 嵌套名解析；将 TODO 更新为确定性选择即忠实语义，
并保留回归锚点。

### 0.36.70 — Phase E：闭合 import-order 镜像 TODO（loader 已拒绝重复项）

`lower.rs` 值位置 Ident 解析长期保留一个 `TODO(#audit-wave2)`：说 checker 按
import-order 解析裸名，而 catalog 近似只接受唯一短名匹配，可能有假阳性。

实测确认用户层面无法构造该分歧：

- 同时导入两个含同名 pub 函数的 std 模块（如 `std::strings` + `std::text`）
  → loader 直接报 `duplicate item 'is_blank' found in modules ...`；
- 用户文件自带函数与导入模块同名 → 同样报
  `duplicate item 'count_lines' found in modules ...`。

因此裸名多候选在进入 resolved lowering 前已被 loader 拒绝；保留唯一短名
匹配的 fail-closed guard 是确定且安全的，不需要镜像服务端 import-order。

本次同步清理 `lower.rs` 与 `audit_fix_lowering.rs` 中的过时 TODO 注释。

### 0.36.69 — Phase E：修复 `audit_h13_close_fd_still_closes_real_fds` 并行 fd 复用误报

该测试在 `--test-threads=4` 下有偶发失败：`close_fd` 关闭测试 fd 后，
另一个并行测试线程可能立刻复用同一 fd 编号；随后测试用 `fcntl(F_GETFD)`
检查时看到“仍打开”，误报为 close_fd 失效。单测单独运行从不失败，符合该竞态特征。

修复：在 close 前记录原文件的 `(st_dev, st_ino)`；close 后用 `fstat` 检查：

- `fstat` 返回 `EBADF` → 原 fd 确已关闭；
- `fstat` 返回另一个文件身份 → 说明原 fd 已被并行线程合法复用，测试不误报；
- `fstat` 返回同一文件身份 → 才是真正的 close_fd 失败。

该改动不关闭也不修改可能已被其他线程占用的 fd，只做身份判断。

### 0.36.68 — Phase E：comptime 折叠失败不再静默吞错，输出 warning

`fold_comptime_items` 注释承诺“单个坏 `comptime` 声明降级为 eprintln! 警告”，
但实际错误分支是 `Err(_) => Ok(())` 完全静默。若字节码侧 comptime 预折叠失败，
调用方只能看到后续的 runtime-dependent 错误，原始失败原因被吞掉。

本次在错误分支补上：

```text
warning: comptime folding failed (...); comptime values will be evaluated at runtime
```

行为仍保持“单个折叠失败不阻断整个 compilation”的既有策略，但不再掩盖根因。

- **验证**：`cargo test --lib comptime -- --test-threads=2` 75 passed；
  `audit_*`/dual comptime 用例无回归。

### 0.36.67 — Phase E：清理过时 TODO / 台账噪音

- `src/codegen/builtins/string/transform.rs`：移除 `.substring()` 方法形式
  仍走 clamp 路径的过时 `TODO(#audit-wave2)`。实际代码自 2026-08-06 D-5 起
  已将方法形式路由到 `str_substring_strict`，并有 `audit_1o_substring_method_strict_bounds`
  双后端回归覆盖。
- `src/tests/audit_fix_stdlib.rs`：
  - 更新文件头：VM-only 测试不再普遍带 `TODO(#audit-wave2-codegen-side)`，
    剩余个别 VM-only 用例由显式 dual companion 覆盖或有根因登记；
  - 移除 `audit_stdlib_trim_left_right_whitespace_set` 上已过时的
    `TODO(#audit-wave2-codegen-side)`（codegen 侧已由
    `audit2_std_trim_left_right_dual` 覆盖）。

纯注释/台账准确性整理，无运行语义变化。

### 0.36.66 — Phase E：`?` 在 lambda 内不再 fail-closed，lowering 追踪内层 lambda owner

此前 `Expr::Try` 的 `propagation_target` 始终取封闭函数 owner；在 lambda
内部这会指向错误的调用者，因此 lowering 对 lambda 内 `?` 直接拒绝（E0830）。
本次为 `BodyLowerer` 增加 `lambda_owners` 栈：

- 进入 lambda 时压入 `{lambda_node}/callable`；
- 退出时弹出；
- lowering `?` 时取栈顶（即最内层 lambda）作为传播目标；
- 普通函数路径保持原行为（无 lambda 栈时仍用函数 owner）。

两端语义不变：`?` 的运行时契约仍是进程级错误退出（`mimi_try_exit` /
VM 对应路径），但 resolved-body 不再人为拒绝 checker 已接受的合法 lambda
内 `?`。

- **验证**：`audit8_try_inside_lambda_fails_closed` 改为
  `audit8_try_inside_lambda_still_lowers`，同时断言 `check_source` 通过、
  VM 输出 `1`、native 输出 `1`。

### 0.36.65 — Phase E：修复 `random.mimi` shuffle/random_sample 的 codegen SIGSEGV

`audit_fix_stdlib.rs` 中登记的既有 codegen 缺陷被复现：`shuffle(xs)` 在
native 后端直接 SIGSEGV，`random_sample` 也不可靠。根因是内联在
`impl<T> RandomChoice<T> for List<T>` 泛型方法里的 remove-at 循环触发了
codegen 的栈 alloca 别名问题：

- `sh_rest = sh_kept` 使 `sh_rest` 指向 `sh_kept` 的栈内列表结构；
- 下一轮迭代重新初始化 `sh_kept` 时，复用同一个 alloca，导致源列表
  `sh_rest` 被新空列表覆盖，出现乱值或崩溃。

修复方式：将 remove-at 循环提升为 `pub` 自由泛型函数
`random_remove_ith`，由 `random_sample` / `shuffle` 调用。同一逻辑在自由
泛型函数中经 native 与 VM 双端验证均正确；这也符合 std 模块 loader 只携带
`pub` 项的既有约束。

- **验证**：`audit2_std_random_sample_shuffle_semantics_dual` 现为 VM +
  codegen 双端测试，输出 `3`，native 不再崩溃；`audit_stdlib_*` 14 tests
  全绿。
- **根因登记**：codegen 列表变量跨循环赋值时的 alloca 复用/别名问题仍
  留在 codegen 层面，`std/random.mimi` 的先绕行不是对该根因的最终修复。

### 0.36.64 — Phase E：audit_fix_stdlib 批量补齐 codegen 侧双后端守卫

继续执行 Wave-1 遗留的“每个 stdlib 回归都应有 codegen 侧”纪律
（wave1-review §6.1）。此前 `audit_fix_stdlib.rs` 中 8 个 stdlib 回归仍为
VM-only，本次为以下场景补上 `audit2_compile_and_run_with_stdlib` native 断言：

- `mymath.mimi`
  - `gcd` 绝对值归一化
  - `lcm` 溢出安全与绝对值
  - `factorial` 13! 溢出守卫
  - `try_pow_int` 负底数/溢出边界
  - `random_exponential` 非法 λ 哨兵
- `strings.mimi`
  - `words` / `count_words` 空 token 过滤
- `collections.mimi`
  - `take` / `drop_n` 负数 n 守卫
- `fs.mimi`
  - `file_size` 按字节计数的 UTF-8 多字节文件
  - `file_size` 缺失文件 Err
- `result.mimi`
  - `ResultExt::map` 在显式结果注解下重建 Err 载荷

- **验证**：`audit_stdlib_*` 14 tests 全绿（VM + codegen 双端），未引入语义变化。
- **剩余 TODO**：`random_sample/shuffle` 的 codegen 侧仍被既有 while-loop
  泛型方法编译缺陷阻塞，已在源码注释中保留登记。

### 0.36.63 — Phase E：物化裸 `let ref` 的 canonical Reference（V-1）

审计遗留 `V-1`（一审 §16，Wave-3）：`let ref r = v` 若出现在非 arena 顶层，
且 `r` 从未被使用，lowering 的 `reference_binding_type` 会在 Resolved
TypeTable 中找不到 canonical Reference（该类型只在 ref 变量作为表达式出现时
才被 intern），于是 fail-closed 报内部错误。合法程序 `let ref r = v; 0` 被拒。

- **根因**：checker 已把 `let ref` 的变量类型建为 `Type::Ref`，但 Resolved
  IR 的节点类型表只从“表达式类型”填充；未使用的 ref 绑定没有对应的表达式，
  因此 Reference 未进入 `ResolvedTypeTable`。
- **修复**：
  - `NodeMeta` 新增临时 `ref_binding` 关联键（记录 `let ref` 初始化表达式）；
  - `collect_stmt_meta` 为 `Stmt::Let{ ref_: true, init: Some(_), .. }` 设置该键；
  - `build_canonical_function_signatures` 仿照 shared-binding 路径，用
    初始化表达式的 checker-finalized 类型构造 `Type::Ref(None, T)` 并 intern
    为 canonical Reference，同时写入对应 let 语句节点类型；
  - 构造完成后清空临时键，保持与 expression/shared/type_operand 相同的边界纪律。
- **回归**：解除 `fix3_ref_nonlinear_let_still_checks` 的 `#[ignore]`，裸
  `let ref` 不再使用的情况下也能完整通过 checker + lowering。

### 0.36.62 — Phase E：check_program 诊断出口统一按源码位置排序（§4-#43）

审计遗留 `§4-#43`：Resolved IR 内部使用多个 `HashMap` 目录，早期在部分
路径已排序（checker 出口、R-2 选择确定性），但 `check_program` 的公共出口
没有统一排序。同一非法程序在不同运行/不同 Hash 种子下可能得到不同诊断顺序。

- **修复**：`src/core/mod.rs` 新增 `sort_diagnostics`，并在
  `check_program` / `check_program_strict` 的公共边界对所有 `Err(Vec<Diagnostic>)`
  统一做源码位置排序；同位置按 message / code 稳定排序。
  - 覆盖 `flow_check_*` 阶段错误与 `CheckedProgram::from_flow_acc` 后续阶段错误。
- **新增回归**：`check_program_diagnostics_are_source_sorted`——构造两个
  未解析函数错误，断言公共出口已是按 `(line, col, message, code)` 排序。
- **影响**：仅改变错误输出顺序，不改变语义或诊断集合。

### 0.36.61 — Phase E：修 cap/dyn/builtin 方法探测投机降低 receiver（R-5）

审计遗留 `R-5`：`lower_expr` 对 `receiver.method(...)` 先尝试
`lower_cap_method_call` / `lower_dyn_method_call` / `lower_builtin_method_call`
。这些探测函数若以“先 `lower_expr(receiver)`、再判断类型”的方式实现，
当 receiver 不是对应目标时，已降低的 receiver 会被丢弃，随后真实方法路径
会再次降低同一 AST，可能造成重复语义身份/局部变量（伪 identity
collision）。

- **修复**：三个探测函数均改为先用 `expr_id(receiver)` 从
  `node_types` 读取 checker 已定型的 receiver 类型，确认属于
  cap / dyn / builtin 方法后才真正 `lower_expr(receiver)`；非目标 receiver
  不再被投机降低。
  - `lower_cap_method_call`
  - `lower_dyn_method_call`
  - `lower_builtin_method_call`
- **新增/验证**：`core::ir::lower::tests` 52 passed，覆盖 actor/impl/内置/
  transition 方法调用，均未出现重复身份回归。

### 0.36.60 — Phase E 查缺补漏：i64::MIN 基数拼写 + silent_transition 包裹块审计

#### 1. i64::MIN 支持十六进制/二进制/八进制拼写

全仓审计遗留清单里 `§1-#11`（i64::MIN 仅十进制可拼写）为低优先级语法缺口：
`-9223372036854775808` 可用，但等值的 `-0x8000000000000000`、
`-0b1000...`、`-0o1000...` 在解析阶段因正数形式超出 i64 范围而报
“invalid hex/binary/octal integer”。本轮补齐，使所有受支持整数基数都能
拼写 i64 下界。

- **修复**：
  - `src/parser/helpers.rs` 新增 `is_i64_min_magnitude`，识别十进制、
    十六进制、二进制、八进制四种写法中的 `2^63` 幅值；
  - `src/parser/parse_expr.rs` 一元负号路径与 `src/parser/pattern.rs`
    负字面量模式路径统一改用该 helper，直接折叠为 `Lit::Int(i64::MIN)`。
- **新增回归**：`audit2_pm_i64_min_radix_spellings_parse_and_match`
  - 表达式侧：三种基数赋给 `i64` 变量并与十进制 `i64::MIN` 相等，运行返回
    42；
  - 模式侧：三种基数负模式均可解析（保持与既有十进制负模式一致的解析层
    契约）。

#### 2. silent_transition 的跨边界检测覆盖 Defer/OnFailure/Parasteps/Pinned

审计遗留 `§4-#39`：`has_cross_boundary_ops` 只递归 Block/Arena/Unsafe/
IeeeFloat，漏掉 `defer`/`on failure`/`parasteps`/`pinned` 包裹块，导致 stay
transition 若把 `emit`/`send_event`/`channel_send` 等跨边界调用藏进上述
包裹块时会被误判为 silent。

- **修复**：`src/core/resolved/mod.rs` 的 `stmt_has_cross_boundary` 为
  `Defer`/`OnFailure`/`Parasteps` 补齐块内递归；`Pinned` 同时检查 `expr` 与
  `body`。
- **新增回归**：`has_cross_boundary_ops_covers_wrapper_blocks`——四种包裹块内
  的 `emit(...)` 均被识别为跨边界；空包裹块仍为 false，防 all-true 回归。

#### 3. async ensures 合约的 `result` 局部类型对齐函数体真实返回类型

审计遗留 `R-3 / §5-LOW`：`lower.rs` 降低 `ensures: result ...` 合约时，把
`result` 局部登记为 `signature.result`（调用方视角 `Future<T>`），而函数体
本身按 `body_result`（内部 `T`）降低。async 函数一旦启用，合约里的
`result` 类型将与函数体/尾表达式不一致。

- **修复**：`src/core/ir/lower.rs` 的 `Stmt::Ensures` 分支从
  `self.signature.result.clone()` 改为 `self.body_result.clone()`，与 body /
  return 降低使用同一类型。
- **影响**：当前解析器还不接受 `async func` 顶层语法（该路径由内部构造与
  未来异步语法接管），但语义定位已消除根因。

- **挣绿**：`audit_fix_parser` 47 passed、lib 全量 5460 passed；
  `cargo fmt --check` / docs / edge 门禁保持绿。

### 0.36.59 — Phase E：fails transition 尾包装 `return` 的 Ok-wrap 修复

探针发现 `ieee_float { return B { ... } }` 出现在 `fails E` transition 中时，
VM 进入 Ok 分支，而 native 进入 Err 分支——一个明确的双后端 L1 分歧。

- **根因**：`compile_block_last_val` 的 `Stmt::Return` 路径在处理尾位置
  wrapper（`ieee_float`/`unsafe`/`arena`/`block`）时，漏掉了
  `in_fails_transition` 的 `Ok` 包装。返回值被当成裸 state，native 将成功的
  transition 误判为 Rejected/Err。
- **修复**（src/codegen/block.rs）：在 `compile_block_last_val` 的 Return 分支
  补齐 `if self.in_fails_transition { val = self.compile_ok_constructor(vec![val])?; }`，
  与 `func.rs` 普通 Return、`block.rs` compile_block Return 保持一致。
- **新增回归**：`dual_ieee_flow_fails_ok_state_native`
  - 有限值：`ieee_float { return B { value: 1.0 } }` VM/legacy/resolved 均走
    Ok 并输出 `ok 1`；
  - NaN：Ok 分支取出 NaN 后，离开 ieee_float 的首个乘法在 VM/native 双方触发
    E0813。

### 0.36.58 — Phase E 审计记录：raw legacy `compile_file` 的线性边界定位

对“旧 state 复用”做双后端 fail-closed 复核时，确认生产路径与测试路径存在边界：

- 生产/CLI：`mimi build` 走 `compile_checked`，先由 checker 拒绝 E0423，不会
  生成旧 state 复用产物；
- resolved 路径：`checked_codegen_compile_and_run` 同样失败关闭；
- **边界**：`compile_file` 是测试专用的 raw legacy 发射器，只解析 AST，不跑
  checker；因此 `flow_state_use_after_transition_rejected` 这类负测试必须通过
  `check_source` / checked 路径锁定，不应依赖 raw legacy 发射器拦截线性错误。
- 已记录到 Phase E 不变量：旧 state 复用的权威保护在 checker + production
  native 路径；raw legacy 测试 harness 不承担线性类型检查。
- 新增 checked fail-closed 回归：
  - `dual_flow_old_state_reuse_checked_fail_closed`——E0423 在
    `check_source` 与 `checked_codegen_compile_and_run` 双入口均失败关闭；
  - `dual_flow_try_after_linear_consumption_checked_fail_closed`——E0429
    在 checked 管线编译前失败关闭；
  - `dual_flow_try_before_linear_consumption_f64_native`——`?` 前线性消费的
    legacy `Ok(flow_state)` 解码扩展覆盖 f64 record payload，VM/legacy/resolved
    输出一致。

### 0.36.57 — Phase E：legacy `Ok(flow_state)` 匹配解引用闭环 + `?` 前线性消费双后端

继续 Phase E “`?` 前线性消费”双后端锁定时，发现 legacy 发射器对
`fails E` 返回的 `Result<Ready, (Pending, string)>` 匹配 `Ok(s1)` 存在隐藏
分歧：`flow_result_static_state` 把 `Ok`/`Err` 也视为 flow-state 构造子，导致
`Ok` 臂走了“静态死臂” sentinel 绑定，`s1` 被绑成 `i64`，随后 `s1.data` 报
“field access requires a struct or actor type, got i64”。

- **修复**（src/codegen/expr/match.rs + src/codegen/expr/access.rs）：
  - static flow-result 绑定路径增加 `find_variant_ordinal_scoped(...).is_err()`
    门禁：只有真实 flow-state 构造子（如 `B { value }`）走静态 record 路径；
    内置 `Ok`/`Err` 继续走通用 Result payload 绑定；
  - 对 built-in `Ok` 的 record payload（legacy 以 ptrtoint i64 传递）从
    `Result<T, E>` 推导 T 的 AST 类型并注册 `var_type_names`；
  - `materialize_field_base` 对“已知 record 类型 + i64 值”支持 inttoptr + load，
    使旧发射器也能对 `Ok(flow_state)` 做字段访问。
- **新增回归**：`dual_flow_try_before_linear_consumption_native`——`?`
  在线性消费前合法，VM/legacy/resolved 三端均输出 `10`。

### 0.36.56 — Phase E 首项：单目标 Flow f64 payload 匹配 ICE 修复 + ieee_float × Flow 边界回归

Phase E（加固与冻结）开始后首个攻击面来自路线图点名的 `ieee_float × Flow
边界逃逸面`。构造探针时发现更基础的门禁缺口：**Flow 状态 payload 含 f64 时，
单目标 transition 结果的 native match 直接 ICE**，导致该边界完全无法在 native
侧验证。

- **缺陷**（src/codegen/expr/match.rs，legacy emitter）：
  1. 单目标 flow 结果是普通 state record（无 __MultiTarget enum/tag），但
     `compile_match_expr` 对任意 `StructValue` 都提取首字段当整数 tag；当首字段
     是 f64 时 `extractvalue { double }` 后 `.into_int_value()` panic。
  2. 静态死臂 sentinel 一律绑 `i64 0`；若活臂返回 f64 绑定值，match phi 因
     `i64 vs double` 报 E0200 不统一。
- **修复**：
  - 通过 `flow_result_static_state` 识别“非 Result 包装的单目标 flow record”，
    对这类 match 走 tag-less 路径；
  - 静态死臂按 state record 声明字段的 LLVM 类型构造 `const_zero()` 哨兵，
    phi 类型与活臂一致；
  - 静态分派仅在 `find_variant_ordinal_scoped` 找不到构造子时触发，避免误伤
    `fails E` 返回的 `Ok/Err` 内置 Result 匹配。
- **新增回归**：
  - `dual_flow_f64_payload_match_native`——f64 flow payload 单目标 match 三后端
    （VM/legacy/resolved）输出一致；
  - `dual_ieee_flow_nonfinite_reentry_trap_native`——ieee_float 内产生的 NaN
    可经 transition payload 逃出，但离开 ieee_float 后的首个确定性浮点乘法仍
    在 VM/native 双方触发 E0813；
  - `dual_flow_match_guard_native`——单目标 flow 结果上加 match guard，正/负
    两条路径均与 VM 等价（guard 失败必须落到下一 arm）。
- **挣绿**：`flow_turn_` 全组 14 passed；新增双后端测试通过；全量 lib 5453 项
  中除一次网络超时 flake（单独重跑通过）外全绿；docs/edge/fmt 门禁保持绿。

### 0.36.55 — 0.1.6 四支柱里程碑核验：失败/状态/线性/语法全部“定案 + 挣绿”

0.36.54 完成 Phase D 收官后，本 sprint 做一次跨 Phase A–D 的里程碑复核，把
路线图 §3/§5 的四支柱验收结论固化：

- **失败归属（Phase A，0.36.3–11）**：Fault payload 已名义化为 `StateId` /
  `EventId`；`last_state == "..."` 字符串逃逸面归零；recover 走穷尽 `match`；
  DoD 门禁 `check_fault_nominal_gate` 在线。
- **状态语义（Phase B，0.36.13–21/29）**：Actor `mut` 定案为简单状态逃生舱 +
  lint，spec 无双状态语义表述；Protocol 定案为 checker-only 静态投影 +
  `unsafe_cast_protocol` 稳定逃生舱；`dyn Protocol` 判给宿主语言 trait
  object。
- **线性系统（Phase C，0.36.36–49）**：`List<cap>`/`Option<cap>` 元素级消费
  （for/match/if-let/`xs[0]` 定向头提取/容器方法变换面）双后端等价；Session
  typed 端点构造 + residual lowering 在 VM/native 全闭环；legacy 转移面
  p13/p14 清零。
- **语法重设计（Phase D，0.36.50–54）**：关键字清理、`?` 去歧义、作用域守卫
  单机制、软关键字政策全部定案；语法权威已同步；`check_phase_d_syntax_gate`
  在线。
- **挣绿复核**：`cargo test --quiet --lib --tests` 全量 5450 lib + 15 dual +
  31 integration + 1 real_world 全绿；`check_language_docs.py` /
  `check_edge_isolation.py` / `cargo fmt --check` 全绿。

**0.1.6 四支柱已全部达到“定案 + 挣绿” ✅。** 下一阶段进入 Phase E（加固与
冻结，0.36.76–95）/ Phase F（复核与锚定，0.36.96+）。

### 0.36.54 — Phase D 定案 + 挣绿收官：语法权威同步 + Phase D 门禁（语法重设计）

承接 0.36.50–53 的最后四个落地点，本轮完成 Phase D 的正式收官核验与语法权威
同步。语义/实现已在之前 sprint 挣绿，本轮把“定案”钉进可检查门禁：

- **关键词句（路线图 DoD 对照）**：
  - `?` 无三义 ✅：`T?` 别名产生式已删（0.36.27），当前只有 `try ?` 与
    `?.` 两义；
  - 作用域守卫单机制 ✅：0.36.15 修 resolved 发射器 + 0.36.29 定案方案 B，
    保留 `defer { }` / `on failure { }` 双表面、共用同一条登记/出口发射单轨
    机制；
  - FFI 专用关键字退出全局保留字表 ✅：`c_shared/c_borrow/c_borrow_mut/
    local_shared/weak_local/raw_string/parasteps/reset/recover/fault` 已全部
    下沉（Ident/上下文标识符/flow 体内上下文），关键字表 63 词（60 硬 +
    and/or/not 软）；
  - 软关键字僵尸清零 ✅：66 词表锁定（0.36.51）、`not` 表达式起始歧义修复、
    `all_soft_keywords_bindable_in_let` 覆盖 old/view/mutate/persistent/
    session/dual/end/parasteps/fault/reset/recover/and/or/not。
- **语法权威同步**：`devdocs/v0.34/golden/syntax-reference.golden.md` 与
  `docs/syntax-reference.md` 补齐 0.36.15/0.36.27/0.36.50-53 的漂移——删除
  残留的 `Type := PostfixType { '?' }` 活跃产生式，补 `defer`/`on failure`
  的 0.36.15 注记，版本升级为 v0.1.6-dev（0.36.X）。
- **门禁硬化**：`scripts/check_language_docs.py` 新增
  `check_phase_d_syntax_gate`——禁止 `T?` 别名以活跃产生式回潮，禁止
  `defer failure` 在无移除标记的当前表面描述中出现。
- **挣绿**：`cargo test --quiet --lib --tests` 全量 5450 passed / 0 failed /
  7 ignored（ASan 工具门禁）；dual_backend 15/15；real_world 1/1；
  `python3 scripts/check_language_docs.py` 全绿；`cargo fmt --check` 净。

**Phase D DoD 全部达成**：`?` 无三义；作用域守卫单机制；FFI 专用关键字退出
全局保留字表；软关键字僵尸清零。语法重设计支柱“定案 + 挣绿” ✅。

### 0.36.53 — Phase D 软关键字政策：`fault` 降为 flow 体内上下文标识符（语法重设计）

继续关键字清理，最后一个仅因 flow 声明而保留的全局关键字 `fault` 也下沉：

- **词法**：`src/lexer/keywords.rs` 移除 `"fault" => TokenKind::Fault` 映射并
  从 `is_keyword_kind` 剔除；`fault` 现在 tokenize 为 `Ident`。
- **解析**：`src/parser/top_level.rs` 在 flow body 解析循环中，遇到
  `Ident("fault")` 时提升为内部 `TokenKind::Fault`，因此 `fault ErrorType`
  声明路径不变；flow 外的 `fault` 是普通标识符，`let fault = 1` /
  `func fault()` 合法。
- **文档**：`docs/syntax-reference.md` 关键字表 64 → 63（60 硬 + and/or/not
  软），golden 同步，语言文档门禁绿。
- **挣绿**：`keyword_table_count_is_63_hard_is_60`、
  `flow_words_tokenize_as_identifiers`（fault/reset/recover 均为 Ident）、
  `all_soft_keywords_bindable_in_let` 扩展覆盖 fault；既有 60 个 fault 相关
  flow/VM/codegen 测试与全量 lib 回归绿。

### 0.36.52 — Phase D 软关键字政策：`reset`/`recover` 降为普通标识符（语法重设计）

继续清理仅因历史/system 名称而保留的假关键字：

- **词法**：`src/lexer/keywords.rs` 移除 `"reset" => TokenKind::Reset` 与
  `"recover" => TokenKind::Recover` 映射，并从 `is_keyword_kind` 剔除——
  `reset`/`recover` 现在 tokenize 为 `Ident`，不再是全局保留字。
- **语义不变**：二者本来就是系统注入 transition 名（`Svc::reset(u)` /
  `Svc::recover(u)`）与方法名，不是语法结构；降为普通标识符后 flow 调用、
  checker、VM/native 路径全部不变。
- **文档**：`docs/syntax-reference.md` 关键字表 66 → 64（61 硬 + and/or/not
  软），golden 同步，语言文档门禁绿。
- **挣绿**：关键字计数测试更新为 `keyword_table_count_is_64_hard_is_61`；
  `fault_is_keyword_reset_recover_identifiers` 锁定 `fault` 仍为关键字、
  `reset`/`recover` 为 Ident；现有 `all_soft_keywords_bindable_in_let` 与
  全量 lib 回归绿。

### 0.36.51 — Phase D 软关键字政策硬化：66 词表锁定 + `not` 标识符读法（语法重设计）

继续 Phase D 软关键字政策，落实两个锁定：

- **关键字表计数锁定**：`src/lexer/keywords.rs` 新增
  `keyword_table_count_is_66_hard_is_63` 测试，硬编码 66 个词表并断言
  `keyword_or_ident` 全部映射到非 `Ident` 的 TokenKind、`is_keyword_kind`
  恰为 63（排除 and/or/not 三个软运算符）；`parasteps` 不在表中。
- **`not` 僵尸读法修复**：`not` 既是布尔一元运算符，也是 `expect_ident`
  允许的软关键字绑定名；但表达式起始处 `parse_unary_inner` 总是把 `not`
  当一元运算符，导致 `let not = 1; not + 1` 报 `unexpected token +`——
  正是“绑定位置合法 / 语句位置报错”的僵尸关键字。修复：在表达式起始处按
  后随 token 前瞻——后随可作一元操作数（标识符/字面量/括号/`not`/`!` 等）
  时保留运算符读法；后随二元运算符/分隔符/右括号/EOF 时走 `parse_primary`
  的 ident-like 路径，`not` 作变量引用；并特判 `not()` 空参调用为函数调用。
- **挣绿**：新增 `all_soft_keywords_bindable_in_let`——§1.4 全部 11 个软
  关键字（old/view/mutate/persistent/session/dual/end/parasteps/and/or/not）
  均可 `let` 绑定并运算，且 `func not() -> i32 { 42 }` + `not()` 合法；
  关键字表计数测试通过；全量 lib 回归绿。

### 0.36.50 — Phase D 预演：`parasteps` 从硬关键字降为上下文标识符（语法重设计）

按 `devdocs/v0.36/phase-d-syntax-inventory.md` §1，关键字清理清单中仅剩
`parasteps` 仍占全局保留字。本轮将其从词法硬关键字降为**上下文标识符**：

- **词法**：`src/lexer/keywords.rs` 移除 `"parasteps" => TokenKind::Parasteps`
  映射，并从 `is_keyword_kind` 剔除——`parasteps` 现在 tokenize 为 `Ident`，
  不再是全局保留字；`func parasteps()` / `let parasteps = 7` / 字段名均合法。
- **解析**：`src/parser/parse_stmt.rs` 新增 `parasteps_followed_by_block()`
  前瞻（允许换行）；当语句起始的 `Ident("parasteps")` 后随 `{` 时提升为内部
  `TokenKind::Parasteps`，复用既有并行块 AST/codegen 路径。非块场景（普通
  标识符表达式）不受影响。
- **文档**：`docs/syntax-reference.md` 关键字表 67 → 66（63 硬 + and/or/not
  软）；`parasteps` 加入软关键字/上下文标识符说明，语句产生式保留并标注
  contextual 语义。
- **挣绿**：关键字 lexer 断言 + `parasteps_identifier_freed_parallel_block_kept`
  （`func parasteps()` / `let parasteps` / `parasteps { ... }` 三种形态）；
  既有 30 个 `parasteps` 双后端/typecheck/codegen 测试全绿。

### 0.36.49 — legacy 转移面补全：隐式尾返回 cap 转移 + 方法实参 cap 转移（L1/L2）

承接 0.36.48 §4v.4 登记的两个 E0303 fail-closed 差距（p13/p14），本轮把这两处
合法转移面在 legacy native 发射器补全；checker/VM 已支持，native 此前误报：

- **p13：隐式尾返回 cap 转移**（`func id(x: cap) -> cap { x }`）——legacy
  `emit_implicit_return` 在返回前无条件 `check_unconsumed_caps()`，把尾表达式中
  转移给调用方的 cap 参数当作泄漏（E0303）。修：`emit_implicit_return` 先收集
  尾返回表达式可达的所有 cap 位置（Ident + Tuple/List/Set/Record/Field/Index
  递归，镜像 `simple.rs::collect_arg_cap_places`），逐个 `consume_cap` 簿记；
  不发射运行期 `cap_consume`——句柄所有权随返回值离开本函数，由调用方登记/drop。
- **p14：方法实参 cap 转移**（`xs.take_away(v)` / stdlib `xs.remove(v)`，v: cap）——
  legacy 方法路径（`compile_self_method_call`）从未像自由函数路径那样收集并消费
  实参里的 cap 位置，导致合法方法实参转移在函数出口被 E0303 误伤。修：在方法
  实参编译后镜像同一 `collect_arg_cap_places` 逻辑（本切在 method.rs 新增
  `collect_method_arg_cap_places`），逐个 `consume_cap`；callee 的 cap 参数由其
  自身 scope 接管。
- **fail-closed 保持**：返回前已 `drop(x)` 再返回 `x` 仍 E0304（consumed more
  than once）；同一 cap 作为两个方法实参仍 E0304；未消费/未返回的 cap 仍按既有
  门禁拒绝。
- **挣绿**：双后端 4 项新测试——尾返回 cap 正例三后端（checked + legacy native
  × VM）、返回后复用负例、方法实参 cap 正例三后端（自包含 trait `take_away` 避开
  VM `remove` builtin 劫持）、方法实参双用负例。全量相关 `dual_linear_cap_*` 4/4
  绿；`cargo check` + `cargo fmt` 通过。

### 0.36.48 — stdlib ListExt 方法面逐方法验证：变换面 ALL 线性参数转出 + resolved guard 精确化（L1/L2）

承接 0.36.47 的 4u「记录」（stdlib 余下 ListExt 变换方法逐个验证），本轮把
容器方法余面按方法摊开验证，并修复验证中暴露的三处缺口：

- **变换面 ALL 线性参数整体转出（0.36.47 补完）**：变换方法（Mutate，结果为
  List/Map/Set/Tuple 携带义务）不只接收者整体转出——`call.arguments` 全循环，
  每个线性实参（Load place 且 place_is_linear）都推 Move：
  - 容器参数（`xs.concat(ys)`）：ys 的元素义务并入结果 zs，用户只 `drop(zs)`
    ——此前 ys 义务原处 → E0256 死锁；
  - 线性值参数（`xs.remove(v)` / `xs.intersperse(sep)`）：义务进方法恰一次
    （方法体结算 或 并入结果）。
  义务守恒：结果义务 = 每个线性实参义务的并集。读/提取面（len/is_empty/
  find/count/find_map/reduce——标量/裸元素/Option 结果）参数保持借用（义务
  原处；`reduce` 归入读面：标量结果，drop(xs) 结算空壳合法）。
- **resolved eligibility guard 精确化（修 0.36.47 的误伤）**：0.36.47 的 guard
  以 `call.type_arguments` 非空判定"方法级泛型调用"→ resolved lowering 把
  impl 级泛型 T（ListExt<T>）也装进 type_arguments → **所有** trait 方法调用
  （intersperse/chunks/…）都被拒 → main 整体落 legacy → 触发 legacy for 链
  类型缺口（E0713）。修：`trait_method_generics`（checker 已有的
  (trait, method) → 方法级泛型名表）镜像到 CheckedProgram（FlowAcc → from_flow_acc
  安装），eligibility 用 MethodId（`function:{Trait}:for:{Type}::{method}:{hash}`）
  解析 (trait, method)，查**方法级泛型数 >0** 才拒（map<U>/filter<U>/find_map<U>/
  reduce<U> 仍归 legacy 单态化切片）。
- **legacy for 链类型登记（E0713「for loop requires a list or range (got
  integer)」根修）**：无注解方法链 `let cs = ns.chunks(2)` 此前不登记变量类型
  → 外层 for 把 c 绑成 i64 → 内层 `for x in c` 崩（0.36.35 同类补丁
  `let y = x` 只覆盖 Ident 继承）。修：func.rs/block.rs 的 Q3 方法调用登记点
  统一走 `register_qualified_var_type`——签名推断串（"List<T>"/"List<List<T>>"）
  中裸大写占位符替换为列表槽位名 i64（LLVM 层 List 元素恒 i64 槽，map 单态
  化本就是 `$T_i64`），登记 "List<i64>"/"List<List<i64>>" → for 元素解析、
  方法分发、单态化命名全部连通（chunks 出现 `$T_i64` mono 版）。
- **双后端等价矩阵（探针 13/16 + 链组合）**：p01 reverse / p02 take / p03
  drop_n / p04 concat / p05 remove_at / p07 first / p08 filter / p09 reduce /
  p10 partition / p11 chunks / p12 find_map / p15 intersperse + p26（intersperse
  → chunks → 双层 for 链，310）全部 VM=native 等价。p13/p14（`func id(x: cap) -> cap`
  裸返回 cap 参数 / `xs.remove(v)` 的 v: cap 方法实参转移）保持 E0303
  fail-closed（legacy capability check 未识别方法实参路径转移）——登记为已知
  差距（§4v），不放开（宁可拒不可漏）。
- **验证过程发现假警报**：native 本地可执行 exit code 被 shell 截断到 8 位
  （310 → 54=310 mod 256）——探针判定改用 mod 256 对拍（此前的"native 值错"
  均为截断误判）。

### 0.36.47 — 容器方法余面：trait 方法级泛型实例化 + 线性接收者变换面（L1/L2）

- **修既有 bug（非线性也受影响）**：`map<U>` 等 ListExt 方法的方法级泛型名
  （U）在 trait_method_sigs 注册时仍是名字型（Type::Name），调用侧从不实例化
  → `xs.map(f)` 一律 E0211「expected fn(T) -> U, found fn(T) -> T」——连
  List<i32>.map 都不可用（仓库语料此前零 `.map(` 成功用例，audit 面只测
  builtin Result.map / 显式标注）。修：`trait_method_generics` 注册方法级泛型
  名；`infer_method_call` 主 trait 分支与 `resolve_trait_method`（DynTrait/
  ImplTrait 面）经 `instantiate_method_generics` 把名字型替换为 fresh 统一变量
  （同一名字的参数/返回共享），arg 统一后 zonk 返回类型。
- **开线性接收者变换面（Phase C「容器方法余面」）**：ListExt 变换方法（Mutate
  借用标记；结果为 List/Map/Set/Tuple 且携带义务）降为消费语义——接收者容器
  **整体转出**（Move，与 for 迭代同构：容器移入方法、义务移至结果），
  `let ys = xs.reverse()` 中 ys 携带元素义务、`drop(ys)` 结算；此前 Mutate
  借用不解体容器 → 用户被迫再 drop(xs) = 不可达语义（E0256 死锁）。
  **读取/提取面不变**（len/is_empty/count/find/first/last/find_map——标量/
  裸元素/Option 结果）：借用接收者，容器义务原处保留（`xs.len()` 后须
  drop(xs)；`first()` 提取 + drop 余部 = 0.36.46 同构面）。
  健全性：变换 = 容器整体移入、结果整体移出（1:1 义务守恒）；读 = 义务不动；
  线性元素在变换回调内逐元素恰一次。
- **codegen 承接（判型放行后首次触达的 Legacy 缺口，SIGSEGV 修复）**：
  方法级泛型调用此前被判型全拒、codegen 从未见过，放行后暴露三处缺口并修复：
  1) legacy 嵌套单态化（method.rs）type_map 只绑 impl 级 T——U 未绑 → 空名字
  类型 → 坏 IR → SelectionDAG SIGSEGV；现从回调实参返回类型绑定 U（命名函数
  ret / lambda 注解 / 嵌套泛型体函数参数经当前 type_map 替换）；
  2) 方法调用路径缺命名函数实参的 closure 包装（simple.rs 自由函数路径已有）→
  `ptr @double` vs callee 期望 `{ptr,ptr}` → verifier 不匹配 + 崩溃；补
  `wrap_named_fn_arg_to_closure`；
  3) resolved 发射器 PM 臂对方法级泛型调用按未实例化符号直查——加 eligibility
  门（`call.type_arguments` 非空 → 归 legacy 单态化切片）。
- **挣绿**：6 项新双后端测试（map 变换三后端等价 ×2 值/类型面、reverse 三后端、
  方法级 U 实例化（List<i32> 双链 map = 36）、变换接收者双用 E0304、变换结果
  泄漏 E0256、读面保持（len+drop 绿 / 不 drop E0256））。
- **回归**：dual 1042/1042、lib 5443/0、clippy 0、fmt 净、四 python 门禁 0。

### 0.36.46 — 元素级投影定向分析：`xs[0]` 头提取面（L1/L2）

- **开面（定向）**：M9/0.36.25-26 起全面 fail-closed 的索引析构，本切打开
  **唯一可无损证明健全的提取形状**——定向头提取 `let c = xs[0]`（字面量常量
  0、单一投影、直接局部基、非可弃线性容器 `List<cap>`）：
  - c 认领头部元素义务（fresh 资源身份 Introduce，元素级记账）；
  - 容器保留**余部义务**（自身身份不动）：须整体消费一次（`drop` = 释放
    余部 / move / return）——不触容器 → 返回门禁 E0256（余部泄漏）；
  - 每容器至多一次索引提取：二次认领同一位置 = 超认领 → E0304
    （「head element is claimed more than once」新诊断，`extracted_containers`
    集合记账）；
  - 义务守恒：1（提取元素）+ (n-1)（余部整体消费）= n 元素义务，任意
    n ≥ 1 完整结算；空表 `xs[0]` = 运行期越界 trap（与非线性索引一致，
    非静默泄漏）。
- **fail-closed 保持**：`xs[1]` / 动态索引 / 多级投影 / 调用实参位置
  （`sink(xs[0])`）/ 元组投影 `t.0` / 切片 `v[1..]` 全部维持 E0304；
  泛型面 `first<T>(xs: List<T>) -> T { xs[0] }` 维持 E0432（0.36.39 H2，
  黑盒不投影、对 T 零依赖不变）。
- **修复**：定向面目录记账——`catalog_pattern` 对定向形状跳过来源继承
  （否则 c 拿到容器的资源身份幻影，`drop(xs)` 撞 RESOURCE-LINEAR-001
  double-consume 假阳性 / 提取后余部无法独立结算）。
- **挣绿**：8 项新双后端/concrete 测试（正例：提取+消费+余部 drop 三后端
  等价、纯释放形状；负例：余部泄漏 E0256、元素未消费 E0256、重复提取
  E0304、非零字面量 E0304、调用实参 E0304、元组投影 E0304）+
  0.36.43 两测试按新语义重述（drop 形转正例、单独形 E0304→E0256）。
- **回归**：dual 1036/1036、lib 5437/0、real_world 70/70、clippy 0、四
  python 门禁 0。

### 0.36.45 — 泛型×线性单态化切片 6：for + if-let 组合（元素级绑定面，L1/L2）

- **开面**：0.36.42 的 if-let 中介面与 0.36.40 的 for 穷举解构合流——
  `for x in xs { if let Some(y) = x { ... } }`（List 元素 = Option，逐迭代
  提取绑定）concrete + 泛型双后端挣绿；for+match 组合（臂内 `Some(y) =>`）
  同步打通。
- **concrete 面修复**（`src/core/cfg/resource_lower.rs`）：
  - if-let then 臂绑定 **Introduce 键 = then 块入口的最内层消费节点**
    （表达式/初始化器/归值下钻；块体下钻首语句或尾 result；调用实参链下钻；
    Drop 语句锚定语句节点）——键到头部分支前 → 两路径都复位 → "consumed on
    only some paths" + E0256 双误报；键到语句节点 → CFG 点序内层在前，Introduce
    落在首个消费之后（顺序翻转）。块入口点 + 动作秩排序（Introduce=3 < Move=5）
    保证"每迭代先复位后消费"（循环背边携带上迭代 Consumed 事实 → 第二迭代
    Move 撞 E0304 的根因即缺复位）；
  - match 臂模式绑定同款 per-iteration Introduce（`visit_arm` 发射）——臂绑定
    也是逐迭代临时资源。
- **泛型面修复**（`src/core/checker/linear_blackbox.rs`）：`stmts_flow` live
  清空后**剩余语句复扫已消费名字**——`sink_g(y); sink_g(y)` 第二条此前因提前
  返回脱离检查（泛型 double-use 漏网；concrete 由 dataflow Move-after-Consumed
  拒绝，双后端对齐后同拒 E0432）。
- **健全性**：组合行为与等价 concrete 副本一致（Option-ness 是容器类型性质，
  固定于泛型签名、与 T 无关——切片 1 论证延续）。
- **挣绿**：concrete/泛型 for+if-let 累加器（`[None, Some, Some]` → 2）、带
  else 形态、for+match（= 2）三后端等价；循环内弃置 then（E0256）、y 双用
  （concrete E0304 / 泛型 E0432）、非 Option 元素 if-let（E0432）fail-closed。
- **回归**：正例 4 + 负例 3 入 dual_backend（0.36.45 段）；0.36.42-44 既有
  dual 全绿（flip/consumes_container 探针形态复验）。

### 0.36.44 — 泛型×线性单态化切片 5：高阶直通——callable-值调用 + closure 臂（L1/L2）

- **开面**：高阶调用携带线性容器——`foldT(xs, fn(x: T) -> i32 { sink_g(x) })`
  泛型调体逐元素直通闭包参数（fold 计数）双后端挣绿：
  - Lambda 字面量实参 = 匿名"臂"：参数名逐一 live 黑盒结算体（恰一次转移/
    drop）；弃置参数体（`{ 0 }` 不触参数）= 具体面元素泄漏同款 → E0432；
  - 闭包绑定（`let c = fn(...)`）义务在**定义点**结算——后续调用只传闭包
    标识符时无法再检查体；捕获 live 容器名的闭包体经 expr_uses_name 的
    Lambda 递归触达 → fail-closed；
  - 方法调用（`receiver.method(args)` = Call(Field(receiver, _), args)）
    实参触碰 live 的名字逐一带整体转移（transfer_wrapped_args，构造包装臂
    提取为共享 helper）；**线性接收者**方法面（`xs.map(f)`）保持 fail-closed
    （容器方法 = 余面，stdlib map_list 体维持 E0432）；
  - 可调用值调用（`f(x)`，f = func 参数）经 transfer_wrapped_args 转移-out
    （f 的体由定义点/具体面各自追踪——具体面 closure 直通已有 0.36.44 前探针
    实证，本切片收敛泛型面）。
- **健全性**：闭包集体黑盒结算（约束恰一次）；被拒形状（弃置/捕获）均
  fail-closed；合法路径（drop 体/绑定体/方法实参转移）双后端等价。
- **已知怪癖（记实不修）**：for-面 transfer_out 模式对"内部消费型"泛型调用
  保守标为返回值携带 → 结果绑定在二元/算术位置误报活值（`r + 0` 形状拒绝，
  `r` 尾返回形状合法）——0.36.40 遗留，调用方以尾返回形状规避。
- **挣绿**：内联/绑定闭包直通 fold 计数 = 2（VM + legacy codegen + resolved
  codegen 三后端）；callable-param 循环直通 = 0。
- **回归**：正例 4 + 负例 3（弃置内联/弃置绑定/捕获）；concrete closure 面
  （0.36.44 前探针形态）保持 clean。全量 5421 绿；real_world 70/70。

### 0.36.43 — 元素析构记账修复：E0304 错误路径状态污染清零（L2）

- **背景**：M9/0.36.25-26 索引析构拒绝（`v[0]` / `v[1..]` / `(a,b).0` 在非
  droppable 线性容器上的 E0304）纯属诊断——但后续 lowering 仍把被拒投影的
  place 配对进绑定/调用/drop：`let x = v[0]` 制造 v→x 伪转移（x 获得容器的
  资源身份），`drop(v)` 随之撞 **RESOURCE-LINEAR-001 double-drop** 调试信号
  （`drop(v[0]); drop(v)` 与元组 `(a,b).0` 同形）。诊断侧只报 E0304，另一条
  CFG 路径还误报 "consumed more than once"。
- **机制**（`src/core/cfg/resource_lower.rs`）：
  - `rejected_extraction_places`：reject 时把被拒投影的 canonical place 记入
    集合；消费漏斗 `collect_capability_places` 与 Drop 臂过滤（后续消费者不再
    为从未移动的值制造转移/消费）；
  - Bind 臂 `last_visit_rejected` 守卫：被拒初始化器整体跳过配对，并清除绑定
    局部上由先前物化写入的幻影所有权（drop(x) 只释放 x 自身）；
  - `drop(v[0])` 在 Drop 臂就地拒绝（Drop 携带 resolved place、不访问表达式，
    此前完全绕过 reject 机制）。
- **健全性**：被拒代码经 E0304 整体失败——本修复纯属错误路径卫生（过滤被拒
  投影的配对），合法程序路径零影响（整体消费/正常析构回归测试确认）。
- **挣绿**：全部探针从 assert 归零为干净单 E0304；合法整体 drop 双后端 1。
- **回归**：正例 1 三 harness + 负例 4（let+drop / drop-by-index / 元组投影 /
  单独提取）。全量 5415 绿；real_world 70/70（69 build+exec）。

### 0.36.42 — 泛型×线性单态化切片 4：if-let 容器义务消解的泛型镜像（L1/L2）

- **背景**：0.36.40/41 记录的"if-let 非穷举面"——泛型体内 `if let Some(x) =
  o` 整体 E0432。具体面（0.36.36）if-let 对 Option 已支持义务消解（Some 路径
  绑定负载、None 变体零负载；**no-else 亦合法**，probe_il4 实证）——泛型面
  缺镜像。
- **机制**（`src/core/checker/linear_blackbox.rs` + `src/core/checker.rs`）：
  - `Stmt::IfLet` 臂：scrutinee 整体包含恰一个 live 名（投影/调用位置
    fail-closed）；then 块绑定名黑盒流动（恰一次；臂内弃置 = 具体面 E0256
    同款禁令）；then/else 块内不得再触容器名；零绑定模式（`if let _ = o`）
    = 整个容器弃置 → drop 门禁；
  - **Option 中介面开关**：`blackbox_param_scrutinee_option` 由
    `generic_linear_blackbox_sound` 按参数表面类型设置/恢复（容器类型性质
    固定于泛型签名、与 T 无关 → 健全性论证延续）；else/no-else 无 drop 门禁
    （None 零负载）——transfer-only 会话经 if-let 的值转移情境也合法（臂内
    协议 action 仍受 0.36.40 builtin 篱笆 → E0432）；
  - **非 Option 容器 fail-closed**：List `[a]` 模式 / Result / 自定义枚举不
    匹配余部义务不可静态表达 → E0432（concrete E0256/E0304 同款）；
  - 配套修复：`stmt_uses_name` 补 `Stmt::IfLet` 覆盖（与 0.36.40
    expr_uses_name-Match 同类触碰检测孔）。
- **挣绿**：`if let Some(x) = o { n = n + sink_g(x) }`（带/不带 else）双后端
  1——具体面 0.36.36 义务解消的泛型镜像面正式闭合。
- **健全性**：Option-ness = 容器类型性质（泛型签名固定，不依赖 T）→ 任意
  具体线性实例化下 if-let 消解行为 == 等价 concrete 副本；容器消费恰一次
  （then 绑定处理或 None 空负载）。
- **回归**：正例 2 三 harness（else / no-else）+ 负例 3（List E0432 / 臂内
  弃置 E0432 / 会话 action E0432）。全量 5410 绿；real_world 70/70（69
  build+exec，flow_test_macros SKIP 为既有 VM-only 面）。

### 0.36.41 — 泛型×线性单态化切片 3：match 臂残差分支级复位（会话元素面，L1/L2）

- **背景**：0.36.40 记录"匹配臂残差顺序共享"为未覆盖面——match 臂顺序分析，
  第二臂看到第一臂推进后的会话残差（E0414 AlreadyEnded 实证），跨臂协议无法
  独立闭环。本切闭合：**臂是互斥分支（替代关系），非顺序**——每臂从 match
  入口残差独立分析。
- **机制**（`src/core/infer/match_.rs` `infer_match_expr`）：
  - 臂循环前置 `pre_match_residuals` 快照；每臂：恢复到入口状态 → check_pattern
    → **模式绑定 SessionChan 残差种子**（`seed_pattern_session_residuals`，收集
    Variable/Constructor/Tuple/Array/Slice 叶，镜像 Let 绑定种子）→ guard → 臂体；
  - 汇合：**仅比较入口已追踪端点**（臂局部引入的端点无续存义务、不参与汇合）——
    任一臂缺键（别名转移后未续存）或残差互异 → E0425（fail-closed，镜像
    `Stmt::If` 分支合并）；合并态 = 第一臂残差（作用域出口 E0425 继续兜底未
    完成端点——臂内截断的协议经合并态在函数出口表面化）；
  - 臂内订单检查闭环：`Some(d)` 的 d 经种子获得完整协议残差——`session_close(d)`
    于 !i32 头 → E0414（此前按 untracked skeleton 静默放行）；臂内弃置 →
    作用域出口 E0425。
- **挣绿**：`flip<T>(o: Option<T>) -> Option<T>`（0.36.40 构造包装）+ 调用方
  match 提取 + 每臂独立协议 = **"SessionChan 经 Option 提取"全协议端到端往返
  （双后端 6）**——路线图"Option<T> 解包线性元素（会话面）"目标面闭合。
- **健全性 = 既有契约的逐臂应用**：臂内订单检查为具体面既有契约；入口端点
  跨臂一致 = 0.36.38 §4d 分支汇合不变量；臂局部端点不参与汇合（无续存义务）。
  余面（已记录，非本切范围）：`if let`（If 合并对模式绑定会话仍 fail-closed）、
  closure 臂、`xs[0]` 投影定向分析（E0432 维持）。
- **回归**：正例 1 三 harness（session option extract 端到端往返）+ 负例 3
  （臂内订单 E0414 / 臂内弃置 E0425 / 汇合发散 E0425）。全量 5405 绿
  （0.36.40 11 项与既有 0.36.38 session 面全绿）；real_world 70/70
  （69 build+exec，flow_test_macros SKIP 为既有 VM-only 面）。

### 0.36.40 — 泛型×线性单态化切片 2：结构化整体消费（元素级贯通，L1/L2）

- **背景**：切片 1（0.36.39）只放行"整体值转移 / 显式 drop"的黑盒调体——
  任何结构性消费（for/match 解构、投影）仍 E0432。本切打开**穷举解构面**
  （路线图"`List<cap>`/`Option<cap>` 元素级消费经泛型"）——调体可对参数做
  结构化整体消费，条件逐条对齐具体面既有契约：
  - `for` 穷举逐元素解构（0.36.37 周期语义）：容器作为 iterable 整体出现，
    元素绑定在循环体内按黑盒规则处理（`n = n + sink_g(x)` 恰一次消费）；
    `for (a, _) in v` 的元组通配弃置 = drop 门禁；
  - `match` 穷举解构（0.36.36 容器义务消解）：scrutinee 整体出现，每臂绑定
    名在臂体内黑盒处理（`Some(x) => sink_g(x)`；臂内弃置 = 具体面 E0256
    同款禁令）；无绑定臂（`_` 等）静默弃置 = drop 门禁；`None` 零参构造在
    模式面解析为裸标识符 → 无绑定无弃置；
  - `Assign` 二元累加槽位（`n = n + sink_g(x)` / `n = n + match x { .. }`）
    路由"转移表达式"（Call / Match）；
  - **构造包装** `Some(x)` / `Ok(v)`（非函数非 builtin 的标识符调用 = 数据
    构造器）按元组字面量同款整体值处理，实参内可嵌套转移链
    （`Some(attach(x))`）——`flip<T>(o: Option<T>) -> Option<T>` 成为合法面。
- **机制**（`src/core/checker/linear_blackbox.rs`）：`pattern_binding_info`
  （模式绑定名 + 弃置标记，零参构造特判）；`match_flow`（臂/guard/body 三重
  校验 + scrutinee 消解）；`expr_tail_flow`（统一尾表达式流：Call/Match/
  Block/整体包含四分支）；`expr_uses_name` 补 Match 覆盖（根因：`n + match x`
  的触碰检测此前漏 Match → 黑盒误判）；`expr_whole_contains` 改 checker-aware
  并识别构造包装；`call_transfer` 构造包装回退路径（每 live 名恰一实参完整
  反应，嵌套转移链递归）。**健全性 = 切片 1 论证的严格推广**：解构形态对 T
  的线性性零依赖（穷举性由正常 checker 按具体类型保证），调用方义务仍由
  call-site 具体类型追踪（`let o2 = flip(o)` 漏消费 → E0256）。
- **会话 transfer-only 维护**：任何弃置形态（`_` 臂、臂内 drop）经 `dropit<T>`
  等价路径 E0432；构造包装在 transfer-only 模式同步放行（flip 接受
  SessionChan 实例化，调用方协议 E0425 职责不变）。
- **未覆盖面（记录）**：匹配臂残差分支级复位（match 臂顺序分析共享残差——
  第二臂看到第一臂推进后的残差，E0414 已实证）、closure 臂、`if let` 非穷举、
  投影接口（`xs[0]` 元素析构保持切片 1 的 E0432）。
- **回归**：正例 7 三 harness（List 元素消费 2 / Option 解构 1 / 嵌套
  List<Option> 2 / flip cap 12 / let-sink 2 / wildcard 臂 1）+ 负例 4
  （for-leak / match-abandon / wildcard-session / flip 漏消费 E0256-
  no-E0432 + flip session E0425-no-E0432）。全量 5401 绿；real_world 70/70
  （69 build+exec，flow_test_macros SKIP 为既有 VM-only 面）。

### 0.36.39 — 泛型×线性单态化切片 1：线性黑盒直通（E0432 边界首开，L1/L2）

- **背景**：路线图 Phase C「泛型×线性单态化」首付。§2.3（0.34.21）以来线性值
  （cap/SessionChan/Flow state/含线性元素容器）全量 E0432 拒绝——泛型参数
  `is_linear() == false`，线性值流经泛型调用会逃逸 exactly-once。0.36.36-38
  已把"名字级"追踪做实（call-site 具体类型驱动：实参按具体类型移动、返回绑定
  按实例化类型追踪），唯一缺口 = 调体可能静默弃值。本切打开唯一可无损证明
  健全的面：**线性黑盒调体**——对 T 的线性性零依赖（每条路径整体转移或显式
  drop，绝不静默丢弃、绝不投影/析构/读入条件循环/转移后复用）→ 放行；
  其余形态维持 E0432（fail-closed 地板）。
- **机制**（`src/core/checker/linear_blackbox.rs`）：路径敏感流动分析——
  `FlowState { live, consumed }`；整体值位置判定 `expr_whole_contains`
  （`return x`/`(x,)`/`[x]`/record 字面量 = 整体；`xs[0]`/`x.f`/调用实参 =
  投影，H2 元素弃置逃逸不因本切打开）；可信接收者链递归判定（记忆化 +
  递归守护，闭环 fail-closed）；`let y = g(x)` 链式转移按接收者 transfer-模式
  判定返回值是否携带。**SessionChan（含任意嵌套）transfer-only**：transfer-
  模式禁 drop 从句（concrete 面 drop 未完成协议 = E0425 同款禁令）——
  `dropit<T> { drop(x) }` 接受 cap 实例化、拒绝 SessionChan 实例化。
- **切入门**：infer 全局调用（simple.rs）+ turbofish 实例化（method.rs）两处
  E0432 站点按参数黑盒健全性决定豁免；closure 臂维持 E0432（切片 2）。
- **迁移注记**：无语法 breaking；语义面从"泛型调用一律拒绝线性实参"收窄为
  "拒绝黑盒不健全的调体"（负例仍全量 E0432；E0432 消息补充
  "pass-through/drop-only generic body" 提示）。
- **回归**：负例 9 项（cap discard / container projection / wildcard discard /
  single-branch abandon / reuse-after-transfer / session drop / missing-drop
  E0256 保持 / turbofish-swallow）+ 正例 5 项三 harness（cap/container-whole/
  branch/session-transfer roundtrip、turbofish pass-through）。全量 5390 绿；
  real_world 70/70（69 build+exec，flow_test_macros SKIP 为既有 VM-only 面）。

### 0.36.38 — session_pair::<S>() 类型化对端：双端残差全闭环（L1/L2）

- **背景**：§4d 候选取 (A) 落地。0.36.32-34 的 `session_open::<S>()` 只建造
  单端点（lo），hi 端仍是 raw i64——整个对端的协议面是"死面"
  （0.36.23）：`let pair = session_pair(); pair[i]` 形式对 hi 端零检查。
- **设计**：`session_pair::<S>()` → `(SessionChan<S>, SessionChan<dual S>)`：
  lo 端说 S，hi 端说 dual(S)（首动作 = Recv，恰与物理交叉接线
  send-lostop/recv → recv-hi 一致）。`dual` 以程序化类型
  `Type::Name("dual", [S])` 由 checker 构造（用户不写 `dual` 类型字面）；
  `builtin_nominal` 注册 `dual` 为"注解身份"（SessionChan 值类型从不 lower
  类型实参 → i64 句柄，dual 永不成为运行时类型）。普通形兼容：
  `session_pair()` → 类型从 `List<i64>` 改为 `(i64, i64)`（pre-1.0 breaking，
  语料机械迁移 `pair[0]`/`pair[1]` → `let (ch0, ch1)`）。
- **残差播种统一**：新增 `session::residual_from_chan_type`（普通名 resolve；
  `dual X` → dual(resolve(X))），替换两处 infer/三处 checker 参数播种；
  三个后端（legacy 元组 load、resolved slice、VM）共用 {lo, hi} 运行时形状。
- **调用点幂等（顺带修复）**：Assign 臂的 expected-type 重检
  （`check_expr`）会重推 RHS——`total = total + session_recv(ch1)` 中的
  session_recv 被第二次 execute → 伪 TOOL-RESOLUTION-001 + 残差双推进。
  修复：`session_recorded_for_call`——同一 call-site 若已记录且当前残差 ==
  记录 after，则纯回声（不推进，recv 从记录 before 纯计算载荷类型）；
  残差偏离记录态 = 真正二次执行 → 维持 fail-closed 冲突错误。
- **循环边界（定案）**：while 体残差 P0-4 保存/还原——循环后 continuation
  不得假设循环内 send 已发生：close/作用域退出在还原态上 E0414/E0425
  fail-closed（typed 对端不再静默放行；旧 raw 形式什么都不查）。
- **迁移注记**：`let pair = session_pair(); ch0 = pair[0]; ch1 = pair[1]` →
  `let (ch0, ch1) = session_pair::<S>()`（lo 说 S，hi 自动 dual）。

### 0.36.37 — for 迭代 List<cap> 元素消费：周期语义 + 容器义务消解（L1/L2）

- **背景**：§4g 矩阵最后一块阻塞面——`for x in v { sink(x) }`（List<cap>）
  此前三错误阻塞（E0304 "x moved after consumed" 循环 backedge 重复消费假象
  + E0256 v 于 for 分歧路径/return 路径）。
- **设计**：for 循环 = 穷举逐元素解构（候选取 (1) 从 match/if-let 扩展）。
  两套独立机制：
  ①**容器义务消解**：对 builtin 序列名义容器（List/Map/Set，含线性实参）在
  循环语句点发射 Drop（pre-header，仅一次）——容器义务由循环整体消解；
  ②**逐次元素义务**：循环变量在 pattern Binding 点（body 入口，环路载点）
  逐次 Introduce——每次固定点扫描都从 Available 起步，body 内消费
  （sink(x)）绝不再触发 backedge E0304 假象；body 跳过消费（continue/
  条件分发/提前 break）→ 元素 Available 于 loop-carried 分歧 sink → E0256。
- **守卫（fail-closed）**：与 0.36.36 同精神——线性槽位 wildcard
  （`for _ in v`）搁浅 → 容器义务不解消（E0256 v）；**提前退出**（body 含
  break/return）不解消（运行时未迭代元素被放弃）→ E0256 v；嵌套循环内的
  break 归内层（仅 return 穿透）；while-let 初始值逐轮重求值且**不消费容器
  绑定**（运行时语义——消解将误收无限循环）→ 容器保持阻塞，仅逐次元素
  记账生效。
- **方块对照**（§4g）：`for x in v { sink(x) }`（List<cap>）E0304+E0256×2
  → ✅ 受准（三 harness 印 3）；负例保持：元素未消费 E0256（双路径）、
  wildcard E0256 v、循环后复用 E0304、提前 break/return E0256 v、while-let
  Option<cap> E0256 o。
- **回归**：dual_linear_for_loop_list_consumes_elements（三 harness 印 3）、
  dual_linear_for_loop_strand_still_rejected、dual_linear_for_loop_wildcard_
  still_rejected、dual_linear_for_loop_post_use_rejected、dual_linear_for_
  loop_early_exit_stays_rejected、dual_linear_whilelet_option_container_stays_
  rejected。全量全绿（门禁见下条）。
- **顺带修复（real_world 门禁回归，L1）**：`tests/real_world/flow_order_system.mimi`
  自基线起 `mimi build` 失败（legacy E0700 "field access … got i64"）——main 的
  `Err((src, e))`（Constructor + Tuple 子模式）此前不被 resolved slice 的
  `require_match_pattern` 接纳 → 落入 legacy → match 臂 flow-state 载荷以裸
  i64 绑定 → 字段访问崩溃。本次两处修复：
  ①`require_match_pattern`（eligibility.rs）接纳 **Tuple 子模式**（镜像
  `require_binding_pattern`）→ main 进入 resolved slice；
  ②resolved `emit_match` 的 Err 臂新增 **{i64,i64} handle-pair 解码**——legacy
  fails-transition 的 `Result<T, (Source, E)>` 把 Err 载荷编码为堆上
  `{i64,i64}` 句柄对（compile_try_rejected 约定），而非内联元组结构体；此前
  resolved 侧按内联元组 load 误读两字段（SIGSEGV/垃圾指针），现逐元素
  inttoptr+load（struct/string 载荷；int 元素截断）。real_world 套件恢复
  31/31；新回归测试 dual_flow_fails_err_tuple_matching_native（VM + checked
  双 harness 印 TXN-42/TRK-001/book/invalid price/0）。

### 0.36.36 — 元素级消费候选 (1) 落地：match/if-let 容器义务消解（Option/Result，L2）

- **设计**：§4g 两候选取 (1)"总元素消费满足容器义务"的首个切片——**穷举解构
  消解容器义务**：match/if-let 对线性聚合容器（Option/Result）的分支要么绑定
  载荷（其自身资源链继续）、要么无载荷——容器整体义务在解构点消解。载荷绑定
  独立链不受影响（exactly-once 保持）；受准面从 split + 记录/元组解构扩展
  到 match/if-let 解构。
- **实现（src/core/cfg/resource_lower.rs）**：visit_expr 的 Match 臂与
  visit_stmt 的 IfLet 臂在解构点对单一线性容器源发射 Drop（容器 id 消解）。
  守卫（fail-closed）：
  ①单一线性源（capability_places.len()==1 且该 place 恰一个资源 id）——
  多源/投影歧义不变；
  ②仅 Option/Result 聚合（List/Map/Set 保持阻塞，for 迭代面另立）；
  ③**线性槽位 wildcard 搁浅检测**（pattern_strands_linear）：`Some(_)` 覆盖
  线性载荷 → 拒（E0256 保持）；`Err(_)` 覆盖非线性 string 载荷 → 放行
  （线性/非线性槽位区分，避免误伤）。
  IfLet 的 Drop 键在 initializer 节点（resolved_lower 把 initializer hoist
  进 CFG pre-header，语句节点无 CFG 点）。
- **方块对照**（§4g）：match Option<cap>（Some 绑定 + None 空臂）E0256→✓；
  match Result<cap,string>（Ok 绑定 + Err(_) 非线性通配）E0256→✓；if-let
  Option<cap> E0256→✓；wildcard 搁浅负例保持拒绝。
- **回归**：dual_linear_option_match_consumes_container（三 harness 印 42）、
  dual_linear_result_match_nonlinear_wildcard_ok、dual_linear_iflet_option_
  consumes_container、dual_linear_match_wildcard_strand_still_rejected
  （两种搁浅形态 E0256 钉）。全量 5369 passed / 0 failed / 7 ignored；
  fmt + clippy(-D warnings) + docs(31/31) + edge(6/6) 全绿。

### 0.36.35 — flow-state-in-container 原生 ABI 统一（Phase C 首项前置交付，L1）

- **背景**：`Result<FlowState, E>` / `Option<FlowState>`（flow-state 装入
  容器槽位）此前无任何后端能编译：legacy 发射器自身双表示分裂——容器载荷槽
  boxed-ptr（结构体装箱后 ptrtoint 进 i64 槽）vs 直接构造的状态值被拍平成
  i64——match 两臂无法统一（E0200 "cannot unify ptr with i64"）；resolved
  片 eligibility 门拒绝 `state:Flow::State` 名义 → 全函数落 legacy → 同一
  E0200。VM 正常（印 2）——三路不一致，0.36.20 项登记的 Phase C 差距。
- **修复**：
  ①resolved 片 eligibility 门接受 `state:` 名义（镜像 0.36.32 的 SessionChan
  豁免——不透明 i64/记录结构布局由发射器侧提供，无需声明目录）；
  ②**nominal 解析钩子**：`llvm_type_for_resolved_with`（types.rs）新增
  `&mut dyn FnMut(&ResolvedTypeId) -> Option<BasicTypeEnum>` 参数，Nominal 臂
  先咨询钩子；递归（Result/Option 槽位下钻、Tuple/List 元素）全程透传——
  容器嵌套载荷与原位值共享同一布局。发射器 lower_type 注册 state:
  钩子（读 legacy type_defs 的 "flow::Flow::State" 记录 → 结构体），修掉
  双表示分裂（boxed-ptr vs flatten-i64 归一律到同一记录结构体）。
- **端到端**：Result<Zero,string> match（Ok 提取 vs Err 回退构造）与
  Option<Zero> match（Some 提取 vs None 回退构造）在 VM/resolved 双后端
  一致印 2/6；legacy 纯路径仍拒绝该形态（E0200 或后续 transition overload
  错误）——已登记 legacy 遗留差距，生产（per-function dispatch）因该形态
  eligible 永不落入。
- **回归**：dual_flow_state_in_container_native（原
  dual_flow_state_in_container_native_gap 摘除 #[ignore] 转正：VM +
  checked_codegen 印 2 + legacy reject 边界钉）+ 新增
  dual_flow_state_in_option_container_native（Option 形态对称三断言）。
  ignored 计数 8→7（首个 0.36.36 窗口项提前交付摘牌）。全量 5365 passed /
  0 failed / 7 ignored；fmt + clippy(-D warnings) + docs(31/31) + edge(6/6)
  全绿。

### 0.36.32-34 — SessionChan 类型化端点构造面落地：session_open::<S>()（L1+L2）

- **背景**：0.36.23 曾判定 Session 构造面"死面"——`session_pair()` 只给
  `List<i64>` raw 句柄，协议违序退化为运行时死锁（VM timeout，checker 零诊断）。
  0.36.29 §4f 证据链（E0414/E0425/E0426 在 typed 参数/注解全活）已将论断刷新为
  "residual 引擎全活、构造面半实现"；本组封闭最后一公里。
- **checker 侧（src/core/infer/call/method.rs、src/core/ir/lower.rs）**：
  ①infer_turbofish 增加 `session_open::<S>()` 特例（此前 turbofish 只查用户
  函数表 → 误报 E0401）：单实参 + `Type::Name(...)` + session_types 收录（未知
  session 名 E0413）+ 空参校验 E0242 → 结果类型 `SessionChan<S>`；
  ②resolved lower 的 builtin 泛型实参闸新增 session_open 豁免（镜像 from_json）：
  校验 check-finalized 结果 `SessionChan<S>` 与显式实参 S 一致（不一致给
  内部不变量错误），`session_open()` 无 turbofish 形态同样合法。
- **codegen 侧**：`session_open` 从"Unsupported：does not yet lower to a typed
  SessionChan endpoint"改为**单端点 i64 句柄**（compile_session_open_endpoint，
  取 pair 的 lo 端；不再误复用 session_pair 的 List 返回——后者会把两个端点
  塞进一个 SessionChan 值）；codegen turbofish 路径（compile_turbofish_expr）
  增加 session_open 委派（与 from_json 特例并列，此前走 find_func_def →
  E0700 "definition not available for monomorphization"）；resolved 片
  eligibility 门接受 `SessionChan<T>` 名义（不透明 i64，镜像 Map/Set），
  llvm_type_for_resolved 的 Nominal 臂补 ends_with("SessionChan") → i64。
- **VM 侧（src/interp/bytecode/builtins/concurrency.rs）**：注册 `session_open`
  builtin，返回 pair 的 lo 端 i64（与 codegen 同构；注意与 session_pair 的
  List 返回语义不同）。
- **端到端实证**：`let ch: SessionChan<Hello> = session_open::<Hello>()` 后
  send/recv/close 全链 check ✓ + VM + native 双双输出一致；E0414（recv 先于
  send/close 序违）、E0425（未完成残差出作用域）、E0413（未知 session 名）在
  typed 端点上全静态拒绝——0.36.23"死面"正式刷新为"构造面闭环"。
- **回归**：dual_session_typed_endpoint_open（三 harness：resolved/legacy/VM
  同印）+ dual_session_typed_endpoint_residual_enforced（E0414/E0425/E0413
  三负例）；adv_codegen_rejects_fake_builtin_results 中 session_open 的
  "必须 unsupported"断言随功能落地删除（仅保留 test_sandbox）。
- 全量 5363 passed / 0 failed / 8 ignored；fmt + clippy(-D warnings) +
  docs(31/31) + edge(6/6) 全绿。

### 0.36.26 — M9 门禁再补全：字面量数组索引 + 元组字段访问（fail-open 收口，L2）

- **补全（src/core/cfg/resource_lower.rs）**：M9 门禁的另两个漏网形态——
  ①字面量数组索引 `[a, b][0]`：collect 侧只选索引元素、pairing 平衡放行，
  未提取线性元素静默泄漏（探针 .l9 check ✓）；②元组字段访问 `t.0`：抽取
  线性元组一个原子、兄弟原子泄漏。reject_index_read_extraction 增加
  **Project 臂**（Index/Tuple 投影 × 容器类型为线性且不可弃 → E0304，单元素
  字面量提取 = 整体消费放行）。首个实现用元素 place 探测判据误判（cap 常数为
  Constant 非 place），改以**容器类型**为判据。
- **边界守恒**（探针矩阵）：单元素 `[c][0]` ✓、整体容器移动（sink/make-drop）
  ✓、非线性索引/元组/切片 ✓ 均不受影响。
- **回归扩展**：dual_linear_container_index_read_rejected 增加字面量数组 +
  元组两种拒绝形态 + 单元素/非线性两个正例控制。全量 5358 passed / 0 failed /
  8 ignored（含 0.36.24 gap 登记）；fmt + clippy + docs + edge 全绿。
- 元素消费缺口（match/for/index/slice/字面量索引/元组字段）全 family fail-closed。

### 0.36.31 — 元组别名解构修复：TOOL-RESOLUTION-001 → 合法受准解构（L1）

- **修复（src/core/ir/lower.rs）**：`type Pair = (cap FileReadCap, i32)` 后
  `let (c, n) = pr` 直接解构命名元组**此前在 resolved 层以内部不变量
  TOOL-RESOLUTION-001 拒绝**（"tuple pattern shape disagrees with canonical
  scrutinee type"——scrutinee 规范型是 Nominal 别名而非裸元组；0.35.19
  CO-H2 纪律的反例）。修复：Tuple 模式形状检查穿透 Alias/Newtype 规范目标
  （复用 instantiated_type_target，镜像 Array 臂对 List-nominal 的处理）。
- **实证**：m5 check ✓、VM 7、native 7——双后端一致；0.36.30 §4g 受准面
  （记录/元组解构）补双 harness 钉住。
- **回归**：dual_container_destructure_tuple_alias（直解构 + 跨函数 + 非线
  性三形态，三 harness）。全量 5361 passed / 0 failed / 8 ignored；fmt +
  clippy + docs + edge 全绿。

### 0.36.30 — 元素级消费现状矩阵（Phase C 设计输入，文档轮）

- **实测钉住**（phase-c §4g）：for 元素消费（m1，E0304+）、match Some 消费
  （m2，E0256 容器义务未识别）均为**已登记 fail-closed**（无新漏网）；受准面
  = cap split（0.36.20）+ **记录/元组解构**（m4：`let (c, n) = unpack(p)` 含
  cap 字段，check ✓）——解构面补双后端 harness 断言排 0.36.31。
- **设计输入**：Phase C 单态化窗口"总元素消费满足容器义务"两候选（按元素记账
  vs 现状文档化受准面）登记；E0256 消息面扩改待窗口。
- 探针侧记：m3 的 fail() 非发散（None 臂类型不合）——非语言缺陷。

### 0.36.29 — guard 统一机制定案（方案 B）+ SessionChan 死面证据链闭合（文档轮）

- **guard 定案（phase-d §3）**：保留双关键字 `defer { }` / `on failure { }`
  （方案 B）——机制已单轨（语句位置登记 → 出口发射），双表面双 harness 三
  后端矩阵全绿（L1 挣绿），零迁移量；`defer on failure { }` 语法收敛纯表面、
  不改变语义，留 Phase D 窗口；C（事件化）维持否决。**作用域守卫支柱
  定案 + 挣绿 ✅**（spec §4.6 无遗留超售）。
- **T? 建议节更新**：§2b 由"正式窗口执行"改为"✅ 0.36.27 已落地"（交叉引用
  回归测试 + 0.36.28 复证）。
- **SessionChan 证据链闭合（phase-c §4f）**：跨函数 typed 端点构造 → E0211
  （死面从"推测"升级为"可证伪构造失败"）；raw i64 句柄绕开 residual 类型 →
  协议违序退化为运行时死锁（checker 零诊断，VM/native 一致）——方案 (A)
  类型化端点入 0.36.36 正式窗口的前置证据齐。无代码变更，门禁维持绿。

### 0.36.28 — 诊断次序修复：`?.` 接收者校验不被 callee 形状错误掩埋（L2）

- **修复（src/core/infer/call.rs）**：infer_call_expr 的非函数 callee 分支此前
  **短路发射** E0223（"callee must be a function name"）后即返回，从不递归推断
  callee 表达式——`x?.to_string()`（x 为 i32）只报 E0223，掩埋了 OptionalChain
  接收者校验（E0224 "?. requires Option or Result receiver"）。现在先 `infer_expr
  (callee)` 再报 E0223：根因与形状错误同时显形。
- **边界守恒**：普通非函数 callee（`x(1)`）仍只报 E0223（单错，回归钉住）；
  T? 删除后 `Type::Option` AST 变体留作内部惰性表示（无 surface 构造点，
  后续清理排 Phase D）。
- **回归**：dual_optional_chain_misuse_diagnostics_not_masked（双报 + 单报
  两形态）。全量 5360 passed / 0 failed / 8 ignored；fmt + clippy + docs +
  edge 全绿。

### 0.36.27 — 语法重设计预演：删除 `T?` 后缀别名（`?` 三义 → 两义，破坏性）

- **删除（src/parser/parse_type.rs）**：`Type := PostfixType { '?' }` 产生式
  移除——`T?` ≡ Option<T> 字面别名是全语料零用例的死语义（spec/stdlib/
  docs/tests 全仓仅 1 处 parser 测试锚点，已改写为无后缀复合类型 span
  覆盖）；`?` 三义（try / optional-chain `?.` / nullable 后缀）收敛为两义。
  `?` 出现在类型位置现在给出迁移诊断（"write Option<T> instead"）。
- **回归**：postfix_question_marker_is_removed_alias_with_migration_note
  （i32?/List<i32?> 拒绝 + Option<i32> 照常）；nested_composite 测试改写。
  CLI 实证：`let x: i32? = 5` → 友好解析错误；Option<T> + match 照常（VM 5）。
- **门禁**：全量 5359 passed / 0 failed / 8 ignored（含 0.36.24 gap 登记）；
  fmt + clippy + docs + edge 全绿。try `?`/`?.` 专组均在套件内保持绿。
- 迁移注记：`T?` → `Option<T>`（零存量迁移量）。

### 0.36.25 — M9 门禁补全：线性容器 slice 读取 fail-open → fail-closed（L2）

- **补全（src/core/cfg/resource_lower.rs）**：M9 门禁（0.36.22 索引读取
  E0304）的**姊妹形态**——`v[1..]`（List<cap>）此前**免检放行**：slice 复制
  句柄值（别名化）且只 drop 切片副本，容器自身句柄静默泄漏（探针 .m5 check
  ✓，与 0.36.22 修复前的索引读取同款 fail-open）。现将 slice 表达纳入同一
  门禁（reject_index_read_extraction 增加 Slice 臂 + 共用诊断出口），消息升级
  为 "cannot be read by index or slice"。
- **边界守恒**：整体移动/丢弃（sink(v) ✓）、非线性容器 slice（xs[1..] ✓）
  不受影响；flow-state 元素容器 slice 继续合法（droppable 豁免）。
- **回归扩展**：dual_linear_container_index_read_rejected 增加 slice 拒绝 +
  非线性 slice / 整体移动两个正例控制。全量 5366 passed / 0 failed /
  7 ignored；fmt + clippy + docs + edge 全绿。
- 线性系统元素消费缺口（match/for/index/slice）至此**全部 fail-closed**。

### 0.36.24 — 登记差距：flow-state 进容器（Result/Option）native 面 E0200（IDD 已知差距）

- **探针实证**：`let got = match boxed { Ok(c) => c, Err(_) => Zero{..} }` 后
  `Counter::inc(got)`——checker ✓、VM ✓（2）、native **E0200 响亮拒绝**（"no
  overload for source state got" / "cannot unify PointerType(ptr) with
  IntType(i64)"——Result 槽位 ptr vs 字面量臂平铺 i64）。Option 同形复现
  （.r3）。capability gate fail-closed，无静默 miscompile。
- **登记**：IDD 已知差距测试 `dual_flow_state_in_container_native_gap`
  （#[ignore]，钉住"双后端须印 2"语义契约）→ Phase C（0.36.36+）容器载荷
  表示统一窗口；Phase E "旧 state 复用"负测试排期对齐。
- 门禁全绿；除 gap 测试外无代码变更（探针轮）。

### 0.36.23 — 预研发现：SessionChan<T> 用户层不可构造（Phase C Session 重设计输入）

- **探针实证（phase-c-linearity-study.md §4d）**：`session_pair()` 返回
  `List<i64>` 句柄；类型标注 `let ch0: SessionChan<Echo> = pair[0]` = E0209、
  `session_pair::<Echo>()` = E0401、typed 参数函数 `client(ch: SessionChan<Echo>)`
  无法被用户 main 调用——pair[0] 为 i64，无任何构造途径。**typed 残差函数为
  死面**：checker 对其强制 residual 顺序（E0414 实证），但协议通道以裸句柄流通，
  证明无法跨函数边界贯穿。
- **路线图映射**：= Phase C Session 项"消除 checker 权威 / codegen best-effort
  分层割裂"的用户可见实例，入 0.36.36+ 正式窗口设计选项（(A) session_pair 返回
  类型化端点——倾向，residual 证明跨边界贯穿；(B) 删除 typed 参数面——死面
  消除）。
- 门禁全绿；无代码变更（预研探针轮）。

### 0.36.22 — M9 修复：线性容器索引读取 fail-open → fail-closed（L2 挣绿）

- **修复（src/core/cfg/resource_lower.rs）**：`ActionEmitter::visit_expr` 顶部
  `reject_index_read_extraction`——Load(place) 带 Index 投影、本地类型线性且
  **不可自动弃**（Cap/SessionChan 元素）→ E0304 专用诊断
  （"element-level extraction from a linear container is not tracked and
  leaks every unextracted element"）。此前索引读取把容器整体记为已消费，但只
  release 提取句柄，未提取元素静默泄漏（fail-open，与 match/for 的
  fail-closed 不一致——0.36.18 M9 发现）。
- **边界守恒**：flow-state 元素容器（0.31.16 P0-5 自动弃）的索引读取 = 既定
  合法模式继续放行（`mutate_field_writeback_clause6_dual_backend` 保持绿）；
  非线容器索引、整体 drop 不受影响。探针 m1（绑定）/m2（调用实参）/m3（整体
  drop ✓）/m4（非线性 ✓）。
- **回归（L2）**：`dual_linear_container_index_read_rejected`——bind + 调用
  实参双形态断言 E0304 + 正例控制（drop(容器) 合法、非线性索引合法）。
- 全量 5365 passed / 0 failed / 7 ignored（初次全量出现 audit_h13 fd 复用
  flake，单测复跑 3 passed 确认与本次变更无关）；fmt + clippy + docs + edge
  全绿。Phase C M9 缺口闭合，线性系统支柱推进一格。

### 0.36.21 — Phase B 正式窗口：Protocol 定案（(a) checker-only 确认 + dyn=稳定逃生舱）+ spec §3.9/§6.5 定稿

- **Protocol 定案落字（docs/language-spec.md）**：
  - §3.9 稳定承诺收敛为 `StaticProtocolProjection`（checker 拓扑 + 稳定身份 +
    版本握手）；**删除 "statically generated language interfaces" 承诺**（Flow
    已生成唯一接口面，Protocol 投影零生成面——0.36.16 死存储证据）；
  - **`dyn Protocol` = 稳定逃生舱**（与 unsafe 同级显式边界，双后端回归覆盖）
    ——撤销 "must be independently feature-gated" 表述（Mimi 无独立
    feature-flag 机制）；
  - runtime VTable/异质集合/动态广播 = 未实现 + `experimental`，capability
    gate 稳定诊断报告（§9.5）。
  - §6.5 Protocol Convergence 的 dyn 条目同步定稿。
- **定案文档**：phase-b-protocol-verdict-draft.md 草案 → 定案（方案 (a)
  checker-only 正式确认；挣绿清单 = conformance 正/负例 + dyn 双后端 +
  E0425-27 全部复核 ✓）。
- Phase B 两问闭合：Actor mut（SD-5，0.36.13/14）+ Protocol（0.36.21）。
  挣绿面复核：conformance/dyn 双后端 + Session residual 三形态（0.36.19）。
- 门禁全绿（fmt + clippy + docs + edge）；spec 变更触及 §3.9/§6.5，docs 门禁
  复验通过。

### 0.36.20 — cap split/借用线性边界复核（全 fail-closed，挣绿确认）+ 0.36.21 窗口前装填

- **split 边界矩阵（探针实测，phase-c-linearity-study.md §4c）**：只 drop 一个
  原子 = E0256；整体 drop 组合 cap = ✓ 双后端；split 后用原 c = E0304；
  通配符丢弃原子 = E0304 + §1.3 红线专用诊断（无静默泄漏）；**view/mutate 借入
  split 原子 = 转移语义**（借入即消费，返回后再 drop = E0304）——cap 无借出
  (loan) 面，保守 fail-closed，无双后端分歧。
- **定案输入**：cap 参数一律转移；Phase C 选项 = 保持现状（倾向）vs 引入
  view 借出（与 flow state 对齐）——登记 1.x 评估。
- 0.36.21 正式窗口前装填：Protocol 定案草案（0.36.16）已就绪——
  §6.5 feature-gated 表述 + §3.10 接口承诺处置 = 窗口首项。
- 门禁全绿（fmt + docs + edge）；无代码变更（预研探针轮）。

### 0.36.19 — Session 复杂 residual 双后端挣绿（roundtrip/分支合并/循环）+ 覆盖缺口闭合

- **Session 双后端正例补齐（dual_backend +3，双 harness）**：此前双端 session
  面仅 E0432 L2 拒绝 + real_world flow_session.mimi；0.36.19 起三种复杂形态经
  legacy compile_and_run 与 checked_codegen_compile_and_run 双路径对 VM 断言
  （与 0.36.15 guard 修复同一路径盲区纪律）：
  `dual_session_residual_roundtrip`（42/84）、
  `dual_session_residual_branch_merge`（分支 residual merge → 1）、
  `dual_session_residual_loop_ops`（同端点循环 send/recv → 3）。
  探针实证全部三后端逐字节一致；矩阵入
  `devdocs/v0.36/phase-c-linearity-study.md` §4b。
- 全量 5357 passed / 0 failed / 7 ignored；fmt + clippy + docs + edge 全绿。

### 0.36.18 — 线性系统支柱预研启动：泛型×线性强制矩阵（M1-M10 探针复核）+ 索引读取 fail-open 缺口

- **现状强制矩阵（探针全实测，devdocs/v0.36/phase-c-linearity-study.md）**：
  E0432 ×4（裸 cap / turbofish / SessionChan / List<cap> 泛型边界 H2）、E0256
  （concrete List<cap> 未消费参数）、drop(容器) 满足义务双后端 ✓、for 迭代 /
  match 解包 = fail-closed（E0304/E0256）——与 AGENTS 声明一致。
- **新发现缺口（M9，fail-open）**：`let c = v[0]`（List<cap> 参数）**免检放行**
  ——索引读取把容器整体记为已消费（E0256 不触发），但未提取元素静默泄漏；
  match/for 同族缺口却 fail-closed → **不一致**。双 drop（l8）仍被 E0304 拦
  （无双释放）。修复配方登记 Phase C 队列：dataflow 对线性元素容器的 Index
  投影读取直接 E0304 拒绝（风险低：全库无合法用例）。
- **泛型×线性定案输入**：维持 E0432 禁令（现状确认）+ 单态化评估登记 1.x；
  元素级消费语义列入 1.x 评估。
- **Session lowering**：0.36.14 复核保持（基础双端 10/11 一致 + E0425-27 强制）。
- 无代码变更（预研探针轮）；门禁全绿（fmt + clippy + docs + edge）。

### 0.36.17 — `?` 三义复核：T? 别名零用例（Phase D 强输入）+ guard 失败出口矩阵实证

- **`?` 三义分离复核（探针实证）**：try `?`（39 处 dual 用例）+ optional chain
  `?.`（audit_round2 专组 + dual_backend 9 处）均活且双后端覆盖；**nullable 类型
  后缀 `T?` = `Option<T>` 字面别名**——`let x: i32? = 5` 被 checker 报为
  "pattern declared as Option<i32>"，且**全 src/tests 无 `T?` 类型位置用例**
  （零语料）；无 null 字面量（Option 空为 None）。建议（Phase D 输入）：删 `T?`
  别名产生式，`?` 三义收为双义——删除量零（无用例），直接收敛路线图"消除
  `?` 三义"目标。E0429（`?` 前线性消费拒绝）复核 = 已强制 + 测试 ✓。
- **guard 失败出口矩阵（三后端一致，实证）**：正常出口/早退 = defer 跑、补偿
  不跑；陷阱（E0803/E0801 abort）= guards 均不跑（进程终止）；吸收
  （panic→`Fault` 结果）= 作用域继续、补偿不跑（失败被包含，未出作用域——
  语义自洽）；`exit(N)` = 均不跑。矩阵入
  `devdocs/v0.36/phase-d-syntax-inventory.md` §3b。
- **guard 统一机制设计草案**：双表面已共单轨登记/发射管线（0.36.15 对齐）；
  方案 A 单关键字限定符 vs **B 保留双关键字（倾向，零迁移、双 harness 钉住）**
  vs C 事件化（否决）；语法层收敛留 Phase D 窗口，若选 A 则机械迁移 + 双后端
  挣绿为本 phase 挣绿项。
- 无代码变更（预研文档轮）；门禁全绿（fmt + clippy + docs + edge）。

### 0.36.16 — Phase B Protocol 定案草案起草（0.36.21+ 前置评审）+ 语法参考 guard 注记同步

- **Protocol 定案草案（devdocs/v0.36/phase-b-protocol-verdict-draft.md）**：按
  路线图裁决标准（更少双状态语义）对照两方案——(a) checker-only 静态投影
  （现状确认，零运行时语义）vs (b) 接入消费端（interp 再造校验 = 双套判定可
  分歧）。倾向 (a)；dyn Protocol 建议定案为稳定逃生舱（与 unsafe 同级，撤
  spec "feature-gated" 未实现承诺）；§3.10 "statically generated language
  interfaces" 承诺建议删（flow 已生成 StateId/EventId，protocol 无需第二套）。
- **E2 强化复核（0.36.16）**：codegen 侧 protocol 拓扑表（transitions/states/
  payloads，compile.rs:101-130 装入 codegen/mod.rs:428-431 字段）**无任何
  消费者**——死存储，进一步坐实 "checker-only、零消费" 定位。
- **语法参考注记（docs/syntax-reference.md）**：`defer { }`（任意出口 LIFO，
  无 `defer failure` 表面）与 `on failure { }`（失败出口补偿，语句执行点登记）
  加语义注记，与 spec §4.6（0.36.15 修正）对齐；docs 门禁复验绿。
- 无代码变更（预研文档轮）；门禁全绿（docs + edge）。

### 0.36.15 — 语法重设计预研：scope-guard resolved 发射器 L1 修复（defer/on failure 双 harness 钉住）+ spec 表面修正

- **L1 修复（生产路径）**：CLI `mimi build`（compile_checked，resolved 发射器）
  把 `defer`/`on failure` 体当内联块在语句位置就地编译——`defer` 先于正文执行、
  `on failure` 正常返回也触发；双后端测试走 legacy compile_file（0.31.24 起
  register_model 正确）→ 套件全绿掩盖生产路径 miscompile（路径盲区）。
  src/codegen/resolved/mod.rs：`ResolvedScopeKind::Defer`/`FailureGuard` 不再
  内联——语句位置登记、函数出口 LIFO 发射（Return 臂 + 尾部回落发射 defer 并
  丢弃补偿；`exit(...)` 前发射补偿镜像 legacy hook；每函数入口清栈）。
- **探针实证（修复后 native == VM）**：`BODY/DEFER`、LIFO `body/third/second/
  first`、早退 `work/cleanup/42`、`exit(1)` 不触发补偿（三后端一致，探针 + 测试）。
- **spec 表面修正（docs/language-spec.md §4.6）**：删除不存在的 `defer failure`
  表面（parser/语法参考中无此形态），改为真实双表面 `defer { }`（任意出口 LIFO）
  + `on failure { }`（失败出口补偿，语句执行点登记）；保留作用域守卫收敛意图 +
  Phase D 统一裁决注记。
- **语法面清单（devdocs/v0.36/phase-d-syntax-inventory.md）**：关键字清理 5/6
  已完成（c_shared/c_borrow/local_shared/weak_local/raw_string 已非关键字，
  lexer 测试断言实证），仅 `parasteps` 仍占硬关键字；`?` 三义产生式已分离、
  用户感知歧义留 Phase D；保留字 67 词（≤80 冻结目标达成）；软关键字僵尸审计
  目标清单登记。
- **测试（dual_backend +3，双 harness）**：
  `dual_guard_resolved_defer_order` / `dual_guard_resolved_defer_lifo_and_comp_discard` /
  `dual_guard_resolved_defer_early_return`——同一程序经 legacy compile_and_run 与
  checked_codegen_compile_and_run 双路径对 VM 断言（消灭路径盲区）。
  全量 5354 passed / 0 failed / 7 ignored；fmt + clippy + docs + edge 全绿。

### 0.36.14 — Phase B 交叉复核：Session × Protocol 状态语义（无第二状态模型）+ mut lint 设计存档

- **交叉复核结论（docs/language-spec.md + 实现证据）**：Session = 线性能力
  模型（两端通信有序性），**不是**第二业务状态模型——§6.4 "Flow is the sole
  model for business state and change" 无违例；spec 承诺的 typed residual
  诊断码 E0425/E0426/E0304/E0427 在实现中均有 emit 点（spec→code 抽查通过）。
- **codegen residual lowering [experimental] = 保守表述（非矛盾）**：实测
  `tests/real_world/flow_session.mimi` VM=10/11 与 native=10/11 逐字节一致
  ——基础 send/recv/close 原生端完整 lower，real_world 套件双跑本已覆盖；
  "not yet fully lower" 仅指复杂 residual 形态（E0425-27 拒绝路径）。
- **登记项**：§3.10 conceptual `session_pair::<T>()` vs surface 无参
  `session_pair()` 的 minor drift（不动代码）；dyn Protocol gate 缺口继续
  保持登记（§5b）。mut→Flow 迁移 lint 设计已存档（§5d，触发/提示文本/不做
  理由，0.36.21+ 与 Actor mut 定案合并实现）。
- 详见 `devdocs/v0.36/phase-b-state-semantics-study.md` §5c/5d。
  门禁全绿（docs + edge + fmt + clippy），无代码变更（纯预研文档轮）。

### 0.36.13 — Phase B 扫描修债：spec 消除 Actor 状态双表述（§6.4 重写）+ SD-5 L1 证据

- **spec 双表述矛盾消除（Phase B DoD 前置）**：§6.4 旧文 "actor 任意可变业务
  字段/mutating method removed from stable set" + "helper 仅允许无状态计算"
  与 Removed 清单条目直接抵触 SD-5（`mut` = 简单状态逃生舱，保留）与实现。
  重写 §6.4：字段写自由（marker 为声明性并发隔离提示，永不写强制）、
  `runs Flow` 拒绝 `mut` 业务字段（E0402）、业务状态变更归 Flow 过渡；
  Removed 清单删除旧条目 + 0.36.13 修正注记（沿用 v0.34.28 修正注记惯例）。
  check_language_docs 门禁复验绿。
- **实现语义矩阵钉住（探针，0.36.13）**：普通 actor 的 mut / 非 mut 字段均可由
  同步方法写（双后端一致 2/1）；`runs Flow` + `mut` = E0402、+ 非 mut = 合法。
- **dyn Protocol feature-gate 缺口（登记，0.36.21+ 定案）**：spec §6.5 承诺
  experimental 项独立门禁，实现无任何 feature-flag 机制——`unsafe_cast_protocol`
  当前无门禁可用（dual tests 直用）；候选处置 (b) 正式定案其为稳定逃生舱（与
  unsafe FFI 同级）并移除 "feature-gated" 表述。详见
  `devdocs/v0.36/phase-b-state-semantics-study.md` §5b。
- **测试（dual_backend +2）**：
  `dual_actor_field_writable_regardless_of_mut_marker`（L1：无标记字段写双后端
  2\n1 与标记孪生一致）、`dual_actor_runs_flow_non_mut_field_allowed`（L2）。
  全量 5351 passed / 0 failed / 7 ignored；fmt + clippy + docs + edge 全绿。
- **并行 fd 竞态修复（审计发现）**：`audit_h13_close_fd_still_closes_real_fds`
  在进程内借 VM 关闭一个已 `drop` 的 fd——fd 号提前释放后可能被并行线程的新
  打开复用，本测试的进程内 close 把另一个测试的 owned fd 关掉 →
  整套件间歇 `IO Safety violation: owned file descriptor already closed`
  SIGABRT（--test-threads=4 约 1/3 概率，threads=1 永不触发）。改为
  `mem::forget` 让 fd 保持占用直到 VM close（无法复用），4 线程连续 3 次
  全量 0 failed 验证。

### 0.36.12 — Phase B 预研首项：单目标 flow 结果 match 静态分派（L1 修复）+ Protocol/Actor-mut 定位审计

- **L1 修复（状态语义挣绿面首项）**：`let d = F::toggle(...)`（单目标
  `-> Active`）后 `match d { Active { reading } => .. }` —— VM 正常取臂，native
  报 `E0713: match arm variant lookup ... not found`（单目标结果是普通记录，
  无 __MultiTarget 枚举可查 ordinal）。修复：`flow_result_static_state` 判定
  （`owner_enum_of_scrutinee` 为 None 且类型为 `Result<S,...>`、S ∈ flow 状态）
  后静态分派——静态态臂无条件取臂并按 `flow::{F}::{S}` 记录布局直接绑定字段，
  非静态态臂为死代码（分派块必须写终结符，否则 LLVM LowerExpect 对空块段
  错误——gdb 定位）；**不劫持 union**（`-> S | Fault` 有注册枚举，ordinal 路径
  不变，flow_panic_absorption_* 回归全绿）。
- **语义基线钉住（checker 为准）**：match 臂按 flow 状态命名空间解析；静态
  结果态臂必须存在（缺失 = E0215）；其它状态臂合法但静态不可达。
- **Protocol 定位审计（Phase B 预研）**：静态投影 = checker-only（E0406/E0404/
  E0402 一致性强制，resolved 目录承载，interp/codegen **零运行时消费**——已有
  形态即"正式降级"）；`dyn Protocol` experimental + `unsafe_cast_protocol` 逃生
  舱（dual_backend 测试已存在）。**Actor mut 审计**：SD-5 现状 = 选项 (a)（简单
  状态逃生舱，双后端写自由，marker 仅提示）；`runs Flow` 拒绝 mut 字段
  （E0402）；lint 缺口登记（mut→Flow 迁移提示，0.36.21+）。预研文档：
  `devdocs/v0.36/phase-b-state-semantics-study.md`。
- **测试（flow_features +5，L1/L2）**：
  `flow_result_single_target_match_dual_backend`（多臂+单臂双后端 3/7）、
  `flow_result_single_target_match_missing_static_arm_rejected`（E0215）、
  `flow_protocol_conformance_positive_dual_backend`（3 双后端）、
  `flow_protocol_conformance_missing_state_rejected`（E0404）、
  `actor_runs_flow_mut_business_field_rejected`（E0402）。
  全量 **5349 passed / 0 failed / 7 ignored**；fmt + clippy -D warnings +
  check_language_docs + check_edge_isolation 全绿。

### 0.36.11 — Phase A 挣绿面收官核验（DoD 1–7 证据全部落地）

- **逃逸面负测试（DoD #2 缺口补上）**：`flow_fault_nominal_escape_face_rejected`
  —— 对名义 `StateId`/`EventId` 做字符串比较（`last_state == "..."` /
  `unexpected_event == "..."`）一律 **E0202** 拒绝（正视 + 括号/拼接两种形态），
  字符串编码的失败归属逃逸面保持零（`check_language_docs.py` 语义新鲜度门禁
  继续钉住该反模式）；孪生 oracle：官方消费路径（对 StateId/EventId 变体的穷尽
  `match` 判别）双后端逐字节一致。
- **DoD 审计核验**（Phase A 收官的逐条证据台账，见 verdict §3 DoD 1–7）：DoD #1
  名义 payload（吸收 + 显式路径双后端 oracle）、#2 字符串逃逸面全仓归零（grep +
  门禁）、#3 recover 穷尽 match 缺失臂拒绝（0.36.5）、#4 错误 trace 双后端
  oracle（0.36.7）、#5 二次 Fault 升级（0.36.6）、#6 吸收声明门（0.36.9）、
  #7 union 直调闭环（0.36.10）。至此 Phase A DoD 全绿。

### 0.36.10 — recover/reset 直调声明可错结果变量（裁决 6 follow-up 闭环）

- **union 结果变量直调 recover/reset（裁决 6 follow-up）**：`let u = Svc::div(a, 0)`
  （`-> S | Fault`）后可直接 `let r = Svc::recover(u)` / `Svc::reset(u)`，不再强制
  中间 `match`。静态类型仍是首目标（`S`），运行时按**实际 tag** 分发：Fault →
  调 Fault 重载（双后端一致）；live tag → 双后端同一错误文本
  `[E0800] no transition {flow}::{verb} from state {state}`（VM 天然按记录名
  分派，native 新增 union tag 判别 + `mimi_trap_no_flow_transition`）。
- **checker + IR 双 widen（L2）**：新增 `faultable_result_vars`（含 2-target
  `-> S | Fault` —— 旧 `multi_target_vars` 排 Fault 且要求 ≥2 用户态，漏掉最
  常见形态）；recover/reset 的实参是该 flow 的 faultable 结果变量时，按
  `flow::F::recover::Fault` 重载放行；resolved-IR lowering 对 recover/reset
  单 (flow, verb) 重载回退 + 自洽 identity 转换（多目标函数恒走 legacy）。
  跨 flow 值（含同名 state）与普通非 union 状态值仍 E0211 拒绝。
- **测试（L1/L2，flow_features +7）**：`flow_union_result_recover_direct_call_dual_backend`
  / `flow_union_result_reset_direct_call_dual_backend` /
  `flow_union_result_recover_three_target_dual_backend`（`-> A | B | Fault` 全局
  ordinal 判别）/ `flow_union_result_alias_recover_dual_backend`（`let x = failed`
  别名保持可恢复）/ `flow_union_result_recover_live_traps_dual_backend`（live
  恢复双后端同文本错误）/ `flow_union_result_recover_cross_flow_rejected` +
  `flow_union_result_recover_plain_state_rejected`（L2 负测试）。

### 0.36.9 — 吸收声明门（裁决 6）+ 非事务草稿语义对齐（L1 分歧关闭）

- **裁决 6（吸收声明门）**：一个运行期 panic 只有在该过渡的**声明目标集**含有
  Fault（`-> S | Fault` 或 `-> Fault`）时才会被吸收为 Fault。单目标
  `-> S` 过渡体内 panic 违反声明契约（结果静态类型是 S，没有 Fault 槽位）——
  双后端一律硬 trap **E0801**。Bytecode VM 此前无条件吸收（`mimi run` 会凭空
  造出 native 二进制永远无法产生的 Fault = L1 分歧），现按声明目标集门控
  （`FlowTxCtx.transition_name` + `flow_defs` 声明表查询），与 native 对齐。
- **非事务草稿语义对齐（L1）**：删除 VM 侧 "dirty 持久字段→归零使 recover 退化为
  reset" 的旧路径——那是 @transactional（v0.34.1 已废除）时代的回滚残留；草稿
  （body 对 `self` 的就地变更）即真相，吸收进 Fault 的 shadow 原样保留，recover
  拉取 faulting draft（`self.value = 99` → 吸收 → recover → 99），双后端逐字节
  一致（此前 VM=0 / native=99 分歧）。
- **"动态 Fault typing"缺口关闭（bytecode-only 测试全部双后端化）**：
  `flow_reset_rebuilds_root` / `flow_reset_discards_persistent` /
  `flow_recover_preserves_persistent` / `flow_fault_recover_uses_faulting_persistent_draft`
  / `flow_user_reset_not_overridden` / `flow_explicit_reset_overrides_system_verb` /
  `flow_explicit_recover_overrides_system_verb` 从"单目标吸收（动态类型、仅
  bytecode）"重锚为**声明式 `-> Fault` 入场**（静态 Fault 类型 → reset/recover
  直调）+ `compile_and_run` 双后端断言；删除死于旧前提的
  `flow_panic_absorbed_to_fault`。
- **新负测试 + 吸收孪生 oracle**（L1/L2）：`flow_single_target_panic_traps_not_absorbs_dual_backend`
  —— 单目标 `-> Pos` panic 双后端 E0801 硬 trap；同程序 `-> Pos | Fault` 孪生
  双后端吸收（`Pos()\nPanic(E0801)` 逐字节一致）。

### 0.36.8 — 跨 FFI = Fault 检查点（裁决 3 收官）+ Phase A 挣绿核验

- **FFI 检查点全量 nominal oracle**（L1, DoD #4 扩界）：`enter_ffi → ffi_crash`
  把 flow 落到 nominal Fault（`last_state = FFI_Pinned()`、
  `unexpected_event = ffi_crash()`），完整负载（snapshot +
  SystemTrace/MemoryDump/PanicPayload 深层字段）在 `mimi run` vs `mimi build`
  （checked + legacy 双管线）逐字节一致。FFI 崩溃是 Fault，不是 Result——
  预期 FFI 失败经 `fails E` 走非 pinned 过渡。
- **FFI 检查点不重入不循环**（L2）：enter_ffi/exit_ffi/ffi_crash 是状态作用域
  sink——从非规范 from-state 调用（含对 Fault 值、跨 flow 值）一律 E0211 拒绝
  （flow-qualified transition keys 隔离 flow）。
- **Phase A 挣绿收官核验**（DoD #1–5）：
  ① Fault payload 零字符串编码状态（`check_fault_nominal_gate` PASSING）+ ②
  `last_state == "..."` 全仓归零；③ recover 缺臂 E0215（match 非穷尽）；
  ④ 错误 trace 双后端等价 oracle（0.36.7 全量 trace + 0.36.8 FFI 检查点）；
  ⑤ 二次 Fault 升级负测试（E0801/E0440）。
- **验证**：flow_features 244 / dual_ 884 / real_world 31 / 全量 lib 全绿；
  fmt + clippy + 语言文档 + 边缘隔离门禁绿。

### 0.36.7 — Fault≠Result 语义边界（裁决 3）+ 错误 trace 双后端等价 oracle（DoD #4）

- **E0441：Fault 禁作函数返回值**（裁决 3, L2）：`Fault` 是状态不是值——只能经
  recover/reset 显式离开；预期失败走 `Result<T, E>` 值传播。checker 对 `func`
  声明返回 Fault sink（裸 `Fault`）即 E0441（fail-closed）；`flow::<name>::Fault`
  qualified 拼写被 parser 直接拒绝（`::` 类型名不可写），双面封死。
- **Result 不是状态机 sink**（裁决 3, L2）：`transition x(A) -> Result<...>` 在
  parse 层即拒绝（fail-closed）；`fails E` 是预期失败的唯一载体。
- **DoD #4 错误 trace 双后端等价 oracle**（L1）：新增
  `fault_trace_full_payload_dual_backend_oracle`——完整 Fault payload（双 flow 的
  nominal `last_state`/`unexpected_event` 判别 + SystemTrace/MemoryDump/
  PanicPayload 全部深层字段）在 `mimi run` vs `mimi build` 逐字节一致，且**两条
  native 管线**（checked/compile_checked + legacy/compile_file）都与 bytecode 等价。
- **resolved native slice 扩展**：`builtin:type:SystemTrace/MemoryDump/PanicPayload`
  布局入 `types.rs`（与 legacy compile.rs 布局逐字段一致，保 per-function dispatch
  ABI 兼容）+ eligibility 接受 + `lookup_field_index` 内建 trace 字段索引
  （checker 内建记录字段 id 为裸 `field:<name>`，无 catalog 条目）——深层
  `.trace.*` 投影不再把 main 踢回 legacy。
- **legacy emitter 跨 flow Fault 补齐**（latent L1 修复）：legacy 路径
  （`compile_file`/`compile_and_run`）对 flow-call 赋值变量（`let sf =
  Scheduler::peer_fault(...)`）原以裸 `Fault` 登记 → `infer_object_type(sf.last_state)`
  解析到首个 flow 的 StateId/EventId → 多 flow 程序 native 打印错误枚举
  （elevator `open_door()`/oracle `Ready()`）。修复：func.rs 登记
  `flow::<name>::Fault`（`transition_result_var_type`）+ 过渡匹配/调用点
  `bare_flow_state_name` 剥 `flow::` 前缀（func.rs 两分支 + expr/call/method.rs
  `compile_flow_transition_call`）。
- **系统动词 ordinal 按 flow 作用域化**（latent L1，非确定性修复）：
  `nominal_variant_enum` 原在全部 type_defs 上无序首匹配——注入动词
  （peer_fault/recover/reset/Panic/ffi_crash）是**每个** flow 的 StateId/EventId
  共有变体，且各 pass 边注册边迭代（HashMap 顺序随 mutation 漂移）→ 同一程序
  两次编译可能取到不同 flow 的枚举 → fault record 的 last_state/unexpected_event
  被打上错误 flow 的 ordinal → native 枚举 Display 打印错误名称
  （elevator dual-backend 套件 open_door() vs peer_fault() 时好时坏）。修复：
  枚举解析先按 `current_flow_name` 作用域查（同 `flow_state_llvm_type` 0.34.36
  纪律），miss 才回退全表。新增 `fault_nominal_verb_ordinal_scoped_multi_flow_dual_backend`
  每次编译连跑 3 轮断言三管线一致且稳定（防回归）。
- **验证**：flow_features 242 / dual_ 883（+1 oracle）/ real_world 31 全绿；
  语言文档 + 边缘隔离门禁绿。

### 0.36.6 — 二次 Fault 升级（裁决 4）+ 跨 flow Fault 调用点名义化补全（裁决 1）

- **E0440：Fault 不是合法转移 source**（裁决 4, DoD #5）：checker 拒绝用户声明的
  `transition x(Fault)`（recover/reset 系统动词与 fallback 豁免）——`Fault` 只能经
  recover/reset 离开，任何其他事件在 Fault 上都会静默 `Fault → Fault` 自循环；
  fail-closed（E0440，描述含迁移注记）。
- **二次 Fault 升级负测试**（裁决 4, DoD #5）：`fault_recover_body_trap_escalates_not_loops`
  ——recover 体内再 trap（`1/0`）→ **升级 + trap（E0801）而非静默循环回 Fault**；**双后端**（bytecode + native）均断言 fail-closed，native 侧硬 trap 不
  回环（旧 `flow_panic_from_fault_does_not_rewrap` 仅覆盖 interp，现补 L1 parity）。
- **跨 flow Fault 调用点名义化补全**（裁决 1 剩余集成点，L1 修复）：`real_world`
  双后端套件 3 程序（flow_elevator/flow_sensor_network/flow_system_trace）native
  打印错误枚举（如 `open_door()` 而非 `peer_fault()`、`HumIdle()` 两次）——根因是
  flow transition 调用的**返回值类型用 unqualified to-state 名**（`Fault`），main()
  中字段访问经 `self.types["Fault"]` 解析到**最后注册 flow** 的 Fault record，
  StateId/EventId 字段类型随错 flow。修复三件套：
  1. checker 调用点类型：单目标 `-> Fault` 的签名返回值 → `flow::<name>::Fault`
     （checker `self.types` 已有该 qualified record，字段访问即按 flow 解析）；
  2. 调用参数归一：method.rs flow-call 路径（overload key + arg unify +
     runs_flow_transition unify）对 `flow::` 前缀名义名剥前缀后比对（`::` 不可能
     出现在用户标识符，前缀必为生成物）——qualified 结果可回传 recover/reset 等
     裸名参数；
  3. resolved IR 目录别名：flow state 额外注册 `flow::<id>::<state>` 键
     （**不带短名别名**，避免裸名跨 flow 变歧义），qualified 调用结果可 intern。
- **验证**：`real_world_flow_dual_backend_suite` 24/24 全绿（3 程序修复）；
  flow_features 238 / dual_ 882 全绿；`check_language_docs.py` 门禁绿。

- **穷尽 match 负测试**（裁决 2, DoD #3）：`fault_recover_exhaustive_match_missing_arm_rejected`
  ——recover 内对 `self.last_state`（StateId）做非穷尽 match（缺 `Fault` 臂）→
  E0215 编译错误锁定；`fault_recover_exhaustive_match_full_arms_ok` 锁定全臂
  match 通过。字符串比较时代的"重命名状态即静默漏判"从此不可能；
- **DoD grep 门禁**（DoD #2）：`scripts/check_language_docs.py` 增
  `check_fault_nominal_gate`——扫 `src/**/*.rs`，`last_state == "…"` /
  `unexpected_event == "…"` 字符串逃逸面归零（0 残留，违规即 lint job 硬错误）；
- **验证**：2 新测试绿 + 门禁脚本绿 + `if last_state == "` 全仓 grep 0 残留。

### 0.36.4 — Fault nominal 实现落地（四层打通，挣绿）

- **核心变更**：`Fault.last_state`/`Fault.unexpected_event` 由 `string` 名义化为
  per-flow `StateId`/`EventId` enum（`snapshot` 保留 string、SystemTrace 子记录
  暂留 string）；`"panic:<code>"` → `EventId::Panic { code }`。`if last_state == "…"`
  全仓归零（DoD 达成，grep 0 残留）；
- **file AST 注入**（flow_matrix）：`expand_items` 在 flow 展开后注入 StateId/
  EventId 顶层 TypeDef（`CompilationRoot` 父提示 + module 路径贯穿消歧）；checker
  `self.types` 不是后端输入（`from_checked_file_base` 从 file AST 派生）；
- **checker scoped 构造**（infer_expr）：`check_expr_inner` 增 `nominal_variant_arity`
  分支——裸 state/event 名在 `expected: flow::X::StateId/EventId` 上下文按枚举
  作用域消歧（Call + 裸 Ident 两形态，镜像 Some/None/Ok/Err）；`collect_item_decls`
  跳过合成 enum 的变体构造器注册（避 E0402 shadow）；
- **跨 flow Fault 重锚**（checker items）：`check_item(Flow)` 开头把 unqualified
  `Fault` 重锚到当前 flow 的 sink（collect_item_decls 先注册全部 flow，unqualified
  指向首个）；T-H8 对 Fault 恒兼容（W0402 非 E0402）；
- **resolved IR 变体 lowering**（core/ir/lower）：`lower_call` 按名义类型识别
  StateId/EventId 变体构造（`lower_variant_constructor_call`）；
- **codegen**：`build_nominal_variant`（`{i32 tag, i64 payload}`，Panic 用全局
  string struct 免 heap leak）；`compile_call_expr`/`compile_ident_expr` legacy
  变体识别；`find_variant_info` 跳过 StateId/EventId（避 `__MultiTarget` 遮蔽 →
  修正常路径静默误编译）；`find_variant_owner_scoped`（match 字段绑定按 scrutinee
  锚定，修多 flow 枚举 Display）；**codegen 预注册全部 TypeDef**（Fault record 的
  enum 字段在 flow 状态注册前已 lower，否则 i64 回退坏 layout）；
- **interp**：`make_fault_value` → `Value::Variant`；
- **测试迁移**：5 个 parse 测试 `items.len 1→3`、用户 Fault 构造 string→enum、
  枚举 Display 期望串、golden IR 重生成（21 文件）、LSP code lens 跳过合成 enum；
- **验证**：5327 lib 全绿（flow_features 235 / codegen_golden 21 / e2e_valgrind /
  lsp_extended / resolved canonical_flow_ids）；dual_ 等价 + `if last_state ==` 归零。

### 0.36.4 — Phase A 实现作战计划 + 变体作用域关键发现

- **作战计划落地**：`devdocs/v0.36/phase-a-implementation-plan.md` 把裁决 1+2 判定为
  原子核心变更（字段类型 + 值构造 + 消费端同步改），锁定改造面 S1–S10（flow_matrix
  ensure_fault_state/system_trace_expr/default_field_value/make_fault_value、
  codegen compile.rs SystemTrace 注册 + expr/fault.rs build_fault_record、checker
  items.rs Fault 校验、resolved SystemTrace 类型、VM absorb_flow_fault、测试消费端）；
- **变体作用域关键发现**：`StateId` 变体名 = state 名，而 `find_variant_info`
  （codegen/mod.rs:3040）是全局 HashMap 迭代序查找——两 flow 同有 state `Active`
  时裸查找歧义。此问题已被 `__MultiTarget` 先例解决（`find_variant_ordinal_scoped`
  + `owner_enum_of_scrutinee` 按 scrutinee 类型锚定 owning enum）；`StateId`/`EventId`
  复用同机制：match 侧按 scrutinee 锚定、构造侧按 `current_flow_name` 锚定，
  enum 类型名 flow-qualified（`flow::<name>::StateId`），变体名保留裸名靠 scoping
  消歧——与 `__MultiTarget` 完全同构；
- **首次实现尝试回执**（已回退保持基线绿）：实证两条架构结论——(1) checker
  生成的类型不传播到 resolved IR（`from_flow_acc` → `from_checked_file_base(file)`
  从 file AST 派生 type_defs，明言"Raw checker types are not a backend input"），
  故 `StateId`/`EventId` 必须在 **flow_matrix 展开 file AST** 时生成为顶层
  TypeDef，而非 checker `self.types`，否则报 `no resolved nominal identity`；
  (2) 裸变体名经 `self.funcs` 全局注册跨 flow 歧义（`expected flow::A::StateId,
  found flow::B::StateId`），构造侧须用 flow-scoped 变体引用；
- **第二次实现尝试回执**（按纠正路线重做，三项验证成功，已回退保持绿）：(1)
  file AST 注入 + `AstParentHint::CompilationRoot` 父提示 → nominal identity 正常
  解析；(2) checker `check_expr_inner` 增 `nominal_variant_arity` scoped 分支 →
  跨 flow 构造消歧（镜像 Some/None/Ok/Err）；(3) codegen `find_variant_info` 跳过
  `*::StateId`/`*::EventId` → 消除 StateId 变体与 `__MultiTarget` 变体在全局查找
  中的遮蔽（否则正常路径**静默误编译**：native 打垃圾 `1078350640` 而非 `5`，L1
  违反）。**剩余两集成点**：resolved IR/codegen 需识别 StateId/EventId 变体构造
  （现报 `undefined function 'peer_fault' in codegen`）+ 4 个 dual_backend 测试
  期望串迁移（`Ready`→`Ready()`、`panic:E0801`→`Panic(E0801)`）；
- **结论**：0.36.4 主体改造面已全量映射（checker + resolved IR + codegen + 测试
  四层），三项已验证的修复可直接复用，下一轮补齐剩余两集成点即可挣绿。

### 0.36.3 — Phase A 锚定：Fault nominal 重设计裁决

- **spec 裁决落地**：`devdocs/v0.36/phase-a-fault-nominal-verdict.md` 锚定 Phase A
  五条裁决——(1) `StateId`/`EventId` 名义化（`last_state: string`/`unexpected_event:
  string` → per-Flow 名义 enum，`snapshot` 保留为人类可读摘要；`"panic:<code>"` →
  `EventId::Panic { code }`）；(2) recover 用穷尽 match 而非字符串比较（缺失分支=
  编译错误）；(3) Fault ≠ Result、跨 FFI = Fault（FFI 契约违反→Fault，FFI 预期错误→
  Result）；(4) 二次 Fault 升级（recover 体内再 fault → escalate + trap，非静默循环）；
  (5) Recover/Reset 物理语义对齐修正案条款 4（in-place reuse）；
- **DoD 定义**：Fault payload 零字符串编码状态 + `if last_state == "..."` 全仓归零 +
  recover 穷尽 match 负测试 + 错误 trace 双后端等价 + 二次 Fault 升级负测试；
- **现状核查**：字符串编码现状分布在 `src/flow_matrix.rs`（`ensure_fault_state`/
  `SystemTrace`）、`src/codegen/expr/fault.rs`、`src/interp/bytecode/` 及
  `src/tests/flow_features.rs`（`last_state == "..."` 反模式实证），为 0.36.4+ 重设计
  划定改造面。

### 0.36.0–2 — Phase 0：理念与治理重锚

- **哲学锚落地**（0.36.0）：`devdocs/v0.36/philosophy-anchor.md` 成为 0.1.6 哲学
  唯一权威——三条铁律（真实资产=设计思想+不变量套件 / "冻结"=锚定非锁定 /
  节奏=激进重设计→锚定→止血挣绿→下一支柱）+ 治理（不变量套件=唯一锚点、break
  纪律、DoD 纪律、边缘解耦纪律、预算纪律）+ 取向/表面边界判读规则；
- **三文档清单挂载**（0.36.0）：`AGENTS.md` §0（战略裁决 + 治理升级）、
  `devdocs/v0.31/architecture-amendment-1.0.md` 序言（"不可逆"语义澄清指向哲学锚）、
  `devdocs/README.md` §1（权威层级顶部挂哲学锚）三处头部统一指向 philosophy-anchor.md；
  白皮书头部"定位澄清"保持指向；AGENTS §13 版本表 0.1.5→✅已发布、0.1.6→⬅当前；
- **边缘解耦清单**（0.36.1）：`devdocs/v0.36/edge-inventory.toml` 注册 6 个边缘项
  （EDGE-01~06：Effect lattice / Protocol dyn / 高级 Session / rich fault / Component
  IR 扩张 / comptime-quote），每项独立 gate 标记（`EDGE-GATE:<marker>`）、解耦后义务
  （只保安全修复，无特性开发）、`core_dep=false` 硬约束；roadmap §4 表加 gate 标记列；
- **门禁机制落地**（0.36.2）：`scripts/check_edge_isolation.py`（4 项检查：manifest
  健全性 / ci.yml 核心门禁路径零边缘 marker 引用 / 标记注册 / 边缘测试必须 `#[ignore]`）
  挂 ci.yml lint job；`scripts/check_language_docs.py` 增 `check_philosophy_anchor()`
  正向引脚（三文档必须指向哲学锚 + 哲学锚含四关键词）；本地实测两新门禁 + 既有
  unsafe/v031 门禁全绿，ci.yml YAML 解析通过；
- **验收**：Phase 0 DoD 达成——文档哲学一致（三文档挂载 + 哲学锚正向引脚门禁）、
  核心门禁零边缘依赖（edge-isolation 门禁入 CI）。

## [0.1.5] — 2026-08-13

> **性能主线 + 审查收口 + 质量次线（Phase A–I）**。
> 0.1.5 里程碑：trap 成本消减（SD-9 链式末端收敛 + cold 权重，dsp O1 3.97×→1.04×
> 追平 C -O2）、resolved 覆盖扩展（fallback 0.3027→0.2735）、O1 正确性切片、深度
> 可用性评估（demos 双后端差分 10+ bug 家族）、外部审查 4C+13H+10M 全闭环、僵尸
> 关键字裁撤（80→67）、VM 性能 R1/R3/R4/R5（RUN dsp 8.9s→4.7s）、双后端统一
> U1/U2/U3/U5、性能门禁入 CI。质量次线：resolve→zonk 迁移（31 处）、parser panic
> 审计、LSP Span/Origin 迁移、trivia 化（desc:/rule:/mms{}）、fmt 评估、错误消息
> CO-H2 精确 span。已知排期外项（native dsp ≤1.15×、RUN dsp ≤1s）登记 0.2/1.x。

### 0.35.47 — R5 VM 循环不变量 LOAD_CONST 提升（LICM-lite）

- **LICM-lite**（`hoist_loop_invariant_loads`，peephole 之后）：`LoadConst` 无
  寄存器操作数，只要其目标寄存器在循环体**唯一定义 + 支配全部使用 + 循环后无
  读**（live-out），即可安全提升到 pre-header——每条迭代省一次 dispatch。
  逐循环迭代到定点（嵌套循环逐层剥离）；后向 `Jmp` 边重映射到提升后的新头；
- **健全性**（L1 dual_ 882 绿锁定）：`while false { x = 0 } x` 这类 live-out
  场景被条件 3（循环后无读）正确拒绝——初版只查"唯一定义"误提升 `x = 0`
  导致返回 42→0，已修并加回归锁；
- **实测 VM dsp（release）**：5.8s → 4.7s（~1.25×，循环 13→10 指令）；R1+R3+
  R4+R5 合计 8.9s → 4.7s（**~1.9×**）。仍非 ≤1s——余量在 156-Op 单 match 分发
  （V2），需 1.x direct-threading/JIT；
- **验证**：5327 lib + 15 main + 31 real_world + 1 cli 全绿；dual_ 882 绿；
  clippy/fmt 全绿。

### 0.35.46 — 收口基建：性能门禁入 CI + AGENTS 模块表补 Semantic IR

- **性能门禁入 CI**（C2 教训闭环）：`scripts/perf-gate.sh` 以 release 构建 +
  **宽松阈值**只拦截灾难性回退（native dsp ≤3× C -O2、VM dsp ≤20s、构建/运行
  不得失败），不拦截共享 runner 噪声；挂入 CI test job 末步。细粒度复测仍用
  本地 `benchmarks/quadrant.sh`；
- **AGENTS 模块表补 `src/core/ir/`**（Semantic IR）：body/callable/types/lower
  职责 + 11.2k LOC 记录（此前模块表缺失该 canonical 语义 IR 目录）；
- **验证**：本地 `perf-gate.sh` PASS（native 1.34×、VM 5.9s）；ci.yml YAML 解析
  通过。

### 0.35.45 — U3 契约派生 arity 断言 + U5 E 码描述字典单点

- **U3 契约派生 arity 强制**：checker 的 builtin 分发前新增**单一通用 arity 检查**，
  直接消费 `core::builtins::builtin_arity` 表——固定 arity 的 builtin 由表驱动拒绝
  surplus/缺失实参（E0242），变参（`usize::MAX`）与特殊臂（`log` 1–2 等）保留各自
  精确规则；**修复 `pi(1)` 等缺口**（原报 TOOL-RESOLUTION-001 混淆错误 → 现
  `pi expects 0 argument(s)`）；`builtin_arity_contract_derived_assertions` 测试
  用表派生的合法/超参调用锁定 checker 强制与表一致；
- **U5 E 码描述字典补全**：`describe()` 覆盖所有已分配 E/W 码（补 W0400/W0401/
  W0402 三个 flow 警告描述），`diagnostic_codes_no_duplicates` 测试追加断言——
  任何已分配码不得回退 "unknown error"（漂移信号）；
- **验证**：5325 lib + 15 main + 31 real_world + 1 cli 全绿；dual_ 882 绿；
  clippy/fmt 全绿。

### 0.35.44 — R4 builtin 快路径 + U1 登记表单点 + U2 trap 常量单点

- **R4 builtin 内联快路径**：`BuiltinRegistry::fast_path` 表 + VM `CallBuiltin`
  分发臂对 `abs`/`min`/`max`/`floor`/`ceil`/`round` 直接读寄存器内联计算，
  跳过 `Vec<Value>` 分配 + 间接函数指针 + 每 builtin 的 Value match；意外
  类型/`abs(i64::MIN)` 溢出回退通用路径（错误文案逐字节一致）；
  **实测 VM abs 密集循环 5.3s → 4.6s（~1.1×）**——未达 ≥1.5× 目标，主因是
  每指令 156-Op 单 match 分发（V2）仍占主导，builtin 开销仅占 ~15%；
- **U1 登记表单点**：`core::builtins::builtin_arity(name)` 成为 257 项 builtin
  arity 的**唯一权威表**；`create_registry` 逐项校验 VM registry 与核心表
  一致（debug fail-closed）+ `arity_consistency_with_core` 测试锁定——新增
  builtin 必须先登记 arity 到 core；
- **U2 trap 常量单点**：`diagnostic/trap_msgs.rs` 承载 SD-7/8/9 四个消息模板，
  经 `include!` 同时进入 `diagnostic::codes::trap`（VM 侧）与 `runtime/mod.rs`
  （standalone 原生 runtime 侧）；E 码串在 runtime 侧以本地 const 镜像
  `codes::E08xx` 并加注释锁定；四 trap 函数重构为 `trap_write_static`/
  `trap_write_raw` helper，输出逐字节不变；
- **验证**：5324 lib + 15 main + 31 real_world + 1 cli 全绿；dual_ 882 绿；
  clippy/fmt/unsafe 门禁绿；standalone runtime（rustc 直编）冒烟通过。

### 0.35.43 — R3 VM 字节码 peephole（MOV 传播 + 冗余 CHECK_I32 消除）

- **peephole 优化 pass**（`peephole_optimize`，compile_func 末尾）：
  1. 复制传播 `op rd=X; MOV rd=Y, rs=X → op rd=Y`（X 死 + 单一定义，跳过 merge 点）；
  2. 相邻重复 `CHECK_I32` 消除；
  3. `CHECK_I32(X); MOV(Y=X)` 重排为 `MOV; CHECK_I32(Y)` 使后续可传播；
- **健全性**：`Op::dest_reg`/`reads_reg`/`with_dest` 完整枚举（含 Unwrap/Call/
  Some 等），`writes_once` 单一定义门禁排除 `if`/`match` merge 寄存器；跳转
  offset 与 `SetFaultPc.handler_pc` 逐 pass 重映射——L1 dual_ 882 绿锁定；
- **实测 VM dsp（release）**：8.4s → 5.8s（~1.45×，循环 17→13 指令）；R1+R3
  合计 8.9s → 5.8s（1.54×）。**未达 ≤1s**：余量在 156 Op 单 match 分发（V2）
  与 const-operand 融合，需 R2 后续（寄存器池已就绪）+ const-op 融合，1.x
  评估 direct-threading/JIT；
- **验证**：5322 lib + 15 main + 31 real_world + 1 cli 全绿；dual_ 882 绿；
  clippy/fmt 全绿。

### 0.35.42 — R1 VM 载荷 Arc 化（String/List 先行）

- **`Value::String(String)` → `Value::String(Arc<String>)`、`Value::List(Vec<Value>)`
  → `Value::List(Arc<Vec<Value>>)`**：真浅拷贝——`Op::Mov`/算术读取仅 O(1) 原子
  引用计数，突变点显式写时分离（9 处 `Arc::make_mut`：StrAppend/ListPush/
  ListPop/index-set/sort/reverse/push/pop/clear）；克隆不再深拷贝 String/List
  载荷；
- **实测 VM dsp（release）**：8.9s → 8.4s（~6%），**未达 ≥3× 目标**——dsp 是
  Int/Float 热循环，V1（Value 大枚举搬运）被高估：枚举尺寸仍由未 Arc 化的
  Record(HashMap) 主导，真正的 dsp 瓶颈是 V2（156 Op 单 match 分发）+ V4
  （checked 算术），由 R2/R3（寄存器池 + superinstruction）与完整 R1
  （Record/Set/Tuple Arc）承接；
- **验证**：5320 lib + 15 main + 31 real_world + 1 cli 全绿；dual_ 882 绿；
  clippy/fmt 全绿（COW 语义由 L1 dual_ 等价门禁锁住）。

### 0.35.41 — C1b/C1c 循环 unroll 调查（dsp 对齐回归，audit-triage-0.35.25.md）

- **结论：dsp ≤1.15× 不可经 unroll 控制达成，登记为 LLVM 18 已知差距。**
  0.35.30 调查的"IPC 0.72 vs 1.00（80× unroll 前端开销）"经实测复核：
  用 `llvm.loop.unroll.count`/`unroll.disable` 抑制 unroll 反而回退（count=4
  → 1.54×，disable → 1.99×，基线 1.38×）——unroll 摊薄了 per-op finiteness
  检查（SD-9）与 `let mut` 局部变量的栈往返（mem2reg 无法跨 80× unroll 提升），
  收紧 unroll 把它们逐迭代暴露。C1b（局部 SSA）/C1a（参数 SSA）已由 LLVM
  mem2reg 在 O1 达成；
- **新增能力（默认关闭）**：`MIMI_LOOP_UNROLL_CAP=N` 环境变量在 while/loop
  回边附加 `llvm.loop.unroll.count` 元数据。关键实现：LLVM 要求 loop 元数据为
  **自引用 + distinct** 节点（`!N = distinct !{!N, !M}`），C API 无
  `MDNode::getDistinct`——用 `LLVMReplaceMDNodeOperandWith` 把占位 operand 0
  替换为节点自身达成自引用，**推翻 0.35.30"metadata 不可行"结论**；
- **验证**：5320 lib 全绿；四象限 dsp/fib/mandelbrot 无回退（fib/mandelbrot
  路径不受 while/loop 改动影响）。

### 0.35.40 — M5 Flow typestate 公理填充（audit-triage-0.35.25.md）

- **`lower_transition_to_vir` 转正**：原为无调用方的死代码（空 TypestateAxioms），
  现从 transition body 的 checker 已验证契约子句填充三字段——`invariant:` →
  `source_invariants`、`requires:` → `transition_guards`、`ensures:` →
  `target_invariants`；
- **Flow transition 验证路径重接**：`flatten_items` 不再把 transition 合成裸
  FuncDef，改为 `StepKind::Transition` → `verify_transition`（VIR typestate
  契约级证明 `(source_invariants ∧ transition_guards) ⊢ target_invariants`，
  不可编码则回退 AST 路径验证可执行体）；
- **只注入 checker 已验证项**：状态级 `invariant:`（`state { ... }` 声明）语言
  尚无，`source_invariants` 仅在 transition 自身 `invariant:` 子句时非空，0.1.6
  Flow typestate 支柱补齐；可执行体（record 构造/`self` 字段）由 checker 验证，
  不进 Z3；
- **回归锁**：`lower_transition_populates_typestate_context` + transition
  Proven/Disproven 双案例；
- **验证**：5319 lib + 15 main + 31 real_world + 1 real_world_cli 全绿；clippy/fmt 全绿。

### 0.35.39 — 僵尸关键字裁撤（13 个 → 关键字表 80→67）

- **裁撤 13 个僵尸/已否决关键字**：`c_shared`/`c_borrow`/`c_borrow_mut`/
  `local_shared`/`weak_local`/`raw_string`/`nothing`(token)/`alloc`/`async`(top-level)/
  `with`/`desc`/`rule`/`mms` —— 删 TokenKind + AST 变体 + 解析分支，零迁移成本
  （语料 0 使用）；共享收敛为 `shared`/`weak` 二态；
- **`Type::Nothing` 保留为语义残差类型**（ZonkedTy 产生，无关键字）；`alloc`
  变体删除后 allocator 保持隐式化；
- **5 处 Shared-unwrap 回归修复**：shared 尾表达式隐式返回、`Option<shared T>`
  deref、shared deref-assign、方法隐式返回、shared 字段访问自动解引用；
- **测试面**：删除/改写 60+ 使用已裁撤关键字的测试（含 12 个
  audit_fix_bind 死测试，raw_string 移除使 StringOwned/StringTransfer 契约
  失去 producer）；保留全部 `shared`/`weak` 活跃测试；
- **验证**：5316 lib + 15 main + 31 real_world + 1 real_world_cli 全绿；
  clippy/fmt/language-docs/unsafe/roadmap 门禁全绿；golden 无受影响。

### 0.35.38 — M4 callee ensures 公理编码失败 fail-closed

- **#M4 callee ensures 公理编码失败静默丢弃（违反红线 #2）**：callee 的 ensures
  公理在实参代入后无法编码（如把 `arr[0]` 传入已验证 callee）时，编码失败被
  静默丢弃——caller 在弱化的上下文上证明，翻 Disproven 不可追踪。修复：
  `assert_callee_ensures_in_expr` 携带 caller_name + errors 向量，`expr_to_z3_bool`
  返回 None 时 push "postcondition cannot be encoded — not verified"（对齐 H1
  requires fail-closed 模式）；block/stmt walker 与两处调用点统一共享
  call_site_errors；
- **回归锁** `verify_callee_unencodable_ensures_is_fail_closed`：callee 无
  requires（H1 无法遮蔽），caller 传 `arr[0]`——无修复时反例不可追踪，有修复
  时点名 postcondition；
- **验证**：5385 lib + 15 real_world + 31 real_world_cli + 1 main 全绿。

### 0.35.37 — 审查 MEDIUM 批量 + H 系列收尾（audit-triage-0.35.25.md）

- **#H6 CAP_TABLE 跨线程静默失败**：thread-local 线性能力语义保留，新增全局
  `CAP_OWNERSHIP` 归属注册表——check/consume/drop 失败路径读归属线程 emit
  warn_cross_thread 诊断；owner 线程 happy path 零成本；
- **#H7 PENDING_C_STRINGS 超限静默 drain**：thread-local `RECLAIMED_C_STRINGS`
  危险注册表，drain 回收前登记，`mimi_string_as_c_str_free` 命中时显式 UAF
  警告并退出租约（非静默 no-op）；
- **#H8 callback store 锁中毒静默跳过插入**：`insert if let Ok` →
  `unwrap_or_else(into_inner)`——锁中毒可恢复（guard 持有数据），注册不得
  静默跳过；callback/ffi_runtime 其余 lock 点统一 into_inner 模式；
- **#H9 CBufferInner Sync 结构强制**：ptr/size 字段私有化 + 只读访问器，
  任何 `&CBufferInner` 无法写 buffer，Sync 健全性由构造保证（编译期断言锁住）；
- **#H11 失败函数体块删除循环吞错**：`delete()` Err 显式 LlvmError（含 block
  count）fail-closed；count>0 但 get_first None 显式守卫（原 `let _ =`
  挂死编译器）；
- **#M2 cap drop 发射错误被吞**：`mimi_cap_drop` 声明补全 + block.rs×2 /
  method.rs `let _ = build_call` → 错误传播；register_cap 移到
  compile_pattern_bind 后（否则 `let c: cap X` 的 Drop 静默跳过）；
- **#M3 Capability 回退门禁**：ResolvedType::Capability 已是独立变体
  catch-all 拒绝（K-5 早已 fail-closed）；真实缺口是 Drop 的 `locals.get`
  静默跳过 → ok_or_else fail-closed + 回归锁；
- **#M6 ABI 反序列化静默回退 → 可观测**：parse_primitive/symbol_kind/
  call_conv/callback_category fallback 首例 emit 一次性警告（点名未知值）；
- **#M7 map_from_list 静默截断/坏 key 跳过**：cap 超限截断显式警告（含丢弃
  数）+ key 校验失败首例警告；values 不可 mincore 校验登记为 caller 契约；
- **#M8 buf_nul_terminate 堆越界写**：codegen total `sadd.with.overflow` 溢出
  trap + runtime helper 加 alloc_size，offset ≥ alloc_size → mimi_runtime_abort
  （child-process 回归锁验证 abort 消息）；
- **#M9 expect/unreachable 不变量崩溃面 → 降级**：session cycle DFS 两处
  expect → let-else；hover Type::Located unreachable → 占位文本；progressive
  find_map expect → 降级 no-injection；
- **#M10 LSP active 文档 URI 沙箱**：register_uri_source 改
  uri_to_path_sandboxed（didOpen 不再探测 workspace 外路径存在性）；
- **顺带**：MIMI_DUMP_MODULE_OPT 诊断（post-pipeline IR dump）、capability
  argument 转移对齐 checker 语义、M8 child-abort 测试防 fd race 加固；
- **验证**：5378 lib + 15 real_world + 31 real_world_cli 全绿；clippy/fmt 零警告。

### 0.35.34 — 审查 H1/H2 verifier 静默缺口（audit-triage-0.35.25.md）

- **#H1 callee requires 静默跳过（fail-open）→ fail-closed**：不可编码前置
  条件（调用/字符串）→ 显式 "cannot be encoded — not verified"（原静默
  跳过）；Unknown 超时 → "could not be decided — not verified"（原被当满足
  吞掉）；Sat 保持 "may violate"。与 V-C4（只采纳已验证 callee ensures）+
  V-2 fail-closed 哲学对齐；
- **#H2 i64 Z3 全程无界建模（definedness 仅 i32 硬编码）**：Resolved 引擎
  int_bounds 按类型表取机器界，collect_* definedness 扩到 i32+i64（Add/Sub/
  Mul 溢出 + Div/Rem 零除数/MIN÷-1 + Negate MIN）；AST 回退路径参数化为
  `int_definedness_obligations(expr, vars, lo, hi)`；i64 溢出从"仅 E0439 分歧
  兜底"升级为双引擎一致 Disproven；
- **验证**：5363 lib + 15 real_world + 31 real_world_cli 全绿。

### 0.35.29 — 审查 H4/H10/H12/H13 net/runtime 家族（audit-triage-0.35.25.md）

- **#H4 SSRF 校验被 [IPv6] 括号绕过**：死代码 ssrf_validate_host 转正为唯一
  守卫（剥 [v6] 括号）；runtime 三份逻辑统一为一份；新增 inet_aton 兼容 IPv4
  解码（十进制/hex/octal、1-4 段点分、127.1 短形式、::ffff: IPv4-mapped）；
  VM 侧镜像同逻辑；fe80::/10 link-local 缺口修复；172.16/12 判定修正；
- **#H10 str_repeat 无上限**：`s.repeat(*n as usize)` 裸奔（i64::MAX 容量溢出
  panic / 大值 OOM）→ checked_mul + 8 GiB cap，超限/溢出干净 Err 非崩溃，
  双端一致；
- **#H13 close_fd 可关闭 stdio**：对齐 net.rs connect 的 fd ≤ 2 守卫——VM
  builtin_close_fd → Err（指名 standard stream）；runtime mimi_close → -1
  哨兵；
- **#H12 exec 大输出 OOM + socket/HTTP 无超时无上限**：runtime/fs.rs 新增共享
  run_exec_capped（spawn + 边读边截 + 超限继续 drain），7 个变体统一走此
  helper（VM 复用 runtime 实现 L1 by construction）；HTTP VM 镜像 runtime
  （connect/read/write 5s 超时 + MAX_HTTP_RESPONSE 100MB）；builtin_recv 补
  MAX_RECV_SIZE 100MB（`vec![0u8; i64::MAX]` 此前直接 abort）；
- **验证**：5358 lib + 15 real_world + 31 real_world_cli 全绿。

### 0.35.28 — 审查 H3/H5 CFG 与 JSON owned（audit-triage-0.35.25.md）

- **#H3 CFG Continue 边漏出循环门（L2 双漏报）**：E0415 借阅检查对称补齐
  Continue 边（body 终止于 Continue、永不产生 Backedge 时 loan 跨迭代存活）；
  is_diverging_sink 对称补齐 Continue 边（`loop { continue }` 持 cap 不再绕过
  E0256 消费门）；
- **#H5 JSON map 嵌套变体外壳未登记 owned（destroy 永不释放）**：台账"6 个
  变体"复核为 **29 处泄漏**（pack 系 19 + Box<MimiList> 系 10 + 自建 16B
  header 2）；新增 MapOwnedValueKind::PackErrCString / ListObject，嵌套外壳随
  destroy 释放；内嵌 map/set/list handle 不递归释放（bounded leak 换 UAF
  防护，与 mimi_map_destroy 注释哲学一致）；
- **验证**：5344 lib + 15 real_world + 31 real_world_cli 全绿。

### 0.35.27 — 审查 C3/C4 悬垂与深度守卫（audit-triage-0.35.25.md）

- **#C3 FFI 跨线程异步回调悬垂指针 UAF**：方案 A1 严格 Arc 化——BytecodeProgram
  生产返回 Arc；BytecodeVM 删生命周期；`Value::BytecodeClosure` 携带所属
  program Arc（闭包自包含，跨线程/跨 VM 永不错配、永不悬垂）；删除整套旧机制
  （SendProgramPtr/CALLBACK_PROGRAM/set_callback_program）；
- **#C4 JSON strict 校验路径无递归深度限制**：strict_value 与 permissive 共享
  self.depth（进入 +1 / 超限 false / 退出 -1），>64 层一律拒绝（50000 层栈溢出
  崩溃消除）；
- **验证**：5337 lib + 15 real_world + 31 real_world_cli 全绿；FFI 106 + actor 98 全绿。

### 0.35.25-26 — 审查 C1/C2/M1 正确性 P0（audit-triage-0.35.25.md）

- **#C1 float_chain 传播性收紧（miscompilation）**：is_chain_op 重构（FDiv/FRem
  仅被除数位置 + libm 白名单 + 用户函数 call 黑盒排除）——FDiv/FRem 除数位置
  漏 trap 影响所有默认 O1 构建，c1_poc2 三端对等（VM/O0/O1 均 E0813 trap，
  修复前 O1 打印 0 通过）；
- **#C2 f-string 深度逃逸**：子解析器继承外层 recursion_depth（唯一生产路径
  逃逸面）+ f-string 专用预算 DEPTH_MAX_FSTRING=64（6000 层 → ParseError 不
  崩溃）；2MB 栈 31 层安全 / 40 层预算拦截；
- **#M1 PROBE 后门删除**：MIMI_PROBE_CAP 环境变量（深度上限可被覆盖到任意值，
  与 C2 叠加放大，违反红线 #3）删除；probe 测试保留（inert）；
- **验证**：5336 lib + 15 real_world + 31 real_world_cli 全绿。

### 0.35.24 — claim_returned_lists / 赋值路径 claim 收口（deep-eval 遗留观察 1 + 根因家族）

- **Call 分支删除**（`src/codegen/func.rs`）：`claim_returned_lists` 不再
  递归 Call 的 args——args 是输入，不属于返回值所有权形状。0.35.23 的递归
  会把局部 List 变量（非 borrow）误置 null：尾调用场景 scope-exit free
  变 free(null) → 每次调用泄漏；字段赋值 `rec.field = g(local)` 场景
  local 后续索引读 null data（native 输出 0，VM 不受影响 → 双后端分歧）。
  mutate-builtin 尾调用语言层返回 unit（`return push(data, n)` 无法
  typecheck），用户函数由 callee 自身返回路径 claim，删除无正确性损失；
- **赋值路径 claim 删除**（`src/codegen/func/body.rs`）：0.35.23 同批在
  `compile_assign_stmt` 加的“RHS list 置 null”（变量赋值 `dst = xs` 与
  字段赋值 `rec.field = xs`）同样违反 COW 共享语义（List 赋值是浅拷贝，
  push 时 realloc 分离，不是所有权转移）——`xs` 被置 null 后后续
  `xs[0]` 读 null data（native 0 vs VM 1）；
- **var_type_names 嵌套污染修复**（`src/codegen/func.rs`）：
  `compile_func_legacy_inner` 与 `compile_generic_func`（monomorphized
  实例入口）现在对 `var_types` / `var_type_names` / `list_elem_llvm_types`
  做入口快照、出口恢复——嵌套编译 callee 实例时不再清掉 caller 已登记的
  局部 List 类型，`claim_returned_lists` 在嵌套调用后不再静默跳过
  （`return data` 场景从 latent use-after-free 变为正确所有权转移）；
- **回归锁 ×4**：`dual_claim_stops_at_call_args_in_legacy_body`（修复前
  native 0 ≠ VM 1）、`dual_var_assign_keeps_rhs_list_alive`、
  `dual_field_assign_keeps_rhs_list_alive`、
  `ir_nested_compile_keeps_caller_var_type_registration`（IR 断言：
  嵌套调用后 claim 的 null-store 必须存在）；
- **验证**：5330 lib + 31 real_world 全绿；golden 无漂移；projects 差分
  mimi-make / mimi-stat / mimichat MATCH；devdocs/v0.35/
  deep-eval-projects-0.35.23.md 遗留观察 1 标记已修。

### 0.35.23 — deep-eval projects 战役（mimi-make 三层修复 + mutate 参数 claim 守卫）

- **mimi-make 三层修复**：K1 嵌套 else-if value 模式坏——`let x = if/else` 类型
  登记三路径 fallback（block.rs compile_block / compile_block_last_val + func.rs
  compile_func_body 顶层），infer_object_type 新增 `Expr::If` 分支，mkr 最小复现
  E0707 消除；递归依赖 E0407；无参数 target 空转；
- **mutate 参数 claim 守卫**：K2 claim_string_return_value 聚合字符串字段 claim
  （tuple/record 返回的 {ptr,i64} 字段走 B9 值精确守卫，所有权转移调用方，
  消除 parse_variable UAF）；K3 borrow_param_names 守卫集（view/mutate 参数
  跳过 claim_returned_lists null-out，修复 mutate_list_push_allowed 存量 SIGSEGV）；
- **引入性能回归（登记）**：main 签名 (argc, argv) + mimi_args_init 使热循环
  偏移 9 字节（op-cache/代码对齐冲突）——dsp O1 1.06×→1.36×，bisect 定位
  2026-08-10，修复排期 Phase I（0.35.30 dsp 对齐回归）；
- **验证**：5325 lib + 15 main + 31 real_world 全绿；examples 22 MATCH；
  projects 五件套（mimi-log/mimi-lint/mimi-stat/v02817_acceptance/mimi-make）
  全 MATCH；golden 再生成；clippy/fmt 全绿。

### 0.35.22 — 可用性修复收官：E0439 提示 + 文档语法 + 错误消息 + test 参数（Phase H）

- **#5 E0439 帮助文本**：`codes.rs` 与 `docs/error-codes.md` 同步——算术属性
  （`ensures: result == x * x`）触发 resolved/flow 引擎分歧的成因说明与消除
  办法（显式边界 `requires: x <= 46340`）；
- **#4 language-spec.md §6.8 合约语法修正**：函数头行尾 `requires x >= 0`
  （无冒号，parser 拒绝）→ 函数体内 `requires: x >= 0`（带冒号，golden
  语法）；全文档无残留错误示例；
- **#2 lower.rs ref 绑定错误消息**：V-1 内部工作项编号（V-1, Wave-3）→
  用户可读语义（`let ref` 需 enclosing arena block）；
- **#1 mimi test 零参过滤**：只收集零参 `test_` 函数并运行；带参辅助函数
  （如 `test_color(c: Color)`）跳过并提示（原无条件零参调用 → E0800 误报）；
- **验证**：5321 lib + 15 real_world + 31 real_world_cli 全绿；demos 差分
  14/15 MATCH 保持。

### 0.35.21 — 推断路径 i32 溢出守卫 + loader impl 去重键（Phase H）

- **#8 推断路径 i32 溢出三路径统一**（interp 静默回绕 / codegen E0802 trap）：
  `infer_expr_type` 字面量按 i32 范围定宽（in-range → Int32）；新增
  `binop_is_i32_width` 统一宽度判定（字面量弹性适配非字面量侧，负字面量
  `Unary(Neg)` 识别）；推断路径 let 级 `CheckI32`（限定 Binary/Unary 标量
  算术，防误伤 Call/closure）；Range/Lambda infer 返回 Unknown；
- **#8 测试**：codegen trap 断言改用 checked（resolved）路径
  （`assert_both_backends_trap_e0802`）；trap_tests 3 处旧宽松断言（静默
  回绕时代）更新为 trap 语义；新增 3 回归锁（fold 溢出 / 变量乘法 /
  i64 字面量不误伤）；
- **#3 loader item_name 去重键**：`Item::Impl` 由 `type_name` 改为
  `(trait, type)` 组合键——`use std::strings` + `use std::fs`（同对
  `string` 实现 Str/FsOps）不再报 duplicate item；`mod.rs` + `flow.rs`
  两处合并路径同步；回归锁
  `loader_std_strings_plus_fs_merge_no_dup_impl_key`；
- **验证**：5320 lib 全绿；45 loader 测试全过。相邻发现：std/json + std/maps
  的 pub `has_key` 同名不同签名（std 库命名问题）登记 0.2。

### 0.35.20 — #6 codegen 嵌套容器全修复：zip/enumerate/partition/chunks（Phase H）

- **zip/enumerate heap-pack pair 布局**：`infer_list_builtin_return_type`
  白名单扩 zip/enumerate + `pending_zip_pair_type` 类型通道 +
  `build_zip_pair` 类型感知构造（string 内联 {ptr,len}、嵌套 List 按值
  load、float bitcast、窄 int truncate）；`05_lists.mimi` 首次完全 MATCH；
- **深挖三处隐藏 bug**：① `build_zip_pair` string 字段 GEP 基址错用
  `pair_heap`（偏移 0）→ 应 `field_gep`——zip(string,i32) 碰巧正确，
  enumerate(i32,string) 覆盖 idx/ptr 槽 → `strlen(0x1)` SIGSEGV；
  ② `emit_return` 的 flush 在 claim 之前（O0 暴露 use-after-free，O1
  掩盖）→ claim/flush 顺序重构；③ List 字面量返回无 claim 路径
  （`([1,2],[3,4])` 无命名槽可 null）→ `claim_returned_list_literals`
  深拷贝 data 缓冲（llvm.memcpy size=0 允许 null 指针，空 List 免分支）；
- **push 对 List<T> 元素深拷贝 data**（chunks 内层悬空根因）；
  `claim_returned_lists` 顺序修正（返回值 load 后、free 前）；
- **附带修复 fmt_type pre-existing bug**：`same_type` Newtype-Name 交叉分支
  剥参数导致 `Newtype("a", Option)` ≡ `Option<i32>`（proptest seed
  0b73fde9 触发）→ else 分支保留 args；
- **回归锁 ×6**（zip strings/ints、enumerate strings、zip+enumerate 同函数、
  partition、chunks、用户函数返回 List tuple）；5317 lib 全绿；demos 差分
  14/15 MATCH（除 14_ffi 环境差）。

### 0.35.19 — 错误消息 CO-H2 精确 span（dx-backlog #7，Phase G）

> 消灭一条内部 `TOOL-RESOLUTION-001` 泄漏路径：tail if/else 分支类型不匹配
> 从“无 E 码 + 无 span + 泄漏 rt:<hash> 类型 ID”变为 E0214 + 精确 if 语句
> 定位 + 语言类型名。报告 `devdocs/v0.35/error-coh2-0.35.19.md`。

- **checker/lowering 对齐**：`check_block_with_implicit_return` 尾部
  `Stmt::If`（带 else）改双向检查（锚定 if 语句 span；双分支 diverging 时
  不触发隐式返回检查）——此前 checker 放行、resolved lowering 用内部码拦截；
- **类型名渲染**：`PrimitiveType::language_name()` + `BodyLowerer::type_display`
  （Primitive/Nominal/Tuple/Result/Option/Reference/Newtype 递归渲染，深度上限
  4；`builtin:type:` 前缀剥离）——`implicit_conversion` 错误消息不再泄漏
  `rt:<hash>` 内部 ID；
- **span 回退链**（resolved/mod.rs）：generated NodeId 查 node_meta 失败时提取
  `function:<owner>` 锚点，不再静默 `Span::UNKNOWN`；
- **统一 if 分支检查**：`Checker::check_if_branch_types` 共享方法（cond bool
  E0205 + 双向分支检查 + unify E0214），接线 `check_expr_inner` 的 `Expr::If`
  / `check_block_expr` 尾部 If / tail-if 三处——**diverging 分支豁免**（尾部
  return/break/continue 不参与 unify，flow `-> A|B` 双状态 return 合法）+
  **数值强制豁免**（i32/i64、int/float 混合分支合法）；
- **消灭合成临时 Expr 隐患**：`check_block_expr` 尾部 If 分支不再合成
  `Expr::If` 调 check_expr（合成节点无 AST meta → 无 stable NodeId →
  `stabilize_expression_types` abort；该路径首次被真实触发后暴露）；
- **回归锁**：`src/tests/error_co_h2.rs` ×4（E0214 精确 span / diverging
  豁免 / 数值强制 / 无内部 ID 泄漏）；全量 **5311 lib** + 15 main + 31
  real_world + cli 绿；clippy 零警告；fmt 干净；dispatch 无静默回退。

### 0.35.18 — fmt 评估收尾（dx-backlog #4，Phase G）

> 全语料 round-trip 幂等 + 语义保持双维度评估，确认 `mimi fmt` 语义安全；
> 语料级回归锁入仓。报告 `devdocs/v0.35/fmt-eval-0.35.18.md`。

- **幂等性**：153 文件（demos/examples/std/libraries/projects 全量）
  `fmt(fmt(x)) == fmt(x)` **100% 通过**；
- **语义保持**：50 个有格式变化文件——token 流等价（lexer 序列对比，忽略
  Newline/Indent/Dedent）+ 同目录 `mimi check` **0 破坏**；
- **方法论教训**：stdlib 上下文文件必须同目录验证——`std/maps.mimi` 拷到
  /tmp 的 `unknown type 'Any'` 是路径假阳性（`Any` 仅 std/ 目录内可见），
  格式化输出放回 std/ 目录即通过；
- **语料级回归锁**：`src/tests/fmt_corpus_eval.rs`（幂等 + token 流保持，
  demos/examples 全量 + std/maps.mimi round-trip 锁）；
- **登记 0.2**：类型位置泛型尖括号插空格（`List<string>` → `List < string >`，
  风格非 bug——token 流不变、parser 正常接受）；golden 快照可选。

### 0.35.17 — 深度可用性评估 + 全面查缺补漏（Phase F：demos 双后端差分）

> 三段式战役：复盘 0.35.1–16 与 backlog 对账 → 深度可用性评估（demos 双后端
> 差分，发现 10+ 存量 bug 家族）→ 逐个修复 + 回归锁固化。修复报告
> `devdocs/v0.35/deep-eval-0.35.17.md`。

- **B1 CFG worklist Continue 死锁**：`while { if c { continue } }` 触发
  "resource analysis did not reach return block" 假阳性（demos/02 整个 0.1.4
  窗口都是坏的）——`predecessors_ready` 豁免 Backedge/Continue loop-carried
  边 + 2 回归锁（borrow_boundary.rs）；
- **B2 字符串所有权家族**：B2a resolved string 返回所有权探针（match 尾 arm
  的 string 返回被 free 后返回悬垂指针，demos/04 abort）；B2b
  register_heap_alloc entry-block null-init（if 分支内 concat 未开分支 free
  垃圾，demos/04 describe_point segv）；B2c 自定义 Res 四层全链（var 绑定
  → match 解码 → `?` 早退传播 → 隐式返回 claim，demos/07 完整 MATCH）；
- **B3 closure string 返回垃圾**（test_closure_call 双后端 MATCH）；
- **B5 嵌套 else-if string 零填充丢值**（静默错误输出，demos/06 修复）；
- **B6 泛型 string 参数单态化**（legacy_param_llvm_type type_map 查询——
  参数 i64 alloca 存 16 字节 string struct 的栈破坏，demos/03
  SelectionDAG 崩）；
- **B7 builtin Result/Option scrutinee 旁路解码**（`match read_file(…)`——builtin
  不在 func_defs，Err(e) 绑定保持 raw i64 句柄 → concat 报 E0700；scrutinee
  helper + pending_scrutinee_result_ty 旁路）；
- **E0200 Result 布局分裂**（std/fs read_lines `use std::fs` 编译失败）：legacy
  Ok(List) 造 {i1,ptr,i64} 而 Err(string) 造 {i1,i64,i64}——Err 构造器改按
  Ok 值表示 pad（pending_result_ok_ty），双 arm 布局统一；
- **resolved Err 解码按绑定类型**（Flow 硬编码 {i64,i64} tuple 误读普通
  Result<string,string> 的 {ptr,i64} string 句柄 → E0722）；numeric_convert
  ptr→List struct 支持（仅限 {i64,ptr} 容器布局，函数指针不 load）；
- **09 Result 显示全链**：let 绑定 builtin Result 注册类型（compile_func_body
  缺失，compile_block 有——两路径对齐）、write_file 类型名 Result<(),string>
  修正、`Ok(())` unit 载荷显示 "()" 而非 0；
- **read_file/write_file Err 契约统一**：裸数据指针 → heap {ptr,len} 句柄
  （match decode/`?`/display probe 契约），错误消息对齐 VM 的
  `e.to_string()`（runtime 新增 mimi_os_error_message）；
- **B4 newtype → Nominal 转换**（demos/11 TOOL-RESOLUTION-001）：lower.rs
  implicit_conversion 同 item 的 NewtypeWrap 放行，demos/11 完整 MATCH；
- **门禁**：demos 双后端差分 03/04/06/07/08/09/10/11/12/13/15 +
  test_result_match 全 MATCH（仅剩 05 zip / 14 ffi 环境 / test_time 时间差
  三项已知非 bug）；全量 5305 lib + 15 main + 31 real_world + cli 绿；
  clippy --all-targets 零警告；fmt 干净；新增 7 回归锁
  （src/tests/deep_eval_20260809.rs，check+VM+codegen 三方断言）。

### 0.35.16 — 全门禁复跑 + 四象限矩阵终测 + 0.1.5 RC 复核（Phase E）

> RC 前全门禁复跑，零代码变更（仅治理文档）。终测报告
> `devdocs/v0.35/quad-final-0.35.16.md`。

- **门禁全绿**：全量 5296 lib + 15 main + 31 real_world + cli 绿；clippy
  零警告；fmt 干净；language docs（31 requirements/support）有效；unsafe
  SAFETY 门禁 OK；dispatch 门禁 fallback_rate 0.2735 = 基线（零静默回退）；
- **四象限终测**：dsp O1 默认 112.7ms（1.06× C -O2，基线 402.1ms/3.97×——
  0.35.3 链式末端检查收敛跨 13 个 sprint 稳定保持）；dsp O1+ieee 1.04×；
  mandelbrot O1 1.81×；无象限回退超仪器方差——DX/质量次线 sprint
  （0.35.12–15）零性能污染；
- **RC 复核裁决**：#21（zip raw-pair 显示）/#22（resolved map builtin，
  closure 桥接）经风险评估改登记 **0.2**——两者都需触碰 0.35.11 的 fragile
  面（product formatter heap-pack 假设 / resolved 高阶内建 emit），RC 窗口
  内引入 segfault 风险不对称；当前状态无 crash、输出语义正确（zip legacy
  显示空行、map 函数体 fallback legacy），不阻断 RC。

### 0.35.15 — LSP 文本搜索 → Span/Origin 迁移（dx-backlog #3，Phase D 顺延项）

> A6 基础设施（Span/Origin/AstNodeMeta + PositionMap）的消费端收尾：LSP
> 内全部“文本扫描定位 AST 位置”的调用点迁移到 AST span（探针测试锁定锚点
> 契约后逐文件迁移）。顺带修掉一批潜伏假阳性：`contains("impl")` 落在
> 首个 impl、`func {name}` 落在注释/调用点、let 绑定落在同名先行绑定、
> 括号计数被 `let s = "}"` 截断。

- **探针测试**（`src/tests/lsp.rs` +2 项）：锁定迁移依赖的 span 锚点契约
  ——FuncDef/TypeDef/ModuleDef/ImplDef 锚定关键字、let pattern 锚定绑定名、
  Call 表达式锚定 callee、FuncDef.end_line 到闭括号行；
- **references.rs**：goto-definition/references/highlight 的 Type/Module/
  let 绑定定位全部 span 化（删除死文本回退）；impl 跳转位置精确到块行；
  `enclosing_func_line_range` 改 AST 包含（rename 作用域不再依赖
  `starts_with("func ")` 启发式）；
- **util.rs**：`find_func_end_line` 删除（SourceScanner 括号计数被
  `span.end_line` 平替），`find_enclosing_func_in_items`/`hash_func_body`
  span 化（签名去 text 参数）；
- **symbols.rs / lens.rs / hierarchy.rs / inlay.rs**：文档符号/工作区符号/
  code lens/调用层级/inlay 提示的 def-line 与 call-line 全部 span 化；
  inlay 参数提示的 call-line 不再落在首次提及，括号扫描从 callee 名尾开始；
- **测试更新**：audit_fix_lsp 括号计数测试重写为 span 包含测试（字符串/
  注释内括号假目标结构性免疫）；全量 5296 lib + 31 real_world + cli 绿；
  clippy 零警告；fmt 干净。

### 0.35.14 — DX backlog 三项：#13 runs_flow 三层集成 + #16 C stdio 混流 + #18 tuple fn 取出（Phase D）

> 质量次线三项落地。头条是 #13（0.34.29 曾有"超单 sprint 范围"回滚史）：
> actor `runs` Flow 的 transition 方法调用（`a.inc()`）此前被 checker
> 误报 E0221 "has no method"（合法程序被拒 = L2 假阳性），本次实测逐层
> 推进完成类型检查层三层集成；`mimi check` 全通，bytecode dispatch 运行时
> 行为不变。#3（LSP Span/Origin 迁移）顺延下个 sprint。

- **#13 层①（checker + infer 方法注册）**：`collect_item_decls` 将 runs_flow
  actor 的每个 transition 注册为合成方法（签名 `(self, event params…) ->
  ToState`；`fails E` 时返回 `Result<ToState, (FromState, E)>`——与
  codegen/VM dispatch 消费的形状一致）；显式同名方法优先。infer 新增
  `runs_flow_transition` 辅助 + `is_actor_method` 扩展：调用点按 actor 方法
  dispatch（E0221/E0257 误报消除），参数类型/元数照常 E0211/E0257，typo
  建议含 transition 名；
- **#13 层②（zonked 签名）**：无需额外注册——`finalize_zonked_func_types`
  遍历 checker `funcs` 目录，自动覆盖合成条目；
- **#13 层③（resolved callable identity）**：resolved 目录为每个 transition
  注册 `function:{Actor}::{transition}` 的 `ResolvedFunction`（含 implicit
  self + 参数 NodeMeta）与 `ResolvedActorMethod`（call-site KIND 事实因此
  归类为 Method 而非 Unknown）；typed body lowering 对合成 callable 豁免
  （无语义体，转移表由 VM/codegen 运行时 dispatch）。**边界**：`mimi build`
  codegen 仍报 E0700（runs_flow codegen 维持 0.2 登记，spec §6.12）；
- **#16（C stdio 混流乱序）**：VM `print`/`println`/`print_err` 每次 Rust
  侧写前 `fflush(nullptr)` 抽干 C stdio 块缓冲——比"退出前单次 flush"更强，
  在每个交错点保持程序序；无 FFI 场景空缓冲 flush 为廉价 no-op；
- **#18（tuple fn 元素取出调用）**：tuple 字面量中的具名函数元素在绑定处
  登记（`record_tuple_fn_elems`/`register_tuple_index_fn_binding`，Stmt::Let
  双路径），`let f = t.0` 取出绑定走间接调用路径（i64 ptrtoint 槽 inttoptr
  还原指针）——此前 codegen 报 E0700 "undefined function 'f'"（VM 可执行，
  双后端分歧）；
- **回归**：actors.rs 新增 3 项（dispatch typed check + 参数类型 E0211 +
  fails Result 形状），real_world 新增 `real_world_tuple_fn_element_call`
  双后端锁；全量 5294 lib + 31 real_world + cli 绿；clippy 零警告；fmt 干净。

### 0.35.13 — trivia 化：desc/rule/mms{} 降注释，Stmt 33→30（Phase D）

> dx-backlog #10（0.34.5a 推迟项）落地：`desc`/`rule`/`mms{}` 从 AST 降为
> trivia——parser 消费即弃（验证括号结构但不产出语句），表面语法兼容
> （旧源码继续可解析）。`math:` 保留 verifier 通道（P1 裁决）。#13（actor
> runs_flow 三层集成）从本 sprint 拆出单独排期（曾有“超单 sprint 范围”
> 回滚史）。

- **parser**：`parse_stmt_kind` 删除 Desc/Rule/Mms 产出臂（块外出现报
  trivia 诊断）；两个 block 循环（terminator/recovery）改为消费即弃；
  `parse_mms_block` 改返回 `()`，删除文本重建逻辑（仅保留括号平衡消费）；
- **AST**：`Stmt::Desc`/`Stmt::Rule`/`Stmt::MmsBlock` 三 variant 删除
  （Stmt 33→30）；
- **消费面清理（82 处 / 22 文件）**：resolved（语义键/节点标签/span 抽取
  7 臂）/ checker（check_stmt×3、func 合约探测、borrow）/ codegen（block/
  func/actors skip 臂）/ bytecode VM / CFG lower / resolved IR lower /
  loader span remap / lint / verifier×4 文件 skip 臂；
- **真实依赖处置**：`doc_core` desc/rule 文档提取循环删除（降注释后无
  结构化意图文本可提取，属裁决内行为）；`core::verify_rules` 降为恒净
  no-op（保留 CLI `--verify-rules` 接口）；LSP `has_contracts` 探测去
  MmsBlock；
- **测试重写**：`mms_integration.rs` 8 项全重写为 trivia 契约（解析无错 +
  零 AST 语句 + 运行时语义不变，新增独立 desc/rule trivia 锁）；
  parser/flow.rs mms 嵌套括号测试改为“消费后仅余 return”；语料实测
  desc/rule/mms{} 使用量为零（demos/examples/tests/std/projects 全扫），
  且新测试发现登记口径误差：真实语法为 `desc "text"` 无冒号；
- **验证**：全量 5292 lib + 30 real_world + cli 绿；clippy 零警告；fmt 干净。

### 0.35.12 — resolve→zonk 全量迁移 + parser panic 审计第一批（Phase D）

> dx-backlog #1 关闭 + #2 第一批（parse_expr/parse_stmt）审计落地。
> 两半各自独立提交（0.35.12a 迁移 / 0.35.12b 审计）。

- **#1 resolve()→zonk_or_unknown() 全量迁移（31 处生产调用点清零）**：
  infer_expr×2 / check_stmt×4 / checker·func×1 / checker·vars×6 /
  checker·items×2 / checker×1 / infer·lambda×1 / infer·call·helpers×5 /
  infer·call·simple×5 / infer·helpers×1 / infer·record×3。**语义裁决**：
  迁移点均为推断内部（非定稿边界），游离 TypeVar（let 多态占位）/ForAll/
  逃逸哨兵在迁移前 resolve 中原样透传——首版直接套 scan_residual 严格化
  在 flow checker unknown 哨兵与 let 多态游离变量两类合法路径上触发
  debug 断言（实证后回退）；`zonk_or_unknown` 最终对齐 pre-migration
  resolve 语义（resolve_infer + unknown 兜底 + 可见 debug 断言），严格
  定稿仍由真边界处的 `zonk` 承担；`resolve()` 降为 #[deprecated] 转发，
  仅 unification.rs 模块测试消费；
- **#2 parser panic 审计第一批（parse_expr/parse_stmt）**：审计结论矫正
  登记口径——**51 处 `panic!` 全部位于 #[cfg(test)] 测试区**（原登记未
  分离测试代码）；`unwrap()/expect(` 计数含 parser 自身的 Result 返回
  `self.expect()` 辅助方法（非 Option::expect）。生产区真实残留仅 3 处
  结构性不变量 unwrap（if-chain 首链 ×2 + token 索引 ×1），全部降级为
  `ParseError` 诊断（用户输入永不 abort 编译器）；
- **验证**：5292 lib 全绿（含 parser 92 / property 44）；clippy 零警告；
  fmt 干净。

### 0.35.11 — O1 正确性切片：O1/O0 双档对等三连修（Phase D）

> O1 默认化后的 dual 对等扫描发现 `demos/05_lists.mimi` 双档分歧：O0
> tcache double free abort、O1 静默输出错。三连根因各自独立，修复后
> 05_lists O0/O1 与 bytecode 逐行对等（除 zip 行，见已知限制）。

- **修复 1：resolved slice trap 块缺 terminator**（`src/codegen/resolved/mod.rs`
  `emit_bounds_trap`）：slice OOB trap 块以 noreturn call 结尾但无 terminator
  → LLVM verify 拒绝整个函数 → **静默降级 legacy emitter**（连带 print
  dispatch 错乱）。补 `build_unreachable`；`sort(data)` + `data[1..4]` 同函数
  复现；
- **修复 2：legacy print dispatch 对 list 返回内建的类型推断缺口**
  （`src/codegen/expr.rs` + `func.rs` + `block.rs`）：`map`/`filter`（编译期
  内建）与 `reverse`/`sort`/`range`（无 func_defs 条目的运行时 builtin）的
  调用结果被推断为 **被调名**（"map"、"reverse"…）或空 → `{i64 len, ptr data}`
  list struct 误入字符串快速路径（printf 对结构体字节 strlen，输出
  空/垃圾，O0 下可触发 double free）。新增共享 helper
  `infer_list_builtin_return_type`（从源参数推导 List<T>；map 优先取 lambda
  声明返回型），接入 `infer_object_type` Call 分支 + 两处 let 绑定追踪；
  另补 `SliceExpr` arm（`xs[1..5]` 同源类型）；**zip 不入 helper**：其裸
  {i64,i64} pair 布局与 product-tuple formatter 的 heap-pack 假设不符，
  强类型化会 segfault（已知限制：legacy 下 zip 显示为空，O1 不 crash）；
- **修复 3：list 字面量绑定 local 后 realloc 所有权陈旧**（resolved，
  `src/codegen/resolved/mod.rs`）：`let mut ys = [1,2,3]; push(ys, 4)` 先在
  构造临时 alloca 建表并注册为 buffer 所有者，再值拷贝进 local；push/pop
  的 realloc 更新的是 **local** 槽，注册槽残留 realloc 前旧指针 → scope
  退出 free 旧指针（realloc 搬移时已内部释放）→ tcache double free。
  O1 仅因 SROA 合并两 alloca 侥幸不炸。新增 `emit_list_literal(target)`
  直接构造模式 + Bind 快速路径：字面量直接绑定简单 local 时就地构造，
  注册所有者 = 被 mutator 更新的槽；
- **诊断钩子**：`MIMI_DUMP_MODULE` 提升到 optimize gate 之外（O0 构建
  也可 dump IR，此前 O1-only 位置使默认 debug opt-out 构建不可见）；
- **回归锁**：`real_world_list_intrinsic_display_and_realloc`（sort/slice/
  reverse/map/filter 内联+绑定显示 + push/pop realloc 所有权，run/build
  双后端对等）；临时复现文件 dblfree_min.mimi 已转正删除；
- **验证**：05_lists O0/O1 与 bytecode 输出逐行对等（zip 行除外）；全量
  5292 lib + 29 real_world + cli 套件绿。

### 0.35.10 — dispatch 门禁复测 + 覆盖率曲线（Phase C 收官）

> Phase C 收官：全语料 113 程序（demos + examples + tests/real_world）
> dispatch 复测与覆盖率曲线。报告 `devdocs/v0.35/dispatch-coverage-0.35.10.md`。

- **曲线**：0.9609（0.34.40 门禁建立）→ 0.3027（0.34.42 slice 放开）→
  **0.2735**（0.35.7 strings/collections 模块体进 slice，−0.0292）；
  eligible 202 → 3783 → **3974**（72.65%）；
- **复测**：113 程序全部编译成功（0 emit_failed）；门禁 check 无静默回退
  （0.2735 < 0.3027）；新增程序 json_parser 自动纳入（0.2667）；
- **基线更新**：dispatch-baseline.json 更新至 3974/5470（0.2735）；
- **剩余拆解**：legacy 1496 中 generics/qualified 1331（**89%**）——泛型函数
  进 resolved slice 需单态化/泛型 emit（架构级），**登记 1.x 评估**；
  module file 81（io/fs/net 未 lift 模块，登记 0.35.x 后续）；unsupported
  type/expr 62；actor/nominal 15；
- **验证**：全量 5292 lib 绿 / clippy 零警告 / fmt 干净。

### 0.35.9 — contracts 守卫发射性能切片（Phase C）

> 0.34.41 第二档（resolved 运行时守卫发射）第一个性能回访。结论：
> **守卫成本 < 噪声，零优化空间**。

- **基准**：`benchmarks/contracts.mimi`——`validated_sum(n, lo)` 带双合约
  （requires n>=lo + ensures result>=0），300 万次调用（参数 k%20 防
  LICM 提升、lo 来自 argv[1] 防常量折叠）；`plain_sum` 同构无合约对照；
- **数据（O1，3 次中位数）**：擦除 0.02s vs `--verify-contracts` 0.02s——
  守卫被内联 + 分支预测近全命中，双向无差异；
- **IR 验证**：requires/ensures 的 icmp + contract_pass/fail 双块 +
  mimi_runtime_abort（E0808）完整存活，与 0.34.41 设计一致；循环内 checked
  add 的 trap_overflow + branch_weights（0.35.4）与守卫共存；
- **登记**：无优化空间（重守卫 = 用户表达式成本，非机制开销，不预优化）；
  报告 `devdocs/v0.35/contracts-slice-0.35.9.md`；
- **验证**：全量 5292 lib 绿 / clippy 零警告 / fmt 干净。

### 0.35.8 — fails transition Result 布局对齐（Phase C）

> dx-backlog #20：flow_order_system native SIGSEGV（`puts(0x1)` 整数当字符串
> 指针，gdb 实证）。fails transition 返回 Result<Target,(Source,E)> 中 string
> 字段布局错位——0.34.25a 只修了 Err 臂（Q1），Ok 臂 + 事件参数路径未修。
> 两个独立根因，双双修复。

- **修复 1 — Ok payload coach load**（func.rs `coerce_field_to_type`）：
  `compile_ok_constructor` 打包 `{i1, ptr, i64}`（payload 槽存目标地址），
  coerce 到声明布局 `{i1, T, E}` 时把裸指针 store 进 T 槽位——struct payload
  （含 string 字段的 Flow state）地址位被当字段读。新增 ptr→struct 分支：
  build_load 解引用（mimi-string wrap 分支在前保持 C-string 指针语义）；
- **修复 2 — flow transition 字符串字面量参数**（method.rs
  `compile_flow_transition_call`）：字符串字面量参数编译为 raw global-string
  指针，旧代码按参数类型 `{ptr,i64}` 直接 load——把字符串自身字节读成
  {data_ptr, len}（"TXN-42" → data_ptr=0x32342D4E5854）。改为 wrap_c_string
  （非 string struct 参数仍 load）；
- **回归**：flow_order_system + flow_system_trace 从 dual-backend
  known_limitations 移除（两个 SIGSEGV 均修复），纳入双后端套件；
- **验证**：flow_ 365 测试全过；全量 5292 lib 绿 / 15 real_world / 29 cli /
  clippy 零警告 / fmt 干净；flow_order_system native 输出与 VM 逐行一致
  （TXN-42/TRK-001/book/invalid price/0）。

### 0.35.7 — strings/collections 模块体进 resolved slice（Phase C）

> dx-backlog #19：strings/collections 模块体（含 trait impl 方法体）进 resolved
> slice。根因是 str_* builtin 的 {ptr,i64} 值 → runtime-direct ptr 的 coercion 失败
> 拖垮所有调用它们的 stdlib 函数体。顺带修复了三个被真实程序暴露的既有 bug。

- **STRING_ABI_BUILTINS**（resolved/mod.rs）：16 个 str_* builtin 跳过
  runtime-direct 快捷路径，强制走 string emitters（compile_builtin_call）——
  `str_char_at` 等 runtime helper 收 raw C-string ptr，而 resolved 传 Mimi
  string 值 {ptr,i64}，coercion 失败导致每个调用它的 stdlib 函数体 resolved
  编译失败落 legacy（0.34.38 只修了 str_substring 单点）；
- **默认白名单扩展**（eligibility.rs `module_bodies_lifted`）：
  prelude,mymath → +strings,collections。A/B 语料五程序（std_strings /
  std_collections / 06_strings / 05_lists / json_parser）编译 + 运行全过，
  dispatch 统计 eligible 34/45、零 module skip（legacy 11 全为
  generics/qualified）；
- **修复 1 — mimi_runtime_assert 缺失**（runtime/mod.rs）：legacy pattern
  binder（func/pattern.rs `PatternKind::Literal`）早就在调用 `mimi_runtime_assert`
  (bool, ptr)，但 runtime 从未定义该符号——任何带 literal 子模式
  （`Bool(true) =>`）的程序链接失败。补实现（E0801 家族：失败打印 + abort）；
- **修复 2 — 泛型参数名遮蔽用户类型**（func.rs `legacy_param_llvm_type`）：
  `type T` + prelude `eq<T>` 时，legacy 编译泛型骨架把参数 T 解析成用户 enum
  的 struct 布局，`a == b` 报 "eq requires same types"（E0700）。declare_func /
  bind_func_params 统一走新 helper：泛型参数名 → i64 占位（骨架仅满足 legacy
  声明 pass，真实调用走 monomorphize）；
- **修复 3 — literal 子模式 fall-through**（expr/match.rs）：Constructor arm
  只在 tag 匹配时进入，payload literal 比较被推迟到 pattern binding——
  `B(false)` 值落到 `B(true)` arm 时 assert abort 而非落入下一 arm。修复：
  literal 字段比较并入 arm 条件（tag AND payload）；
- **回归测试**：real_world_literal_pattern_fallthrough（双后端，覆盖修复 2/3）；
- **验证**：全量 5292 lib 绿 / 16 real_world / 29 cli / clippy 零警告 / fmt 干净。

### 0.35.6 — 四象限矩阵终测 + 曲线报告（Phase B 收官）

> Phase B（trap 成本消减）终测：链收敛 + cold 权重双項落地后的完整矩阵复测
> 与收敛曲线报告。

- **终测矩阵（O1，2026-08-08）**：dsp 402.1→104.5ms（**1.04× 追平 C -O2，降 74%**）；
  mandelbrot 40.0→19.8ms（1.78×，降 50%）；fib 持平 2.90×（递归调用 + checked
  intrinsic 组合开销，登记 0.1.5 范围外——需 emitter 架构级优化）；O0 全程不变
  （收敛/cold 权重仅 O1 路径）；
- **曲线**：trap 主导项（dsp）成本降 74% ≥ 30% 验收达成；dsp 默认档与 ieee 档
  持平（104.5 vs 103.9ms）——链收敛 + cold 权重达成 ieee_float 全部收益且完整
  保留 trap 语义；**Phase B 关闭**。

### 0.35.5 — nsw/nuw 语义分级评估（裁剪登记，Phase B）

> Phase B 可选工作项评估后裁剪：SD-7/8/9 trap 是语言承诺（E0802/E0801/E0813），
> nsw/nuw 放宽会改变可观测 trap 行为，违反 L1 不变量（0.35.2 已否决）。
> 无代码变更，仅为路线图/预算完整性登记。

### 0.35.4 — trap 分支 cold 权重（trap 成本消减 L2，Phase B）

> 0.1.5 性能主线的收尾项：为全部 trap/Fault 分支附加 branch_weights cold
> metadata，让 LLVM 分支布局优化把 trap 代码移出热路径。检查合并/循环提升
> 经链收敛后无剩余空间（热循环已压到 1 检查/迭代），裁剪登记；CVP pass
> 实测无收益（fib 2.96× vs 2.98× 噪声内）不引入风险面，撤销。

- **mark_cold_trap_branch**（float_chain.rs）：`branch_weights {0,1}` cold 权重
  附加 helper（LLVMGetMDKindIDInContext + metadata_node）；5 处 trap 分支统一
  标记：SD-8 div-zero / MIN÷−1、SD-7 checked add/sub/mul、SD-9 float finiteness
  （legacy check_float_finite + resolved enforce_float_finite 两路）；
- **golden IR 更新 ×14**：branch_weights metadata 入 golden 快照（`!0 =
  !{!"branch_weights", i32 0, i32 1}`）；
- **CVP 评估**：pass 串实验 `default<O1>,correlated-propagation`——fib 的
  checked intrinsic 未被消除（alloca/load 模型下 range 分析不propagate），
  矩阵无收益（fib 47.3 vs 47.4ms 噪声内），按"无收益不引入风险"撤销；
- **检查合并/循环提升裁剪**：L1 链收敛后 dsp/mandelbrot 热循环均为 1 检查/
  迭代（环守卫/比较观察点，语义必需），无合并空间；环守卫不能提升出循环
  （非有限传播需实时捕获）；登记无剩余工作项；
- **验证**：全量 5292 lib 绿 / 15 real_world / 28 cli / clippy 零警告 / fmt 干净；
  矩阵终测 dsp 1.04× / mandelbrot 1.78× / fib 2.90×（O1）。

### 0.35.3 — SD-9 链式末端检查收敛（trap 成本消减 L1，Phase B）

> 0.1.5 性能主线的第一个实现 sprint：链收敛 pass 落地，dsp 追平 C -O2。
> 语义保持——trap 是语言承诺（SD-7/8/9），收敛只移动检查位置不改变可观测行为。

- **float_chain pass**（`src/codegen/float_chain.rs`，O1 管线前 LLVM IR 层）：
  收集全部 SD-9 检查点（`fcmp uno x,x` 特征指令）→ 被检值的所有用户（排除检查
  部件）都是受检 f64/f32 代数 op → 链中继点删除检查；末端/观察点（比较/存储/
  函数参数/返回/phi/不受检 op）保留；无逃逸 alloca store→load 转发入链（其他
  store 仅允许常量初始化）；[零真实消费（dead 结果）保留检查——防止 DCE 丢失
  trap 语义]；fallible 上下文（Fault 吸收）不收敛；
- **性能（四象限复测）**：dsp O1 默认 402.1→108.9ms（**3.7×，1.07× 追平
  C -O2**，较基线 3.97× 下降 73%）；mandelbrot O1 3.64×→1.80×（40.0→19.8ms）；
  fib 持平（无 float 链）；O0 档不变（收敛仅 O1 路径）；dsp 默认档与 ieee 档
  持平（108.9 vs 106.9）——链收敛达成 ieee_float 的全部收益且保留 trap 语义；
- **修复回归**：`ieee_depth_does_not_leak_across_function_boundary`——dead 结果
  的 fmul 检查被误删导致 Inf 偷偷通过（检查分支使 fmul 保持 live，删除后 LLVM
  DCE 掉表达式丢失 trap 语义）；修复为零真实消费 → 保留检查；
- **测试**：`src/tests/float_chain.rs` 探针 ×5（链中非有限末端 trap / dead 结果
  保留检查 / 比较观察点保留 / 有限链双端对等 / ieee 块消费边界）+ 既有
  ieee_depth 回归；全量 **5292 lib** 绿 / 15 real_world / 28 cli / clippy 零警告 /
  fmt 干净；诊断钩子 MIMI_DUMP_MODULE_CONVERGED（pass 后 dump）。

### 0.35.2 — trap 成本分解（perf 数据驱动，Phase A）

> 0.1.5 性能主线的第二步：perf 级分解锁定 SD-9 检查的真实成本结构，
> 完成 trap 语义分级表裁决。纯分析 sprint，零代码变更。
> 报告：`devdocs/v0.35/trap-decomposition-0.35.2.md`。

- **perf 分解（dsp 5×10^7）**：O1 默认 408.7ms vs O1+ieee 106.8ms
  （−73.9%）；指令 38.5→9.1/迭代，分支 4.05→0.015/迭代——**SD-9 检查的
  隐性成本 = 破坏 LLVM 向量化窗口**（每个 f64 binop 后的 finiteness 检查 +
  两路分支横插链中），向量化恢复是主要杠杆（ieee 版分支几近归零）；
- **链式末端检查假设验证成立**：IEEE 754 NaN/Inf 传播性论证——中间结果
  非有限必传播至链末端（NaN ⊛ x = NaN；Inf 参与结果 ∈ {NaN, ±Inf}），
  逆否等价；适用四条件：纯 binop 链 / 中间值未被比较·分支·内存写·调用消费 /
  非 fallible 上下文 / 非 ieee_float 块内；
- **trap 语义分级表裁决**：L1 链式末端检查（resolved+legacy+VM 三面对等）
  0.35.3 实施；L2 trap 分支 cold weight 顺带；L3 常量折叠先行；L4 整数 trap
  维持现状（0.35.1 实证 O1 已消除循环内 checked add）；**L5 nsw/nuw 放宽否决**
  （E0802 是语言承诺，改变可观测 trap 行为违反 L1 不变量）；L6 跨函数链
  识别 0.35.4 评估不承诺。

### 0.35.1 — 性能基线套件 + 四象限矩阵（0.1.5 首 sprint，Phase A）

> 0.1.5 性能主线的第一步：建立可复现基线，锁定 trap 成本分解的第一个靶子。
> 纯增量基建，零行为变更。基线报告 `devdocs/v0.35/perf-baseline-0.35.1.md`。

- **基准套件扩展**：`benchmarks/` 新增 dsp 热循环（一阶低通 5×10^7，dogfood
  M-014 同构场景）+ `dsp_ieee`/`mandelbrot_ieee` 变体（ieee_float 包裹悬浮
  SD-9 finiteness trap）+ `dsp.c`（gcc -O2 对照）/ `dsp.py`（CPython 对照）；
- **quadrant.sh**：四象限矩阵脚本——MIMI_OPT ∈ {0,1} × {默认, ieee_float}
  对 fib/mandelbrot/dsp 计时（纳秒括弧，RUNS=3 中位数）+ MIMI_DUMP_MODULE
  IR dump 静态 trap 调用点计数（trap 发射与优化档无关）；
- **基线矩阵（2026-08-08，32 核机）**：dsp O1 默认 402.1ms（3.97× C -O2）→
  O1+ieee 106.4ms（**1.06× 追平**）；mandelbrot O1 3.64× → ieee 1.88×；
  fib O1 2.88×（O0 3.68×）；
- **关键发现**：SD-9 float finiteness trap 占 dsp O1 默认耗时 **73.5%**
  （295.7ms，每 f64 binop 发射 NaN/Inf 检查）；整数 trap（SD-7）在 O1 下被
  LLVM 循环分析消除（ieee 版整数计数器仍 checked add 却追平 C）——整数
  trap 不是主要靶子；trap 静态计数与动态成本定性吻合（dsp 28→24 /
  mandelbrot 38→30）；
- **0.35.2 输入**：SD-9 链式末端检查假设（f64 代数链中间非有限必传播到
  末端，逐点检查可收敛为末端检查，语义保持）列入验证。

### 收口状态：已知排期外项（登记 0.2 / 1.x）

> 0.1.5 收口时按 §6 条件 4/5/9 明确登记、不放松语义的性能/架构项。

| 项 | 现状 | 根因 / 排期 |
|----|------|------------|
| native dsp O1 ≤1.15× C -O2 | 1.31–1.38× | LLVM 18 LoopUnroll IPC 差距（IPC 0.72 vs 1.00），C1b/C1c 已证不可经自引用 unroll 元数据根治；0.2/1.x 评估 LLVM 升级或 nsw 放宽（需语义裁决） |
| RUN dsp ≤1s | 4.7s（R1 Arc + R3 peephole + R4 builtin 快路径 + R5 LICM 合计 ~1.9×） | 余量在 156-Op 单 match 分发（V2），safe-Rust superinstruction 已到顶；1.x 评估 direct-threading/JIT |
| fib Z3 证明消除（O1 性能） | 未做 | 需 verifier 输出 no_overflow 事实喂 codegen 消除 checked 检查；0.2 |
| generics 单态化 | 未做 | 泛型 emit 需单态化，resolved slice 覆盖受阻；1.x |
| `--test-threads=16` 偶发失败 | 未根治 | 已知测试并发偶发（AGENTS §4.2 注记），CI 用 2 线程不触发；1.x |

## [0.1.4] — 2026-08-08

> **语法冻结 + 语义裁决落地 + 架构冻结（Phase G）**。
> 0.1.4 里程碑：become/stay 删除（ADR-001）、multi-target 稳定 tagged-union ABI
> （ADR-002）、`'a` 删除（ADR-004）、`do` wrapper 删除（关键字 81→80）、and/or/not
> 软关键字化、if let / for 解构、`ieee_float {}`、单向数值强制、View/Mutate 闭合、
> O1 默认优化、诊断契约（diagnostics.md）。Phase G 架构冻结：ADR-005~008 四项正式
> 裁决、resolved dispatch 度量门禁（fallback_rate 0.9609→0.3027）、contracts 与
> stdlib 模块函数体进 resolved slice、view/mutate 借用参数 ABI 对齐、verifier 引擎
> 隔离（E0439）、ABI 布局冻结 + abi_version 握手（native-abi-1 §7/§8）、pre-0.1 更名、
> 0.minor=大版本战略。

### 0.34.46 — 0.1.4 全面查缺补漏（登记面 / 记录面 / 代码面）

> 0.1.4 收尾后对全部开发内容做系统性查缺补漏：登记完整性、记录完整性、
> 代码卫生三面清理。无行为变更（探针验证 + 新增回归锁）。

- **登记面**：dx-backlog 补 #19（strings/collections 模块体 trait 符号路由阻塞，
  0.34.42 裁剪）+ #20（flow_order_system fails transition SIGSEGV，0.34.45）；
  审计台账 §11-#45 回写 ✅（0.34.44 ADR-008 已闭：LSP 缓存键引擎隔离 + 双引擎
  fail-closed）；台账新增 §12.1 移交登记表（8 项无去向 🔴 统一登记轨：§2-#20/
  §4-#39/§4-#43/R-3/R-5/G-3/§1-#11/§13-#73）；
- **记录面**：golden-document §10 补 Phase G 表（0.34.34-45 全量完成状态，此前
  只到 0.34.33）；0.34.5a trivia 化排期行补 ⬜ 推迟 0.1.5 回写；0.34.3 的
  "codegen E0700 登记缺口"实测已闭回写；CHANGELOG 补 0.34.1-23（Phase A–E）
  合并条目（此前只到 0.34.24，发布需完整）；README 双语 0.1.4 行同步 Phase G
  交付 + RC 测试数 4598→5285；
- **代码面**：E0439 注册补全（codes.rs 常量 + describe 表，此前 verifier 硬编码
  未登记）；码集合补 E0431（0.34.10 引入时漏加）+ E0439；5 处 `e2e_asan_*` 裸
  `#[ignore]` 补裁决注释（§13.15 "0 未登记 ignore" 纪律）；if let / for (k,v)
  解构 native codegen 实测已闭（探针 p13 双端对等，main 走 resolved）——补
  dual 回归锁 ×2（dual_if_let_and_tuple_for_destructuring /
  dual_if_let_none_arm_skips）；
- **验证**：全量（含新 dual 测试）绿；clippy + fmt 干净；探针 p13 双端对等。

### 0.34.45 — AF-2 ABI 定稿 + pre-0.1 更名战役 + 0.1.4 RC 复核（G3 硬复核点）

> Phase G 第七个 sprint（收尾）。AF-2 布局冻结定稿 + abi_version 握手登记、
> pre-1.0 → pre-0.1 更名战役、0.minor=大版本战略、Phase G 验收清单全绿。
> **Phase G 至此全部关闭**（AF-1/2/3/4 + 度量门禁 + 三假边界 + 引擎隔离）。

- **① ABI 布局定稿**（ADR-007 落地）：`docs/spec/native-abi-1.md` 新增 §7 布局冻结
  声明（string `{ptr,len}` 无 capacity / list `has_header` 显式标志禁裸读
  `data[-1]` / handle tag 位永驻 / nominal handle slot+generation）+ §7.2
  “布局内解决”约束清单（**A2** 指针往返 loss → handle 化路径禁 ptr↔int；
  **A3** tag 永驻是正式语义；**B10** has_header fail-closed）；
- **② abi_version 握手登记**：`ComponentIdentity.abi_version`（当前值 **1**）与
  本文件布局绑定；1.x 布局变更（胖指针/tag 剥离/capacity）→ 新 version，旧组件
  以旧版本继续加载——**Two-Way Door**（0.2 随 bindgen 回归铁律实施）；
- **③ pre-1.0 → pre-0.1 更名战役**：git 跟踪文件 pre-1.0 **零残留**（grep 门禁：
  `git grep "pre-1.0" -- ':!devdocs' ':!CHANGELOG.md'` = 0）；spec 同步闸
  `check_language_docs.py` 指向 `devdocs/pre-0.1`（修掉 glob 扫空脱闸）；
  docs×6 + README×2 + codes.rs 注释 + devdocs 路径引用全清（CHANGELOG
  历史条目豁免）；
- **④ 0.minor=大版本战略入 AGENTS §13.1.1**：minor 即大版本边界（0.1.x 是 1.0
  的 pre-阶段）、每 minor 冻结点表、breaking 政策（1.0.0 起需 major）、迁移
  注记纪律（无迁移注记的 breaking 禁止合入）；
- **⑤ Phase G 验收**：全门禁复跑通过——lib 全量 5285 绿 / clippy 零警告 / fmt 干净 /
  `check_language_docs.py` 31+31 绿 / dispatch 门禁 0.3027 无回退 / unsafe gate 通过；
  fallback 率曲线收尾（0.9609 → 0.3030 → 0.3027，eligible 212 → 3783）；
- **登记 0.1.5（real_world 存量缺陷，非本次回归）**：`flow_order_system.mimi`
  native 产物 SIGSEGV（`puts(0x1)` 整数当字符串指针）——2026-07-27 `9d4f17f3`
  已登记同族缺陷（string event parameter + fails transition → SIGSEGV，
  VM run 正常）；0.34.44 后 codegen/core 零改动（diff=0）证明存量；
  `flow_system_trace.mimi`（同形态无 fails）PASS 锁定差异面在 fails 路径。
  修复排 0.1.5 codegen 轨；
- **工具**：`dispatch_stat.py` 开头清理容错修复（残留时 rmtree 竞态抛错）。

### 0.34.44 — verifier 引擎隔离治理（AF-3 落地，ADR-008）

> Phase G 第六个 sprint。resolved 主引擎 + 缓存引擎隔离 + 双引擎分歧
> fail-closed；VIR（flow_ast）降级为 math: 通道，退役登记 0.2 轨。

- **引擎身份入缓存键**：`ProofArtifact` 新增 `engine` 字段
  （`ENGINE_FLOW_AST`/`ENGINE_RESOLVED`）；`cache_key()` =
  (semantics, solver, integer_model, **engine**, program_identity)，
  resolved 引擎绑 resolved_ir_hash、flow 引擎绑 vir_hash；`is_compatible`
  含引擎相等——跨引擎证明复用结构上不可能（fail-loud cache miss）；
- **LSP 只读 resolved 引擎缓存**：验证缓存键统一走
  `verification_cache_key`（uri + func + resolved + 语义版本），读写
  同源；旧格式磁盘缓存（无引擎段）永不命中，升级自动失效；
- **双引擎分歧 fail-closed**：`mimi verify` 主路径改用
  `verify_checked_dual`——resolved（主判定）+ flow/VIR 并跑，裁决类
  （Proven/Disproven/Inconclusive/NoOpinion）分歧时取较弱结论 +
  新诊断码 **E0439**（注册 error-codes.md）+ artifact 作废；
  NoOpinion（未尝试证明）不构成分歧；仅 flow 覆盖的义务（如 call-site）
  透传不丢；
- **实测分歧（诚实披露）**：两引擎整数模型不对称——flow 对 i64 强制
  溢出定义性而 resolved 按无界模型证明（i32 方向相反）；存量算术合约
  （examples/validation_contracts.mimi 的 fib/factorial/divide/withdraw）
  现报 E0439 + fail-closed，两引擎整数模型统一登记为 VIR 退役前置
  条件（0.2 轨，diagnostics.md G8）；
- **验证**：回归锁 ×8（缓存键引擎隔离/兼容性/LSP 键形/分歧合并四
  向/NoOpinion 让位/flow-only 透传）+ Z3 端到端 ×3（一致路径、i64/i32
  双向分歧 fail-closed）；全量 5285 绿；clippy 零警告；

### 0.34.43 — view/mutate 借用参数 ABI 对齐进 resolved slice（AF-4 前置 2③）

> Phase G 第五个 sprint，AF-4 三个假边界全部落地。非-self 标量 view/mutate
> 借用参数进 resolved slice，指针 ABI 与 legacy 完全对齐；曲线点
> fallback_rate 0.3030→0.3027（eligible 3781→3783）。

- **ABI 对齐**：`declare_callable` 对非-self 借用参数发 ptr 参数类型（对齐
  legacy `declare_func` 的 `param.borrow.is_some() → ptr`）；`bind_parameters`
  借用参数直用入参指针作 storage（不新建 alloca——callee 存储即 caller
  存储，真引用语义，无写回步骤，对齐 0.34.13）；
- **调用传址**：Call arm 按 callee signature 逐位识别借用参数，实参仅接受
  裸 `Load(place)`（无投影、conversion=Identity）时传 caller 存储地址；
  其余形态 fail-closed per-function 降级 legacy（不静默 ABI 错配）；嵌套
  借用转发（借用参数再传入借用参数）指针透传不重建；
- **切片纪律**：eligibility 删除 0.32.20 拒绝；`require_scalar_type` 继续
  守住 record/List 借用（`mutate Buffer` 等仍留 legacy，待指针投影/间接
  存储支持后评估）；self 接收者保持 value ABI 例外；
- **验证**：探针 p11/p12 双端对等（mutate 写回可见 / view 只读 / 嵌套转发，
  34/34 全函数转正）；dual 回归 ×3（`dual_borrow_mutate_scalar_writeback` /
  `dual_borrow_view_scalar_read` / `dual_borrow_forward_nested`）；全量
  5274 绿（含既有 view_mutate_exec/mutate_field_writeback 条款 6 族）；
  clippy 零警告；A/B 默认模式全语料零回退；

### 0.34.42 — stdlib 模块函数体进 resolved slice：source_id workaround 根因化（AF-4 前置 2②）

> Phase G 第四个 sprint。历史 SIGSEGV（0.32.8 std_strings/multiple_std_modules）
> 当年用 source_id 过滤器整体屏蔽了 3848 个模块函数实例——本 sprint 实测根因
> 并逐 slice 放开。**曲线点：fallback_rate 0.9609 → 0.3030**（eligible
> 212→3781，+3569 转正，AF-4 迄今最大单步覆盖率收益）。

- **根因实测**（与 AF-4 草案预判同框架）：gdb 抓崩在 LLVM
  `LowerExpectIntrinsicPass`；`MIMI_DUMP_MODULE` 导出 IR 发现无终止符残桩函数。
  链路：resolved emitter 发射失败后残桩留在模块 → legacy 重编的 skip 守卫用
  surface `func.name` 查 `resolved_failed_functions`，而失败登记用 catalog
  `qualified_name`（impl 方法两者不同，如 `char_at` vs `string_char_at`）→
  查不到 → 残桩当“已编译”跳过 → 无效 IR 崩 pass pipeline；
- **修复 ×3**：① `compile_subset` 两个失败分支（verify 失败 / emit Err）现场
  清空残桩块（`clear_partial_body` 还原纯声明，符号保留供调用方引用）；
  ② legacy skip 守卫改 `func.name` + LLVM 实际符号名双键查找；③ source_id
  过滤器改白名单门 `module_bodies_lifted`（env `MIMI_RESOLVED_MODULE_BODIES`：
  `=1` 全解除实验模式 / `=csv` 片段覆盖 / 未设=内建白名单 `prelude,mymath`，
  按 `<frag>.mimi` 路径尾匹配防误放）；
- **A/B 证据**（120 程序双模式 build+run 对比）：prelude 零回退 → +mymath
  零回退 → +strings 回退 1（a1_verification 链接 undefined symbol
  `string_char_at`——resolved trait 调用方指向的符号与 legacy 方法体所在
  mangled 符号不同，trait dispatch 符号路由问题，显式裁剪登记 0.1.5）；
  默认模式全语料 equivalent 101/120（其余为两侧同态既有噪音）；
- **曲线纪律**：基线 dispatch-baseline.json 重生（3781/5425，0.3030），
  门禁只降不升；覆盖率曲线单调不降 + 全量零新增失败即验收（允许部分模块，
  strings/collections 登记 0.1.5）；

### 0.34.41 — contracts 进 resolved slice：默认擦除转正（第一档）+ 运行时守卫发射（第二档）（AF-4 前置 2①）

> Phase G 第三个 sprint。安全分档策略：**第一档**让带合约函数在默认路径
> （verify_contracts=false，合约擦除）脱离 legacy；**第二档**把运行时守卫发射
> 接入 resolved emitter，解除 --verify-contracts 的 fail-closed。两档均已落地。

- **第一档落地**：`require_resolved_native_callable_with_source` 加
  `verify_contracts` 门——verify_contracts=false（默认）时带合约函数进 resolved
  （resolved emitter 的 `Contract` arm 已为 no-op，与 legacy 默认擦除对齐）；
  --verify-contracts 时 fail-closed 到 legacy（守卫仍由 legacy 发射，不静默丢守卫）；
  all-or-nothing 路径保守不变（`require_resolved_native_callable` 传 true）；
  传参链：`eligible_function_ids_with_stats`/`resolved_eligible_functions`/compile.rs
  各加 verify_contracts 形参；
- **fallback 率差值报告**：eligible 202→212（+10 带合约函数全量转正），聚合
  fallback_rate 0.9628→0.9609（−0.0018，改进方向）；contracts skip reason 清零；
  基线 dispatch-baseline.json 更新至 212/5425；
- **dual 回归 ×2**：`dual_contract_fn_erased_default_runs_on_resolved`（合约擦除下
  多合约函数交互双端对等，无错值）+ `dual_contract_requires_violation_traps_both_
  backends`（--verify-contracts 违反 requires 双端 E0808 trap 对等）；新增 helper
  `dual_assert_contract_violation`（断言 VM + codegen 都 trap 且含 E0808）；
- **门禁**：全量 5268 lib 绿（+2 新测试）/ clippy -D warnings 绿 / fmt 干净 /
  语料零 SIGSEGV（112 程序编译成功）；
- **第二档落地（resolved 守卫发射）**：`emit_contract_prologue`（入口 old() 快照
  按 Old 节点 NodeId 键控 + requires 声明序断言）+ `emit_ensures_checks`
  （fall-through 与每个早退 Return 两处漏斗，`result` 绑定 lower.rs
  `{owner}/contract-result/local` 伪 local，检查后恢复 frame）+
  `emit_contract_assert`（E0808 abort 消息对齐 legacy scope.rs 格式，BB 名以
  条件 NodeId 去重免计数器）；`collect_old_nodes`/`collect_old_block` 全 variant
  递归收集；`ResolvedFrame.old_snapshots` + `Old` arm 快照加载（无快照时保持
  擦除恒等语义）；eligibility 的 verify_contracts fail-closed gate 删除
  （条件表达式仍由 require_block Contract arm slice 检查，不支持则 per-function
  降级 legacy，不静默丢守卫）；消息中条件文本降级为 span 坐标（resolved IR 无
  surface 渲染器；VM 本身打印的是求值结果值，跨后端文本平等本就不存在）；
- **附带修复存量 L1（第二档测试暴露）**：legacy 早退 Return 六处 ensures 断言
  无 `result` 绑定——`block.rs` compile_block 值/None 臂 + 块表达式路径值/None
  臂 + `actors.rs` 方法体值/None 臂，任何引用 `result` 的 ensures 在这些路径
  直接编译失败（undefined variable 'result'）；抽 `compile_ensures_asserts`
  共享 helper（scope.rs，result alloca + 绑定 + 断言，对齐 func.rs emit_return
  语义）统一六处；
- **第二档验证**：探针矩阵 p1-p6 双端对等（requires 违反 / ensures 违反 /
  早退 Return 违反 / old()+result 通过 / 多合约函数，VM 与 native 逐项一致）；
  dual 回归 ×3（`dual_contract_verify_ensures_old_result_on_resolved` /
  `..._early_return_ensures_violation` / `..._multi_clause_pass`）；全量 5271
  lib 绿；门禁 check 无回退（fallback_rate 0.9609 不变——第二档只动
  verify 路径，默认模式分派第一档已定型）；

### 0.34.40 — resolved dispatch 度量门禁（AF-4 前置 1，纯增量基建）

> Phase G（0.34.39-45）第二个 sprint：进 0.1.5 前把 legacy 受管退役的度量地基钉死——
> fallback 率成为一等指标 + CI 禁静默回退。依据 ADR-005（legacy 受管退役）。

- **MIMI_STAT=1 结构化分派报告**：`DispatchStats`（`src/codegen/resolved/eligibility.rs`）
  收集 eligible/legacy 计数 + skip 原因直方图；`eligible_function_ids_with_stats`
  逐函数记录 skip 类别；`emit_dispatch_stats` 写 JSON 到
  `MIMI_STAT_OUT`（默认 `target/mimi-stat/<src>.json`）；
- **skip reason 规范化**：`normalize_skip_reason` 折叠含 `NodeId(`/`@external:`/
  `ResolvedTypeId(`/`ResolvedCall {` 等不稳定 Debug 输出的 reason 为稳定类别
  （unsupported expression/statement/pattern/type/callee）——避免源哈希跨会话
  漂移导致门禁误报 + JSON 膨胀；
- **基线语料入仓**：`devdocs/v0.34/golden/dispatch-baseline.json`（demos/+examples/+
  tests/real_world/ 全跑）——112 程序编译成功 / 16 跳过，202 eligible / 5425 总函数，
  聚合 fallback_rate=0.9628（符合 AF-4 草案“resolved 覆盖 = entry 顶层无泛型无
  合约标量函数”的现状描述）；
- **门禁脚本**：`scripts/dispatch_stat.py`（generate/check/report）。check 模式对比
  基线禁静默回退率上升（EPSILON=1e-9）；白名单登记制
  （`devdocs/v0.34/golden/dispatch-whitelist.json`，reason 必填，`_` 前缀说明 key
  过滤，同 ignored 测试纪律）；新程序首次纳入不判回退；TMPDIR 指向工作区适配
  sandbox 只读 /tmp；
- **门禁自检三项全过**：无回退→exit 0；篡改静默回退→exit 1（报 `demos/01_basics.mimi:
  0.5000 → 0.9767`）；白名单登记→`[whitelisted]` 放行 exit 0；
- **MIMI_VERBOSE info 行格式冻结**：既有 `info: resolved skip/…` 行格式不变
  （向后兼容承诺，本 sprint 仅新增 stats 收集不改 verbose 输出）；

### 0.34.39 — 架构裁决战役：四项 One-Way Door 升格正式 ADR（纯裁决，零实现）

> Phase G（0.34.39-45）启动 sprint：进 0.1.5（性能主线）前把架构形状钉死——裁决先行，
> 实现分档。依据 `devdocs/v0.34/architecture-freeze-draft.md`（四项深度评估 + 裁决草案
> + §8 版本战略重估）。

- **ADR-005 legacy 发射器受管退役（AF-4）**：推翻“长期保留”旧裁决——退役终态
  （legacy 不再编译函数体，三引擎函数体统一 Resolved IR）+ 四前置（度量门禁/
  假边界消灭/单态化/机制迁移，顺序不可颠倒）+ 回退纪律（回退>0.5% 或 SIGSEGV 即
  回滚）；实施 0.34.40-43 启动，终态 0.2 轨；
- **ADR-006 codegen 内存所有权账本（AF-1）**：编译期所有权账本（复用 CFG +
  ResourceAnalysis，与线性能力 exactly-once 同一套机器）——字面量非 owned 不入账本、
  builtin returns_owned 目录、per-turn 固定容量池分配策略优先评估（QP/C 参照）；
  实施 0.2 轨；
- **ADR-007 值表示 ABI 布局冻结（AF-2）**：冻结当前 handle/`{ptr,len}`/has_header
  布局 + Component IR abi_version 握手做 Two-Way Door；0.34.45 定稿文档 + 0.2 实施；
- **ADR-008 Verifier 引擎收敛（AF-3）**：resolved 主引擎 + VIR 降级 math: 通道 +
  缓存引擎隔离（键=(program_hash, engine, clause)，分歧 fail-closed 新诊断码）；
  实施 0.34.44；
- **版本战略**：0.minor=大版本（Zig 式，每 minor 边界即冻结点 + 允许 breaking +
  必附迁移注记）；AF-1/AF-4 前置 3/4 重分类 0.2 轨；pre-1.0/ → pre-0.1/ 更名随
  0.34.45 落地；草案标“已升格”归档，README §4 裁决表补行 ADR-005~008；

### 0.34.38 — 字符串参数守卫战役 + 2026-08-07 大项清扫 + R-2 确定性选择

> 依据台账 `devdocs/audit-unfixed-2026-08-05.md` §14（字符串参数守卫战役）+
> §12 结构性项。战役动机：LLVM 层 List 值以原始指针传递（List = ptr to `{i64,ptr}`），
> 与字符串指针在 emitter 内不可区分——宽松 builtin 参数不受 checker 约束导致
> `str_trim([1,2,3])` strlen 一个 List struct → 垃圾输出 / `into_pointer_value()`
> panic / abort(核心转储)。本次系统性消除该族。全量 lib 5263 → 5273 全绿。

- **string 家族编译期守卫（`7c5574ed` + `1fde0249`）**：表驱动
  `string_only_builtin_string_args` 按参数位守卫（str_repeat/str_char_at/str_parse_int/
  str_parse_float/string_to_int/str_to_c_str 位0、str_split/starts_with/ends_with/
  index_of/count_substring/regex_match/regex_find/regex_find_all 位0,1、str_replace/
  regex_replace 位0,1,2、str_join 位1）；`is_definitely_not_string` 保守判定未知
  类型放行防误拒；emitter 布局检查防 panic。**str_contains/starts_with/ends_with/
  regex_match 返回 i1(bool) 修 L1 分歧**（checker 推断 bool、VM 返回 bool、codegen
  此前 zext i64 打印 1 vs true）；
- **contains 多态接收者（`827a2764`）**：全局 `contains("hello","ell")` 曾
  SIGSEGV（compile_contains 把 string 当 List 指针 load_list_len）；string 干草堆
  重定向 str_contains + needle 守卫；compile_contains 返回 i1；resolved 字符串
  方法映射（`builtin.method.string.X` → str_X，消除 E0709）；
- **json/crypto 家族守卫（`b137e088`）**：sha256/base64_encode/base64_decode/
  from_json/json_is_valid/json_array_length/json_get_element（位0）、json_get_string/
  json_get_int/json_has_key（位0,1）；消除 List 参数 abort(exit 134)；
- **fs/env/path/exec 守卫（`0771be95` + `26ca5af5`）**：read_file/write_file/
  path_join/set_env 等 List 参数编译期拒绝；bool 谓词返回 i1 修 L1 分歧；
  exec_safe varargs 守卫——List 参数静默成为垃圾 argv 修复；
- **network + lexer 守卫（`0ebf8a04` + `c9fbad3b`）**：http_get/http_post/send/
  connect 字符串参数守卫；lexer 内建字符串参数守卫；
- **str_contains List/Set 干草堆路由（`c1e2300f` + `9e04bf94` + `cff7c640`）**：
  List 干草堆路由 compile_contains、Set 干草堆路由 mimi_set_contains、函数形式
  `contains(Set, x)` 同路由（消除 VM-only gap）；type_name(x) 修复 Located 解包 +
  返回规范 string struct；
- **§6-#65 variant 名子串错配（`508147db`）**：lookup_variant_name fallback 改
  精确后缀（`variant.Errors` 不再误配 Err）+ §4-#44 AST 重复条目删除 + enum
  variant type_name 登记；
- **§7-#81 fn pointer 返回 ABI 错配（`b6d1730d`）**：间接调用返回类型从
  var_types 恢复（closure_return_llvm_type 同款），f64 不再读 %rax 垃圾；
- **D-5 substring strict 越界（`05f354f7`）**：方法形路由新 builtin
  str_substring_strict（mimi_str_substring strict runtime），函数形保持 clamp；
  resolved 直连 ABI 桥修复（STRING_ABI_BUILTINS 跳过 runtime 直连走 emitter，
  E0722 消除）；
- **2026-08-07 大项清扫（`5e30cf8a`）**：§11-#37 Z3 命名空间残留清扫（call_var_key
  歧义拼接改 `#` 分隔 + `{p}.len`/`{p}.ne` 派生常量改点分隔，消跨调用别名）；
  R-1/V-11 嵌套函数遮蔽 lowering 歧义（shadowing_nested_function 镜像 checker
  声明序裸名注册 + codegen 遮蔽符号重定向 + 帧守卫，LLVM ERROR 消除，#[ignore]
  已解）；§4-#41 module/actor NodeId 碰撞（collect_item_decls 补 module path）；
  B-7 print_err auto-deref（builtin_eprintln 改走 io::print_display）；to_int/
  to_float 消息差异（静态已知聚合类型以 VM 对齐消息 `[E0800] cannot convert
  this type` 编译期拒绝）；测试基建：audit_fix_io 两条 VM input EOF 测试加
  IsTerminal 守卫（tty 环境读真实 stdin 永久阻塞曾冻结 audit_fix 运行 1.5h）；
- **R-2 构造器模式确定性（`0b909de6`）**：flow_variant 查找提取为
  `pick_matching_record_def`（字典序最小 variant id 确定性选择，注释钉死 R-2），
  消除 `HashMap::iter().find()` 非确定遍历 + 缺失 field 捏造；
  `r2_pick_matching_record_def_is_deterministic` 回归（50 次 fresh-HashMap 断言
  稳定 + 全名匹配 + 非 record/未知名不匹配）；

### 0.34.37 — 显示截断族修复：legacy 显示固定缓冲 → 精确尺寸组装

> §8-#96/D-4 固定缓冲显示截断族全闭（台账 `devdocs/audit-unfixed-2026-08-05.md`）。
> 实证复现：VM 404/305/606 字符 vs legacy 255/255/511（256B/512B/固定缓冲−1），静默数据丢失。
> 全量 lib 测试 5260 → 5263 全绿（+3 回归）。

- **显示四族固定缓冲 → sized_cat_parts 精确组装**：enum 128B snprintf（emit_enum_display）、
  record est-size snprintf（emit_record_display）、Result 256B snprintf（emit_result_to_string_typed）、
  Option 512B snprintf（emit_option_to_string）全部改为两遍 strlen 精确 malloc + memcpy 组装，
  统一 out_slot+merge 注册 + 臂内局部 display_marker 生命周期模式（共享前置 marker 会跨臂
  污染并释放 merge 注册的 wrap → UAF）；长 payload 双端字节一致（404/305/606 不再截断）；
- **预存崩溃 bug 顺手闭合**：`Result<string,string>` 的 Err(string) 槽 legacy 存裸数据指针
  （运行时字符串，如 str_repeat）或 {ptr,len} 结构指针（字面量）——结构解码对数据指针
  valence 把 payload 字节当指针 strlen → SIGSEGV（0.34.36 HEAD 同崩，非本轮引入）。
  str_bb err 分支改双通道：mincore 探测 field0 可读性（新 runtime helper
  `mimi_runtime_ptr_readable`）区分两条构造路径，字面量与运行时字符串均正确显示；
- **D-4 复核闭合（台账过时）**：List 显示族已无固定缓冲（runtime 动态分配 +
  sized_cat_parts），List<string> 双 500 字符探针 1005 字节完整；
- **回归测试**：audit_896 ×3（Result/Option 长 payload 双端全量字节断言 + enum/record
  长字段 + 短路径臂精确字节）；

### 0.34.36 — 审计台账收尾战役：audit-unfixed-2026-08-05 收尾包 A–F 全闭

> 依据 `devdocs/audit-unfixed-2026-08-05.md`（单一事实源）。纪律：冻结期不加新语法，
> 只让报错说真话（fail-closed / fail-loud），每项修复带回归测试。全量 lib 测试
> 5228 → 5260 全绿（+7 登记 ignored 不变）。

- **收尾包 B（checker/codegen/VM）**：K-4 数值强制双门禁复核闭合（防回归测试）；
  K-5 resolved Drop 对 Capability place 从静默 no-op 改 fail-closed（E0830）；
  H-9 match 落空分支发射 `Op::NonExhaustiveMatch`（E0805，与 codegen 对齐）；
- **收尾包 A/C**：§2-#19 bound-generic trait 方法分发诚实拒绝（新诊断码 E0437，
  单态化延 1.x，Clone 特化保留）；§11-#46/47/48/50 verifier 四项——i64 算术溢出
  definedness 义务、f64 let 绑定编码、match 无约束 fallback、Unknown→Timeout 分流；
- **收尾包 E（诊断卫生/CLI）**：§13-#67 `--verify-contracts` 真接线 + `--allocator`
  非 system 拒绝 + `--verify-ffi` VM 未实现警告披露；X-5 loader is_dep 子串判定改
  结构化（stdlib_dir 前缀 + [".mimi","deps"] 组件窗口）；E0830 入 Diagnostic.code；
  E0231→E0438 拆分、E0422→W0402 降级（retired 码不复用）；§1-#10 f-string 插值
  双层引号感知扫描；
- **收尾包 D（runtime 安全）**：§10-#27 ReDoS 时间预算——`REGEX_MAX_WORK`（1M 步/次）
  贯穿两个回溯引擎全 5 入口，耗尽失败 + 每进程一次警告；§10-#31 product 序列化器
  4 处裸 `from_raw_parts` + list 头解引用改 mincore 探针（不可读句柄零值 + 警告，
  不再 SIGSEGV）；§10-#35 `mimi_map_destroy` 回收 map 自持 value 缓冲（Pack/
  ListOfPacks 形状登记，balance 归零可观测）；§10-#22 mailbox 动态化裁决延 1.x
  （dispatch ABI 契约变更）；
- **收尾包 F（语义/工具链）**：V-7 pow 类型契约裁决——checker 按实参推 pow
  （int×int→i64），codegen 删 sitofp 强制转换，pow(2,60) 双端精确渲染；
  `__mimi_pow_i64` exp>u32::MAX 双端回归补齐；stress 套件改双向契约（守卫内必过 +
  越界必响亮报 recursion limit）；nextest 幽灵键 `test-timeout` 清除（被 0.9.140
  静默忽略，硬上限由 slow-timeout period×terminate-after 承载）；stdlib JSON 与
  serde 语义对齐（1e999 f64 溢出拒绝，双端等价）。

### 0.34.35 — FFI 审计闭环：repr(C) 导出 SysV ABI 修复 + fn 字段调用 L1 修复

> 依据 2026-08-05 Jupitune dogfood 外部评估审计（`devdocs/v0.34/dogfood-jupitune-eval-0.34.34.md`，
> 17 条复核 14 成立 + 3 个审计新发现）。本 sprint 处置 L1/L2/L3 级阻断 bug；
> 特性缺口（f32/指针/数据符号/vtable）登记 0.2 不进本版。
> **0.34.35b/c 收尾（2026-08-07 随 0.34.36 战役落地）**：M-001 extern 符号
> 真实命名 + M-006 dlopen ABI 测试族 + M-011③ 直接调用诚实拒绝 + N-3 警告
> 洪水 + M-003/016/017 文档统一，见下。

- **N-2｜fn 字段调用 codegen 静默误编译修复（L1）**：裸函数引用存入 closure 型
  record 字段（`type VTable { add: func(...) }`）时，codegen 把 8 字节 fn 指针存进
  16 字节 `{fn_ptr, env_ptr}` 槽、env 半初始化，调用端把垃圾 env 当首参注入——VM
  结果对、codegen 静默错值（`f(1,2)` 返回 255）。修复（`codegen/expr/record.rs`）：
  静态具名 callee 走 `{closure_wrapper, null}`；运行时 fn 指针（如变量持有）走
  **签名键 trampoline**（callee 坐 env 槽、trampoline 以声明签名间接调用，无 env
  注入）。真 closure（lambda）不受影响。新增 5 个 dual 对齐测试（含捕获 lambda 回归）；
- **M-010/N-1｜repr(C) 导出 SysV ABI 修复（L3 内存安全）**：`extern "C"` 导出函数
  的 repr(C) struct 按值传递此前违反 SysV——`is_simple_reprc_record` 仅 ≤2 全 i32
  走寄存器、其余一律当指针，`{i64}`/`{i64,i64}` 参数把寄存器值当指针解引用 →
  C 侧 dlopen 调用 SIGSEGV，≥24B 返回垃圾，debug 编译器对部分形状直接段错误。
  根因双层（`codegen/func/export.rs`）：
  ① ABI 侧实现 **SysV eightbyte 分类/coercion**——≤16B 按 8 字节分类（INTEGER→i64、
  SSE→double）用 coerce 寄存类型穿越边界（LLVM 原生 struct 参数不做 SysV 合并，
  实测 `{i32,i32}` 被拆到 edi+esi 而 C 调用方打包进 rdi）；>16B 参数加 `byval`
  （SysV 栈传递，裸 ptr 读 rdi 是垃圾）、>16B 返回加 `sret` 隐藏缓冲。
  ② IR 侧把 `const_named_struct` 喂运行时 SSA 的用法全部改 `insertvalue` 链——
  前者产出"伪常量"IR，独立 `opt` 报 `invalid use of function-local name`，
  LLVM-18 优化器在 LazyCallGraph/InstCombine 处段错误（即 debug 编译器崩溃根因）。
  dlopen 探针 5 参数形状 + 2 返回形状 + 混合 SSE 全通过；顺带消除旧堆指针返回的
  per-call malloc 泄漏；
- **测试纪律：nextest 每测试硬超时**（`.config/nextest.toml`）：每测试独立进程 +
  60s 硬上限（slow-timeout 30s 标黄两次即杀）+ leak 检测。动机：全量 `cargo test` 遇
  死锁/死循环会无声挂起只能肉眼盯进度；独立进程使段错误/死锁被单独报失败而非拖死
  全场。default 60s / ci profile 120s（对齐 2 vCPU runner）。全量 nextest 复跑绿：
  4623 passed / 0 failed / 7 skipped。（0.34.36 补记：原 `test-timeout` 键为
  nextest 不识别的幽灵键，硬上限实际由 slow-timeout 承载，已清理。）
- **0.34.35b｜M-001 extern 符号真实命名（L1 分裂）**：extern 符号默认直接使用
  声明名（`func strlen` 链接 C 库的 `strlen`，与 VM 侧 `lib.get(name)` 一致），
  `__mimi_extern_` 前缀机制移除；内部测试桩经显式完整符号名保留
  （`__mimi_extern_test_*`）。LLVM 侧先查模块复用同名兼容符号（签名不兼容
  → fail-loud 诚实拒绝，不 mangle 成连不上的名字）；`demos/14_ffi.mimi`
  端到端链接真实 libc 可跑；
- **0.34.35b｜M-006 dlopen ABI 测试族**：`--shared` + C dlopen 往返探针常规化——
  编译 mimi 共享库 → C 探针 dlopen/dlsym 调用导出函数 → 校验返回值（
  `build_shared.rs` `dlopen_roundtrip` helper，8B/16B/24B/混合 SSE 形状矩阵 +
  f32 缺位负测试）；
- **0.34.35b｜M-011③ fn 字段直接调用诚实拒绝**：`vt.add(1, 2)` 字段直接调用
  此前 E0207 吞参静默错值；现 E0223 明确诊断
  （"field 'add' of 'VTable' is a function value and cannot be invoked directly
  on the record"）+ help 指导（bind 后调用），audit_fix_checker 回归绿；
- **0.34.35c｜N-3 component-ir 警告洪水闭合**：四个 trap 运行时函数
  （mimi_trap_overflow/div_by_zero/div_overflow/float_not_finite）登记 Component
  IR 注册表（component/gen.rs），构建不再刷屏
  `get_runtime_fn("mimi_trap_*") not in Component IR registry`；
- **0.34.35c｜M-003/016/017 FFI 文档矛盾统一**：ffi-guide §4 i32 宽度改正
  （int32_t，与 ffi-type-mapping 一致）；`mimi run` 非零退出码回显
  （`-> <exit_code>`）行为文档化（readme/07-cli.md）；spec §7.4 ffi
  slice/buffer 标注"未实现（0.2 评估）"；devdocs ffi-1.0-surface-eval §5
  已就绪清单更正注记（M-010/N-1 实证为假）；

### 0.34.34 — i32 算术语义钉死：SD-7 trap 对等 + O1 毒值修复（L1 双后端等价闭环）

> 用户报告 i32 标注算术 L1 分歧（VM 以 i64 宽度静默 wrap / codegen E0802 trap），
> 顺藤摸出两处 O1 毒值。本 sprint 将 i32 运算语义在双后端钉到同一裁决上：
> **算术溢出 = trap（SD-7）；收窄绑定 = trap；显式 cast = wrap；界外移位 = 硬件掩码**。

- **VM i32 宽度保真**：bytecode compiler 新增 `VarType::Int32`（let 声明的标量
  标注**优先于**类型推断），新操作码 `CheckI32` / `CheckI32DivRem` / `WrapI32` /
  `MaskShiftAmt`；binop/let/assign/neg 全部带 i32 守卫；`as i32` cast 走 trunc+sext
  wrap 语义。trap 消息与 codegen 对齐（`integer division overflow (MIN / -1)`、
  `integer overflow in power`）。此前 VM 把 i32 值当 i64 全程运算，`i32::MAX + 1`
  静默返回 2147483648——现已按 E0802 trap；
- **双后端 SD-7 trap 对等**：i32 add/sub/mul、div/mod（MIN ÷ -1）、一元负号
  （`-i32::MIN`）在 VM 与 codegen 上均 E0802 trap、消息一致；
- **收窄绑定守卫（narrowing bind/assign trap）**：向 `i32` 注解槽位绑定/赋值一个
  越界宽值是 E0802 overflow，不再静默 trunc。五个编译点全覆盖——legacy 顶层函数体
  `compile_func_body`、嵌套块 `compile_block` / 取值块 `compile_block_last_val`、
  legacy 赋值 `assign_to_var`、resolved emitter `bind_pattern` + `Assign`
  （NumericNarrowChecked before-trunc 守卫）。配套根因修复：`Type::Located` 包装
  导致注解直接匹配 `Type::Name` **永远不命中**——新增 `annotated_type_name`
  pierce helper（此 bug 意味着既有一切按注解名的 legacy 守卫均有同款盲区）；
  显式 `as` cast 保持 wrap（不改语义）；
- **O1 移位毒值修复（SD-7 附带裁决）**：LLVM 未掩码移位为 poison——O0 下硬件指令
  自行掩码看似无害，O1 常量折叠 `1 << 65` → poison 毒值泄漏。裁决：**界外移位保持
  O0 可观察的硬件掩码语义**（`1<<65`→2、`1<<-1`→i64::MIN），在全部优化级别确定一致：
  codegen 移位前显式 AND 掩码（宽度-1），VM `Shl`/`Shr` 从 trap 改为 mod-64 掩码；
  `v1_2_core_edge` 两个旧 trap 预期测试改写为掩码语义断言；
- **O1 physreg crash 修复**：multi-target transition 函数在已终结块后被
  `emit_implicit_return` 追加游离 `ret`（无效 IR → `LLVM ERROR: Cannot emit
  physreg copy instruction`）——追加前检查 `block_has_terminator()`；
- **O1 优化改为默认**：`MIMI_OPT` 语义反转为 opt-out（`MIMI_OPT=0/false` 关闭，
  未设置或 1/true 保持 O1）。0.31.21 已修复 O1 codegen 两 bug（try_expr i32-vs-i1、
  extern wrapper 名冲突），本 sprint 4618 全量 + golden + CLI 冒烟矩阵在 O1 默认
  下全绿，满足放行基线；
- **诊断输出契约**：新增 `docs/diagnostics.md`（normative 文法：单行致密
  `SEVERITY[CODE] LOCATION MESSAGE | field:...`，机器/AI 优先，caret/gutter 装饰
  由坐标区间无损替代）；runtime `mimi_trap_overflow` / `mimi_trap_float_not_finite`
  的 Hint 行并入单行 `| hint:` 字段；`docs/error-codes.md` 登记契约引用；
- **测试**：`dual_backend.rs` 新增 12 个 i32/i64/cast 对等测试（trap 对等 +
  shl 掩码 + pow wrap + const-fold 收窄 + cast wrap）；golden IR 4 件重生成
  （recursive_fib/pipeline/mutual_recursion/try_operator——收窄守卫的 icmp/br
  插入，合法程序守卫恒通过，无行为变化）。全量 4618 passed / 0 failed / 7 ignored，
  CLI 冒烟矩阵（trap 对 / shl / cast / 收窄 / fib 不误杀）在 O0（MIMI_OPT=0）与
  O1 默认下全部一致。

### 0.34.33 — 审计收尾：文档同步残留闭环 + 门禁加深（0.1.4 深度审计修复）

> 依据 2026-08-04 0.1.4 全量深度审计（对照 pre-1.0/05 RC 门禁 + v0.34 验收标准，
> 全门禁实测：fmt/clippy/check_unsafe_safety/4598 lib + 13 real_world + 28 cli 全绿）。
> 审计结论：工程侧与代码实现全部达标；0.34.28 同步战役残留 3 处 P0 + 多处 P1/P2，
> 本 sprint 全部闭环，不留尾巴。

- **外围文档族普查闭环（第二轮）**：
  - **README.md / README.zh.md**：Hello Flow 主示例 `do { return }` 迁移为裸
    `return`（0.34.27 删除语法不再教给用户）；特性表 multi-target 与 View/Mutate
    由 📋 升 ✅（均已落地）；Progressive mode 注记"真 lowering post-1.0"纠正为
    已实现的语义脱糖（spec §3.13）；0.1.4-dev 版本行重写（trivia 化误称为 0.1.4
    成果 → 明示登记 0.1.5，补齐 do 删除/软关键字/ieee_float/数值强制等实况）；
  - **docs/spec/transition-turn.md**（normative profile）：Terminal outcome 分类
    `Become`/`Stay` → 统一 `Commit(target, payload)`（ADR-001 术语对齐）；Turn 配置
    移除已废止的 `transaction_log`（修正案条款 3 WAL）；multi-target 条款升 stable 表述；
  - **docs/ast-appendix.md**：快照版本刷新（v0.30.0 → 0.1.4-dev）；Stmt 表 `Do`/
    `Delegate` 改 absent（variant 已删）、`Math` 改 verifier 通道描述（0.34.28 裁决）、
    补 `IfLet`；Expr variant 行纠正（31→34，删已不存在的 `Range`）；§2.1 Progressive
    maturity partial→complete（0.34.28 实测）、§2.2 N×M 自动补全 partial→repealed
    （0.34.18b 稀疏图 + E0211）；表格措辞遵守该文件"禁状态词汇"既有门禁；
  - **readme/ 教程族**：00-index/01-syntax/llmprompt 加 MimiSpec 时代快照状态 banner；
    01-syntax §2 与 llmprompt §2.4 关键字表**整体重写为冻结实况**——与
    keywords.rs:92-177 程序化比对 **80/80 精确零差**（删 steps/ui/binds/on 等已失效词，
    补 flow/state/transition/session/dual/end 等全部现行词）；llmprompt 版本行
    （v0.7.0 时代）加诚实化注记；
  - **gramma/mimi-grammar-v1.0.0-rc.md**：加"不再权威"状态 banner，纠正过时的
    MimiSpec 保留字声明（flow 已是 Mimi 关键字）与权威性措辞；
  - **门禁再加深**：`check_language_docs.py` 语义新鲜度扫描面扩展至 README/README.zh/
    docs/spec/*.md/docs/ast-appendix.md；REMOVED_MARKERS 补 `removal`。
- **核心文档族闭环（第一轮）**：P0×3（spec §6.12 math/multi-target 清单 +
  requirements FLOW-TURN-001）、pre-1.0 九份源头、support.toml 三条 evidence、
  syntax-reference 真再生成、await 撤销回写、golden 记账、测试卫生、根部垃圾清理。
- **CI/CD 流水线深度完善**：
  - **致命缺陷×2 修复**：release.yml 环境变量 `LLVM_SYS_180_PREFIX` → `181`
    （llvm-sys 181.3.0，旧名导致发布构建找不到 LLVM）；tag 触发器 `mimi-v*` →
    纯 semver `[0-9]+.[0-9]+.[0-9]+`（AGENTS §15.1 已弃用 mimi-v* 前缀，旧触发器
    导致 0.1.x tag 永不出包），mimi-v* 保留兼容匹配；
  - **新增 composite action `.github/actions/setup-mimi/`**：LLVM 18（apt-key 弃用 →
    keyring）+ wrapper + z3/ffi/binutils + toolchain + cache 收敛为单一事实源，
    lint/test/release 三处 ~40 行复制安装脚本退役（此前已漂移：release 缺 binutils）；
  - **门禁补齐**：`check_language_docs.py`（Spec 同步闸 + 0.34.33 语义新鲜度探针）
    进入 lint job；`cargo test -- --ignored` 收编为 advisory 门禁（continue-on-error，
    实测无 ASan/Valgrind 工具链时 6/7 失败——allow-fail 是必需而非防御）；
  - **稳定性防护**：全量测试步骤 `ulimit -v 20000000`（超限 kill 不冻 runner；
    实测全量峰值 RSS ~307MB，§3 的 12GB 为旧时代数据）；Z3 验证套件单线程隔离步骤；
    全部 job 补 timeout-minutes；顶层 concurrency 取消同分支旧运行；
  - **发布加固**：release 前 `mimi check` 冒烟、产物补 sha256sum、发布说明从
    CHANGELOG 提取当前版本小节（缺失回退全文，旧配置整文件灌入每个 release body）；
  - **Makefile `ci-check` 假门禁修复**：clippy/fmt 的 `2>/dev/null || true` 吞失败
    移除，补 language-docs/unsafe 门禁，与 CI 对齐；
  - AGENTS §3 内存警告补实测数据注记、§15.3 门禁清单重写为新流水线实况。
- **门禁结果**：fmt/clippy/unsafe gate/check_language_docs 全绿；Rust 代码本轮零改动，
  测试状态与 0.34.32 RC 复核一致（4598 lib + 13 real_world + 28 cli）。

- **P0 闭环（spec 同步闸违规项）**：
  - spec §6.12：`math` 移出 Removed 清单（§5.6/§6.8 已标 `[stable]`，清单漏改）；
    multi-target 与 math 升入 Stable 清单（multi-target 移出 Experimental 后未并入）；
  - `docs/language-requirements.toml` FLOW-TURN-001：become/stay 现行时态描述
    改为 `return State {}` 唯一终止符（ADR-001 0.34.11）——同步战役漏掉的规范索引文件；
  - spec 头部差异登记表四条全部闭环（`|>` 条目补 ✅）。
- **P1 闭环**：pre-1.0 源头九份文档残留（04 multi-target experimental 位置性矛盾×2、
  04/03 math 移除正文 superseded 注、04 Removed 清单 ADR-003 保留项、AGENTS §0 注记 +
  坐标漂移 850→883 修正——本地文档族）；support.toml 三条 evidence 刷新
  （FLOW-PROGRESSIVE-001 implicit main 实测纠正、SYNTAX-REMOVED-001 刷新至 0.34.27、
  FLOW-MULTI-001 partial 维度澄清）；`docs/syntax-reference.md` 从 golden 真正重新生成
  （关键字 81→80、软关键字枚举补 and/or/not、§12.2 残留与自指差异表删除，
  正文与 golden 逐字节一致）；golden §1.3 关键字 box 算术修正（补 and/or/not、删误列 i32）。
- **P2 闭环**：裁决回写断链——await 删除裁决撤销标注回写（feature-decision-0.34.23、
  golden §10 0.34.23 行、v0.34 README Phase F 行）；golden §10 记账修正
  （0.34.1/0.34.18 盖章、0.34.9 计数 30→31、0.34.19 soft 计数 1→3）；
  dx-backlog 补登记 #13（runs_flow E0221 三层集成）；测试卫生
  （`multi_target_incompatible_payload_layout_rejected` 改名 `_accepted_adr002` 名实相符 +
  `flow_turn_become_multi_target` 陈旧 E0226 TODO 删除并补 checker E0420 fail-closed 护栏）；
  根部垃圾清理（13 个零字节 test_physreg_*.o + 零引用 scratch_lexer.mimi）；
  0.1.5 zonk 迁移脚手架（`zonk_or_unknown`，dx-backlog #1）收纳提交。
- **门禁加深（防同类回归）**：`scripts/check_language_docs.py` 新增语义新鲜度检查——
  become/stay/do 现行时态引用、multi-target experimental 降级、math `[removed]` 标注
  行级探针（同行裁决标记豁免历史引用）+ spec §6.12 结构引脚 + 关键字计数漂移引脚 +
  FLOW-MULTI-001 target=stable 引脚。实测 5/5 历史违规复现触发、6/6 合法引用零误报。
- **门禁结果**：fmt/clippy `-D warnings`/check_unsafe_safety（0 violations）/
  check_language_docs（31/31 + 新鲜度）全绿；4598 lib（0 failed / 7 ignored）+
  13 real_world + 28 cli 全绿。0.1.4 保持 `0.1.4-dev`（tag 按用户裁决暂缓）。

### 0.34.32 — 0.1.4 RC 复核 + 工具门禁（Phase F 收尾）

- **unsafe SAFETY gate 清零（61 → 0）**，CI lint 门禁恢复全绿：
  - `scripts/check_unsafe_safety.py` 增强：
    - **raw string 剥离**：mimi 测试源码以 `r#"..."#` 嵌入 `unsafe { }` 语言关键字块（`unsafe` 是 Mimi 语言特性），此前被误计为 Rust unsafe（6 处误检）；现在先整文件剥离 raw string 再行扫描。
    - **行号保留**：raw string 替换保留换行数，`--list` 输出行号与源文件一致（此前偏小 ~200 行）。
    - **rustfmt 布局兼容**：`unsafe { // SAFETY: ... }` 被 rustfmt 移到块内首行时同样识别。
  - 为 **61 处真实 unsafe 补 `// SAFETY:` 注释**，含 0.33 bytecode 迁移存量：
    - `bytecode/builtins/net.rs` 26 处：libc socket/getsockopt/getaddrinfo/freeaddrinfo/bind/listen/accept/send/recv/connect/dup2/close/`__errno_location` 标准调用，参数经 builtin 校验；
    - `interp/ffi_runtime.rs`：runner 裸指针（`Option<*mut dyn FfiClosureRunner>`）同步调用期有效性、CStr 借用、`Box::into_raw/from_raw` 配对、`call_ffi_raw_struct` 低层入口；
    - `bytecode/builtins/misc.rs`：close_fd、C 侧字符串 CStr 读取 + `libc::free` 配对；
    - `ffi/callback.rs`：回调参数槽 C 字符串释放（IP-C3 allocator match）、`FFI_CALLBACK_CTX` runner 指针；
    - `codegen`：inkwell `build_gep`/`BasicBlock::delete`（LLVM 构建 API）、`errno.rs` `strerror_r`（线程安全 XPG 变体）；
    - `tests/` + `main/run.rs`：测试 harness FFI 回调、`libc::flock` 文件锁、`libc::signal` SIGINT 安装。
  - 门禁结果：**0 non-runtime unsafe without SAFETY**（baseline 37），此后新增 unsafe 必须带 SAFETY。
- **RC 复核**：全量 4598 lib（0 failed / 7 ignored）+ 13 real_world + 28 cli 全绿；clippy `-D warnings` + `fmt --check` 干净。0.1.4 保持 `0.1.4-dev`（用户裁决不 RELEASE）。

### 0.34.31 — 内存模型切片评估（Phase F 之五，诊断闭环）

- **valgrind 实证三类泄漏（codegen 路径）统一根因** = **codegen C-ABI 无托管内存所有权模型**：
  - **to_json 标量缓冲** 512 B/次（`builtins/json.rs` Float/Bool/Int 三路径 `malloc_or_abort(512)` 裸返回，注释 `returned value owns the allocation`，消费端零释放）；
  - **字符串拼接** 51 B/次（`acc = acc + "x"` 每次 concat 分配新缓冲，旧 acc heap 指针覆盖丢弃，独立于 to_json 复现）；
  - **List 容器 box** 24 B/元素（`List<(i32,i32)>` push 时 data buffer 生长 + 元素装箱，List 无析构）。
  - 运行时**已有** `mimi_string_free`（mod.rs:923）等释放原语，但 **codegen 零调用点**——"有 free 设施、无注入策略"。bytecode VM 无泄漏（Rust 所有权），仅 codegen 路径。
- **裁决**：完整修复需方案 A（owned 字符串标记 `{ptr,len,flags}` + 消费端 free 注入点清单：重赋值/concat/println/to_json/FFI 边界）与方案 B（容器析构 glue），跨 checker+codegen+FFI 三端，属 **1.x 内存模型主线**（详细设计见 devdocs/v0.34/memory-model-0.34.31.md §3）。
- **不在 0.1.4 做**：Phase F 是语法冻结 + 语言自洽性战役；任何局部 free 注入都会在字面量（`{ptr,len}` 无法区分 `.rodata` vs 堆）与共享字符串场景触发 UB（L1 双后端大面积回归）。泄漏为**可量化固定量**（非无限增长），不构成 RC 阻断。
- 复现资产：`/tmp/leak_{json,concat,tuples}.mimi` + valgrind（评估文档 §5）。

### 0.34.30 — codegen 缺口补齐（Phase F 之四）

- **项一：LEGACY_CODEGEN 嵌套 list 索引测试转正**。harness 重构：从 `compile_and_run_with_config` 抽出 `link_and_run_module`（legacy/checked 两 harness 共享 object/link/run 逻辑）；新增 `checked_codegen_compile_and_run`（check → `compile_checked` → 链接运行，精确复刻 `mimi build` CLI 管线）。`tricky_nested_loop_list`（v1_4_tricky_interaction.rs）un-ignore 并改走 checked harness——此前被 `#[ignore = "LEGACY_CODEGEN"]` 掩盖（legacy `compile_file` harness 跳过类型检查、嵌套 list 循环索引误编译；CLI 全管线本就正确），现由 resolved dispatch 路径直接门禁。v1_4 31 全绿 0 ignore。
- **项二：trait-impl 方法 resolved emitter legacy 回退消除（dx-backlog #11）**。实测根因：含**非 i64 ok 载荷** Result（如 `Result<f64, string>`）的 trait-impl 方法函数体 resolved 编译失败——legacy `compile_err_constructor` 非 list 路径把 ok-pad 硬编码为 i64（产 `{bool,i64,i64}`），而 `Ok(1.5)` 产 `{bool,double,i64}` → if/else 分支合并时 `numeric_convert` 拒绝 struct→struct → 整个 impl 方法落 legacy → 所有调用方（ProtocolMethod callee）连锁落 legacy（dx#11 原判 "callee undeclared" 实为连锁表象）。
  - 新增 `emit_resolved_optional_ctor`：`Some/None/Ok/Err` 按目标类型 lower 布局构造（`{bool, ok_llvm, i64_err}`），merge/return 类型恒一致；
  - 新增 `resolved_err_to_handle`：err 载荷 → i64 handle（int widen/truncate、ptrtoint、string heap-pack `{ptr,len}`、enum tag），镜像 legacy B4 语义，`?` 算子的 inttoptr+GEP 重建 ABI 不变。
  - CLI 实测 `impl FloatGetter for string { func get(...) -> Result<f64, string> }`：`resolved emitter compiled 2/2 function(s), 0 fell back to legacy`（此前 1/2 fallback）；输出 `Ok(1.5)`/`Err(missing)` 双后端一致。
- **测试**：4598 lib / 0 failed / 7 ignored（6 工具门禁 + 1 Z3 专项，既有不变）；dual_ 830 + v1_4 31 + codegen_e2e 204 全绿；clippy + fmt 干净。

### 0.34.29 — actor 收敛（裁决重估 + SD-5 文档化，Phase F 之三）

- **await 裁决纠正（重要）**：0.34.23 U6 评估 "await 无意义 / interp no-op / codegen 无路径" 基于**过时证据**。实测：codegen `compile_await_expr`（expr/call/async.rs:327）是**真实 async**（mimi_executor_run + mimi_await_future spin-wait + future+8 结果加载），runtime/future.rs 提供 pthread future，dual_backend.rs "Both interpreter and codegen use real spawn/await with pthread" + `dual_actor_await_get`（1000 call no deadlock）+ real_world concurrency_spawn_await 资产。**await 是完整并发能力，保留**——0.34.23 "删除 await 语法" 裁决基于含错证据，正式撤销。正确语义：actor 场景 `await`（非 Future）由 E0245 `infer_await` 拒绝（helpers.rs:291，即裁决执行）；spawn future 的 await 保留。
- **runs_flow E0221**：`a.inc()` 误报修复经分析需**三层集成**（① infer method 注册 ② checker 注册 `zonked_function_types` ③ resolved `function:{Actor}::{method}` callable identity + lower_method_call）→ 登记 0.1.5。bytecode 已完整支持 runs_flow dispatch（run_source 42）；测试 `await a.inc()` → `a.inc()`（actor 调用同步，去 await）。
- **SD-5 mut 字段文档化 ✅**：spec §3.8 新增 `mut` Field Semantics——mut 是声明性标记（并发隔离提示）非写强制；1.0 不引入写强制（Flow 状态机平替 borrow checker，状态转移是唯一状态通道）；`runs Flow` actor 仍拒 mut 业务字段（E0402）。
- 相关源码未提交（infer/resolved 三层方案登记 0.1.5，不在本次落地半成品）。

### 0.34.28 — 裁决文档同步战役（Phase F 之二，sprint 规划 `sprint-0.34.28-doc-sync.md`）

- **文档族全链路对齐四组裁决**（do 已删 0.34.27 / math stable / multi-target stable / become-stay 已删 0.34.11）：
  - **spec** §5.6+§6.8 `math` 由 `[removed]` 修正为 `[stable]`（verifier 通道，Stmt::Math 由 vir.rs:495/878 消费）；§3.7 multi-target 由 `[experimental]` 升 `[stable]`（0.34.15-16 tagged-union ABI）且废止 ":369 not part of the minimum 1.0 RC stable core" 过时句；§6.12 Experimental 清单移除 multi-target；:143 多目标 tag 保留声明升 stable；:1565 RC 阻断条款改稳定表述；§6.12 `do` 加 `(0.34.27 已删除)`。
  - **pre-1.0 九份源头**（spec 的 source）12 处改写：00:83、01:45（Silent Stay→Silent transition 语义更名）、01:84-85（become/stay→`return Target {}`）、02:180、04:178 + §14 Removed 清单（do 加版本 + math 归 stable）、04:165/186、05:99/183/275/284（multi-target experimental→stable、terminal path 四类→三类）。
  - **support.toml** FLOW-MULTI-001 evidence 重写（tagged-union ABI 三后端门禁）+ resolved_ir/interp/codegen 维度如实化（unsupported/partial→complete）；become/stay evidence 术语 → `return S{}`。
  - **AGENTS.md** §13.1 0.1.4 里程碑范围扩至 0.34.32，补 Phase F（do 删除 81→80 达成 ≤80 + 文档同步）。
- **spec §3.13 implicit main 状态登记（实测推翻规划误判）**：`sprint-0.34.28` 规划引用 bytecode/compiler.rs:654 "no main function found" 判定"未实现"——实测 `mimi run` 输出正常，progressive.rs `apply_progressive_typestate`（v0.29.22）注入 `flow Main { state Single }`、main 体进 run transition，:654 仅为无 main 时的 entry 兜底。spec 改为"已实现"登记，非 not-yet-implemented。
- **验收**：check_language_docs.py 通过（31 requirements / 31 support）；`grep become|stay` 仅剩带版本的合法历史引用；"not part of the minimum 1.0 RC stable core" 归零。

### 0.34.27 — `do` wrapper 删除（语言自洽性，sprint 规划 `sprint-0.34.27-do-removal.md`）

- **AST**：删除 `Stmt::Do(Block)`（ast.rs）——`do { X }` ≡ 裸 block `{ X }`（ir/lower.rs 同路径 Lexical Scope + 裸 block 是表达式），无表达力损失。
- **关键字**：`do` 移出关键字表（keywords.rs `"do" => TokenKind::Do` 删除）→ 映射 **81→80，达成 0.1.4 ≤80 硬关键字目标**（golden §1.4）。`do` 现 tokenize 为普通 Ident。
- **解析/**：parse_stmt.rs `do { }` 语句分支删除；token.rs `TokenKind::Do` variant 删除；parser/helpers、pattern、parse_expr 的 TokenKind::Do 分类 arm 清理。
- **消费点清扫**（~28 点/15 文件）：cfg/ir lowering、resolved（kind_name stmt.do→block）、checker（borrow/func/check_stmt）、lint、loader/flow、codegen（block/func + **compile.rs transition do-unwrap 逻辑简化为 `t.body` 直接取块**——该 unwrap 代码本身印证了 `{ do { X } }` ≡ `{ X }`）、bytecode（Do arm 与 Block arm 逐字节一致，删除零风险）、verifier（Do arm 删除，顶层 Return/Expr/Let/If arm 天然接管）。
- **语料迁移**：24 real_world（84 处）+ flow_features（203 处）+ resolved/cfg/ir/audit/actors/codegen_e2e/diagnostic 测试全部展开。`{ do { X } }` → `{ X }`（transition body 扁平化）；普通函数内 do 块 → 裸块语句 `{ X }`。codegen 的 transition 返回值依赖实现体扁平，嵌套 block 会 SIGSEGV——已全部消除。
- **负测试**：`flow_do_keyword_rejected`（单行/多行 transition + func 三场景）——`do { }` 现被 checker 以「未定义类型 do 的结构体构造」拒绝（do 是普通 Ident）。
- **文档同步**：spec §6.7 实施注记 + :518 示例改裸 return；syntax-reference.golden + syntax-reference.md（EBNF 删 do 行、关键字表删 do、差异表 81→80）；golden-document §1.4 达成 ≤80 标记 + §1.3/§10 Phase F。
- **测试**：4597 lib / 13 real_world / 28 cli / 8 ignored（不变）；clippy + fmt 干净。

### 0.34.1–0.34.23（Phase A–E：语法冻结 + 语义裁决 + Flow 补完 + 1.0 准备）

> 0.34.24 之前的 0.1.4 主体 sprint。逐 sprint 权威记录在
> `devdocs/v0.34/golden-document.md` §10（Phase A–E），此处为发布用合并条目。

**Phase A（0.34.1-5）语法冻结前置**：
- 僵尸语法删除（delegate / @transactional / metadata_shadow / `|>` 分隔符）+ CI 负测试；
- 死关键字清理（subflow/steps/consume → Ident）+ `and`/`or`/`not` 软关键字化 +
  `Expr::Range`/`BinOp::Assign` 变体删除；DEAD 代码清理（pinned 路径 ~400 行）；
- `if let` 补实现 + `for (k, v)` 解构 + f-string BUG-5 修复；
- ADR-004 实施（`'a` 生命周期标注删除）+ SD-3 修正（`#[abi(errno)]` → `#[errno]`）+
  白皮书废止标注 + AGENTS §13.20 修正；
- `docs/syntax-reference.md` 从 golden EBNF 再生成 + support.toml 矩阵刷新。

**Phase B（0.34.6-10）语义决策落地**：
- 数值强制统一：双向豁免删除 → 单向 `is_numeric_coercion`（i32→i64/f64、i64→f64），
  stdlib 3 处修复（env::get_int / io::input_int / net::send）+ spec §6.13；
- W013 newtype lint + newtype 虚假承诺注释删除；`nominal_is_flow_state()` 单一事实源；
- `resolve()` release fallback 收紧为 `Type::unknown`（不再静默毒化下游）；
  31 处调用点完整迁移 zonk 登记 0.1.5（dx-backlog #1）；
- E0431 新码（escape 泄漏到边界外）+ `Any` 用户语法移除（builtin_type_names 删除）；
- `ieee_float {}` 双后端实现（域准入证）+ quote!/ast_eval checker 注册（AST 类型）。

**Phase C（0.34.11-18）Flow 补完 + 逃生舱**：
- ADR-001 实施：become/stay 删除（唯一终止符 `return State {}`）；
- View/Mutate 最小闭合（参数级强制 + payload 成员级真借用，双 checker 同步）；
- Multi-target 实施（ADR-002）：`-> A | B` 语法 + E0419 反转 + E0226 闭合；
- Fault 范围收缩（spec §3.12 实现驱动改写）+ @dense 删除 + `with` 子句删除（E0254）。

**Phase D（0.34.19-22）1.0 准备**：
- CHECKER-GAP 软测试审计：47 处 soft→hard 全处理（quote 1 处 R8 裁决豁免）；
- `unsafe_cast_protocol` 逃生舱（checker/resolved/interp/codegen 四层 + dyn fat-pointer
  存量 bug 修复）；
- E0432 泛型×线性边界拒绝 + 文档化（后续 0.34.24 收紧为容器同拒）；
- FFI 1.0 表面评估（盲审 P0 实质闭环，bindgen 残余登记 0.2）+ DX 积压登记表
  （`devdocs/v0.34/dx-backlog-0.1.5.md` 8 项）。

**Phase E（0.34.23）未评估特性评审 I**：
- parasteps / actor / protocol / capability 1.0 决策清单（`feature-decision-0.34.23.md`）：
  parasteps 收敛为结构化并发语法、actor 核心保留（runs_flow codegen 登记 0.2）、
  protocol 编译期契约保留（session 桥登记 0.2）、capability 核心保留（codegen split
  补齐）；认领 3 项（spawn_detached interp 置位 / cap split codegen / 文档漂移修正）。

### 0.34.24 审计战役（四份 audit-{codegen,flow,type,syntax} 报告驱动，原始报告存 /tmp 已被系统清理，以下条目为唯一留存记录）

**CRITICAL 全部闭环**：
- **audit-codegen C1**：multi-target turn 标签 + box 尺寸修复。
- **audit-type C1**：Any 移除后 stdlib Set dispatch 前置豁免。
- **audit-type C2**：turbofish 显式实例化绕过 E0432 线性逃逸封堵（`infer_turbofish` 补线性检查）。
- **audit-type C3**：prelude `f`/`g` 高阶参数被用户同名全局函数遮蔽误拒整文件——resolved lowering + bytecode 解析顺序对齐 checker（local 优先）。
- **audit-syntax C1**：`quote!` 内 `if let` 静默丢弃导致 E0800 栈下溢 → 编译期干净拒绝。
- **audit-syntax C2**：`if let` / `for (k, v)` 解构 native codegen 落地（desugar 到 match / let-tuple），L1 双后端目标测试 un-ignore。

**HIGH**：
- **audit-flow H3**：单组件 cap split 双后端分歧 → check 期 E0221 统一拒绝。
- **audit-codegen H2**：fallible 转移上下文泄漏进逃逸 lambda → 错型 fault return UB 消除。
- **audit-codegen H3/H1**：fault return 路径堆清理（valgrind 0 泄漏）+ 非穷尽 match 吸收对齐。
- **audit-codegen H4**：persistent 字段 Fault shadow 双后端分歧修复。
- **audit-codegen H5**：`unsafe_cast_protocol` 非 record ICE → E0713。
- **audit-type H4**：E0431 自分配后从未发射 → finalize 边界逃逸泄漏（`_`/Infer/unknown 残基）现发射 E0431（ResolveError::EscapeHatch 变分码），let-init `_` 合法边界不变。
- **audit-type H2**：容器线性逃逸封堵——`List/Option/Map<cap>` 禁止穿越泛型边界（E0432，表面类型深度线性化）；具体签名容器参数 CFG 整体视为线性，必须整体消费（E0256，valgrind 实证 drop 后 0 泄漏）。旧"容器放行"豁免被证明是 exactly-once 逃逸，契约测试翻转 + §2.3 注释与 AGENTS.md 诚实改写；元素级消费（match/for）为既有分析缺口，fail-closed 而非静默泄漏。
- **audit-type H3**（文档）：0.34.19 切片 G 名实更正——测试契约改判而非 checker 变严，"CHECKER-GAP 归零"表述修正；0.34.21 容器放行判定标注推翻。

**附带**：release 构建断链修复（`consumed_resources` cfg 门禁误加）。

### 0.34.25 收尾战役（rc-quality-gate-0.34.25a.md 切片）

**0.34.25a — L1 正确性切片（Q1/Q3/Q4）**：
- **Q1**：`match Result<T, string> { Err(msg) }` 的 Err 字符串载荷绑定误编译修复——codegen 此前把 Err 载荷当 i64 重新解释，`p_mi`/`p_mf` 均 panic；Constructor 臂按 payload_idx==2 && 非 variant_owner 推导 `err_expected_ty`（string，兼容 `Type::Result` 与 `Name("Result",[ok,err])` 两 AST 形态），`decode_payload_struct` 两调用点接入，Err 字符串经 inttoptr+load 正确取回。
- **Q3**：trait impl 方法返回 Result 的值在 let 绑定 / println 直调两条路径丢失类型（codegen 显示裸积元组 `(true, 1.5, 0)` 而 VM 显示 `Ok(1.5)`）——新增 `infer_impl_method_return_type`（type_impls 查 FuncDef.ret + fmt_type 渲染），compile_block / compile_block_last_val / compile_func_body 三处 let 绑定 type 追踪与 infer_object_type method-call 背配合接入。
- **Q4**：`ast_eval(quote!{true})` 显示分歧——Value::Bool 折叠由 i64(0/1) 改为布尔显示，VM/codegen 一致。

**0.34.25b — 泄漏切片（Q2）**：io.rs 显示缓冲（256B/次）系统性泄漏修复——marker 机制（`display_marker`/`flush_display_since`）：display 缓冲 codegen 期注册但运行期走分支/循环，顶层一次性 flush 会 free(undef)（Err 臂）并 double-free（Option Some 臂在 None 迭代）；marker 在自身无条件缓冲后取样、flush 发射在**同一运行期块**（臂尾/循环尾）使 frees 与 mallocs 同频。8 个 list 发射器 + emit_result_to_string_typed（ok/err 臂）+ emit_option_to_string 插桩。`dual_from_json` 143/0、`dual_list_option_list_tuple`/`dual_option_record_println` 转绿；p_leak 10000 循环 valgrind **0 errors**；p_dbl4 输出正确无 double-free。

**0.34.25c — mutate place fail-closed（Q5/Q6）**：mutate 实参 place 文法校验——合法 place 为 `Ident` 与单层 `Ident.field`（含 `self.field`，与现有写回机制对齐）；拒绝嵌套 place（`o.inner.value`）、非 place 实参（字面量/计算值/`bump(42)`）、同调用内重复 mutate place（别名同时借用）。E0434（非 place）/ E0435（别名）新码登记；2 个 ignored 测试转负测试（flow_features.rs 4448/4476 对应）。嵌套写回与跨调用别名追踪登记 1.x。

## [0.1.3] — 2026-08-02

### Codegen O1 优化全量回归修复（MIMI_OPT=1 双后端全绿）

- **fix(codegen): lambda 主体补全**：`emit_lambda_body` 新增 `Stmt::If`/`Assign`/`While`/`For`/`Block` 分支（此前仅参数/返回值，闭包回调全部产出 `ret i64 0`）；If 为值模式 + 返回类型宽度调整；`default_ret_value` 按 IntType 自身宽度（`count_val(xs, 2)` O0 0→1，O1 同）。
- **fix(codegen): 泛型实参强转**：`coerce_args_to_param_types` 对泛型函数跳过 `llvm_type_for` 强转（未知泛型名 fallback i64 会把 `0.0` fptosi 成 i64 0），monomorph 调用点按具体形参类型重 coerce（`coerce_args_to_function`）。`sum_float` 经 `reduce_list` 求和修复。
- **fix(codegen): reduce 内建 float 支持**：`compile_reduce_intrinsic` 重写（acc/elem/ret 类型携带、list 槽 i64 位模式 `bit_cast` 回 f64/f32、int↔float 转换）；`try_convert_loop_elem` 对 f32/f64 位模式恢复。
- **fix(codegen): tuple 含 List 返回值**：`compile_tuple_expr` 对 List 字段按值 load（`partition` 返回 `{ptr,ptr}` 与结果类型 `{{i64,ptr},{i64,ptr}}` 不符，O1 verifier 报错）。
- **fix(codegen): 分支/循环条件 i64→i1 归一化**：builtin 谓词（如 `str_starts_with`）返回 zext 的 i64，`compile_if_stmt`/`compile_while_stmt`/func.rs 第二处 if emitter 统一 `icmp ne` 归一（`br i64` 非法 IR）。
- **fix(codegen): 分支 string 字面量值归一**：`normalize_block_last_string` 将裸 string 指针分支值包裹为 `{ptr,i64}`（`first_char` 的 `{ "" }` 分支）。
- **fix(codegen): heap free 跨块 SSA 支配**：`register_heap_alloc` 将指针固化到 entry alloca 槽，两个 free 路径从槽 load（修复 `free(ptr %str_char_at_call)` 不支配 use → O1 指令选择 SIGSEGV）。`a1_verification.mimi` O1 全 10 项通过。
- **fix(codegen): trait 方法调用 string 字面量实参**：`compile_self_method_call` 对 string 形参包裹 raw 指针为 `{ptr,i64}`（`str_index_of("ll")` 经 O1 inline 展开报 `extractvalue ptr` 类型错）。
- **fix(codegen): actors.rs if-else 合并 phi 缺失 else 边默认值**（O1 verifier SIGSEGV）；**io.rs write/fopen Result 返回值修复**（上一轮遗留）。
- 验证：`MIMI_OPT=1` 与 `MIMI_OPT=0` 双路径 4513 lib + real_world 28 + real_world_cli 1 全绿，clippy/fmt 门禁全过。

### INTERP 性能深度优化 III（寄存器缓冲池化 + L1 Any 转换修复）

- **fix(codegen): L1 双后端断裂——`to_int`/`to_float` 对 `Any`（map_get 值）返回裸堆指针**：`map_get` 的值在 LLVM 层是未类型化的 i64 handle（字符串存堆指针）。codegen 的 `to_int`/`to_float`/`str_parse_int`/`str_parse_float` IntValue 分支把 handle 当整数直接返回（`to_int("3000")` → 指针值 591434576），interp 正确解析为 3000。修复：新增 runtime `mimi_any_to_int`/`mimi_any_to_float`（复用 `safe_c_string_from_handle` 启发式：<1MB 整数直通、映射页 + NUL 终止 → strtol/strtod 解析），四个 builtin 的 i64 参数统一经 runtime 判定（与既有 `to_string` Any 路径同设计；CheckedProgram 无局部变量类型目录，静态区分不可行）。`parse_c_decimal_i64` 自实现 strtol 语义（runtime libc feature 无 strtol/strtod），含 i64::MIN 饱和边界。
- **perf(bytecode): frame 寄存器缓冲区池化**：`BytecodeVM.free_regs: Vec<Vec<Value>>` 池 + `pop_frame()` 统一回收（exec_loop 落尾 / RetEarly / do_return 三处），push_frame 复用池中缓冲区（capacity 保留）。release fib(30) 0.72s → **0.57s**（-21%）；debug 被边界检查开销掩盖（3.12s）。
- **回归测试**：`dual_map_get_string_value_to_int`、`dual_map_get_string_value_to_float`（L1 双后端等价）。
- 验证：4513 lib 全绿 + real_world 28 + real_world_cli 1 + clippy/fmt 门禁全过。

### INTERP 性能深度优化 II（exec_loop 单借用重写）

- **perf(bytecode): exec_loop 热分支单借用重写**：常量/整数/浮点/比较/位运算/字符串/跳转 ~40 个 opcode 重写为「单次 `last_mut` 借用内完成读+算+写」，消除每 op 2-3 次 `get_reg`/`set_reg` 的边界检查与 `debug_assert` 开销（debug 构建下 slice/Vec 访问检查约占 40% 执行时间）。`get_int`/`get_float`/`get_float2`/`get_int2` 内联进热分支，错误消息语义逐分支保持（`expected Int/Float, got {}`）；`check_float` 内联为 `is_nan`/`is_infinite` 检查（静态 op 字符串不再 `format!`）。
- **perf(bytecode): CALL 参数寄存器提示**：`compile_expr_into`/`compile_binary_into`/`compile_unary_into`/`compile_literal_into`——CALL 参数表达式直接编译到参数槽寄存器，消除每调用点冗余 `Op::Mov`（fib 每帧 15 op → 13 op，寄存器 12 → 10）；非简单表达式安全回退 `compile_expr` + `Mov`（副作用顺序不变）。
- **perf(bytecode): push_frame 预分配**：`Vec::with_capacity(register_count)` 一次分配到位，消除 resize 的 realloc + memmove。
- **perf(bytecode): Op::Mov 同寄存器短路**：`rd == rs` 跳过克隆。
- **基准 (debug build, 本机)**：fib(30) 3.67s → **3.10s**（-15.5%）；for-range 1M 0.92s → **0.79s**（-14%）；list 循环 1M 0.96s → **0.79s**（-18%）；map 3000×2 3.80s → 3.66s（map_set 值语义深克隆主导，O(n) 克隆为语言语义固有）。
- 验证：4511 lib 全绿 + real_world 28 + real_world_cli 1 + clippy/fmt 门禁全过。

### 查缺补漏：clippy/fmt 债务清零

- **refactor(interp): InterpError 变体内容 Box<ErrorContext>**：`InterpError` 15 个 variant 从直接持有 `ErrorContext`（~128 字节）改为 `Box<ErrorContext>`（8 字节）。`Result<Value, InterpError>` 在解释器热路径上的复制成本大幅下降；消灭 ~672 个 `result_large_err` clippy warning。工厂方法（`InterpError::new`/`div_by_zero` 等）签名不变，调用点零改动。`src/interp/error.rs`、`src/interp/value.rs`。
- **fix(interp): bytecode VM/compiler 39 处 `unwrap()` 清零**：VM 新增 `cur_frame()`/`cur_frame_mut()` 不变量 helper（`mimi_debug_assert!` + ICE `unreachable!`），替换 `exec_loop`/寄存器访问/`do_ret` 中 22 处 `stack.last().unwrap()`；FuncCompiler 新增 `vars_mut`/`break_jumps_mut`/`continue_jumps_mut`/`defer_scopes_mut`/`on_failure_scopes_mut` helper，替换 17 处作用域栈 unwrap（`src/interp/bytecode/vm.rs`、`compiler.rs`）。
- **fix(interp): 剩余 10 个 clippy warning 清零**：`cloned_ref_to_slice_refs`（hof.rs `from_ref`）、`new_without_default`（BytecodeCompiler/BuiltinRegistry）、`option_map_unit_fn`（compiler.rs 2 处）、`manual_map`（compiler.rs 4 处）、`redundant_closure`（stmt.rs）。
- **门禁状态**：`cargo clippy --all-targets -- -D warnings` 从 711 errors / 672 warnings → **0 errors / 0 warnings**（CI 门禁 2 恢复全绿）。4511 lib 测试全绿，零行为变化。
- **fmt 状态**：HEAD 既有 1080 fmt diff（`src/interp/` 与 `src/runtime/mod.rs`，0.33 冲刺遗留）登记为已知债务，本次提交不混入无关格式化改动。

### Soundness 收尾：审计遗留项修复

- **fix(codegen): B9 逃逸闭包 env 泄漏（0.33 收尾审计遗留）**：
  - 根因：旧 claim 设计在 emit_return 只释放函数级 scope 顶部的共享注册项，嵌套 scope（lambda 参数 / 泛型 body / 提前 return 未回退路径）中的闭包 env 泄漏；且 claim 持久化跨 CFG 路径，跨块引用导致 SSA dominance 违规（valgrind uninitialised value）。
  - 重构为「函数边界 + 每条 CFG 路径各自发射 free」设计：`begin_function_heap_scope` / `end_function_heap_scope` / `flush_heap_scopes_to_boundary`（`src/codegen/mod.rs`）。flush 只发射 free、不弹栈、不删注册（编译期堆栈是所有路径共享的），栈平衡由 `end_function_heap_scope`（只丢弃）在函数编译结束时完成。
  - claim 一次性消费：每次 flush `mem::take` 清空 `claimed_returned_envs`，守卫只在当前块内生成 → 消除跨块 SSA 引用。
  - 覆盖全部 return 路径：emit_return / emit_implicit_return / compile_block 4 条提前返回 / compile_block_last_val / lambda 3 处 / try RejectPath / actor 提前返回 + epilogue / legacy Break-Continue；`compile_block` 结尾 heap_depth 守卫防嵌套提前 return 后重复弹栈。
  - 回归测试：`e2e_valgrind_b9_closure_env`（valgrind 零泄漏零未初始化）、`dual_b9_closure_escape_chain`、`ir_b9_closure_env_guard_on_scope_exit`、`ir_b9_closure_return_tracked_at_call_site`。
- **fix(interp): IN-H8 并发 builtin channel_recv sentinel 缺陷**：旧实现用 try_recv 自旋 + 哨兵 -1 表示超时，与合法通道数据冲突（发送 -1 被静默丢弃并替换为 0）。改为直接调用阻塞式 `mimi_channel_recv`，与 codegen 路径语义一致（`src/interp/builtins/concurrency.rs:326`）。
- **fix(runtime): CG-H16 Any 值整数标记残留**：`mimi_any_to_string` 整数不再带 `(val<<1)|1` 标记，直接存储；指针与整数靠对齐/大小/有界扫描启发式区分（`src/runtime/mod.rs:1502`，`MAX_BOUNDED_SCAN=256` C12 限制）。
- **fix(builtins): bytecode Phase D builtin 迁移补全（~55 个）**：并发 26 + Shadow/FS/Regex/Allocator 15 + 网络/工具/actor/测试缺口 14。bytecode builtins 总注册 254 个，覆盖 12 模块（`src/interp/bytecode/builtins/`）。
- **A6 状态澄清（LSP 文本搜索 → AST 位置）**：领域基础设施（Span/Origin/AstNodeMeta + `PositionMap` 字节/字符索引）已在 v0.31.1 落地，但 LSP 消费端 ~20 处文本搜索调用点（如 `state.rs:530 find_enclosing_func_in_items`）尚未迁移到 AST 位置——A6 仅基础设施部分完成，消费端迁移未完成。

### INTERP 性能深度优化（原 0.1.4-dev 周期归入）

- **perf(bytecode): 消除 O(n²) 寄存器克隆**：ListGet/MapGet/MapContains/RecordGet/ConcatStr/JmpIf/EqInt 等 12 个 opcode 消除整个集合克隆，改为借用+仅克隆元素。for-range 10k: 1106ms→53ms (20.9x)。
- **perf(bytecode): for-range 计数器循环**：编译器检测 `for i in range(a, b)` 模式，直接编译为计数器循环（不调用 range() builtin，零列表分配）。内存 O(n)→O(1)。
- **perf(bytecode): 算术/比较 Int 快速路径**：AddInt/SubInt/MulInt/DivInt/ModInt/LtInt/GtInt/LeInt/GeInt/EqInt/NeInt 重构为 `if let (Int, Int)` 首选匹配，消除 2-4 个 `matches!` 分支。for-range 1M: 21%↑。
- **perf(bytecode): push_frame 消除参数双重克隆**：`push_frame` 改为接受 `Vec<Value>` (owned)，消除内部逐元素 clone。
- **perf(bytecode): StrAppend 原地拼接**：新增 `Op::StrAppend`，检测 `s = s + expr`（仅 String 类型），`push_str` 原地追加。string concat 100k: O(n²)→140ms。
- **perf(bytecode): do_return mem::replace**：函数返回时 `mem::replace` 移出返回值（frame 即将 pop），消除返回值深拷贝。

### CODEGEN 性能深度优化（原 0.1.4-dev 周期归入）

- **perf(codegen): legacy emitter for-range() 计数器循环**：检测 `for i in range(a, b)` 调用模式，编译为计数器循环（零 malloc、零列表分配）。与 `BinOp::Range` 共享 `build_for_index_*` 基础设施。
- **perf(codegen): LLVM pass pipeline 优化**：添加 `internalize` + `globaldce` pass（消除未使用 prelude 函数）；Target machine O3→O2 对齐 pass pipeline。
- **fix(codegen): for-in-list i32 元素截断**：`convert_list_elem_i64` 对 IntType 缺少 i64→i32 截断，`for x in [1,2,3]` 段错误。添加 `build_int_truncate` 当 target bit_width < 64。

### 基准 (debug build)

| 场景 | 优化前 | 优化后 | 加速 |
|------|--------|--------|------|
| for-range 10k | 1106ms | 56ms | 19.8x |
| for-range 1M | >120s (timeout) | 922ms | >130x |
| string concat 100k | >120s (O(n²)) | 140ms | ∞ |
| fib(28) | 1.72s | 1.53s | 11% |
| nested 1k×1k | >120s | 921ms | new |

## [0.1.2] — 2026-07-29

### Phase A: Codegen 全量迁移（0.32.1–0.32.15）

- **0.32.1 Option/Result 类型进入 resolved native slice**：Option<T> → `{i1, T}` / Result<T, E> → `{i1, T, E}` LLVM 结构体降低。类型降低层（types.rs）注册 Nominal Result/Option 到 struct type 映射；eligibility gate 放行 Option/Result 类型的参数/返回值/本地绑定。+3 resolved codegen 测试。
- **0.32.2 List 构造/索引/len 进入 resolved native slice**：List<T> → `{i64 len, ptr data}` LLVM 结构体降低。eligibility gate 接受 Nominal List/Map/Set（元素类型递归检查）；ResolvedExprKind::List 字面量发射（malloc + store elements + build_list_struct）；ResolvedProjection::Index 值/左值投影放行。Map/Set → i64 handle（opaque，类型降低层注册）。+4 resolved codegen 测试。
- **Map/Set literal emission（无 sprint 标签）**：ResolvedExprKind::Map/Set 字面量发射进入 resolved emitter。Map → {i64, i64} 键值对数组 + runtime `mimi_map_create` 调用；Set → 元素数组 + runtime `mimi_set_create`。+resolved_failed_functions 追踪基础设施。3 个遗留 bug 修复（match+fstring double free、claim_match_arm_string 精确弹栈、heap scope 管理）。
- **0.32.5 用户自定义 record 类型进入 resolved native slice**：Record 构造（alloca + GEP + store 逐个字段）、Record 字段访问（Field projection lvalue/rvalue）、Record 类型 signature/body 匹配（field 名索引对齐）。+2 resolved codegen 测试。
- **0.32.6 Option/Result match Constructor pattern 进入 resolved native slice**：Match 表达式 Constructor 模式（`Some(x)` / `Ok(x)` / `Err(e)` / `None`）降低为 if-else 链 + discriminant 检查（`{i1, ...}` struct 的 0/1 标签）。+2 resolved codegen 测试。
- **0.32.7 per-function dispatch 重新激活 + ABI 修复**：修复 resolved vs legacy ABI 漂移（`{ptr, i64}` string ABI 在跨 emitter 调用时导致 SIGSEGV）。string 参数传递统一为 `{ptr, i64}` struct-by-value。恢复 per-function dispatch 为默认路径。+3 E2E 双后端测试。
- **0.32.8 for-in-list 迭代进入 resolved native slice**：`for x in list_expr` 降低为 while 循环 + index 变量 + get_list 调用。List iterable 型的 eligibility 检查。+1 resolved codegen 测试。
- **0.32.9 for-in 接受任意 List<T> 表达式作为 iterable**：不限于字面量或变量——任意元表达式（call return、projection 等）的 List<T> 类型都可作为 for-in iterable。
- **0.32.10 Try 表达式 (?) 进入 resolved native slice**：`expr?` 降低为 if-else 检查 Result/Option discriminant + early return Err/None。返回值类型校验（inner type 必须为 Result 或 Option）。+1 resolved codegen 测试。
- **0.32.11 Alias/Newtype 转换进入 resolved native slice**：AliasWrap/AliasUnwrap/NewtypeWrap/NewtypeUnwrap 转换在 eligibility 中接受（LLVM 级 identity），使使用别名和新类型的程序可通过 per-function 检查。
- **0.32.12 自定义 enum 类型进入 resolved native slice**：用户自定义 enum → `{i32 tag, i64 payload}` LLVM 结构体。Enum 类型的构造器模式匹配降低为 tag 比较 + payload 提取。+2 resolved codegen 测试。
- **0.32.13 解除 impls 程序级阻断 + source_id 精确过滤**：checker 自动生成 From/Into impls 导致所有程序 `program.impls()` 非空，0.32.8 阻断修复后仍 SIGSEGV（stdlib 模块函数 body 编译）。修复：per-function source_id 过滤——只有 entry source 文件的函数进入 resolved 编译，模块函数由 legacy emitter 编译。+source_id 精确过滤降低误伤。+2 E2E 双后端测试。
- **0.32.14 range() call 迭代 + Newtype 类型 lowering**：`for x in range(start, end)` 降低。Newtype 在类型降低层注册（transparent wrapper，LLVM repr 同 inner 类型）。+1 resolved codegen 测试。
- **0.32.15 Newtype match Constructor pattern 支持**：Newtype 模式匹配（`MyNewtype(x) => ...`）降低为 payload 解包。+1 resolved codegen 测试。

### Phase B: Codegen 迁移深化（0.32.16–0.32.25）

- **0.32.16 非捕获 Lambda/闭包进入 resolved native slice**：Lambda 表达式 → `{ptr fn_ptr, ptr env}` 闭包结构体。非捕获闭包（captures 为空）无环境指针。LocalClosure 调用通过函数指针间接调用。+heap scope 生命周期修复（free 移到 return 之前）。+2 resolved codegen 测试。
- **0.32.17 自定义 enum 构造器（Ok/Err 等复用标准库名字）**：允许用户自定义 enum variant 使用 `Ok`/`Err` 等名字（消除与标准库 Result/Option 的名字冲突限制）。variant 名称解析按类型作用域。
- **0.32.17b Option/Result builtin methods**：`is_some()`/`is_ok()`/`unwrap_or(default)` 等 builtin 方法进入 resolved native slice。方法调用 dispatch 通过 `ResolvedCallee::Builtin` 桥接到既有 `compile_builtin_call`。
- **0.32.18 解除 callee source_id 限制 + builtin shadow call**：允许从 resolved 函数调用模块函数（前向声明由 legacy emitter 注册，resolved emitter 仅发射 `call` 指令）。builtin shadow call 修复（builtin 调用链中参数传递匹配 callee 声明类型）。
- **0.32.19 call 参数 coerce 到 callee 声明类型**：修复 resolved emitter 中 call 表达式参数类型不匹配问题——callee 声明类型与被调用表达式类型做 conversion 统一。
- **0.32.20 Flow 程序 per-function dispatch 解锁**：Flow 程序不再被程序级门禁阻断。per-function eligibility 检查 transition 函数体内是否含有不支持的 node；Flow 基础设施（类型定义、前向声明）由 legacy emitter 编译。Flow transition 函数中 view/mutate borrow 参数被拒绝（指针 ABI 不匹配），`self` receiver 例外（值 ABI）。
- **0.32.21 Flow state field access 支持**：Flow state 的 record-like 字段访问（`state.field_name`）在 resolved emitter 中发射为 GEP + load。
- **0.32.22 builtin call i32→i64 参数提升 + verification IR dump**：builtin 调用参数从 i32 提升到 i64（匹配 legacy `mimi_type_to_llvm` 的 ABI）。Verification IR dump 基础设施（`--dump-verification-ir` 标志）。
- **0.32.23 ProtocolMethod 静态 trait dispatch**：ProtocolMethod 调用（ResolvedCallee::ProtocolMethod）进入 resolved native slice。静态 dispatch——编译器在 checker 阶段解析具体 callee，resolved emitter 发射直接函数调用。
- **0.32.24 actors/sessions/protocols 程序级门禁拆除**：含有 actor/session/protocol 的程序不再被程序级门禁阻断。per-function eligibility 过滤器处理函数级别的排除（actor 方法：non-User origin + qualified name；session/protocol 函数：non-User origin；函数签名含 actor/session/protocol 类型：require_scalar_type 拒绝非标称 Nominal 类型）。legacy emitter 编译 actor/session/protocol 基础设施。
- **0.32.25 count_substring builtin + List O(1) push + ABI safety**：
  - `count_substring` builtin：字符串子串计数（O(n·m) 扫描），codegen 经 LLVM `memcmp` 循环 + 解释器经 Rust `matches` 实现。双后端等价。
  - List O(1) push：`push(list, item)` 经 `mimi_list_push` runtime（amortized O(1)），不再重新分配整个数组。
  - ABI safety：编译期校验 callee 签名与 caller 期望的类型一致性，防止跨 emitter ABI 漂移。
  - +2 E2E 双后端测试。

### Phase C: 删除 legacy body + 审计（0.32.26–0.32.30）

- **0.32.26 Extern FFI 调用进入 resolved native slice**：
  - `ResolvedCallee::Extern` 现在在 per-function eligibility 检查中被接受（不再是静默回退到 legacy 的原因）。
  - resolved emitter 新增 extern 调用处理：通过 `program.extern_blocks()` 查找 extern 签名 → 获取 wrapper 函数名 → 在 LLVM 模块中查找 → 参数 coercion → 调用。
  - extern wrapper 函数由 legacy emitter 在阶段 1 前向声明，resolved emitter 在阶段 4 消费，架构一致。
  - 无新增 real_world 测试（extern 调用需要实际 C 库，不属于纯双后端等价集）。

- **0.32.27 Verifier C3 迁移：合同验证从 AST 切换到 Resolved IR**：
  - C5: `core/resolved/tests.rs` 的 `legacy_body_file()` 断言替换为 `functions().len() > 0`。
  - `resolved_expr.rs::verify_contracts_from_resolved()` 新增 math 义务处理：在 requires 断言之后、body 编码之前检查 `Stmt::Math` 表达式，验证其是否被前置条件蕴含。若全部 proven 但无 ensures，返回 `Proven` 而非 `NoObligations`。
  - `resolved_expr.rs::has_math_obligations()` 新增辅助函数。
  - `ctx.rs::verify_checked_contracts()` 新增方法：遍历 `program.callables()`，为每个有 contracts/math 的 callable 调用 `verify_contracts_from_resolved()`。
  - `ctx.rs::verify_checked()` 的 `verify_file(program.legacy_body_file())` 替换为 `verify_checked_contracts(program)`。
  - 移除 `#[allow(dead_code)]` 注解。新增 `mock_verify_checked()` 作为 C4 scaffold。
  - 主验证入口 `verify_checked` (mod.rs) 拆分为双路径：Z3 可用时走 flow verifier（仍用 raw_ast），Z3 不可用时走 `mock_verify_checked`（CheckedProgram-based，无 raw_ast）。
  - `mock_verify_checked` 现在同时处理 callables 合约和 extern block 合约。
  - `verify_ffi_checked` (mod.rs) 同样拆分为双路径。
  - `flow_verify_file_or_mock` 降级为 `pub(crate)` + `#[allow(dead_code)]`（仅被测试助手使用）。
  - 基线：4448 全量测试绿，0 退化。
  - **C1 签名的迁移**：`compile_file_with_resolved` 签名移除 `file: &File` 参数，caller 不再调用 `program.legacy_body_file()`。改为内部通过 `program.raw_ast()` 获取，封装实现细节。
  - 当前 `raw_ast()` 剩余 3 个调用点：C1 (codegen 内部)、C2 (interp)、C4×2 (verifier Z3 路径)。mock 路径和三部分验证（C3/C5/ctx）已完全脱离。

- **0.32.28 C1/C4 内部保留决策（文档 sprint）**：
  - C1（codegen 第五遍）永久保留 `raw_ast()` 内部调用：AST body 注册 + ineligible body class 编译需要 raw AST。
  - C4（verifier Z3 路径）永久保留 `raw_ast()` 调用：Flow verifier 的 Z3 编码定义在 AST Expr 节点上。
  - CHANGELOG 补全 Phase A（0.32.1–0.32.15）、Phase B（0.32.16–0.32.25）全部 sprint 记录。

- **0.32.29 legacy_body_file()→raw_ast() 私有化 + compile_func→compile_func_legacy（不可逆）**：
  - `legacy_body_file()` 重命名为 `raw_ast()`，添加完整架构文档（C1/C2/C4 三个永久 consumer 的理由）。旧名删除（所有 caller 已迁移）。
  - `compile_func` 重命名为 `compile_func_legacy`，添加文档说明第五遍架构：resolved native emitter（第四遍）处理 eligible subset，legacy emitter（第五遍）处理永久 ineligible body classes（capturing lambdas/generics/async/extern ABI/view-mutate borrow params）。skip guard（`count_basic_blocks != 0`）防止双重发射。
  - 所有 consumer 调用点添加显式理由注释（C1 codegen/C2 interp/C4 verifier）。
  - 禁止新代码调用 `raw_ast()`：声明层数据通过 typed accessor 获取。
  - clippy/fmt 修复：`and_then(|x| Some(y))` → `map(|x| y)`（resolved/mod.rs:2863）。
  - 基线：4448 lib + 13 real_world + 28 cli 全绿，clippy 0 warnings，fmt 0 diffs。

### Phase D: 缺口审查 + 填补（0.32.31–0.32.35）

- **0.32.31 Slice 表达式进入 resolved native slice**：`xs[start:end]` view 语义（不拷贝数据），构建新 `{i64 len, ptr data}` struct 指向原有 buffer 偏移处。索引 clamp 到 [0, list_len] 防止 OOB 指针算术。
- **0.32.32 Old 表达式进入 resolved native slice**：合约 `old(x)` 在 codegen 为 identity（合约运行时擦除，只有 verifier 区分 old 语义）。
- **0.32.33 Comprehension 进入 resolved native slice**：`[value for pattern in iterable if guard]` 降低为预分配 buffer + 循环 + guard 过滤 + 计数 + 构建结果 list。
- **0.32.34 OptionalChain 进入 resolved native slice**：`receiver?.field` 降低为 discriminant 分支 + 字段投影 + PHI 合并。Some/Ok → 投影字段包装 Some；None/Err → 返回 None。
- **0.32.35 Callable（一等函数值）进入 resolved native slice**：`ResolvedCallee::Function` 返回 LLVM 函数指针（`GlobalValue.as_pointer_value()`）。仅 User-origin 非 qualified 函数。
- **0.32.35b NestedCallable 语句修复**：嵌套函数声明标记在 eligibility 中放行（no-op），修复含嵌套函数的程序被错误拒绝。

### Phase E: 性能基线（0.32.37–0.32.38）

- **0.32.37 性能基线基础设施**：`benchmarks/` 目录（fib/mandelbrot），Mimi codegen/interp + C (gcc -O2) + CPython 四路对比。`run.sh` 自动编译/运行/计时/异常检测（>2x 偏差标红）。
- **0.32.38 性能异常分析**：Mimi/C ≈ 4x（MIMI_OPT=1）。根因：SD-7 checked arithmetic（每个 add/sub/mul 走 `llvm.sadd.with.overflow` + overflow 检查 + trap 分支，3-4 指令 vs C 的 1 指令）。设计决策非 bug。MIMI_OPT 默认关闭（需 `MIMI_OPT=1` 启用 O2）。

---

## [0.1.1] — 2026-07-28

### 审计修复（发布前深度审计）

- **P1-1 Origin::User 过滤**：`require_resolved_native_program()` 添加 `Origin::User` 检查。non-User-origin 函数（stdlib 导入/runtime 生成）触发 all-or-nothing 路径拒绝，强制走 per-function dispatch。修复窄触发 SIGSEGV（纯函数 + `use std::xxx` 程序）。
- **P1-4 per-function LLVM verify**：`compile_subset()` 每个成功发射的函数调用 `llvm_fn.verify()`。验证失败视为发射失败，回退到 legacy emitter。防止无效 LLVM IR 溜过。
- **P2-2 死代码删除**：`compile.rs:888` `let _ = &flow.persistent_fields` 无操作引用删除。

### Phase H: RC + 发布（0.31.57–0.31.58）

- **CODEGEN Typed Resolved IR 迁移 S8–S13（per-function dispatch 激活）**：
  - S8: String ABI 统一（`{ptr, i64}` struct，消除 resolved/legacy 双 ABI 漂移）
  - S8b: Builtin string 返回值包装（`wrap_builtin_string_result`，raw ptr → `{ptr, i64}`）
  - S9: Per-function dispatch 激活 + Tuple ABI 统一（sub-64-bit int → i64，匹配 legacy `mimi_type_to_llvm`）
  - S10: E2E 双后端测试 `real_world_per_function_dispatch`（fib/divmod eligible，sum_list ineligible via List）
  - S11: 文档化 traits/impls/externs 阻断根因（impl method body 调用 builtin 传 `{ptr,i64}` struct 但 builtin 期望 raw ptr → SIGSEGV）
  - S12: `compile_resolved_subset` 移入 `compile_file_inner`（setup 之后、legacy body pass 之前），确保所有符号声明先于 resolved 发射
  - S13: Origin-based filtering（`Origin::User(_)` 检查）+ 精确文档化剩余阻断
- **Silent fallback → verbose warning**：per-function resolved emitter fallback 不再静默。`MIMI_VERBOSE=1` 输出逐函数 fallback 详情和聚合计数。默认输出不变。满足 0.1.1 硬门禁：codegen dispatch 路径无 silent fallback。
- **退出标准修订**：`devdocs/v0.31/README.md` §9 "只消费 Typed Resolved IR" → 精确分层描述（声明层全量 typed IR / 函数体 per-function dispatch + 显式 legacy arm）。SD-6 `legacy_body_file()` 删除推迟到 0.1.2 (0.32)。

### Phase H: RC1 阻断修复（0.31.57，进行中）

- **CODEGEN Typed Resolved IR 迁移 S0/S1**：新增 `codegen::resolved` 生产 emitter，首批 primitive scalar leaf callable 直接消费 `CheckedProgram` 中的 `ResolvedCallable` / `ResolvedTypeId` / `ResolvedLocalId` / `ResolvedCallee`；支持 canonical 类型降低、稳定 local identity、标量运算/转换与直接函数调用。生产入口仅对静态 eligibility 通过的 body class 切换；typed emitter 一旦入选则 fail-closed，禁止失败后回退 raw AST。毒化 `legacy_file` 的回归测试已证明该 cohort 不读 surface body。
- **CODEGEN Typed Resolved IR 迁移 S2–S4（控制流 + builtin + 类型扩展）**：resolved native emitter 逐步扩展至覆盖：If/Block/Scope 表达式、While/Loop/For(Range)/Break/Continue 语句、`ResolvedCallee::Builtin` 桥接 `compile_builtin_call`（含 print-family `pending_print_arg_types` 设置）、NumericNarrowChecked 通用数值转换（int↔float、truncate/extend/fptosi/sitofp）、String 类型（opaque ptr ABI）+ 字符串字面量 + println、Constant 目录查询发射、程序级门禁放宽（允许 type_defs 声明）。已知 checker 限制：resolved body lowering 不支持 range iterable（TOOL-RESOLUTION-001）、资源分析不认 break CFG 路径。
- **CODEGEN Typed Resolved IR 迁移 S5–S7（聚合 + FString + Match）**：Tuple 构造（alloca+gep+store）、Tuple 投影（extractvalue / GEP 链）、Tuple 解构模式（`let (a,b) = expr`）；全 primitive 类型放行（消除 eligibility 与 type lowering 白名单漂移）；no-op 规范语句（Drop/Contract/Math）；FString text-only 全局常量 + interpolation snprintf 方案（i32/i64/f64/string/bool 格式符映射）；Match 表达式链式 if-else 降低（Literal/Wildcard/Binding pattern + guard AND）。18 项 resolved codegen 测试全绿；e2e 验证 divmod/sum_digits/classify/f-string 程序双后端等价。已知 L1 cosmetic 限制：float FString 格式精度差异（snprintf %g vs interpreter Display）。
- **CODEGEN 迁移结构门禁**：`src/codegen/resolved/` 禁止调用 `legacy_body_file()` / `compile_file()`，禁止导入 surface body AST（仅允许复用无语义身份的 `BinOp` 运算符枚举）。未迁移 body class 仍保留显式 legacy arm，不计入 Typed IR 完成度。
- **测试并发文档校正**：全量日常门禁改为 `cargo test -- --test-threads=4`（性能优化后约 42 秒）；Z3 验证专项仍使用单线程，极端内存受限环境可退回单线程。
- **Native `List<string>` ABI 修复**：`mimi_list_to_string` 不再读取 native codegen 两字段 `{len, data}` 布局中不存在的 `element_kind` 尾字段；`std::array` 的真实 List 返回 API 补齐 codegen 类型识别。修复 `println(array_slice(...))` 输出地址而非字符串的 silent miscompilation，并用 real-world 双后端回归覆盖 slice/take/drop/concat。
- **路线图机器契约修复**：校验器识别 `soundness`/`completeness` milestone kind；R1/R2/R3 审查不变量不再冒充语言 requirement ID；README/RC 分册同步到权威 `last = 58`。

### Phase F: Soundness（0.31.49–0.31.54）

- **0.31.49 类型系统 `let _ =` 漏洞修复（T-1~T-4）**：push() 统一检查（`push(list_of_i32, "hello")` 被拒绝）；slice 索引整数验证；range 表达式整数验证（防御深度）；reduce() 签名验证（累加器/元素/返回类型三路统一）。Slice<X> 兼容 X（slice view 运行时强制）。+6 L2 测试。
- **0.31.50 结构性缺口修复（T-6/T-7/T-8）**：resolve() 静默回退添加 debug_assert!；resolve_type 10 个所有权 wrapper 递归解析内部类型（Shared<MyAlias> 别名解析）；ExternFunc 泛型替换保留 ABI 标记（不再静默转为 Func）。
- **0.31.51a SD-7/SD-8 整数 Trap**：add/sub/mul 使用 LLVM 溢出内建（llvm.sX.with.overflow）+ mimi_trap_overflow；div/mod 检查除零（mimi_trap_div_by_zero）和 MIN/-1（mimi_trap_div_overflow）。之前除零返回 0（静默），现在 Trap（显式失败）。21 个黄金文件重新生成。
- **0.31.51b SD-9/10/12 浮点语义统一**：SD-9 codegen 浮点 NaN/Inf trap（fcmp UNO + llvm.fabs + OEQ vs Inf，mimi_trap_float_not_finite）；SD-10 解释器 float == 改为精确比较（消除 epsilon L1 divergence）；SD-12 json get_float → Result<f64, string>（消除 0.0/0.0 NaN 哨兵）+ str_parse_float 拒绝 "NaN"/"inf"。
- **0.31.52 Runtime 安全（R-1/R-4）**：ERROR_HANDLER 函数指针存储 UB 修复（AtomicPtr<ErrorHandler> → AtomicPtr<()> + usize round-trip）；Capability 表 thread-local 限制文档化。
- **0.31.53 Verifier 健全性（V-2）**：不可编码的 requires/ensures 改为 fail-closed（返回 NotInTrustedSubset），防止约束不完整时产生假 Proven。
- **0.31.54 Interpreter/Quote（Q-1/I-4）**：QuotedAst::Return 设置 early_return（之前是空操作）；QuotedAst::For 每次迭代 push_scope/pop_scope（修复循环变量泄漏）。

### Phase D: 工具与隔离（0.31.42–0.31.44）

- **0.31.42 TyErr 毒药类型 + Z3 报错翻译**：`TyErr` 毒药类型防止级联错误刷屏（18 文件 / 31 match arms）；`VerifStatus::plain_language()` + `hint()` + `icon()` 将 Z3 验证状态翻译为用户可读语言。+12 测试。
- **0.31.43 SD-1 is_linear 结构化标记**：`ResolvedType::Nominal { is_linear: bool }` 替代字符串匹配；`NominalTypeId::nominal_is_linear()` 单一真相源（`state:` 前缀 / `SessionChan` 后缀）；`intern_type()` 设置标记，`substitute()` 保留，`canonical()` 不含（派生属性）。6 文件 / 23 处 pattern match 修复。+6 测试。
- **0.31.44 SD-3 #[errno] 属性**：块级 `#[errno] extern "C" { ... }` 传播到所有函数；函数级 `#[errno] func ...` per-function 控制。`ExternFunc.returns_errno` / `ResolvedExternFunc.returns_errno`。过渡期保留 `ERRNO_CHECK_FUNC_NAMES` 名称猜测（1.0 删除）。15 文件。+6 测试。
- **0.31.44 SD-4 fork() → 信号守卫**：删除 `FORK_LOCK` / `check_multithreaded_fork` / `call_ffi_with_fork_isolation*`（−457 行）；新增 `signal_guard.rs`（+263 行）：SIGSEGV/SIGABRT/SIGBUS/SIGILL/SIGFPE 信号处理器 + `sigsetjmp`/`siglongjmp` 恢复点 + thread-local jmp_buf。GUARD_LOCK 全局 Mutex 序列化 + IN_GUARDED_CALL thread-local 重入检测（防 C 回调 → Mimi → FFI 死锁）。+7 测试。
- **0.31.44 ResolvedExpr Z3 编码**：`src/verifier/resolved_expr.rs`（+758 LOC）：`resolved_to_z3_int/real/bool` 三路编码 + `verify_contracts_from_resolved()` 端到端合约验证。P0 健全性修复：body 编码失败 → `NotInTrustedSubset`（防假 Disproven）；encoding_failures > 0 → 无条件 `NotInTrustedSubset`。P1：result 变量类型推断（Int/Real/Bool）+ bool 等式编码（int→real→bool 三级回退）。+10 测试。

### Phase E: 冻结与 RC（0.31.45–0.31.46）

- **0.31.45 DEBUG 周期 + Interpreter dual-path**：`run_dual_path()` 同时运行 AST 和 Resolved 解释器，比较结果；`DualPathResult` 枚举（Match / ResolvedUnsupported / ResolvedFailed / BothFailed / AstFailedResolvedOk / Mismatch）；`values_equal_for_test()` 深度值比较。28 项 dual-interpreter 等价测试覆盖：基础算术、控制流、函数、递归、数据结构、模式匹配、记录/变体、Option/Result、字符串、合约、推导、闭包。关键发现：ResolvedInterpreter 支持闭包，不支持 FFI。
- **0.31.46 最终敌对审查**：
  - **Trap Tests**（+26 测试）：IEEE-754 边界（NaN/Inf/除零/负零）、整数溢出（i32/i64 MIN/MAX/wrap）、OOB 访问（列表/字符串/嵌套）。关键发现：Mimi 不遵循 IEEE-754 除零语义（抛 DivisionByZero 而非 Inf/NaN）；NaN 比较抛错而非返回 false；列表支持 Python-style 负索引。
  - **type_folder 测试补充**（2→18 项）：RemapFolder / CollectVarsFolder / NamedSubstitutionFolder 全覆盖，含 ForAll binder 遮蔽、链式替换、环检测（P1-13）。
  - **诊断输出确定性**：Checker::check() 对 errors/warnings 按 (start_line, start_col, message) 排序，消除 HashMap 迭代非确定性。
  - **0.31.30–38 深度审查修复**：BUG-1 c_header.rs 数字开头标识符净化死代码；BUG-2 codegen_e2e.rs 测试空转；LIE-1/LIE-2 文档撒谎修正；GAP-2 serialize.rs 静默降级警告；GAP-5 AllocLedger DoubleAlloc 检测；GAP-7 wire.rs contains_handle_depth 保守返回。

### Phase C: Component 边界（0.31.30–0.31.36 完善）

- **0.31.30 Component IR**（COMPONENT-IR-001）：`ComponentIr` 单一语义源（identity/exports/imports/types）+ `AbiGenerator` 类型化注册表（`register_core_runtime_abi` 覆盖 RC/list/map/set/string/io/concurrency/fs/time ~40 函数）+ `.mimiabi` JSON 序列化 round-trip + BLAKE3 防篡改哈希。Codegen debug 构建对发射的 runtime 调用做 Component IR 校验。
- **0.31.31 Native ABI + 代际 handle**（COMPONENT-HANDLE-001）：`HandleRegistry` nominal generational handle 运行时——kind 标签 + runtime owner + generation 计数 + 并发 lease；destroy 要求零 lease 且递增 generation（ABA 防御）；拒绝 StaleGeneration/WrongKind/WrongRuntime/UnknownSlot/LeasesOutstanding；Mutex 线程安全零 unsafe。String 表面迁移到胖指针（MimiString/MimiSlice）+ 零拷贝 `mimi_string_as_slice`。11 个测试含 8 线程并发压力。
- **0.31.32 稳定检查点**：`checkpoint.rs` 无新 surface——ABI 布局 probe（offset 越界/非单调/对齐校验）+ `.mimiabi` round-trip 不动点 + allocator provenance ledger（wrong-side/double-free/unknown free/leak 检测）+ handle lease race 高并发压力（generation 复用安全）。11 个测试。

### io.rs 格式化 dispatch 重构（Sprint A1–A2 完成）

- 46 个 `emit_map_*` 近重复函数合并为参数化 `emit_map_container_product_to_json` + `product_tuple_arity` 辅助函数（-1544 行）。
- `extract_print_arg` 的 Map/Set 分发树（1886 行手工展开 if-else）替换为 `resolve_container_product` 递归类型解析器（-1693 行）。
- io.rs: 14658 → 11421 行（-22%），双后端 735/0 等价确认。

### FLOW-IDENTITY-001 状态身份（0.31.8 完成）

- **E0421 状态不可伪造**：非根 flow state 在 transition 体外构造被拒绝。根状态（flow 第一个 state）构造不受影响（Flow 构造器语义）。3 个新测试。
- **E0422 名义状态区分**（warning）：跨流同名义状态 payload 兼容时发出 warning，提示使用限定名 `flow::<flow_name>::<state_name>`。
- **E0423 线性 generation**：flow state 变量被 transition 消费后，后续使用被静态拒绝。Checker 层 `consumed_flow_vars` 追踪 + `lookup_var` 拦截；解释器 `mark_moved` safety net。2 个新测试。`flow_counter.mimi` 和 `flow_codegen_chain` 中的 use-after-transition 已修正。

### FLOW-TURN-001 原子 Turn（0.31.9 完成）

- **`fails E` 语法**：transition 签名支持 `-> Target fails ErrorType`，声明可回滚失败路径。Lexer 新增 `fails` 关键字，Parser 解析 `fails E`，AST `TransitionDef.fails: Option<Type>`。
- **E0424**：transition 体内 `?` 无 `fails E` 声明时静态拒绝。
- **Rejected 路径（解释器）**：`?` 失败时 transition 返回 `Err((source_payload, error))`，source generation 归还调用方。修复 `early_return` 泄漏 bug（transition 体内 `?` 的 `early_return` 不再穿透到调用方）。4 个新测试。
- **Codegen fail-closed**：`fails E` transition 中 `?` 在 codegen 报 `CompileError::Unsupported`（E0722），防止静默产生错误行为。无 `?` 的 `fails E` transition 正常编译。
- **`transition_fails_types` 基础设施**：Checker 存储每个 transition 的 `fails E` 类型。
- **返回类型 `Result<Target, (Source, E)>`**：Checker 注册 `fails E` transition 返回类型为 `Result<Target, (Source, E)>`；Resolved IR `ResolvedTransition.fails` 字段 + canonical 签名同步包装；IR Lower `transition_fails` 标志使 return 语句期望内层 Target 类型；Interpreter 成功路径包装 `Ok(v)`。测试更新为 match Ok/Err 模式。
- **`become`/`stay` 显式 terminal 关键字**：`become Expr` 构造目标状态并结束 transition（等价于 return）；`stay` 返回 source 状态不变（自环终端）。全栈实现：Lexer/Parser/AST/Checker/IR Lower/CFG/Interpreter/Codegen（block.rs + func.rs）。5 个新测试（含双后端等价）。
- **Codegen Rejected 完整镜像**：`fails E` transition 中 `?` 在 codegen 不再 fail-closed（E0722 已移除）。Rejected 路径构造 `Err((source, error))` 并返回；成功路径包装 `Ok(target)`。`transition_to_func` 返回类型变为 `Result<Target, (Source, E)>`。2 个新双后端测试。
- **Draft isolation**：transition 体内 `self` 为不可变参数（`mut_: false`），source 在 Rejected 时原样归还。原子 turn 保证：transition 要么成功返回 Ok(target)，要么失败返回 Err((source, error))，不存在中间状态泄漏。
- **已知限制**：codegen match on Result with record payloads 不支持对绑定变量的字段访问（`var_type_names` 未注册 Ok payload 类型名），需用 `Ok(_)`/`Err(_)` 模式。

### 0.31.10 稀疏图 + typed Fault + 显式 reset/recover（进行中）

- **Per-Flow typed Fault**：`fault ErrorType` 声明语法，注入的 Fault 状态携带 `error: ErrorType` 字段。回退 transition 自动填充默认值。2 个新测试（含双后端）。
- **@sparse 稀疏图**：`@sparse` bare annotation 跳过 N×M fallback 注入。未声明的 (state, event) 对产生编译时错误而非自动路由到 Fault。2 个新测试。
- **显式 reset/recover**：用户自定义 `transition reset(Fault) -> State` / `transition recover(Fault) -> State` 覆盖自动注入的系统动词。2 个新双后端测试验证覆盖行为。
- 待实现：progressive Main 真 lowering（main 函数体作为 transition body 参与 Resolved IR lowering）。

### 0.31.11 Actor runs Flow（进行中）

- **`actor Name runs FlowName` 语法**：AST `ActorDef.runs_flow: Option<String>`，Parser 解析 `runs` soft keyword，Checker 验证引用的 flow 存在（E0402）。
- **Interpreter 集成**：`ActorInstance` 新增 `runs_flow` + `flow_state` 字段。spawn 时初始化 flow_state 为 root state（默认值）。Worker thread dispatch：`runs_flow` 设置时消息路由到 Flow transition table（从 flow_state 提取当前状态名 → 查找 (from_state, event) 匹配的 transition → `eval_flow_transition` 执行原子 turn → 更新 flow_state）。
- **mut 字段禁止**：`runs_flow` actor 的 `mut` 业务字段被 E0402 拒绝（状态由 Flow 携带）。
- **测试**：`actor_runs_flow_dispatch_through_transition` 验证 Zero→Positive→Positive 多 turn 累积（s3.n == 2）。
- 待实现：Codegen actor runs flow（需要 tagged-union state 存储 + state-dependent dispatch，与当前 flat-struct actor 模型不兼容，需专门设计）。

### 审查修复（0.31.9–0.31.11 事后审查）

- **C1 `block_returns_on_all_paths` 不认识 `Become`/`Stay`**：match 缺少分支导致 E0255 误报，CLI 拒绝合法 `become`/`stay` 代码。测试因 `run_source_result` 跳过 checker 而漏网。修复：添加 `Stmt::Become(_) | Stmt::Stay => return true`。
- **C2 Rejected 路径 error 双重包装**：`eval_try` 设 `early_return = Some(Err(e))`（完整 variant），Rejected 路径再包一层 `Err((source, Err(e)))`。修复：解包 variant 取内层 error 值。
- **H1 CFG 中 `Stay` 是 no-op**：`stay` 后的代码在 CFG 中仍可达，影响 ownership 分析。修复：标为 `Terminator::Return`。
- **H2 `Stay` 无类型验证**：注释声称 checker 验证 source 类型匹配，但无代码。修复：`self` 类型与返回类型 unify 失败时 E0209。
- **附加：`become`/`stay` 不再设 `early_return`**：仅 `?` 使用 Rejected 信号，避免 `become` 在 `fails E` transition 中误触发 Rejected 路径。

### 0.31.12 Typed Session Residual（完成）

- **E0425 scope exit 检查**：函数结束时，非 `end` residual 的 session endpoint 被拒绝。endpoint 必须完成协议（send/recv/close）或显式 return/transfer。
- **E0426 use-after-alias**：`let b = a`（a 是 session endpoint）后，a 被标记为 consumed，再用 a 触发 E0426（线性消费）。
- **Alias residual 转移**：`let b = a` 将 residual 从 a 转移到 b，a 的 residual 被移除。
- **Branch merge 一致性**：if/else 两分支的 session residual 必须一致才能 merge，分歧时 E0425。无 else 分支时保守恢复 pre-branch 状态。
- **测试基础设施（H3）**：新增 `checked_run_source_result` / `checked_compile_and_run`（checker + 后端），迁移 0.31.9–0.31.11 测试到 checked helper。
- 6 个新测试：alias 转移、use-after-alias、scope exit 拒绝/通过、branch merge 一致/分歧。

### 0.31.13 Resource exactly-once（进行中）

- **Session endpoint 函数参数 move**：session endpoint 作为函数参数传递时消费 residual，修复 E0425 误报，正确报 E0304 (moved after consumed)。
- **Session 线性回归验证**：double-close (E0304)、branch partial consume (E0425)、move-to-function (E0304) 三个场景确认 CFG dataflow 覆盖。
- **Cap 闭包 capture**：已有 TransferChild 分析 + E0304 强制（`ownership_checker_rejects_implicit_nested_capability_capture`），无需新增。
- 3 个新测试：session_double_close_rejected、session_branch_partial_consume_rejected、session_endpoint_move_to_function_rejected。
- **追加 A — Flow 状态别名追踪 + shared/ref 拒绝**：
  - `let b = s0`（s0 是 flow state）消费 s0，后续使用 s0 触发 E0423（对标 session E0426 机制）。
  - `shared`/`local_shared`/`weak`/`weak_local` 包装 flow state → E0427 拒绝（线性资源不允许多重引用）。
  - `let ref r = flow_state` → E0427 拒绝（借用隐含原值仍可用，违反线性）。
  - 删除 `consumed_flow_vars.remove(name)` shadowing 清除逻辑——shadowing 不重置线性消费（保守策略，0.31.16 CFG place 追踪修正）。
  - `flow_state_type_names: HashSet<String>` 注册所有 flow state 类型名（qualified + unqualified）。
  - CFG `is_linear()` 预留 transition `self` 跳过逻辑（0.31.16 启用 FlowStateSet + state: Nominal）。
  - 5 个新负测试。4098 测试全绿。
- 待实现：cross-turn exactly-once（Flow transition 间资源跟踪）、Fault path 资源清理。

### 0.31.14 Static Protocol Stable（进行中）

- **移除 deprecated `protocol_methods`**：spec 标记 `[removed]`，从 builtins/inference/codegen/interpreter 全部清除。Protocol 是纯编译期拓扑检查，不需要运行时反射。
- **Protocol 测试迁移**：4 个双后端测试迁移到 checked helper。
- **追加 A — Protocol conformance × 线性检查**：
  - Protocol state payload 线性匹配：protocol 声明线性 payload (Cap, SessionChan) 时，flow state 对应字段必须也是线性类型，降级 → E0427。
  - 3 个新测试：alias bypass (E0423)、alias target valid、payload downgrade (E0427)。
- 待实现：permission/effect 约束检查、fault 暴露策略、版本握手（需 Component IR，Phase C）。

### 0.31.15 Canonical Semantic Trace（基础设施完成）

- **TraceEvent / TraceCollector / compare_traces**：canonical 语义追踪基础设施（`src/trace.rs`），记录 Transition + Fault 事件，5 个单元测试。
- **Interpreter 集成**：`trace_collector` 字段 + `eval_flow_transition` 中 transition/Fault 事件记录。
- **`run_source_with_trace` 测试 helper**：trace 收集测试基础设施。
- **追加 A — 所有权转移事件 + generation 失效记录**：
  - `OwnershipTransfer` 事件：记录 flow state 所有权转移时刻（from_var → to_var，generation 失效精确位置）。
  - `LinearViolation` 事件：记录运行时 use-after-move 安全网诊断路径。
  - `compare_traces()` 扩展：generation_before/generation_after 参与比较（happens-before DAG generation 边）。
  - 3 个新单元测试。4104 测试全绿。
- 待实现：session/actor trace 记录、双后端 trace 比较测试。

### 0.31.16 Flow 状态 CFG 级线性（核心完成）

- **`is_linear()` 纳入 Flow 状态**：`FlowStateSet`（multi-target 结果）和 `state:` 前缀 Nominal（individual flow state）在 CFG dataflow 中是线性资源。
- **Auto-droppable**：Flow 状态代表数据，scope exit 时可安全丢弃（与 Cap/SessionChan 必须显式消费不同）。`ActionEmitter` 收集 flow state 局部变量作为 droppable 集合，`validate_return_resources` 跳过。
- **Transition `self` 隐式消费**：`build_resource_catalog` 和 `introduce_parameters` 跳过 transition 首参。
- **`_` 前缀 auto-drop**：`_d` 等 intentionally unused 变量不报 E0256。
- **`consumed_flow_vars` 保留为诊断增强层**：E0423 带 transition 名（比 CFG 的 E0304 更友好），CFG dataflow 是强制层。
- **Channel/Mutex/Atomic 遗留**：builtin 函数（整数 handle），非 ResolvedType Nominal，`is_linear()` 无法覆盖，留给后续类型表示升级。
- 4104 测试全绿。

### 0.31.19 攻击审查 I（完成）

- **审查范围**：0.31.16–18 闭环后的地基层（Flow 线性完备性、generation 失效、Actor×Flow 边界、Session×Flow 交互、双后端一致性、错误信息）。
- **P1 发现 + 修复**：tuple 构造 flow state 不消费原变量（`let t = (s0, 42); Counter::inc(s0)` 通过）→ `infer_tuple_expr` 加 `is_flow_state_type` 检查，E0427 拒绝。
- **线性完备性审查结果**（10 条攻击路径全部静态拒绝）：

| 攻击路径 | 诊断 | 层 |
|----------|------|-----|
| use-after-transition | E0423 | checker |
| alias chain (`let b = s0; let c = b; use(b)`) | E0423 | checker |
| self-loop double-use | E0423 | checker |
| function param move | E0304 | CFG |
| closure capture | E0427 | checker |
| list literal | E0427 | checker |
| map value | E0427 | checker |
| **tuple construction** | **E0427** | **checker (本次修复)** |
| shared/ref wrapping | E0427 | checker |
| shadowing no-reset | E0423 | checker |

- **错误信息质量**：E0423 带 transition 名 + help 文本；E0427 带类型名 + help 文本；E0304 (CFG) 无 transition 名（consumed_flow_vars 诊断增强层补充）。
- **Known limitation**：Channel/Mutex/Atomic 非 ResolvedType Nominal，is_linear() 无法覆盖；consumed_flow_vars 名字追踪保留为诊断层。
- **P0 = 0**。审查报告归档于 CHANGELOG。
- 4109 测试全绿。

### 0.31.18 证据同步与回归扫描（完成）

- **language-support.toml 全面更新**：implementation_version 更新至 0.1.1-dev (sprint 0.31.17)，8 个 requirement evidence 更新。
- **Clippy/fmt 修复**：`!is_ok()` → `is_err()`，`run_source_with_trace` dead_code allow。
- **回归扫描**：4108/0/10 全绿，clippy 0 warnings，fmt clean，real_world 70/70 run / 69/70 build。
- **Deferred 项清点（0.31.8–17）**：

| 项 | 来源 | 状态 | 去向 |
|---|---|---|---|
| progressive Main 真 lowering | 0.31.10 | 推迟 | 需 Resolved IR 级设计（broke 23 golden IR tests） |
| Codegen actor runs flow | 0.31.11 | 推迟 | 需 tagged-union state 存储设计 |
| cross-turn exactly-once | 0.31.13 | 推迟 | Flow transition 间资源跟踪，需 CFG 扩展 |
| Fault path 资源清理 | 0.31.13 | 推迟 | 需 Fault 路径 resource analysis |
| permission/effect 约束 | 0.31.14 | 推迟 | 需 Component IR (Phase C) |
| fault 暴露策略 | 0.31.14 | 推迟 | 需 Component IR (Phase C) |
| 版本握手 | 0.31.14 | 推迟 | 需 Component IR (Phase C) |
| session/actor trace 记录 | 0.31.15 | 推迟 | 需 interpreter session/actor 路径集成 |
| 双后端 trace 比较测试 | 0.31.15 | 推迟 | 需 codegen trace 收集 |
| Channel/Mutex/Atomic is_linear() | 0.31.16 | 降级 known limitation | builtin 函数（整数 handle），非 ResolvedType Nominal |
| consumed_flow_vars 删除 | 0.31.16 | 降级 known limitation | 保留为诊断增强层（E0423 带 transition 名） |

- **Consumer 迁移审计**：interp/codegen/verifier 三后端声明层（签名/Flow transition/Actor/Session/Protocol/ownership/CFG）从 CheckedProgram 安装；**函数体仍经 `legacy_body_file()` 消费 raw AST**（`interp/mod.rs:323`、`codegen/compile.rs:596`、`verifier/ctx.rs:1130`、`verifier/mod.rs:49,125`）。`legacy_body_file()` 为 `pub(crate)` 可见性，阻止 crate 外新 consumer 回退。函数体 Resolved IR 迁移按 body class 追踪于 0.31.8–0.31.19。

### 0.31.17 高阶交互闭环（完成）

- **闭包 × Flow**：lambda 内引用外层 flow state 变量 → E0427 拒绝（"linear resource cannot be captured by closure"）。`lambda_depth` + `lambda_param_names` 追踪，区分参数和 capture。Lambda 参数中的 flow state 合法。
- **集合 × Flow**：`[s0, s1]` list literal → E0427 拒绝。Map literal value 为 flow state → E0427 拒绝。
- **修复既有坏测试**：`flow_state_lambda_param_accepted`（fn 类型语法不合法）、`flow_state_in_set_rejected`（set{} 语法不存在）。
- 4108 测试全绿。

## [0.1.0] — 基线稳定 - 2026-07-23

### 止血 II 收尾 + 版本管理切换 + 架构重构

- **版本管理切换**：外部版本从 `mimi-v0.31.X` 切换为纯 semver（`0.1.0`、`0.1.1`、...、`1.0.0`）。旧 `mimi-v*` tag 保留为开发历史，不再新增。内部 sprint 仅体现在 commit message 中。
- **架构重构（0.1.0 收尾）**：
  - `src/runtime/mod.rs` 拆分（24105→18142 行，14 个模块）：regex/lexer/crypto/fs/binary_io/future/ffi_test/concurrency/actor/quote/net/shadow_mte/capability/env 抽出，机械拆分不改语义，419 个 `#[no_mangle]` 符号全导出、4053 测试绿。硬共享簇（map/set/list/string/json ~180 extern fn）函数交错且互引，作为耦合核心保留 mod.rs。
  - `src/core/resolved.rs` 拆分（12702→8551 行）：目录化为 resolved/mod.rs，`#[cfg(test)] mod tests`（4129 行）分离到 resolved/tests.rs。identity/catalog/walk 生产代码边界模糊且重度耦合，作为耦合核心保留 mod.rs。
- 止血 II 修复项（按信任链排序，逐项完成后登记）：
  - **F1 测试 oracle**：删除进程级 `GLOBAL_STDOUT_CAPTURE` 全局槽与 `resolve_stdout_buf` fallback，消除并行测试 stdout 串扰。
  - **silent error 止血**：codegen 12 处 `let _ = build_store/build_call` 改传播；`test_sandbox` spawn 失败如实报告。
  - **文档真值**：`AGENTS.md` §13/§0 重新对齐（函数体层仍经 `legacy_body_file()`、线性能力 0.1.1 前零强制）。
  - **CI 门禁**：`LLVM_SYS_181_PREFIX` 修正、clippy `--all-targets`、分级门禁、unsafe SAFETY baseline 锁定。
  - **测试质量**：清零走过场测试、`v1_4` 家族强制 L1、real_world golden（增量）。

> 开发历史：1863 commits，66 个 `mimi-v*` tag（v0.12.0–v0.31.6），38 天（2026-06-15 至 2026-07-22）。
> 详细施工记录见 `devdocs/archive/` 和 git log。

### 里程碑

- **CheckedProgram 语义中枢**：唯一语义真值源，持有 canonical 签名、Flow transition 表、Actor/Session/Protocol 目录、ownership action summaries、CFG。
- **Typed Resolved IR**：ResolvedFunction/ResolvedFlow/ResolvedTransition/ResolvedActor 等 canonical 声明（12.7k LOC）。
- **HM Unification**：undo trail + TypeScheme + zonk；泛型调用 fresh instantiate。
- **CFG/Ownership 分析**：per-callable 控制流图 + stable-ID CallableCfg + 线性资源 ledger（Introduce/Move/Drop/Return + borrow）。
- **止血 I/II**：测试 oracle 修复、silent error 传播、文档真值对齐、CI 门禁强化、Clippy 基线清零。
- **双后端等价**：4063 测试（4053 passed / 0 failed / 10 ignored），69 个 real_world 程序双后端 68/69 通过（`flow_test_macros.mimi` 为 interpreter-only，不参与双后端比对）。
- **Flow 范式**：38 项白皮书能力全部达成（v0.29 冻结），双后端 stdout 等价。
- **stdlib**：io/fs/strings/collections/json/csv/crypto/maps/mymath/net/time/datetime/env/testing/regex/template/set。
- **工具链**：mimi check/run/build/verify/fmt/lint/lsp/init/add/install/tree。

### 已知限制（0.1.0 基线）

- 线性能力仅有分析，零用户可见强制（exactly-once 闭环排入 0.1.1）。
- Flow 转移无原子 terminal model（atomic turn 排入 0.1.1）。
- Session 端点运行时可退化为整数（typed residual 排入 0.1.1）。
- Component IR / ABI / Wire 不存在（排入 0.1.1 内部 Phase C）。
- 函数体仍经 `legacy_body_file()` 消费 raw AST（迁移排入 0.1.1）。

---

## Pre-0.1.0 时代摘要

> 详细施工日志（v0.1.0–v0.31.6，1863 commits，66 个 `mimi-v*` tag）保留在 git 历史中
> （`git log -- CHANGELOG.md`），本地归档副本见 `devdocs/archive/CHANGELOG-pre-0.1.0.md`。

| 时代 | 版本范围 | 日期 | 主题 |
|------|---------|------|------|
| 原型 | v0.1.0–v0.7.0 | 06-15 ~ 06-17 | 解释器 + 类型检查器 + CLI 原型 |
| 筑基 | v0.12.0–v0.20.1 | 06-23 | 控制流、函数、类型系统、stdlib 基础 |
| 补全 | v0.21.0–v0.27.6 | 06-24 ~ 06-26 | JSON、LSP、pipe/loop、Z3 验证器、结构化并发、安全审计 |
| 使用驱动 | v0.28.0–v0.28.37 | 06-27 ~ 07-03 | 7 语言 FFI、profiler、bindgen、包管理器；Feature Bugs 清零 |
| Flow 范式 | v0.29.0–v0.29.41 | 07-03 ~ 07-12 | 编译器内部 Flow 替换（Parser→Lexer→Loader→LSP→Interp→Verifier→Checker）+ 语言级 Flow 语义 + 白皮书 38 项能力全部达成 |
| 止血 | v0.30.0 | 07-14 | 0 新 Feature — 15 项架构债务清零（sprintf→snprintf、路径安全、malloc 检查等） |
| 语义中枢 | v0.31.0–v0.31.6 | 07-15 ~ 07-22 | CheckedProgram / HM unification / CFG / Resolved IR / 止血 I/II → 汇入 0.1.0 |
