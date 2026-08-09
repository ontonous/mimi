//! Deep-eval 2026-08-09 regression locks — demos dual-backend differential
//! findings (B2a/B2b resolved string return probe + heap slot null-init,
//! B5 nested else-if string, B6 generic string param monomorphization,
//! B7 builtin Result/Option scrutinee decode, E0200 Result layout split,
//! 09 Result display incl. `Ok(())` unit payload, test_result_match Err
//! inttoptr+load decode). All cases are pure-logic (no filesystem) so the
//! assertions are deterministic across environments.
use super::*;

fn can_link() -> bool {
    crate::tests::can_link()
}

/// Dual-backend assertion mirroring src/tests/audit_fix_list_string.rs style:
/// checker gate, then VM stdout and codegen stdout must BOTH equal expected.
/// Codegen runs through `checked` (resolved-dispatch) codegen — the same
/// path the `mimi build` CLI uses — so the lock pins user-visible behavior.
fn assert_dual(src: &str, expected: &str) {
    if !can_link() {
        return;
    }
    check_source(src).unwrap_or_else(|diags| {
        panic!(
            "checker rejected dual source:\n{}",
            diags
                .iter()
                .map(|d| format!("{}", d))
                .collect::<Vec<_>>()
                .join("\n")
        )
    });
    let interp_run = std::panic::catch_unwind(|| run_source_with_stdout(src));
    assert!(interp_run.is_ok(), "interpreter panicked");
    let (_interp_val, interp_stdout) = interp_run.unwrap();
    let codegen_stdout = checked_codegen_compile_and_run(src).expect("codegen failed");
    assert_eq!(
        codegen_stdout.trim(),
        expected,
        "codegen mismatch\ncodegen: {}\nexpected: {}",
        codegen_stdout.trim(),
        expected
    );
    assert_eq!(
        interp_stdout.trim(),
        expected,
        "interpreter stdout mismatch\ninterp: {}\nexpected: {}",
        interp_stdout.trim(),
        expected
    );
}

// ── E0200: Result<…> layout split between Ok(list) and Err(string) ────────
// std/fs read_lines family: `Ok(str_split(…))` builds {i1,ptr,i64} (list
// handle) while `Err("…")` built {i1,i64,i64} — the phi unification rejected
// the match (match arm values have incompatible types). The legacy Err
// constructor now pads the Ok slot with the Ok value-shape zero
// (pending_result_ok_ty), unifying both arms.

#[test]
fn deep_eval_result_enum_layout_split_dual() {
    assert_dual(
        r#"
func f() -> Result<List<string>, string> {
    let r: Result<string, string> = Ok("a,b")
    match r {
        Ok(content) => Ok(str_split(content, ","))
        Err(e) => Err("err: " + e)
    }
}
func main() -> i32 {
    match f() {
        Ok(xs) => println("ok", len(xs))
        Err(e) => println("err", e)
    }
    0
}
"#,
        "ok 2",
    );
}

// ── B7: builtin Option call directly as match scrutinee ───────────────────
// `match str_index_of(…)` — the callee is not in func_defs, so the scrutinee
// type probe used to miss and the Some(i) binding stayed a raw i64 handle.

#[test]
fn deep_eval_builtin_option_scrutinee_dual() {
    assert_dual(
        r#"
func main() -> i32 {
    match str_index_of("hello world", "world") {
        Some(i) => println("found", i)
        None => println("none")
    }
    0
}
"#,
        "found 6",
    );
}

// ── 09: Result display — Ok(string) and Ok(()) unit payload ───────────────
// The unit payload lowers to i64 zero; display must show `()` not `0`.

#[test]
fn deep_eval_result_display_ok_and_unit_dual() {
    assert_dual(
        r#"
func main() -> i32 {
    let a: Result<string, string> = Ok("hi")
    println(a)
    let b: Result<(), string> = Ok(())
    println(b)
    0
}
"#,
        "Ok(hi)\nOk(())",
    );
}

// ── test_result_match: Ident scrutinee + Err(inttoptr+load) decode ────────
// `let r = Err("boom")` then `match r` — the Err payload is a heap {ptr,i64}
// handle; decode must inttoptr+load the string struct, not read the handle as
// a data pointer (that segfaulted in mimi_str_concat via strlen of garbage).

#[test]
fn deep_eval_result_ident_scrutinee_err_decode_dual() {
    assert_dual(
        r#"
func handle() -> string {
    let result: Result<string, string> = Err("boom")
    match result {
        Ok(content) => "ok: " + content,
        Err(msg) => "err: " + msg,
    }
}
func main() -> i32 {
    println(handle())
    0
}
"#,
        "err: boom",
    );
}

// ── B2a/B2b: resolved string return probe + heap slot null-init ───────────
// 04_adt_match describe_point family: a resolved enum match returning string
// in a conditional branch; the heap-alloc registration in an untaken branch
// must free null, not garbage (register_heap_alloc null-init).

#[test]
fn deep_eval_resolved_string_return_conditional_dual() {
    assert_dual(
        r#"
type Color {
    Red
    Green
    Blue
}
func color_name(c: Color) -> string {
    match c {
        Red => "red"
        Green => "green"
        Blue => "blue"
    }
}
func main() -> i32 {
    println(color_name(Green))
    println(color_name(Red))
    0
}
"#,
        "green\nred",
    );
}

// ── B5: nested else-if string zero-fill ───────────────────────────────────
// A nested else-if whose branches are string literals vs concat must not
// zero-fill the taken value (value-mode if width unification).

#[test]
fn deep_eval_nested_elseif_string_dual() {
    assert_dual(
        r#"
func f(a: bool, b: bool) -> string {
    if a { "x" } else if b { "y" } else { "z" }
}
func main() -> i32 {
    println(f(true, false))
    println(f(false, true))
    println(f(false, false))
    0
}
"#,
        "x\ny\nz",
    );
}

// ── B6: generic string parameter monomorphization ─────────────────────────
// A generic fn instantiated with string must lower the parameter as a
// {ptr,i64} struct slot, not an i64 placeholder (stack smash when the callee
// stores a 16-byte struct into a 8-byte alloca).

#[test]
fn deep_eval_generic_string_param_dual() {
    assert_dual(
        r#"
func id<T>(x: T) -> T { x }
func main() -> i32 {
    println(id("hello"))
    0
}
"#,
        "hello",
    );
}
