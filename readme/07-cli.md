# 07 - Mimi CLI 参考

本页按当前 `mimi --help` 说明常用入口。省略源码路径时，命令会从当前目录的 `mimi.toml` 查找包入口；把目录作为路径传入时也会读取该目录的包入口。

## 1. 常用命令

| 命令 | 用途 |
|------|------|
| `mimi check [path]` | 解析并类型检查 Mimi 源码 |
| `mimi run [path]` | 检查并运行程序 |
| `mimi test [path]` | 在一个源文件中运行零参数 `test_*` 函数 |
| `mimi build [path]` | 编译原生程序或导出 LLVM IR |
| `mimi verify [path]` | 用 Z3 验证源码合约 |
| `mimi mir [path]` | 查看确定性 Canonical MIR 或路由回执 |
| `mimi fmt [files...]` | 格式化源码；省略文件时处理当前项目中的 `.mimi` 文件 |
| `mimi lint [files...]` | 检查常见问题 |
| `mimi doc <path>` | 从源码生成文档 |
| `mimi lsp` | 启动编辑器 LSP 服务 |

`.mms` MimiSpec 草图已从语言移除；没有 `mimi promote` 命令。当前只有 `mimi check` 保留草图解析检查，不会执行或转换草图。

## 2. check - 检查源码

```bash
mimi check src/main.mimi
mimi check                         # 使用当前包入口
mimi check sketch.mms              # 仅解析草图语法，不进行 Mimi 类型检查
```

`check` 没有 `--strict` 或 `--verify-rules` 选项。契约证明使用 `mimi verify`。

## 3. run - 运行程序

```bash
mimi run src/main.mimi
mimi run --verify-contracts src/payment.mimi
mimi run src/main.mimi -- input.txt
```

常用选项：

| 选项 | 说明 |
|------|------|
| `--verify-contracts` | 启用运行期 requires/ensures 检查 |
| `--verify-ffi` | 启用外部调用合约检查；默认开启 |
| `--skip-verify-ffi` | 关闭外部调用合约检查 |
| `--mir` | 经实验性 Canonical MIR 执行；未支持的形状直接拒绝 |
| `--allocator system` | 当前唯一实现的分配器，也是默认值；arena/bump 会明确报未实现 |
| `--profile` | 输出函数调用计数和耗时 |
| `--watch`, `-w` | 文件变化后重新运行 |
| `-- <args...>` | 把后续参数传给 Mimi 程序 |

程序的返回值成为 CLI 进程退出码。操作系统通常只保留退出码低 8 位。

## 4. test - 运行测试函数

```mimi
use std::testing;

func test_addition() {
    assert_eq_int(2 + 2, 4)
}

func main() -> i32 { 0 }
```

```bash
mimi test tests/math_tests.mimi
mimi test --filter addition tests/math_tests.mimi
mimi test --verbose tests/math_tests.mimi
```

只收集以 `test_` 开头且没有参数的函数。布尔测试返回 `false` 会失败；断言报错或运行时错误也会失败。文件需通过完整类型检查，因此保留 `main` 入口。每次调用只接受一个源文件；把目录传入时会读取包入口，不会递归收集目录中的测试。没有匹配的测试时命令会提示并以成功结束。

选项：`--filter, -f` 按函数名子串过滤；`--verbose, -v` 显示失败详情。

## 5. build - 原生编译

```bash
mimi build src/main.mimi
mimi build --emit-ir src/main.mimi > output.ll
mimi build src/main.mimi --link-search ./native --link-lib math_ext --output app
```

| 选项 | 说明 |
|------|------|
| `--output, -o PATH` | 指定产物路径 |
| `--emit-ir` | 输出 LLVM IR，不链接程序 |
| `--mir` | 经实验性 Canonical MIR 原生后端；未支持的形状直接拒绝 |
| `--verify-contracts` | 把合约编译为运行期断言 |
| `--verify-ffi` | 用 Z3 检查外部调用前提 |
| `--link-search, -L DIR` | 添加原生库搜索目录，可重复 |
| `--link-lib, -l NAME` | 链接库，可重复；只写库名，不写 `lib` 前缀、文件后缀或路径 |
| `--shared` | 生成共享库 |
| `--target TRIPLE` | 指定交叉编译目标 |
| `--no-std` | 以 freestanding/no-stdlib 模式链接 |

链接选项面向宿主 C linker；交叉编译时库必须匹配目标平台和架构。标量 FFI 范围及运行时动态绑定见[FFI 指南](./10-ffi.md)。

## 6. verify 与 MIR 检查

```bash
mimi verify src/account.mimi
mimi verify --stats src/account.mimi
mimi verify --mir src/account.mimi
mimi mir src/main.mimi
mimi mir --receipt src/main.mimi
mimi mir --all src/main.mimi
```

`verify --stats` 显示验证统计；`verify --dump-z3` 把 SMT-LIB2 写到 stderr。实验性 `verify --mir` 对未支持的形状 fail-closed，且不能与 `--dump-z3` 合用。MIR 命令用于检查 lowering 结果；`--receipt` 输出确定性路由回执，`--all` 纳入显式导入模块。

## 7. 格式、lint 与文档

```bash
mimi fmt src/main.mimi
mimi fmt --check src
mimi lint src/main.mimi
mimi lint --fail-on-warnings src/main.mimi
mimi doc src/main.mimi
```

`fmt --check` 在需要格式化时以非零状态退出，不改写文件。lint 的警告转错误由 `--fail-on-warnings` 控制。

## 8. 包与依赖

```bash
mimi init shop
mimi add local-utils --path ../local-utils
mimi add http-client --git https://example.com/http-client.git --tag v0.2
mimi install
mimi tree
```

`mimi init [name]` 在**当前目录**创建 `mimi.toml` 和缺失的 `main.mimi`；`name` 只设置包名，不创建同名目录，也不会覆盖已有清单。依赖命令还包括 `remove`、`list`、`update`、`search` 和 `publish`，各自选项以 `mimi <command> --help` 为准。

## 9. 其他命令

- `mimi disasm <file>`：反汇编字节码，用于调试。
- `mimi bindgen <file> [--output DIR]`、`mimi emit-*-bindings`：生成语言绑定；这不扩大导入 FFI 的 ABI 范围。
- `mimi abi <core|export|validate|hash|diff|check|emit-*>`：导出、校验或比较 Component ABI。
- `mimi wire <encode|decode|validate-schema>`：封装、解封或校验 Wire 数据。
- `mimi stat [path]`、`mimi stats [path]`：目录分析与语言用量统计。

每个子命令的准确参数以 `mimi <command> --help` 显示为准。
