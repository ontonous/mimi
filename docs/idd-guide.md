# Mimi IDD 开发流程指南

> Invariant-Driven Development：以不变量为驱动的开发模式。
> 适用于 Mimi 双后端（解释器 + LLVM 代码生成）编译器。

---

## 核心原则

传统 TDD 对编译器失效的原因：**功能正确不等于后端一致**。一个特性可能在解释器中工作，在代码生成中损坏，而所有测试仍然通过——如果它们只测试了一个后端。

IDD 的定义：**在任何实现开始之前，先编写捕获不变量违反的测试。提交实现时，测试必须通过。**

三个不变量层级，优先级严格：

| 层级 | 名称 | 断言 |
|------|------|------|
| L1 | 双后端等价性 | `run_source(p) == compile_and_run(p)` |
| L2 | 类型系统健全性 | `check_source(bad_p) → Err`（应有错误） |
| L3 | 内存安全 | Valgrind/Miri/ASan 下零警告 |

L1 失败 = 代码生成损坏，用户得到错误的二进制。
L2 失败 = 类型检查器漏报错误，崩溃在运行时才出现。
L3 失败 = 未定义行为，可能被利用。

---

## 工作流

### 1. 添加新功能

```
步骤 1 — 编写 L1 双后端测试（在实现之前）
步骤 2 — 在解释器中实现（L1 通过）
步骤 3 — 在代码生成中实现（L1 仍然通过）
步骤 4 — 添加 L2 健全性测试（应拒绝不当用法）
步骤 5 — 运行 L3 内存检查（Valgrind + ASan）
步骤 6 — 提交
```

```
示例：添加 power 内置函数

// step1: 先写这个，此时两个后端都返回 42 占位
#[test]
fn dual_builtin_pow() {
    let src = "func main() -> i32 { println(pow(2, 10)); 0 }";
    // 这两个断言在实现前都会失败
    let interp = format!("{}", run_source(src));  // 通过 Value 的 Display
    let codegen = compile_and_run(src).unwrap();
    assert_eq!(interp.trim(), "1024");
    assert_eq!(codegen.trim(), "1024");
}

// step2: 在 interp/builtins/math.rs 中实现 → 第一个断言通过
// step3: 在 codegen/builtins/math.rs 中实现 → 两个断言都通过
// step4: 添加类型检查
#[test]
fn typecheck_pow_non_numeric() {
    let src = "func main() -> i32 { pow(\"hello\", 2) }";
    assert!(check_source(src).is_err());
}
```

### 2. 修复 Bug

```
步骤 1 — 编写重现该 Bug 的 L1 测试（它失败）
步骤 2 — 修复代码
步骤 3 — 测试通过
步骤 4 — 思考：这个 Bug 属于哪个更广泛的类别？
        如果类别尚未被自动化保护，添加一个通用的 L1 测试。
步骤 5 — 提交
```

```
示例：修复 match guard 在代码生成中缺失

// step1: 重现 bug 的测试
#[test]
fn dual_match_guard() {
    let src = r#"
        func main() -> i32 {
            let x = 42
            let r = match x {
                v if v > 100 => 1
                v if v > 10  => 2
                _ => 3
            }
            println(r); 0
        }
    "#;
    // 修复前：解释器输出 "2"，代码生成输出 "1"（忽略 guard）
    // 修复后：两者都输出 "2"
    let interp = format!("{}", run_source(src));
    let codegen = compile_and_run(src).unwrap();
    assert_eq!(interp.trim(), "2");
    assert_eq!(codegen.trim(), "2");
}

// step2: 修复 codegen/expr/match.rs 中的 compile_match_expr
// step3: 测试通过
// step4: 添加通用的 guard 测试套件
//   - dual_match_guard_multiple()
//   - dual_match_guard_enum()
//   - dual_match_guard_fallback()
// step5: 提交，附带引用此测试的提交信息
```

### 3. 提交信息规范

```
fix(codegen): 修复 match guard 在代码生成中缺失

L1 测试: dual_match_guard (tests/dual_backend.rs)
类别: 模式匹配语义

Guard 条件在 compile_match_expr 中被静默忽略，
导致第一个匹配的 arm 无论 guard 如何都被选中。
通过添加 guard 表达式的编译和条件分支修复。
```

每次修复都必须引用**测试名称**和**不变量类别**，以便将来的审计可以追踪哪些区域被覆盖。

---

## 测试宏

`dual_backend.rs` 和 `tests/mod.rs` 中已提供以下工具：

```rust
// 双后端等价断言：解释器无错误 + codegen stdout 匹配预期
dual_assert!(program, "expected_output")

// 解释器仅测试：当 codegen 已知有差距时使用（记录已知问题）
dual_assert_interp_only!(program, expected_value)

// 双后端无错运行（Mimi 代码内部使用 assert_eq，两个后端都必须通过）
dual_assert_ok(program)

// 双后端合约验证（两个后端都启用合约，必须成功）
dual_assert_contract_ok(program)
```

已知差距有 `#[ignore = "codegen gap: ..."]` 标记，通过 `cargo test dual_gap_` 查看：

---

## 测试基础设施速查

| 函数 | 路径 | 作用 |
|------|------|------|
| `parse(src)` | `tests/mod.rs:146` | 词法分析 + 解析为 `File` |
| `run_source(src)` | `tests/mod.rs:151` | 解析 + 解释器运行，返回 `Value` |
| `run_source_result(src)` | `tests/mod.rs:157` | 同上，启用合约验证 |
| `check_source(src)` | `tests/mod.rs:178` | 类型检查，返回 `Result<(), Vec<Diagnostic>>` |
| `check_source_strict(src)` | `tests/mod.rs:183` | 严格模式类型检查 |
| `compile_and_run(src)` | `tests/mod.rs:325` | E2E 编译 → 执行 → stdout |
| `compile_and_run_valgrind(src)` | `tests/mod.rs:335` | Valgrind 下 E2E |
| `compile_and_run_asan(src)` | `tests/mod.rs:341` | ASan 下 E2E |
| `compile_and_run_ubsan(src)` | `tests/mod.rs:345` | UBSan 下 E2E |
| `compile_and_verify_contracts(src)` | `tests/mod.rs:330` | 合约验证 E2E |
| `dual_assert_contract_ok(src)` | `tests/mod.rs:357` | 合约双后端无错运行 |
| `dual_assert_ok(src)` | `tests/mod.rs:349` | 双后端无错运行 |

---

## 不变量映射：已添加的测试

以下映射记录了本次 IDD 补齐中为每个历史修复添加的测试：

| 修复 Commit | 审计发现 | L1 双后端测试 | L2 类型检查测试 |
|------------|----------|--------------|----------------|
| `6459fdb` | match guard SIGSEGV / 枚举布局 | `dual_gap_match_guard_*` ✅, `dual_gap_enum_match_payload` ✅, `dual_gap_enum_bool_variant` ✅ | — |
| `6459fdb` | tuple/array 元素级匹配 | `dual_gap_match_tuple_*` ✅ | — |
| `b08855a` | 枚举序号确定性（排序变体名） | `dual_gap_enum_reorder_stable` ✅ | — |
| `2f1477f` | `old()` 在 codegen 中快照 | `dual_contract_ensures_old_dual` ✅, `dual_contract_old_tautology` ✅ | — |
| `eedf8be` | comptime 隔离（错误信息） | — | `adv_comptime_block_error_message` ✅, `adv_comptime_func_call_error_message` ✅, `adv_quote_block_error_message` ✅, `adv_quote_interpolate_error_message` ✅ |
| `4cf48e9` | push 变异语义 | `dual_gap_push_mut_content` ✅ | — |
| `5d9add0` | 内置函数统一（25 个） | `builtin_registry.rs` (已存在) | — |
| `4f7e760` | `fmt_type` Option 格式一致 | — | `fmt_type_option_consistent_with_same_type` ✅, `fmt_type_option_nested` ✅, `fmt_type_result_contains_option` ✅ |
| `e895f82` | 数值类型提升 `is_numeric_coercion` | — | `typecheck_numeric_coercion_i32_to_i64_let` ✅, `typecheck_numeric_coercion_i32_to_i64_arg` ✅, `typecheck_numeric_coercion_i32_to_f64` ✅ |
| `e895f82` | ensures `result` 绑定 | — | `typecheck_ensures_result_binding` ✅ |
| `10802d7` | NaN 排序语义 | (跳过: 无 `nan()` 内建) | — |
| 本次修复 | match guard / tuple / enum / contains 回归 | `dual_match_guard_mixed_literal`, `dual_match_tuple_bind_vars`, `dual_enum_custom_mixed_variants`, `dual_contains_false`, `dual_contains_empty`, `dual_push_mut_read_back` ✅ | — |
| `be574a1` | 二进制数值运算符 widening | `dual_numeric_coercion_i32_i64_add`, `dual_numeric_coercion_i32_i64_sub`, `dual_numeric_coercion_i32_i64_comparison`, `dual_numeric_coercion_i32_f64_add`, `dual_numeric_coercion_i64_f64_mul` ✅ | `typecheck_binary_numeric_coercion_i32_i64_add` ✅, `typecheck_binary_numeric_coercion_i32_i64_all_ops` ✅, `typecheck_binary_numeric_coercion_i32_f64` ✅, `typecheck_binary_numeric_coercion_i64_f64` ✅ |

| `77e538e` | await-in-parasteps 中 spawn 返回占位符 0 | `dual_parasteps_spawn_await` ✅, `e2e_parasteps_spawn_and_await` ✅ | — |
| `77e538e` | codegen: await 在 parasteps 内调用 pthread_join(0) | 同上 | — |
| (current) | `rule` 文本未映射为结构化合约 | `e2e_rule_ensures_basic` ✅, `e2e_rule_requires_prefix` ✅, `e2e_rule_colon_separated` ✅, `e2e_rule_unmappable_is_metadata` ✅, `e2e_rule_violation_detected` ✅, `e2e_rule_requires_violation_detected` ✅, `e2e_rule_spawn_and_await` ✅, `e2e_rule_parasteps_with_rule` ✅, `e2e_rule_ensures_prefix` ✅ | — |
| 
| `2cee0cf` | v0.10 后端对齐: codegen parasteps 改为真实 pthread | `dual_parasteps_spawn_await` ✅, `e2e_parasteps_spawn_and_await` ✅ | — |
| `2cee0cf` | v0.9-3 shared write-write W005 | — | `warn_shared_write_write_parasteps` ✅, `warn_shared_write_write_no_warning_single_write` ✅, `warn_shared_write_write_different_vars` ✅, `warn_shared_write_write_nested_parasteps` ✅, `warn_shared_write_write_no_warning_read_only` ✅ |
| `2cee0cf` | v0.9-2 arena 逃逸 E0306 | — | `typecheck_arena_escape_ref_to_outer_rejected` ✅, `typecheck_arena_escape_ref_to_outer_rejected_func_call` ✅, `typecheck_arena_escape_ref_arg_to_outer` ✅ |
| `2cee0cf` | v0.9-4 合约+shared E0502 | — | `typecheck_contract_with_shared_param_is_error` ✅, `typecheck_requires_with_shared_param_rejected` ✅, `typecheck_ensures_with_shared_param_rejected` ✅, `typecheck_contract_with_shared_mut_rejected` ✅ |
| `2cee0cf` | v0.9-5 parasteps 合约提取 | — | `typecheck_parasteps_requires_local_shared_rejected` ✅, `typecheck_parasteps_ensures_local_shared_rejected` ✅ |
| `3b0e102` | v0.10 bug 修复: spawn+await 类型跟踪 + malloc 尺寸 | `e2e_parasteps_spawn_and_await` ✅ | — |

✅ = 通过且启用 | (已忽略) = `#[ignore]` 标记的已知 codegen 差距

### 已知 Codegen 差距速查

当前无未解决的已知 codegen 差距。

以下差距已在本轮 IDD 修复中关闭：

- match guard SIGSEGV
- tuple 模式匹配
- 单元/负载枚举变体构造函数与匹配
- `push()` 就地变异
- `contains()` SIGSEGV
- 嵌套 enum 作为 payload（通过 heap-allocated struct payload 机制已修复，`dual_nested_enum_match` ✅）
- `quote!` + `ast_eval`（编译期折叠：literal-only quote 块在 codegen 中直接求值，`dual_quote_eval_literal` ✅）
- await-in-parasteps（spawn 在 parasteps 内创建真实 pthread, await 调用 pthread_join 获取返回值，`dual_parasteps_spawn_await` ✅）

CI 中 `cargo test dual_` 必须 100% 通过（忽略的测试除外）。新增功能的 L1 测试不可跳过；和代码一起提交。

---

## CI 门禁顺序

```
1.  cargo test                          # 所有测试（1,881 个）
2.  cargo test dual_                    # 双后端等价性（L1，~177 个）
3.  cargo test "typecheck::"            # 类型系统健全性（L2）
4.  cargo test "adv_comptime|adv_quote" # 编译时错误信息
5.  cargo test ffi_                     # FFI 契约等价性
6.  cargo test codegen_e2e              # 代码生成 E2E
7.  cargo test "fmt_type"               # 类型格式化一致性
8.  cargo test dual_gap_ -- --ignored   # 已知差距（必须编译通过，允许失败）
9.  cargo miri test interp ffi          # Miri 下 UB 检测（L3）
10. cargo test codegen_e2e -- valgrind  # Valgrind 下内存安全（L3）
```

第 1 项失败 → 别动新代码。
第 2 项失败 → 后端语义分歧，95 个启用测试必须全通过。
第 8 项已知差距被明确追踪，关闭差距后移除 `#[ignore]`。

---

## 常见错误和纠正

| 旧习惯 | 新习惯 |
|--------|--------|
| 先实现解释器，再实现代码生成 | 先写双后端测试，再实现两者 |
| 测试只验证解释器输出 | 测试验证 `run_source` == `compile_and_run` |
| 审计发现 bug → 修复 → 下一个审计 | 审计发现 bug → 修复 + 添加不变量测试 → 下一个人无法回归 |
| 提交信息只写"修复了什么" | 提交信息写"哪个测试证明了修复"，引用 `dual_` 测试名 |
| 已知差距被静默容忍 | 已知差距有 `dual_assert_interp_only!` 标记，链接到追踪问题 |
