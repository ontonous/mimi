# 06 - Mimi 模块与包管理

Mimi 的模块是**文件级合并**：`use` 加载一个 `.mimi` 文件，并把该文件的公开声明合并到当前编译单元。调用导入的函数和类型时直接使用名字，不写模块前缀。

## 1. 导入同目录文件

目录结构：

```text
my-project/
├── main.mimi
└── math_utils.mimi
```

`math_utils.mimi`：

```mimi
pub func double(value: i32) -> i32 {
    value * 2
}
```

`main.mimi`：

```mimi
use math_utils;

func main() -> i32 {
    println(double(21));
    0
}
```

运行 `mimi run main.mimi` 会打印 `42`。被导入文件中需要对外使用的函数、类型和 trait 必须标为 `pub`；私有声明不会成为导入接口。

导入后应写 `double(21)`，不能写 `math_utils::double(21)`。`::` 保留给 Flow 转移，例如 `Counter::increment(state)`。

## 2. 标准库导入

标准库同样按文件导入，导出的名字合并到当前作用域：

```mimi
use std::collections;

func main() -> i32 {
    println(sum([1, 2, 3]));
    0
}
```

Map 工具属于 `std::maps`，List 工具属于 `std::collections`。导入后调用 `new(...)`、`get(...)` 等导出名，不写 `maps::get(...)`。如果导入的模块有同名公开声明，Mimi 会报告冲突，不会静默覆盖。

## 3. 当前不支持的模块写法

- 不支持在 `.mimi` 文件里嵌套 `module Name { ... }`；解析器以 E0445 拒绝这种写法。
- 不支持 `module::function()`、`use std::collections::Map` 这样的符号级路径导入或模块前缀调用。
- 旧式 `@import` 不属于当前模块入口。用 `use path;` 加载文件。

权威规则见 [`docs/language-spec.md` §6.14](../docs/language-spec.md#614-module-system-use-merge-naming-self-description-and--reservation-stable-039137)。

## 4. 包清单与依赖

在现有目录里运行 `mimi init [name]` 会创建 `mimi.toml` 和缺失的 `main.mimi`。`name` 只写入包名，不会创建同名目录；命令不会覆盖已有 `mimi.toml`。

```bash
mimi init shop
mimi add local-utils --path ../local-utils
mimi add http-client --git https://example.com/http-client.git --tag v0.2
mimi install
mimi tree
```

依赖写入 `mimi.toml` 的 `[[dependencies]]` 表。路径依赖、Git 依赖和 registry 版本依赖都可用 `mimi add` 配置；以 `mimi add --help` 查看当前选项。

典型项目布局：

```text
my-project/
├── mimi.toml
├── main.mimi
├── math_utils.mimi
└── .mimi/             # 安装的依赖数据
```

默认包入口是 `main.mimi`，也可在 `[package]` 中用 `entry` 指定入口文件。

## 5. 补充

- `use std::io;` 导入 `print_line` 等标准库 I/O 函数；内建的 `println` 无需导入。
- 标准库模块清单见仓库根目录 `std/` 和 [README 标准库概览](../README.md#standard-library-24-modules)。
- MimiSpec `.mms` 和 `mms{}` 已从语言移除；模块系统不加载草图文件。
