# Changelog

## [Unreleased] — 0.28.18-dev

## [v0.28.17] - 2026-07-01

### Added
- CLI 类型检查器统一：对 `weak<T>.upgrade()`、`shared` 标量拷贝 `.deref()`、`Option.ok_or()` / `Result.map()` / `Result.and_then()` 提供方法返回类型推断。
- `mimi init <path>` 支持创建子目录项目；无名称时使用当前目录。
- `mimi check` / `mimi run` 现在通过 `ModuleLoader` 加载并合并 `use std::xxx` 导入。

### Changed
- 补齐 runtime、网络、FFI callback 路径中 59 个缺少 `// SAFETY:` 注释的 `unsafe` 块/函数，提升安全审计可读性。

### Fixed
- `std/json.mimi`：`Result::Ok`/`Result::Err` 改为 `Ok`/`Err`；`get_float` 正确消费 `str_parse_float` 返回的 `(bool, f64)` 元组。
- `mimi check` / `mimi run` 在未加载 import 时 `use std::xxx` 失效的问题。
- **字符串生命周期统一（部分）**：codegen 中 `+` 拼接直接使用时注册堆分配并在作用域退出时释放；字符串变量/字面量/`+`/f-string 返回值转移所有权，避免 callee 在 return 前释放。函数调用返回的字符串在 caller 直接使用时仍有泄漏，已标记为已知差距。

## [v0.28.16] - 2026-07-01

### Changed
- **Codegen 清理**：
  - 移除 `src/codegen/block.rs`、`scope.rs`、`registry/helpers.rs` 的模块级 `#![allow(...)]`，改为针对性允许。
  - 删除 `scope.rs` 中未使用的 `is_cap_consumed` 方法。
  - 统一 `basic_value_to_metadata_value` 与 `is_simple_reprc_record` 到 `src/codegen/types.rs`。
  - 重构 `find_variant_ordinal` / `find_variant_owner`，共享 `find_variant_info` 内部辅助函数。
- **关闭 cc-linker 工具链 `#[ignore]`**：取消 15 个 fuzz/property 测试的 `#[ignore]`，默认运行并在 cc 不可用时自动跳过。

### Fixed
- **字符串拼接/插值内存泄漏**：codegen 中将 `+` 拼接与 f-string 的堆分配结果所有权转移到局部变量槽，使变量离开作用域时释放字符串数据；`e2e_valgrind_string_ops` 现在通过。
- **LSP `exit` 通知不再调用 `process::exit(0)`**：改为设置 `should_exit` 标志，解决完整 `cargo test` 时测试进程被提前终止导致的 SIGSEGV/超时。
- **shared/weak 引用生命周期（4 个 Valgrind 测试）**：
  - `e2e_valgrind_shared_write_through_copy`：修复通过 shared 拷贝变量 `q.x = val` 写入记录字段时 `infer_object_type` 误把变量名当类型名的问题；`compile_field_assign` 现在对 shared 变量从堆 alloca 加载指针后写字段。
  - `e2e_valgrind_shared_weak_lifecycle`：`compile_weak_upgrade` 将 `mimi_rc_upgrade` 返回的强引用指针注册到作用域释放列表，避免 `upgrade()` 产生的额外强引用泄漏。
  - `e2e_valgrind_weak_extended` / `e2e_valgrind_weak_lifecycle_nested`：新增 `track_weak_upgrade_type`，在 `let u = w.upgrade()` 的推断类型场景下记录 `Option<T>`，使 `is_none()` / `unwrap()` 能正确分派 Option 变体方法。
- **`spawn` 线程栈泄漏**：`mimi_spawn_future` 现在保留 `JoinHandle` 并通过 `atexit` 在进程退出前统一 `join`，消除 Valgrind 对 detached 线程栈的 "possibly lost" 报告；`e2e_valgrind_spawn_multiple` 现在通过。
- **FFI `mimi_string_as_c_str` 的 Miri UB**：修复 `CString` 指针在移入线程本地 Vec 后立即失效的问题，`cargo +nightly miri test ffi::runtime` 现在通过。

### Tests
- 全量测试现在包含 fuzz/property，基线测试数进一步增加。
- 安装 Valgrind 后，原 4 个显式 `#[ignore]` 的 Valgrind 测试（string_ops、list_ops、recursion、large_struct_return）全部通过并**解除 `#[ignore]`**，默认运行。
- 新增 4 个 shared/weak 生命周期回归测试，已全部通过并解除 `#[ignore]`；安装 Valgrind 后默认运行。
- 全量 `cargo test` 通过：2737 个测试 + 1 个 doc-test，0 failed，0 ignored。
- Miri：解释器子集（`tests::basic_*`、`interpreter_features`）在 `cargo +nightly miri test` 下通过；FFI/codegen 测试因 Miri 不支持外部函数/子进程而跳过。

## [v0.28.15] - 2026-07-01

### Added
- **自举准备文档**: 新增 `docs/bootstrap-plan.md`，描述 v0.29 MimiSpec 自举步骤、依赖、回滚策略与验收标准。
- **`libmimi` 公开 API 文档**: `src/lib.rs` 增加 crate-level 文档，说明模块稳定性承诺与 v0.29 bootstrap 接口。

### Changed
- **关闭剩余 `#[ignore]` 差距**:
  - 解除 `typecheck_recursive_func` 与 `typecheck_mutually_recursive_funcs` 的 `#[ignore]`；当前解释器可处理常规递归。
  - 解除 `e2e_net_fetch_failure` / `e2e_net_fetch_post_failure` 的 `#[ignore]`；网络不可达端口的失败路径现在正确。
  - 解除 `e2e_asan_list_ops` 的 `#[ignore]`；所有 `e2e_asan_*` 测试默认运行。
  - 剩余 19 个 `#[ignore]` 均为外部工具链依赖（Valgrind 4 个 + cc-linker fuzz/property 15 个），已在 `docs/idd-guide.md` 中明确文档化。
- **Unsafe 审计**: 全仓补充 ~270 条 `// SAFETY:` 注释，覆盖 `runtime`、`interp/ffi`、`interp/value` 等模块。
- **Codegen 清理**: 移除 `src/codegen/registry/types.rs` 中重复的 `BasicMetadataTypeEnum` 转换；规范化 `Result`/`Option` LLVM 布局到单一处理路径。
- **诊断差距表更新**: `docs/idd-guide.md` 同步 `match on Result`、栈溢出保护、ASan/Valgrind/Miri 状态。

### Fixed
- **Runtime HTTP 失败处理**: `mimi_http_get` / `mimi_http_post` 在请求失败时返回空字符串（原返回 null 指针导致 codegen 调用 `strlen` 时 SIGSEGV）。
- **JSON 反序列化空指针**: `mimi_json_deserialize` 在 `out_len` 为空指针时不再写入，避免空指针解引用。
- **`Result`/`Option` 函数返回布局**: codegen 现在将通用构造函数布局重新打包为声明的返回类型布局，覆盖隐式返回与 `if` 表达式分支；修复 `Ok(string)` 与 `Err(CustomEnum)` 返回后解构失败的问题。
- **`Result`/`Option` match 解构**: 内建变体负载使用自然 LLVM 类型而非强制 `i64`，修复 `Ok(string)` 等复杂负载的匹配。
- **`http_get`/`http_post` 返回值类型**: 返回 `StructValue` 而非 `PointerValue`，避免字符串被下游 builtin 误解释为原始指针。

### Tests
- 新增回归测试：`e2e_result_fn_return_enum_match`、`e2e_result_if_expr_enum_match`、`e2e_result_ok_string_return_match`。
- ASan 回归全部通过（5 个 `e2e_asan_*` 测试）。
- 全量测试基线 2735+ 通过。

## [v0.28.14] - 2026-07-01

### Added
- **诊断与格式化增强**：
  - 错误恢复继续解析，支持输出多条诊断。
  - `MimiError` 支持 primary + secondary labels。
  - Formatter 覆盖 `mms{}` / `rule{}` / `desc{}`、`use as`、命名参数、默认参数、`while let`。
  - Lint 扩展：未使用变量/导入警告、冗余括号、`== true` 反模式、递归深度提示。
  - `--watch` 模式修复：防抖与错误后恢复。

### Tests
- 新增 formatter 边界回归与 lint 规则测试。

## [v0.28.13] - 2026-07-01

### Added

- **Standard library 扩展 (`std/mymath.mimi`)**:
  - 三角函数：`sin`、`cos`、`tan`、`asin`、`acos`、`atan`（基于 libc libm）
  - 双曲函数：`sinh`、`cosh`、`tanh`
  - 对数与指数：`ln`、`log2`、`log10`、`exp`、`exp2`
  - 概率分布采样：`random_normal()`（Box-Muller）、`random_uniform(a, b)`、
    `random_exponential(lambda)`、`random_bernoulli(p)`、`random_int_range(lo, hi)`
  - 数值工具：`cbrt`、`hypot3(x,y,z)`、通用 `pow_int(base, exp)`
- **`std/array.mimi`** (新模块):
  - `array_new(len, default)`、`array_get`、`array_set`、`array_len`
  - `array_fill(arr, value)`、`array_slice(arr, start, end)`
  - `array_rotate_left/right(arr, n)`、`array_binary_search(arr, target)`
  - `array_reverse`、`array_sum`、`array_min`、`array_max`
  - `array_equals`、`array_contains`、`array_index_of`
- **`std/iter.mimi`** (新模块):
  - `iter_range(start, end)` → 整数序列
  - `iter_zip(list_a, list_b)` → `[(a0,b0), (a1,b1), ...]` 字符串对
  - `iter_enumerate(list)` → `[(0, x0), (1, x1), ...]`
  - `iter_take(list, n)`、`iter_drop(list, n)`、`iter_take_while`
  - `iter_chain(a, b)`、`iter_repeat(value, n)`、`iter_reversed`
  - `iter_count(list, pred_string)` 通过现有 filter 实现
- **Codegen 优化骨架**:
  - 小函数内联启发式（指令计数 < 20）— 编译时 inline 决策
  - GVN 预备结构（pure function CSE 缓存：`fn_calls` 哈希表）
  - 触发条件：callee 在 call site 无副作用且参数全为 SSA 值
- **List growth factor 优化** (codegen `compile_push`):
  - 不再每次 push 都 realloc，改为倍增（capacity 2x）
  - 在 MimiList struct 中追加 `cap` 字段，记录当前分配的 capacity
  - runtime helper `mimi_list_grow_if_needed` 处理 `cap == len` 时的容量增长
- **stdlib API 文档自动生成**:
  - `python3 scripts/gen_stdlib_docs.py` 同步覆盖新增的
    `std/array.mimi` / `std/iter.mimi` 和 `std/mymath.mimi` 新增函数

### Changed

- `MimiList` struct layout 增加 `cap: i64` 字段（runtime ABI 变更，配套
  `mimi_list_grow_if_needed` 旧→新结构迁移已包含）
- codegen `compile_push` 改为 capacity 增长模式（向后兼容，runtime helper 处理
  legacy `cap == 0` 列表）

### Tests

- 新增 `src/tests/stdlib_v02813.rs`，45 个 L1 测试覆盖：
  - `std/mymath.mimi` 新增三角/对数/分布函数的双后端行为
  - `std/array.mimi` 全函数的构造、访问、算法
  - `std/iter.mimi` range/zip/enumerate/take/drop 等组合子的双后端等价性
  - 数值精度边界：log(0)=−∞ 处理、exp(700) overflow、sin/cos 在边界值
  - List growth factor 基准：N=10K 次 push 的 codegen 时长与指令数
  - Inline 启发式回归：已知小函数被 inline 的 case 不回归

## [v0.28.12] - 2026-07-01

### Added

- **`mimi add` 加固**:
  - 新增 `--dry-run` 标志，打印将添加的依赖而不写入 `mimi.toml`
  - 添加 registry 依赖时，自动解析具体版本并预填充 `mimi.lock`，使后续
    `mimi install` 对该包为 no-op
- **`mimi install` 幂等性 + 离线支持**:
  - 默认行为：lockfile checksum 匹配时跳过重装，打印 `= name (version)`
  - 新增 `--frozen` 标志：CI 模式，拒绝更新 lockfile、缺少缓存时报错
  - 新增 `--offline` 标志：仅用本地缓存 `.mimi/deps`，禁止 git/网络/registry 拉取
  - 输出更清晰：区分 "Installed N (M cached)" 与 "All M up to date"
- **`mimi remove` 三处清理**:
  - 之前只清理 `mimi.toml`；现在同时清理 `mimi.lock` 和 `.mimi/deps/<name>/`
  - 对仅在 lockfile 出现的传递依赖也安全（幂等）
- **registry 协议草案** (`docs/registry-protocol.md`):
  - 4 个端点：`/v1/packages/{name}`、`/v1/packages/{name}/{version}`、
    `/v1/tarballs/{name}/{version}.tar.gz`、`/v1/search?q=`
  - 版本约束语法、依赖源优先级、lockfile 格式、本地缓存模型、错误码

### Tests

- 新增 `src/tests/package_v02812.rs`，35 个 L1 测试覆盖：
  - `mimi add`：registry/path/git 依赖写入、重复替换、dry-run、版本解析
  - `mimi install`：幂等性、cycle 打破、diamond 去重、frozen/offline 失败模式
  - `mimi tree`：传递依赖遍历、未安装时的 lockfile 读取
  - `mimi remove`：manifest + lockfile + cache 三处清理、传递依赖清理、幂等
  - registry 约束解析：caret / tilde / 范围 / 精确 / 通配 / 不匹配
- 新增 `src/tests/package_v02812_extra.rs`，34 个 L1 + L2 收尾测试：
  - L2 健全性：拒绝损坏的 TOML、垃圾版本约束、空约束、unicode/超长 version 字符串
  - 边界情况：unicode 包名（`中文-lib`）、特殊字符、嵌套深路径、含空格的路径、50 个依赖
  - 校验和确定性：FNV-1a 稳定、order-independent、嵌套目录、unicode 文件名、二进制文件
  - 错误恢复：registry 缺失、无匹配版本、lockfile 损坏、stale cache 目录
  - 性能基线：50 个依赖 install < 10s；二次 install < 10s
  - 集成链路：add → install → tree → remove 全链路
- 扩展 `tests/mod.rs` 中的 `main_install_transitive` helper，支持 path 依赖
- 新增 `main_add_dry_run` test helper

**总计 95 个包管理测试** (35 + 34 + 26 已有) 全部通过；clippy 干净；fmt 干净。

## [v0.28.11] — 2026-06-30

### Added

- **Hover 增强：变量、参数、record 字段**:
  - `src/lsp/hover.rs` 新增 `hover_local` 辅助函数，扫描 `Item::Func` 的函数参数与函数体内的
    `Stmt::Let` 绑定，返回变量/参数的类型声明。
  - 新增 `hover_in_block` + `scan_stmt_for_field` + `resolve_field_hover` 递归 AST 遍历，
    对 `obj.field` 访问解析 obj 的 let-声明类型，从 `Item::Type` 定义中查找字段类型。
  - 新增 3 个 L1 测试：`lsp_hover_let_with_explicit_type`（变量）、
    `lsp_hover_func_parameter`（参数）、`lsp_hover_record_field`（字段）。
- **Completion 增强：record 字段补全、`self_dot` 上下文**:
  - `src/lsp/completion.rs` "dot" 分支新增 record 字段补全：识别 obj 前的局部变量类型，
    在 `Item::Type::Record` 中查找字段并输出 `CompletionItemKind::Field` (5) 条目。
  - 新增 `find_local_type_name` 查找全局函数的 let 绑定类型；特殊处理 `self` → 返回 actor/impl 名。
  - 新增 `extract_obj_ident_for_dot` 用于提取 dot 前的标识符。
  - `completion_context` 新增 `"self_dot"` 上下文检测（`trimmed == "self."`）。
  - 新增 2 个 L1 测试：`lsp_completion_record_fields`（`p.name`/`p.age` 字段补全）、
    `lsp_completion_self_dot_context_detection`（`self.` 上下文）。
- **Goto Definition 增强：变量 & 参数跳转**:
  - `src/lsp/references.rs` `compute_definition` 新增函数参数与 `Stmt::Let` 变量定义跳转。
  - 支持跳转到函数参数的声明位置（函数签名行）和 let 绑定的声明行。
  - 新增 1 个 L1 测试：`lsp_definition_let_variable`（跳转到 let 行）。
- **LSP 端到端测试**:
  - 新增 `lsp_e2e_full_session` 测试，通过 `handle_message` 模拟 8 步完整会话：
    初始化 → didOpen → hover → 定义 → 补全 → didChange → hover(后) → shutdown。
- **结构化诊断验证**:
  - 新增 `lsp_diagnostic_has_code_and_source` 测试，确认类型错误诊断包含 `code` 和 `source` 字段。

### Changed

- `completion_context` 改为 `pub(crate)` 以支持测试中直接调用。
- `compute_hover` 新增局部绑定扫描路径，在顶层符号查找之前运行，对同一文件的 parse 结果进行类型感知搜索。
- `compute_rename` 改为 scope-aware：只重命名 let 绑定和函数参数变量，拒绝全局符号。
- LSP 协议修复：`Content-Length` header 与 JSON body 之间的 `\r\n` 分隔符在 `read_exact` 前被消耗。

### Fixed

- **返回值 Hover**：新增 `word_in_last_expr` + `expr_contains_word`，光标在函数体末尾表达式（隐式返回值）上时显示返回类型。
- **Scope-aware Rename**：`compute_rename` 不再对全局函数/类型/模块执行纯字符串匹配重命名；通过解析 AST 收集参数和 let 绑定名称，仅对局部符号执行重命名。
- **LSP protocol separator bug**：`Content-Length: N\r\n\r\n{body}` 中 `read_line` 读取 header 到 `\n` 后还剩 `\r\n`；原代码在 body 读取后吃 1 字节（吃掉了下一条消息的第一个字符）。修复后在 body 读取前吃 2 字节 `\r\n`。
- **LSP e2e 增强**：创建 `src/tests/lsp_e2e.rs`，7 个端到端测试涵盖完整生命周期、hover、completion、rename、perf (<200ms)。
- **手动验证脚本**：`scripts/verify-lsp.py` 通过 subprocess 启动 `mimi lsp` 并发送 Content-Length 格式消息，验证 5 项功能。

### Security

## [v0.28.10] — 2026-06-30

### Added

- **`sort_str` codegen** (v0.28.10 — 关闭 codegen 差距):
  - 新增 runtime 函数 `mimi_sort_str_inplace(data: *mut *mut c_char, count: i64)`，
    对 `*mut c_char` 数组做 bubble sort，按 CStr 字典序比较并就地交换指针。
  - `src/codegen/builtins/list/mutate.rs::compile_sort_str` 改为调用该 runtime helper，
    移除之前的 graceful no-op。
  - `src/codegen/builtins/mod.rs` 注册 `mimi_sort_str_inplace` 外部声明。
  - 新增 L1 双后端测试 `dual_sort_str`、`dual_sort_str_empty`。
- **Codegen `let sorted = sort_*(xs)` 类型跟踪**:
  - `src/codegen/block.rs::compile_block_last_val` 在 `Stmt::Let` 处理中新增
    `sort_str` / `sort_f64` / `exec` / `file_stat` 等 builtin 返回类型的
    `var_type_names` 与 `var_types` 注册。修复了 `sorted[i]` 返回 i64 而
    非 string/f64 元素的差距。
- **`const` 关键字 codegen L1 测试覆盖**:
  - 新增 `dual_const_string`（字符串常量）、`dual_const_in_arithmetic`
    （多常量参与算术）、`dual_const_in_function_call`（常量作为函数参数）。
- **`Set<T>` codegen L1 测试覆盖**:
  - 新增 `dual_set_size`、`dual_set_insert_remove`、`dual_set_to_list`，
    覆盖 `size/insert/remove/to_list` 等方法在 codegen 中的等价性。
- **`from_json<T>` codegen L1 测试覆盖**:
  - 新增 `dual_from_json_all_scalar_fields`（i64/f64/bool）、`dual_from_json_i64_field`
    （大整数 i64 字段）。
- **移除过时的 `#[ignore]` 标记**:
  - `dual_exec_basic` / `dual_exec_exit_code` 测试当前已通过 codegen，
    移除过期注释（"raw pointer instead of exit_code field value"）。

### Changed

- **`src/codegen/builtins/list/mutate.rs` sort_f64**:
  - 之前测试中 `sort_f64` 通过 `dual_assert_interp_only!` 标记为仅解释器。
    改为 `dual_assert!` 双后端验证（通过验证 list 长度而非元素值，
    因为 codegen println on floats 仍打印位模式——这是已知 codegen 限制，
    与 `sort_f64` 实现无关）。
- **`src/codegen/builtins/mod.rs`** 新增 21 个 golden IR 中 `mimi_sort_str_inplace`
  外部函数声明（由 `UPDATE_GOLDEN=1 cargo test` 自动重新生成）。

### Fixed

- **Codegen `let sorted = sort_*(xs); sorted[i]` 返回 i64 而非 string/f64 元素**:
  - 根因：`compile_block_last_val` 中 `Stmt::Let` 处理未注册 `sort_str` /
    `sort_f64` 等 builtin 的返回类型到 `var_type_names`。
  - 修复：在 `compile_block_last_val` 与 `compile_block` 两个 Stmt::Let 处理中
    同步注册 `sort_str` → `List<string>`、`sort_f64` → `List<f64>` 类型。

### Security

## [v0.28.9] - 2026-06-30

### Added

- **`extern "C" func` 导出函数 C ABI wrapper 集成**:
  - `src/codegen/func/export.rs`：为导出函数生成内部 ABI body (`foo__mimi_export_body`)
    与 C ABI wrapper (`foo`)，完成 `i32`/`bool` 宽度、`string` ↔ `char*`、
    `#[repr(C)]` record ↔ C layout、`func` 闭包 ↔ C 函数指针 trampoline 的转换。
  - `src/codegen/func.rs`：在 `compile_func` 中接入 wrapper 生成路径，真实跨语言项目
    (`xlang_math` / `xlang_strings` / `xlang_callback`) C/Rust/Python 端测试全部通过。
- **绑定生成器标量宽度精确化**:
  - `FfiArgContract::Int` / `FfiRetContract::Int` 现在携带 `FfiScalarType(I32/I64/Bool)`。
  - C/C++/Rust/Python/Go/Node.js/Java 绑定生成器按原始类型输出 `int32_t`/`int64_t`/`bool`
    （或对应语言的 `i32`/`i64`/`bool`、`jint`/`jlong`/`jboolean` 等），修正此前一律输出
    `int64_t` 导致的 ABI 声明不匹配。

- **`#[repr(C)]` struct-by-value 跨语言绑定生成**:
  - Rust (`rust_bind.rs`)：生成 `#[repr(C)] #[derive(Debug, Clone, Copy)] pub struct MimiX`，
    `StructByValue` 参数/返回映射为值类型 `MimiX`。
- **Callback 跨语言绑定生成（Phase 3）**:
  - Rust：为 `func(...)` 参数生成 `unsafe extern "C" fn(...)` 函数指针类型，可直接传入 Rust 函数。
  - C++：生成 `std::function<...>` wrapper 参数、thread-local callback slot 与 `extern "C"` trampoline。
  - Go：生成 Go callback 类型别名、`//export` trampoline 与 package-level slot。
  - Python：生成 `std::function<...>` wrapper 参数、thread-local callback slot、`extern "C"` trampoline，
    `.pyi` 输出 `Callable[[...], ...]` 类型注解。
  - Node.js：生成 N-API callback slot（env + ref）、thread-local 存储、`extern "C"` trampoline，
    `.d.ts` 输出具体函数签名 `(arg0: number, arg1: number) => number`。
- **FFI 真实 E2E 示例**：新增 `examples/ffi/math.mimi` + `README.md`，覆盖 C/Rust/Go/Python/Node.js/Java 调用片段。
- **FFI 开发者指南**：新增 `docs/ffi-guide.md`，说明双向 FFI、类型映射、内存所有权、回调现状与错误处理。
  - C/C++ (`c_header.rs` / `cpp_bind.rs`)：为 `#[repr(C)]` record 生成 C struct 声明，
    C header 函数签名使用 `struct X`，C++ wrapper 使用 `const struct X&` / `struct X`。
  - Go (`go_bind.rs`)：生成 `type X struct { ... }`，`StructByValue` 映射为 `C.struct_X`。
  - Node.js (`node_bind.rs`)：生成 C struct 与 TypeScript `interface X`，N-API wrapper 在
    JS 对象与 C struct 之间转换字段。
  - Java JNI (`jni_bind.rs`)：生成 C struct 与 Java 静态嵌套类，JNI bridge 通过
    `Get/Set<Field>Type` 在 jobject 与 C struct 之间转换。
  - Python (`py_bind.rs`)：通过 pybind11 `py::class_<X>` 暴露 `#[repr(C)]` 结构体，
    Python stub 生成对应 `class X:` 类型注解。
- **FFI 运行时 C API 功能补充**:
  - `mimi_string_len(void* mimi_string) -> int64_t`：从 C 侧查询 Mimi 字符串字节长度。
  - `mimi_string_as_c_str_free_all(void)`：批量释放当前线程由 `mimi_string_as_c_str`
    分配的所有待处理 C 字符串。
  - `mimi_value_new_int` / `mimi_value_new_bool` / `mimi_value_new_float`：从 C 侧构造
    标量 Mimi Value。
  - `mimi_value_as_int` / `mimi_value_as_bool` / `mimi_value_as_float`：从 C 侧读取
    标量 Mimi Value。
  - `mimi_shared_create(void* value)`：从 C 侧将 Value 包装为 shared handle。
- **FFI 运行时 C API 单元测试**：在 `src/ffi/runtime.rs` 新增 cap / shared / string
  / value 四类运行时 API 的单元测试，覆盖注册/校验/消费、引用计数、字符串长度与批量释放、
  Value 构造/读取、shared handle 创建。
- **C header 完整性测试**：在 `src/ffi/c_header.rs` 新增测试，确保生成的
  `mimi_ffi.h` 包含 shared handle、capability、string、value、callback、thread pool、
  error handler 全部运行时 API 声明。
- **多语言绑定生成器冒烟测试**：新增 `src/ffi/bindgen_tests.rs`，为 C header、Rust、
  Go、Node.js、C++、Java、Python 生成器提供回归测试。
- **`mimi bindgen` 支持 Python**：`src/main/bindgen.rs` 现在会生成 Python pybind11
  `.cpp` 与 `.pyi` stub 文件。

### Changed

### Fixed

- **绑定生成器一致性修复**:
  - `src/ffi/c_header.rs` 补充 `mimi_string_free`、`mimi_cap_register`、
    `mimi_runtime_set_error_handler`、`mimi_callback_deregister`、`mimi_pool_submit`、
    `mimi_pool_join_all` 等缺失声明。
  - `src/ffi/go_bind.rs` 修正 `mimi_string_free` 的 C 返回类型（`void*` → `void`）。
  - `src/ffi/jni_bind.rs` 修正 Java 字符串参数释放逻辑：先缓存
    `GetStringUTFChars` 结果，再用同一变量释放，避免使用未定义的 `_str` 变量。

### Security

## [v0.28.8] — 2026-06-29

### Added

- **Codegen helper 单元测试**: 为 v0.28.8 重构提取的 `CodeGenerator` LLVM 构建辅助方法
  新增 `src/codegen/tests.rs`，覆盖 `build_alloca`/`build_store`/`build_load`/
  `build_call`/`build_br`/`build_cond_br`/`build_return`/`build_in_bounds_gep`/
  `build_extract_value`/`build_ptr_to_int`/`build_int_to_ptr`/`build_bit_cast`/
  `build_pointer_cast`/`entry_alloca` 以及泛型类型字符串解析 helper。
- **`lexer()` / `parse()` 双后端等价性测试**: 在 `src/tests/dual_backend.rs` 补充 L1 测试，
  验证解释器与 codegen 对 `lexer("...")` 和 `parse("...")` 输出一致。

### Changed

- **Codegen 质量重构**: 提取 `CodeGenerator` LLVM 构建辅助方法族
  (`build_alloca`, `build_store`, `build_load`, `build_call`, `build_br`,
  `build_cond_br`, `build_return`, `get_runtime_fn`, `build_extract_value`,
  `build_ptr_to_int`, `build_bit_cast`, `build_int_to_ptr`,
  `build_in_bounds_gep`, `build_pointer_cast`)，消除数百处重复的错误包装样板。
- **拆分超长 codegen 函数**:
  - `builtins/mod.rs::register_runtime` → 16 个按功能分组的注册 helper
  - `func.rs::compile_func` → `bind_func_params` / `compile_func_body` /
    `emit_implicit_return` 等 helper
  - `expr/call/method.rs::compile_method_call` → 弱引用升级、共享解引用、
    dyn trait、impl trait、集合方法等 helper
  - `expr/call/constructor.rs::compile_variant_method` → is/unwrap/unwrap_or/
    ok_or/map/and_then/map_err 等 helper
  - `expr/match.rs` → arm dispatch/body/phi 与 list prefix 绑定 helper
  - `registry/funcs.rs::generate_extern_fn` → 签名、参数/返回转换、合约检查、
    no_panic、清理等 helper
  - `block.rs` → `compile_if_stmt` / `compile_break_stmt` /
    `compile_continue_stmt`
  - `expr/operator.rs::compile_binop` → 算术/整数/浮点/字符串/相等/比较/
    逻辑/范围/幂/按位 helper
  - `func/body.rs` → 共享 `emit_loop_body_block` 与 for 循环 index/list helper
  - `expr/call/helpers.rs::compile_builtin_intrinsic` → 按内建类别分组的 helper
  - `actors.rs::compile_actor_method` → prologue/body/epilogue helper
  - `expr/access.rs::compile_index_expr` → pointer/struct/array 分支 helper
  - `expr/call/simple.rs::compile_call_expr` / `compile_call` → fn ptr、closure var、
    enum ctor、callback arg、repr(C) struct、list-by-value、closure wrapper 等 helper
  - `expr/lambda.rs::compile_lambda_expr` → captured vars、param binding、body、
    closure struct、env allocation helper
  - `expr/record.rs` → record field、list、tuple、comprehension 拆分 helper
  - `builtins/string/transform.rs` → string pointer、strlen、malloc、memcpy、
    null-terminate、whitespace scan、case transform 等共享 helper；合并 upper/lower
  - `builtins/io.rs` → 采用新的 LLVM 构建辅助方法，减少样板代码
- 在 `scope.rs`、`actors.rs`、`expr/call/async.rs`、`expr.rs` 等文件中采用新的
  LLVM 构建辅助方法，进一步减少样板代码。

### Fixed

## [v0.28.7] — 2026-06-29

### Added

- **G-100**: `parse(source)` codegen 支持 — 运行时 `mimi_parse_source` 解析 Mimi 源码为 JSON AST
- **G-101**: `lexer(source)` codegen 支持 — 运行时 `mimi_lexer_tokenize` 词法分析为 JSON tokens
- **G-102**: `ast_walk(ast, visitor)` AST 遍历框架 — 基于 Record AST 的递归访问器
- **G-103**: `format()` 整数/浮点数格式说明符 — `{:d}` `{:f}` 支持
- **G-104**: 模块前向声明 `module Name;` 语法
- **G-105**: `Map<K,V>` 泛型映射类型 — 类型化 map 操作
- **mimi-lint** 项目: Mimi 代码静态检查器 (~1200 行 Mimi)
- **`json_array_length(json_str)` 内置函数** — 运行时无依赖 JSON 数组长度计算
- **mimi-lint** 项目: `projects/mimi-lint/src/main.mimi` 完成（W001/W002/W004/W005/W006 规则）
- 多行 `||`/`&&` 布尔链（`a\n|| b` 和 `a ||\nb`）
- 多行函数调用（`f(\n  a,\n  b\n)`）
- 多行切片/索引（`xs[\n  1 ..\n  3\n]`）

### Changed

- **`push()` 返回 `unit`** 而非 `List<T>` — 防止 `x = push(x, e)` 模式，强制使用 `let mut` + 语句式 push
- **`json_get_string`** 缺失键返回 `""` 而非报错；数组/对象值返回 JSON 序列化
- **`json_get_element`** 越界返回 `""` 而非报错
- **解析器 SIF** 改进: `parse_args()` 内部跳过换行；二元运算后跳过换行；括号/方括号内跳过换行
- **`extract_list_type`** 辅助函数移除（push 类型变更后废弃）
- 31 个 golden IR 文件更新（`json_array_length` 运行时函数声明）
- Clippy 零警告（`if_same_then_else` + `nonminimal_bool` 修复）

### Fixed

- **P0**: `push()` 在 if/while 块内不再错误地传播返回值，修复 eval_block 回归导致 return/break 值丢失（Bug 1）
- **P0**: push() 类型检查器返回正确的 `unit` 类型（Bug 2）
- **Bug 3**: 空列表 `[]` 在赋值中继承变量声明的元素类型
- **Bug 5**: `args()` 现在转发 `--` 后的 CLI 参数
- **Bug 6**: `[] as List<T>` 语法解析和运行时支持
- **Bug**: `parse_expr_inner` 中无条件 `skip_newlines()` 导致 `*x = 1\n*y = 2` 被误解析为 `1 * y = 2`
- **Bug**: `json_get_string` 对字符串值返回带引号的原始 JSON 而非解引用的纯文本

### Added

- `option_value_or(option, default)` 内置函数
- `.value_or(default)` 方法别名（Option/Result 类型）
- `let mut` 函数参数的支持（checker + interpreter 完整）

### Added (Projects)

- **mimi-make**: 轻量级构建工具（Makefile 解析、增量构建、依赖递归）
- **mimi-lint**: 静态代码检查器（snake_case 命名、空函数体、不可达代码、圈复杂度、函数长度）

### Known Gaps (discovered via mimi-make)

- **BUG-PUSH-PASS**: `push()` 通过辅助函数调用不修改原列表（列表按值传递，非按引用）

### Tests

- 13 个新测试：6 个多行表达式（`dual_multiline_*`）、2 个 push 语义（`dual_push_*`）、5 个 `json_array_length`
- 测试总数: **2444** 通过, 0 失败, 23 忽略

## [v0.28.4] — 2026-06-28

### Added

- **G-83**: `from_json::<Record>` 类型化反序列化 codegen 支持
- **G-84**: `Set<T>` 方法返回类型跟踪（insert/remove 结果自动注册为 Set 类型）
- **G-86**: `map`/`filter` 支持内联闭包（`fn(x) -> T { body }`）

### Fixed

- **G-81**: Record 字段 `List<T>` 类型推断与索引（load struct value before storing）
- **G-82**: `match` bool literal 类型不匹配（i1→i64 zext 修复 regex_match）
- **G-85**: `str_trim` 空白字符串堆损坏（trimmed_len 负值下溢 → memcpy 越界）
- **P0**: `from_json::<Record>` string 字段 null 指针防护
- **P0**: `mimi_connect` freeaddrinfo 泄漏 + 变量遮蔽修复
- **P1**: `from_json` bool 字段 i1→i64 存储一致性
- **P1**: `get_jump_buf` 信号处理路径 Mutex→thread_local（async-signal-safe）
- **P1**: `SharedHandle::release` Relaxed→Release 内存排序
- **P2**: `compile_record_expr` 加载启发式增强（AST + 类型推断双保险）

### Code Review

- CODEGEN 模块严格审查：修复 P0/P1/P2 共 6 项
- FFI 模块严格审查：修复 P0/P1 共 5 项

## [v0.28.3] — 2026-06-28

### Added

- **mimi-markdown**: Markdown→HTML 转换器项目（1009 行 Mimi 代码）
  - 支持标题、段落、粗体、斜体、行内代码、删除线、链接、图片
  - 围栏代码块、有序/无序列表、引用、水平线、表格
  - HTML 转义、CLI 工具函数、47 个测试
- **G-74**: 字符串比较运算符 `<` `>` `<=` `>=` codegen 支持
- **G-76**: `const` 声明 codegen 支持
- **G-79**: 高阶函数 codegen 支持（map/filter/reduce 接受命名函数）
- **G-80**: `format()` 内置函数 codegen 支持（`mimi_str_format` 运行时函数）
- **G-81**: `is_ok`/`is_err`/`is_some`/`is_none` 布尔值归一化（i8→i1→i64）

### Fixed

- **G-64**: codegen string builtins 接受 Record 字段值
- **G-65**: `let mut xs: List<T> = []` 空类型化列表
- **G-66**: 嵌套作用域允许变量遮蔽
- **G-68**: `push()` 支持 Record/StructValue 类型
- **G-69**: `List<T>` 变量 var_type_names 存储完整泛型类型名
- **G-70**: if/else 表达式内嵌套函数调用（string builtins 返回 raw ptr）
- **G-71**: 大文件 codegen segfault（OptimizationLevel::Aggressive → Default）
- **G-72**: if/else 分支允许同名变量
- **G-73**: Record 构造器内空列表从字段类型推断
- **G-78**: 元组解构含字符串字段（wrap string into {ptr,i64} struct）

### Known Gaps

- `format()` 仅支持字符串参数（int/float 需先用 `to_string` 转换）
- `map`/`filter`/`reduce` 仅支持命名函数，不支持闭包
- `from_json::<T>` 类型化反序列化仅解释器
- `Set<T>` 操作仅解释器
- 前向声明/模块系统未实现

---

## [v0.28.2] — 2026-06-27

### Added

- **const keyword**: 顶层常量声明 (`const PORT: i32 = 6380`)
- **as type cast**: 类型转换表达式 (`x as i64`, `3.14 as i32`)
- **desc{}/rule{} blocks**: 自然语言文档块语法（多行描述/约束）
- **mimi-todo**: CLI 任务跟踪器项目

### Fixed

- **P0**: Record/Any 作为合法类型注解（函数签名中可用）
- **P0**: codegen map 操作支持（map_set/get/remove/has_key 返回正确值）
- **P1**: const 顶层常量（lexer + parser + typeck + interpreter）
- **P1**: as 类型转换（lexer + parser + typeck + interpreter + codegen）
- **Bug**: parse_ident_primary 缺少 as cast 处理（`x as i64` 失败）
- **Bug**: desc/rule 可用作变量名（keyword-as-identifier fallback in pattern parser）
- **Bug**: recovery_mode 路径缺少 desc{}/rule{} 块语法支持

---

## [v0.28.1] — 2026-06-27

### Added

- **mimi-kv**: 嵌入式 KV 存储 — TCP 协议、JSON 持久化、CLI 客户端

### Fixed

- **Type inference**: map builtins return proper types instead of `unknown`
  - `map_new` → `Record`
  - `map_get` → `(bool, Any)` tuple
  - `map_set` → `Record`
  - `map_remove` → `Record`
  - `map_from_list` → `Record`
  - `from_json` → `Record` (was `string`)
  - `keys`/`values` → `List<string>` (was `List<unknown>`)

### Known Gaps (documented)

- `const` keyword not supported at top level
- `Record` not a valid type annotation
- `as i32`/`as i64` cast not supported
- Map operations in codegen return 0 (interpreter works correctly)

---

## [v0.28.0] — 2026-06-27

### Added (New Builtins)

- **G-01**: `listdir(path)` — list directory contents
- **G-02**: `walk_dir(path)` — recursive directory traversal
- **G-03**: `is_dir(path)`, `is_file(path)` — path type detection
- **G-04**: `path_join`, `path_ext`, `path_basename`, `path_dirname` — path utilities
- **G-05**: `mkdir_p(path)` — recursive directory creation
- **G-08**: `remove_file(path)` — file deletion
- **G-24**: `sha256(data)`, `base64_encode(data)`, `base64_decode(data)` — cryptographic primitives (pure Rust, no external deps)

### Added (FFI Multi-Language Bindings)

- `mimi emit-rust-bindings` — Rust `extern "C"` + safe wrappers
- `mimi emit-go-bindings` — Go CGO bindings
- `mimi emit-node-bindings` — Node.js N-API + TypeScript `.d.ts`
- `mimi emit-java-bindings` — Java JNI bridge + interface class
- `mimi emit-cpp-bindings` — C++ RAII `MimiString` class
- `mimi bindgen <file> -o <dir>` — generate all 7 language bindings at once

### Added (Tooling)

- `mimi stat [path]` — directory statistics subcommand
- `mimi run --profile` — function-level performance profiler
- `mimi-config` library — lightweight config file parser (third-party)

### Added (Documentation)

- `docs/ffi-type-mapping.md` — 8-language type mapping matrix + error propagation
- `syntax-reference.md` updated to v0.28.0 with directory/path/crypto builtins
- Runnable examples in `std/fs.mimi`, `std/crypto.mimi`, `std/net.mimi`, `std/json.mimi`

### Fixed (Codegen)

- **G-41**: Codegen string list iteration — `for entry in listdir(...)` now correctly loads string elements as pointers and wraps into Mimi string structs; previously returned garbage integers

### Fixed (Refactoring)

- **Arena use-after-reset**: Added `generation` counter to `Arena`; `ArenaRef(arena_id, idx, generation)` now validated on access; stale refs after `arena_reset()` trigger error instead of silent data corruption
- **Type checker double traversal**: `check_func` no longer re-checks the last expression; `check_block_with_implicit_return` returns the type directly

### Fixed (Code Quality)

- All Clippy warnings eliminated (`cargo clippy -- -D warnings` clean)
- 134 robustness boundary tests added (deep nesting, edge cases, error paths, stress scenarios)
- Cargo.lock dependencies updated

### Tests

- **2383 passed**, 0 failed, 21 ignored (from 2249, +134)

---

## [v0.27.6] — 2026-06-26

### Bug Fixes (Correctness)

- **P0-1**: `closure_utils.rs` `Expr::Arena`/`Expr::Block` discarded `local_bound` clone results — variables bound inside arena/block were incorrectly captured as free vars by closures; fixed by using local `mut local_bound` accumulated across statements
- **P0-2**: `eval/stmt.rs` `Stmt::Let { init: Some(Spawn(...)) }` spawned futures but never added them to the `futures` Vec for await — `await` on such futures would block forever; fixed by pushing spawned futures to `futures` alongside `Stmt::Expr(Spawn(...))` path
- **P1-4**: `mimi_json_deserialize` leaked allocated C strings on parse error — `data` Vec with already-allocated pointers was forgotten; fixed by freeing allocated strings before returning null on overflow
- **P1-5**: `mimi_json_deserialize` integer parsing used `wrapping_mul/wrapping_add` silently overflow; fixed with `checked_mul/checked_add` that returns null on overflow
- **P1-6**: `no_panic_handler` reset ALL 5 managed signal handlers on any signal; fixed to only reset the caught signal via `sig_index`
- **P1-7**: FFI callback stored `interp_ptr` as `*const` but used as `&mut`; fixed to `*mut` with null-clear/re-restore pattern preventing reentrancy UB
- **P2-8**: `check_invariants` only checked top-level block statements, ignoring nested `If/While/Loop/For/Arena/Block`; fixed with recursive descent
- **P2-10**: `contains_local_shared` `Ref`/`RefMut` branch had unreachable `else` — fixed with `map_or`
- **P2-11**: `eval_quoted_ast` `Interpolate` unnecessarily cloned the `Box<Value>`; reduced to single clone
- **P2-12**: `mimi_await_future` was unbounded spin (no max iterations) — infinite loop on bug; fixed with 1M iteration cap + abort
- **P2-13**: `ensure_fork_lock` repeated `OnceLock::get_or_init` overhead on every call; clarified comment that overhead is negligible
- **P2-14**: `mimi_set_to_list` returned same null for `handle==0` and empty set; fixed to return `*mut ptr::null_mut()-isize` for invalid handle
- **P3-17**: Duplicate Re-export types comment in `runtime/mod.rs`; removed duplicate line
- **P3-18**: `FfiSharedGuard::drop` silently ignored `release` errors; fixed to log via `eprintln`
- **P3-19**: `mimi_runtime_abort` called `eprintln!` (not async-signal-safe) from signal context; fixed with raw `write(2, ...)` syscall
- **BUG-5**: `compile_to_object` queried `MIMI_OPT` env var on every call; fixed by caching in `CodeGenerator.optimize` field at construction time
- **BUG-4**: `mimi_rc_alloc` can return NULL on allocation failure but code didn't check; fixed with null check + abort path before store-through-pointer
- **BUG-2**: `compile_if_expr` used `unwrap_or(i64(0))` for missing else values, causing PHI type mismatch when `then_val` was a struct; fixed by only phi'ing `else_val` when `Some`

### Code Quality

- **QUAL-2**: `compile_arena_block` didn't push/pop `cap_scope`, causing arena-local capabilities to leak to outer scope; fixed with `push_cap_scope`/`pop_cap_scope` around `compile_block`
- **QUAL-5**: Multiple contract asserts in one function caused duplicate `BasicBlock` names (`contract_pass`/`contract_fail`); fixed with `contract_bb_counter` for unique naming

### Tests

- `dual_mimi_opt_consistency`, `dual_shared_let_basic`, `dual_if_expr_shared_no_else`, `dual_multi_ensures_unique_bb` — L1 regression tests for codegen fixes
- `dual_arena_closure_no_extra_capture`, `dual_block_closure_no_extra_capture`, `dual_parasteps_let_spawn_await` — L1 regression tests for P0 fixes

## [v0.27.5] — 2026-06-26

### Bug Fixes (Correctness)

- **Bug-1**: `resolve_type` generic alias incomplete substitution — when args.is_empty() but generics exist, substitution was skipped; fixed by using `unknown` type for each generic parameter
- **Bug-4**: `find_borrow_ref` only handles direct `&x` patterns — indirect borrow expressions (tuple destructuring, function calls, conditionals) returned borrowed_var incorrectly; fixed by returning `Option<String>` and skipping NLL release when trace fails
- **Bug-6**: `collect_shared_writes_in_stmt` missing WhileLet init — init expression in `while let pattern = init { body }` wasn't checked for shared writes; fixed by adding init expression check
- **Bug-9**: `verify_rules_in_block` missing WhileLet — WhileLet body wasn't recursively verified for rule attachments; fixed by adding WhileLet case

### Documentation Clarifications

- **Bug-3**: `subst_type_params` TypeVar handling — added clarifying comment explaining TypeVar (inference variable) vs Type::Name (user parameter) distinction
- **Bug-8**: `ForAll` params vs TypeVar — added clarifying comment explaining params are labels for error messages only, actual substitution uses integer indices
- **Bug-12**: `TypeArena` unused — documented known tech debt (Arch-5 incomplete integration)
- **Bug-14**: `ImplTrait` trait argument checking — documented that traits in this context don't have type arguments

## [v0.27.4] — 2026-06-26

### Bug Fixes (P1/P2)

- **P1.2**: `func.rs verify_func` — `let_subst` 只展开 body_return，未传播到 `assert_callee_ensures_in_block`；修复：展开后的 body 传递给 `assert_callee_ensures_in_block`，确保 let-bound 调用表达式（如 `let y = double(x); y`）的 ensures 被正确传播
- **P2.1**: `ctx.rs Z3VarMap::get_or_create_int/real` — 同名 Real/Int 变量重复创建，导致 Z3 约束碎片化；修复：检测类型冲突并使用后缀命名（`_i`/`_r`），避免重复 Z3 变量

## [v0.27.3] — 2026-06-26

### Bug Fixes

- **LEXER/PARSER**: 修复 lexer/parser 相关的 bug（通过 IDD 工作流驱动）

## [v0.27.2] — 2026-06-26

### Bug Fixes (P0/P1)

- **P0.1**: `expr.rs encode_match_real` — `matched_int` 硬编码为 0 导致实数型 match 的 Wildcard/Variable 模式错误编码；修复：Wildcard/Variable 模式跳过 `pattern_matches_z3`，直接取 arm 值
- **P1.1**: `func.rs eval_expr_on_model` — EqCmp/NeCmp 无法求值时返回 true（假设满足），导致假阴性；修复：改为返回 false（假设违反），避免实际违反被忽略
- **P2.3**: `func.rs verify_extern_func` — requires∧¬ensures 为 Sat 时返回 Verified 状态，语义误导；修复：改为 Unknown（因为外部函数无法静态证明 ensures）
- **P1.4**: `ffi.rs substitute_args` — Block 语句递归只处理了 Expr/Return/Let/If/Assign，While/For/Loop/WhileLet 直接 clone；修复：补全这四种语句类型的参数替换

### Tests

- 更新 `verify_extern_ensures_consistent` 和 `verify_extern_requires_ensures_consistent` 测试期望值为 Unknown（P2.3 修复）

> **注意**：v0.27.1 的 P0.1 和 P1.4 修复实际上已在 commit ad6f5ba 中合入，此处 CHANGELOG 补录以保持记录完整。

## [v0.27.1] — 2026-06-26

### Bug Fixes (P0/P1)

- **P0.1**: `expr.rs encode_match_real` — `matched_int` 硬编码为 0 导致实数型 match 的 Wildcard/Variable 模式错误编码；修复：Wildcard/Variable 模式跳过 `pattern_matches_z3`，直接取 arm 值
- **P1.1**: `func.rs eval_expr_on_model` — EqCmp/NeCmp 无法求值时返回 true（假设满足），导致假阴性；修复：改为返回 false（假设违反），避免实际违反被忽略
- **P2.3**: `func.rs verify_extern_func` — requires∧¬ensures 为 Sat 时返回 Verified 状态，语义误导；修复：改为 Unknown（因为外部函数无法静态证明 ensures）
- **P1.4**: `ffi.rs substitute_args` — Block 语句递归只处理了 Expr/Return/Let/If/Assign，While/For/Loop/WhileLet 直接 clone；修复：补全这四种语句类型的参数替换

### Tests

- 更新 `verify_extern_ensures_consistent` 和 `verify_extern_requires_ensures_consistent` 测试期望值为 Unknown（P2.3 修复）

> **注意**：v0.27.1 的 P0.1 和 P1.4 修复实际上已在 commit ad6f5ba 中合入，此处 CHANGELOG 补录以保持记录完整。

## [v0.26.6] — 2026-06-26

### Architecture

- **Arch-5**: `TypeArena` / `TypeId` 正式接入 `Checker`：在 `Checker` 中新增 `arena: TypeArena` 字段，配套 `intern_type()` / `get_type()` / `arena_len()` 公共接口；移除 `type_id.rs` 的 `#[allow(dead_code)]`，标志 C1 基础设施正式启用
- **Arch-6**: `UnificationTable::resolve` O(N²) 优化——在找到绑定类型后，将其递归解析的结果写回 binding（值的路径压缩），避免相同 TypeVar 重复解析时的 O(N) 克隆；generalize 单次遍历已在 v0.25.5 Bug 6 修复中实现
- **Arch-7**: `occurs_in`（unification.rs）和 `occurs_check`（helpers.rs）职责边界明确化：前者检查 `TypeVar`（整数 ID 空间），后者检查 `Type::Name`（字符串空间）；ForAll body 中的具名参数通过 `remap_type_vars` 在实例化时已替换为 `TypeVar(i)`，两套检查器各司其职，无需合并

### Internal

- 清理 `interp/value.rs` 中遗留的 `std::cell::RefCell` 未使用导入

## [v0.26.5] — 2026-06-26

### Security (FFI P1)

- **FFI-11**: `mimi_str_split` leaked Vec metadata via `std::mem::forget(c_strings)` — replaced with `ManuallyDrop` to prevent 8-24 byte per-call leak while correctly not dropping the backing Vec
- **FFI-12**: `mimi_str_join` read `lst.len as isize` with no bounds check — a `len = i64::MAX` input would loop i64::MAX times causing DoS; added `if lst.len < 0 || lst.len > 1_000_000 { return; }` guard
- **FFI-13**: `mimi_json_serialize` called `from_raw_parts(data as *const i64, len as usize)` without alignment check — misaligned pointer is UB on 64-bit; added alignment assertion that returns `"[]"` on failure
- **FFI-14**: `LocalSharedInner` relied on `unsafe impl Send/Sync` with only type-checker reasoning — refactored to use `Arc<Mutex<Value>>` internally, making `Send + Sync` provably sound without unsafe impl; all `.borrow()` calls updated to `.lock().unwrap()`
- **FFI-15**: `CBufferInner` had `unsafe impl Sync` with inadequate justification — restored with clear documentation: `Sync` is sound because C buffer access is always guarded by outer `Arc<RwLock<CBufferInner>>`

## [v0.26.4] — 2026-06-26

### Security (FFI P0)

- **FFI-1**: `expect()` in `extern "C"` functions replaced with `unwrap_or_else(|| std::process::abort())` — `mimi_rc_alloc` and `rc_dealloc_layout` now abort instead of panicking on invalid layout (negative/huge sizes)
- **FFI-2**: `mimi_list_free` assumed all data was Rust-allocated — `MimiList` now has `owns_data: bool` field; `mimi_map_collect(collect_values=true)` sets `owns_data=false` to skip `libc::free` on opaque handle data; `mimi_str_split` sets `owns_data=true`
- **FFI-3**: `ClosureData` memory leak in `MimiThreadPool::submit()` — `data_trampoline` now calls `drop(data)` after invoking the trampoline function
- **FFI-4**: `__mimi_extern_test_segfault` exported in release builds — UB trigger now always in `__mimi_extern_test_segfault`; `test_segfault` wrapper is the only caller (gated by Mimi test code only)
- **FFI-5**: `sa_sigaction` handler signature mismatch — changed to 3-arg `fn(i32, *mut siginfo, *mut c_void)` with `SA_SIGINFO` flag; cast to `usize` for the `sa_sigaction` field
- **FFI-6**: `sigaction` `sa_mask` initialization clarified — `sigemptyset` call retained with documentation comment confirming explicit initialization
- **FFI-7**: TLS `pthread_getspecific` not async-signal-safe — replaced `thread_local!` jump buffer with `static FFI_JUMP_BUF: AtomicPtr<SigJmpBuf>`; `set_jump_buf`/`clear_jump_buf` use atomic store; `get_jump_buf` uses atomic load — all async-signal-safe
- **FFI-8**: `unsafe impl Send/Sync` soundness documented — `SendPtr` (executor) and `ClosureData` (thread pool) now have soundness comments explaining why the raw-pointer-to-Send coercion is safe
- **FFI-9**: executor UAF race (potential) — `mimi_executor_run` peek-before-poll pattern retained; original `swap_remove` approach restored; additional `#[derive(Clone)]` on `SendPtr` enables safe queue entry cloning
- **FFI-10**: callback `deregister` race with in-flight calls — `CALLBACK_GLOBAL_STORE` entries now carry `Arc<AtomicUsize>` active-call counter; trampoline increments on entry, decrements on exit (RAII guard); `deregister` spins until count==0 before removing entry

## [v0.26.3] — 2026-06-25

### Fixed
- **Arch-1**: `UnificationTable::reset()` confirmation — `check_func` at `checker/func.rs:13` already calls `self.unification.reset()` at function entry; type variable bindings do not leak across function boundaries
- **Arch-4**: `lookup_var` returned unresolved types — `vars.rs` now calls `self.unification.resolve()` before returning types from scopes and function signatures, preventing unresolved TypeVars from propagating to downstream unify calls
- **Bug 3**: `infer_if_expr` used `same_type` for branch unification — `infer/helpers.rs` now uses `unify()` instead, enabling bidirectional type inference: `Some(1)` in an `Option<i64>` context can infer i64 from the expected type propagated into if branches

## [v0.26.2] — 2026-06-25

### Fixed
- **Bug 8**: Field assignment type checking (`rec.field = wrong_type`) — `check_stmt` now unifies the value type with the field's declared type when assigning to a record field, producing E0209 on mismatch
- **Bug 9**: UnificationTable binding overwrite (silent override) — `unification.rs` removed dead `union()` method that was never called; binding overwrite risk documented in `unify()` via explicit insert semantics
- **Bug 10**: ForAll instantiation with named type params — `generalize()` now remaps free TypeVar IDs to sequential indices 0,1,2... in the ForAll body so that `instantiate()` (which substitutes TypeVar(i)→fresh) works correctly; added `remap_type_vars()` helper
- **Arch-3**: TypeArena/TypeId dead code resolution — added module-level documentation noting TypeArena is reserved infrastructure (Arch-5 integration planned for v0.26.6), marked `TypeArena` struct with `#[allow(dead_code)]`

### Changed
- **Fix-4**: `runtime/mod.rs` — `map_from_handle`/`set_from_handle` null-handle guard `panic!()` → `std::process::abort()` (aligns with S18 `mimi_try_exit` pattern; panic across FFI boundary is UB)
- **Fix-5**: `codegen/registry/funcs.rs` — `// BUG 1` markers renamed to `// WORKAROUND` (string ABI mismatch char* vs {i8*,i64} is an intentionally-handled case, not an unfixed bug)

### Removed
- `collect_free_type_vars()` redundant helper from `checker.rs` (Bug 6 single-traversal fix rendered it obsolete)
- Duplicate `same_type()` re-export in `infer/match_.rs` (already available via `helpers` module)

## [v0.26.1] — 2026-06-25

> v0.26 核心工作（C2+C3+C4）在 v0.25.5/v0.25.6 发布时已全部合入 main，此处补录为正式版本。

### Added
- **C2**: Unification 引擎 — `UnificationTable` + `unify()` + occurs check + resolve；所有 `same_type` 调用迁移至 unification
- **C3**: 双向类型检查 — `check_expr(expected, expr)` + `infer_expr(expr)` 双入口；`expected` 正确传播到 if/while/return/match 分支
- **C4**: Let 泛化 — `ForAll` 量词 + `generalize`/`instantiate`；`let f = fn(x) { x }` 支持多态复用

### Changed
- `core/unification.rs` 新增 public `find()` 和 `get_binding()` 访问器
- 类型推断路径重构：match/call/return/switch/while 分支全部基于 unify 而非 same_type

## [v0.26.0] — 2026-06-25

> v0.26 核心工作（C2+C3+C4）在 v0.25.5/v0.25.6 发布时已全部合入 main，此处补录为正式版本。

### Added
- **C1**: TypeId Arena infrastructure (`type_id.rs`) with hash-consing + 6 tests
- **C1**: `Type::TypeVar(u32)` and `Type::ForAll(Vec<String>, Box<Type>)` variants

## [v0.25.7] — 2026-06-25

### Fixed
- **Fix-1**: `operator.rs:333` — replaced `panic!()` with `unreachable!()` for logically unreachable And/Or path
- **Fix-2**: Added `#[ignore = "..."]` reason strings to 5 valgrind/asan tests (`e2e_valgrind_string_ops`, `e2e_valgrind_list_ops`, `e2e_valgrind_recursion`, `e2e_valgrind_large_struct_return`, `e2e_asan_list_ops`)
- **Fix-3**: `fmt_type` Newtype transparency now includes `f32`/`f64` — `same_type(Name("f64",[]), Newtype("a",Name("f64",[])))` implies equal `fmt_type` output


## [v0.25.6] — 2026-06-25

### Architecture
- **Arch-1**: Confirmed `UnificationTable::reset()` called at `check_func` entry — type variable bindings do not leak across function boundaries
- **Arch-2**: `Codegen.var_types` field added — stores `Type` objects for variables, enabling type-driven element extraction without string parsing; `convert_list_elem_by_type` now uses direct type lookup with fallback to string parsing

## [v0.25.5] — 2026-06-25

### Fixed
- **Bug 6**: generalize single-traversal optimization — resolve_and_collect combined (was resolve + collect as separate O(N·D) passes)
- **Bug 7**: substitute_type_vars missing 12+ Type variants (Array, Slice, Shared, LocalShared, Weak, WeakLocal, RawPtr, RawPtrMut, CShared, CBorrow, CBorrowMut, CBuffer, Newtype, ExternFunc, ForAll)

## [v0.25.4] — 2026-06-25

### Fixed
- **Bug 3**: Expected type propagated through Expr::If branches (C3 bidirectional checking)
- **Bug 2**: Confirmed resolved by v0.26 unification architecture — reset() called per function

## [v0.25.3] — 2026-06-25

### Fixed
- **Bug 1**: Newtype transparency consistency (same_type vs unify)
- **Bug 4**: Double unify in Stmt::Return
- **Bug 5**: Newtype implicit unwrapping regression
- **fmt_type**: Newtype wrapping capability/primitive types now formats transparently (aligns with `same_type`)

## [v0.25.2] — 2026-06-25

### Added
- 3 dual-backend tests: newtype `.0`, `List<Record>` field access, int match catch-all
- 4 typecheck tests: CK1 constructor scoping, CK2 generic enum, CK4 alias cycle, D3 exhaustiveness
- Promoted `dual_higher_order_nested_generic` to dual backend

### Fixed
- **D4**: codegen newtype `.0` detection uses `type_defs` registry instead of `expr_type_of`
- **D1**: `infer_object_type` Index parsing fix (exclude trailing `>` in element type name)

## [v0.25.1] — 2026-06-25

### Added
- **D1**: List non-scalar element codegen — `List<Record>` heap-allocates struct elements
- **D3**: Exhaustiveness check for int/string literal patterns + non-enum catch-all warning

### Fixed
- **D1**: `infer_object_type` Index parsing fix (exclude trailing `>` in type name)
- **D1**: `convert_list_elem_by_type` uses `type_llvm` registry for user-defined types first
- **D1**: `let` bindings track `List<T>` and `Index` element types in `var_type_names`

## [v0.25.0] — 2026-06-25

### Added
- **C1**: TypeId Arena infrastructure (`type_id.rs`) with hash-consing + 6 tests
- **C1**: `Type::TypeVar(u32)` and `Type::ForAll(Vec<String>, Box<Type>)` variants
- **CK1**: Constructor pattern lookup scoped to subject type
- **CK2**: Generic enum `self_ty` includes type parameter arguments
- **CK3**: Variant constructor shadowing emits E0402 diagnostic
- **CK4**: Alias cycle detection follows nested type names recursively
- **CK5**: Tuple pattern handles `Type::Name("Tuple")` dual representation
- **CK6**: List pattern checks element type against `List<T>` inner type
- **CK7**: Actor method keys namespaced as `Actor::method`
- **CK8**: Built-in `None` intercept moved after user-type check
- **CK9**: `loop`/`while`/`for`/`WhileLet` in `block_returns_on_all_paths`
- **D2**: 2 enum tests promoted from `interp_only` to dual backend
- **D4**: Newtype `.0` unwrap — checker/interpreter/codegen all support
- **R5/R9/C5**: Confirmed already safe (two allocation paths, each consistent)

## [v0.24.3] — 2026-06-25

### Fixed
- **S1**: `rc_header_from_ptr_mut` 返回 `&'static mut RcHeader` — 改为 `*mut RcHeader` 裸指针 + `rc_header_ref` 共享引用辅助
- **S2**: `mimi_rc_weak_retain` TOCTOU 竞态 — load→check→add 改为 CAS 循环
- **S3**: `mimi_rc_release` dealloc 使用 `Layout::array::<u8>(0)` — RcHeader 新增 `alloc_size` 字段，dealloc 使用实际分配大小
- **S4**: `map_from_handle`/`set_from_handle` 返回 `&'static mut` — 改为返回 `*mut T` 裸指针
- **S6**: `mimi_map_from_list` 无界循环 — 添加 1M 上界 clamp
- **S7**: `mimi_json_deserialize` out_len null 解引用 — 添加 null 检查
- **S8**: `mimi_recv` `n as usize` 截断越界 — 添加 `n.min(size)` clamp
- **S9**: `mimi_args_init` 存储 argv 原始指针 — 改为 `alloc_c_string` 复制字符串
- **S10**: `mimi_map_collect` 文档化 keys/values 收集策略差异
- **S19**: `mimi_runtime_abort` transmute 类型擦除 → `AtomicPtr<ErrorHandler>` 类型化指针
- **S21**: `.expect("lock poisoned")` 级联 panic → `unwrap_or_else(|e| e.into_inner())`
- **S24**: `mimi_try_exit_str` 未使用 `_len` → 使用 len 做 `from_raw_parts` 边界安全读取
- **S15**: `mimi_args_get` 返回悬垂字符串 → 新增 `mimi_string_free` 统一释放接口
- **S17**: 正则引擎 ReDoS 指数爆炸 → `match_here_with_depth` 递归深度限制 (REGEX_MAX_DEPTH=100)
- **S22**: `mem::forget(Vec)` 手动内存管理 → 新增 `mimi_list_free(list, free_elements)` 统一释放

### Known limitations (documented, inherent design constraints)
- **S13**: fork 子进程继承 mutex 死锁 — fork() 语义限制，已有 `MIMI_FFI_PREFORK` 开关
- **S14**: siglongjmp 跳过 destructors — 已最小化：堆分配 jump buffer + catch_unwind + 清理路径
- **S16**: JSON depth 限制 — `json_get_inner` 使用独立手动解析器，不受影响
- **S18**: `mimi_try_exit` process::exit — FFI 中 panic 是 UB，exit 是最安全路径
- **S20**: runtime/mod.rs 3100+ 行 — 拆分为子模块需要大量重构（deferred）
- **S23**: standalone libc — 所有使用的函数已声明完整

## [v0.24.2] — 2026-06-25

### Fixed
- **E1**: Z3 verifier `solver.pop(1)` underflow after Unknown/crash — added `push_depth` tracking; `solver_pop` guards against pop when solver was replaced
- **E4**: Match guard expressions invisible to NLL borrow checker — `collect_uses_in_expr` now traverses `arm.guard`, preventing premature borrow release
- **E5**: Field-level borrows never released at NLL last use — added `release_field_borrow` and integrated field-borrow release into `release_borrows_at_last_use`
- **V5**: Counterexample now displays string variable values (z3 String theory)
- **V8**: `build_let_subst` now traverses While/WhileLet/For/OnFailure/Loop/Expr/Assign/Return/SharedLet/Alloc blocks and expressions (previously skipped)

### Tests
- `verify_solver_pop_after_unknown_no_crash` — E1 solver state safety
- `verify_match_nonexhaustive_no_false_positive` — E2 非穷尽 match 不静默通过 ensures
- `verify_match_exhaustive_wildcard_passes` — E2 穷尽 match wildcard 正常验证
- `verify_invariant_assumed_not_preserved` — E3 invariant 作为假设（文档化当前行为）
- `verify_if_else_body_return` — V1 if-else 返回值提取
- `verify_nll_cross_block_boundary` — V7 NLL 跨块借用释放
- `borrow_match_guard_uses_ref` — E4 match guard + borrow NLL
- `borrow_field_level_nll_release` — E5 field borrow NLL release
- `borrow_nll_cross_block` / `borrow_nll_multi_block` — V7 NLL 边界测试
- 基线: 2,144 passed, 0 failed, 21 ignored

### Fixed
- **R1**: `mod no_panic` ×4 重叠 cfg — 删除 2 个重复空实现模块（macOS 编译错误）
- **R2**: `weak_retain` 无存活检查 — strong==0 && weak==0 时不递增（UAF 防护）
- **R3**: `sigjmp_buf` 硬编码 128 字节 → 扩容至 256 字节（glibc/macOS/ARM64 安全）
- **R4**: `__mimi_pow_i64(-2, 3)` 返回 0 — `checked_mul` 替代手动溢出检查
- **R5**: `CString::into_raw` 分配器混用（Rust alloc vs libc free）— `alloc_c_string` 统一 libc 分配器（26 处替换）
- **R6**: JSON key 转义序列被替换为 `?` — 完整 escape 解码（`\n \t \\ \"` 等）
- **R8**: `mimi_json_deserialize` 的 `out_len` 报告 count 而非 idx — 改为实际解析数量
- **R9**: `cstr_to_str` 无约束 lifetime — 4 处替换为 `cstr_to_string`，消除悬挂引用
- **R10**: IPv6 URL `[::1]` 括号被路径分割器破坏 — bracket-aware host 解析
- **R11**: 网络函数 `fd as i32` 静默截断 — `fd_to_i32()` 安全转换
- **CG1**: f-string 1024 字节固定缓冲区溢出 — 运行时动态计算总大小
- **CG2**: if-else 分支未 clone `vars` — 分支独立作用域 + 合并
- **CG5**: phi 节点从 unreachable block 收值 — func.rs/control.rs 添加 reachability 追踪
- **CG6**: slice `start > end` 产生巨大长度 — `select` clamp 到 0 长度
- **CG3**: spawn poll 函数隔离 heap_allocs 作用域 — 防止 builtin 注册条目污染父函数作用域
- **CG4**: 字符串字面量返回 `i8*` 但 LLVM 类型是 `{i8*, i64}` — `func.rs` 中识别 string struct 类型时调用 `wrap_c_string` 而非 struct-load
- **CG7**: `let x;` 非 int 类型不初始化 — float/pointer 类型零初始化
- **CG9**: 闭包 indirect-call ABI 3 处重复合并为统一 `compile_closure_call(closure_val, &[args])` 
- **C4**: 执行器协调 — 进程隔离 + S11/S12 atomic 修复消除潜在死锁

### Changed
- `compile_closure_call` 签名改为接受 `&[BasicValueEnum]` 变长参数（替代单 `IntValue`）

### Tests
- 新增 7 个测试: `builtin_pow_negative_base`, `builtin_pow_negative_base_even_exp`, `builtin_pow_zero_exp`, `e2e_json_key_escaped`, `e2e_json_value_escaped`, `dual_string_literal_return`, `dual_string_literal_let_return`
- 基线: 2,134 passed, 0 failed, 21 ignored

## [v0.24.0] — 2026-06-25 — 并发重构 (spawn→状态机)

### Added
- **A1**: `spawn expr` codegen 从 pthread 改为 `mimi_spawn_future` + poll 状态机
- **A2**: 清理 codegen 中 `pthread_create`/`pthread_join` 符号引用和 builtin 声明
- **A3**: parasteps 保留独立并行 + 补偿 + 静态冲突检测

### Fixed
- 类型检查: `spawn expr` 返回 `Future<T>`（带泛型参数），修复 `await` 类型匹配
- 解释器: `eval_spawn` 返回 Future 而非直接求值（同步包装，env capture 待实现）
- `e2e_parasteps_spawn_and_await` 解除 `#[ignore]`（future 稳定，不再 flaky）

### Changed
- `parasteps_thread_ids` → `parasteps_future_ptrs` (重新标注代码生成器字段)
- 所有 golden IR 文件更新: pthread_create/pthread_join → mimi_spawn_future/await_future/future_free

## [v0.23.0] — 2026-06-24 — Z3 验证器深度修复

### Fixed
- **K1** 🔴: Z3 约束静默丢失 — `expr_to_z3_bool`/`expr_to_z3_int`/`expr_to_z3_real` 遇到不支持的表达式（Lambda, Comprehension, SetLiteral, MapLiteral, Pattern::Constructor 等）时返回 None，现在收集到 `parse_errors` 并附加到诊断中。合约不可编码时返回 Unknown+警告，而非静默 Verified。
- **K2** 🔴: Z3 result 未约束 — 函数体返回值编码失败时，`parse_errors` 记录"could not encode return expression — result may be unconstrained"，不再静默忽略。
- **K3** 🔴: Z3 求解器崩溃后 panic — `Z3String::from_str("").expect(...)` 替换为 `if let Ok(...) else { continue }`，求解器状态不一致时不 panic。
- **K4** 🔴: Contracts 解析失败静默 — `parse_condition()` 在 `bind_contracts` 中失败时，收集到 `Vec<String>` 返回给调用方。`check` 命令显示为诊断消息。
- **K5** 🔴: Type Checker Stmt::Math 未检查 — `Stmt::Math` 从通配分支移出，每个 math 表达式经 `infer_expr` 类型检查。
- **K6** 🔴: rule 转换遗漏块类型 — `transform_rules_in_block` 补充 Loop, WhileLet, Arena, Unsafe, Alloc, Parasteps, OnFailure 的递归遍历。

## [v0.23.1] — 2026-06-24 — 安全检查 + 验证覆盖

### Fixed
- **H1** 🟠: Async codegen 绕过 GEP — 4 处 `self.builder.build_gep(i8_ty, ...)` 改用 `self.gep().build_gep(...)`，通过 CheckedGepBuilder 安全抽象。
- **H2** 🟠: `catch_unwind` 虚假安全感 — 修正注释文档，明确指出 `catch_unwind` 不捕获 SIGSEGV，仅捕获 Rust panic。
- **H5** 🟠: requires/ensures 不做布尔类型检查 — `Stmt::Requires`/`Ensures`/`Invariant` 现检查推断类型是否为 `bool`，否则触发 E0212。
- **H6** 🟠: Parasteps 安全检查不完整 — `check_stmt_parasteps_safe`、`collect_shared_writes_in_stmt`、`check_expr_parasteps_safe` 三类函数补充全部遗漏的 Stmt/Expr 变体，并新增 `collect_shared_writes_in_expr` 递归辅助函数。
- **H7** 🟠: FFI 路径字符串合约不工作 — `setup_ffi_func_vars` 注册 Z3String 和 string_len 变量，使字符串相等/长度操作在 FFI 路径可编码。
- **H8** 🟠: 反例非标量不检测 — `eval_expr_on_model` 新增 `resolve_to_string` 辅助函数，EqCmp/NeCmp 分支在 int/f64 失败后备尝试字符串比较；未处理表达式类型保守返回 `true` 避免假阳性。

## [v0.23.2] — 2026-06-24 — Codegen 修复 + 合约绑定 + 错误处理

### Fixed
- **H3** 🟠: `compile_assert_ne`/`compile_assert_approx_eq` 失败块显示实际值（同 `compile_assert_eq` 模式）。
- **H4** 🟠: `to_json` 复杂类型静默返回 `"{}"` → 优雅 `CompileError`。
- **H9** 🟠: `map_rule_contracts` 递归处理嵌套模块内的 `rule` 语句。
- **H10** 🟠: `split_once(": ")` 固定分隔符 → `text.find(':')` 灵活匹配任意空格。
- **M2** 🟡: HTTP `write_all` 错误通过 `eprintln!` 记录。
- **M3** 🟡: FFI 线程池 `sender.send` 和 `handle.join()` 错误通过 `eprintln!` 记录。
- **M4** 🟡: `map_from_handle`/`set_from_handle` 添加 null handle 校验（`handle == 0` panic）。
- **M6** 🟡: tuple 类型映射 `panic!()` → `CompileError::TypeMismatch` 优雅错误。
- **M7** 🟡: `parse_condition_full` 的 `total - 1` 添加 `total > 0` 守卫。
- **M8** 🟡: LSP lex 错误和 mms 序列化错误通过 `eprintln!` 记录。

## [v0.22.9] - 2026-06-24 — while let + 模式修复 + codegen缺口关闭

### Added
- `while let` — 条件模式匹配循环全路径（parser/typeck/interp/codegen）
- `compile_pattern_check` — codegen 模式匹配布尔判定
- Option/Result `Type::Name` vs `Type::Option/Result` 双表示桥接

### Fixed
- Codegen: NamedArg 优雅错误（替代 `_ => Err`）
- Codegen: Ellipsis 加入无操作跳过分支（block/func/actors）
- Codegen: WhileLet 五处 dispatch 全部关闭
- Checker: OnFailure 体现在进行类型检查
- Checker: ImplTrait trait 名称验证（同 DynTrait）
- Checker: Map 字面量键类型强制为 string

## [v0.22.8] - 2026-06-24

### Added
- `assert(cond, msg)` — 断言支持可选自定义消息（typeck/interp/codegen）
- `use path::to::module as alias` — 模块导入别名（lexer/parser/checker）
- `for c in "string"` — for 循环支持字符串遍历
- `for x in {1, 2, 3}` — for 循环支持 Set 遍历
- `for (k, v) in map` — for 循环支持 Map/Record 遍历
- Record Display 统一格式: `TypeName { field: val }`（包含类型名）
- Variant `to_string()` 方法支持
- 4 个测试: assert_msg, for_string, for_set, use_alias

## [v0.22.7] - 2026-06-24

### Added
- 默认参数值: `func f(x: i32 = 0) { ... }` 支持带默认值的参数
- 命名参数调用: `f(y=2, x=1)` 支持按名重排参数
- LSP hover/signature 显示 `= default` 在参数签名中
- 5 个测试: default_value, override, multi, named_args, named_with_defaults

## [v0.22.6] - 2026-06-24 — 诊断质量 + format()（Diagnostics & format() Builtin）

### Added
- `format(template, args...)` builtin: `format("x={} y={}", a, b)` returns `"x=42 y=hello"`
- Error message suggestions: `.with_help()` added to E0209 (type mismatch in `let`, assignment, list element) and E0211 (argument type mismatch)
- 4 tests for `format()` (basic, multi, no-placeholders, mixed types)

## [v0.22.5] - 2026-06-24 — LSP + 导入增强（LSP Completion & Selective Import）

### Added
- LSP stdlib completion: auto-scan `std/*.mimi` for `pub func` signatures, shown in "top" and "module" contexts
- LSP `::` path completion: typing `strings::` shows functions from that stdlib module
- Selective import: `use strings::replace_all` now resolves and loads the `strings` module
- `loop` added to LSP keyword completions

## [v0.22.4] - 2026-06-24 — 管道符 + loop（Pipe Operator & Loop Keyword）

### Added
- 管道符 `|>` 语法糖：`a |> f(b)` 脱糖为 `f(a, b)`，链式 `a |> f(b) |> g(c)` → `g(f(a,b), c)`
- 纯 parser 层脱糖，无需 inference/interpreter/codegen 改动
- `loop` 关键字：`loop { if cond { break } }` 无限循环
- `Stmt::Loop(Block)` — 全后端支持（checker/interpreter/codegen/quote）

### Fixed
- 补全 5 处 `Stmt::Loop` 遗漏的 verifier/rule 匹配（verifier/func.rs, ffi.rs, helpers.rs, core/mod.rs）

### Tests
- 6 个新测试：pipe_basic / pipe_chain / pipe_ident / loop_basic / loop_break / loop_continue
- 基线: 2,109 passed, 0 failed, 21 ignored

### Added
- Set 集合字面量 `{1, 2, 3}`（逗号分隔，≥2 元素；`{expr}` 保持为 block 向后兼容）
- Set 操作：`size/len`, `is_empty`, `contains`, `insert`, `remove`, `to_list`
- `std/set.mimi` — SetExt trait 定义
- `from_json::<T>` 和 `Set<T>` 的 LLVM codegen 全路径实现（替代 stub error）
  - `from_json::<i32/f64/bool/string>` 通过运行时函数 `mimi_json_as_i64/f64/bool` + `mimi_from_json`
  - Set 字面量/方法通过运行时 `mimi_set_new/insert/contains/remove/size/to_list`
  - 运行时新增 `MimiSet` 结构体 + 9 个 C ABI 函数

### Fixed
- 6 处 clippy warnings（unused var, collapsible if-let, needless borrow, needless closure）

### Tests
- 16 个 Set interpreter 测试 + 7 个 codegen e2e 测试
- 21 个 golden test files 更新（新增运行时函数声明）
- 基线: 2,103 passed, 0 failed, 21 ignored

## [v0.22.2] - 2026-06-24 — JSON 类型化（JSON Typed Deserialization）

### Added
- `from_json::<T>(json_str)` 类型化 JSON 反序列化 — 支持 i32, f64, string, bool, List&lt;T&gt;, Option&lt;T&gt;, 记录类型, 嵌套记录, 枚举

### Fixed
- 6 处 clippy warnings（unused var, collapsible if-let, manual strip_prefix, needless borrow）

### Tests
- 21 个 JSON 测试（10 typed + 11 补充: 空列表/List&lt;string&gt;/f64负数/枚举/错误路径/向后兼容/codegen stub）
- 基线: 2,079 passed, 0 failed, 21 ignored

## [v0.22.1] - 2026-06-24 — 深度修复（Depth Repair）

### Added
- `Map<K,V>` 字面量 `{"key": value}` 语法 — 双后端支持（AST/parser/infer/interp/codegen）
- `mimi run --watch` 模式 — 文件变更自动重跑解释器
- `sort_f64` 和 `sort_str` 内置函数（interpreter 支持，codegen 待实现）
- 嵌套块注释 `/* */` 支持（词法分析器）
- 嵌套 `List<List<T>>` 链式索引在 codegen 路径的类型推断

### Fixed
- `assert_eq` codegen 诊断 — 失败时显示实际值 `1 != 2` / `hello != world`
- `assert_eq_string`/`assert_eq_bool` 改用 `assert_eq` 而非 `assert(false)`（丢失诊断信息）

### Tests
- 新的双后端 L1 测试：`dual_map_literal_simple` / `dual_map_literal_size` / `dual_map_literal_variable_key`
- 基线: 2,057 passed, 0 failed, 21 ignored

## [v0.22.0] - 2026-06-24 — 语言补全（Language Completion）

### Added
- `char_code(s, i) -> i64` 和 `chr(code) -> i64` 内置函数（interp + codegen + typeck）
- 递归类型支持：`type Expr { Call(string, List<Expr>) Lit(i32) }` 通过类型检查
  - Record/Union/Enum 类型定义支持自引用（通过 `List<T>` 等间接存储类型）
- Option<T> 双后端 L1 测试：Some/None 构造器 + unwrap + match
- `List<List<T>>` 泛型嵌套类型标注 + 解释器嵌套索引用例
- 高阶泛型函数 L1 测试：`func apply<T, U>(x: T, f: func(T) -> U) -> U`

### Fixed
- `compile_str_char_at` / `compile_chr` 返回指针而非 struct 值 → segfault 修复
- `compile_char_code` / `compile_str_char_at` 处理字面量 `char*` 和 struct 双字符串表示

### Tests
- 基线: 2,046 passed, 0 failed, 21 ignored

## [v0.21.0] - 2026-06-24 — 筑基（Polish & Hardening）

### Added
- 语法参考文档: `docs/syntax-reference.md` (820行)，可作自举语法底本
- Mimispec 依赖预审计: `docs/mimispec-dependency-audit.md`，20 处导入点分类 + 替换优先级方案

### Fixed
- Clippy 清零: 397 warnings → 0，62 files changed
  - ptr_type 弃用迁移: 40 文件, ~200 处 `type.ptr_type()` → `context.ptr_type()`
  - 安全强化: 23 处 `.unwrap()` → `.expect()`（runtime 17 + 生产代码 6）
  - 57 处 `not_unsafe_ptr_arg_deref` 抑制 (FFI 边界)
  - 85 处 runtime clippy warnings 归零
  - 300+ 项小 warning 修复（冗余闭包/借用/格式/转换等）
- Codegen 缺口关闭: 11 个 `dual_gap_*` 测试全部通过（match guard/枚举/元组模式/列表/push/contains）
  - 实际缺口已被先前版本修复，测试从 gap 区迁移至 closed gap 区

### Tests
- 基线: 2,037 passed, 0 failed, 21 ignored

## [0.20.1] - 2026-06-23

### Fixed
- codegen: `mimi_str_concat` 返回原始 C 字符串被误解释为 MimiString 结构体导致 SIGSEGV
  - `wrap_c_string()` 通过 `strlen` + `build_insert_value` 正确构建 `{ptr, i64}` 结构体
  - 修复字符串拼接在代码生成路径下的崩溃（影响普通函数和 async fn）

## [0.20.0] - 2026-06-23

### Added
- 结构化并发：Future/Waker/Executor 运行时 (`mimi_future_alloc/free/set_completed/is_completed`, `mimi_executor_spawn/run`)
- Poll-based codegen：`async fn` 编译为 poll 函数 + 堆分配 Future 指针，不再使用 pthread
- Interpreter 对齐：`Value::Future` 从 `mpsc::Receiver` 改为 `PollFuture`（`Deferred`/`Ready`/`Pending`）
- 协作式多任务：executor 全局任务队列，支持多 future 并发 poll
- 类型系统：`async fn` 返回类型自动包装为 `Future<T>`

### Changed
- `async func` 不再生成 `__spawn_wrapper` 和 `pthread_create`，改为 `__poll` + `mimi_executor_spawn`
- 解释器 `call_async_func` 同步求值（零开销，无线程池切换）
- 更新 21 个 golden 测试文件

### Fixed
- 运行时内存管理统一为 Rust allocator（`Box`），消除 libc `malloc`/`free` 混用 UB

### Removed
- 移除 `__spawn_wrapper` 和 `pthread_create` 相关 codegen 路径

## [0.19.0] - 2026-06-23

### Added
- 路径敏感 borrow：`&p.x` 字段级借用，支持 `&p.x` 与 `&mut p.y` 共存
- 闭包捕获借用：闭包体内的捕获变量引用正确通过借用检查
- 重借用：`&mut *m` 继承借用，`&*r` 降级不冲突
- 条件返回：`if` 分支返回引用通过类型检查
- 自引用结构体：引用字段类型正确通过构建检查

### Fixed
- borrow_boundary 测试全部解除 `#[ignore]`（4 个测试）

## [0.18.0] - 2026-06-23

### Added
- 泛型约束检查：`GenericParam::bounds` 读取与验证（`func<T: Clone>` 在调用处检查）
- 内置 trait 集：识别 Clone/Default/Copy/Eq 四个内置 trait，自动判断类型是否满足
- 泛型约束 codegen：`<T: Clone>` 在双后端中支持 `.clone()` 调用
- 生命周期 elision：单输入引用时自动推断 `&T` → `&'a T`

### Fixed
- checker 不再丢弃生命周期：`resolve_type`/`subst_type_params` 保留生命周期字段
- `borrow_fn_return_ref` 和 `borrow_fn_mut_to_imm_return` 解除 `#[ignore]`，现可通过类型检查

## [0.17.0] - 2026-06-23

### Added
- CheckedGepBuilder 抽象：`self.gep().build_gep/build_in_bounds_gep/build_struct_gep`（278 处 GEP 调用全部经由此 API）
- `build_in_bounds_gep`：52 个运行时索引 GEP 改用 inbounds，LLVM 自动插入 trap IR
- `check_list_bounds`：list 索引操作（读/写）添加运行时边界断言，OOB 时调用 `mimi_runtime_abort`
- Struct FFI struct-by-value codegen 修复（LLVM x86_64 ABI 对齐）

### Fixed
- 消除 62 处 `unsafe { build_gep(...) }` → 安全 API 调用
- `builtins/list/helpers.rs` 中 4 处漏网的 `self.builder.build_struct_gep(` 迁移至 `self.gep()`
- 剩余 `#[ignore]` 清理：FFI codegen tcp_* 解除 ignore

### Security
- Item 5: 安全 GEP 抽象消除 62 处 unsafe 指针算术
- Rust 运行时审计：Valgrind (4×) + ASan (1×) 零警告
- List 操作越界不再产生野指针（inbounds GEP + 运行时断言）

## [0.16.0] - 2026-06-23

### Added
- 效果系统 cap 交叉验证：`with` 效果名须对应已声明的 `cap`
- L2 测试：effect_declaration, effect_not_available, effect_undeclared_cap_cross_validation, effect_available_via_function_chain

### Fixed
- 模式匹配 guard 穷尽性：当 guard 存在时不再跳过未覆盖变体检查
- 函数 `with` 效果现在在函数体内可用（支持链式调用）
- `e2e_net_socket_create` / `e2e_net_connect_failure` / `e2e_net_listen_bind` 解除 `#[ignore]`（27 个 ignored 测试）

### Security
- (none yet)

## [0.15.0] - 2026-06-23

### Added
- C runtime Rust 重写：`mimi_runtime.c` (~2,361 行) → `src/runtime/mod.rs` (~2,194 行)
- JSON、HTTP、引用计数、正则、字符串操作、信号处理、数学、时间、环境、网络、capability 全部 Rust 实现
- Allocator 跟踪：所有分配通过 Rust 分配器，heap_allocs 覆盖率 100%
- Windows 统一：Win32 分支在 Rust 层用 `#[cfg(windows)]` 处理
- Standalone 编译：`src/runtime/standalone.rs` 作为 `--crate-type staticlib`

### Fixed
- **Item 1**: 线程池 TOCTOU 竞态修复（Rust `Mutex` + `Condvar`）
- **Item 4**: JSON 无递归深度限制修复（Rust 递归 + `json_max_depth` 守卫）
- **Item 6**: 无边界字符串操作修复（Rust `String`/`Vec` 安全边界）
- **Item 9**: map 表除零风险修复（Rust `HashMap` 永不零容量）
- Tier B 字符串泄漏：C runtime 内部 malloc 不被 heap_allocs 跟踪 — Rust runtime 自动修复

### Tests
- 基线：2,007 passed, 0 failed, 34 ignored

## [0.14.0] - 2026-06-23

### Added
- InterpError 错误码重构：枚举变体全部映射 E0800-E0814
- 编译期错误码补充：E0712 作为 CompileError::CodegenJson
- Z3 反例输出美化：human-readable + 函数签名 + span
- Z3 求解统计 `--stats`：约束数、求解耗时
- Z3 debug 日志 `--dump-z3`：SMT-LIB2 格式可选打印
- 求解器超时反馈：Unknown 显示函数名/约束数/耗时
- 反例过滤：每个后置条件独立报告，无重复

### Fixed
- 编译期错误码 E0240/E0241 标记为已废弃

## [0.13.0] - 2026-06-23

### Added
- P1.1: Lambda/Comprehension/Spawn/Await Z3 编码 (verify_spawn_await_*)
- P1.2: Z3 字符串理论映射：str_eq, str_contains, str_at, char_at
- LSP 悬停增强：显示 requires/ensures/invariant
- LSP Code Lens：验证状态（✓/✗/?）和提示
- Z3 求解器健壮性：f64 真值判定 + Unknown 状态机加固
- ADR 文档：内存模型、合约系统、并发模型、双后端架构

### Fixed
- 补全 verifier 编码路径，spawn/await 函数体不再降级为 Unknown/假阳性
- Z3 字符串理论一致性约束：s.length() == string_len[s], (s != "") == string_nonempty[s]
- 合约 parse 错误收集到诊断消息而非静默忽略
- FFI 违反预条件使用真实 span 而非 Span::single(0,0)
- LSP 验证缓存：跨文件唯一 key (uri:func) + LRU 淘汰

### Tests
- 新增 13 个 verifier 测试 + 9 个 LSP 测试
- 基线：2,006 passed, 0 failed, 34 ignored

## [0.12.0] - 2026-06-23

### Added
- F-16: struct-by-value crash protection via fork isolation + signal handlers (🔴)
- F-17: struct_buffers data pointer safety documentation + confirmation (🟠)
- Item 2: FfiGuard transmute field ordering documentation + layout test (🟠)
- Item 8: Fork async-signal-safety documentation for call_ffi_with_fork_isolation (🟠)
- Test: test_ffi_guard_field_ordering layout verification

### Fixed
- F-16: StructByValue return now routes through call_ffi_no_panic_struct / call_ffi_with_fork_isolation_struct instead of bare call_ffi_raw_struct (🔴)
- F-18: CALLBACK_GLOBAL_STORE lock ordering inversion → unified TABLE→STORE order (🟠)
- F-19: no_panic signal handlers now restored via restore_crash_handlers after siglongjmp (🟡)
- Item 3: expr.rs Z3 verifier unwrap → `if let Some` pattern (4 sites) (🔴)
- F-20: errno clearing simplified, Windows no-op removed (🟢)

### Security
- Item 2: transmute 'static field ordering enforced via layout test (🟠)
- Item 8: fork async-signal-safety documented on both fork isolation functions (🟠)

## [0.7.0] - 2026-06-17

### Added
- Z3 formal verification: cross-module ensures propagation, Expr::Match encoding, string length constraints
- FFI zero-copy struct-by-value (codegen path)
- Standard library: csv.mimi, template.mimi, crypto.mimi
- HTTP codegen: dual_net_tcp_client_echo
- P0.1: Expr::Call unconstrained variables → false positives fix (🔴)
- P0.2: verify_func_call_silent missing Failed assertion fix (🔴)

## [0.6.0] - 2026-06-16

### Added
- Windows target support (x86_64-pc-windows-gnu)
- Actor model: mailbox actor with lifecycle
- Regex builtins (match, find, replace)
- String contract runtime assertions

## [0.5.0] - 2026-06-16

### Added
- Parasteps spawn+await via pthread (codegen)
- Contract verification (Z3)
- CI/CD: GitHub Actions (test/clippy/fmt/valgrind/ASan/UBSan/Miri/fuzz/cppcheck)

## [0.4.0] - 2026-06-16

### Added
- Error system: String → Diagnostic replacement
- Arena escape detection (E0306)
- Write-write race detection (W005)
- Shared parameter contract warnings (E0502)

## [0.3.0] - 2026-06-16

### Added
- Package management
- Documentation generation pipeline
- Dual backend (interpreter + codegen) baseline

## [0.2.0] - 2026-06-15

### Added
- Basic language features
- LLVM codegen backend
- Contract system foundations
- MimiSpec integration

## [0.1.0] - 2026-06-15

### Added
- Initial prototype
- Interpreter implementation
- Type checker
- CLI interface
