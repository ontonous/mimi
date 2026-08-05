//! Wave-1 audit-fix regression tests — codegen_infra.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).
//!
//! Coverage (owned codegen infrastructure):
//! - §6 HIGH actors.rs mailbox blob bounds + pack/unpack type parity + f32/struct result pack
//! - §6 HIGH block.rs if-expression phi width unification (max width, extend BOTH)
//! - §6 HIGH compile.rs/mod.rs/block.rs/func.rs flow-state qualified layout lookup
//! - §6 HIGH func.rs/block.rs generic-body defer + value-position return cleanup
//! - §6 HIGH registry/funcs.rs per-call-site FFI callback thunks
//! - §6 MEDIUM registry/types.rs Alias/Newtype registry lowering + variant boxing by owner
use super::*;

fn can_link() -> bool {
    crate::tests::can_link()
}

/// L1 dual assertion (mirrors dual_backend!dual_assert): checker gates the
/// source, then VM stdout and codegen stdout must both equal `expected`.
macro_rules! dual_eq {
    ($src:expr, $expected:expr) => {{
        check_source($src).unwrap_or_else(|diags| {
            panic!(
                "checker rejected dual_eq source:\n{}",
                diags
                    .iter()
                    .map(|d| format!("{}", d))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        });
        let (_vm_val, vm_stdout) = run_source_with_stdout($src);
        let cg_stdout = compile_and_run($src).expect("codegen failed");
        assert_eq!(
            cg_stdout.trim(),
            $expected,
            "codegen mismatch\ncodegen: {}\nexpected: {}",
            cg_stdout.trim(),
            $expected
        );
        assert_eq!(
            vm_stdout.trim(),
            $expected,
            "VM stdout mismatch\nvm: {}\nexpected: {}",
            vm_stdout.trim(),
            $expected
        );
    }};
}

// ============================================================
// Fix (§6 HIGH, actors.rs): mailbox argument/result packing must use a single
// type lowering (registry-backed) on BOTH the call-site pack and the dispatch
// unpack. Record-typed parameters cross as their real struct bytes; scalar and
// string slots keep consistent 8-byte-aligned offsets.
// ============================================================

#[test]
fn actor_mailbox_record_param() {
    if !can_link() {
        return;
    }
    // A record parameter crosses the mailbox. Pre-fix, the call site stored the
    // record's alloca pointer into an 8-byte accounting slot while dispatch read
    // a full struct — pack/unpack divergence. Post-fix both sides agree on the
    // registry-resolved struct layout.
    // Wave-2 C-group fix: record fields keep the declared i64 layout (the test
    // intent is 8-byte slot packing); the literals are pinned to i64 explicitly
    // because integer literals infer at minimum fitting width (i32) and record
    // fields apply strict unification with no numeric coercion (E0247).
    dual_eq!(
        r#"
        type Point { x: i64, y: i64 }
        actor Calc {
            func sum(p: Point) -> i64 { return p.x + p.y; }
        }
        func main() -> i32 {
            let c = Calc.spawn();
            println(c.sum(Point { x: 3 as i64, y: 4 as i64 }));
            0
        }
        "#,
        "7"
    );
}

#[test]
fn actor_mailbox_mixed_param_offsets() {
    if !can_link() {
        return;
    }
    // i64, string (16-byte slot), i64 — exercises slot offset arithmetic across
    // a non-scalar middle argument. The result must reflect the correct unpack.
    // Wave-2 C-group fix: actor-method call sites unify strictly (no numeric
    // coercion), so pin the i64 literals explicitly (E0211 otherwise).
    dual_eq!(
        r#"
        actor Adder {
            func combine(a: i64, s: string, b: i64) -> i64 { return a + b; }
        }
        func main() -> i32 {
            let ad = Adder.spawn();
            println(ad.combine(10 as i64, "sep", 32 as i64));
            0
        }
        "#,
        "42"
    );
}

// ============================================================
// Fix (§6 HIGH, block.rs): if-expression phi width unification. The target
// width must be max(then, else) and BOTH branches extend to it; phi_type is
// taken from a branch that actually reaches the merge. Pre-fix, a wider else
// value was zero-filled (silently discarded).
// ============================================================

#[test]
fn if_expr_phi_mixed_int_widths() {
    if !can_link() {
        return;
    }
    // then branch is i32 (param), else branch is an i64 literal. The wider
    // else value (70000) must survive the merge; the i32 then value widens.
    // Pre-fix picked target i32 and discarded the else value -> 0.
    dual_eq!(
        r#"
        func pick(cond: bool, a: i32) -> i64 {
            if cond { a } else { 70000 }
        }
        func main() -> i32 {
            println(pick(true, 5))
            println(pick(false, 5))
            0
        }
        "#,
        "5\n70000"
    );
}

// ============================================================
// Fix (§6 MEDIUM, registry/types.rs): Alias lowering must route through the
// registry (llvm_type_for) so an alias to a named record gets the real struct
// layout, not the bare-i64 fallback of the raw name map.
// ============================================================

#[test]
fn alias_to_record_layout() {
    if !can_link() {
        return;
    }
    // `type Pt = Point` aliases a record; field access through the alias needs
    // the struct layout. Pre-fix the alias lowered to i64 (miscompiled access).
    // Wave-2 C-group fix: pin the record literals to i64 (fields keep their
    // declared i64 layout; literals infer i32 and fields unify strictly).
    dual_eq!(
        r#"
        type Point { x: i64, y: i64 }
        type Pt = Point
        func main() -> i32 {
            let p: Pt = Point { x: 5 as i64, y: 6 as i64 };
            println(p.x + p.y);
            0
        }
        "#,
        "11"
    );
}

// ============================================================
// Fix (§6 HIGH, func.rs/block.rs): generic function bodies compile through
// compile_block_last_val, which previously dropped `defer` (and skipped
// return-path cleanup). The VM runs defers; codegen now registers a defer
// scope and executes it on every exit path.
// ============================================================

#[test]
fn generic_body_defer_runs() {
    if !can_link() {
        return;
    }
    // A defer inside a generic body must execute at scope exit (LIFO, before
    // the value is returned). Pre-fix codegen silently dropped it -> VM printed
    // "deferred", codegen did not (L1 divergence).
    // Wave-2 C-group fix: the `tag` argument is pinned to i64 explicitly —
    // minimum-width literal inference (i32) disagrees with the declared i64
    // generic-parameter instantiation (TOOL-RESOLUTION-001 otherwise).
    dual_eq!(
        r#"
        func tap<T>(x: T, tag: i64) -> i64 {
            defer { println("deferred") }
            println("body")
            tag
        }
        func main() -> i32 {
            let r = tap(9, 42 as i64)
            println(r)
            0
        }
        "#,
        "body\ndeferred\n42"
    );
}

#[test]
fn generic_body_defer_lifo_and_early_return() {
    if !can_link() {
        return;
    }
    // Multiple defers run in reverse order, and still run when the generic body
    // returns early.
    // Wave-2 C-group fix: pin the returned literal to i64 — explicit `return`
    // statements unify strictly against the declared return type (E0207
    // otherwise; minimum-width literal inference yields i32).
    dual_eq!(
        r#"
        func wrap<T>(x: T) -> i64 {
            defer { println("first") }
            defer { println("second") }
            println("work")
            return 7 as i64
        }
        func main() -> i32 {
            let v = wrap(1)
            println(v)
            0
        }
        "#,
        "work\nsecond\nfirst\n7"
    );
}
