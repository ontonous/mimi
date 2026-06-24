# Changelog

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

## [0.7.0] - 2026-06-xx

### Added
- Z3 formal verification: cross-module ensures propagation, Expr::Match encoding, string length constraints
- FFI zero-copy struct-by-value (codegen path)
- Standard library: csv.mimi, template.mimi, crypto.mimi
- HTTP codegen: dual_net_tcp_client_echo
- P0.1: Expr::Call unconstrained variables → false positives fix (🔴)
- P0.2: verify_func_call_silent missing Failed assertion fix (🔴)

## [0.6.0] - 2026-05-xx

### Added
- Windows target support (x86_64-pc-windows-gnu)
- Actor model: mailbox actor with lifecycle
- Regex builtins (match, find, replace)
- String contract runtime assertions

## [0.5.0] - 2026-04-xx

### Added
- Parasteps spawn+await via pthread (codegen)
- Contract verification (Z3)
- CI/CD: GitHub Actions (test/clippy/fmt/valgrind/ASan/UBSan/Miri/fuzz/cppcheck)

## [0.4.0] - 2026-03-xx

### Added
- Error system: String → Diagnostic replacement
- Arena escape detection (E0306)
- Write-write race detection (W005)
- Shared parameter contract warnings (E0502)

## [0.3.0] - 2026-02-xx

### Added
- Package management
- Documentation generation pipeline
- Dual backend (interpreter + codegen) baseline

## [0.2.0] - 2026-01-xx

### Added
- Basic language features
- LLVM codegen backend
- Contract system foundations
- MimiSpec integration

## [0.1.0] - 2025-12-xx

### Added
- Initial prototype
- Interpreter implementation
- Type checker
- CLI interface
