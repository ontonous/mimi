# 06 - Mimi 模块与包管理

---

## 1. 文件即模块

每个 `.mimi` 文件自动成为一个模块，文件名即模块名：

```
src/
├── main.mimi        # main 模块
├── models.mimi      # models 模块
└── utils.mimi       # utils 模块
```

---

## 2. 模块声明

### 2.1 使用 module 关键字

```mimi
module Shop {
    pub func process_order() {
        // ...
    }

    func internal_helper() {
        // ...
    }
}
```

### 2.2 嵌套模块

```mimi
module Company {
    module Engineering {
        func build_feature() {
            // ...
        }
    }

    module Marketing {
        func launch_campaign() {
            // ...
        }
    }
}
```

---

## 3. 可见性

### 3.1 pub 关键字

默认所有定义都是私有的，使用 `pub` 导出：

```mimi
module Shop {
    pub func process_order() { ... }    // 公开
    func internal_helper() { ... }       // 私有（默认）

    pub type Order { ... }               // 公开类型
    type InternalState { ... }           // 私有类型

    pub actor OrderProcessor { ... }     // 公开 Actor
    actor InternalCache { ... }          // 私有 Actor
}
```

### 3.2 可见性规则

- 私有定义只能在同一模块内访问
- 公开定义可以被其他模块通过 `use` 导入
- 嵌套模块可以访问外层模块的私有定义

```mimi
module Outer {
    let secret = 42;   // 私有

    module Inner {
        func get_secret() -> i32 {
            secret     // OK：嵌套模块可访问外层私有
        }
    }
}
```

---

## 4. 导入

### 4.1 use 导入

```mimi
use std::collections::Map;
use crate::models::User;
use super::helper;
use another_package::some_func;
```

### 4.2 路径语法

| 路径 | 含义 |
|------|------|
| `std::collections::Map` | 标准库模块 |
| `crate::models::User` | 当前包的模块 |
| `super::helper` | 上级模块 |
| `another_package::func` | 外部包 |

### 4.3 字段与方法访问

```mimi
// 模块路径用 ::
let user = User::new("Alice");

// 字段访问用 .
let name = user.name;

// 方法调用用 .
let display = user.to_string();
```

### 4.4 @import 兼容

```mimi
// 保留兼容，推荐使用 use
@import "models.mms"
@import "utils.mms"
```

---

## 5. 包管理

### 5.1 mimi.toml

项目根目录的 `mimi.toml` 定义包配置：

```toml
[package]
name = "shop"
version = "0.1.0"
description = "E-commerce shop"

[dependencies]
std = "1.0"
payment-sdk = { path = "../payment-sdk" }
database = { git = "https://github.com/example/database" }
```

### 5.2 包管理命令

```bash
# 初始化新包
mimi init my_project

# 添加依赖
mimi add payment-sdk
mimi add database --git https://github.com/example/database

# 移除依赖
mimi remove payment-sdk

# 列出依赖
mimi list
```

---

## 6. 项目结构

```
my_project/
├── mimi.toml           # 包配置
├── src/
│   ├── main.mimi       # 入口文件
│   ├── models.mimi     # 数据模型
│   ├── services/
│   │   ├── payment.mimi
│   │   └── auth.mimi
│   └── utils.mimi
├── tests/
│   └── integration.mimi
└── sketches/           # 草图文件（可选）
    └── design.mms
```

### 6.1 入口文件

`src/main.mimi` 是程序入口：

```mimi
func main() -> i32 {
    println("Hello, Mimi!");
    0
}
```

### 6.2 模块组织

```mimi
// src/main.mimi
use crate::models::User;
use crate::services::payment;

func main() -> i32 {
    let user = User::new("Alice");
    payment::process(user);
    0
}
```

---

## 7. MimiSpec 集成

### 7.1 mms 块

在 `.mimi` 文件中嵌入 MimiSpec 意图描述：

```mimi
module Shop {
    mms {
        module Shop:
            desc "订单管理模块"
            rule "所有操作必须有日志"
    }

    // Mimi 实现
}
```

### 7.2 契约提取

从 `mms {}` 块提取契约：

```bash
mimi check --extract-contracts file.mimi
```

---

## 8. 模块间通信

### 8.1 通过函数调用

```mimi
// models.mimi
pub type User {
    name: string
    age: i32
}

// main.mimi
use crate::models::User;

func main() -> i32 {
    let user = User { name: "Alice".into(), age: 30 };
    println(user.name);
    0
}
```

### 8.2 通过 Actor

```mimi
// counter.mimi
pub actor Counter {
    mut count: i32 = 0;

    pub func increment() {
        self.count += 1;
    }

    pub func get() -> i32 {
        self.count
    }
}

// main.mimi
use crate::counter::Counter;

func main() -> i32 {
    let c = Counter.spawn();
    await c.increment();
    let count = await c.get();
    println(count);
    0
}
```

### 8.3 通过 Trait

```mimi
// display.mimi
pub trait Display {
    func to_string() -> string;
}

// models.mimi
use crate::display::Display;

pub type User {
    name: string
}

impl Display for User {
    func to_string() -> string {
        "User(" + self.name + ")"
    }
}
```

---

## 9. 标准库

Mimi 提供以下标准库模块：

### 9.1 内置函数

以下函数无需导入，直接可用：

| 函数 | 说明 |
|------|------|
| `println(msg)` | 打印并换行 |
| `print(msg)` | 打印不换行 |
| `eprintln(msg)` | 打印到标准错误 |
| `assert(cond)` | 断言条件为真 |
| `assert_eq(a, b)` | 断言两个值相等 |
| `assert_ne(a, b)` | 断言两个值不相等 |
| `len(collection)` | 获取长度 |
| `push(list, item)` | 向列表添加元素 |
| `pop(list)` | 从列表移除最后一个元素 |
| `range(start, end)` | 生成整数序列 |
| `abs(x)` | 绝对值 |
| `min(a, b)` / `max(a, b)` | 最小值/最大值 |
| `sqrt(x)` | 平方根 |
| `floor(x)` / `ceil(x)` / `round(x)` | 取整 |
| `to_string(val)` | 转换为字符串 |

### 9.2 std 模块

```mimi
// 基础工具
use std::prelude
let doubled = prelude::double(5)        // 10
let msg = prelude::type_of(42)          // "i32"

// 数学函数
use std::mymath
let x = prelude::sqr(5)                 // 25
let y = mymath::factorial(5)            // 120
let a = mymath::abs(-3)                 // 3

// 集合操作
use std::collections
let total = collections::sum([1, 2, 3]) // 6
let evens = collections::filter_list(nums, fn(x: i32) -> bool { x % 2 == 0 })

// 字符串操作
use std::strings
let upper = strings::to_upper("hello")
let parts = strings::split("a,b,c", ",")

// 文件系统
use std::fs
let content = fs::read("data.txt")
fs::write("output.txt", "content")
let exists = fs::exists("config.toml")

// 随机数
use std::random
let r = random::random_int(1, 100)
let pick = random::random_choice(items)

// 文本处理
use std::text
let slug = text::slugify("Hello World") // "hello-world"
```

### 9.3 可用模块

| 模块 | 说明 | 关键函数 |
|------|------|----------|
| `std::prelude` | 基础工具 | `identity`, `compose`, `pipe`, `tap`, `fail`, `clamp`, `lerp`, `type_of`, `repeat_action` |
| `std::io` | I/O 操作 | `print_line`, `print_raw`, `print_err`, `input_line`, `input_int`, `input_float` |
| `std::mymath` | 数学函数 | `abs`, `factorial`, `fibonacci`, `gcd`, `lcm`, `is_prime`, `mod_pow`, `random_int` |
| `std::collections` | 集合操作 | `sum`, `map_list`, `filter_list`, `reduce_list`, `find`, `dedup`, `any`, `all`, `partition`, `group_by` |
| `std::strings` | 字符串处理 | `contains`, `split`, `join`, `replace`, `trim`, `repeat`, `to_upper`, `to_lower`, `capitalize`, `title` |
| `std::text` | 文本处理 | `slugify`, `wrap_text`, `camel_to_snake`, `is_blank`, `is_numeric` |
| `std::random` | 随机数 | `random_int`, `random_float`, `random_choice`, `random_sample`, `shuffle` |
| `std::result` | Result 组合子 | `is_ok_result`, `unwrap_or`, `map_result`, `map_err_result` |
| `std::fs` | 文件系统 | `read`, `write`, `exists`, `read_lines`, `write_lines`, `file_size` |
| `std::json` | JSON 操作 | `to_json`, `from_json`, `get_string`, `get_int`, `get_bool`, `array_length` |
| `std::maps` | Map 操作 | `new`, `get`, `set`, `has_key`, `remove`, `merge`, `to_list`, `filter_keys`, `map_values` |
| `std::time` | 时间操作 | `timestamp`, `timestamp_ms`, `sleep_ms`, `elapsed`, `duration` |
| `std::datetime` | 日期时间 | `format_duration_secs`, `days_from_now`, `is_future`, `is_past`, `sleep_until` |
| `std::net` | 网络操作 | `tcp_connect`, `tcp_listen`, `tcp_send`, `tcp_recv`, `fetch`, `fetch_post` |
| `std::env` | 环境变量 | `get_var`, `cli_args`, `get_var_or`, `has_var`, `arg_count` |
| `std::testing` | 测试工具 | `assert_eq_int`, `assert_true`, `assert_false`, `assert_eq_string`, `assert_eq_bool` |
```
