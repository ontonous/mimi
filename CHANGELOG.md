# Changelog

## [Unreleased] — v0.28.3-dev

### Added

- **mimi-markdown**: Markdown→HTML 转换器项目（1009 行 Mimi 代码）
  - 支持标题、段落、粗体、斜体、行内代码、删除线、链接、图片
  - 围栏代码块、有序/无序列表、引用、水平线、表格
  - HTML 转义、CLI 工具函数
  - 47 个测试全部通过
- **G-74**: 字符串比较运算符 `<` `>` `<=` `>=` codegen 支持
- **G-76**: `const` 声明 codegen 支持

### Fixed

- **G-64**: codegen string builtins 接受 Record 字段值（str_substring, str_trim 等）
- **G-65**: `let mut xs: List<string> = []` 空类型化列表现在可以工作
- **G-66**: 嵌套作用域允许变量遮蔽（仅同作用域重定义报错）
- **G-68**: `push()` 支持 Record/StructValue 类型
- **G-69**: `List<T>` 变量的 var_type_names 存储完整泛型类型名
- **G-70**: string builtins 返回 `{ptr, i64}` 结构体指针导致 codegen 输出乱码
- **G-72**: if/else 分支允许同名变量
- **G-73**: Record 构造器内空列表从字段类型推断

### Known Gaps

- **G-71**: codegen 第二次构建同一文件 segfault（LLVM 状态未重置）

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
