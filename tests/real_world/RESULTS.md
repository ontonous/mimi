# Mimi 真实代码可用性评估结果

**评估时间**：2026-07-04  
**Mimi 版本**：0.28.26-dev  
**最后更新**：2026-07-05（all gaps closed: Any to_string, from_json/to_json List<RecordType>）  
**评估命令**：`python3 tests/real_world/run_suite.py`  
**环境**：Ubuntu, LLVM 18 (via /tmp/llvm-wrapper), cc/gcc

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
