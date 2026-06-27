# Invariant-Driven Development (IDD) Guide

## 核心原则

IDD 是 Mimi 的标准开发流程。核心思想：**先定义不变量，再实现功能**。

## 三层不变量

| 层级 | 名称 | 断言 | 失败含义 |
|------|------|------|---------|
| L1 | 双后端等价性 | `run_source(p) == compile_and_run(p)` | 代码生成损坏 |
| L2 | 类型系统健全性 | `check_source(bad_p) → Err` | 类型检查器漏报 |
| L3 | 内存安全 | Valgrind/Miri/ASan 下零警告 | 未定义行为 |

## 新增功能流程

```
1. 编写 L1 双后端测试（允许暂时 #[ignore]）
2. 在解释器中实现
3. 在代码生成中实现
4. 添加 L2 健全性测试
5. 运行 L3 内存检查
6. COMMIT
```

## 修复 Bug 流程

```
1. 编写重现该 Bug 的 L1/L2 测试
2. 修复代码
3. 测试通过
4. 补充通用回归测试
5. COMMIT（引用测试名）
```

## 已知 Codegen 差距速查

| 编号 | 描述 | 状态 |
|------|------|------|
| G-64 | string builtins 接受 Record 字段值 | ✅ 已修复 |
| G-65 | 空类型化列表 | ✅ 已修复 |
| G-66 | 嵌套作用域变量遮蔽 | ✅ 已修复 |
| G-68 | push() 支持 Record 类型 | ✅ 已修复 |
| G-69 | List<T> 完整泛型名 | ✅ 已修复 |
| G-70 | if/else 表达式内嵌套函数调用 | ✅ 已修复 |
| G-71 | 大文件 codegen segfault | ✅ 已修复 |
| G-72 | if/else 分支同名变量 | ✅ 已修复 |
| G-73 | Record 构造器空列表推断 | ✅ 已修复 |
| G-74 | 字符串比较运算符 | ✅ 已修复 |
| G-76 | const codegen 支持 | ✅ 已修复 |
| G-78 | 元组解构含字符串字段 | ✅ 已修复 |
| G-79 | 高阶函数 codegen | ✅ 已修复 |
| G-80 | format() int/float 参数 | ✅ 已修复 |
| G-81 | Record 字段 List 类型推断 | ✅ 已修复 |
| G-82 | regex_match codegen | ✅ 已修复 |
| G-83 | from_json::<Record> codegen | ✅ 已修复 |
| G-84 | Set<T> 方法返回类型跟踪 | ✅ 已修复 |
| G-85 | str_trim 空白字符串堆损坏 | ✅ 已修复 |
| G-86 | map/filter 内联闭包 | ✅ 已修复 |

## CI 门禁顺序

```
1.  cargo test                          # 全量测试
2.  cargo test dual_                    # L1 双后端等价性
3.  cargo test "typecheck::"            # L2 类型系统健全性
4.  cargo test codegen_e2e              # 代码生成 E2E
5.  cargo clippy -- -D warnings         # Clippy 零警告
6.  cargo test -- --ignored             # 已知差距
```

## 提交信息规范

```
<type>(<scope>): <简短描述>

<不变量类别>: L1 / L2 / L3
测试: <测试名> (<文件路径>)
```
