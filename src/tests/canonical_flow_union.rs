//! Canonical multi-target Flow union (`-> A | B`) consumer tests (R6-1034).
//!
//! The flat Copy union face — one single-field variant payload of the same
//! Copy scalar type across every target — is executable by the reference
//! interpreter, the AST-free bytecode VM, and the native emitter on one shared
//! `MirProgram`.  The heterogeneous face (a variant payload outside the flat
//! Copy scalar contract) stays executable by reference/bytecode but must keep
//! failing closed on the native and capability consumers until its own native
//! tagged-union contract is promoted; the route layer keeps such graphs on the
//! compatibility route.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::core::mir::reference::{
    MirProgram, MirReferenceFfiResolver, MirReferenceInterpreter, MirRuntimeValue,
};
use crate::core::mir::MirFfiCallContract;
use crate::core::{NodeId, ResolvedTypeId};
use crate::interp::bytecode::{compile_mir_program, BytecodeVM};
use crate::interp::Value;

const FLAT_COPY_UNION_SOURCE: &str =
    include_str!("../../tests/real_world/flow_multi_target_union_copy_flat.mimi");
const HETEROGENEOUS_UNION_SOURCE: &str =
    include_str!("../../tests/real_world/flow_multi_target_union_match.mimi");

const FLAT_COPY_UNION_STDOUT: &str = "60\n40\n";
const HETEROGENEOUS_UNION_STDOUT: &str = "110\n5\n";

fn materialize(source: &str, label: &str) -> MirProgram {
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file)
        .unwrap_or_else(|diags| panic!("check {label}: {diags:?}"));
    MirProgram::from_checked_program(&checked)
        .unwrap_or_else(|error| panic!("materialize {label}: {error:?}"))
}

/// Test-owned C ABI fixture for the float-conversion pins: compiles the
/// helper source into a real shared object the bytecode VM loads through
/// `MIMI_FFI_LIB` and the native harness links statically.
struct FloatFfiFixture {
    dir: PathBuf,
}

impl Drop for FloatFfiFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

static FLOAT_FFI_FIXTURE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn float_ffi_fixture(counter: u64, c_source: &str) -> FloatFfiFixture {
    let fixture_id = FLOAT_FFI_FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "mimi-flow-union-float-ffi-{}-{counter}-{fixture_id}",
        std::process::id(),
    ));
    std::fs::create_dir_all(&dir).expect("create float FFI fixture directory");
    let c_path = dir.join("ffi.c");
    let library = dir.join("ffi.so");
    std::fs::write(&c_path, c_source).expect("write float FFI fixture C source");
    let cc = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2"])
        .arg(&c_path)
        .arg("-o")
        .arg(&library)
        .output()
        .expect("C compiler for float FFI fixture");
    assert!(
        cc.status.success(),
        "{}",
        String::from_utf8_lossy(&cc.stderr)
    );
    FloatFfiFixture { dir }
}

fn flow_union_instruction_mut(
    function: &mut crate::core::mir::MirFunction,
) -> Option<&mut crate::core::mir::types::MirFlowEffectReceipt> {
    function.blocks.values_mut().find_map(|block| {
        block
            .instructions
            .iter_mut()
            .find_map(|instruction| match &mut instruction.kind {
                crate::core::mir::MirInstructionKind::FlowTransition { effect_receipt, .. } => {
                    effect_receipt.as_mut()
                }
                _ => None,
            })
    })
}

const MULTI_FIELD_UNION_SOURCE: &str = r#"
    flow P {
        state A { v: i32, w: i32 }
        state B { v: i32, w: i32 }
        transition go(A, d: i32) -> A | B {
            if d > 0 {
                return B { v: d, w: 1 }
            } else {
                return A { v: d, w: 2 }
            }
        }
    }

    func main() -> i32 {
        let a = A { v: 10, w: 20 }
        let r = P::go(a, 5)
        let t = match r {
            A { v, w } => v + w
            B { v, w } => v + w
        }
        println(t)
        0
    }
"#;

const MULTI_FIELD_UNION_STDOUT: &str = "6\n";

const MIXED_MULTI_FIELD_UNION_SOURCE: &str = r#"
    flow P {
        state A { v: i32 }
        state B { name: string, score: i32 }
        transition go(A, d: i32) -> A | B {
            if d > 0 {
                return B { name: "hit", score: d }
            }
            return A { v: d }
        }
    }

    func main() -> i32 {
        let a = A { v: 10 }
        let r = P::go(a, 5)
        let t = match r {
            A { v } => v
            B { name, score } => score
        }
        println(t)
        0
    }
"#;

const MIXED_MULTI_FIELD_UNION_STDOUT: &str = "5\n";

const FAULT_ABSORPTION_UNION_SOURCE: &str = r#"
    flow F {
        state S { v: i64 }
        transition go(S) -> S | Fault {
            return S { v: self.v }
        }
    }

    func main() -> i64 {
        let s = S { v: 7 }
        let r = F::go(s)
        let v = match r {
            S { v } => v
            Fault { last_state: _, unexpected_event: _, snapshot: _, trace: _ } => 1 as i64
        }
        println(v)
        0
    }
"#;

const FAULT_ABSORPTION_UNION_STDOUT: &str = "7\n";

// R6-1041: the last union legacy source-reachable residual recorded by
// R6-1040 — a checker-legal bare integer literal call argument inside the
// transition body.  The checker accepts `guarded(1)` via NumericWiden, but
// the resolved body records the pre-coercion i32 identity, so MIR call
// validation fail-closed (`rt:1eb1…` i32 vs `rt:49fa…` i64) and the graph
// kept the explicit legacy route.  Once the conversion receipt is
// materialized, this face must close onto the canonical route with the
// same three-consumer equivalence as every other promoted shape.
const BARE_LITERAL_CALL_UNION_SOURCE: &str = r#"
    func guarded(x: i64) -> i64 {
        x
    }

    flow F {
        state S { v: i64 }
        transition go(S) -> S | Fault {
            let y = guarded(1)
            return S { v: y }
        }
    }

    func main() -> i64 {
        let s = S { v: 7 }
        let r = F::go(s)
        let v = match r {
            S { v } => v
            Fault { last_state: _, unexpected_event: _, snapshot: _, trace: _ } => 1 as i64
        }
        println(v)
        0
    }
"#;

const BARE_LITERAL_CALL_UNION_STDOUT: &str = "1\n";

#[test]
fn bare_integer_literal_call_argument_closes_the_union_face() {
    let mir = materialize(
        BARE_LITERAL_CALL_UNION_SOURCE,
        "bare literal call union fixture",
    );
    assert!(crate::core::mir::contains_multi_target_flow_union_candidate(&mir));
    assert!(
        crate::core::mir::multi_target_flow_union_face_closed(&mir),
        "a NumericWiden call argument must not reopen the union face"
    );
    assert!(crate::verifier::validate_mir_capabilities(&mir).is_ok());

    let reference = MirReferenceInterpreter::new(&mir)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference executor bare literal call union");
    assert_eq!(reference.output, BARE_LITERAL_CALL_UNION_STDOUT);

    let bytecode = compile_mir_program(&mir).expect("bare literal call union bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(
        vm.run_value().is_ok(),
        "bytecode bare literal call union runs"
    );
    assert_eq!(vm.stdout(), BARE_LITERAL_CALL_UNION_STDOUT);

    if !can_link() {
        return;
    }
    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_flow_union_bare_literal_call");
    generator
        .compile_mir_native(&mir)
        .expect("native bare literal call union emission");
    generator
        .module
        .verify()
        .expect("valid LLVM bare literal call union module");
    let native = link_and_observe_canonical_mir(&generator).expect("native union execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, BARE_LITERAL_CALL_UNION_STDOUT);
    assert_eq!(native.stderr, "");
}

// R6-1042: the >2-target union differential — three targets (S | Big |
// Fault) with a Big branch actually taken, plus mixed-width scalar operands
// in the transition body (constant-first comparison `100 < self.v` and
// arithmetic `self.v + 1`).  The checker legalizes both through its numeric
// coercion rule without re-recording the operand identity, so the canonical
// binary contract (which requires operand TypeDesc equality) fail-closed
// the whole default route until the narrower side is materialized as an
// explicit Convert — the same disease R6-1041 fixed for call arguments, at
// the binary-operand site.
const THREE_TARGET_MIXED_WIDTH_UNION_SOURCE: &str = r#"
    flow F {
        state S { v: i64 }
        state Big { w: i64 }
        transition go(S) -> S | Big | Fault {
            if 100 < self.v {
                let bumped = self.v + 1
                return Big { w: bumped }
            }
            return S { v: self.v }
        }
    }

    func main() -> i64 {
        let s = S { v: 150 }
        let r = F::go(s)
        let v = match r {
            S { v } => v
            Big { w } => w
            Fault { last_state: _, unexpected_event: _, snapshot: _, trace: _ } => 0 as i64
        }
        println(v)
        0
    }
"#;

const THREE_TARGET_MIXED_WIDTH_UNION_STDOUT: &str = "151\n";

#[test]
fn three_target_union_mixed_width_operands_execute_on_all_consumers() {
    let mir = materialize(
        THREE_TARGET_MIXED_WIDTH_UNION_SOURCE,
        "three-target mixed-width union fixture",
    );
    assert!(crate::core::mir::contains_multi_target_flow_union_candidate(&mir));
    assert!(
        crate::core::mir::multi_target_flow_union_face_closed(&mir),
        "a three-target union with mixed-width binary operands must stay on the promoted contract"
    );
    assert!(crate::verifier::validate_mir_capabilities(&mir).is_ok());

    let reference = MirReferenceInterpreter::new(&mir)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference executor three-target union");
    assert_eq!(reference.output, THREE_TARGET_MIXED_WIDTH_UNION_STDOUT);

    let bytecode = compile_mir_program(&mir).expect("three-target union bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(vm.run_value().is_ok(), "bytecode three-target union runs");
    assert_eq!(vm.stdout(), THREE_TARGET_MIXED_WIDTH_UNION_STDOUT);

    if !can_link() {
        return;
    }
    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_flow_union_three_target");
    generator
        .compile_mir_native(&mir)
        .expect("native three-target union emission");
    generator
        .module
        .verify()
        .expect("valid LLVM three-target union module");
    let native = link_and_observe_canonical_mir(&generator).expect("native union execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, THREE_TARGET_MIXED_WIDTH_UNION_STDOUT);
    assert_eq!(native.stderr, "");
}

#[test]
fn binary_float_widening_closes_onto_the_canonical_contract() {
    // R6-1043: the checker legalizes int-vs-float arithmetic through the
    // (f64,i64)/(f64,i32) numeric-coercion pairs, and the canonical
    // conversion contract now admits signed (i32|i64) -> f64 widening, so
    // the narrower operand materializes as an explicit Convert inside both
    // the transition body and main.  The f64 result is observed bit-honestly
    // through a real C ABI boundary (x == 7.5 ? 42 : -1) on all three
    // consumers — this flips the R6-1042 known-boundary negative pin.  The
    // float *comparison* face stays fail-closed and is pinned separately
    // below.
    const C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_expect_f64(double x) { return x == 7.5 ? 42 : -1; }
"#;
    const SOURCE: &str = r#"
        extern "C" { func mir_ffi_expect_f64(x: f64) -> i64; }

        flow F {
            state S { v: i64 }
            transition go(S) -> S | Fault {
                let widened = self.v + 0.5
                return S { v: self.v }
            }
        }

        func main() -> i64 {
            let s = S { v: 7 }
            let r = F::go(s)
            let v = match r {
                S { v } => v
                Fault { last_state: _, unexpected_event: _, snapshot: _, trace: _ } => 0 as i64
            }
            let observed = mir_ffi_expect_f64(v + 0.5)
            println(observed)
            0
        }
    "#;
    const EXPECTED: &str = "42\n";
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = float_ffi_fixture(counter, C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let mir = materialize(SOURCE, "float-widening union fixture");
    assert!(crate::core::mir::contains_multi_target_flow_union_candidate(&mir));
    assert!(
        crate::core::mir::multi_target_flow_union_face_closed(&mir),
        "a float-widening binary operand must not reopen the union face"
    );
    assert!(crate::verifier::validate_mir_capabilities(&mir).is_ok());

    struct Oracle;
    impl MirReferenceFfiResolver for Oracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            args: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            match (receipt.symbol.as_str(), args) {
                ("mir_ffi_expect_f64", [MirRuntimeValue::FloatBits(bits)]) => {
                    let matched = f64::from_bits(*bits) == 7.5;
                    Ok(MirRuntimeValue::Int(if matched { 42 } else { -1 }))
                }
                _ => Err("unexpected float-widening FFI call".into()),
            }
        }
    }
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&Oracle)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference executor float-widening union");
    assert_eq!(reference.output, EXPECTED);

    let bytecode = compile_mir_program(&mir).expect("float-widening union bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(vm.run_value().is_ok(), "bytecode float-widening union runs");
    assert_eq!(vm.stdout(), EXPECTED);

    if !can_link() {
        return;
    }
    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_flow_union_float_widening");
    generator
        .compile_mir_native(&mir)
        .expect("native float-widening union emission");
    generator
        .module
        .verify()
        .expect("valid LLVM float-widening union module");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native union execution against the C fixture");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, EXPECTED);
    assert_eq!(native.stderr, "");
}

#[test]
fn binary_float_comparison_stays_outside_the_canonical_binary_contract() {
    // R6-1043 restatement of the R6-1042 negative pin.  The conversion
    // contract now admits signed (i32|i64) -> f64, so the int operand
    // materializes as an explicit Convert and the old stale-identity
    // rejection is gone — but the float comparison itself is still outside
    // the canonical finite-only Copy f64 binary contract (Add/Subtract
    // only), so the capability gate must keep rejecting the program with
    // the operator rejection.  Widening this face is a separate contract
    // extension (all consumers plus the SD-9/SD-10 comparison semantics),
    // not part of the conversion domain.
    let source = r#"
        flow F {
            state S { v: i64 }
            transition go(S) -> S | Fault {
                if self.v > 1.5 {
                    return S { v: 1 }
                }
                return S { v: self.v }
            }
        }

        func main() -> i64 {
            let s = S { v: 7 }
            let r = F::go(s)
            let v = match r {
                S { v } => v
                Fault { last_state: _, unexpected_event: _, snapshot: _, trace: _ } => 0 as i64
            }
            println(v)
            0
        }
    "#;
    let mir = materialize(source, "float-comparison boundary fixture");
    let capability_error = crate::verifier::validate_mir_capabilities(&mir)
        .expect_err("capability gate must keep rejecting float comparisons");
    assert!(
        capability_error.iter().any(|error| error.contains(
            "float binary operator Greater is outside the canonical finite-only Copy f64 contract"
        )),
        "the rejection must name the float operator, not a stale operand identity: {capability_error:?}"
    );
    assert!(
        !capability_error
            .iter()
            .any(|error| error.contains("binary operands have different TypeDesc identities")),
        "the int->f64 conversion must have materialized: {capability_error:?}"
    );
}

#[test]
fn integer_float_call_argument_receipt_executes_on_all_consumers() {
    // R6-1043: the call-argument NumericWiden receipt (R6-1041) covers the
    // checker's float-widening pairs too, so the integer literal argument to
    // an f64 parameter materializes as an Int->Float64 Convert at the call
    // site inside main, flows through the identity function, and is observed
    // through a real C ABI boundary (x == 2.0 ? 42 : -1) on all three
    // consumers.
    const C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_expect_scaled(double x) { return x == 2.0 ? 42 : -1; }
"#;
    const SOURCE: &str = r#"
        extern "C" { func mir_ffi_expect_scaled(x: f64) -> i64; }

        func scaled(x: f64) -> f64 {
            x
        }

        flow F {
            state S { v: i64 }
            state Big { w: i64 }
            transition go(S) -> S | Big | Fault {
                if 100 < self.v {
                    let bumped = self.v + 1
                    return Big { w: bumped }
                }
                return S { v: self.v }
            }
        }

        func main() -> i64 {
            let s = S { v: 150 }
            let r = F::go(s)
            let v = match r {
                S { v } => v
                Big { w } => w
                Fault { last_state: _, unexpected_event: _, snapshot: _, trace: _ } => 0 as i64
            }
            println(mir_ffi_expect_scaled(scaled(2)))
            0
        }
    "#;
    const EXPECTED: &str = "42\n";
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = float_ffi_fixture(counter, C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let mir = materialize(SOURCE, "int-to-float call argument fixture");
    assert!(crate::core::mir::contains_multi_target_flow_union_candidate(&mir));
    assert!(crate::core::mir::multi_target_flow_union_face_closed(&mir));
    assert!(crate::verifier::validate_mir_capabilities(&mir).is_ok());

    struct Oracle;
    impl MirReferenceFfiResolver for Oracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            args: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            match (receipt.symbol.as_str(), args) {
                ("mir_ffi_expect_scaled", [MirRuntimeValue::FloatBits(bits)]) => {
                    let matched = f64::from_bits(*bits) == 2.0;
                    Ok(MirRuntimeValue::Int(if matched { 42 } else { -1 }))
                }
                _ => Err("unexpected int-to-float call argument FFI call".into()),
            }
        }
    }
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&Oracle)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference executor int-to-float call argument");
    assert_eq!(reference.output, EXPECTED);

    let bytecode = compile_mir_program(&mir).expect("int-to-float call argument bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(
        vm.run_value().is_ok(),
        "bytecode int-to-float call argument runs"
    );
    assert_eq!(vm.stdout(), EXPECTED);

    if !can_link() {
        return;
    }
    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_flow_union_int_float_call");
    generator
        .compile_mir_native(&mir)
        .expect("native int-to-float call argument emission");
    generator
        .module
        .verify()
        .expect("valid LLVM int-to-float call argument module");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native union execution against the C fixture");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, EXPECTED);
    assert_eq!(native.stderr, "");
}

#[test]
fn i64_f64_conversion_rounds_to_nearest_even_on_all_consumers() {
    // Aggressive differential: 2^53 + 1 is not representable in f64 and
    // IEEE-754 round-to-nearest-even maps it exactly onto 2^53
    // (bit pattern 0x4330000000000000).  The C ABI helper pins the exact
    // bit pattern, so a consumer that compared exactly, truncated,
    // narrowed, or widened differently would return -1 instead of 42 —
    // the printed value pins which consumer disagreed.
    const C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_probe_f64_bits(double x) {
    union { double d; uint64_t u; } payload = { .d = x };
    return payload.u == 0x4340000000000000ULL ? 42 : -1;
}
"#;
    const SOURCE: &str = r#"
        extern "C" { func mir_ffi_probe_f64_bits(x: f64) -> i64; }

        func main() -> i64 {
            let big: i64 = 9007199254740993
            let widened = big + 0.0
            println(mir_ffi_probe_f64_bits(widened))
            0
        }
    "#;
    const EXPECTED: &str = "42\n";
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = float_ffi_fixture(counter, C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let mir = materialize(SOURCE, "i64-to-f64 rounding fixture");
    assert!(crate::verifier::validate_mir_capabilities(&mir).is_ok());

    struct Oracle;
    impl MirReferenceFfiResolver for Oracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            args: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            match (receipt.symbol.as_str(), args) {
                ("mir_ffi_probe_f64_bits", [MirRuntimeValue::FloatBits(bits)]) => {
                    let matched = *bits == 9007199254740992.0_f64.to_bits();
                    Ok(MirRuntimeValue::Int(if matched { 42 } else { -1 }))
                }
                _ => Err("unexpected i64-to-f64 rounding FFI call".into()),
            }
        }
    }
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&Oracle)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference executor i64-to-f64 rounding");
    assert_eq!(
        reference.output, EXPECTED,
        "round-to-nearest-even must land on exactly 2^53"
    );

    let bytecode = compile_mir_program(&mir).expect("i64-to-f64 rounding bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(vm.run_value().is_ok(), "bytecode i64-to-f64 rounding runs");
    assert_eq!(vm.stdout(), EXPECTED);

    if !can_link() {
        return;
    }
    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_flow_union_i64_f64_rounding");
    generator
        .compile_mir_native(&mir)
        .expect("native i64-to-f64 rounding emission");
    generator
        .module
        .verify()
        .expect("valid LLVM i64-to-f64 rounding module");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native execution against the C fixture");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, EXPECTED);
    assert_eq!(native.stderr, "");
}

#[test]
fn fault_absorption_union_promotes_to_canonical_mir() {
    // R6-1040: the compiler-owned Fault sink (0.36.9 verdict 6 — absorption
    // requires a DECLARED Fault target) is the last source-reachable union
    // legacy face. Promotion is surgical: (1) builtin trace records
    // (SystemTrace/MemoryDump/PanicPayload) materialize Record layouts from
    // the shared builtin_record_schema so product glue covers them; (2) the
    // Fault variant admits glue-complete payloads in the union contract —
    // user-declared variants keep the R6-1039 scalar/owned-String admission.
    let mir = materialize(FAULT_ABSORPTION_UNION_SOURCE, "fault absorption fixture");
    assert!(crate::core::mir::contains_multi_target_flow_union_candidate(&mir));
    assert!(
        crate::core::mir::multi_target_flow_union_face_closed(&mir),
        "the Fault-absorption union must close onto the promoted contract"
    );
    assert!(crate::verifier::validate_mir_capabilities(&mir).is_ok());

    let reference = MirReferenceInterpreter::new(&mir)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference executor fault absorption union");
    assert_eq!(reference.output, FAULT_ABSORPTION_UNION_STDOUT);

    let bytecode = compile_mir_program(&mir).expect("fault absorption union bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(
        vm.run_value().is_ok(),
        "bytecode fault absorption union runs"
    );
    assert_eq!(vm.stdout(), FAULT_ABSORPTION_UNION_STDOUT);

    if !can_link() {
        return;
    }
    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_flow_union_fault_absorption");
    generator
        .compile_mir_native(&mir)
        .expect("native fault absorption union emission");
    generator
        .module
        .verify()
        .expect("valid LLVM fault absorption union module");
    let native = link_and_observe_canonical_mir(&generator).expect("native union execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, FAULT_ABSORPTION_UNION_STDOUT);
    assert_eq!(native.stderr, "");
}

#[test]
fn user_zero_payload_enum_stays_outside_the_native_flow_id_contracts() {
    // R6-1040 negative: the flow StateId/EventId native admissions are
    // scoped to the checker-owned `type:flow::*::StateId`/`::EventId`
    // names. A user-declared all-zero-payload enum keeps failing the flat
    // Copy variant contract on the native consumer, and neither new
    // predicate claims it.
    let source = r#"
        type Gate { Open | Shut }

        func pick() -> Gate {
            Open()
        }

        func main() -> i64 {
            let gate = pick()
            match gate {
                Open => 1 as i64
                Shut => 2 as i64
            }
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check user enum fixture");
    let catalog = crate::core::mir::types::MirTypeCatalog::from_checked_program(&checked)
        .expect("catalog user enum fixture");
    let user_enum_id = checked
        .resolved_types()
        .iter()
        .find(|(_, ty)| {
            matches!(ty, crate::core::ResolvedType::Nominal { item, .. } if item.as_str().contains("Gate"))
        })
        .map(|(id, _)| id.clone())
        .expect("user enum TypeDesc");
    assert!(
        !catalog.is_zero_payload_flow_state_id_enum(&user_enum_id),
        "the ::StateId admission must not claim user zero-payload enums"
    );
    assert!(
        !catalog.is_flow_event_id_enum(&user_enum_id),
        "the ::EventId admission must not claim user zero-payload enums"
    );
    // The MIR structural validator is the outer guard: the program cannot
    // even materialize, let alone reach the native emitter.
    let error = MirProgram::from_checked_program(&checked)
        .expect_err("user all-zero-payload enum must stay fail-closed");
    assert!(
        format!("{error:?}").contains("all-zero-payload enum"),
        "the flat Copy variant contract must keep rejecting all-zero-payload user enums: {error:?}"
    );
}

#[test]
fn fault_sink_union_rejects_glue_incomplete_payload() {
    // R6-1040 negative: the Fault carve-out admits only payloads whose
    // canonical glue schedule is complete. Corrupting the trace record's
    // drop glue re-opens the union contract rejection, so a non-checker
    // producer cannot smuggle an opaque payload into the Fault variant.
    let mir = materialize(FAULT_ABSORPTION_UNION_SOURCE, "fault absorption fixture");
    let union_id = mir
        .transitions()
        .values()
        .find(|contract| contract.targets.len() > 1)
        .map(|contract| contract.result.clone())
        .expect("multi-target union transition");
    let trace_id = mir
        .type_catalog()
        .variant_layout(&union_id)
        .and_then(|(_, variants)| {
            variants
                .iter()
                .find(|variant| variant.name == "Fault")
                .and_then(|variant| {
                    variant
                        .fields
                        .iter()
                        .find(|field| field.name == "trace")
                        .map(|field| field.ty.clone())
                })
        })
        .expect("Fault variant trace payload");
    let mut forged_catalog = mir.type_catalog().clone();
    let mut forged_trace = forged_catalog
        .get(&trace_id)
        .cloned()
        .expect("SystemTrace TypeDesc");
    forged_trace.glue.drop = crate::core::mir::types::MirGlueKind::Unsupported;
    forged_catalog.replace_for_test_only(trace_id, forged_trace);
    let error = forged_catalog
        .validate_multi_target_union_variant(&union_id)
        .expect_err("glue-incomplete Fault payload must leave the union contract");
    // The union's variant-glue plan validation is the outer guard and fires
    // first: corrupting the trace record's glue re-opens the whole glue
    // plan, so the fail-closed message names the plan; the per-field
    // carve-out beneath it stays defense-in-depth.
    assert!(
        error.contains("glue plan is incomplete"),
        "the union contract must fail closed on glue-incomplete payloads: {error}"
    );
}

#[test]
fn mixed_multi_field_union_three_consumers_match() {
    // R6-1038: one variant may carry Copy and owned payloads side by side.
    // The consuming match moves the owned String through its own native
    // slot, and the arm-less faces (drop) settle every field obligation of
    // the active variant.
    let mir = materialize(
        MIXED_MULTI_FIELD_UNION_SOURCE,
        "mixed multi-field union fixture",
    );
    assert!(crate::core::mir::multi_target_flow_union_face_closed(&mir));
    assert!(crate::verifier::validate_mir_capabilities(&mir).is_ok());

    let reference = MirReferenceInterpreter::new(&mir)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference executor mixed multi-field union");
    assert_eq!(reference.output, MIXED_MULTI_FIELD_UNION_STDOUT);

    let bytecode = compile_mir_program(&mir).expect("mixed multi-field union bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(
        vm.run_value().is_ok(),
        "bytecode mixed multi-field union runs"
    );
    assert_eq!(vm.stdout(), MIXED_MULTI_FIELD_UNION_STDOUT);

    if !can_link() {
        return;
    }
    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_flow_union_mixed_multi_field");
    generator
        .compile_mir_native(&mir)
        .expect("native mixed multi-field union emission");
    generator
        .module
        .verify()
        .expect("valid LLVM mixed multi-field union module");
    let native = link_and_observe_canonical_mir(&generator).expect("native union execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, MIXED_MULTI_FIELD_UNION_STDOUT);
    assert_eq!(native.stderr, "");
}

#[test]
fn multi_field_union_three_consumers_match() {
    // R6-1038: the promoted multi-target union tagged-union contract widens
    // from one payload field per variant to a multi-field Copy/owned payload
    // union.  The same MirProgram must execute identically on reference,
    // bytecode, and native, with per-field native ABI slots in name-sorted
    // variant order.
    let mir = materialize(MULTI_FIELD_UNION_SOURCE, "multi-field union fixture");
    assert!(crate::core::mir::contains_multi_target_flow_union_candidate(&mir));
    assert!(
        crate::core::mir::multi_target_flow_union_face_closed(&mir),
        "a multi-field Copy union must close onto the widened contract"
    );
    assert!(crate::verifier::validate_mir_capabilities(&mir).is_ok());

    // Consumer 1: AST-free reference executor on the shared MirProgram.
    let reference = MirReferenceInterpreter::new(&mir)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference executor multi-field union");
    assert_eq!(reference.output, MULTI_FIELD_UNION_STDOUT);

    // Consumer 2: bytecode compiled from the same MirProgram (no AST).
    let bytecode = compile_mir_program(&mir).expect("multi-field union bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(vm.run_value().is_ok(), "bytecode multi-field union runs");
    assert_eq!(vm.stdout(), MULTI_FIELD_UNION_STDOUT);

    // Consumer 3: native LLVM emission from the same MirProgram.
    if !can_link() {
        return;
    }
    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_flow_union_multi_field");
    generator
        .compile_mir_native(&mir)
        .expect("native multi-field union emission");
    generator
        .module
        .verify()
        .expect("valid LLVM multi-field union module");
    let native = link_and_observe_canonical_mir(&generator).expect("native union execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, MULTI_FIELD_UNION_STDOUT);
    assert_eq!(native.stderr, "");
}

#[test]
fn multi_field_union_contract_symbolic_proven() {
    // R6-1038: the symbolic verifier domain merges one return path per target
    // state into a multi-field symbolic union and discharges the contract
    // through the caller's multi-field SwitchMove distribution.
    let source = r#"
        flow Gate {
            state Shut { v: i32, w: i32 }
            state Opened { v: i32, w: i32 }
            transition toggle(Shut, flag: bool) -> Opened | Shut {
                requires: flag == true
                ensures: flag == true
                if flag {
                    return Opened { v: 1, w: 2 }
                } else {
                    return Shut { v: 3, w: 4 }
                }
            }
        }

        func main() -> i32 {
            ensures: result == 0
            let g = Shut { v: 40, w: 50 }
            let next = Gate::toggle(g, true)
            let t = match next {
                Opened { v, w } => v + w
                Shut { v, w } => v + w
            }
            println(t)
            0
        }
    "#;
    let mir = materialize(source, "multi-field union verifier fixture");
    let results = crate::verifier::verify_mir(&mir, "multi-field-union-proven".into())
        .expect("MIR verifier runs the multi-field union program");
    let toggle = results
        .iter()
        .find(|result| result.func_name.contains("toggle"))
        .expect("toggle verification result");
    assert_eq!(
        toggle.status,
        crate::verifier::VerifStatus::Proven,
        "{}",
        toggle.message
    );
    let main = results
        .iter()
        .find(|result| result.func_name.contains("main"))
        .expect("main verification result");
    assert_eq!(
        main.status,
        crate::verifier::VerifStatus::Proven,
        "{}",
        main.message
    );
}

#[test]
fn flat_copy_union_three_consumers_match() {
    let mir = materialize(FLAT_COPY_UNION_SOURCE, "flat Copy union fixture");
    assert!(crate::core::mir::contains_multi_target_flow_union_candidate(&mir));
    assert!(crate::core::mir::multi_target_flow_union_face_closed(&mir));
    let transition = mir
        .transitions()
        .values()
        .find(|contract| contract.targets.len() > 1)
        .expect("multi-target transition contract");
    assert_eq!(transition.targets.len(), 2);
    assert_eq!(transition.failure, None);
    assert!(!transition.is_fallback && !transition.is_ffi_pinned);

    // Consumer 1: AST-free reference executor on the shared MirProgram.
    let reference = MirReferenceInterpreter::new(&mir)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference executor union face");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, FLAT_COPY_UNION_STDOUT);

    // Consumer 2: bytecode compiled from the same MirProgram (no AST).
    let bytecode = compile_mir_program(&mir).expect("AST-free union bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value().expect("bytecode union execution"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), FLAT_COPY_UNION_STDOUT);

    // Consumer 3: native LLVM emission from the same MirProgram.
    if !can_link() {
        return;
    }
    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_flow_union_flat_copy");
    generator
        .compile_mir_native(&mir)
        .expect("native union emission");
    generator.module.verify().expect("valid LLVM union module");
    let native = link_and_observe_canonical_mir(&generator).expect("native union execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, FLAT_COPY_UNION_STDOUT);
    assert_eq!(native.stderr, "");
}

#[test]
fn heterogeneous_union_three_consumers_match() {
    // R6-1035B: the promoted multi-target union tagged-union contract admits
    // the heterogeneous face (Copy integer versus owned Move string payloads)
    // on every consumer.  The whole graph routes canonical and the same
    // MirProgram executes identically on reference, bytecode, and native.
    let mir = materialize(HETEROGENEOUS_UNION_SOURCE, "heterogeneous union fixture");
    assert!(crate::core::mir::contains_multi_target_flow_union_candidate(&mir));
    assert!(crate::core::mir::multi_target_flow_union_face_closed(&mir));
    assert!(crate::verifier::validate_mir_capabilities(&mir).is_ok());

    // Consumer 1: AST-free reference executor on the shared MirProgram.
    let reference = MirReferenceInterpreter::new(&mir)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference executor heterogeneous union");
    assert_eq!(reference.output, HETEROGENEOUS_UNION_STDOUT);

    // Consumer 2: bytecode compiled from the same MirProgram (no AST).
    let bytecode = compile_mir_program(&mir).expect("heterogeneous union bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(vm.run_value().is_ok(), "bytecode heterogeneous union runs");
    assert_eq!(vm.stdout(), HETEROGENEOUS_UNION_STDOUT);

    // Consumer 3: native LLVM emission from the same MirProgram.
    if !can_link() {
        return;
    }
    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_flow_union_heterogeneous");
    generator
        .compile_mir_native(&mir)
        .expect("native heterogeneous union emission");
    generator.module.verify().expect("valid LLVM union module");
    let native = link_and_observe_canonical_mir(&generator).expect("native union execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, HETEROGENEOUS_UNION_STDOUT);
    assert_eq!(native.stderr, "");
}

#[test]
fn heterogeneous_union_tag_contract_is_name_sorted_with_payload_mirroring() {
    // R6-1035A baseline pin: the union tag contract that the native
    // tagged-union promotion must preserve.  Variants stay name-sorted with
    // enumeration discriminants — the same ordering the legacy synthesized
    // enum uses — so a promoted native tag dispatch cannot silently disagree
    // with the reference/bytecode value model.  Each variant mirrors its
    // target state payload: `Closed{v: i32}` (Copy integer) versus
    // `Open{tag: string}` (owned Move) is the exact heterogeneous face the
    // promotion must admit.
    let mir = materialize(HETEROGENEOUS_UNION_SOURCE, "heterogeneous union fixture");
    let contract = mir
        .transitions()
        .values()
        .find(|contract| contract.owner.0.contains("Pipe::push"))
        .expect("Pipe::push multi-target contract");
    let union = mir
        .type_catalog()
        .get(&contract.result)
        .expect("union TypeDesc materialized");
    assert_eq!(
        union.kind,
        crate::core::mir::types::MirTypeKind::FlowStateSet
    );
    assert_eq!(union.ownership, crate::core::mir::types::MirOwnership::Move);
    let variants = match &union.layout {
        crate::core::mir::types::MirLayout::Enum { variants, .. } => variants,
        other => panic!("expected Enum layout, got {other:?}"),
    };
    let names: Vec<&str> = variants
        .iter()
        .map(|variant| variant.name.as_str())
        .collect();
    assert_eq!(names, vec!["Closed", "Open"], "variants stay name-sorted");
    for (index, variant) in variants.iter().enumerate() {
        assert_eq!(
            variant.discriminant, index as u16,
            "discriminants enumerate"
        );
    }
    let closed = &variants[0];
    assert_eq!(closed.fields.len(), 1);
    assert_eq!(closed.fields[0].name, "v");
    let closed_payload = mir
        .type_catalog()
        .get(&closed.fields[0].ty)
        .expect("Closed payload TypeDesc");
    assert!(matches!(
        closed_payload.abi,
        crate::core::mir::types::MirAbiClass::Integer { .. }
    ));
    let open = &variants[1];
    assert_eq!(open.fields.len(), 1);
    assert_eq!(open.fields[0].name, "tag");
    let open_payload = mir
        .type_catalog()
        .get(&open.fields[0].ty)
        .expect("Open payload TypeDesc");
    assert_eq!(
        open_payload.abi,
        crate::core::mir::types::MirAbiClass::StringHandle
    );
    assert_eq!(
        open_payload.ownership,
        crate::core::mir::types::MirOwnership::Move
    );
}

#[test]
fn heterogeneous_union_drop_face_runs_on_reference_and_bytecode() {
    // R6-1035B: consuming a heterogeneous union value with an explicit
    // `drop(...)` (no match projection) exercises the union drop face — the
    // tag-switched variant drop glue — on every consumer, including the
    // native emitter's recursive payload drop.
    let source = r#"
        flow Pipe {
            state Open { tag: string }
            state Closed { v: i32 }
            transition push(Open) -> Closed | Open {
                return Closed { v: 5 }
            }
        }

        func main() -> i32 {
            let o = Open { tag: "hello" }
            let r = Pipe::push(o)
            drop(r)
            println(7)
            0
        }
    "#;
    let mir = materialize(source, "heterogeneous union drop fixture");
    assert!(crate::core::mir::contains_multi_target_flow_union_candidate(&mir));
    assert!(crate::core::mir::multi_target_flow_union_face_closed(&mir));

    let reference = MirReferenceInterpreter::new(&mir)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference union drop face");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "7\n");
    let bytecode = compile_mir_program(&mir).expect("union drop bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(vm.run_value().is_ok(), "bytecode union drop runs");
    assert_eq!(vm.stdout(), "7\n");

    if !can_link() {
        return;
    }
    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_flow_union_drop");
    generator
        .compile_mir_native(&mir)
        .expect("native union drop emission");
    generator.module.verify().expect("valid LLVM union module");
    let native = link_and_observe_canonical_mir(&generator).expect("native union drop execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "7\n");
    assert_eq!(native.stderr, "");
}

#[test]
fn aggregate_payload_union_rejected_at_checker() {
    // R6-1039 fail-close ruling (RED first): out-of-contract multi-target
    // union payload shapes — aggregate (List / user record / Option), float,
    // and payload-less variants — are checker-legal today and keep the legacy
    // union route reachable (mixed-coverage disposition).  The ruling rejects
    // them at the declaration site with E0446 so every checker-legal
    // multi-target union closes the promoted tagged-union contract face.
    let list_payload = r#"
        flow P {
            state A { v: i32 }
            state B { xs: List<i32> }
            transition go(A, d: i32) -> A | B {
                return B { xs: [d] }
            }
        }

        func main() -> i32 {
            let a = A { v: 1 }
            let r = P::go(a, 2)
            drop(r)
            0
        }
    "#;
    let record_payload = r#"
        type Stats { hits: i32, misses: i32 }

        flow P {
            state A { v: i32 }
            state B { s: Stats }
            transition go(A, d: i32) -> A | B {
                return B { s: Stats { hits: d, misses: 0 } }
            }
        }

        func main() -> i32 {
            let a = A { v: 1 }
            let r = P::go(a, 2)
            drop(r)
            0
        }
    "#;
    let float_payload = r#"
        flow P {
            state A { v: i64 }
            state B { v: f64 }
            transition go(A, d: i64) -> A | B {
                return B { v: 1.5 }
            }
        }

        func main() -> i64 {
            let a = A { v: 1 }
            let r = P::go(a, 2)
            drop(r)
            0
        }
    "#;
    let payloadless_variant = r#"
        flow P {
            state A
            state B { v: i32 }
            transition go(A, d: i32) -> A | B {
                return B { v: d }
            }
        }

        func main() -> i32 {
            let a = A { }
            let r = P::go(a, 2)
            drop(r)
            0
        }
    "#;
    let option_payload = r#"
        flow P {
            state A { v: i32 }
            state B { o: Option<i32> }
            transition go(A, d: i32) -> A | B {
                return B { o: Some(d) }
            }
        }

        func main() -> i32 {
            let a = A { v: 1 }
            let r = P::go(a, 2)
            drop(r)
            0
        }
    "#;
    let fixtures = [
        ("List payload", list_payload),
        ("record payload", record_payload),
        ("float payload", float_payload),
        ("payload-less variant", payloadless_variant),
        ("Option payload", option_payload),
    ];
    for (label, source) in fixtures {
        let diagnostics = check_source(source)
            .expect_err("out-of-contract union fixture unexpectedly passed the checker");
        let rendered = diagnostics
            .iter()
            .map(|d| format!("{} {}", d.code.clone().unwrap_or_default(), d.message))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("E0446"),
            "{label}: expected the E0446 fail-close ruling at the checker, got:\n{rendered}"
        );
    }
}

#[test]
fn multi_target_union_fail_close_keeps_admitted_faces_legal() {
    // R6-1039 no-over-rejection pins: the E0446 ruling is scoped to
    // multi-target payload shapes only.  Single-target aggregate states stay
    // legal (the union contract never looks at them), transparent type
    // aliases resolve to admitted scalars, and the compiler-owned Fault sink
    // stays exempt as a multi-target target (0.36.9 裁决 6: absorption
    // requires a DECLARED Fault target).
    let single_target_aggregate = r#"
        flow P {
            state A { v: i32 }
            state B { xs: List<i32> }
            transition go(A, d: i32) -> B {
                return B { xs: [d] }
            }
        }

        func main() -> i32 {
            let a = A { v: 1 }
            let b = P::go(a, 2)
            drop(b)
            0
        }
    "#;
    let alias_resolved_scalar = r#"
        type Id = i64

        flow P {
            state A { v: i32 }
            state B { v: Id }
            transition go(A, d: i32) -> A | B {
                if d > 0 {
                    return B { v: 7 }
                }
                return A { v: d }
            }
        }

        func main() -> i64 {
            let a = A { v: 1 }
            let r = P::go(a, 2)
            let t = match r {
                A { v } => v as i64
                B { v } => v
            }
            t
        }
    "#;
    let fault_absorption_target = r#"
        func guarded(x: i64) -> i64 {
            requires: x > 0
            x
        }

        flow F {
            state S { v: i64 }
            transition go(S) -> S | Fault {
                let y = guarded(1)
                return S { v: y }
            }
        }

        func main() -> i64 {
            let s = S { v: 0 }
            let r = F::go(s)
            let v = match r {
                S { v } => v
                Fault { last_state: _, unexpected_event: _, snapshot: _, trace: _ } => 1 as i64
            }
            v
        }
    "#;
    for (label, source) in [
        ("single-target aggregate state", single_target_aggregate),
        ("alias-resolved scalar union field", alias_resolved_scalar),
        ("Fault absorption target", fault_absorption_target),
    ] {
        if let Err(diagnostics) = check_source(source) {
            let rendered = diagnostics
                .iter()
                .map(|d| format!("{} {}", d.code.clone().unwrap_or_default(), d.message))
                .collect::<Vec<_>>()
                .join("\n");
            panic!("{label} must stay checker-legal, got:\n{rendered}");
        }
    }
}

#[test]
fn union_outside_promoted_payload_contract_stays_fail_closed() {
    // R6-1039 restatement: aggregate payloads (List, record) are rejected by
    // the checker itself before any MIR consumer runs, so the MIR-level
    // native/capability negatives are no longer source-buildable — those
    // gates remain as defense-in-depth for non-checker producers only.  The
    // formerly-rejected wide multi-field variant stays pinned as a converged
    // positive across the native and capability consumers (R6-1038).
    let list_payload = r#"
        flow P {
            state A { v: i32 }
            state B { xs: List<i32> }
            transition go(A, d: i32) -> A | B {
                return B { xs: [d] }
            }
        }

        func main() -> i32 {
            let a = A { v: 1 }
            let r = P::go(a, 2)
            drop(r)
            0
        }
    "#;
    let list_diagnostics = check_source(list_payload)
        .expect_err("the checker rejects a List payload union before any MIR consumer runs");
    assert!(list_diagnostics
        .iter()
        .any(|d| d.code.as_deref() == Some("E0446")));

    let record_payload = r#"
        type Stats { hits: i32, misses: i32 }

        flow R {
            state A { v: i32 }
            state S { s: Stats }
            transition go(A, d: i32) -> A | S {
                return S { s: Stats { hits: d, misses: 0 } }
            }
        }

        func main() -> i32 {
            let a = A { v: 1 }
            let r = R::go(a, 2)
            drop(r)
            0
        }
    "#;
    let record_diagnostics = check_source(record_payload)
        .expect_err("the checker rejects a record payload union before any MIR consumer runs");
    assert!(record_diagnostics
        .iter()
        .any(|d| d.code.as_deref() == Some("E0446")));

    let wide_variant = r#"
        flow Q {
            state A { v: i32 }
            state W { a: i32, b: i32 }
            transition go(A, d: i32) -> A | W {
                return W { a: d, b: d }
            }
        }

        func main() -> i32 {
            let a = A { v: 1 }
            let r = Q::go(a, 2)
            drop(r)
            0
        }
    "#;
    let wide_mir = materialize(wide_variant, "wide variant union fixture");
    assert!(
        crate::core::mir::multi_target_flow_union_face_closed(&wide_mir),
        "the multi-field variant face is promoted since R6-1038"
    );
    crate::codegen::mir::validate_mir_native(&wide_mir)
        .expect("native must admit the promoted multi-field union variant");
    crate::verifier::validate_mir_capabilities(&wide_mir)
        .expect("capability gate must admit the promoted multi-field union variant");
}

#[test]
fn missing_union_effect_receipt_rejects_all_consumers() {
    let mir = materialize(FLAT_COPY_UNION_SOURCE, "missing receipt fixture");
    let mut forged_main = mir
        .functions()
        .get(&NodeId("function:main".into()))
        .expect("main body")
        .clone();
    let flow_transition = forged_main
        .blocks
        .values_mut()
        .find_map(|block| {
            block
                .instructions
                .iter_mut()
                .find_map(|instruction| match &mut instruction.kind {
                    crate::core::mir::MirInstructionKind::FlowTransition {
                        effect_receipt, ..
                    } => Some(effect_receipt.take()),
                    _ => None,
                })
        })
        .expect("FlowTransition in main");
    assert!(
        flow_transition.is_some(),
        "the fixture main carries a union effect receipt"
    );
    let mut forged = mir.clone();
    forged.replace_function_for_test_only(forged_main);

    let reference_error = MirReferenceInterpreter::new(&forged)
        .execute(&NodeId("function:main".into()), &[])
        .expect_err("reference must reject a missing union receipt");
    assert!(
        reference_error
            .to_string()
            .contains("explicit canonical effect receipt"),
        "{reference_error}"
    );
    let bytecode_error =
        compile_mir_program(&forged).expect_err("bytecode must reject a missing union receipt");
    assert!(bytecode_error
        .iter()
        .any(|error| error.message.contains("receipt")));
    let capability_error = crate::verifier::validate_mir_capabilities(&forged)
        .expect_err("capability gate must reject a missing union receipt");
    assert!(capability_error
        .iter()
        .any(|error| error.contains("receipt")));
    let native_error = crate::codegen::mir::validate_mir_native(&forged)
        .expect_err("native must reject a missing union receipt");
    assert!(native_error
        .iter()
        .any(|error| error.message.contains("explicit canonical effect receipt")));
}

#[test]
fn forged_union_receipt_target_rejects_all_consumers() {
    let mir = materialize(FLAT_COPY_UNION_SOURCE, "forged receipt fixture");
    // Forge the receipt to name the second target state instead of the union
    // identity: a multi-target union receipt must name contract.result.
    let second_target: ResolvedTypeId = {
        let contract = mir
            .transitions()
            .values()
            .find(|contract| contract.targets.len() > 1)
            .expect("multi-target contract");
        contract.targets[1].clone()
    };
    let mut forged_main = mir
        .functions()
        .get(&NodeId("function:main".into()))
        .expect("main body")
        .clone();
    let receipt =
        flow_union_instruction_mut(&mut forged_main).expect("union effect receipt in main");
    receipt.target = second_target;
    let mut forged = mir.clone();
    forged.replace_function_for_test_only(forged_main);

    let reference_error = MirReferenceInterpreter::new(&forged)
        .execute(&NodeId("function:main".into()), &[])
        .expect_err("reference must reject a forged union receipt target");
    assert!(
        reference_error
            .to_string()
            .contains("target identity disagrees"),
        "{reference_error}"
    );
    let bytecode_error = compile_mir_program(&forged)
        .expect_err("bytecode must reject a forged union receipt target");
    assert!(bytecode_error
        .iter()
        .any(|error| error.message.contains("receipt")));
    let capability_error = crate::verifier::validate_mir_capabilities(&forged)
        .expect_err("capability gate must reject a forged union receipt target");
    assert!(capability_error
        .iter()
        .any(|error| error.contains("receipt")));
}

#[test]
fn union_contract_disproven_is_real_and_no_contract_stays_no_obligations() {
    // R6-1036B flip of the R6-1034 boundary pin
    // (union_verifier_boundary_is_scoped_to_contract_bearing_callables): an
    // ensures-only union transition now gets a real verdict.  `flag == false`
    // has no requires constraining the parameter, so Z3 finds the
    // counterexample and returns Disproven — not a boundary observation and
    // not a vacuous green.  Contract-free functions emit no verification
    // obligation at all, and definitive verdicts are route-compatible
    // without the runtime-only boundary opt-in flags.
    let source = r#"
        flow Gate {
            state Shut { v: i32 }
            state Opened { v: i32 }
            transition toggle(Shut, flag: bool) -> Opened | Shut {
                ensures: flag == false
                if flag {
                    return Opened { v: 1 }
                } else {
                    return Shut { v: 0 }
                }
            }
        }

        func main() -> i32 {
            let g = Shut { v: 40 }
            let next = Gate::toggle(g, true)
            let t = match next {
                Opened { v } => v
                Shut { v } => v
            }
            println(t)
            0
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check union contract fixture");
    let mir = MirProgram::from_checked_program(&checked).expect("materialize");
    let results = crate::verifier::verify_mir(&mir, "flow-union-disproven".into())
        .expect("MIR verifier runs the union program");
    let toggle = results
        .iter()
        .find(|result| result.func_name.contains("toggle"))
        .expect("toggle verification result");
    assert_eq!(
        toggle.status,
        crate::verifier::VerifStatus::Disproven,
        "{}",
        toggle.message
    );
    assert!(
        !toggle
            .message
            .contains(crate::core::mir::types::MIR_VERIFIER_FLOW_UNION_BOUNDARY_CODE),
        "a real verdict must not carry the runtime-only boundary identity: {}",
        toggle.message
    );
    // Contract-free functions (main here) are outside the verifier's
    // obligation set: verify_mir emits no result for them, so the single
    // definitive toggle verdict keeps the route ready without any boundary
    // opt-in.
    assert!(
        results
            .iter()
            .all(|result| !result.func_name.contains("main")),
        "contract-free functions emit no verification result: {results:?}"
    );
    assert!(
        crate::verifier::canonical_execution_route_verifier_ready(&results, false, false),
        "a Disproven union verdict is definitive and route-compatible"
    );
}

#[test]
fn union_contract_symbolic_pre_post_proven() {
    // R6-1036B flip of union_contract_verifier_trusted_subset_baseline
    // (821bb80f pinned the wholesale MIR-VERIFIER-FLOW-UNION-001 rejection):
    // contract-bearing union callables now get real verdicts.  The
    // transition's requires/ensures are discharged on both union return
    // paths, and a contract-bearing caller is verified through the
    // union-aware transition call plus the generic variant SwitchMove.
    let source = r#"
        flow Gate {
            state Shut { v: i32 }
            state Opened { v: i32 }
            transition toggle(Shut, flag: bool) -> Opened | Shut {
                requires: flag == true
                ensures: flag == true
                if flag {
                    return Opened { v: 1 }
                } else {
                    return Shut { v: 0 }
                }
            }
        }

        func main() -> i32 {
            ensures: result == 0
            let g = Shut { v: 40 }
            let next = Gate::toggle(g, true)
            let t = match next {
                Opened { v } => v
                Shut { v } => v
            }
            println(t)
            0
        }
    "#;
    let mir = materialize(source, "verified union contract program");
    assert!(crate::core::mir::contains_multi_target_flow_union_candidate(&mir));
    let results = crate::verifier::verify_mir(&mir, "union-symbolic-proven".into())
        .expect("MIR verifier runs the union program");
    let toggle = results
        .iter()
        .find(|result| result.func_name.contains("toggle"))
        .expect("toggle verification result");
    assert_eq!(
        toggle.status,
        crate::verifier::VerifStatus::Proven,
        "{}",
        toggle.message
    );
    assert!(!toggle
        .message
        .contains(crate::core::mir::types::MIR_VERIFIER_FLOW_UNION_BOUNDARY_CODE));
    let main = results
        .iter()
        .find(|result| result.func_name.contains("main"))
        .expect("main verification result");
    assert_eq!(
        main.status,
        crate::verifier::VerifStatus::Proven,
        "{}",
        main.message
    );
    assert!(!main
        .message
        .contains(crate::core::mir::types::MIR_VERIFIER_FLOW_UNION_BOUNDARY_CODE));
}

#[test]
fn union_outside_symbolic_payload_contract_rejected_upstream_of_verifier() {
    // R6-1039 restatement: the MIR-VERIFIER-FLOW-UNION-001 NotInTrustedSubset
    // boundary is no longer reachable from source — the checker rejects the
    // out-of-contract union (List payload) at the declaration site with
    // E0446, so the program never lowers to MIR and never reaches a verifier
    // verdict.  The boundary code stays in the verifier as defense-in-depth
    // for non-checker producers; its source-reachable pin retired with the
    // checker ruling.
    let source = r#"
        flow P {
            state A { v: i32 }
            state B { xs: List<i32> }
            transition go(A, d: i32) -> A | B {
                requires: d > 0
                ensures: d > 0
                if d > 0 {
                    return B { xs: [d] }
                } else {
                    return A { v: d }
                }
            }
        }

        func main() -> i32 {
            ensures: result == 0
            let a = A { v: 1 }
            let r = P::go(a, 2)
            drop(r)
            0
        }
    "#;
    let diagnostics =
        check_source(source).expect_err("the checker rejects the union before the verifier");
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code.as_deref() == Some("E0446")),
        "expected the E0446 fail-close ruling upstream of the verifier, got:\n{diagnostics:?}"
    );
}

#[test]
fn union_transition_with_failure_stays_checker_rejected() {
    // E0433 fail-closed boundary: `fails` combined with a multi-target union
    // return stays rejected at the checker before any MIR exists.
    let source = r#"
        flow P {
            state A { v: i32 }
            state B { v: i32 }
            transition go(A, d: i32) -> A | B fails string {
                return B { v: d }
            }
        }

        func main() -> i32 {
            0
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let diagnostics = crate::core::check_program(&file)
        .expect_err("fails + multi-target union must stay checker-rejected");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("E0433")),
        "{diagnostics:?}"
    );
}

#[test]
fn fault_absorption_union_stays_checker_legal_and_legacy_executable() {
    // R6-1041 restatement: the R6-1039 triple-bounded residual is erased.
    // The stale pre-coercion literal identity that made this exact graph
    // fail MIR materialization is fixed (the NumericWiden call-argument
    // receipt is now materialized), so the checker-legal absorption union
    // closes onto the canonical contract — pinned positively by
    // `bare_integer_literal_call_argument_closes_the_union_face`.  What
    // stays pinned here: (1) the Fault target remains exempt from the
    // E0446 ruling (0.36.9 裁决 6 — checker legality), and (2) the legacy
    // AST bytecode engine — the compatibility route for shapes still
    // outside every migrated island — keeps compiling and executing the
    // union graphs it always handled, until the M2 deletion gate removes
    // that route for closed islands.
    let source = r#"
        func guarded(x: i64) -> i64 {
            requires: x > 0
            x
        }

        flow F {
            state S { v: i64 }
            transition go(S) -> S | Fault {
                let y = guarded(1)
                return S { v: y }
            }
        }

        func main() -> i64 {
            let s = S { v: 0 }
            let r = F::go(s)
            let v = match r {
                S { v } => v
                Fault { last_state: _, unexpected_event: _, snapshot: _, trace: _ } => 1 as i64
            }
            println(v)
            0
        }
    "#;
    assert!(
        check_source(source).is_ok(),
        "the Fault absorption target stays exempt from the E0446 ruling (0.36.9 裁决 6)"
    );
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check Fault absorption fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("the absorption graph materializes once the literal identity boundary is fixed");
    assert!(
        crate::core::mir::multi_target_flow_union_face_closed(&mir),
        "the absorption union face is closed onto the promoted contract"
    );
    let mut compiler = crate::interp::bytecode::BytecodeCompiler::new();
    compiler.install_checked_program(&checked);
    let prog = compiler
        .compile_file(&file)
        .expect("legacy route compiles the Fault absorption union graph");
    let mut vm = crate::interp::bytecode::BytecodeVM::new(prog);
    vm.enable_stdout_capture();
    let exit = vm.run().expect("legacy route executes the union graph");
    assert_eq!(exit, 0);
    assert_eq!(
        vm.take_stdout().trim(),
        "1",
        "the legacy union route must reach the end of main"
    );
}
