// Real-thread spawn path.
//
// Direct `spawn f(args...)` now runs on a real worker thread by default. This
// suite explicitly checks that path, and also covers the MIMI_EAGER_SPAWN
// escape hatch (by leaving it unset we get the default real-thread behavior).
use super::{build_and_run_native, run_program_with_env, spawn_sum_source};

fn channel_spawn_source(n: usize) -> String {
    format!(
        r#"func send_id(ch: Channel<i64>, x: i64) -> i32 {{
    channel_send(ch, x)
    0
}}
func main() -> i32 {{
    let ch = channel_new()
    for i in range(0, {n}) {{
        let t = spawn send_id(ch, i)
    }}
    let mut sum: i64 = 0
    for i in range(0, {n}) {{
        sum = sum + channel_recv(ch)
    }}
    println(sum)
    channel_drop(ch)
    0
}}
"#
    )
}

fn string_list_spawn_source(n: usize) -> String {
    format!(
        r#"func total_len(xs: List<string>) -> i64 {{
    let mut sum = 0
    for s in xs {{
        sum = sum + len(s)
    }}
    sum
}}
func main() -> i32 {{
    let mut total: i64 = 0
    for i in range(0, {n}) {{
        let xs = ["a", "bb", "ccc"]
        let t = spawn total_len(xs)
        total = total + await t
    }}
    println(total)
    0
}}
"#
    )
}

fn nested_list_spawn_source(n: usize) -> String {
    format!(
        r#"func sum_all(rows: List<List<i32>>) -> i32 {{
    let mut total = 0
    for row in rows {{
        for x in row {{
            total = total + x
        }}
    }}
    total
}}
func main() -> i32 {{
    let mut total = 0
    for i in range(0, {n}) {{
        let rows = [[1, 2], [3, 4, 5], [6]]
        let t = spawn sum_all(rows)
        total = total + await t
    }}
    println(total)
    0
}}
"#
    )
}

fn nested_string_list_spawn_source(n: usize) -> String {
    format!(
        r#"func total_len(rows: List<List<string>>) -> i64 {{
    let mut total: i64 = 0
    for row in rows {{
        for s in row {{
            total = total + len(s)
        }}
    }}
    total
}}
func main() -> i32 {{
    let mut total: i64 = 0
    for i in range(0, {n}) {{
        let rows = [["a", "bb"], ["ccc", "dddd", ""]]
        let t = spawn total_len(rows)
        total = total + await t
    }}
    println(total)
    0
}}
"#
    )
}

fn heap_struct_spawn_source(n: usize) -> String {
    format!(
        r#"type User {{ name: string, age: i32 }}
func describe(u: User) -> i32 {{
    len(u.name) + u.age
}}
func main() -> i32 {{
    let mut total = 0
    for i in range(0, {n}) {{
        let u = User {{ name: "hello", age: 5 }}
        let t = spawn describe(u)
        total = total + await t
    }}
    println(total)
    0
}}
"#
    )
}

fn heap_struct_list_spawn_source(n: usize) -> String {
    format!(
        r#"type Bag {{ items: List<i32>, label: string }}
func total(b: Bag) -> i32 {{
    let mut s = 0
    for x in b.items {{
        s = s + x
    }}
    s + len(b.label)
}}
func main() -> i32 {{
    let mut acc = 0
    for i in range(0, {n}) {{
        let b = Bag {{ items: [1, 2, 3], label: "hi" }}
        let t = spawn total(b)
        acc = acc + await t
    }}
    println(acc)
    0
}}
"#
    )
}

fn nested_scalar_struct_spawn_source(n: usize) -> String {
    format!(
        r#"type Point {{ x: i32, y: i32 }}
type Line {{ a: Point, b: Point }}
func sum_x(l: Line) -> i32 {{
    l.a.x + l.b.x
}}
func main() -> i32 {{
    let mut acc = 0
    for i in range(0, {n}) {{
        let l = Line {{ a: Point {{ x: 1, y: 2 }}, b: Point {{ x: 3, y: 4 }} }}
        let t = spawn sum_x(l)
        acc = acc + await t
    }}
    println(acc)
    0
}}
"#
    )
}

fn recursive_heap_struct_spawn_source(n: usize) -> String {
    format!(
        r#"type Inner {{ name: string, nums: List<i32> }}
type Outer {{ inner: Inner, tag: i32 }}
func score(o: Outer) -> i32 {{
    len(o.inner.name) + len(o.inner.nums) + o.tag
}}
func main() -> i32 {{
    let mut acc = 0
    for i in range(0, {n}) {{
        let o = Outer {{ inner: Inner {{ name: "hi", nums: [1, 2, 3] }}, tag: 10 }}
        let t = spawn score(o)
        acc = acc + await t
    }}
    println(acc)
    0
}}
"#
    )
}

fn scalar_struct_list_spawn_source(n: usize) -> String {
    format!(
        r#"type Point {{ x: i32, y: i32 }}
func sum_points(points: List<Point>) -> i32 {{
    let mut s = 0
    for p in points {{
        s = s + p.x + p.y
    }}
    s
}}
func main() -> i32 {{
    let mut acc = 0
    for i in range(0, {n}) {{
        let pts = [Point {{ x: 1, y: 2 }}, Point {{ x: 3, y: 4 }}]
        let t = spawn sum_points(pts)
        acc = acc + await t
    }}
    println(acc)
    0
}}
"#
    )
}

fn heap_record_list_spawn_source(n: usize) -> String {
    format!(
        r#"type Inner {{ name: string, nums: List<i32> }}
func total(items: List<Inner>) -> i32 {{
    let mut s = 0
    for it in items {{
        s = s + len(it.name) + len(it.nums)
    }}
    s
}}
func main() -> i32 {{
    let mut acc = 0
    for i in range(0, {n}) {{
        let items = [Inner {{ name: "hi", nums: [1, 2, 3] }}, Inner {{ name: "ab", nums: [4, 5] }}]
        let t = spawn total(items)
        acc = acc + await t
    }}
    println(acc)
    0
}}
"#
    )
}

fn real_spawn_run(source: &str) -> Result<String, String> {
    // The default is already real-thread; pass no env to assert that default.
    run_program_with_env(source, &[])
}

fn mutex_parasteps_source(n: usize, per_task: i64) -> String {
    let mut body = String::new();
    for i in 0..n {
        body.push_str(&format!(
            "        let t{} = spawn worker(m, {})\n        let r{} = await t{}\n        if r{} != 0 {{ return {} }}\n",
            i, per_task, i, i, i, (i + 1) % 256
        ));
    }
    format!(
        r#"func worker(m: Mutex<i64>, n: i64) -> i32 {{
    for i in range(0, n) {{
        let g = mutex_lock(m)
        let v = mutex_get(g)
        mutex_set(g, v + 1)
        mutex_unlock(g)
    }}
    0
}}
func main() -> i32 {{
    let m = mutex_new(0)
    parasteps {{
{}
    }}
    let g = mutex_lock(m)
    let v = mutex_get(g)
    mutex_unlock(g)
    println(v)
    mutex_drop(m)
    0
}}
"#,
        body,
    )
}

fn atomic_bool_lock_parasteps_source(n: usize, per_task: i64) -> String {
    let mut body = String::new();
    for i in 0..n {
        body.push_str(&format!(
            "        let t{} = spawn worker(flag, count, {})\n        let r{} = await t{}\n        if r{} != 0 {{ return {} }}\n",
            i, per_task, i, i, i, (i + 1) % 256
        ));
    }
    format!(
        r#"func worker(flag: AtomicBool, count: AtomicI64, n: i64) -> i32 {{
    for i in range(0, n) {{
        let mut done = 0
        while done == 0 {{
            done = atomic_bool_compare_exchange(flag, false, true)
        }}
        atomic_i64_fetch_add(count, 1)
        atomic_bool_store(flag, false)
    }}
    0
}}
func main() -> i32 {{
    let flag = atomic_bool_new(false)
    let count = atomic_i64_new(0)
    parasteps {{
{}
    }}
    let total = atomic_i64_load(count)
    println(total)
    atomic_i64_drop(count)
    atomic_bool_drop(flag)
    0
}}
"#,
        body,
    )
}

fn spawn_await_loop_source(n: i32) -> String {
    format!(
        r#"func id(x: i32) -> i32 {{ x }}
func main() -> i32 {{
    let mut sum = 0
    for i in range(0, {n}) {{
        let t = spawn id(i)
        sum = sum + await t
    }}
    println(sum)
    0
}}
"#
    )
}

fn nested_spawn_chain_source(depth: i32) -> String {
    format!(
        r#"func worker(x: i32, depth: i32) -> i32 {{
    if depth == 0 {{ x }} else {{
        let t = spawn worker(x + 1, depth - 1)
        let r = await t
        r
    }}
}}

func main() -> i32 {{
    let t = spawn worker(0, {depth})
    let r = await t
    println(r)
    0
}}
"#
    )
}

fn nested_spawn_fanout_source(depth: i32) -> String {
    format!(
        r#"func worker(x: i32, depth: i32) -> i32 {{
    if depth == 0 {{ 1 }} else {{
        let a = spawn worker(x, depth - 1)
        let b = spawn worker(x, depth - 1)
        let ra = await a
        let rb = await b
        ra + rb
    }}
}}

func main() -> i32 {{
    let t = spawn worker(0, {depth})
    let r = await t
    println(r)
    0
}}
"#
    )
}

fn atomic_parasteps_source(n: usize, per_task: i32) -> String {
    let mut body = String::new();
    for i in 0..n {
        body.push_str(&format!(
            "        let t{} = spawn worker(a, {})\n        let r{} = await t{}\n        if r{} != 0 {{ return {} }}\n",
            i, per_task, i, i, i, (i + 1) % 256
        ));
    }
    format!(
        r#"func worker(a: AtomicI32, n: i32) -> i32 {{
    for i in range(0, n) {{
        atomic_i32_fetch_add(a, 1)
    }}
    0
}}
func main() -> i32 {{
    let a = atomic_i32_new(0)
    parasteps {{
{}
    }}
    let v = atomic_i32_load(a)
    println(v)
    atomic_i32_drop(a)
    0
}}
"#,
        body,
    )
}

fn atomic_i64_cas_parasteps_source(n: usize, per_task: i64) -> String {
    let mut body = String::new();
    for i in 0..n {
        body.push_str(&format!(
            "        let t{} = spawn worker(a, {})\n        let r{} = await t{}\n        if r{} != 0 {{ return {} }}\n",
            i, per_task, i, i, i, (i + 1) % 256
        ));
    }
    format!(
        r#"func worker(a: AtomicI64, count: i64) -> i32 {{
    for i in range(0, count) {{
        let mut done = 0
        while done == 0 {{
            let cur = atomic_i64_load(a)
            let next = cur + 1
            done = atomic_i64_compare_exchange(a, cur, next)
        }}
    }}
    0
}}
func main() -> i32 {{
    let a = atomic_i64_new(0)
    parasteps {{
{}
    }}
    let v = atomic_i64_load(a)
    println(v)
    atomic_i64_drop(a)
    0
}}
"#,
        body,
    )
}

fn parasteps_channel_source(n: usize) -> String {
    let mut body = String::new();
    for i in 0..n {
        body.push_str(&format!(
            "        let t{} = spawn send_id(ch, {})\n        let r{} = await t{}\n        if r{} != 0 {{ return {} }}\n",
            i, i, i, i, i, (i + 1) % 256
        ));
    }
    format!(
        r#"func send_id(ch: Channel<i64>, x: i64) -> i32 {{
    channel_send(ch, x)
    0
}}
func main() -> i32 {{
    let ch = channel_new()
    parasteps {{
{}
    }}
    let mut sum: i64 = 0
    for i in range(0, {n}) {{
        sum = sum + channel_recv(ch)
    }}
    println(sum)
    channel_drop(ch)
    0
}}
"#,
        body,
    )
}

fn eager_spawn_run(source: &str) -> Result<String, String> {
    // Explicit escape hatch used when debugging or comparing semantics.
    run_program_with_env(source, &[("MIMI_EAGER_SPAWN", "1")])
}

#[test]
fn stress_real_spawn_direct_call_smoke() {
    let out = real_spawn_run(&spawn_sum_source(50)).expect("real spawn smoke failed");
    assert_eq!(out.trim(), "1225");
}

#[test]
#[ignore = "heavy: 500 real-thread spawn/await; run explicitly with --ignored"]
fn stress_real_spawn_direct_call_heavy() {
    let out = real_spawn_run(&spawn_sum_source(500)).expect("real spawn heavy failed");
    assert_eq!(out.trim(), "124750");
}

#[test]
fn stress_real_spawn_list_scalar_deep_copy_smoke() {
    let source = r#"func first(xs: List<i32>) -> i32 { xs[0] }
func main() -> i32 {
    let xs = [10, 20, 30]
    let t = spawn first(xs)
    let r = await t
    println(r)
    0
}
"#;
    // Scalar-element lists are deep-copied into the worker env and freed after
    // the callee reads them, so the caller may safely mutate/free its list.
    let out = real_spawn_run(source).expect("list-arg spawn deep-copy failed");
    assert_eq!(out.trim(), "10");
}

#[test]
fn stress_real_spawn_string_list_deep_copy_smoke() {
    let source = r#"func total_len(xs: List<string>) -> i64 {
    let mut sum = 0
    for s in xs {
        sum = sum + len(s)
    }
    sum
}
func main() -> i32 {
    let xs = ["hello", "world", "!"]
    let t = spawn total_len(xs)
    let r = await t
    println(r)
    0
}
"#;
    // List<string> worker args are deep-copied element-by-element, and the
    // worker frees both the cloned strings and the cloned data array.
    let out = real_spawn_run(source).expect("string-list spawn deep-copy failed");
    assert_eq!(out.trim(), "11");
}

#[test]
fn stress_real_spawn_string_list_matches_eager_semantics_smoke() {
    let source = string_list_spawn_source(20);
    let real = real_spawn_run(&source).expect("real string-list spawn semantic run failed");
    let eager = eager_spawn_run(&source).expect("eager string-list spawn semantic run failed");
    assert_eq!(real.trim(), "120");
    assert_eq!(eager.trim(), "120");
}

#[test]
#[ignore = "heavy: 200 List<string> real-thread spawns; run explicitly with --ignored"]
fn stress_real_spawn_string_list_heavy() {
    let out =
        real_spawn_run(&string_list_spawn_source(200)).expect("string-list spawn heavy failed");
    // 200 * (1 + 2 + 3) = 1200
    assert_eq!(out.trim(), "1200");
}

#[test]
fn stress_real_spawn_nested_list_deep_copy_smoke() {
    let source = r#"func sum_all(rows: List<List<i32>>) -> i32 {
    let mut total = 0
    for row in rows {
        for x in row {
            total = total + x
        }
    }
    total
}
func main() -> i32 {
    let rows = [[1, 2], [3, 4, 5], [6]]
    let t = spawn sum_all(rows)
    let r = await t
    println(r)
    0
}
"#;
    // Nested lists are deep-copied at both levels: outer data array, inner
    // boxes, and inner data buffers all belong to the worker env.
    let out = real_spawn_run(source).expect("nested-list spawn deep-copy failed");
    assert_eq!(out.trim(), "21");
}

#[test]
fn stress_real_spawn_nested_list_matches_eager_semantics_smoke() {
    let source = nested_list_spawn_source(20);
    let real = real_spawn_run(&source).expect("real nested-list spawn semantic run failed");
    let eager = eager_spawn_run(&source).expect("eager nested-list spawn semantic run failed");
    assert_eq!(real.trim(), "420");
    assert_eq!(eager.trim(), "420");
}

#[test]
#[ignore = "heavy: 100 List<List<i32>> real-thread spawns; run explicitly with --ignored"]
fn stress_real_spawn_nested_list_heavy() {
    let out =
        real_spawn_run(&nested_list_spawn_source(100)).expect("nested-list spawn heavy failed");
    // 100 * (1+2+3+4+5+6) = 2100
    assert_eq!(out.trim(), "2100");
}

#[test]
fn stress_real_spawn_nested_string_list_deep_copy_smoke() {
    let source = r#"func total_len(rows: List<List<string>>) -> i64 {
    let mut total = 0
    for row in rows {
        for s in row {
            total = total + len(s)
        }
    }
    total
}
func main() -> i32 {
    let rows = [["a", "bb"], ["ccc", "dddd", ""]]
    let t = spawn total_len(rows)
    let r = await t
    println(r)
    0
}
"#;
    // List<List<string>> needs three layers of ownership: outer data array,
    // inner list boxes, inner data arrays, and each individual string.
    let out = real_spawn_run(source).expect("nested string-list spawn deep-copy failed");
    assert_eq!(out.trim(), "10");
}

#[test]
fn stress_real_spawn_nested_string_list_matches_eager_semantics_smoke() {
    let source = nested_string_list_spawn_source(20);
    let real = real_spawn_run(&source).expect("real nested string-list spawn semantic run failed");
    let eager =
        eager_spawn_run(&source).expect("eager nested string-list spawn semantic run failed");
    assert_eq!(real.trim(), "200");
    assert_eq!(eager.trim(), "200");
}

#[test]
#[ignore = "heavy: 100 List<List<string>> real-thread spawns; run explicitly with --ignored"]
fn stress_real_spawn_nested_string_list_heavy() {
    let out = real_spawn_run(&nested_string_list_spawn_source(100))
        .expect("nested string-list spawn heavy failed");
    // 100 * (1+2+3+4+0) = 1000
    assert_eq!(out.trim(), "1000");
}

#[test]
fn stress_real_spawn_heap_struct_string_deep_copy_smoke() {
    let source = r#"type User { name: string, age: i32 }
func describe(u: User) -> i32 {
    len(u.name) + u.age
}
func main() -> i32 {
    let u = User { name: "hello", age: 5 }
    let t = spawn describe(u)
    let r = await t
    println(r)
    0
}
"#;
    // Structs with a string field need deep-copying of the string payload
    // into the worker env; scalar fields are copied by value.
    let out = real_spawn_run(source).expect("heap struct string-field spawn failed");
    assert_eq!(out.trim(), "10");
}

#[test]
fn stress_real_spawn_heap_struct_string_matches_eager_semantics_smoke() {
    let source = heap_struct_spawn_source(20);
    let real = real_spawn_run(&source).expect("real heap struct spawn semantic run failed");
    let eager = eager_spawn_run(&source).expect("eager heap struct spawn semantic run failed");
    assert_eq!(real.trim(), "200");
    assert_eq!(eager.trim(), "200");
}

#[test]
#[ignore = "heavy: 100 heap struct string-field real-thread spawns; run explicitly with --ignored"]
fn stress_real_spawn_heap_struct_string_heavy() {
    let out =
        real_spawn_run(&heap_struct_spawn_source(100)).expect("heap struct spawn heavy failed");
    // 100 * (5+5) = 1000
    assert_eq!(out.trim(), "1000");
}

#[test]
fn stress_real_spawn_heap_struct_list_deep_copy_smoke() {
    let source = r#"type Bag { items: List<i32>, label: string }
func total(b: Bag) -> i32 {
    let mut s = 0
    for x in b.items {
        s = s + x
    }
    s + len(b.label)
}
func main() -> i32 {
    let b = Bag { items: [1, 2, 3], label: "hi" }
    let t = spawn total(b)
    let r = await t
    println(r)
    0
}
"#;
    // Records may now carry both List and String fields; both heap payloads
    // are deep-copied into the worker env.
    let out = real_spawn_run(source).expect("heap struct list-field spawn failed");
    assert_eq!(out.trim(), "8");
}

#[test]
fn stress_real_spawn_heap_record_list_matches_eager_semantics_smoke() {
    let source = heap_record_list_spawn_source(20);
    let real = real_spawn_run(&source).expect("real heap record list spawn semantic run failed");
    let eager = eager_spawn_run(&source).expect("eager heap record list spawn semantic run failed");
    assert_eq!(real.trim(), "180");
    assert_eq!(eager.trim(), "180");
}

#[test]
#[ignore = "heavy: 100 heap struct list-field real-thread spawns; run explicitly with --ignored"]
fn stress_real_spawn_heap_record_list_heavy() {
    let out = real_spawn_run(&heap_record_list_spawn_source(100))
        .expect("heap record list spawn heavy failed");
    // 100 * (2+3+2+2) = 900
    assert_eq!(out.trim(), "900");
}

#[test]
fn stress_real_spawn_nested_scalar_struct_smoke() {
    let source = r#"type Point { x: i32, y: i32 }
type Line { a: Point, b: Point }
func sum_x(l: Line) -> i32 {
    l.a.x + l.b.x
}
func main() -> i32 {
    let l = Line { a: Point { x: 1, y: 2 }, b: Point { x: 3, y: 4 } }
    let t = spawn sum_x(l)
    let r = await t
    println(r)
    0
}
"#;
    // Nested records whose fields are transitively scalar are passed by
    // value; no heap-copy metadata is required.
    let out = real_spawn_run(source).expect("nested scalar struct spawn failed");
    assert_eq!(out.trim(), "4");
}

#[test]
fn stress_real_spawn_nested_scalar_struct_matches_eager_semantics_smoke() {
    let source = nested_scalar_struct_spawn_source(20);
    let real =
        real_spawn_run(&source).expect("real nested scalar struct spawn semantic run failed");
    let eager =
        eager_spawn_run(&source).expect("eager nested scalar struct spawn semantic run failed");
    assert_eq!(real.trim(), "80");
    assert_eq!(eager.trim(), "80");
}

#[test]
#[ignore = "heavy: 100 nested scalar struct real-thread spawns; run explicitly with --ignored"]
fn stress_real_spawn_nested_scalar_struct_heavy() {
    let out = real_spawn_run(&nested_scalar_struct_spawn_source(100))
        .expect("nested scalar struct spawn heavy failed");
    // 100 * (1+3) = 400
    assert_eq!(out.trim(), "400");
}

#[test]
fn stress_real_spawn_recursive_heap_struct_smoke() {
    let source = r#"type Inner { name: string, nums: List<i32> }
type Outer { inner: Inner, tag: i32 }
func score(o: Outer) -> i32 {
    len(o.inner.name) + len(o.inner.nums) + o.tag
}
func main() -> i32 {
    let o = Outer { inner: Inner { name: "hi", nums: [1, 2, 3] }, tag: 10 }
    let t = spawn score(o)
    let r = await t
    println(r)
    0
}
"#;
    // A record containing another record with both String and List fields
    // recurses through the nested struct layout for deep-copy/cleanup.
    let out = real_spawn_run(source).expect("recursive heap struct spawn failed");
    assert_eq!(out.trim(), "15");
}

#[test]
fn stress_real_spawn_recursive_heap_struct_matches_eager_semantics_smoke() {
    let source = recursive_heap_struct_spawn_source(20);
    let real =
        real_spawn_run(&source).expect("real recursive heap struct spawn semantic run failed");
    let eager =
        eager_spawn_run(&source).expect("eager recursive heap struct spawn semantic run failed");
    assert_eq!(real.trim(), "300");
    assert_eq!(eager.trim(), "300");
}

#[test]
#[ignore = "heavy: 100 recursive heap struct real-thread spawns; run explicitly with --ignored"]
fn stress_real_spawn_recursive_heap_struct_heavy() {
    let out = real_spawn_run(&recursive_heap_struct_spawn_source(100))
        .expect("recursive heap struct spawn heavy failed");
    // 100 * (2+3+10) = 1500
    assert_eq!(out.trim(), "1500");
}

#[test]
fn stress_real_spawn_scalar_struct_list_smoke() {
    let source = r#"type Point { x: i32, y: i32 }
func sum_points(points: List<Point>) -> i32 {
    let mut s = 0
    for p in points {
        s = s + p.x + p.y
    }
    s
}
func main() -> i32 {
    let pts = [Point { x: 1, y: 2 }, Point { x: 3, y: 4 }]
    let t = spawn sum_points(pts)
    let r = await t
    println(r)
    0
}
"#;
    // Lists of scalar records are boxed per element; the worker env gets a
    // fresh data array and fresh boxes so the caller may safely reuse/free
    // the original list.
    let out = real_spawn_run(source).expect("scalar struct list spawn failed");
    assert_eq!(out.trim(), "10");
}

#[test]
fn stress_real_spawn_scalar_struct_list_matches_eager_semantics_smoke() {
    let source = scalar_struct_list_spawn_source(20);
    let real = real_spawn_run(&source).expect("real scalar struct list spawn semantic run failed");
    let eager =
        eager_spawn_run(&source).expect("eager scalar struct list spawn semantic run failed");
    assert_eq!(real.trim(), "200");
    assert_eq!(eager.trim(), "200");
}

#[test]
#[ignore = "heavy: 100 scalar struct list real-thread spawns; run explicitly with --ignored"]
fn stress_real_spawn_scalar_struct_list_heavy() {
    let out = real_spawn_run(&scalar_struct_list_spawn_source(100))
        .expect("scalar struct list spawn heavy failed");
    // 100 * (1+2+3+4) = 1000
    assert_eq!(out.trim(), "1000");
}

#[test]
fn stress_real_spawn_heap_record_list_smoke() {
    let source = r#"type Inner { name: string, nums: List<i32> }
func total(items: List<Inner>) -> i32 {
    let mut s = 0
    for it in items {
        s = s + len(it.name) + len(it.nums)
    }
    s
}
func main() -> i32 {
    let items = [Inner { name: "hi", nums: [1, 2, 3] }, Inner { name: "ab", nums: [4, 5] }]
    let t = spawn total(items)
    let r = await t
    println(r)
    0
}
"#;
    // List elements that are records containing String and List fields must
    // deep-copy those leaves inside each element box, not just the box.
    let out = real_spawn_run(source).expect("heap struct list spawn failed");
    assert_eq!(out.trim(), "9");
}

#[test]
fn stress_real_spawn_heap_struct_list_matches_eager_semantics_smoke() {
    let source = heap_struct_list_spawn_source(20);
    let real =
        real_spawn_run(&source).expect("real heap struct list-field spawn semantic run failed");
    let eager =
        eager_spawn_run(&source).expect("eager heap struct list-field spawn semantic run failed");
    assert_eq!(real.trim(), "160");
    assert_eq!(eager.trim(), "160");
}

#[test]
#[ignore = "heavy: 100 heap struct list real-thread spawns; run explicitly with --ignored"]
fn stress_real_spawn_heap_struct_list_heavy() {
    let out = real_spawn_run(&heap_struct_list_spawn_source(100))
        .expect("heap struct list-field spawn heavy failed");
    // 100 * (1+2+3+2) = 800
    assert_eq!(out.trim(), "800");
}

#[test]
fn stress_real_spawn_scalar_tuple_arg_smoke() {
    let source = r#"func first(p: (i32, i32)) -> i32 { p.0 }
func main() -> i32 {
    let t = spawn first((11, 22))
    let r = await t
    println(r)
    0
}
"#;
    // Pure scalar tuples can be copied by value safely into the worker env.
    let out = real_spawn_run(source).expect("scalar-tuple spawn failed");
    assert_eq!(out.trim(), "11");
}

#[test]
fn stress_real_spawn_list_return_smoke() {
    let source = r#"func make() -> List<i32> { [1, 2, 3] }
func main() -> i32 {
    let t = spawn make()
    let xs = await t
    println(xs[0] + xs[2])
    0
}
"#;
    // Real-thread results that are heap-backed (List) must survive the worker
    // returning and be readable by await in the caller.
    let out = real_spawn_run(source).expect("list-return spawn failed");
    assert_eq!(out.trim(), "4");
}

#[test]
fn stress_real_spawn_channel_workers_smoke() {
    let out = real_spawn_run(&channel_spawn_source(50)).expect("channel+spawn worker smoke failed");
    // sum 0..49 = 1225 even though receive order is nondeterministic
    assert_eq!(out.trim(), "1225");
}

#[test]
fn stress_native_parasteps_channel_smoke() {
    let source = parasteps_channel_source(8);
    let out = build_and_run_native(&source).expect("native parasteps+Channel smoke failed");
    assert_eq!(out.trim(), "28");
}

#[test]
#[ignore = "heavy: 64 native parasteps channel senders; run explicitly with --ignored"]
fn stress_native_parasteps_channel_heavy() {
    let source = parasteps_channel_source(64);
    let out = build_and_run_native(&source).expect("native parasteps+Channel heavy failed");
    assert_eq!(out.trim(), "2016");
}

#[test]
fn stress_native_nested_spawn_chain_smoke() {
    let source = nested_spawn_chain_source(16);
    let out = build_and_run_native(&source).expect("native nested spawn chain smoke failed");
    assert_eq!(out.trim(), "16");
}

#[test]
#[ignore = "heavy: 128-deep native nested spawn chain; run explicitly with --ignored"]
fn stress_native_nested_spawn_chain_heavy() {
    let source = nested_spawn_chain_source(128);
    let out = build_and_run_native(&source).expect("native nested spawn chain heavy failed");
    assert_eq!(out.trim(), "128");
}

#[test]
fn stress_native_nested_spawn_fanout_smoke() {
    let source = nested_spawn_fanout_source(6);
    let out = build_and_run_native(&source).expect("native nested spawn fanout smoke failed");
    // depth 6 => exactly 2^6 leaf tasks finish and sum to 64
    assert_eq!(out.trim(), "64");
}

#[test]
#[ignore = "heavy: 2^10 native nested spawn fanout; run explicitly with --ignored"]
fn stress_native_nested_spawn_fanout_heavy() {
    let source = nested_spawn_fanout_source(10);
    let out = build_and_run_native(&source).expect("native nested spawn fanout heavy failed");
    // depth 10 => 2^10 leaves sum to 1024
    assert_eq!(out.trim(), "1024");
}

#[test]
fn stress_native_atomic_fetch_add_smoke() {
    let source = atomic_parasteps_source(8, 100);
    let out = build_and_run_native(&source).expect("native atomic fetch-add smoke failed");
    assert_eq!(out.trim(), "800");
}

#[test]
#[ignore = "heavy: 32 native atomic workers * 1000 increments; run explicitly with --ignored"]
fn stress_native_atomic_fetch_add_heavy() {
    let source = atomic_parasteps_source(32, 1000);
    let out = build_and_run_native(&source).expect("native atomic fetch-add heavy failed");
    assert_eq!(out.trim(), "32000");
}

#[test]
fn stress_native_atomic_i64_cas_smoke() {
    let source = atomic_i64_cas_parasteps_source(8, 50);
    let out = build_and_run_native(&source).expect("native atomic i64 CAS smoke failed");
    assert_eq!(out.trim(), "400");
}

#[test]
#[ignore = "heavy: 32 native atomic CAS workers * 500 increments; run explicitly with --ignored"]
fn stress_native_atomic_i64_cas_heavy() {
    let source = atomic_i64_cas_parasteps_source(32, 500);
    let out = build_and_run_native(&source).expect("native atomic i64 CAS heavy failed");
    assert_eq!(out.trim(), "16000");
}

#[test]
fn stress_native_atomic_bool_lock_smoke() {
    let source = atomic_bool_lock_parasteps_source(8, 50);
    let out = build_and_run_native(&source).expect("native atomic bool lock smoke failed");
    assert_eq!(out.trim(), "400");
}

#[test]
#[ignore = "heavy: 32 native AtomicBool lock workers * 500 increments; run explicitly with --ignored"]
fn stress_native_atomic_bool_lock_heavy() {
    let source = atomic_bool_lock_parasteps_source(32, 500);
    let out = build_and_run_native(&source).expect("native atomic bool lock heavy failed");
    assert_eq!(out.trim(), "16000");
}

#[test]
fn stress_native_mutex_protected_smoke() {
    let source = mutex_parasteps_source(8, 50);
    let out = build_and_run_native(&source).expect("native mutex protected smoke failed");
    assert_eq!(out.trim(), "400");
}

#[test]
#[ignore = "heavy: 32 native mutex workers * 500 increments; run explicitly with --ignored"]
fn stress_native_mutex_protected_heavy() {
    let source = mutex_parasteps_source(32, 500);
    let out = build_and_run_native(&source).expect("native mutex protected heavy failed");
    assert_eq!(out.trim(), "16000");
}

#[test]
#[ignore = "heavy: 500 real-thread channel workers; run explicitly with --ignored"]
fn stress_real_spawn_channel_workers_heavy() {
    let out =
        real_spawn_run(&channel_spawn_source(500)).expect("channel+spawn worker heavy failed");
    // sum 0..499 = 124750 even though receive order is nondeterministic
    assert_eq!(out.trim(), "124750");
}

#[test]
#[ignore = "heavy: 10000 native channel workers; run explicitly with --ignored"]
fn stress_native_channel_workers_tenk() {
    let source = channel_spawn_source(10000);
    let out = build_and_run_native(&source).expect("native channel workers tenk failed");
    // sum 0..9999 = 49995000 even though receive order is nondeterministic
    assert_eq!(out.trim(), "49995000");
}

#[test]
#[ignore = "heavy: 10000 native spawn/await loop; run explicitly with --ignored"]
fn stress_native_spawn_await_tenk() {
    let source = spawn_await_loop_source(10000);
    let out = build_and_run_native(&source).expect("native spawn/await tenk failed");
    // sum 0..9999 = 49995000
    assert_eq!(out.trim(), "49995000");
}

#[test]
fn stress_eager_spawn_escape_hatch_smoke() {
    let out = eager_spawn_run(&spawn_sum_source(50)).expect("eager spawn smoke failed");
    assert_eq!(out.trim(), "1225");
}

#[test]
fn stress_real_spawn_matches_eager_semantics_smoke() {
    let source = spawn_sum_source(100);
    let real = real_spawn_run(&source).expect("real spawn semantic run failed");
    let eager = eager_spawn_run(&source).expect("eager spawn semantic run failed");
    assert_eq!(real.trim(), eager.trim());
    assert_eq!(real.trim(), "4950");
}

#[test]
fn stress_real_spawn_string_arg_deep_copy_smoke() {
    let source = r#"func greet(name: string) -> string { "Hello, " + name }
func main() -> i32 {
    let t = spawn greet("world")
    let s = await t
    println(s)
    0
}
"#;
    // String args are deep-copied into the worker env (mimi_str_clone) and
    // freed after the callee reads them; this must not use-after-free the
    // caller's original string literal/ownership.
    let out = real_spawn_run(source).expect("string-arg spawn deep-copy failed");
    assert_eq!(out.trim(), "Hello, world");
}
