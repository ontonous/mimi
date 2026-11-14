//! The owned String List slice opens construction, Move, Clone and Drop only.

use super::*;
use crate::core::mir::reference::{MirReferenceInterpreter, MirRuntimeValue};
use crate::interp::bytecode::{compile_mir_program, BytecodeVM};

fn canonical_string_list_program(source: &str) -> MirProgram {
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    MirProgram::from_checked_program(&checked).expect("canonical owned String List MIR")
}

#[test]
fn owned_string_list_construct_clone_drop_agree_across_canonical_consumers() {
    let program = canonical_string_list_program(
        r#"
        trait Marker<T> { func read() -> i32; }
        impl Marker<i32> for List<i32> { func read() -> i32 { 11 } }
        impl Marker<string> for List<string> { func read() -> i32 { 22 } }
        impl Marker<i64> for List<i64> { func read() -> i32 { 33 } }
        func main() -> i32 {
            let text = "A\0雪"
            let empty: List<string> = []
            let words: List<string> = [text, "", "B\0木"]
            let copy = words
            let numbers: List<i32> = [1]
            let wide: List<i64> = [2]
            println(copy.read())
            drop(copy)
            println(words.read())
            println(numbers.read())
            println(wide.read())
            println(text)
            drop(empty)
            0
        }
    "#,
    );
    let owner = crate::core::NodeId("function:main".into());
    let expected = "22\n22\n11\n33\nA\0雪\n";
    let reference = MirReferenceInterpreter::new(&program)
        .execute_with_output(&owner, &[])
        .expect("reference String List lifecycle");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, expected);
    let mut vm = BytecodeVM::new(compile_mir_program(&program).expect("String List bytecode"));
    vm.enable_stdout_capture();
    assert!(matches!(
        vm.run_value().expect("String List VM lifecycle"),
        crate::interp::Value::Int(0)
    ));
    assert_eq!(vm.take_stdout(), expected);
    crate::verifier::validate_mir_capabilities(&program).expect("String List verifier capability");

    let context = Context::create();
    let mut generator = CodeGenerator::new(&context, "owned_string_list_lifecycle");
    generator
        .compile_mir_native(&program)
        .expect("String List native lifecycle");
    generator
        .module
        .verify()
        .expect("valid native String List IR");
    let ir = generator.emit_ir();
    for symbol in [
        "mimi_mir_list_new_string",
        "mimi_mir_list_push_string",
        "mimi_mir_list_clone_string",
        "mimi_mir_list_drop_string",
    ] {
        assert!(
            ir.lines()
                .any(|line| line.contains("call ") && line.contains(&format!("@{symbol}("))),
            "{symbol}"
        );
    }
    let clone_failure = ir
        .split("mir_string_list_clone_abort:")
        .nth(1)
        .and_then(|tail| tail.split("mir_string_list_clone_ok:").next())
        .expect("String List clone null branch");
    let source_drop = clone_failure
        .find("call void @mimi_mir_list_drop_string(")
        .expect("failed String List clone releases its still-live source");
    let trap = clone_failure
        .find("call void @mimi_runtime_abort(")
        .expect("failed String List clone enters the runtime trap");
    assert!(source_drop < trap, "source cleanup must precede the trap");
    assert_eq!(
        clone_failure
            .matches("call void @mimi_mir_list_drop_string(")
            .count(),
        1,
        "the null branch drops only the source; runtime clone already rolled back its partial output"
    );
    assert!(clone_failure.contains("unreachable"));
    assert!(ir.contains("canonical MIR List clone failed"));
    if crate::tests::can_link() {
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("String List native run");
        assert_eq!(native.stdout, expected);
        assert_eq!(native.stderr, "");
    }
}

#[test]
fn owned_string_list_mir_rejects_missing_and_forged_construction_receipts() {
    let program = canonical_string_list_program(
        r#"
        func main() -> i32 {
            let words: List<string> = ["A\0雪"]
            drop(words)
            0
        }
    "#,
    );
    for missing in [true, false] {
        let mut functions = program.functions().clone();
        let instruction = functions
            .values_mut()
            .flat_map(|function| function.blocks.values_mut())
            .flat_map(|block| block.instructions.iter_mut())
            .find(|instruction| {
                matches!(instruction.kind, MirInstructionKind::ConstructList { .. })
            })
            .expect("String List construction");
        let MirInstructionKind::ConstructList {
            list_construct_contract,
            ..
        } = &mut instruction.kind
        else {
            unreachable!()
        };
        if missing {
            *list_construct_contract = None;
        } else {
            list_construct_contract
                .as_mut()
                .expect("receipt")
                .element_count += 1;
        }
        let errors = MirProgram::with_type_catalog(functions, program.type_catalog().clone())
            .expect_err("malformed String List construction must fail before any consumer");
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("construction")
                    && error.message.contains("receipt")),
            "{errors:?}"
        );
    }
}

#[test]
fn owned_string_list_mir_rejects_forged_payload_and_container_ownership() {
    let program = canonical_string_list_program(
        r#"
        func main() -> i32 { let words: List<string> = ["A\0雪"]; drop(words); 0 }
    "#,
    );
    let (list_id, list_desc) = program
        .type_catalog()
        .iter()
        .find(|(ty, _)| {
            program
                .type_catalog()
                .validate_owned_string_list(ty)
                .is_ok()
        })
        .expect("owning String List descriptor");
    let MirLayout::List { element } = &list_desc.layout else {
        unreachable!()
    };
    for forgery in 0..4 {
        let mut catalog = program.type_catalog().clone();
        let (id, mut descriptor) = if forgery < 2 {
            (
                element.clone(),
                catalog.get(element).expect("String").clone(),
            )
        } else {
            (list_id.clone(), list_desc.clone())
        };
        match forgery {
            0 => descriptor.abi = MirAbiClass::OpaqueHandle,
            1 | 2 => {
                descriptor.ownership = MirOwnership::Copy;
                descriptor.glue = MirGlueContract {
                    move_out: MirGlueKind::Noop,
                    clone: MirGlueKind::Noop,
                    drop: MirGlueKind::Noop,
                };
            }
            _ => descriptor.glue.drop = MirGlueKind::Unsupported,
        }
        catalog.replace_for_test_only(id, descriptor);
        MirProgram::with_type_catalog(program.functions().clone(), catalog)
            .expect_err("forged String List ownership must fail before backend effects");
    }
}

#[test]
#[ignore = "strict native clone-OOM L3 requires cc and Valgrind"]
fn owned_string_list_native_clone_oom_rolls_back_and_drops_source_under_memcheck() {
    use std::process::Command;

    const SOURCE: &str = r#"
        func main() -> i32 {
            let words: List<string> = ["first", "abcdefghijklmnopqrstuvwxyz12345"]
            let copy = words
            drop(copy)
            drop(words)
            0
        }
    "#;

    let artifacts = std::env::var_os("MIMI_STRING_LIST_CLONE_OOM_L3_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "mimi-string-list-clone-oom-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system clock")
                    .as_nanos()
            ))
        });
    if let Some(parent) = artifacts.parent() {
        std::fs::create_dir_all(parent).expect("artifact parent directory");
    }
    std::fs::create_dir(&artifacts).expect("fresh native clone-OOM artifact directory");

    let c_source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/canonical_mir_string_list_clone_oom_harness.c");
    let c_object = artifacts.join("clone_oom_harness.o");
    let native_object = artifacts.join("canonical_mir.o");
    let native_binary = artifacts.join("canonical_mir_clone_oom");
    let runtime_archive = crate::tests::cached_runtime_lib().expect("production runtime archive");
    std::fs::copy(&c_source, artifacts.join("clone_oom_harness.c"))
        .expect("preserve C fault-injection fixture");
    std::fs::write(
        artifacts.join("runtime-archive.txt"),
        format!("{}\n", runtime_archive.display()),
    )
    .expect("preserve runtime archive identity");

    let program = canonical_string_list_program(SOURCE);
    let context = Context::create();
    let mut generator = CodeGenerator::new(&context, "canonical_mir_string_list_clone_oom");
    generator
        .compile_mir_native(&program)
        .expect("emit real canonical String List Clone MIR");
    generator
        .module
        .verify()
        .expect("native clone-OOM module verifies");
    let ir = generator.module.print_to_string().to_string();
    assert_eq!(
        ir.matches("call ptr @mimi_mir_list_clone_string(").count(),
        1,
        "the native fixture must reach one production String List Clone import"
    );
    let failure_block = ir
        .split("mir_string_list_clone_abort:")
        .nth(1)
        .and_then(|tail| tail.split("mir_string_list_clone_ok:").next())
        .expect("emitted String List null branch");
    assert!(
        failure_block.contains("call void @mimi_mir_list_drop_string("),
        "the MIR null branch must retire its still-owned source"
    );
    assert!(
        failure_block.contains("call void @mimi_runtime_abort("),
        "the MIR null branch must reach the real runtime trap"
    );
    generator
        .compile_to_object(&native_object)
        .expect("emit native object for isolated clone-OOM executable");

    let cc_compile = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-c"])
        .arg(&c_source)
        .arg("-o")
        .arg(&c_object)
        .output()
        .expect("start C fixture compiler");
    std::fs::write(&artifacts.join("cc-compile.stdout"), &cc_compile.stdout)
        .expect("preserve C compiler stdout");
    std::fs::write(&artifacts.join("cc-compile.stderr"), &cc_compile.stderr)
        .expect("preserve C compiler stderr");
    assert!(
        cc_compile.status.success(),
        "C fault-injection fixture did not compile: {}",
        String::from_utf8_lossy(&cc_compile.stderr)
    );

    let mut link = Command::new("cc");
    link.arg("-no-pie");
    for flag in crate::tests::linker_flag() {
        link.arg(flag);
    }
    for symbol in [
        "malloc",
        "free",
        "mimi_mir_list_clone_string",
        "mimi_mir_list_drop_string",
        "mimi_runtime_abort",
    ] {
        link.arg(format!("-Wl,--wrap={symbol}"));
    }
    link.arg(&native_object)
        .arg(&c_object)
        .arg(&runtime_archive)
        .arg("-o")
        .arg(&native_binary)
        .args(["-lpthread", "-ldl", "-lm"]);
    let link_argv = std::iter::once(link.get_program().to_string_lossy().into_owned())
        .chain(
            link.get_args()
                .map(|argument| argument.to_string_lossy().into_owned()),
        )
        .collect::<Vec<_>>();
    std::fs::write(
        artifacts.join("link-command.txt"),
        format!("{link_argv:#?}\n"),
    )
    .expect("preserve exact native link argv");
    let linked = link.output().expect("start native linker");
    std::fs::write(&artifacts.join("link.stdout"), &linked.stdout).expect("preserve linker stdout");
    std::fs::write(&artifacts.join("link.stderr"), &linked.stderr).expect("preserve linker stderr");
    assert!(
        linked.status.success(),
        "isolated native clone-OOM link failed: {}",
        String::from_utf8_lossy(&linked.stderr)
    );

    let native = Command::new(&native_binary)
        .output()
        .expect("run fault-injected production native executable");
    std::fs::write(&artifacts.join("native.stdout"), &native.stdout)
        .expect("preserve native stdout");
    std::fs::write(&artifacts.join("native.stderr"), &native.stderr)
        .expect("preserve native stderr");
    std::fs::write(
        &artifacts.join("native.status"),
        format!("{:?}\n", native.status),
    )
    .expect("preserve native process status");
    assert_eq!(
        native.status.code(),
        Some(1),
        "real MIR clone OOM must take the runtime's orderly E0800 trap: {}",
        String::from_utf8_lossy(&native.stderr)
    );
    let native_stderr = String::from_utf8_lossy(&native.stderr);
    assert!(
        native_stderr.contains(
            "string-list-clone-oom-native injected=1 rollback=clean source-drop=1 trap=E0800"
        ),
        "native wrapper did not witness clone rollback, MIR source Drop, and the real trap: {native_stderr}"
    );
    assert!(
        native_stderr.contains("canonical MIR List clone failed"),
        "the real MIR trap diagnostic is absent: {native_stderr}"
    );

    let valgrind_version = Command::new("valgrind")
        .arg("--version")
        .output()
        .expect("strict String List L3 requires Valgrind");
    std::fs::write(
        &artifacts.join("valgrind-version.txt"),
        format!(
            "status={:?}\n{}{}",
            valgrind_version.status,
            String::from_utf8_lossy(&valgrind_version.stdout),
            String::from_utf8_lossy(&valgrind_version.stderr)
        ),
    )
    .expect("preserve Valgrind identity");
    assert!(
        valgrind_version.status.success(),
        "strict String List L3 requires an executable Valgrind"
    );
    let memcheck_log = artifacts.join("valgrind.log");
    let valgrind_argv = vec![
        "valgrind".to_owned(),
        "--tool=memcheck".to_owned(),
        "--leak-check=full".to_owned(),
        "--show-leak-kinds=all".to_owned(),
        "--errors-for-leak-kinds=definite,indirect,possible".to_owned(),
        "--error-exitcode=91".to_owned(),
        "--track-origins=yes".to_owned(),
        format!("--log-file={}", memcheck_log.display()),
        native_binary.display().to_string(),
    ];
    std::fs::write(
        artifacts.join("valgrind-command.txt"),
        format!("{valgrind_argv:#?}\n"),
    )
    .expect("preserve exact Memcheck argv");
    let memcheck = Command::new(&valgrind_argv[0])
        .args(&valgrind_argv[1..])
        .output()
        .expect("run production native clone-OOM binary under Memcheck");
    std::fs::write(&artifacts.join("valgrind.stdout"), &memcheck.stdout)
        .expect("preserve Memcheck child stdout");
    std::fs::write(&artifacts.join("valgrind.stderr"), &memcheck.stderr)
        .expect("preserve Memcheck child stderr");
    std::fs::write(
        &artifacts.join("valgrind.status"),
        format!("{:?}\n", memcheck.status),
    )
    .expect("preserve Memcheck process status");
    let report = std::fs::read_to_string(&memcheck_log).expect("complete Memcheck log");
    assert_eq!(
        memcheck.status.code(),
        Some(1),
        "Memcheck must preserve the intentional E0800 child exit and report no tool error: {}\n{report}",
        String::from_utf8_lossy(&memcheck.stderr)
    );
    let memcheck_stderr = String::from_utf8_lossy(&memcheck.stderr);
    assert!(
        memcheck_stderr.contains(
            "string-list-clone-oom-native injected=1 rollback=clean source-drop=1 trap=E0800"
        ),
        "Memcheck child did not traverse the native clone-null cleanup branch: {memcheck_stderr}\n{report}"
    );
    let zero_loss = report.contains("All heap blocks were freed -- no leaks are possible")
        || [
            "definitely lost: 0 bytes",
            "indirectly lost: 0 bytes",
            "possibly lost: 0 bytes",
        ]
        .iter()
        .all(|line| report.contains(line));
    assert!(
        zero_loss && report.contains("ERROR SUMMARY: 0 errors"),
        "strict native clone-OOM Memcheck failed:\n{report}"
    );

    if std::env::var_os("MIMI_STRING_LIST_CLONE_OOM_L3_ARTIFACT_DIR").is_none() {
        std::fs::remove_dir_all(&artifacts).expect("remove temporary clone-OOM artifacts");
    }
}
