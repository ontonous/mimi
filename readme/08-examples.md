# 08 - Mimi 示例集

---

## 1. Hello World

```mimi
func main() -> i32 {
    println("Hello, Mimi!");
    0
}
```

---

## 2. 函数与闭包

```mimi
// 基本函数
func add(a: i32, b: i32) -> i32 {
    a + b
}

// 带契约的函数
func safe_divide(a: f64, b: f64) -> Result<f64, string> {
    requires: b != 0.0
    ensures: result == a / b

    if b == 0.0 {
        Err("division by zero")
    } else {
        Ok(a / b)
    }
}

// 闭包
func main() -> i32 {
    let double = fn(x: i32) -> i32 { x * 2 };
    let add = fn(a: i32, b: i32) -> i32 { a + b };

    let nums = [1, 2, 3, 4, 5];
    let doubled = map(nums, double);
    let sum = reduce(nums, add, 0);

    println(doubled);   // [2, 4, 6, 8, 10]
    println(sum);       // 15
    0
}
```

---

## 3. ADT 与模式匹配

```mimi
type Shape {
    Circle(f64)
    Rectangle(f64, f64)
    Triangle { a: f64, b: f64, c: f64 }
}

func area(s: Shape) -> f64 {
    match s {
        Circle(r) => 3.14159 * r * r,
        Rectangle(w, h) => w * h,
        Triangle { a, b, c } => {
            let s = (a + b + c) / 2.0;
            (s * (s - a) * (s - b) * (s - c)).sqrt()
        }
    }
}

func describe(s: Shape) -> string {
    match s {
        Circle(r) => "Circle with radius " + to_string(r),
        Rectangle(w, h) => "Rectangle " + to_string(w) + "x" + to_string(h),
        Triangle { a, b, c } => "Triangle with sides " + to_string(a) + ", " + to_string(b) + ", " + to_string(c)
    }
}

func main() -> i32 {
    let shapes = [
        Shape::Circle(5.0),
        Shape::Rectangle(4.0, 6.0),
        Shape::Triangle { a: 3.0, b: 4.0, c: 5.0 }
    ];

    for s in shapes {
        println(describe(s), " area = ", area(s));
    }
    0
}
```

---

## 4. 错误处理

```mimi
type Res {
    Ok(i32)
    Err(string)
}

func parse_int(s: string) -> Res {
    // 简化示例
    if s == "42" {
        Res::Ok(42)
    } else {
        Res::Err("not a number")
    }
}

func process() -> Res {
    let n1 = parse_int("42")?;
    let n2 = parse_int("42")?;
    Res::Ok(n1 + n2)
}

// Saga 补偿
func booking() -> Result<(), string> {
    let seat = reserve_seat()?;
    on failure { cancel_seat(seat); }

    let hotel = book_hotel()?;
    on failure { cancel_hotel(hotel); }

    let payment = charge()?;
    on failure { refund(payment); }

    Ok(())
}
```

---

## 5. Actor 并发

```mimi
actor BankAccount {
    mut balance: f64 = 0.0;

    func deposit(amount: f64) {
        self.balance += amount;
    }

    func withdraw(amount: f64) -> Result<f64, string> {
        if self.balance >= amount {
            self.balance -= amount;
            Ok(amount)
        } else {
            Err("insufficient funds")
        }
    }

    func get_balance() -> f64 {
        self.balance
    }
}

func main() -> i32 {
    let account = BankAccount.spawn();

    account.deposit(1000.0);
    let cash = account.withdraw(250.0)?;
    let balance = account.get_balance();

    println("Withdrew: ", cash);
    println("Balance: ", balance);
    0
}
```

---

## 6. Parasteps 并发

```mimi
func fetch_users() -> string { "users data" }
func fetch_orders() -> string { "orders data" }

func load_dashboard() -> string {
    parasteps "加载仪表板数据" {
        let users = spawn fetch_users();
        let orders = spawn fetch_orders();
        let r1 = await users;
        let r2 = await orders;
        r1 + "\n" + r2
    }
}

func main() -> i32 {
    let data = load_dashboard();
    println(data);
    0
}
```

---

## 7. Trait 与泛型

```mimi
trait Display {
    func to_string() -> string;
}

type Point {
    x: f64
    y: f64
}

impl Display for Point {
    func to_string() -> string {
        "Point(" + to_string(self.x) + ", " + to_string(self.y) + ")"
    }
}

func print_item<T>(item: T) where T: Display {
    println(to_string(item));
}

func main() -> i32 {
    let p = Point { x: 1.0, y: 2.0 };
    print_item(p);
    0
}
```

---

## 8. Cap 线性能力

```mimi
cap FileReadCap;
cap FileWriteCap;
cap FullFileAccess = FileReadCap + FileWriteCap;

func read_config(path: string, cap: FileReadCap) -> string {
    let data = std::fs::read(path, cap);
    data
}

func write_config(path: string, data: string, cap: FileWriteCap) {
    std::fs::write(path, data, cap);
    drop(cap);
}

func sync_config(path: string, full: FullFileAccess) {
    let (read, write) = full.split();
    let data = read_config(path, read);
    write_config(path, data, write);
}
```

---

## 9. Arena 区域内存

```mimi
func process_graph(data: List<i32>) -> i32 {
    arena {
        let ref graph = build_graph(data);
        let ref analysis = analyze(graph);
        let result = summarize(analysis);
        result.copy()   // 提取值逃逸 arena
    }
}
```

---

## 10. Comptime 元编程

```mimi
comptime func make_const(name: string, value: i32) -> AST {
    quote! {
        const $(name): i32 = $(value);
    }
}

func main() -> i32 {
    comptime {
        let ast = make_const("MAX_SIZE", 1024);
        ast_dump(ast);
    }
    0
}
```

---

## 11. 列表推导

```mimi
func main() -> i32 {
    let nums = range(0, 10);

    // 平方
    let squares = [x * x for x in nums];
    println(squares);

    // 偶数
    let evens = [x for x in nums if x % 2 == 0];
    println(evens);

    // 字符串转换
    let strings = [to_string(x) for x in nums];
    println(strings);

    0
}
```

---

## 12. 模块与导入

```mimi
// models.mimi
pub type User {
    name: string
    age: i32
}

pub func new_user(name: string, age: i32) -> User {
    User { name: name, age: age }
}

// main.mimi
use crate::models::{User, new_user};

func main() -> i32 {
    let user = new_user("Alice", 30);
    println(user.name);
    0
}
```

---

## 13. MimiSpec 集成

```mimi
func process_order(order: Order) -> Result<(), string> {
    mms {
        func ProcessOrder(order):
            desc "处理订单：验证、扣款、发货"
            rule "订单必须幂等"
            requires: order.status == New
            ensures: order.status == Paid
            steps:
                check inventory
                charge payment
                order.status = Paid to done
    }

    // Mimi 实现
    requires: order.status == New
    ensures: order.status == Paid

    let inventory = check_inventory(order)?;
    if !inventory.available {
        return Err("out of stock");
    }
    charge_payment(order.amount)?;
    order.status = OrderStatus::Paid;
    Ok(())
}
```

---

## 14. FFI 外部函数

```mimi
cap SQLiteCap;

extern "C" {
    fn sqlite3_open(path: string, cap @db: SQLiteCap) -> Result<i64, string>;
    fn sqlite3_exec(db: i64, query: string, cap @db: SQLiteCap) -> Result<(), string>;
    fn sqlite3_close(db: i64, cap @db: SQLiteCap) -> Result<(), string>;
}

func init_database(path: string, cap: SQLiteCap) -> Result<i64, string> {
    let db = sqlite3_open(path, cap)?;
    Ok(db)
}
```

---

## 15. 完整示例：简单 HTTP 服务器

```mimi
cap NetListenCap;
cap NetAcceptCap;

extern "C" {
    fn listen(addr: string, cap @srv: NetListenCap) -> Result<i64, string>;
    fn accept(server: i64, cap @cl: NetAcceptCap) -> Result<(i64, string), string>;
    fn send(client: i64, data: string) -> Result<(), string>;
    fn close(client: i64) -> Result<(), string>;
}

actor HttpServer {
    mut server_fd: i64 = 0;

    func start(addr: string, cap: NetListenCap) {
        self.server_fd = listen(addr, cap)?;
    }

    func handle_request(client_fd: i64) {
        let (fd, request) = accept(self.server_fd, 0)?;
        let response = "HTTP/1.1 200 OK\n\nHello, Mimi!";
        send(fd, response)?;
        close(fd)?;
    }
}

func main() -> i32 {
    let server = HttpServer.spawn();
    server.start("0.0.0.0:8080", net_listen_cap);

    loop {
        let client = server.accept_connection();
        spawn server.handle_request(client);
    }

    0
}
```

---

## 16. 列表操作

```mimi
func main() -> i32 {
    // 创建列表
    let nums = [1, 2, 3, 4, 5]

    // 内置函数
    let len = len(nums)           // 5
    push(nums, 6)                 // [1, 2, 3, 4, 5, 6]
    let last = pop(nums)          // 6

    // 高阶函数
    let doubled = map(nums, fn(x: i32) -> i32 { x * 2 })
    let evens = filter(nums, fn(x: i32) -> bool { x % 2 == 0 })
    let sum = reduce(nums, fn(a: i32, b: i32) -> i32 { a + b }, 0)

    // 集合操作
    let total = sum(nums)         // 15
    let reversed = reverse(nums)  // [5, 4, 3, 2, 1]
    let has_three = contains(nums, 3)  // true

    println("Sum:", sum, "Doubled:", doubled)
    0
}
```

---

## 17. 标准库使用

```mimi
// 数学函数
use std::mymath

func main() -> i32 {
    let x = prelude::sqr(5)          // 25
    let y = prelude::cube_int(3)     // 27
    let z = prelude::clamp(15, 0, 10) // 10
    let f = mymath::factorial(5)     // 120
    let fib = mymath::fibonacci(10)  // 55

    println("Square:", x, "Factorial:", f)
    0
}
```

---

## 18. 测试框架

```mimi
// test_math.mimi
func test_addition() {
    assert_eq(2 + 2, 4)
}

func test_string_operations() {
    let s = "hello world"
    assert_eq(len(s), 11)
    assert_eq(to_string(42), "42")
}

func test_list_operations() {
    let nums = [1, 2, 3]
    assert_eq(len(nums), 3)
    assert_eq(sum(nums), 6)
}

func test_edge_cases() {
    assert_ne(1, 2)
    assert(10 > 5)
}
```

运行测试：

```bash
# 运行所有测试
mimi test test_math.mimi

# 过滤测试
mimi test --filter addition test_math.mimi

# 显示详细输出
mimi test --verbose test_math.mimi
```

---

## 19. 模块化项目

```
my_project/
├── mimi.toml
├── main.mimi
├── models.mimi
└── utils/
    ├── math.mimi
    └── string.mimi
```

```mimi
// main.mimi
use crate::models::User
use crate::utils::math::calculate_tax

func main() -> i32 {
    let user = User { name: "Alice", balance: 1000.0 }
    let tax = calculate_tax(user.balance)
    println("Tax:", tax)
    0
}
```

---

## 20. 编译为原生代码

```bash
# 编译为可执行文件
mimi build main.mimi

# 输出 LLVM IR（调试用）
mimi build --emit-ir main.mimi > output.ll

# 编译并运行
./main
```
