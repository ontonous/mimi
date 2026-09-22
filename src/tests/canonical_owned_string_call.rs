//! R6-1076: the owned-String constant callable admission face.
//!
//! Before this slice, a program whose only String shapes were a constant
//! callable (`func greet() -> string { "hi" }`) and a call-result bind of it
//! stayed on the compatibility route: the checker-side admission scanner
//! floored the callee's String signature result, its root block, the bind
//! pattern and the call expression's own type, and the island gate rejected
//! the StringHandle values the materialized graph carries outside any string
//! print face.  R6-1074 had already opened the verifier half (the capability
//! gate mirrors `eval_direct_owned_string_call`'s routing), so the default
//! verify route proved these contracts through the dual-engine floor.
//!
//! R6-1076 closes the admission half with a shape-derived face: a concrete,
//! effect-free, non-prelude callable whose whole body is a string-literal
//! result joins the owned-String constant set.  Its String result, root
//! block, call-result binds and call expressions skip the profile floor, the
//! construction ledger (`validate_owned_string_return_shape` via the new
//! exactly-one-instruction constant-return candidate) re-proves the
//! materialized `Const "…" → Return` graph, and the island gate's String
//! value/constant arms are now the canonical StringHandle contract itself
//! (per-instruction Move/Clone/Drop/PrintlnString/Call arms keep policing
//! every use).  Off-shape callees — multi-statement bodies, String
//! parameters, concatenation, branches, second-hand rebinds outside a string
//! print face — keep the compatibility floor.
//!
//! R6-1077: the constant set closes over one-edge wrappers.  A callable
//! whose whole body is a call to a set member (`func wrap() -> string {
//! greet() }`) joins the set on the checker side, the construction ledger
//! gains the mirrored exactly-one-call-instruction candidate (callee
//! candidacy recurses with a cache and an active-set cycle guard, so cyclic
//! wrapper bodies keep the floor), and the shape ledger's Call arm admits
//! the wrapper's `Call(proven callee) → Return` graph exactly like a Clone
//! introduces a live String.  The verifier's per-hop routing explores the
//! wrapper body and re-proves the callee at its own hop, so contracts
//! through wrappers verify on the single canonical engine.
//!
//! R6-1078: the shape ledger's Call arm consumes its String arguments — the
//! same non-Copy transfer the verifier performs on the caller's symbolic
//! state — so wrapper chains whose callee takes a String argument
//! (`nested() { inner("hi") }` over a String-parameter identity) verify on
//! the canonical engine.  The checker-side scanner kept its provenance
//! floor: argument-position String literals stayed outside the wrapper
//! closure, so those programs still classified mixed — the same
//! classify/gate layering R6-1076 recorded.
//!
//! R6-1079: the identity member and the literal-argument face join the
//! checker-side set, closing that layering.  A concrete, effect-free,
//! non-prelude callable whose whole body returns its single String
//! parameter (`func echo(s: string) -> string { s }`) joins the set, and a
//! String-literal argument to any set member is admitted as the member's
//! input contract (its ledger-proven body consumes the parameter exactly
//! once).  `wrap() { echo("hi") }` and direct `echo("x")` binds now flip
//! the default run/build route; a String parameter in any other position
//! and a second-hand (non-literal) String argument keep the floor.
//!
//! R6-1080: the one-statement constant-bind body joins the set.  A
//! concrete, effect-free, non-prelude callable whose whole body is one
//! String-literal bind returned whole (`let a = "x"; a`) lowers to the
//! `Const → Move → Return` glue the ledger's shape validation proves, so
//! the scanner admits the same exactly-one-statement shape; a parameter,
//! a second bind, or a call-result initializer keeps the floor.
//!
//! R6-1081: the one-statement call-bind body joins the set through the
//! fixpoint — the whole body binds a set member's call result and returns
//! the binding (`let t = greet(); t`), the `Call → Move → Return` glue the
//! ledger proves.  Callee side is set membership, so the face lives in the
//! closure loop; a leaky callee, a parameter, or a second statement keeps
//! the floor.
//!
//! R6-1082: the second-hand rebind drops its print-function envelope.  A
//! String local's origin site floors independently of any later read and
//! mixed is sticky, so `let t = s` is admitted island-wide without hiding
//! unadmitted provenance; candidate evidence deliberately stays with the
//! boundary operations (a glue-only graph materializes no production
//! boundary and a literal-origin rebind keeps its floor at the origin).
//!
//! R6-1084: the end-of-body drop glue widened beyond owned-String returns.
//! The `mimi_string_free` ABI makes any unaccounted live String a real
//! leak, and an `i32`-returning `main` that binds a String it never prints
//! holds exactly that — the R6-1082-admitted rebind main leaked 6 bytes in
//! 2 blocks under valgrind.  The glue now closes every value-returning
//! single-block body's String ledger (multi-block bodies keep the
//! boundary; a body already consuming each handle emits nothing and stays
//! byte-identical), and glue Drops anchor at the root node with indexed
//! roles (`drop.0`, `drop.1`, …) so instruction identities stay unique.
//!
//! R6-1085: the settlement widened to multi-block bodies — per Return
//! site, through a deliberately conservative path-insensitive face: the
//! value must be introduced by the entry block's straight-line prefix
//! (String parameter or top-level bind) and consumed by no instruction
//! anywhere, which makes it definitely live at every Return; each return
//! site then drops the remaining set (`drop.mb.*` anchored at the owning
//! block).  A value consumed on any path (an explicit branch-local
//! `drop(s)`) holds the pass entirely — soundness over coverage, the
//! unreleased path keeps its leak as a documented boundary until a real
//! must-liveness dataflow replaces the shape predicate.
//!
//! R6-1083: the parameter-rebind body joins the set and the ledger gains
//! end-of-body drop glue.  The lowerer now discharges the original handles
//! a single-block owned-String body copied or left unused (`Drop` before the
//! Return terminator), so the rebind copy `let v = s; v` ends the ledger's
//! live set empty exactly like the identity face and the whole shape
//! (`func echo_chain(s: string) -> string { let v = s; v }`) flips the
//! route.  A side value beside a call no longer floors the verifier — the
//! glue consumes it — so the R6-1078 leak boundary moved to the structural
//! use-after-move floor (consuming a value twice).  A second statement, a
//! non-parameter initializer, or a reference binding keeps the scanner
//! floor.

use crate::core::mir::reference::{MirProgram, MirReferenceInterpreter, MirRuntimeValue};
use crate::core::NodeId;
use crate::interp::bytecode::{compile_mir_program, BytecodeVM};

fn checked_program_of(source: &str) -> crate::core::CheckedProgram {
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    crate::core::check_program(&file).expect("check")
}

fn assert_admitted(source: &str, label: &str) -> MirProgram {
    let checked = checked_program_of(source);
    assert_eq!(
        crate::core::mir::classify_scalar_collection_admission(&checked),
        crate::core::mir::ScalarCollectionAdmission::CompleteCoverage,
        "{label} must classify complete"
    );
    let mir = MirProgram::from_checked_program(&checked)
        .unwrap_or_else(|error| panic!("{label} materialize: {error:?}"));
    crate::core::mir::validate_scalar_collection_island(&mir)
        .unwrap_or_else(|errors| panic!("{label} island gate: {errors:?}"));
    mir
}

#[test]
fn owned_string_constant_call_bind_and_print_agrees_across_consumers() {
    let source = r#"
        func greet() -> string {
            "hi"
        }
        func main() -> i32 {
            let s = greet()
            println(s)
            0
        }
    "#;
    let label = "owned-string constant call bind and print";
    let mir = assert_admitted(source, label);
    let reference = MirReferenceInterpreter::new(&mir)
        .execute(&NodeId("function:main".into()), &[])
        .unwrap_or_else(|error| panic!("{label} reference: {error:?}"));
    assert_eq!(
        reference,
        MirRuntimeValue::Int(0),
        "{label} reference result"
    );
    let bytecode =
        compile_mir_program(&mir).unwrap_or_else(|error| panic!("{label} bytecode: {error:?}"));
    BytecodeVM::new(bytecode)
        .run()
        .unwrap_or_else(|error| panic!("{label} vm: {error:?}"));
}

#[test]
fn owned_string_constant_call_with_integer_stdout_routes_complete() {
    // The R6-1074 CLI contract shape without the extern declaration: the
    // dead call-result bind plus an integer stdout receipt is a complete
    // scalar-island program, and the ensures contract verifies through the
    // canonical route (no dual-engine E0439 floor).
    let source = r#"
        func greet() -> string {
            "hi"
        }
        func main() -> i32 {
            ensures: result == 1
            let s = greet()
            println(7)
            1
        }
    "#;
    let label = "owned-string constant call integer stdout";
    let mir = assert_admitted(source, label);
    crate::verifier::validate_mir_capabilities(&mir)
        .unwrap_or_else(|errors| panic!("{label} capability gate: {errors:?}"));
    let results = crate::verifier::verify_mir(&mir, "owned-string-call-int-stdout".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Verified),
        "{label} must verify, got {:?}",
        results[0].status
    );
}

#[test]
fn owned_string_constant_call_prints_through_call_argument() {
    // The call result in print-argument position joins the same face: the
    // visit_expr call-result exemption admits the call node and the string
    // print arm polices the PrintlnString contract.
    let source = r#"
        func greet() -> string {
            "hi"
        }
        func main() -> i32 {
            println(greet())
            0
        }
    "#;
    let label = "owned-string constant call print argument";
    let mir = assert_admitted(source, label);
    let bytecode =
        compile_mir_program(&mir).unwrap_or_else(|error| panic!("{label} bytecode: {error:?}"));
    BytecodeVM::new(bytecode)
        .run()
        .unwrap_or_else(|error| panic!("{label} vm: {error:?}"));
}

#[test]
fn owned_string_constant_callable_is_construction_validated() {
    // The materialized `Const "…" → Return` graph of a constant callable is
    // exactly the new candidate class of the construction ledger: the
    // one-block liveness proof runs on it at construction time.
    let source = r#"
        func greet() -> string {
            "hi"
        }
        func main() -> i32 {
            let s = greet()
            println(7)
            0
        }
    "#;
    let label = "owned-string constant callable construction ledger";
    let mir = assert_admitted(source, label);
    let catalog = mir.type_catalog();
    let greet = mir
        .functions()
        .get(&NodeId("function:greet".into()))
        .unwrap_or_else(|| panic!("{label} greet absent"));
    assert!(
        crate::core::mir::is_owned_string_return_candidate(greet, mir.functions(), catalog),
        "{label} greet must be an owned-String return candidate"
    );
    crate::core::mir::validate_owned_string_return_shape(greet, mir.functions(), catalog)
        .unwrap_or_else(|message| panic!("{label} greet ledger: {message}"));
}

#[test]
fn owned_string_one_edge_wrapper_bind_and_print_agrees_across_consumers() {
    // R6-1077: the wrapper's whole body is a call to a set member, so it
    // joins the closed face on the checker side, its materialized
    // `Call(proven callee) → Return` graph passes the ledger's Call arm, and
    // the bind/print consumers see exactly the same StringHandle chain.
    let source = r#"
        func greet() -> string {
            "hi"
        }
        func wrap() -> string {
            greet()
        }
        func main() -> i32 {
            let s = wrap()
            println(s)
            0
        }
    "#;
    let label = "owned-string one-edge wrapper bind and print";
    let mir = assert_admitted(source, label);
    let catalog = mir.type_catalog();
    let wrap = mir
        .functions()
        .get(&NodeId("function:wrap".into()))
        .unwrap_or_else(|| panic!("{label} wrap absent"));
    assert!(
        crate::core::mir::is_owned_string_return_candidate(wrap, mir.functions(), catalog),
        "{label} wrap must be an owned-String return candidate"
    );
    crate::core::mir::validate_owned_string_return_shape(wrap, mir.functions(), catalog)
        .unwrap_or_else(|message| panic!("{label} wrap ledger: {message}"));
    let reference = MirReferenceInterpreter::new(&mir)
        .execute(&NodeId("function:main".into()), &[])
        .unwrap_or_else(|error| panic!("{label} reference: {error:?}"));
    assert_eq!(
        reference,
        MirRuntimeValue::Int(0),
        "{label} reference result"
    );
    let bytecode =
        compile_mir_program(&mir).unwrap_or_else(|error| panic!("{label} bytecode: {error:?}"));
    BytecodeVM::new(bytecode)
        .run()
        .unwrap_or_else(|error| panic!("{label} vm: {error:?}"));
}

#[test]
fn owned_string_wrapper_chain_contract_verifies_on_mir() {
    // Two wrapper edges plus a contract-bearing main: the capability gate's
    // ledger mirror admits the wrapper composition, and the evaluator
    // explores wrap_of_wrap → wrap → greet one proven hop at a time, so the
    // ensures contract verifies on the single canonical engine.
    let source = r#"
        func greet() -> string {
            "hi"
        }
        func wrap() -> string {
            greet()
        }
        func wrap_of_wrap() -> string {
            wrap()
        }
        func main() -> i32 {
            ensures: result == 1
            let s = wrap_of_wrap()
            println(7)
            1
        }
    "#;
    let label = "owned-string wrapper chain contract";
    let mir = assert_admitted(source, label);
    crate::verifier::validate_mir_capabilities(&mir)
        .unwrap_or_else(|errors| panic!("{label} capability gate: {errors:?}"));
    let results = crate::verifier::verify_mir(&mir, "owned-string-wrapper-chain".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Verified),
        "{label} must verify, got {:?}",
        results[0].status
    );
}

#[test]
fn cyclic_owned_string_wrapper_keeps_the_compatibility_floor() {
    // A self-recursive wrapper body has no provenance chain: the scanner
    // closure never admits it (the callee is not yet a member when the
    // wrapper is judged), and the ledger's active-set guard makes the
    // recursive candidacy short-circuit false instead of diverging.
    let source = r#"
        func wrap() -> string {
            wrap()
        }
        func main() -> i32 {
            let s = wrap()
            println(7)
            0
        }
    "#;
    let label = "cyclic owned-string wrapper";
    let checked = checked_program_of(source);
    assert!(
        matches!(
            crate::core::mir::classify_scalar_collection_admission(&checked),
            crate::core::mir::ScalarCollectionAdmission::MixedCoverage
        ),
        "{label} must classify mixed"
    );
    let mir = MirProgram::from_checked_program(&checked)
        .unwrap_or_else(|error| panic!("{label} materialize: {error:?}"));
    let wrap = mir
        .functions()
        .get(&NodeId("function:wrap".into()))
        .unwrap_or_else(|| panic!("{label} wrap absent"));
    assert!(
        !crate::core::mir::is_owned_string_return_candidate(
            wrap,
            mir.functions(),
            mir.type_catalog()
        ),
        "{label} wrap must stay outside the owned-String return contract"
    );
}

#[test]
fn wrapper_composing_an_off_shape_callee_keeps_the_floor() {
    // R6-1080 restatement: the closure composes proven callees only.  The
    // former off-shape callee (`let a = "x"; a`) is now the admitted
    // constant-bind face, so the floor rides a callee that still escapes
    // every closed shape — a second bind leaks a source the ledger
    // rejects, and the wrapper inherits no closed face of its own.
    let source = r#"
        func shaky() -> string {
            let a = "x"
            let b = "y"
            a
        }
        func wrap() -> string {
            shaky()
        }
        func main() -> i32 {
            let s = wrap()
            println(7)
            0
        }
    "#;
    let label = "wrapper over off-shape callee";
    let checked = checked_program_of(source);
    assert!(
        matches!(
            crate::core::mir::classify_scalar_collection_admission(&checked),
            crate::core::mir::ScalarCollectionAdmission::MixedCoverage
        ),
        "{label} must classify mixed"
    );
}

#[test]
fn second_hand_rebind_routes_complete_across_consumers() {
    // R6-1082: the second-hand rebind face drops its print-function
    // envelope.  A String local in a concrete island can only originate in
    // an admitted call-result bind, an admitted literal, a seeded identity
    // parameter, or an earlier rebind — every other origin floors at its
    // own site and mixed is sticky, so admitting `let t = s` island-wide
    // never hides unadmitted provenance.  The rebind plus an integer
    // print face (the former second_hand_rebind negative) now classifies
    // complete and every consumer executes it on the canonical engine.
    // A rebind with no boundary operation at all stays OutsideProfile —
    // deliberately not candidate evidence, since a glue-only graph
    // materializes no production boundary operation and construction
    // would reject the admission.
    let source = r#"
        func greet() -> string {
            "hi"
        }
        func main() -> i32 {
            let s = greet()
            let t = s
            println(7)
            0
        }
    "#;
    let label = "second-hand rebind";
    let mir = assert_admitted(source, label);
    let reference = MirReferenceInterpreter::new(&mir)
        .execute(&NodeId("function:main".into()), &[])
        .unwrap_or_else(|error| panic!("{label} reference: {error:?}"));
    assert_eq!(
        reference,
        MirRuntimeValue::Int(0),
        "{label} reference result"
    );
    let bytecode =
        compile_mir_program(&mir).unwrap_or_else(|error| panic!("{label} bytecode: {error:?}"));
    BytecodeVM::new(bytecode)
        .run()
        .unwrap_or_else(|error| panic!("{label} vm: {error:?}"));
}

#[test]
fn param_rebind_member_routes_complete_across_consumers() {
    // R6-1083: the parameter-rebind face — `let v = s; v` copies the
    // parameter once and returns the copy, and the lowerer's end-of-body
    // drop glue discharges the parameter's original handle, so the ledger's
    // live set ends empty exactly like the identity face.  The member plus
    // a wrapper closure over it (literal argument) and both direct and
    // composed consumption route canonical on every consumer.  A
    // two-statement rebind chain keeps the scanner floor: the shape
    // predicate constrains the whole body to one rebind of the parameter.
    let source = r#"
        func echo_chain(s: string) -> string {
            let v = s
            v
        }
        func wrap() -> string {
            echo_chain("hi")
        }
        func main() -> i32 {
            let direct = echo_chain("yo")
            println(direct)
            println(wrap())
            0
        }
    "#;
    let label = "parameter rebind";
    let mir = assert_admitted(source, label);
    let reference = MirReferenceInterpreter::new(&mir)
        .execute(&NodeId("function:main".into()), &[])
        .unwrap_or_else(|error| panic!("{label} reference: {error:?}"));
    assert_eq!(
        reference,
        MirRuntimeValue::Int(0),
        "{label} reference result"
    );
    let bytecode =
        compile_mir_program(&mir).unwrap_or_else(|error| panic!("{label} bytecode: {error:?}"));
    BytecodeVM::new(bytecode)
        .run()
        .unwrap_or_else(|error| panic!("{label} vm: {error:?}"));
}

#[test]
fn main_side_leftover_strings_close_their_ledger() {
    // R6-1084: the end-of-body drop glue widened beyond owned-String
    // returns — an `i32`-returning `main` that binds a String and never
    // prints it (`let s = greet(); let t = s; println(7)`) used to hold
    // two unaccounted handles (valgrind: 6 bytes in 2 blocks definitely
    // lost on the native binary).  The graph now closes its ledger with
    // `drop.0`/`drop.1` before the Return — instruction identity anchors
    // at the root node, so each glue Drop gets an indexed role — and the
    // rebind face stays admitted with all consumers agreeing on the
    // dropped graph.
    let source = r#"
        func greet() -> string {
            "hi"
        }
        func main() -> i32 {
            let s = greet()
            let t = s
            println(7)
            0
        }
    "#;
    let label = "main-side leftover";
    let mir = assert_admitted(source, label);
    let main = mir
        .functions()
        .get(&NodeId("function:main".into()))
        .expect("main function present");
    let text = main.canonical_text();
    assert!(text.contains("inst:drop.0:"), "{text}");
    assert!(text.contains("inst:drop.1:"), "{text}");
    let reference = MirReferenceInterpreter::new(&mir)
        .execute(&NodeId("function:main".into()), &[])
        .unwrap_or_else(|error| panic!("{label} reference: {error:?}"));
    assert_eq!(
        reference,
        MirRuntimeValue::Int(0),
        "{label} reference result"
    );
    let bytecode =
        compile_mir_program(&mir).unwrap_or_else(|error| panic!("{label} bytecode: {error:?}"));
    BytecodeVM::new(bytecode)
        .run()
        .unwrap_or_else(|error| panic!("{label} vm: {error:?}"));
}

#[test]
fn multiblock_leftover_strings_settle_per_return_site() {
    // R6-1085: the settlement widened to multi-block (branch-tailed)
    // bodies for the shape where branch structure cannot hide a
    // consumption — the leftover is introduced by the entry prefix and
    // no instruction anywhere consumes it, so it is definitely live at
    // every Return and each return site drops the remaining set.  The
    // branchy rebind main used to leak 6 bytes in 2 blocks under
    // valgrind; the graph now carries `drop.mb.*` at the return block
    // and all consumers agree.
    let source = r#"
        func greet() -> string {
            "hi"
        }
        func main() -> i32 {
            let s = greet()
            let t = s
            if 1 > 0 {
                println(7)
            } else {
                println(8)
            }
            0
        }
    "#;
    let label = "multiblock leftover";
    let mir = assert_admitted(source, label);
    let main = mir
        .functions()
        .get(&NodeId("function:main".into()))
        .expect("main function present");
    let text = main.canonical_text();
    assert!(text.contains("inst:drop.mb.0:"), "{text}");
    assert!(text.contains("inst:drop.mb.1:"), "{text}");
    let reference = MirReferenceInterpreter::new(&mir)
        .execute(&NodeId("function:main".into()), &[])
        .unwrap_or_else(|error| panic!("{label} reference: {error:?}"));
    assert_eq!(
        reference,
        MirRuntimeValue::Int(0),
        "{label} reference result"
    );
    let bytecode =
        compile_mir_program(&mir).unwrap_or_else(|error| panic!("{label} bytecode: {error:?}"));
    BytecodeVM::new(bytecode)
        .run()
        .unwrap_or_else(|error| panic!("{label} vm: {error:?}"));
}

#[test]
fn multiblock_explicit_drop_holds_settlement_conservatively() {
    // R6-1085 boundary: an explicit `drop(s)` inside one branch puts the
    // value in the global consumed set, so the pass holds entirely — the
    // join/exit return site must not drop a value the other path already
    // released.  The unreleased path keeps the leak (documented
    // boundary); the pin holds the pass to its conservative face.
    let source = r#"
        func greet() -> string {
            "hi"
        }
        func main() -> i32 {
            let s = greet()
            if 1 > 0 {
                drop(s)
            } else {
                println(8)
            }
            0
        }
    "#;
    let label = "multiblock explicit drop hold";
    let mir = assert_admitted(source, label);
    let main = mir
        .functions()
        .get(&NodeId("function:main".into()))
        .expect("main function present");
    let text = main.canonical_text();
    assert!(!text.contains("inst:drop.mb."), "{text}");
    let reference = MirReferenceInterpreter::new(&mir)
        .execute(&NodeId("function:main".into()), &[])
        .unwrap_or_else(|error| panic!("{label} reference: {error:?}"));
    assert_eq!(
        reference,
        MirRuntimeValue::Int(0),
        "{label} reference result"
    );
}

#[test]
fn call_bind_member_routes_complete_across_consumers() {
    // R6-1081: the one-statement call-bind body (`let t = greet(); t`)
    // joins the set through the fixpoint closure — its `Call → Move →
    // Return` glue is ledger-proven (the Call arm introduces the binding
    // exactly like a Clone and the Move settles it into the return), and
    // the R6-1078 chain probes already verified the shape at the verifier
    // layer.  The former multi-statement-wrapper floor classifies complete
    // and composes: deep call-bind chains, direct member calls, and both
    // print faces route canonical together.
    let source = r#"
        func greet() -> string {
            "hi"
        }
        func wrap() -> string {
            let t = greet()
            t
        }
        func wrap2() -> string {
            let u = wrap()
            u
        }
        func main() -> i32 {
            let s = wrap2()
            println(s)
            println(wrap())
            println(7)
            0
        }
    "#;
    let label = "call-bind member";
    let mir = assert_admitted(source, label);
    let reference = MirReferenceInterpreter::new(&mir)
        .execute(&NodeId("function:main".into()), &[])
        .unwrap_or_else(|error| panic!("{label} reference: {error:?}"));
    assert_eq!(
        reference,
        MirRuntimeValue::Int(0),
        "{label} reference result"
    );
    let bytecode =
        compile_mir_program(&mir).unwrap_or_else(|error| panic!("{label} bytecode: {error:?}"));
    BytecodeVM::new(bytecode)
        .run()
        .unwrap_or_else(|error| panic!("{label} vm: {error:?}"));
}

#[test]
fn string_argument_wrapper_chain_routes_complete_across_consumers() {
    // R6-1078 opened the ledger's Call arm (String arguments transfer like
    // the verifier's own symbolic transfer) and pinned the wrapper chain
    // `nested() { inner("hi") }` at the verifier layer while the scanner
    // kept its provenance floor.  R6-1079 closes that layering: the
    // checker-side argument face admits a String-literal argument to an
    // owned-String member — the member's ledger-proven body consumes the
    // parameter exactly once — so the same chain now classifies complete
    // and every consumer executes it on the canonical engine: reference,
    // bytecode VM, and the single-engine MIR verifier.
    let source = r#"
        func inner(s: string) -> string { s }
        func nested() -> string { inner("hi") }
        func main() -> i32 {
            ensures: result == 1
            let s = nested()
            println(7)
            1
        }
    "#;
    let label = "string-argument wrapper chain";
    let mir = assert_admitted(source, label);
    let reference = MirReferenceInterpreter::new(&mir)
        .execute(&NodeId("function:main".into()), &[])
        .unwrap_or_else(|error| panic!("{label} reference: {error:?}"));
    assert_eq!(
        reference,
        MirRuntimeValue::Int(1),
        "{label} reference result"
    );
    let bytecode =
        compile_mir_program(&mir).unwrap_or_else(|error| panic!("{label} bytecode: {error:?}"));
    BytecodeVM::new(bytecode)
        .run()
        .unwrap_or_else(|error| panic!("{label} vm: {error:?}"));
    crate::verifier::validate_mir_capabilities(&mir)
        .unwrap_or_else(|errors| panic!("{label} capability gate: {errors:?}"));
    let results = crate::verifier::verify_mir(&mir, "owned-string-arg-wrapper".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    let main = results
        .iter()
        .find(|result| result.func_name == "function:main")
        .unwrap_or_else(|| panic!("{label} main obligation absent: {results:?}"));
    assert!(
        matches!(main.status, crate::verifier::VerifStatus::Verified),
        "{label} must verify, got {:?}",
        main.status
    );
}

#[test]
fn constant_bind_member_routes_complete_across_consumers() {
    // R6-1080: the one-statement constant-bind body (`let a = "x"; a`)
    // joins the set — its `Const → Move → Return` glue is exactly the
    // candidate the ledger's shape validation proves, and the R6-1077
    // journey probes already showed the verifier treating the shape as the
    // constant face.  The scanner side now agrees, so the former
    // multi_statement_body floor classifies complete and every consumer
    // executes it on the canonical engine — including composition with a
    // one-edge wrapper and the StringHandle print face.
    let source = r#"
        func greet() -> string {
            let a = "x"
            a
        }
        func wrap() -> string {
            greet()
        }
        func main() -> i32 {
            let s = wrap()
            println(s)
            println(7)
            0
        }
    "#;
    let label = "constant-bind member";
    let mir = assert_admitted(source, label);
    let reference = MirReferenceInterpreter::new(&mir)
        .execute(&NodeId("function:main".into()), &[])
        .unwrap_or_else(|error| panic!("{label} reference: {error:?}"));
    assert_eq!(
        reference,
        MirRuntimeValue::Int(0),
        "{label} reference result"
    );
    let bytecode =
        compile_mir_program(&mir).unwrap_or_else(|error| panic!("{label} bytecode: {error:?}"));
    BytecodeVM::new(bytecode)
        .run()
        .unwrap_or_else(|error| panic!("{label} vm: {error:?}"));
}

#[test]
fn identity_member_argument_face_routes_complete_across_consumers() {
    // R6-1079: the identity member itself (`echo(s) { s }`) joins the
    // owned-String set, and a direct literal-argument call from main
    // composes with the bind face and the StringHandle print face — the
    // end-to-end default-route shape the slice exists to unlock.
    let source = r#"
        func echo(s: string) -> string {
            s
        }
        func main() -> i32 {
            let s = echo("x")
            println(s)
            0
        }
    "#;
    let label = "identity member argument face";
    let mir = assert_admitted(source, label);
    let reference = MirReferenceInterpreter::new(&mir)
        .execute(&NodeId("function:main".into()), &[])
        .unwrap_or_else(|error| panic!("{label} reference: {error:?}"));
    assert_eq!(
        reference,
        MirRuntimeValue::Int(0),
        "{label} reference result"
    );
    let bytecode =
        compile_mir_program(&mir).unwrap_or_else(|error| panic!("{label} bytecode: {error:?}"));
    BytecodeVM::new(bytecode)
        .run()
        .unwrap_or_else(|error| panic!("{label} vm: {error:?}"));
}

// The narrowest floors this slice keeps: every callee below returns a
// String but its provenance escapes the closed constant face, so the whole
// program stays on the compatibility route.  R6-1077 moved the one-edge
// wrapper into the positive matrix; R6-1079 moved the identity member and
// the literal-argument face into it; R6-1080 moved the one-statement
// constant-bind body into it — the floors below pin the residues.
#[test]
fn off_shape_string_callables_keep_the_compatibility_floor() {
    struct OffShapeCase {
        name: &'static str,
        source: &'static str,
    }
    const CASES: &[OffShapeCase] = &[
        // R6-1080: the one-statement constant-bind body
        // (`let a = "x"; a`) joined the positive matrix — its
        // `Const → Move → Return` glue is ledger-proven.  The floors below
        // keep the compatibility route: a parameter in a multi-statement
        // body has no identity face, and a second bind leaks a source.
        OffShapeCase {
            name: "parameterized_multi_statement_body",
            source: r#"
                func greet(n: string) -> string {
                    let a = "x"
                    a
                }
                func main() -> i32 {
                    let s = greet("y")
                    println(7)
                    0
                }
            "#,
        },
        OffShapeCase {
            name: "double_constant_leak",
            source: r#"
                func greet() -> string {
                    let a = "x"
                    let b = "y"
                    a
                }
                func main() -> i32 {
                    let s = greet()
                    println(7)
                    0
                }
            "#,
        },
        // A String parameter keeps the parameter profile floor.
        OffShapeCase {
            name: "string_parameter",
            source: r#"
                func greet(n: string) -> string {
                    "hi"
                }
                func main() -> i32 {
                    let s = greet("x")
                    println(7)
                    0
                }
            "#,
        },
        // Concatenation is an unmodeled String operation.
        OffShapeCase {
            name: "concat_body",
            source: r#"
                func greet() -> string {
                    "a" + "b"
                }
                func main() -> i32 {
                    let s = greet()
                    println(7)
                    0
                }
            "#,
        },
        // A branchy String return floors through its nested blocks.
        OffShapeCase {
            name: "branchy_body",
            source: r#"
                func greet(c: bool) -> string {
                    if c {
                        "a"
                    } else {
                        "b"
                    }
                }
                func main() -> i32 {
                    let s = greet(true)
                    println(7)
                    0
                }
            "#,
        },
        // R6-1082: the second-hand rebind floor dropped island-wide — the
        // origin site (literal bind, parameter, member body) floors
        // independently and mixed is sticky, so a rebind never hides an
        // unadmitted origin.  The residue: a literal-origin rebind keeps
        // its floor at the origin bind, which is not a member body and
        // carries no print face.
        OffShapeCase {
            name: "literal_origin_rebind",
            source: r#"
                func main() -> i32 {
                    let a = "x"
                    let t = a
                    println(7)
                    0
                }
            "#,
        },
        // R6-1081: the call-bind face composes set members only — a
        // call-result bind of a leaky callee inherits no closed face, and
        // the bind pattern keeps its floor.
        OffShapeCase {
            name: "call_bind_over_leak",
            source: r#"
                func shaky() -> string {
                    let a = "x"
                    let b = "y"
                    a
                }
                func wrap() -> string {
                    let t = shaky()
                    t
                }
                func main() -> i32 {
                    let s = wrap()
                    println(7)
                    0
                }
            "#,
        },
        // R6-1079: the argument face admits only literal String arguments —
        // a second-hand String fed into an identity member has no
        // checker-side provenance and keeps the compatibility floor.
        OffShapeCase {
            name: "second_hand_argument",
            source: r#"
                func echo(s: string) -> string {
                    s
                }
                func main() -> i32 {
                    let a = "x"
                    let b = echo(a)
                    println(b)
                    0
                }
            "#,
        },
        // R6-1083: the parameter-rebind face is exactly one rebind of the
        // parameter — a second rebind hop is a second statement the shape
        // predicate does not constrain, so the body keeps the floor.
        OffShapeCase {
            name: "param_rebind_two_statement",
            source: r#"
                func chain2(s: string) -> string {
                    let v = s
                    let w = v
                    w
                }
                func main() -> i32 {
                    println(chain2("x"))
                    0
                }
            "#,
        },
        // R6-1083: a parameter-rebind shape over a non-parameter initializer
        // is not the face — the copy provenance must be the member's own
        // parameter for the end-of-body drop glue to discharge it.
        OffShapeCase {
            name: "param_rebind_non_param_origin",
            source: r#"
                func other() -> string {
                    let o = "o"
                    o
                }
                func chain3(s: string) -> string {
                    let v = other()
                    v
                }
                func main() -> i32 {
                    println(chain3("x"))
                    0
                }
            "#,
        },
    ];
    for case in CASES {
        let checked = checked_program_of(case.source);
        assert!(
            matches!(
                crate::core::mir::classify_scalar_collection_admission(&checked),
                crate::core::mir::ScalarCollectionAdmission::MixedCoverage
            ),
            "{} must classify mixed",
            case.name
        );
    }
}
