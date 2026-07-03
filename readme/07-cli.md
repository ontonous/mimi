# 07 - Mimi CLI 参考

---

## 1. 命令总览

| 命令 | 说明 |
|------|------|
| `mimi check <file>` | 类型检查 |
| `mimi run <file>` | 类型检查并运行 |
| `mimi test <file>` | 运行测试函数 |
| `mimi build <file>` | 编译为原生代码 |
| `mimi verify <file>` | 验证契约（Z3 SMT） |
| `mimi promote <file>` | 提升草图为生产代码 |
| `mimi doc <file>` | 生成文档 |
| `mimi lsp` | 启动 LSP 服务器 |
| `mimi init [name]` | 初始化新包 |
| `mimi add <dep>` | 添加依赖 |
| `mimi remove <dep>` | 移除依赖 |
| `mimi list` | 列出依赖 |

---

## 2. check - 类型检查

```bash
mimi check <file.mimi>
```

### 选项

| 选项 | 说明 |
|------|------|
| `--extract-contracts` | 提取并显示 `mms{}` 块中的 MimiSpec 契约 |
| `--strict` | 对含 MimiSpec 意图后缀（`$$` 等）的文件强制锁语义 |
| `--verify-rules` | 验证 `mms{}` 中 MimiSpec rule 附着一致性 |

### 示例

```bash
# 基本类型检查
mimi check src/main.mimi

# 提取契约
mimi check --extract-contracts src/payment.mimi

# 严格模式
mimi check --strict src/critical.mimi
```

### 输出

成功：
```
✓ Type checking passed: src/main.mimi
```

失败：
```
✗ Type checking failed for src/main.mimi with 2 error(s)
  Error [line 5]: type mismatch: expected i32, found string
  Error [line 12]: unknown function: nonexistent
```

---

## 3. run - 运行

```bash
mimi run <file.mimi>
```

先进行类型检查，通过后运行程序。

### 选项

| 选项 | 说明 |
|------|------|
| `--verify-contracts` | 启用运行时契约验证 |
| `--allocator={system,arena,bump}` | 设置默认分配器 |

### 示例

```bash
# 运行程序
mimi run src/main.mimi

# 启用契约验证
mimi run --verify-contracts src/payment.mimi

# 使用 Arena 分配器
mimi run --allocator=arena src/main.mimi
```

---

## 4. test - 运行测试

```bash
mimi test <file.mimi>
```

运行所有以 `test_` 开头的函数。

### 选项

| 选项 | 说明 |
|------|------|
| `--filter <pattern>` | 按模式过滤测试（子串匹配） |
| `--verbose` | 显示详细的失败信息 |
| `--allocator={system,arena,bump}` | 设置默认分配器 |

### 测试函数命名

```mimi
func test_basic_addition() {
    assert_eq(2 + 2, 4)
}

func test_string_concat() {
    assert_eq("hello" + " world", "hello world")
}

func test_with_setup() {
    let data = setup_test_data()
    assert(data.is_valid())
    cleanup(data)
}
```

### 示例

```bash
# 运行测试
mimi test tests/basic.mimi

# 运行所有测试
mimi test tests/

# 过滤测试
mimi test --filter addition tests/basic.mimi

# 显示详细输出
mimi test --verbose tests/basic.mimi
```

### 输出

```
Running 3 test(s)...

  ✓ test_basic_addition
  ✓ test_string_concat
  ✓ test_with_setup

3 passed, 0 failed
```

---

## 5. build - 编译

```bash
mimi build <file.mimi>
```

通过 LLVM 编译为原生代码。

### 选项

| 选项 | 说明 |
|------|------|
| `--emit-ir` | 输出 LLVM IR 而非编译 |

### 示例

```bash
# 编译为可执行文件
mimi build src/main.mimi

# 输出 LLVM IR
mimi build --emit-ir src/main.mimi > output.ll
```

---

## 6. verify - 契约验证

```bash
mimi verify <file.mimi>
```

使用 Z3 SMT 求解器验证 `requires`/`ensures` 契约。

### 示例

```mimi
func withdraw(mut account: Account, amount: f64) -> Result<(), Err> {
    requires: account.balance >= amount
    ensures: account.balance == old(account.balance) - amount

    account.balance -= amount;
    Ok(())
}
```

```bash
mimi verify src/account.mimi
```

### 输出

```
Verifying contracts in src/account.mimi...
  withdraw: ✓ Postcondition verified
```

---

## 7. promote - 提升草图

```bash
mimi promote <file.mms>
```

将 `.mms` 草图文件转换为 `.mimi` 生产文件。

### 示例

```bash
# 提升单个文件
mimi promote sketches/payment.mms

# 提升后文件变为 payment.mimi
```

### 要求

- 所有 `...` 占位符必须已填充
- 缩进体必须转换为花括号体

---

## 8. doc - 生成文档

```bash
mimi doc <file.mimi>
```

从 `desc` 和签名生成文档。

### 示例

```bash
# 生成 Markdown 文档
mimi doc src/payment.mimi > docs/payment.md
```

---

## 9. lsp - 语言服务器

```bash
mimi lsp
```

启动 LSP 服务器（JSON-RPC over stdin/stdout），为编辑器提供：

### 支持的功能

| 功能 | 说明 |
|------|------|
| 诊断 | 解析错误 + 类型错误，实时更新 |
| 代码补全 | 关键字、函数名、类型名、内置函数 |
| 悬停信息 | 函数签名、类型信息、内置函数文档 |
| 跳转定义 | 跳转到函数/类型/模块定义 |
| 查找引用 | 查找符号的所有使用位置 |
| 重命名 | 重命名符号并更新所有引用 |
| 签名帮助 | 函数调用时显示参数信息 |
| 语义高亮 | 基于 AST 的精确语法着色 |
| 文档符号 | 文件内所有定义的概览 |

---

## 10. 包管理命令

### 10.1 init

```bash
mimi init [project_name]
```

初始化新包，创建 `mimi.toml` 和目录结构。

### 10.2 add

```bash
mimi add <dependency>
mimi add <dependency> --git <url>
mimi add <dependency> --path <path>
```

### 10.3 remove

```bash
mimi remove <dependency>
```

### 10.4 list

```bash
mimi list
```

列出所有依赖。

---

## 11. 环境变量

| 变量 | 说明 |
|------|------|
| `MIMI_FFI_LIB` | FFI 共享库搜索路径 |
| `MIMI_PATH` | 模块搜索路径 |

---

## 12. 退出码

| 退出码 | 含义 |
|--------|------|
| 0 | 成功 |
| 1 | 错误（类型检查失败、运行时错误等） |
