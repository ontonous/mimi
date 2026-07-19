# Mimi 真实代码可用性评估结果

**评估时间**：2026-07-19
**Mimi 版本**：0.31.3-dev
**最后更新**：2026-07-19
**评估命令**：`cargo test real_world_cli_suite -- --test-threads=1` + 定向双后端 smoke
**环境**：Ubuntu, LLVM 18 (via /tmp/llvm-wrapper), cc/gcc

## v0.31.3 CFG / ownership 收口门禁

- Cargo 自动发现的 real-world 套件全绿；每个非 interpreter-only 程序均执行 `mimi run`、`mimi build`、native executable 与 stdout 等价检查。
- `hm_core.mimi` 持续覆盖 canonical HM instantiate/constraint/zonk；`ownership_cfg.mimi` 已扩展到 branch/terminal CFG、nested field place、shared→mutable index NLL 与循环内 dynamic-index loan。
- `ownership_cfg.mimi` 实测 interpreter/native stdout 一致：`8` / `42`；native 定向 ABI 同时覆盖 nested field、tuple、constant index 与 dynamic index 真地址 write-back。
- CFG/resource L2 实测覆盖 stable block/edge identity、terminal predecessor、reachable join、partial consume、multiple shared loans、mutable read/write conflict、sibling place separation、edge-specific BorrowEnd 与 back-edge loan 拒绝。
- `flow_test_macros.mimi` 的 `assert_state`/`inject_fault` 契约保持 interpreter-only，Cargo runner 与 Python runner 均显式跳过 native build。
- 收口期间修复 nested-generic `>>` 导致 Flow parser 跳过下一 `pub`、generic body binder scope、lambda 尾 `if`、CheckedProgram bare-name method arity collision，以及 codegen intrinsic 未剥离 `Located`。

### 收口门禁状态

- 通过：`cargo fmt --check`、`cargo test --no-run`、language docs 31/31、CFG/core/ownership/borrow 聚焦测试、Z3 `v1_2_verification` 26/26（1 ignored）、`real_world_cli_suite`。
- 首次全量：3826 passed / 101 failed / 10 ignored。其中 stdlib JSON loader 失败由 nested CFG role 碰撞引起，已修复并增加回归；代表性复测确认余下失败分属既有 native if assignment、named-argument fail-closed、JSON 容器 ABI 和 verifier overflow/call-contract 基线。
- 未通过：Clippy 1.93 `--all-targets -D warnings` 在跨 runtime/codegen/resolved 既有代码上报 181 项 lint；未用全局 `allow` 掩盖。因此版本保持 `0.31.3-dev`，待这两项独立门禁清零后再执行 0.31.4-dev 切换。

### v0.31.4 基线分类（新设计为准）

| 失败簇 | 规范绑定 | 处理方式 |
|---|---|---|
| native `if`/nested assignment 轨迹不一致 | `TOOL-RESOLUTION-001` | typed local/place lowering 的真回归，不改期望值 |
| named/default argument 被 codegen fail-closed | `LANG-FUNCTION-001`, `TOOL-RESOLUTION-001` | checker 生成已排序 `ResolvedCall` |
| JSON/container 组合只有部分 native ABI | `TOOL-RESOLUTION-001`, `TOOL-SUPPORT-001` | stable 能力实现 typed lowering；非 stable 能力在 checker capability gate 拒绝 |
| verifier overflow/call-summary 旧期望 | `VERIFY-CORE-001` | 按 exact integer/definedness 改写；unsupported 返回 `NotInTrustedSubset` |
| parser/Flow AST 形状断言 | `TOOL-SUPPORT-001`, `SYNTAX-REMOVED-001` | 仅当旧断言与 normalized target 冲突时改写 |
| Clippy 1.93 存量 | RC tooling gate | 删除旧路径后局部修复，禁止 blanket allow |

从此表开始，不允许未分类失败进入 0.31.6 证据；修改旧测试必须在提交中引用对应 requirement ID。

## Flow 范式 MCDD — 阶段二+三 (v0.29.9–0.29.41)

| 指标 | 通过/总数 | 比例 |
|------|----------|------|
| flow_*.mimi 解释器 | 17 / 17 | 100% |
| flow_*.mimi build+exec | 17 / 17 | 100% |
| flow_*.mimi L1 双后端 stdout | 17 / 17 | 100% |

| 测试文件 | 解释器 | Build | 执行 | L1 | 覆盖版本 |
|---------|--------|-------|------|----|---------|
| flow_counter.mimi | ✅ | ✅ | ✅ | ✅ | 0.29.9 |
| flow_matrix_fault.mimi | ✅ | ✅ | ✅ | ✅ | 0.29.10 |
| flow_fault_absorb.mimi | ✅ | ✅ | ✅ | ✅ | 0.29.11 |
| flow_system_trace.mimi | ✅ | ✅ | ✅ | ✅ | 0.29.12→0.29.39 |
| flow_reset_recover.mimi | ✅ | ✅ | ✅ | ✅ | 0.29.13–14 |
| flow_pinned.mimi | ✅ | ✅ | ✅ | ✅ | 0.29.16→0.29.32 |
| flow_subflow.mimi | ✅ | ✅ | ✅ | ✅ | 0.29.17 |
| flow_protocol.mimi | ✅ | ✅ | ✅ | ✅ | 0.29.18→0.29.36 |
| flow_session.mimi | ✅ | ✅ | ✅ | ✅ | 0.29.19→0.29.34 |
| flow_peer_fault.mimi | ✅ | ✅ | ✅ | ✅ | 0.29.20 |
| flow_mailbox_bp.mimi | ✅ | ✅ | ✅ | ✅ | 0.29.21 |
| flow_progressive.mimi | ✅ | ✅ | ✅ | ✅ | 0.29.22 |
| flow_view_mutate.mimi | ✅ | ✅ | ✅ | ✅ | 0.29.23→0.29.33 |
| flow_spawn_quota.mimi | ✅ | ✅ | ✅ | ✅ | 0.29.24 |
| flow_broadcast.mimi | ✅ | ✅ | ✅ | ✅ | 0.29.25→0.29.35 |
| flow_actor_lifecycle.mimi | ✅ | ✅ | ✅ | ✅ | 0.29.37 |
| flow_test_macros.mimi | ✅ | ✅ | ✅ | ✅ | 0.29.38 |

## 白皮书 38 项能力覆盖表 (v0.29.41 冻结)

| # | 白皮书能力 | 实现版本 | 状态 | MCDD 测试 |
|---|-----------|---------|------|----------|
| 1 | Flow 声明 (state/transition) | 0.29.9 | ✅ | flow_counter.mimi |
| 2 | 转移矩阵自动补全 (+1 兜底) | 0.29.10 | ✅ | flow_matrix_fault.mimi |
| 3 | Fault 吸收态 + 资源析构 | 0.29.11 | ✅ | flow_fault_absorb.mimi |
| 4 | SystemTrace 溯源 | 0.29.12 | ✅ | flow_system_trace.mimi |
| 5 | Reset / Recover 系统动词 | 0.29.13 | ✅ | flow_reset_recover.mimi |
| 6 | Persistent Payload 事务一致性 | 0.29.14 | ✅ | flow_reset_recover.mimi |
| 7 | delegate view/mutate/consume | 0.29.15 | ✅ | flow_reset_recover.mimi |
| 8 | pinned { timeout } FFI 锚定 | 0.29.16→32 | ✅ | flow_pinned.mimi |
| 9 | Subflow 同步嵌套 | 0.29.17 | ✅ | flow_subflow.mimi |
| 10 | Protocol 接口抽象 | 0.29.18 | ✅ | flow_protocol.mimi |
| 11 | Session Types 骨架 | 0.29.19 | ✅ | flow_session.mimi |
| 12 | PeerFault 跨 Actor 传播 | 0.29.20 | ✅ | flow_peer_fault.mimi |
| 13 | Mailbox 背压自动治理 | 0.29.21 | ✅ | flow_mailbox_bp.mimi |
| 14 | 渐进式 Typestate | 0.29.22 | ✅ | flow_progressive.mimi |
| 15 | view/mutate 局部借用 | 0.29.23 | ✅ | flow_view_mutate.mimi |
| 16 | Spawn 配额控制 | 0.29.24 | ✅ | flow_spawn_quota.mimi |
| 17 | Flow 多态广播 dispatch | 0.29.25 | ✅ | flow_broadcast.mimi |
| 18 | Session 双端运行时 | 0.29.34 | ✅ | flow_session.mimi |
| 19 | Protocol VTable + broadcast Result | 0.29.35 | ✅ | flow_broadcast.mimi |
| 20 | Payload 协变 + 保守投影 | 0.29.36 | ✅ | flow_protocol.mimi |
| 21 | SystemKill 级联终止 | 0.29.37 | ✅ | flow_actor_lifecycle.mimi |
| 22 | spawn detached 守护进程 | 0.29.37 | ✅ | flow_actor_lifecycle.mimi |
| 23 | assert_state! 测试宏 | 0.29.38 | ✅ | flow_test_macros.mimi |
| 24 | inject_fault! 测试宏 | 0.29.38 | ✅ | flow_test_macros.mimi |
| 25 | MemoryDump 内存快照 | 0.29.39 | ✅ | flow_system_trace.mimi |
| 26 | PanicPayload 结构化栈 | 0.29.39 | ✅ | flow_system_trace.mimi |
| 27 | pinned 协作式超时看门狗 | 0.29.32 | ✅ | flow_pinned.mimi |
| 28 | view/mutate 深层 realloc 禁 | 0.29.33 | ✅ | flow_view_mutate.mimi |
| 29 | Session Open 通道通信 | 0.29.34 | ✅ | flow_session.mimi |
| 30 | broadcast PeerFault sentinel | 0.29.35 | ✅ | flow_broadcast.mimi |
| 31 | E0418 保守投影拒绝 | 0.29.36 | ✅ | (L2 unit test) |
| 32 | 线性类型推断 (multi-target) | 0.29.40 | ✅ | (L2 unit test) |
| 33 | Session 编译期顺序检查 | 0.29.19 | ✅ | flow_session.mimi |
| 34 | PeerFault 默认注入 | 0.29.20 | ✅ | flow_peer_fault.mimi |
| 35 | @mailbox(depth=N) 自动应用 | 0.29.21 | ✅ | flow_mailbox_bp.mimi |
| 36 | @max_children(N) 配额 | 0.29.24 | ✅ | flow_spawn_quota.mimi |
| 37 | W011 渐进迁移诊断 | 0.29.22 | ✅ | flow_progressive.mimi |
| 38 | Flow 收尾回归门禁 | 0.29.26 | ✅ | real_world_flow_dual_backend_suite |

## 历史评估（v0.28.30-era 基线）

## 汇总

| 指标 | 通过/总数 | 比例 |
|------|----------|------|
| 解释器 (`mimi run`) | 35 / 35 | 100% |
| Codegen build (`mimi build`) | 35 / 35 | 100% |
| 编译后执行 | 35 / 35 | 100% |

## 详细结果

| 测试文件 | 解释器 | Build | 执行 | 备注 |
|---------|--------|-------|------|------|
| concurrency_actor.mimi | ✅ | ✅ | ✅ | Actor spawn + method call |
| concurrency_atomic.mimi | ✅ | ✅ | ✅ | AtomicI32 load/store/fetch_add |
| concurrency_channel.mimi | ✅ | ✅ | ✅ | Channel send/recv |
| concurrency_mutex.mimi | ✅ | ✅ | ✅ | Mutex lock/get/set/unlock |
| concurrency_spawn_await.mimi | ✅ | ✅ | ✅ | `spawn` / `await` Future |
| core_basic_control.mimi | ✅ | ✅ | ✅ | while / for / if |
| core_closures.mimi | ✅ | ✅ | ✅ | `fn` 闭包变量调用 |
| core_enums_match.mimi | ✅ | ✅ | ✅ | enum + match |
| core_functions_recursion.mimi | ✅ | ✅ | ✅ | 递归函数 |
| core_generics_adt.mimi | ✅ | ✅ | ✅ | 泛型 ADT 构造与字段访问 |
| core_list_index.mimi | ✅ | ✅ | ✅ | List 索引 |
| core_newtype.mimi | ✅ | ✅ | ✅ | newtype + 模式匹配 |
| core_option_result.mimi | ✅ | ✅ | ✅ | Option / Result 方法 |
| core_records.mimi | ✅ | ✅ | ✅ | record 类型 |
| core_shared_weak.mimi | ✅ | ✅ | ✅ | shared / weak / upgrade |
| core_traits_methods.mimi | ✅ | ✅ | ✅ | trait impl + 方法分发 |
| core_try_operator.mimi | ✅ | ✅ | ✅ | `?` 运算符 |
| meta_comptime_quote.mimi | ✅ | ✅ | ✅ | comptime 函数求值 |
| meta_contracts.mimi | ✅ | ✅ | ✅ | requires / ensures |
| std_collections.mimi | ✅ | ✅ | ✅ | map_list/filter_list/reduce_list + 内置 reduce/map/filter lambda |
| std_crypto.mimi | ✅ | ✅ | ✅ | hex 验证 |
| std_csv.mimi | ✅ | ✅ | ✅ | CSV parse/get |
| std_datetime.mimi | ✅ | ✅ | ✅ | datetime 工具 |
| std_env.mimi | ✅ | ✅ | ✅ | env / cli args |
| std_fs.mimi | ✅ | ✅ | ✅ | 文件写入 + 读取内容 + 内容相等断言 |
| std_io.mimi | ✅ | ✅ | ✅ | print_raw / print_line |
| std_json.mimi | ✅ | ✅ | ✅ | from_json + to_json 标量/List/Record |
| std_maps.mimi | ✅ | ✅ | ✅ | map_new / set / get / has_key |
| std_mymath.mimi | ✅ | ✅ | ✅ | math 函数 + -lm |
| std_prelude.mimi | ✅ | ✅ | ✅ | prelude 自动加载函数 |
| std_set.mimi | ✅ | ✅ | ✅ | Set 字面量 + 方法 |
| std_strings.mimi | ✅ | ✅ | ✅ | strings 模块函数 |
| std_template.mimi | ✅ | ✅ | ✅ | simple_render 可调用 |
| std_time.mimi | ✅ | ✅ | ✅ | timestamp / sleep |
| projects/consumer/main.mimi | ✅ | ✅ | ✅ | `use mylib::func` 包导入 |

## 修复记录

### 1. 泛型 ADT 构造推断（`core_generics_adt.mimi`）

**问题**：CLI 类型检查器无法推断 `type Box<T> { value: T }; let b = Box { value: 42 }`，报错 `field 'value' expected T, found i32`。

**根因**：
- `infer_record_expr` 未将类型参数 `T` 实例化为 unification 变量，字段值类型无法反推类型参数。
- `infer_field_access_on_type` 返回字段原始类型 `T`，未根据对象类型 `Box<i32>` 实例化。
- `cargo test` 中的 `compile_and_run` / `run_source` 跳过 `core::check`，因此该问题被隐藏。

**修复**：
- `src/core/infer/record.rs`：为 record 构造时的每个类型参数分配 fresh `TypeVar`，用字段值与字段期望类型做 `unify`，并返回 resolve 后的具体类型。
- `src/core/infer/access.rs`：字段访问时，根据对象类型的类型实参替换字段类型中的类型参数。
- `src/core/unification.rs`：将 `occurs_in` 暴露为 `pub(crate)`，供 record 构造使用。

## 已关闭的 codegen 差距

1. **std_fs.mimi**（v0.28.26）：`compile_read_file` 重构为返回 `Result<string, string>` 类型结构，支持 `match read_file(path) { Ok(content) => len(content) }`。包含错误处理（fopen 失败返回 Err）。
2. **std_crypto.mimi**（v0.28.26）：`hex_encode` codegen 段错误已修复（`hex_digit` 改用 `str_substring`，字符串字面量改为正规化 struct 表示）。
3. **from_json::<List<i32>> codegen**（v0.28.26）：支持 `from_json::<List<i32>>("[1,2,3]")` 反序列化 JSON 数组为 Mimi List，使用 `json_array_length`/`json_get_element`/`mimi_json_as_i64` 运行时函数逐元素解析。
4. **string struct 解包修复**（v0.28.26）：`compile_getenv`/`compile_lexer`/`compile_parse`/`compile_assert` 改用 `extract_raw_str_ptr` 支持 `{i8*, i64}` string struct。
5. **compile_to_string StructValue**（v0.28.26）：`to_string` 接受 `StructValue` 直接返回（string 已是 string）。
6. **from_json::<List<f64/bool>> codegen**（v0.28.26）：支持 `from_json::<List<f64>>` 和 `from_json::<List<bool>>`，f64 用 bitcast，bool 用 i64 0/1。
7. **i1 零扩展修复**（v0.28.26）：`promote_binop_operands` 中将 i1 改为零扩展而非符号扩展，避免 `true`（i1 1）变成 -1。
8. **to_json List<T> codegen**（v0.28.26）：新增 `mimi_list_i64_to_json` / `mimi_list_f64_to_json` / `mimi_list_bool_to_json` / `mimi_list_str_to_json` 四个运行时函数，类型感知的 `to_json` 分发覆盖 `i32`/`i64`/`f64`/`bool`/`string` 元素。
9. **reduce 类型推断**（v0.28.26）：`core/infer/call/simple.rs` 中的 `reduce` 分支改为推断并返回初始值类型，不再返回 `unknown`。内置 `reduce(nums, fn(a, e) { a + e }, 0)` 现在通过类型检查。
10. **to_json Record type codegen**（v0.28.26）：在 `compile_call`（simple.rs）中添加 Record 类型检测，通过 `sprintf` 逐字段序列化为 JSON 对象。支持 `string`、`i32`/`i64`、`bool`、`f64` 字段类型。字段按字母序排列，与解释器 `serde_json::Map` 一致。
11. **from_json::<List<RecordType>> codegen**（v0.28.26）：支持 JSON 对象数组反序列化为 `List<T>`，`T` 为 record 类型。每个元素堆分配 struct 并通过 `ptrtoint(i64)` 存储在列表 data 数组中。
12. **std_template any_to_string codegen**（v0.28.26）：`mimi_any_to_string` 运行时 helper 启发式判断 `ValueHandle` 是指针还是整数，支持 `Any` 类型的 `to_string`。

## 结论

- 解释器路径：35/35 通过，可运行 500–1000 行级真实程序（mimi-log、mimi-httpd、mimi-markdown）。
- codegen 路径：35/35 通过，但大程序（>~200 行）仍有 P0 阻塞点。
- **test suite 内所有已知 codegen 差距已关闭**：Any to_string、from_json/to_json List<RecordType>、std_fs/crypto/template、泛型 ADT/newtype/trait self/包导入。
- 下一步应修复大程序 codegen P0 阻塞点（Result<string> 方法分发、LLVM physreg、ADT/SEGFAULT），使 mimi-kv/mimi-httpd/mimi-log 可编译。
