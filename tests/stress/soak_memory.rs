// Soak memory smoke tests.
//
// This initial smoke uses `/usr/bin/time -v` to sample the max RSS of a VM
// allocation loop. The 0.1.7 follow-up will extend it to long-running compiled
// native binaries and RSS growth curve sampling.

use super::{
    alloc_loop_source, build_and_run_native_with_max_rss_kb, flow_chain_source,
    run_mimi_with_max_rss_kb, run_program,
};

#[test]
fn stress_soak_repeated_flow_chain_smoke() {
    let source = flow_chain_source(50);
    for _ in 0..5 {
        let out = run_program(&source).expect("soak flow chain iteration failed");
        assert_eq!(out.trim(), "50");
    }
}

#[test]
fn stress_soak_loop_break_continue_drop_smoke() {
    let break_source = r#"func main() -> i32 {
    for i in range(0, 100) {
        let xs = [i, i + 1]
        if i == 3 {
            break
        }
    }
    println(0)
    0
}
"#;
    let out = run_program(break_source).expect("loop break drop smoke failed");
    assert_eq!(out.trim(), "0");

    let continue_source = r#"func main() -> i32 {
    let mut acc = 0
    for i in range(0, 10) {
        let xs = [i, i + 1]
        if i % 2 == 0 {
            continue
        }
        acc += len(xs)
    }
    println(acc)
    0
}
"#;
    let out = run_program(continue_source).expect("loop continue drop smoke failed");
    assert_eq!(out.trim(), "10");
}

#[test]
fn stress_soak_early_return_loop_drop_smoke() {
    // Early return from inside a loop must run the same path-specific heap
    // flush as break/continue. If the loop-local List leaks on this path, the
    // native run below accumulates hundreds of MiB.
    let elems = vec!["0"; 1000].join(", ");
    let source = format!(
        r#"func find() -> i32 {{
    for i in range(0, 100) {{
        let xs = [{elems}]
        if i == 50 {{
            return len(xs)
        }}
    }}
    0
}}
func main() -> i32 {{
    for j in range(0, 80000) {{
        find()
    }}
    println(0)
    0
}}
"#
    );
    let Some((out, max_rss_kb)) = build_and_run_native_with_max_rss_kb(&source) else {
        eprintln!("SKIP: /usr/bin/time or native build not available");
        return;
    };
    assert_eq!(out.trim(), "0");
    assert!(
        max_rss_kb < 512 * 1024,
        "native early-return loop max RSS exceeded 512 MiB: {max_rss_kb} KiB"
    );
}

#[test]
fn stress_soak_resolved_list_return_ownership_smoke() {
    // Resolved calls returning List values must register the returned data
    // buffer with the caller's heap scope. Without caller-side tracking each
    // loop iteration leaked one 1000-element List (8 KiB), reaching ~800 MiB
    // at 100k iterations; with tracking native RSS stays small.
    let elems = vec!["0"; 1000].join(", ");
    let source = format!(
        r#"func pick() -> List<i32> {{
    [{elems}]
}}
func main() -> i32 {{
    for j in range(0, 100000) {{
        let xs = pick()
        if xs[0] + xs[999] != 0 {{ return 1 }}
    }}
    println(0)
    0
}}
"#
    );
    let Some((out, max_rss_kb)) = build_and_run_native_with_max_rss_kb(&source) else {
        eprintln!("SKIP: /usr/bin/time or native build not available");
        return;
    };
    assert_eq!(out.trim(), "0");
    assert!(
        max_rss_kb < 512 * 1024,
        "returned List ownership max RSS exceeded 512 MiB: {max_rss_kb} KiB"
    );
}

#[test]
fn stress_soak_resolved_string_return_ownership_smoke() {
    // Resolved calls returning String values must also hand the heap data to
    // the caller. Without caller-side tracking, a 1000-byte returned string
    // accumulated ~200 MiB RSS at 200k iterations.
    let big = "a".repeat(1000);
    let source = format!(
        r#"func pick() -> string {{
    "{big}"
}}
func main() -> i32 {{
    for j in range(0, 200000) {{
        let s = pick()
        if len(s) != 1000 {{ return 1 }}
    }}
    println(0)
    0
}}
"#
    );
    let Some((out, max_rss_kb)) = build_and_run_native_with_max_rss_kb(&source) else {
        eprintln!("SKIP: /usr/bin/time or native build not available");
        return;
    };
    assert_eq!(out.trim(), "0");
    assert!(
        max_rss_kb < 128 * 1024,
        "returned String ownership max RSS exceeded 128 MiB: {max_rss_kb} KiB"
    );
}

#[test]
fn stress_soak_resolved_heap_record_return_ownership_smoke() {
    // A returned Record containing both String and List fields must have its
    // nested String leaves made heap-owned by the callee and all heap leaves
    // tracked by the caller. This guards against freeing a .rodata literal
    // and against accumulating ~800 MiB across 100k iterations.
    let elems = vec!["0"; 1000].join(", ");
    let big = "a".repeat(807);
    let source = format!(
        r#"type Inner {{ name: string, nums: List<i32> }}
func make() -> Inner {{
    Inner {{ name: "{big}", nums: [{elems}] }}
}}
func main() -> i32 {{
    for j in range(0, 100000) {{
        let r = make()
        if len(r.name) != 807 || len(r.nums) != 1000 {{ return 1 }}
    }}
    println(0)
    0
}}
"#
    );
    let Some((out, max_rss_kb)) = build_and_run_native_with_max_rss_kb(&source) else {
        eprintln!("SKIP: /usr/bin/time or native build not available");
        return;
    };
    assert_eq!(out.trim(), "0");
    assert!(
        max_rss_kb < 512 * 1024,
        "returned heap Record max RSS exceeded 512 MiB: {max_rss_kb} KiB"
    );
}

#[test]
fn stress_soak_early_return_heap_record_ownership_smoke() {
    // Same ownership chain for an early-returned heap Record: callee must
    // claim the nested String leaf (copying .rodata if needed), then hand all
    // heap pointers to the caller without freeing them before ret.
    let elems = vec!["0"; 1000].join(", ");
    let big = "a".repeat(807);
    let source = format!(
        r#"type Inner {{ name: string, nums: List<i32> }}
func make() -> Inner {{
    for i in range(0, 2) {{
        let r = Inner {{ name: "{big}", nums: [{elems}] }}
        if i == 1 {{ return r }}
    }}
    Inner {{ name: "x", nums: [0] }}
}}
func main() -> i32 {{
    for j in range(0, 100000) {{
        let r = make()
        if len(r.name) != 807 || len(r.nums) != 1000 {{ return 1 }}
    }}
    println(0)
    0
}}
"#
    );
    let Some((out, max_rss_kb)) = build_and_run_native_with_max_rss_kb(&source) else {
        eprintln!("SKIP: /usr/bin/time or native build not available");
        return;
    };
    assert_eq!(out.trim(), "0");
    assert!(
        max_rss_kb < 512 * 1024,
        "early-returned heap Record max RSS exceeded 512 MiB: {max_rss_kb} KiB"
    );
}

#[test]
fn stress_soak_resolved_heap_record_string_list_return_ownership_smoke() {
    // A returned Record with a `List<string>` field owns both the list data
    // array and every string data pointer. Caller-side cleanup must free the
    // strings before the array; otherwise 100k iterations accumulate ~200 MiB.
    let half = "a".repeat(500);
    let source = format!(
        r#"type Bag {{ items: List<string> }}
func make() -> Bag {{
    let a = "{half}" + "{half}"
    let b = "{half}" + "{half}"
    Bag {{ items: [a, b] }}
}}
func main() -> i32 {{
    for j in range(0, 100000) {{
        let r = make()
        if len(r.items) != 2 {{ return 1 }}
    }}
    println(0)
    0
}}
"#
    );
    let Some((out, max_rss_kb)) = build_and_run_native_with_max_rss_kb(&source) else {
        eprintln!("SKIP: /usr/bin/time or native build not available");
        return;
    };
    assert_eq!(out.trim(), "0");
    assert!(
        max_rss_kb < 128 * 1024,
        "returned Record<List<string>> max RSS exceeded 128 MiB: {max_rss_kb} KiB"
    );
}

#[test]
fn stress_soak_resolved_nested_heap_record_string_list_return_ownership_smoke() {
    // Nested user Records must also propagate the resolved type context so a
    // `List<string>` living one level below the top record is registered as
    // element-freeable by the caller.
    let half = "a".repeat(500);
    let source = format!(
        r#"type Inner {{ items: List<string> }}
type Outer {{ inner: Inner }}
func make() -> Outer {{
    let a = "{half}" + "{half}"
    let b = "{half}" + "{half}"
    Outer {{ inner: Inner {{ items: [a, b] }} }}
}}
func main() -> i32 {{
    for j in range(0, 100000) {{
        let r = make()
        if len(r.inner.items) != 2 {{ return 1 }}
    }}
    println(0)
    0
}}
"#
    );
    let Some((out, max_rss_kb)) = build_and_run_native_with_max_rss_kb(&source) else {
        eprintln!("SKIP: /usr/bin/time or native build not available");
        return;
    };
    assert_eq!(out.trim(), "0");
    assert!(
        max_rss_kb < 128 * 1024,
        "nested Record<List<string>> return max RSS exceeded 128 MiB: {max_rss_kb} KiB"
    );
}

#[test]
fn stress_soak_early_return_nested_heap_record_string_list_ownership_smoke() {
    let half = "a".repeat(500);
    let source = format!(
        r#"type Inner {{ items: List<string> }}
type Outer {{ inner: Inner }}
func make() -> Outer {{
    for i in range(0, 2) {{
        let a = "{half}" + "{half}"
        let b = "{half}" + "{half}"
        let r = Outer {{ inner: Inner {{ items: [a, b] }} }}
        if i == 1 {{ return r }}
    }}
    Outer {{ inner: Inner {{ items: ["x"] }} }}
}}
func main() -> i32 {{
    for j in range(0, 100000) {{
        let r = make()
        if len(r.inner.items) != 2 {{ return 1 }}
    }}
    println(0)
    0
}}
"#
    );
    let Some((out, max_rss_kb)) = build_and_run_native_with_max_rss_kb(&source) else {
        eprintln!("SKIP: /usr/bin/time or native build not available");
        return;
    };
    assert_eq!(out.trim(), "0");
    assert!(
        max_rss_kb < 128 * 1024,
        "early-returned nested Record<List<string>> max RSS exceeded 128 MiB: {max_rss_kb} KiB"
    );
}

#[test]
fn stress_soak_resolved_string_list_literal_return_ownership_smoke() {
    // A returned `List<string>` containing only literals must heap-copy the
    // `.rodata` string pointers before the caller frees each element.
    let source = r#"func make() -> List<string> {
    ["a", "b", "c", "d"]
}
func main() -> i32 {
    for j in range(0, 100000) {
        let r = make()
        if r[0] != "a" || r[3] != "d" { return 1 }
    }
    println(0)
    0
}
"#;
    let Some((out, max_rss_kb)) = build_and_run_native_with_max_rss_kb(source) else {
        eprintln!("SKIP: /usr/bin/time or native build not available");
        return;
    };
    assert_eq!(out.trim(), "0");
    assert!(
        max_rss_kb < 128 * 1024,
        "literal List<string> return max RSS exceeded 128 MiB: {max_rss_kb} KiB"
    );
}

#[test]
fn stress_soak_resolved_list_of_string_list_return_ownership_smoke() {
    // A returned `List<List<string>>` owns outer data, inner list boxes, inner
    // data arrays, and every string element. Without recursive cleanup this
    // leaked >400 MiB in 100k iterations.
    let half = "a".repeat(500);
    let source = format!(
        r#"func make() -> List<List<string>> {{
    let a = "{half}" + "{half}"
    let b = "{half}" + "{half}"
    let c = "{half}" + "{half}"
    let d = "{half}" + "{half}"
    [[a, b], [c, d]]
}}
func main() -> i32 {{
    for j in range(0, 100000) {{
        let r = make()
        if len(r[1]) != 2 {{ return 1 }}
        if r[1][0] != "{half}" + "{half}" {{ return 2 }}
    }}
    println(0)
    0
}}
"#
    );
    let Some((out, max_rss_kb)) = build_and_run_native_with_max_rss_kb(&source) else {
        eprintln!("SKIP: /usr/bin/time or native build not available");
        return;
    };
    assert_eq!(out.trim(), "0");
    assert!(
        max_rss_kb < 128 * 1024,
        "returned List<List<string>> max RSS exceeded 128 MiB: {max_rss_kb} KiB"
    );
}

#[test]
fn stress_soak_early_return_list_of_string_list_ownership_smoke() {
    // Early-returning `List<List<string>>` must claim inner boxes, inner data
    // arrays, and string elements so the callee flush does not free them.
    let source = r#"func make() -> List<List<string>> {
    let xs = [["a", "b"], ["c", "d"]]
    if xs[0][0] == "a" { return xs }
    [["x"]]
}
func main() -> i32 {
    for j in range(0, 100000) {
        let r = make()
        if len(r[1]) != 2 { return 1 }
        if r[1][0] != "c" { return 2 }
        if r[1][1] != "d" { return 3 }
    }
    println(0)
    0
}
"#;
    let Some((out, max_rss_kb)) = build_and_run_native_with_max_rss_kb(source) else {
        eprintln!("SKIP: /usr/bin/time or native build not available");
        return;
    };
    assert_eq!(out.trim(), "0");
    assert!(
        max_rss_kb < 128 * 1024,
        "early-returned List<List<string>> max RSS exceeded 128 MiB: {max_rss_kb} KiB"
    );
}

#[test]
fn stress_soak_early_return_heap_record_string_list_ownership_smoke() {
    // Early-returning a Record<List<string>> must claim both the list data
    // pointer and every string element pointer, so the callee's flush does
    // not free strings that the caller now owns.
    let half = "a".repeat(500);
    let source = format!(
        r#"type Bag {{ items: List<string> }}
func make() -> Bag {{
    for i in range(0, 2) {{
        let a = "{half}" + "{half}"
        let b = "{half}" + "{half}"
        let r = Bag {{ items: [a, b] }}
        if i == 1 {{ return r }}
    }}
    Bag {{ items: ["x"] }}
}}
func main() -> i32 {{
    for j in range(0, 100000) {{
        let r = make()
        if len(r.items) != 2 {{ return 1 }}
    }}
    println(0)
    0
}}
"#
    );
    let Some((out, max_rss_kb)) = build_and_run_native_with_max_rss_kb(&source) else {
        eprintln!("SKIP: /usr/bin/time or native build not available");
        return;
    };
    assert_eq!(out.trim(), "0");
    assert!(
        max_rss_kb < 128 * 1024,
        "early-returned Record<List<string>> max RSS exceeded 128 MiB: {max_rss_kb} KiB"
    );
}

#[test]
fn stress_soak_early_return_list_ownership_smoke() {
    // Early return of a loop-local List must transfer ownership to the caller
    // (the callee must not free it before ret) and the caller must later free
    // it. The combined failure mode (use-after-free or 800 MiB accumulation)
    // is caught by native RSS plus the deterministic output check.
    let elems = vec!["0"; 1000].join(", ");
    let source = format!(
        r#"func pick() -> List<i32> {{
    for i in range(0, 2) {{
        let xs = [{elems}]
        if i == 1 {{ return xs }}
    }}
    [0]
}}
func main() -> i32 {{
    for j in range(0, 100000) {{
        let xs = pick()
        if xs[0] + xs[999] != 0 {{ return 1 }}
    }}
    println(0)
    0
}}
"#
    );
    let Some((out, max_rss_kb)) = build_and_run_native_with_max_rss_kb(&source) else {
        eprintln!("SKIP: /usr/bin/time or native build not available");
        return;
    };
    assert_eq!(out.trim(), "0");
    assert!(
        max_rss_kb < 512 * 1024,
        "early-returned List ownership max RSS exceeded 512 MiB: {max_rss_kb} KiB"
    );
}

#[test]
fn stress_soak_native_list_string_loop_memory() {
    // A list of string literals created and dropped inside a loop. The native
    // binary must not accumulate unbounded heap: 200k iterations should stay
    // within a few tens of MiB, far below the 512 MiB ceiling.
    let source = r#"func main() -> i32 {
    for i in range(0, 200000) {
        let xs = ["a", "b", "c"]
    }
    println(0)
    0
}
"#;
    let Some((out, max_rss_kb)) = build_and_run_native_with_max_rss_kb(source) else {
        eprintln!("SKIP: /usr/bin/time or native build not available");
        return;
    };
    assert_eq!(out.trim(), "0");
    assert!(
        max_rss_kb < 512 * 1024,
        "native list/string loop max RSS exceeded 512 MiB: {max_rss_kb} KiB"
    );
}

#[test]
fn stress_soak_memory_alloc_loop_smoke() {
    // 10 万次临时 List 分配/释放循环；输出必须确定，RSS 只做灾难性上限检查。
    let source = alloc_loop_source(100_000);
    let Some((out, max_rss_kb)) = run_mimi_with_max_rss_kb(&source, &["run"]) else {
        eprintln!("SKIP: /usr/bin/time -v not available");
        return;
    };
    assert_eq!(out.trim(), "300000");
    // 1 GiB 宽松上限：当前 VM 基线约 100 MB，超过此值说明存在灾难性失控。
    assert!(
        max_rss_kb < 1024 * 1024,
        "allocation loop max RSS exceeded 1 GiB: {max_rss_kb} KiB"
    );
}

#[test]
#[ignore = "heavy soak: native memory-stability soak (default 5s; set MIMI_SOAK_SECONDS for nightly 24h)"]
fn stress_soak_native_memory_stability_heavy() {
    use std::time::Duration;

    // Continuous allocation loop in a compiled native binary. Each iteration
    // allocates a temporary list; a memory leak in any of the resolved/native
    // ownership paths causes VmRSS to grow without bound.
    let source = r#"func main() -> i32 {
    let mut i = 0
    let mut acc = 0
    while true {
        i = i + 1
        let xs = [i, i + 1, i + 2]
        acc += len(xs)
        if i % 10000 == 0 {
            println(acc)
        }
    }
    0
}"#;

    let Some((dir, exe_path)) = super::build_native_only(source) else {
        eprintln!("SKIP: native build not available");
        return;
    };

    let duration_secs: u64 = std::env::var("MIMI_SOAK_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let duration = Duration::from_secs(duration_secs);

    let mut child = match std::process::Command::new(&exe_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            panic!("failed to spawn soak binary: {e}");
        }
    };

    // Let the first 500ms serve as the baseline after startup.
    std::thread::sleep(Duration::from_millis(500));
    let mut rss_samples: Vec<u64> = Vec::new();
    let start = std::time::Instant::now();
    while start.elapsed() < duration {
        if let Some(rss) = read_vmrss_kb(child.id()) {
            rss_samples.push(rss);
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    // Stop the infinite loop.
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);

    let baseline = *rss_samples.first().unwrap_or(&0);
    let peak = *rss_samples.iter().max().unwrap_or(&baseline);
    let growth_kb = peak.saturating_sub(baseline);
    // 5s should stay far below 128 MiB; 24h nightly allows a comfortable but
    // still bounded 512 MiB envelope.
    let allowed_kb = if duration_secs >= 3600 {
        512 * 1024
    } else {
        128 * 1024
    };
    assert!(
        growth_kb < allowed_kb,
        "soak RSS grew {growth_kb} KiB (baseline {baseline} KiB, peak {peak} KiB) after {duration_secs}s; memory stability violated"
    );
    println!(
        "soak_ok duration_secs={duration_secs} samples={} baseline_kb={baseline} peak_kb={peak} growth_kb={growth_kb}",
        rss_samples.len()
    );
}

fn read_vmrss_kb(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("VmRSS:")?
                .trim()
                .strip_suffix(" kB")
        })
        .and_then(|v| v.trim().parse().ok())
}
