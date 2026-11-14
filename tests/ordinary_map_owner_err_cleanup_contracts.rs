#![cfg(feature = "ordinary-map-set-result-l3")]

//! Direct native ownership evidence for the exact owner-v2 dynamic String-key
//! fixture. The feature-gated root-publication failpoint drives successful,
//! New `Err(3)`, and Set `Err(2)` native executions; the C observer then checks
//! the settled runtime owner domains under Memcheck.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use inkwell::context::Context;
use mimi::codegen::CodeGenerator;
use mimi::core::mir::{
    reference::MirProgram, validate_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route,
};
use mimi::lexer::Lexer;

const FIXTURE: &str =
    include_str!("fixtures/mir_ordinary_map_any_owner_v2_dynamic_string_keys.mimi");
const FIXTURE_SHA256: &str = "79d307bf23c4aadfde8720d56447ea74ab01ff9b68390d124ff782f9000ecd66";
const ROUTE_PROFILE: &str = "ordinary-map-owned-any-v2-computed-string-key-owner-chain-v1";
const RUNTIME_FEATURE: &str = "ordinary-map-set-result-l3";
const GENERATED_ENTRY: &str = "mimi_owner_err_cleanup_generated_main";

const DRIVER_SOURCE: &str = r#"
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>

struct mimi_set_result_l3_observation {
    uint64_t set_calls;
    int32_t source_before_status;
    int32_t source_before_size;
    int32_t source_after_status;
    int32_t source_after_size;
    int32_t set_status;
    int32_t output_root_zero;
    uint64_t roots_after_set;
    uint64_t managed_any_after_set;
    uint64_t any_owners_after_set;
    uint64_t root_publish_attempts;
    int32_t shutdown_registered;
    uint64_t final_roots;
    uint64_t final_ordinary_map_handles;
    uint64_t final_owned_get_handles;
    uint64_t final_runtime_map_handles;
    uint64_t final_managed_any;
    uint64_t final_owned_strings;
    uint64_t final_any_owner_tokens;
    uint64_t final_lists;
};
_Static_assert(sizeof(struct mimi_set_result_l3_observation) == 136,
               "Rust/C owner observation ABI changed");

extern int32_t mimi_owner_err_cleanup_generated_main(void);
extern int32_t mimi_ordinary_map_set_result_l3_install_capacity_failure_after(
    uint64_t successful_publications);
extern int32_t mimi_ordinary_map_set_result_l3_read_observation(
    struct mimi_set_result_l3_observation *out_observation);

static int final_domains_are_zero(
    const struct mimi_set_result_l3_observation *observation) {
    return observation->final_roots == 0 &&
           observation->final_ordinary_map_handles == 0 &&
           observation->final_owned_get_handles == 0 &&
           observation->final_runtime_map_handles == 0 &&
           observation->final_managed_any == 0 &&
           observation->final_owned_strings == 0 &&
           observation->final_any_owner_tokens == 0 &&
           observation->final_lists == 0;
}

int main(void) {
#if MIMI_L3_FAILURE_AFTER >= 0
    if (mimi_ordinary_map_set_result_l3_install_capacity_failure_after(
            (uint64_t)MIMI_L3_FAILURE_AFTER) != 0) {
        fputs("owner-v2 root-publication failpoint installation failed\n", stderr);
        return 90;
    }
#endif

    const int32_t generated_status = mimi_owner_err_cleanup_generated_main();
    struct mimi_set_result_l3_observation observation = {0};
    const int32_t observation_status =
        mimi_ordinary_map_set_result_l3_read_observation(&observation);
#if MIMI_L3_FAILURE_AFTER == 0
    const int32_t expected_status = 3;
    const char *mode = "new-err";
#elif MIMI_L3_FAILURE_AFTER == 1
    const int32_t expected_status = 2;
    const char *mode = "set-err";
#else
    const int32_t expected_status = 0;
    const char *mode = "success";
#endif

    printf("native-owner-v2 mode=%s generated_status=%" PRId32
           " observation_status=%" PRId32 " shutdown_registered=%" PRId32
           " final_roots=%" PRIu64 " final_map_handles=%" PRIu64
           " final_owned_get_handles=%" PRIu64
           " final_runtime_map_handles=%" PRIu64 " final_managed_any=%" PRIu64
           " final_owned_strings=%" PRIu64 " final_any_owner_tokens=%" PRIu64
           " final_lists=%" PRIu64 "\n",
           mode, generated_status, observation_status,
           observation.shutdown_registered, observation.final_roots,
           observation.final_ordinary_map_handles,
           observation.final_owned_get_handles,
           observation.final_runtime_map_handles, observation.final_managed_any,
           observation.final_owned_strings, observation.final_any_owner_tokens,
           observation.final_lists);

    if (generated_status != expected_status || observation_status != 0 ||
        observation.shutdown_registered != 1 ||
        !final_domains_are_zero(&observation)) {
        fputs("owner-v2 source route did not settle every runtime owner domain\n",
              stderr);
        return 91;
    }
    return 0;
}
"#;

#[test]
fn computed_string_key_source_err_releases_every_owner_domain() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_path =
        repo_root.join("tests/fixtures/mir_ordinary_map_any_owner_v2_dynamic_string_keys.mimi");
    let on_disk_fixture = fs::read(&fixture_path).expect("read exact owner-v2 fixture");
    assert_eq!(
        on_disk_fixture,
        FIXTURE.as_bytes(),
        "fixture changed after test compilation"
    );
    assert_eq!(
        sha256(FIXTURE.as_bytes()),
        FIXTURE_SHA256,
        "fixture source hash changed"
    );

    let root = unique_temp_dir();
    fs::create_dir(&root).expect("create isolated native test directory");
    let object_path = root.join("owner-v2-native.o");
    let runtime_archive = root.join("libmimi_runtime_owner_err_cleanup.a");
    let driver_source = root.join("owner-v2-driver.c");
    fs::write(&driver_source, DRIVER_SOURCE).expect("write native observation driver");

    let receipt = compile_exact_fixture(&fixture_path, &object_path);
    assert_eq!(
        receipt.profile, ROUTE_PROFILE,
        "native route profile changed"
    );

    compile_runtime_archive(&repo_root, &runtime_archive);
    for (mode, failure_after, expected_stdout) in [
        (
            "success",
            -1,
            "true\ntrue\nfalse\nnative-owner-v2 mode=success generated_status=0 observation_status=0 shutdown_registered=1 final_roots=0 final_map_handles=0 final_owned_get_handles=0 final_runtime_map_handles=0 final_managed_any=0 final_owned_strings=0 final_any_owner_tokens=0 final_lists=0\n",
        ),
        (
            "new-err",
            0,
            "native-owner-v2 mode=new-err generated_status=3 observation_status=0 shutdown_registered=1 final_roots=0 final_map_handles=0 final_owned_get_handles=0 final_runtime_map_handles=0 final_managed_any=0 final_owned_strings=0 final_any_owner_tokens=0 final_lists=0\n",
        ),
        (
            "set-err",
            1,
            "native-owner-v2 mode=set-err generated_status=2 observation_status=0 shutdown_registered=1 final_roots=0 final_map_handles=0 final_owned_get_handles=0 final_runtime_map_handles=0 final_managed_any=0 final_owned_strings=0 final_any_owner_tokens=0 final_lists=0\n",
        ),
    ] {
        let driver_object = root.join(format!("owner-v2-driver-{mode}.o"));
        let executable = root.join(format!("owner-v2-{mode}"));
        compile_driver(&driver_source, &driver_object, failure_after, &root);
        link_native(&object_path, &driver_object, &runtime_archive, &executable, &root);
        let memcheck_log = root.join(format!("owner-v2-{mode}-memcheck.log"));
        let output = Command::new("valgrind")
            .arg("--tool=memcheck")
            .arg("--leak-check=full")
            .arg("--show-leak-kinds=all")
            .arg("--errors-for-leak-kinds=definite,indirect,possible")
            .arg("--error-exitcode=91")
            .arg("--track-origins=yes")
            .arg(format!("--log-file={}", memcheck_log.display()))
            .arg(&executable)
            .output()
            .unwrap_or_else(|error| panic!("run native {mode} route under Memcheck: {error}"));
        assert!(
            output.status.success(),
            "native {mode} route failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            expected_stdout,
            "native {mode} output or final ownership census changed"
        );
        assert!(
            output.stderr.is_empty(),
            "native {mode} route wrote unexpected stderr under Memcheck: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let memcheck = fs::read_to_string(&memcheck_log)
            .unwrap_or_else(|error| panic!("read {mode} Memcheck log: {error}"));
        let error_summaries = memcheck
            .lines()
            .filter(|line| line.contains("ERROR SUMMARY:"))
            .collect::<Vec<_>>();
        assert!(
            error_summaries.len() == 1
                && error_summaries[0]
                    .contains("ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)"),
            "Memcheck found errors in native {mode} route:\n{memcheck}"
        );
        let all_heap_blocks_freed =
            memcheck.contains("All heap blocks were freed -- no leaks are possible");
        for leak_marker in ["definitely lost:", "indirectly lost:", "possibly lost:"] {
            let leak_lines = memcheck
                .lines()
                .filter(|line| line.contains(leak_marker))
                .collect::<Vec<_>>();
            let zero_reported = leak_lines.len() == 1
                && leak_lines[0].contains("0 bytes in 0 blocks");
            assert!(
                zero_reported || (leak_lines.is_empty() && all_heap_blocks_freed),
                "Memcheck did not prove zero {leak_marker} allocations for native {mode} route:\n{memcheck}"
            );
        }
    }

    fs::remove_dir_all(root).expect("remove isolated native test directory");
}

fn compile_exact_fixture(
    fixture_path: &Path,
    object_path: &Path,
) -> mimi::core::mir::CanonicalMirRouteReceipt {
    let tokens = Lexer::new(FIXTURE)
        .tokenize()
        .expect("lex exact source fixture");
    let (mut file, parse_errors) = mimi::loader::parser_for_path(tokens, fixture_path)
        .expect("create parser for exact source fixture")
        .parse_file_with_recovery();
    assert!(
        parse_errors.is_empty(),
        "fixture parse errors: {parse_errors:?}"
    );
    mimi::loader::merge_prelude_into(&mut file);
    if let Some(main_index) = file
        .items
        .iter()
        .position(|item| matches!(item, mimi::ast::Item::Func(function) if function.name == "main"))
    {
        let main_item = file.items.remove(main_index);
        file.items.push(main_item);
    }
    let checked = mimi::core::check_program(&file).expect("Checker accepts exact fixture");
    let program =
        MirProgram::from_checked_program(&checked).expect("materialize exact fixture MIR");
    let receipt = validate_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route(
        &checked, &program,
    )
    .expect("exact Checker-bound computed String-key owner-chain route");
    assert_eq!(receipt.profile, ROUTE_PROFILE);
    receipt
        .validate_against_program(&program)
        .expect("bind exact route receipt to canonical MIR");

    let prior_opt = std::env::var_os("MIMI_OPT");
    let prior_dump_module = std::env::var_os("MIMI_DUMP_MODULE");
    std::env::set_var("MIMI_OPT", "0");
    std::env::remove_var("MIMI_DUMP_MODULE");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "owner_v2_err_cleanup_native");
        generator
            .compile_checked_mir_native_with_computed_string_key_owner_chain_route(
                &checked, &program, &receipt,
            )
            .unwrap_or_else(|errors| panic!("emit exact native source route: {errors:?}"));
        generator
            .module
            .verify()
            .expect("verify exact native LLVM module");
        let main = generator
            .module
            .get_function("main")
            .expect("native source route has main");
        assert_eq!(main.count_params(), 0);
        assert_eq!(
            main.get_type().get_return_type(),
            Some(context.i32_type().into())
        );
        rename_function(&main, GENERATED_ENTRY);
        generator
            .module
            .verify()
            .expect("verify renamed native source route");
        generator
            .compile_to_object(object_path)
            .unwrap_or_else(|error| panic!("write exact native source object: {error}"));
    }));
    match prior_opt {
        Some(value) => std::env::set_var("MIMI_OPT", value),
        None => std::env::remove_var("MIMI_OPT"),
    }
    match prior_dump_module {
        Some(value) => std::env::set_var("MIMI_DUMP_MODULE", value),
        None => std::env::remove_var("MIMI_DUMP_MODULE"),
    }
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
    receipt
}

fn compile_runtime_archive(repo_root: &Path, archive_path: &Path) {
    run_checked(
        Command::new("rustc")
            .current_dir(repo_root)
            .arg("--edition")
            .arg("2021")
            .arg("--crate-type")
            .arg("staticlib")
            .arg("--cfg")
            .arg("standalone")
            .arg("--cfg")
            .arg(format!("feature=\"{RUNTIME_FEATURE}\""))
            .arg("--crate-name")
            .arg("mimi_runtime")
            .arg("-A")
            .arg("dead_code")
            .arg("-o")
            .arg(archive_path)
            .arg(repo_root.join("src/runtime/standalone.rs")),
        "build feature-gated standalone runtime archive",
    );
}

fn compile_driver(source: &Path, object: &Path, failure_after: i32, repo_root: &Path) {
    run_checked(
        Command::new("cc")
            .current_dir(repo_root)
            .arg("-std=c11")
            .arg("-Wall")
            .arg("-Wextra")
            .arg("-Werror")
            .arg(format!("-DMIMI_L3_FAILURE_AFTER={failure_after}"))
            .arg("-c")
            .arg(source)
            .arg("-o")
            .arg(object),
        "compile native observation driver",
    );
}

fn link_native(object: &Path, driver: &Path, archive: &Path, executable: &Path, repo_root: &Path) {
    run_checked(
        Command::new("cc")
            .current_dir(repo_root)
            .arg("-no-pie")
            .arg(object)
            .arg(driver)
            .arg(archive)
            .arg("-o")
            .arg(executable)
            .arg("-lpthread")
            .arg("-ldl")
            .arg("-lm"),
        "link native source route and feature-gated runtime",
    );
}

fn run_checked(command: &mut Command, operation: &str) {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{operation}: could not start command: {error}"));
    assert!(
        output.status.success(),
        "{operation} failed: {:?}\nstdout:\n{}\nstderr:\n{}",
        command,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn rename_function(function: &inkwell::values::FunctionValue<'_>, name: &str) {
    use inkwell::llvm_sys::core::LLVMSetValueName2;
    use inkwell::values::AsValueRef;

    // SAFETY: this live function belongs to the current LLVM module, and LLVM
    // copies the supplied byte slice before returning.
    unsafe {
        LLVMSetValueName2(
            function.as_value_ref(),
            name.as_ptr() as *const std::os::raw::c_char,
            name.len(),
        );
    }
}

fn sha256(bytes: &[u8]) -> String {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("sha256sum is available for exact fixture identity");
    child
        .stdin
        .take()
        .expect("sha256sum stdin is piped")
        .write_all(bytes)
        .expect("write exact fixture bytes to sha256sum");
    let output = child.wait_with_output().expect("read sha256sum output");
    assert!(output.status.success(), "sha256sum failed");
    String::from_utf8(output.stdout)
        .expect("sha256sum output is UTF-8")
        .split_whitespace()
        .next()
        .expect("sha256sum prints a digest")
        .to_owned()
}

fn unique_temp_dir() -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "mimi-owner-err-cleanup-{}-{nonce}",
        std::process::id()
    ))
}
