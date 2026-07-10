#!/usr/bin/env python3
"""Generate real-world Mimi feature tests for dual-backend availability."""

from pathlib import Path

ROOT = Path(__file__).resolve().parent

tests = {}

# ---------- Core language ----------
tests["core_basic_control.mimi"] = r'''
func main() -> i32 {
    let mut n = 0
    let mut i = 0
    while i < 5 {
        n += i
        i += 1
    }
    for j in range(0, 3) {
        n += j
    }
    if n == 13 {
        0
    } else {
        1
    }
}
'''

tests["core_functions_recursion.mimi"] = r'''
func factorial(n: i32) -> i32 {
    if n <= 1 { 1 } else { n * factorial(n - 1) }
}

func main() -> i32 {
    if factorial(5) == 120 { 0 } else { 1 }
}
'''

tests["core_records.mimi"] = r'''
type Point { x: i32, y: i32 }

func main() -> i32 {
    let p = Point { x: 3, y: 4 }
    if p.x == 3 && p.y == 4 { 0 } else { 1 }
}
'''

tests["core_enums_match.mimi"] = r'''
type Shape { Circle(f64) | Rect(f64, f64) }

func area(s: Shape) -> f64 {
    match s {
        Circle(r) => 3.14159 * r * r,
        Rect(w, h) => w * h
    }
}

func main() -> i32 {
    let c = Circle(1.0)
    let r = Rect(2.0, 3.0)
    if area(c) > 3.0 && area(r) == 6.0 { 0 } else { 1 }
}
'''

tests["core_option_result.mimi"] = r'''
func half(n: i32) -> Option<i32> {
    if n % 2 == 0 { Some(n / 2) } else { None }
}

func div(a: i32, b: i32) -> Result<i32, string> {
    if b == 0 { Err("divide by zero") } else { Ok(a / b) }
}

func main() -> i32 {
    let o = half(8)
    let r = div(10, 2)
    let ok = o.is_some() && o.unwrap_or(0) == 4 && r.is_ok() && r.unwrap_or(0) == 5
    if ok { 0 } else { 1 }
}
'''

tests["core_try_operator.mimi"] = r'''
type Res { Ok(i64) | Err(string) }

func parse_pos(s: string) -> Res {
    let p = str_parse_int(s)
    if p.0 {
        if p.1 > 0 { Ok(p.1) } else { Err("not positive") }
    } else {
        Err("not a number")
    }
}

func half_of(s: string) -> Res {
    let n = parse_pos(s)?
    if n % 2 == 0 { Ok(n / 2) } else { Err("odd") }
}

func main() -> i32 {
    match half_of("20") {
        Ok(n) => if n == 10 { 0 } else { 1 },
        _ => 1
    }
}
'''

tests["core_closures.mimi"] = r'''
func main() -> i32 {
    let add = fn(a: i32, b: i32) -> i32 { a + b }
    let double = fn(x: i32) -> i32 { x * 2 }
    if add(2, 3) == 5 && double(7) == 14 { 0 } else { 1 }
}
'''

tests["core_generics_adt.mimi"] = r'''
type Box<T> { value: T }

func main() -> i32 {
    let b = Box { value: 42 }
    if b.value == 42 { 0 } else { 1 }
}
'''

tests["core_newtype.mimi"] = r'''
newtype UserId = i32

func uid(n: i32) -> UserId { UserId(n) }

func main() -> i32 {
    let id = uid(7)
    match id {
        UserId(n) => if n == 7 { 0 } else { 1 }
    }
}
'''

tests["core_traits_methods.mimi"] = r'''
trait Desc {
    func describe() -> string
}

type Dog { name: string }

impl Desc for Dog {
    func describe() -> string {
        "Dog: " + self.name
    }
}

func main() -> i32 {
    let d = Dog { name: "Rex" }
    if d.describe() == "Dog: Rex" { 0 } else { 1 }
}
'''

tests["core_list_index.mimi"] = r'''
func first(xs: List<i32>) -> i32 {
    xs[0]
}

func main() -> i32 {
    let xs = [10, 20, 30]
    if first(xs) == 10 { 0 } else { 1 }
}
'''

tests["core_shared_weak.mimi"] = r'''
func main() -> i32 {
    shared s = 42
    weak w = s
    let u = w.upgrade()
    if u.deref() == 42 { 0 } else { 1 }
}
'''

# ---------- Concurrency ----------
tests["concurrency_atomic.mimi"] = r'''
func main() -> i32 {
    let a = atomic_i32_new(5)
    atomic_i32_store(a, 10)
    let v = atomic_i32_load(a)
    let prev = atomic_i32_fetch_add(a, 3)
    let ok = v == 10 && prev == 10 && atomic_i32_load(a) == 13
    if ok { 0 } else { 1 }
}
'''

tests["concurrency_mutex.mimi"] = r'''
func main() -> i32 {
    let m = mutex_new(0)
    let token = mutex_lock(m)
    mutex_set(token, 42)
    let v = mutex_get(token)
    mutex_unlock(token)
    if v == 42 { 0 } else { 1 }
}
'''

tests["concurrency_channel.mimi"] = r'''
func main() -> i32 {
    let ch = channel_new()
    channel_send(ch, 99)
    let v = channel_recv(ch)
    if v == 99 { 0 } else { 1 }
}
'''

tests["concurrency_spawn_await.mimi"] = r'''
func add(a: i32, b: i32) -> i32 { a + b }

func main() -> i32 {
    let t = spawn add(2, 3)
    let r = await t
    if r == 5 { 0 } else { 1 }
}
'''

tests["concurrency_actor.mimi"] = r'''
actor Counter {
    mut count: i32 = 0;
    func increment() { self.count = self.count + 1 }
    func get() -> i32 { return self.count }
}

func main() -> i32 {
    let c = Counter.spawn()
    c.increment()
    c.increment()
    let v = c.get()
    if v == 2 { 0 } else { 1 }
}
'''

# ---------- Stdlib ----------
tests["std_prelude.mimi"] = r'''
func main() -> i32 {
    let a = clamp(10, 0, 5)
    let b = double(7)
    let c = identity(7)
    let d = is_even(4)
    let ok = a > 4 && a < 6 && b > 13 && b < 15 && c == 7 && d
    if ok { 0 } else { 1 }
}
'''

tests["std_io.mimi"] = r'''
use std::io

func main() -> i32 {
    print_raw("io test")
    print_line(" ok")
    0
}
'''

tests["std_strings.mimi"] = r'''
use std::strings

func main() -> i32 {
    let s = "  hello world  "
    let t = trim(s)
    let parts = split(t, " ")
    let joined = join(parts, "-")
    let up = to_upper(joined)
    if t == "hello world" && len(parts) == 2 && joined == "hello-world" && up == "HELLO-WORLD" {
        0
    } else {
        1
    }
}
'''

tests["std_collections.mimi"] = r'''
use std::collections

type Item { val: i32 }

func main() -> i32 {
    let xs = [3, 1, 4, 1, 5]
    let doubled = map_list(xs, fn(x: i32) -> i32 { x * 2 })
    let evens = filter_list(xs, fn(x: i32) -> bool { x % 2 == 0 })
    let total = reduce_list(xs, fn(a: i32, b: i32) -> i32 { a + b }, 0)
    if len(doubled) != 5 || len(evens) != 1 || total != 14 { return 1 }
    // Test builtin reduce with lambda (type inference now works)
    let total2 = reduce(xs, fn(a: i32, e: i32) -> i32 { a + e }, 0)
    if total2 != 14 { return 2 }
    // Test struct-typed filter (was hitting i1/i64 ABI mismatch in codegen)
    let items = [Item { val: 10 }, Item { val: 20 }]
    let gt2 = filter(items, fn(x: Item) -> bool { x.val > 15 })
    if len(gt2) != 1 { return 3 }
    let mapped = map(items, fn(x: Item) -> i32 { x.val })
    if len(mapped) != 2 { return 4 }
    let mut sum = 0
    for v in mapped { sum = sum + v }
    if sum != 30 { return 5 }
    // Test trait method dispatch with struct-typed elements
    let flt = filter_list(items, fn(x: Item) -> bool { x.val > 15 })
    if len(flt) != 1 { return 6 }
    let mlt = map_list(items, fn(x: Item) -> i32 { x.val })
    if len(mlt) != 2 { return 7 }
    let mut sum2 = 0
    for v in mlt { sum2 = sum2 + v }
    if sum2 != 30 { return sum2 }
    0
}
'''

tests["std_maps.mimi"] = r'''
func main() -> i32 {
    let m = map_new()
    let m2 = map_set(m, "a", 1)
    let m3 = map_set(m2, "b", 2)
    let found = map_get(m3, "a")
    let sz = map_size(m3)
    let has = has_key(m3, "b")
    if found.0 && sz == 2 && has { 0 } else { 1 }
}
'''

tests["std_mymath.mimi"] = r'''
use std::mymath

func main() -> i32 {
    let a = abs(-5)
    let b = factorial(4)
    let c = is_prime(7)
    let d = sqrt_val(16.0)
    let diff = abs_float(d - 4.0)
    if a == 5 && b == 24 && c && diff < 0.001 { 0 } else { 1 }
}
'''

tests["std_json.mimi"] = r'''
type Config { name: string, count: i64 }
type JsonRecord { name: string, value: i32, flag: bool }

func main() -> i32 {
    // Record deserialization
    let s = "{\"name\":\"mimi\",\"count\":42}"
    let cfg = from_json::<Config>(s)
    if cfg.name != "mimi" || cfg.count != 42 { return 1 }

    // List deserialization
    let nums = from_json::<List<i32>>("[10, 20, 30]")
    if nums[0] != 10 || nums[1] != 20 || nums[2] != 30 { return 2 }

    // Empty list
    let empty = from_json::<List<i32>>("[]")
    if len(empty) != 0 { return 3 }

    // String list
    let items = from_json::<List<string>>("[\"hello\",\"world\"]")
    if items[0] != "hello" || items[1] != "world" { return 4 }

    // Float list
    let floats = from_json::<List<f64>>("[1.5, 2.5, 3.5]")
    if floats[0] != 1.5 || floats[1] != 2.5 || floats[2] != 3.5 { return 5 }

    // Bool list
    let bools = from_json::<List<bool>>("[true, false, true]")
    if bools[0] != true || bools[1] != false || bools[2] != true { return 6 }

    // Record to_json
    let rec = JsonRecord { name: "hi", value: 7, flag: true }
    if to_json(rec) != "{\"flag\":true,\"name\":\"hi\",\"value\":7}" { return 19 }

    // List of records
    let rec_list = from_json::<List<JsonRecord>>("[{\"name\":\"x\",\"value\":10,\"flag\":true},{\"name\":\"y\",\"value\":20,\"flag\":false}]")
    if len(rec_list) != 2 { return 7 }
    if rec_list[0].name != "x" || rec_list[0].value != 10 || rec_list[0].flag != true { return 8 }
    if rec_list[1].name != "y" || rec_list[1].value != 20 || rec_list[1].flag != false { return 9 }
    let empty_rec = from_json::<List<JsonRecord>>("[]")
    if len(empty_rec) != 0 { return 70 }
    let rec_json = to_json(rec_list)
    if rec_json != "[{\"flag\":true,\"name\":\"x\",\"value\":10},{\"flag\":false,\"name\":\"y\",\"value\":20}]" { return 71 }
    let empty_rec_json = to_json(empty_rec)
    if empty_rec_json != "[]" { return 72 }

    // --- to_json scalars ---
    if to_json(42) != "42" { return 10 }
    if to_json(true) != "true" { return 11 }
    if to_json(false) != "false" { return 12 }
    if to_json("hello") != "\"hello\"" { return 14 }

    // List to_json
    let list_to_json = from_json::<List<i32>>("[1, 2, 3]")
    if to_json(list_to_json) != "[1,2,3]" { return 15 }
    let str_list = from_json::<List<string>>("[\"a\",\"b\"]")
    if to_json(str_list) != "[\"a\",\"b\"]" { return 16 }
    let f64_list = from_json::<List<f64>>("[1.5, 2.5]")
    if to_json(f64_list) != "[1.5,2.5]" { return 17 }
    let bool_list = from_json::<List<bool>>("[true, false]")
    if to_json(bool_list) != "[true,false]" { return 18 }

    0
}
'''

tests["std_time.mimi"] = r'''
use std::time

func main() -> i32 {
    let t1 = timestamp_ms()
    sleep_ms(10)
    let t2 = timestamp_ms()
    let e = elapsed(t1)
    if t2 >= t1 && e >= 0 { 0 } else { 1 }
}
'''

tests["std_datetime.mimi"] = r'''
use std::datetime

func main() -> i32 {
    let now = now_secs()
    let future = days_from_now(1)
    let fmt = format_duration_secs(3661)
    if future > now && len(fmt) > 0 { 0 } else { 1 }
}
'''

tests["std_env.mimi"] = r'''
use std::env

func main() -> i32 {
    let args = cli_args()
    let has_path = has_var("PATH")
    let count = arg_count()
    if count >= 0 && has_path { 0 } else { 1 }
}
'''

tests["std_fs.mimi"] = r'''
func main() -> i32 {
    let path = "/tmp/mimi_rw_test.txt"
    let _ = write_file(path, "hello world")

    // Read back and verify content
    match read_file(path) {
        Ok(content) => {
            if content != "hello world" { 2 }
            else { 0 }
        }
        Err(_) => 1
    }
}
'''

tests["std_set.mimi"] = r'''
func main() -> i32 {
    let s: Set<i32> = {1, 2, 1}
    let s2 = s.insert(3)
    let sz = s2.size()
    let has = s2.contains(2)
    if sz == 3 && has { 0 } else { 1 }
}
'''

tests["std_csv.mimi"] = r'''
use std::csv

func main() -> i32 {
    let rows = parse("a,b\n1,2")
    let cell = get(rows, 1, 1)
    if len(rows) == 2 && get(rows, 0, 0) == "a" && cell == "2" { 0 } else { 1 }
}
'''

tests["std_template.mimi"] = r'''
use std::template

func main() -> i32 {
    let vars = map_new()
    let vars2 = map_set(vars, "name", "Mimi")
    let result = simple_render("Hello {{name}}!", vars2)
    // Verify the function returns the correct rendered string
    if result == "Hello Mimi!" { 0 } else { 1 }
}
'''

tests["std_crypto.mimi"] = r'''
use std::crypto

func main() -> i32 {
    if !is_valid_hex("0a1f") { return 1 }
    if !is_valid_hex("abcdef") { return 2 }
    let encoded = hex_encode("ABC")
    if encoded == "414243" { 0 } else { 3 }
}
'''

# ---------- Meta / verification ----------
tests["meta_contracts.mimi"] = r'''
func abs(x: i32) -> i32 {
    requires: x >= 0
    ensures: result >= 0
    if x < 0 { -x } else { x }
}

func main() -> i32 {
    if abs(5) == 5 { 0 } else { 1 }
}
'''

tests["meta_comptime_quote.mimi"] = r'''
comptime func make_const() -> i32 {
    21 * 2
}

func main() -> i32 {
    let v = comptime { make_const() }
    if v == 42 { 0 } else { 1 }
}
'''

def main():
    # Clean up stale generated files
    for old in ROOT.glob("*.mimi"):
        old.unlink()
    for name, src in tests.items():
        (ROOT / name).write_text(src.strip() + "\n")
    print(f"Generated {len(tests)} test files in {ROOT}")


if __name__ == "__main__":
    main()
