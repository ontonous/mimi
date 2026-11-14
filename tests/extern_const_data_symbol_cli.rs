use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn mimi_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_mimi") {
        return PathBuf::from(path);
    }
    std::env::current_exe()
        .ok()
        .and_then(|test| {
            test.parent()
                .and_then(Path::parent)
                .map(|dir| dir.join("mimi"))
        })
        .filter(|path| path.is_file())
        .expect("Cargo must provide the mimi binary for the native data export test")
}

fn temp_dir(label: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "mimi-native-data-export-{label}-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create native data export test directory");
    dir
}

fn build_shared(source: &Path, output: &Path, explicit_mir: bool) -> Output {
    let mut command = Command::new(mimi_bin());
    command.current_dir(env!("CARGO_MANIFEST_DIR")).arg("build");
    if explicit_mir {
        command.arg("--mir");
    }
    command
        .arg("--shared")
        .arg(source)
        .arg("-o")
        .arg(output)
        .env("MIMI_ROUTE_CENSUS", "1")
        .output()
        .expect("spawn Mimi shared-library build")
}

fn run_mimi(source: &Path, explicit_mir: bool) -> Output {
    let mut command = Command::new(mimi_bin());
    command.current_dir(env!("CARGO_MANIFEST_DIR")).arg("run");
    if explicit_mir {
        command.arg("--mir");
    }
    command
        .arg(source)
        .env("MIMI_ROUTE_CENSUS", "1")
        .output()
        .expect("spawn Mimi program run")
}

fn assert_memcheck_clean(consumer: &Path, library: &Path, label: &str) {
    let Some(evidence_dir) = std::env::var_os("MIMI_NATIVE_DATA_EXPORT_MEMCHECK_DIR") else {
        return;
    };
    let evidence_dir = PathBuf::from(evidence_dir);
    std::fs::create_dir_all(&evidence_dir).expect("create native data export Memcheck evidence");

    let mut command = Command::new("valgrind");
    command
        .args([
            "--leak-check=full",
            "--show-leak-kinds=all",
            "--errors-for-leak-kinds=definite,indirect,possible",
            "--error-exitcode=97",
        ])
        .arg(consumer)
        .arg(library)
        .env("LC_ALL", "C");
    let rendered_command = format!("{command:?}");
    let output = command.output().expect("spawn Valgrind for C consumer");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    std::fs::write(
        evidence_dir.join(format!("{label}.command.txt")),
        rendered_command,
    )
    .expect("write Memcheck command evidence");
    std::fs::write(
        evidence_dir.join(format!("{label}.stdout")),
        stdout.as_bytes(),
    )
    .expect("write Memcheck stdout evidence");
    std::fs::write(
        evidence_dir.join(format!("{label}.stderr")),
        stderr.as_bytes(),
    )
    .expect("write Memcheck stderr evidence");
    std::fs::write(
        evidence_dir.join(format!("{label}.exit")),
        output.status.code().unwrap_or(128).to_string(),
    )
    .expect("write Memcheck exit status evidence");

    assert!(
        output.status.success(),
        "{label} C consumer failed under Valgrind: {stderr}"
    );
    let all_freed = stderr.contains("All heap blocks were freed -- no leaks are possible");
    let zero_reported_leaks = ["definitely lost", "indirectly lost", "possibly lost"]
        .iter()
        .all(|leak_class| stderr.contains(&format!("{leak_class}: 0 bytes in 0 blocks")));
    assert!(
        all_freed || zero_reported_leaks,
        "{label} Valgrind output does not prove zero definite, indirect, and possible leaks: {stderr}"
    );
    assert!(
        stderr.contains("ERROR SUMMARY: 0 errors"),
        "{label} has a Valgrind error: {stderr}"
    );
}

fn parse_census_fields(stderr: &str, prefix: &str, label: &str) -> BTreeMap<String, String> {
    let records = stderr
        .lines()
        .filter(|line| line.starts_with(prefix))
        .collect::<Vec<_>>();
    assert_eq!(
        records.len(),
        1,
        "{label} must emit exactly one {prefix:?} record:\n{stderr}"
    );
    let payload = records[0]
        .strip_prefix(prefix)
        .expect("record has the requested prefix");
    let mut fields = BTreeMap::new();
    for token in payload.split_whitespace() {
        let (key, value) = token
            .split_once('=')
            .unwrap_or_else(|| panic!("{label} has malformed census field {token:?}"));
        assert!(
            fields.insert(key.to_owned(), value.to_owned()).is_none(),
            "{label} repeats census field {key}"
        );
    }
    fields
}

fn assert_canonical_native_census(
    output: &Output,
    label: &str,
    expected_mode: &str,
) -> (String, String, String) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{label} selected the legacy route:\n{stderr}"
    );

    let admission = parse_census_fields(&stderr, "MIMI_ROUTE_CENSUS route-admission ", label);
    assert_eq!(admission.get("consumer").map(String::as_str), Some("build"));
    assert_eq!(
        admission.get("mode").map(String::as_str),
        Some(expected_mode)
    );
    assert_eq!(
        admission.get("checker_profile").map(String::as_str),
        Some("native-data-export-v1")
    );
    assert_eq!(
        admission.get("build_route_profile").map(String::as_str),
        Some("native-data-export-v1")
    );

    let native = parse_census_fields(&stderr, "MIMI_ROUTE_CENSUS canonical-native-build ", label);
    assert_eq!(
        native.get("consumer").map(String::as_str),
        Some(expected_mode)
    );
    assert_eq!(
        native.get("route_id").map(String::as_str),
        Some("native-data-export-v1")
    );
    assert_eq!(
        native.get("profile").map(String::as_str),
        Some("native-data-export-v1")
    );
    assert_eq!(
        native.get("build_route_profile").map(String::as_str),
        Some("native-data-export-v1")
    );
    let digest = native
        .get("mir_digest")
        .expect("canonical native build receipt contains the MIR digest");
    assert!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{label} has malformed MIR digest {digest:?}"
    );
    assert_eq!(
        admission.get("mir_digest"),
        Some(digest),
        "{label} route admission and native build must use the same MIR graph"
    );
    let admission_identity = admission
        .get("receipt_identity_digest")
        .expect("route admission contains the complete build identity");

    let summaries = stderr
        .lines()
        .filter(|line| line.starts_with("info: canonical MIR native compiled "))
        .collect::<Vec<_>>();
    assert_eq!(
        summaries.len(),
        1,
        "{label} must emit one complete canonical native census:\n{stderr}"
    );
    let (counts, native_digest) = summaries[0]
        .strip_prefix("info: canonical MIR native compiled ")
        .expect("native summary has its stable prefix")
        .split_once(" function(s), 0 fell back; mir_digest=")
        .expect("native summary proves zero fallback and includes the MIR digest");
    let (emitted, expected) = counts
        .split_once('/')
        .expect("native summary reports emitted and expected function counts");
    let emitted = emitted
        .parse::<usize>()
        .expect("native emitted function count is numeric");
    let expected = expected
        .parse::<usize>()
        .expect("native expected function count is numeric");
    assert_eq!(emitted, 2, "{label} must emit both Mimi functions");
    assert_eq!(
        expected, 2,
        "{label} MIR graph must contain both Mimi functions"
    );
    assert_eq!(
        native_digest, digest,
        "{label} native census MIR digest differs"
    );
    let native_identity = native
        .get("route_identity_digest")
        .expect("native build census contains the complete build identity");
    assert_eq!(
        admission_identity, native_identity,
        "{label} admission and native consumer must share the complete route identity"
    );

    let data_export = parse_census_fields(&stderr, "MIMI_ROUTE_CENSUS native-data-export ", label);
    assert_eq!(
        data_export.get("schema").map(String::as_str),
        Some("mimi-native-data-export-table-receipt-v1")
    );
    assert_eq!(
        data_export.get("export_count").map(String::as_str),
        Some("6"),
        "{label} must bind all six C-visible data symbols"
    );
    let table_digest = data_export
        .get("table_digest")
        .expect("native data-export receipt contains its canonical table digest");
    assert!(
        table_digest.len() == 64 && table_digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{label} has malformed native data-export digest {table_digest:?}"
    );
    assert_eq!(
        data_export.get("route_identity_digest"),
        Some(native_identity),
        "{label} data table and MIR graph must form the reported native route identity"
    );

    let owners = stderr
        .lines()
        .filter_map(|line| {
            line.strip_prefix("info: canonical MIR native emitted owner='")
                .and_then(|owner| owner.strip_suffix('\''))
        })
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let expected_owners =
        BTreeSet::from(["function:main".to_owned(), "function:read_f32".to_owned()]);
    assert_eq!(
        owners, expected_owners,
        "{label} must emit every expected Mimi function exactly once:\n{stderr}"
    );
    (
        digest.clone(),
        native_identity.clone(),
        table_digest.clone(),
    )
}

#[test]
#[cfg(target_os = "linux")]
fn scalar_extern_const_data_symbols_reach_default_and_explicit_mir_shared_libraries() {
    let dir = temp_dir("positive");
    let source = dir.join("exports.mimi");
    let source_text = r#"
extern "C" const DATA_I32: i32 = -12345
extern "C" const DATA_I64: i64 = 1234567890123
extern "C" const DATA_F32: f32 = 0.1
extern "C" const DATA_F64: f64 = -2.5
extern "C" const DATA_BOOL: bool = true
type DataAliasI32 = i32
extern "C" const DATA_ALIAS_I32: DataAliasI32 = 64
func read_f32() -> f32 { DATA_F32 }
func main() -> i32 { DATA_I32 }
"#;
    std::fs::write(&source, source_text).expect("write scalar export source");

    let mut run_receipts = None;
    for (mode, explicit_mir) in [("default", false), ("explicit-mir", true)] {
        let run = run_mimi(&source, explicit_mir);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert_eq!(
            run.status.code(),
            Some((-12345i32) as u8 as i32),
            "{mode} bytecode run must return the checker-owned DATA_I32 value; stderr: {stderr}"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy"),
            "{mode} run fell back to the compatibility route: {stderr}"
        );
        let admission = parse_census_fields(&stderr, "MIMI_ROUTE_CENSUS route-admission ", mode);
        assert_eq!(
            admission.get("checker_profile").map(String::as_str),
            Some("native-data-export-v1")
        );
        let receipts = (
            admission.get("mir_digest").expect("run MIR digest").clone(),
            admission
                .get("receipt_identity_digest")
                .expect("run route identity")
                .clone(),
        );
        if let Some(first) = &run_receipts {
            assert_eq!(
                &receipts, first,
                "default and explicit --mir runs must share one MIR route identity"
            );
        } else {
            run_receipts = Some(receipts);
        }
    }

    let consumer_source = dir.join("consumer.c");
    let consumer_source_text = r#"
#include <dlfcn.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv) {
    if (argc < 2 || argc > 3) return 2;
    void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!library) { fprintf(stderr, "dlopen: %s\n", dlerror()); return 3; }

    int32_t *i32 = (int32_t *)dlsym(library, "DATA_I32");
    int64_t *i64 = (int64_t *)dlsym(library, "DATA_I64");
    int32_t *alias_i32 = (int32_t *)dlsym(library, "DATA_ALIAS_I32");
    float *f32 = (float *)dlsym(library, "DATA_F32");
    double *f64 = (double *)dlsym(library, "DATA_F64");
    bool *boolean = (bool *)dlsym(library, "DATA_BOOL");
    float (*mimi_read_f32)(void) = (float (*)(void))dlsym(library, "read_f32");
    int (*mimi_main)(void) = (int (*)(void))dlsym(library, "main");
    if (!i32 || !i64 || !alias_i32 || !f32 || !f64 || !boolean || !mimi_read_f32 || !mimi_main) {
        fprintf(stderr, "missing data symbol: %s\n", dlerror());
        return 4;
    }
    if (argc == 3 && strcmp(argv[2], "mutate") == 0) {
        *(volatile int32_t *)i32 = 7;
        return 8;
    }
    if (*i32 != -12345 || *i64 != INT64_C(1234567890123) || *alias_i32 != 64
        || *f32 != 0.1f || *f64 != -2.5 || !*boolean) {
        fprintf(stderr, "wrong exported scalar value\n");
        return 5;
    }
    if (mimi_read_f32() != *f32) {
        fprintf(stderr, "Mimi function result disagreed with exported DATA_F32\n");
        return 6;
    }
    if (mimi_main() != -12345) {
        fprintf(stderr, "Mimi main did not read DATA_I32\n");
        return 7;
    }
    dlclose(library);
    return 0;
}
"#;
    std::fs::write(&consumer_source, consumer_source_text).expect("write C dlsym consumer");

    let consumer = dir.join("consumer");
    let cc = Command::new("cc")
        .arg(&consumer_source)
        .arg("-ldl")
        .arg("-o")
        .arg(&consumer)
        .output()
        .expect("compile C dlsym consumer");
    assert!(
        cc.status.success(),
        "C consumer did not compile: {}",
        String::from_utf8_lossy(&cc.stderr)
    );

    let mut initial_receipts = None;
    for (mode, explicit_mir) in [("default", false), ("explicit-mir", true)] {
        let library = dir.join(format!("libexports-{mode}.so"));
        let build = build_shared(&source, &library, explicit_mir);
        assert!(
            build.status.success(),
            "{mode} shared build failed: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        let expected_route_mode = if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        };
        let receipts = assert_canonical_native_census(&build, mode, expected_route_mode);
        if let Some(initial) = &initial_receipts {
            assert_eq!(
                &receipts, initial,
                "default and explicit --mir builds must bind the same MIR graph and export table"
            );
        } else {
            initial_receipts = Some(receipts);
        }

        let symbols = Command::new("readelf")
            .args(["-Ws"])
            .arg(&library)
            .output()
            .expect("read ELF dynamic symbols");
        assert!(symbols.status.success(), "readelf failed");
        let symbols = String::from_utf8_lossy(&symbols.stdout);
        for (name, size) in [
            ("DATA_I32", 4),
            ("DATA_I64", 8),
            ("DATA_ALIAS_I32", 4),
            ("DATA_F32", 4),
            ("DATA_F64", 8),
            ("DATA_BOOL", 1),
        ] {
            let symbol = symbols.lines().find(|line| {
                let fields = line.split_whitespace().collect::<Vec<_>>();
                fields.len() >= 8
                    && fields[2] == size.to_string()
                    && fields[3] == "OBJECT"
                    && fields[4] == "GLOBAL"
                    && fields[6] != "UND"
                    && fields[7] == name
            });
            assert!(
                symbol.is_some(),
                "{mode} must export {name} as a {size}-byte global OBJECT; ELF table:\n{symbols}"
            );
        }

        let consumed = Command::new(&consumer)
            .arg(&library)
            .output()
            .expect("run C dlsym consumer");
        assert!(
            consumed.status.success(),
            "{mode} C consumer failed: {}",
            String::from_utf8_lossy(&consumed.stderr)
        );
        assert_memcheck_clean(&consumer, &library, &format!("{mode}-c-consumer"));

        let mutation = Command::new(&consumer)
            .arg(&library)
            .arg("mutate")
            .status()
            .expect("attempt C mutation of exported const");
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&mutation),
            Some(11),
            "{mode} data export must reside in read-only storage; mutation status was {mutation}"
        );
    }

    // DATA_I64 is deliberately not read by any Mimi function. Changing only
    // its exported value must leave the executable MIR digest unchanged while
    // changing both the source-bound table receipt and the composed native
    // route identity.
    let initial_receipts = initial_receipts.expect("both build modes ran");
    let changed_source_text = source_text.replace("1234567890123", "1234567890124");
    std::fs::write(&source, changed_source_text).expect("rewrite one unreferenced export value");
    let changed_library = dir.join("libexports-data-drift.so");
    let changed_build = build_shared(&source, &changed_library, false);
    assert!(
        changed_build.status.success(),
        "data-only value change failed to build: {}",
        String::from_utf8_lossy(&changed_build.stderr)
    );
    let changed_receipts =
        assert_canonical_native_census(&changed_build, "changed-data-value", "default");
    assert_eq!(
        changed_receipts.0, initial_receipts.0,
        "an unreferenced data symbol must not alter the executable MIR graph"
    );
    assert_ne!(
        changed_receipts.2, initial_receipts.2,
        "changing an exported data value must alter the source-bound table digest"
    );
    assert_ne!(
        changed_receipts.1, initial_receipts.1,
        "changing an exported data value must alter the composed native route identity"
    );

    let changed_consumer_source = dir.join("consumer-data-drift.c");
    let changed_consumer_text = consumer_source_text.replace("1234567890123", "1234567890124");
    std::fs::write(&changed_consumer_source, changed_consumer_text)
        .expect("write changed-value C consumer");
    let changed_consumer = dir.join("consumer-data-drift");
    let changed_cc = Command::new("cc")
        .arg(&changed_consumer_source)
        .arg("-ldl")
        .arg("-o")
        .arg(&changed_consumer)
        .output()
        .expect("compile changed-value C consumer");
    assert!(
        changed_cc.status.success(),
        "changed-value C consumer did not compile: {}",
        String::from_utf8_lossy(&changed_cc.stderr)
    );
    let changed_consumed = Command::new(&changed_consumer)
        .arg(&changed_library)
        .output()
        .expect("run changed-value C consumer");
    assert!(
        changed_consumed.status.success(),
        "changed-value C consumer failed to observe changed exported data: {}",
        String::from_utf8_lossy(&changed_consumed.stderr)
    );
    assert_memcheck_clean(
        &changed_consumer,
        &changed_library,
        "changed-data-value-c-consumer",
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[cfg(target_os = "linux")]
fn data_exports_composed_with_scalar_export_use_the_same_profile_for_both_build_modes() {
    let dir = temp_dir("scalar-export-composition");
    let source = dir.join("composed.mimi");
    std::fs::write(
        &source,
        r#"
extern "C" const DATA_I32: i32 = 41
extern "C" func exported_read() -> i32 { DATA_I32 }
func main() -> i32 { exported_read() }
"#,
    )
    .expect("write scalar C export plus data export source");

    let consumer_source = dir.join("consumer.c");
    std::fs::write(
        &consumer_source,
        r#"
#include <dlfcn.h>
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    void *handle = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!handle) return 3;
    int *data = (int *)dlsym(handle, "DATA_I32");
    int (*read_value)(void) = (int (*)(void))dlsym(handle, "exported_read");
    if (!data || !read_value || *data != 41 || read_value() != 41) return 4;
    dlclose(handle);
    return 0;
}
"#,
    )
    .expect("write scalar-export/data C consumer");
    let consumer = dir.join("consumer");
    let cc = Command::new("cc")
        .arg(&consumer_source)
        .arg("-ldl")
        .arg("-o")
        .arg(&consumer)
        .output()
        .expect("compile scalar-export/data C consumer");
    assert!(
        cc.status.success(),
        "scalar-export/data C consumer did not compile: {}",
        String::from_utf8_lossy(&cc.stderr)
    );

    let mut common_receipts = None;
    for (mode, explicit_mir) in [("default", false), ("explicit-mir", true)] {
        let library = dir.join(format!("libcomposed-{mode}.so"));
        let build = build_shared(&source, &library, explicit_mir);
        let stderr = String::from_utf8_lossy(&build.stderr);
        assert!(
            build.status.success(),
            "{mode} composed route failed: {stderr}"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy"),
            "{mode} composed route may not fall back: {stderr}"
        );

        let admission = parse_census_fields(&stderr, "MIMI_ROUTE_CENSUS route-admission ", mode);
        assert_eq!(
            admission.get("mode").map(String::as_str),
            Some(if explicit_mir {
                "explicit-mir"
            } else {
                "default"
            })
        );
        assert_eq!(
            admission.get("checker_profile").map(String::as_str),
            Some("scalar-export-v1")
        );
        assert_eq!(
            admission.get("build_route_profile").map(String::as_str),
            Some("scalar-export-v1+native-data-export-v1")
        );

        let native =
            parse_census_fields(&stderr, "MIMI_ROUTE_CENSUS canonical-native-build ", mode);
        assert_eq!(
            native.get("route_id").map(String::as_str),
            Some("scalar-export-v1")
        );
        assert_eq!(
            native.get("profile").map(String::as_str),
            Some("scalar-export-v1")
        );
        assert_eq!(
            native.get("build_route_profile").map(String::as_str),
            Some("scalar-export-v1+native-data-export-v1")
        );
        assert_eq!(admission.get("mir_digest"), native.get("mir_digest"));
        assert_eq!(
            admission.get("receipt_identity_digest"),
            native.get("route_identity_digest"),
            "both consumers must share the composed build identity"
        );

        let data = parse_census_fields(&stderr, "MIMI_ROUTE_CENSUS native-data-export ", mode);
        assert_eq!(data.get("export_count").map(String::as_str), Some("1"));
        assert_eq!(
            data.get("build_route_profile").map(String::as_str),
            Some("scalar-export-v1+native-data-export-v1")
        );
        assert_eq!(
            data.get("route_identity_digest"),
            native.get("route_identity_digest"),
            "the scalar-export MIR and C-visible data table must share one identity"
        );

        let consumed = Command::new(&consumer)
            .arg(&library)
            .output()
            .expect("run scalar-export/data C consumer");
        assert!(
            consumed.status.success(),
            "{mode} C consumer could not read the data symbol and call the exported function"
        );
        assert_memcheck_clean(
            &consumer,
            &library,
            &format!("scalar-export-{mode}-c-consumer"),
        );

        let receipts = (
            native.get("mir_digest").expect("MIR digest").clone(),
            native
                .get("route_identity_digest")
                .expect("composed route identity")
                .clone(),
            data.get("table_digest").expect("table digest").clone(),
        );
        if let Some(first) = &common_receipts {
            assert_eq!(
                &receipts, first,
                "default and explicit --mir must select the same scalar C export route"
            );
        } else {
            common_receipts = Some(receipts);
        }
    }

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn data_exports_with_opaque_scalar_ffi_effect_fail_closed_for_both_build_modes() {
    let dir = temp_dir("scalar-ffi-opaque-effect");
    let source = dir.join("opaque-ffi.mimi");
    std::fs::write(
        &source,
        r#"
extern "C" const DATA_I32: i32 = 41
extern "C" { func foreign_plus_one(value: i32) -> i32; }
func main() -> i32 { foreign_plus_one(DATA_I32) }
"#,
    )
    .expect("write data export plus opaque scalar FFI source");

    for (mode, explicit_mir) in [("default", false), ("explicit-mir", true)] {
        let library = dir.join(format!("opaque-{mode}.so"));
        let build = build_shared(&source, &library, explicit_mir);
        assert!(
            !build.status.success(),
            "{mode} must reject an FFI call whose effect summary is unknown"
        );
        let stderr = String::from_utf8_lossy(&build.stderr);
        assert!(
            stderr.contains("MIR-COVERAGE-001") && stderr.contains("unknown effect"),
            "{mode} must preserve the exact fail-closed effect boundary: {stderr}"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy"),
            "{mode} rejection must not select a compatibility fallback: {stderr}"
        );
        assert!(
            !library.exists(),
            "{mode} rejected FFI composition must not leave a shared library"
        );
    }

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn unsupported_or_colliding_extern_const_declarations_fail_during_checking() {
    let dir = temp_dir("negative");
    let cases = [
        (
            "computed",
            r#"
extern "C" const BAD_COMPUTED: i32 = 2 + 3
func main() -> i32 { 0 }
"#,
        ),
        (
            "aggregate",
            r#"
extern "C" const BAD_AGGREGATE: (i32, i32) = (2, 3)
func main() -> i32 { 0 }
"#,
        ),
        (
            "function-collision",
            r#"
extern "C" { func collision() -> i32; }
extern "C" const collision: i32 = 7
func main() -> i32 { 0 }
"#,
        ),
    ];

    for (label, source_text) in cases {
        let source = dir.join(format!("{label}.mimi"));
        std::fs::write(&source, source_text).expect("write rejected export source");
        for (mode, explicit_mir) in [("default", false), ("explicit-mir", true)] {
            let library = dir.join(format!("{label}-{mode}.so"));
            let build = build_shared(&source, &library, explicit_mir);
            assert!(
                !build.status.success(),
                "{label} {mode} build unexpectedly succeeded"
            );
            let stderr = String::from_utf8_lossy(&build.stderr);
            assert!(
                stderr.contains("NATIVE-DATA-EXPORT-001"),
                "{label} {mode} did not report the checked data-export diagnostic: {stderr}"
            );
            assert!(
                !library.exists(),
                "{label} {mode} rejection left a shared library behind"
            );
        }
    }

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn computed_f32_extern_const_does_not_gain_an_implicit_narrowing_conversion() {
    let dir = temp_dir("computed-f32");
    let source = dir.join("computed-f32.mimi");
    std::fs::write(
        &source,
        r#"
extern "C" const BAD_F32: f32 = 1.0 + 2.0
func main() -> i32 { 0 }
"#,
    )
    .expect("write computed f32 data export source");

    for (mode, explicit_mir) in [("default", false), ("explicit-mir", true)] {
        let library = dir.join(format!("computed-f32-{mode}.so"));
        let build = build_shared(&source, &library, explicit_mir);
        assert!(
            !build.status.success(),
            "computed f32 {mode} build unexpectedly succeeded"
        );
        let stderr = String::from_utf8_lossy(&build.stderr);
        assert!(
            stderr.contains("E0209"),
            "computed f32 {mode} must fail type checking instead of narrowing implicitly: {stderr}"
        );
        assert!(
            !library.exists(),
            "computed f32 {mode} rejection left a shared library behind"
        );
    }

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[cfg(target_os = "linux")]
fn comptime_functions_do_not_enter_native_data_export_mir_or_symbols() {
    let dir = temp_dir("comptime-exclusion");
    let source = dir.join("comptime-exclusion.mimi");
    std::fs::write(
        &source,
        r#"
extern "C" const DATA: i32 = 7
comptime func compile_only() -> i32 { 1 }
func main() -> i32 { DATA }
"#,
    )
    .expect("write source with a valid export and a compile-time-only function");

    for (mode, explicit_mir) in [("default", false), ("explicit-mir", true)] {
        let library = dir.join(format!("comptime-{mode}.so"));
        let build = build_shared(&source, &library, explicit_mir);
        assert!(
            build.status.success(),
            "{mode} data export failed with a pure comptime helper: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        let stderr = String::from_utf8_lossy(&build.stderr);
        let emitted_owners = stderr
            .lines()
            .filter_map(|line| {
                line.strip_prefix("info: canonical MIR native emitted owner='")
                    .and_then(|owner| owner.strip_suffix('\''))
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            emitted_owners,
            BTreeSet::from(["function:main"]),
            "{mode} executable MIR roster must exclude the comptime helper"
        );
        assert!(
            !stderr.contains("function:compile_only"),
            "{mode} route census must not attribute the comptime function as executable"
        );
        let symbols = Command::new("readelf")
            .args(["-Ws"])
            .arg(&library)
            .output()
            .expect("read comptime shared-library symbols");
        assert!(symbols.status.success(), "readelf failed");
        assert!(
            !String::from_utf8_lossy(&symbols.stdout).contains("compile_only"),
            "{mode} native artifact must not contain a comptime runtime symbol"
        );
    }

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn data_export_route_fails_closed_for_an_unproved_runtime_cfg_cycle() {
    let dir = temp_dir("unsupported-runtime-cycle");
    let source = dir.join("unsupported-runtime-cycle.mimi");
    std::fs::write(
        &source,
        r#"
extern "C" const DATA: i32 = 7
func spin() -> i32 {
    let mut i: i32 = 0
    while i < 1 { i = i + 1 }
    i
}
func main() -> i32 { spin() }
"#,
    )
    .expect("write source with a runtime loop outside the verifier subset");

    for (mode, explicit_mir) in [("default", false), ("explicit-mir", true)] {
        let library = dir.join(format!("unsupported-{mode}.so"));
        let build = build_shared(&source, &library, explicit_mir);
        assert!(
            !build.status.success(),
            "unproved runtime CFG cycle unexpectedly passed {mode} data-export admission"
        );
        let stderr = String::from_utf8_lossy(&build.stderr);
        assert!(
            stderr.contains("native-data-export-v1")
                && stderr.contains("cyclic CFG outside the verifier capability"),
            "{mode} must fail at the shared MIR verifier gate: {stderr}"
        );
        assert!(
            !library.exists(),
            "{mode} verifier rejection left a shared library behind"
        );
    }

    let _ = std::fs::remove_dir_all(dir);
}
