//! 0.1.8 audit-fix regression — codegen/mod.rs heap-scope claims.
//!
//! Finding: devdocs/audit0820/codegen_mod.md F1 — escaped `List<string>`
//! ownership claims (`claimed_returned_string_lists` / `..._list_lists`) were
//! `mem::take`n on every nested scope flush (`free_heap_allocs`,
//! `emit_frees_for_top_scope`) but never restored, while the symmetric
//! closure-env claim (`claimed_returned_envs`) WAS restored. The free loops
//! also passed empty string-list claims to `emit_guarded_scope_free`, so an
//! escaping `List<string>` registered in an outer scope lost its guard the
//! moment any inner scope popped and could be freed → UAF / double-free.
//!
//! Fix: make string-list claims symmetric with the env claim — saved into
//! locals, passed to the guarded free, and restored when `heap_allocs.len()>1`
//! (i.e. until the function-level scope is popped). Strictly more guarding,
//! never less.
//!
//! N.B. The exact UAF is masked in practice by `ensure_returned_heap_strings_owned`
//! copying returned list contents; this test locks the correct *behavioral*
//! contract (escaped `List<string>` survives nested scopes intact) under both
//! backends. Memory correctness is verified via the `MIMI_ASAN=1` ASan channel.
use super::*;

fn can_link() -> bool {
    crate::tests::can_link()
}

/// Dual-backend assertion mirroring src/tests/dual_backend.rs style:
/// checker gate, then VM stdout and codegen stdout must BOTH equal expected.
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
    let codegen_stdout = compile_and_run(src).expect("codegen failed");
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
        "vm mismatch\nvm: {}\nexpected: {}",
        interp_stdout.trim(),
        expected
    );
}

#[test]
fn audit_codegen_mod_f1_escaped_list_string_through_loop() {
    let src = r#"
func make_names() -> List<string> {
    let names = ["alpha", "beta", "gamma"];
    let mut i = 0;
    while i < 3 {
        i = i + 1;
        if i == 2 {
            break;
        }
    }
    return names;
}

func main() {
    let r = make_names();
    println(r[0]);
    println(r[1]);
    println(r[2]);
    println(to_string(r.len()));
}
"#;
    assert_dual(src, "alpha\nbeta\ngamma\n3");
}

#[test]
fn audit_codegen_mod_f1_escaped_list_string_through_inner_block() {
    let src = r#"
func make_names() -> List<string> {
    let names = ["x", "y"];
    {
        let tmp = ["ignored"];
    }
    return names;
}

func main() {
    let r = make_names();
    println(r[0]);
    println(r[1]);
}
"#;
    assert_dual(src, "x\ny");
}
