# 10 - FFI 范围与链接

本页说明 **Mimi 调用宿主 C 函数** 的当前执行范围。Canonical MIR 标量 FFI 已接入 `mimi run`、`mimi build`、`mimi verify` 和直接 MIR 消费入口；未覆盖的 ABI 会在执行前 fail-closed，并返回 `MIR-FFI-DECLARATION-001`。

## 1. 当前支持的声明

调用的声明必须满足以下 ABI：

| 声明位置 | 支持类型 |
|-----------|----------|
| 参数 | `i32`、`i64`、`f32`、`f64`、`bool`，或这些基础类型的透明别名 |
| 返回值 | `i32`、`i64`、`f32`、`f64`、`bool`，或 unit/无返回值 |
| ABI | `extern "C"` |

示例：

```mimi
extern "C" {
    func ffi_add(left: i64, right: i64) -> i64;
}

func main() -> i32 {
    println(ffi_add(20 as i64, 22 as i64));
    0
}
```

对被调用的外部函数，字符串、指针、回调、记录/元组/枚举等聚合参数或结果、variadic、非 C ABI、参数模式、`errno` 转换和 `no_panic` 保护都不在此 profile 内。出现这些形状时，编译器会在执行任何程序副作用或查找宿主符号之前拒绝它；不会回退到旧 FFI runtime。仅声明但没有调用的外部函数不会因为此边界单独拒绝程序。

## 2. 运行时绑定（`mimi run`）

运行时使用 `MIMI_FFI_LIB` 指向**一个动态库文件**。例如，在 Linux 上准备 C 实现：

```c
// ffi.c
#include <stdint.h>

int64_t ffi_add(int64_t left, int64_t right) {
    return left + right;
}
```

```bash
cc -shared -fPIC -o libmimi_math.so ffi.c
MIMI_FFI_LIB="$PWD/libmimi_math.so" mimi run main.mimi
# 输出 42
```

显式指定的路径是严格绑定：库无法加载或找不到所需符号时，运行失败，不会改用其他库。未设置 `MIMI_FFI_LIB` 时，运行时只探测平台的系统 libc/libm 候选；这不包含项目目录中的自定义库。

## 3. 原生链接（`mimi build`）

原生二进制由宿主 C linker 链接。`MIMI_FFI_LIB` 只配置 Bytecode VM 的动态绑定，不会自动传给原生链接器。传入库目录和库名：

```bash
cc -c ffi.c -o ffi.o
ar rcs libmimi_math.a ffi.o
mimi build main.mimi \
  --link-search "$PWD" \
  --link-lib mimi_math \
  --output app
./app
# 输出 42
```

`--link-search DIR`（短写 `-L DIR`）和 `--link-lib NAME`（短写 `-l NAME`）可以重复指定。库名写作 `mimi_math`，不加 `lib` 前缀、文件后缀或 linker 参数。链接库仍须符合目标平台的原生 C ABI 与目标架构。

也可以用 `mimi build --emit-ir main.mimi` 检查外部符号声明生成的 LLVM IR；该操作不执行程序，也不链接最终二进制，因此同时提供的 `--link-search` / `--link-lib` 选项不会参与此输出。

## 4. 合约检查

- `mimi run` 默认检查标量外部调用上的 FFI 合约；使用 `--skip-verify-ffi` 可关闭这项运行时检查。
- `mimi run --verify-ffi` 显式开启检查。`mimi build --verify-ffi` 使用 Z3 验证外部调用前提。
- `mimi verify main.mimi` 对源码中的合约生成验证结果；`mimi verify --mir main.mimi` 请求 Canonical MIR verifier，并对不支持形状 fail-closed。

## 5. 导出与绑定生成

Mimi 导出为共享库和生成语言绑定是另一条工具链：CLI 提供 `mimi build --shared`、`mimi emit-c-headers`、`mimi emit-rust-bindings`、`mimi emit-go-bindings`、`mimi emit-py-bindings`、`mimi emit-node-bindings`、`mimi emit-cpp-bindings`、`mimi emit-java-bindings` 和 `mimi bindgen`。这些命令的存在不扩大本页描述的**导入**标量 ABI；具体生成器的输入限制以实际命令输出为准。
