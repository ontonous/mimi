//! Wave-2 audit-fix regression tests — bytecode VM execution side
//! (vm.rs exec/contract/fault semantics; compiler-side findings live in audit_fix_vm.rs).
//! Findings: devdocs/full-audit-2026-08-05-0656.md (second-round audit, H-9/H-10/H-11/H-14/B-*).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source_bytecode*, codegen via compile_and_run).
use super::*;

/// Registration smoke: bytecode VM executes a trivial program and prints through
/// the captured stdout channel. Guards the module wiring for Wave-2 additions.
#[test]
fn audit2_vm_exec_registration_smoke() {
    let src = r#"
func main() -> i32 {
    println(40 + 2)
    0
}
"#;
    let (_val, stdout) = run_source_bytecode_with_stdout(src);
    assert_eq!(stdout.trim(), "42");
}
