# Mimi FFI 跨语言示例：math

本目录展示如何将 Mimi 模块编译为共享库，并从 C、Rust、Go、Python、Node.js、Java 调用。

## 1. 编译 Mimi 共享库

```bash
mimi build --shared math.mimi -o libmath.so
```

## 2. 生成绑定

```bash
mimi bindgen math.mimi -o bindings
```

生成文件：

- `bindings/math.h` — C 头文件（含运行时 API 声明）
- `bindings/math.hpp` — C++ RAII 包装头文件
- `bindings/math.rs` — Rust FFI 绑定
- `bindings/math.go` — Go CGO 绑定
- `bindings/math_pybind.cpp` / `bindings/math.pyi` — Python pybind11 绑定
- `bindings/math_napi.c` / `bindings/math.d.ts` — Node.js N-API 绑定
- `bindings/Math.java` / `bindings/Math_jni.c` — Java JNI 绑定

## 3. C 调用示例

```c
#include "bindings/math.h"
#include <stdio.h>

int main() {
    int64_t r = add(2, 3);
    printf("add = %lld\n", r);

    struct Point p = { .x = 10, .y = 20 };
    printf("point_sum = %lld\n", point_sum(p));

    char* msg = greet("Mimi");
    printf("%s\n", msg);
    mimi_string_free(msg);
    return 0;
}
```

编译：

```bash
gcc -o math_c main.c -I. -L. -lmath -lmimi_runtime
```

## 4. Rust 调用示例

在 `Cargo.toml` 中把生成的 `math.rs` 作为模块：

```rust
mod math;

fn main() {
    let r = math::add(2, 3);
    println!("add = {r}");

    let p = math::MimiPoint { x: 10, y: 20 };
    println!("point_sum = {}", math::point_sum(p));

    println!("{}", math::greet("Mimi"));

    unsafe extern "C" fn cb(a: i64, b: i64) -> i64 { a + b }
    println!("apply_callback = {}", math::apply_callback(cb, 5));
}
```

## 5. Go 调用示例

```go
package main

import (
    "fmt"
    math "bindings"
)

func main() {
    fmt.Println("add =", math.Add(2, 3))

    p := math.Point{X: 10, Y: 20}
    fmt.Println("point_sum =", math.Point_sum(p))

    fmt.Println(math.Greet("Mimi"))

    cb := func(a, b int64) int64 { return a + b }
    fmt.Println("apply_callback =", math.Apply_callback(cb, 5))
}
```

## 6. Python 调用示例

```python
import math

print(math.add(2, 3))
p = math.Point()
p.x = 10
p.y = 20
print(math.point_sum(p))
print(math.greet("Mimi"))
```

## 7. Node.js 调用示例

```typescript
import * as math from './bindings/math.node';

console.log(math.add(2, 3));
console.log(math.point_sum({ x: 10, y: 20 }));
console.log(math.greet("Mimi"));
```

## 8. Java 调用示例

```java
public class Main {
    static {
        System.loadLibrary("mimi_math");
    }

    public static void main(String[] args) {
        System.out.println(Math.add(2, 3));
        Math.Point p = new Math.Point();
        p.x = 10; p.y = 20;
        System.out.println(Math.point_sum(p));
        System.out.println(Math.greet("Mimi"));
    }
}
```

## 注意事项

- 字符串返回值由 Mimi 运行时分配，调用方需使用 `mimi_string_free` 释放。
- `#[repr(C)]` record 以值传递，绑定生成器会生成对应的语言结构体。
- 当前 Rust / Go / C++ / Python 绑定生成器已支持 `func(...)` 回调参数；Node.js / Java 的回调包装仍在完善中。
