// Native L3 fault-injection coverage for the nested Result Values profile.
// Kept in a child test module so its failpoint observers and native JIT
// adapters do not expand the already large ordinary Map/Any unit test file.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};

const CHILD_ENV: &str = "MIMI_V2_VALUES_RESULT_BRANCH_L3_CHILD";
const FIXTURE: &str =
    "tests/fixtures/mir_ordinary_map_any_owner_v2_values_local_index_i32_result_branch.mimi";
const VALUES_PUBLISH_FAILURE: usize = 0;
const SELECTED_OWNER_CLONE_FAILURE: usize = 1;
const NEW_ROOT_PUBLISH_FAILURE: usize = 2;
const SET_ROOT_PUBLISH_FAILURE: usize = 3;
const CAPACITY_STATUS: i32 = 3;

static ACTIVE_CASE: AtomicUsize = AtomicUsize::new(usize::MAX);
static VALUES_CALLS: AtomicUsize = AtomicUsize::new(0);
static VALUES_FAILURE_OBSERVED: AtomicBool = AtomicBool::new(false);
static VALUES_OUTPUT_ZERO: AtomicBool = AtomicBool::new(false);
static VALUES_SOURCE_UNCHANGED: AtomicBool = AtomicBool::new(false);
static SELECTED_GET_CALLS: AtomicUsize = AtomicUsize::new(0);
static SELECTED_GET_FAILURE_OBSERVED: AtomicBool = AtomicBool::new(false);
static SELECTED_GET_OUTPUTS_ZERO: AtomicBool = AtomicBool::new(false);
static SELECTED_SOURCE_UNCHANGED: AtomicBool = AtomicBool::new(false);
static MAP_DROP_CALLS: AtomicUsize = AtomicUsize::new(0);
static MAP_DROP_HANDLES: [AtomicI64; 2] = [AtomicI64::new(0), AtomicI64::new(0)];
static MAP_DROPS_SUCCEEDED: AtomicBool = AtomicBool::new(true);
static VALUES_LIST_DROP_CALLS: AtomicUsize = AtomicUsize::new(0);
static NATIVE_RETURNED: AtomicBool = AtomicBool::new(false);
static NATIVE_RESULT: AtomicI64 = AtomicI64::new(i64::MIN);
static ABORT_CALLS: AtomicUsize = AtomicUsize::new(0);
static NEW_CALLS: AtomicUsize = AtomicUsize::new(0);
static NEW_STATUS: AtomicI64 = AtomicI64::new(i64::MIN);
static NEW_OUTPUT: AtomicI64 = AtomicI64::new(i64::MIN);
static SET_CALLS: AtomicUsize = AtomicUsize::new(0);
static SET_STATUS: AtomicI64 = AtomicI64::new(i64::MIN);
static SET_OUTPUT: AtomicI64 = AtomicI64::new(i64::MIN);
static SET_SOURCE_UNCHANGED: AtomicBool = AtomicBool::new(false);
static ROOT_PUBLISH_ATTEMPTS_OBSERVED: AtomicUsize = AtomicUsize::new(usize::MAX);
static PAYLOAD_DROP_CALLS: AtomicUsize = AtomicUsize::new(0);
static PAYLOAD_DROP_TOKEN: AtomicI64 = AtomicI64::new(0);
static PAYLOAD_DROP_SUCCEEDED: AtomicBool = AtomicBool::new(true);
static BASELINE_ROOTS: AtomicUsize = AtomicUsize::new(usize::MAX);
static BASELINE_MAP_HANDLES: AtomicUsize = AtomicUsize::new(usize::MAX);
static BASELINE_MANAGED_ANY: AtomicUsize = AtomicUsize::new(usize::MAX);
static BASELINE_OWNER_TOKENS: AtomicUsize = AtomicUsize::new(usize::MAX);
static BASELINE_LISTS: AtomicUsize = AtomicUsize::new(usize::MAX);
static PRE_ABORT_ROOTS_SETTLED: AtomicBool = AtomicBool::new(false);
static PRE_ABORT_LISTS_SETTLED: AtomicBool = AtomicBool::new(false);

fn branch_program() -> (
    crate::core::CheckedProgram,
    MirProgram,
    crate::core::mir::CanonicalMirRouteReceipt,
) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let tokens = crate::lexer::Lexer::new(&source)
        .tokenize()
        .unwrap_or_else(|error| panic!("lex {}: {error:?}", path.display()));
    let parsed = crate::loader::parser_for_path(tokens, &path)
        .unwrap_or_else(|error| panic!("configure parser for {}: {error}", path.display()))
        .parse_file()
        .unwrap_or_else(|error| panic!("parse {}: {error:?}", path.display()));
    let mut file = if parsed.imports.is_empty() {
        parsed
    } else {
        let base_dir = path
            .parent()
            .expect("branch fixture has a parent directory")
            .to_path_buf();
        let mut loader = crate::loader::ModuleLoader::new(base_dir);
        loader
            .load_main_with_file(&path, parsed)
            .unwrap_or_else(|error| panic!("load branch fixture: {error}"));
        loader
            .merge_all()
            .unwrap_or_else(|error| panic!("merge branch fixture imports: {error}"))
    };
    crate::loader::merge_prelude_into(&mut file);
    let excluded_sources = file
        .sources
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<std::collections::HashSet<_>>();
    let checked = crate::core::check_program(&file)
        .unwrap_or_else(|diagnostics| panic!("check branch fixture: {diagnostics:?}"));
    let route =
        crate::core::mir::materialize_canonical_mir_route(&checked, Some(&excluded_sources))
            .unwrap_or_else(|error| panic!("materialize branch fixture: {error}"));
    assert_eq!(
        route.admission.ordinary_map_owned_any_v2,
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::ValuesLocalIndexI32ResultBranch,
        "L3 child must materialize the distinct nested-Result Values route"
    );
    let receipt = crate::core::mir::validate_ordinary_map_owned_any_v2_values_local_index_i32_result_branch_route(
        &checked,
        &route.program,
    )
    .unwrap_or_else(|error| panic!("validate nested-Result Values route: {error}"));
    (checked, route.program, receipt)
}

fn reset_observations(case: usize) {
    ACTIVE_CASE.store(case, Ordering::SeqCst);
    VALUES_CALLS.store(0, Ordering::SeqCst);
    VALUES_FAILURE_OBSERVED.store(false, Ordering::SeqCst);
    VALUES_OUTPUT_ZERO.store(false, Ordering::SeqCst);
    VALUES_SOURCE_UNCHANGED.store(false, Ordering::SeqCst);
    SELECTED_GET_CALLS.store(0, Ordering::SeqCst);
    SELECTED_GET_FAILURE_OBSERVED.store(false, Ordering::SeqCst);
    SELECTED_GET_OUTPUTS_ZERO.store(false, Ordering::SeqCst);
    SELECTED_SOURCE_UNCHANGED.store(false, Ordering::SeqCst);
    MAP_DROP_CALLS.store(0, Ordering::SeqCst);
    MAP_DROP_HANDLES[0].store(0, Ordering::SeqCst);
    MAP_DROP_HANDLES[1].store(0, Ordering::SeqCst);
    MAP_DROPS_SUCCEEDED.store(true, Ordering::SeqCst);
    VALUES_LIST_DROP_CALLS.store(0, Ordering::SeqCst);
    NATIVE_RETURNED.store(false, Ordering::SeqCst);
    NATIVE_RESULT.store(i64::MIN, Ordering::SeqCst);
    ABORT_CALLS.store(0, Ordering::SeqCst);
    NEW_CALLS.store(0, Ordering::SeqCst);
    NEW_STATUS.store(i64::MIN, Ordering::SeqCst);
    NEW_OUTPUT.store(i64::MIN, Ordering::SeqCst);
    SET_CALLS.store(0, Ordering::SeqCst);
    SET_STATUS.store(i64::MIN, Ordering::SeqCst);
    SET_OUTPUT.store(i64::MIN, Ordering::SeqCst);
    SET_SOURCE_UNCHANGED.store(false, Ordering::SeqCst);
    ROOT_PUBLISH_ATTEMPTS_OBSERVED.store(usize::MAX, Ordering::SeqCst);
    PAYLOAD_DROP_CALLS.store(0, Ordering::SeqCst);
    PAYLOAD_DROP_TOKEN.store(0, Ordering::SeqCst);
    PAYLOAD_DROP_SUCCEEDED.store(true, Ordering::SeqCst);
    BASELINE_ROOTS.store(usize::MAX, Ordering::SeqCst);
    BASELINE_MAP_HANDLES.store(usize::MAX, Ordering::SeqCst);
    BASELINE_MANAGED_ANY.store(usize::MAX, Ordering::SeqCst);
    BASELINE_OWNER_TOKENS.store(usize::MAX, Ordering::SeqCst);
    BASELINE_LISTS.store(usize::MAX, Ordering::SeqCst);
    PRE_ABORT_ROOTS_SETTLED.store(false, Ordering::SeqCst);
    PRE_ABORT_LISTS_SETTLED.store(false, Ordering::SeqCst);
}

// SAFETY: called by compiled JIT code matching `mimi_ordinary_map_owned_any_v2_new`'s C ABI.
unsafe extern "C" fn observe_new_root(out_root: *mut i64) -> i32 {
    NEW_CALLS.fetch_add(1, Ordering::SeqCst);
    // SAFETY: generated NewResult code supplies a writable i64 output slot.
    let status = unsafe { crate::runtime::mimi_ordinary_map_owned_any_v2_new(out_root) };
    NEW_STATUS.store(status as i64, Ordering::SeqCst);
    NEW_OUTPUT.store(
        if out_root.is_null() {
            i64::MIN
        } else {
            unsafe {
                // SAFETY: `out_root` was checked non-null and points to the caller's i64 out slot.
                *out_root
            }
        },
        Ordering::SeqCst,
    );
    status
}

// SAFETY: called by compiled JIT code matching `mimi_ordinary_map_owned_any_v2_set`'s C ABI.
unsafe extern "C" fn observe_generic_set(
    handle: i64,
    key: *const u8,
    key_len: i64,
    payload_owner: i64,
    selection_bytes: *const u8,
    selection_len: i64,
    selection_hash: *const u8,
    out_root: *mut i64,
) -> i32 {
    SET_CALLS.fetch_add(1, Ordering::SeqCst);
    let before_counts = crate::runtime::ordinary_map_domain_live_counts_for_test().ok();
    let mut before_size = i32::MIN;
    // SAFETY: the source handle belongs to this generated SetResult call.
    let before_status =
        unsafe { crate::runtime::mimi_ordinary_map_owned_any_v2_size(handle, &mut before_size) };
    // SAFETY: preserve the full generic Set ABI and all call inputs verbatim.
    let status = unsafe {
        crate::runtime::mimi_ordinary_map_owned_any_v2_set(
            handle,
            key,
            key_len,
            payload_owner,
            selection_bytes,
            selection_len,
            selection_hash,
            out_root,
        )
    };
    SET_STATUS.store(status as i64, Ordering::SeqCst);
    SET_OUTPUT.store(
        if out_root.is_null() {
            i64::MIN
        } else {
            unsafe {
                // SAFETY: `out_root` was checked non-null and points to the caller's i64 out slot.
                *out_root
            }
        },
        Ordering::SeqCst,
    );
    let mut after_size = i32::MIN;
    let after_status = unsafe {
        // SAFETY: SetResult retains its source handle when publication fails, and `after_size` is local.
        crate::runtime::mimi_ordinary_map_owned_any_v2_size(handle, &mut after_size)
    };
    let after_counts = crate::runtime::ordinary_map_domain_live_counts_for_test().ok();
    SET_SOURCE_UNCHANGED.store(
        status == CAPACITY_STATUS
            && before_status == 0
            && after_status == 0
            && before_size == 0
            && after_size == before_size
            && before_counts.is_some()
            && before_counts == after_counts,
        Ordering::SeqCst,
    );
    status
}

// SAFETY: called by compiled JIT code matching `mimi_ordinary_map_owned_any_v2_values_list`'s C ABI.
unsafe extern "C" fn observe_values_publication(
    handle: i64,
    selection_bytes: *const u8,
    selection_len: i64,
    selection_hash: *const u8,
    out_list: *mut *mut crate::runtime::MimiList,
) -> i32 {
    VALUES_CALLS.fetch_add(1, Ordering::SeqCst);
    let counts_before = crate::runtime::ordinary_map_domain_live_counts_for_test().ok();
    let lists_before = crate::runtime::managed_any_v2_list_count_for_test();
    if let Some(counts) = counts_before {
        BASELINE_ROOTS.store(counts.roots, Ordering::SeqCst);
        BASELINE_MAP_HANDLES.store(counts.runtime_map_handles, Ordering::SeqCst);
        BASELINE_MANAGED_ANY.store(counts.managed_any, Ordering::SeqCst);
        BASELINE_OWNER_TOKENS.store(counts.any_owner_tokens, Ordering::SeqCst);
    }
    if let Some(lists) = lists_before {
        BASELINE_LISTS.store(lists, Ordering::SeqCst);
    }
    // SAFETY: generated MIR supplies the exact checked Values-list ABI; this
    // observer forwards every argument unchanged to the production runtime.
    let status = unsafe {
        crate::runtime::mimi_ordinary_map_owned_any_v2_values_list(
            handle,
            selection_bytes,
            selection_len,
            selection_hash,
            out_list,
        )
    };
    let output_zero = !out_list.is_null()
        && unsafe {
            // SAFETY: `out_list` was checked non-null and initialized by `mimi_ordinary_map_owned_any_v2_values_list`.
            (*out_list).is_null()
        };
    let counts_after = crate::runtime::ordinary_map_domain_live_counts_for_test().ok();
    let lists_after = crate::runtime::managed_any_v2_list_count_for_test();
    VALUES_FAILURE_OBSERVED.store(status == CAPACITY_STATUS, Ordering::SeqCst);
    VALUES_OUTPUT_ZERO.store(output_zero, Ordering::SeqCst);
    VALUES_SOURCE_UNCHANGED.store(
        status == CAPACITY_STATUS
            && counts_before.is_some()
            && counts_before == counts_after
            && lists_before.is_some()
            && lists_before == lists_after,
        Ordering::SeqCst,
    );
    status
}

// SAFETY: called by compiled JIT code matching `mimi_mir_list_get_managed_any_v2`'s C ABI.
unsafe extern "C" fn observe_selected_owner_clone(
    list: *const crate::runtime::MimiList,
    index: i64,
    out_found: *mut i32,
    out_owner: *mut i64,
) -> i32 {
    SELECTED_GET_CALLS.fetch_add(1, Ordering::SeqCst);
    // Snapshot source list identity, length, and selected owner before and
    // after the real operation; the selected-owner failure must not mutate it.
    let before = if list.is_null() {
        None
    } else {
        // SAFETY: generated MIR supplies its still-live canonical Values list.
        let source = unsafe { &*list };
        if source.len < 0
            || (source.len > 0 && source.data.is_null())
            || index < 0
            || index >= source.len
        {
            None
        } else {
            let owner = unsafe {
                // SAFETY: `source.data` is non-null and `0 <= index < source.len` for i64 owner handles.
                *((source.data as *const i64).add(index as usize))
            };
            Some((list as usize, source.len, source.data as usize, owner))
        }
    };
    let counts_before = crate::runtime::ordinary_map_domain_live_counts_for_test().ok();
    let lists_before = crate::runtime::managed_any_v2_list_count_for_test();
    // SAFETY: all pointer arguments come from generated, live MIR slots and
    // retain the exact runtime ABI layout.
    let status = unsafe {
        crate::runtime::mimi_mir_list_get_managed_any_v2(list, index, out_found, out_owner)
    };
    let after = if list.is_null() {
        None
    } else {
        // SAFETY: the operation is required to preserve the source Values list.
        let source = unsafe { &*list };
        if source.len < 0
            || (source.len > 0 && source.data.is_null())
            || index < 0
            || index >= source.len
        {
            None
        } else {
            let owner = unsafe {
                // SAFETY: `source.data` is non-null and `0 <= index < source.len` for i64 owner handles.
                *((source.data as *const i64).add(index as usize))
            };
            Some((list as usize, source.len, source.data as usize, owner))
        }
    };
    let counts_after = crate::runtime::ordinary_map_domain_live_counts_for_test().ok();
    let lists_after = crate::runtime::managed_any_v2_list_count_for_test();
    SELECTED_GET_FAILURE_OBSERVED.store(status == CAPACITY_STATUS, Ordering::SeqCst);
    SELECTED_GET_OUTPUTS_ZERO.store(
        !out_found.is_null()
            && !out_owner.is_null()
            && unsafe {
                // SAFETY: both `out_found` and `out_owner` were checked non-null and written by `mimi_mir_list_get_managed_any_v2`.
                *out_found == 0 && *out_owner == 0
            },
        Ordering::SeqCst,
    );
    SELECTED_SOURCE_UNCHANGED.store(
        before.is_some()
            && before == after
            && counts_before.is_some()
            && counts_before == counts_after
            && lists_before.is_some()
            && lists_before == lists_after,
        Ordering::SeqCst,
    );
    status
}

// SAFETY: called by compiled JIT drop glue matching `mimi_ordinary_map_owned_any_v2_drop`'s C ABI.
unsafe extern "C" fn observe_map_drop(handle: i64) -> i32 {
    let slot = MAP_DROP_CALLS.fetch_add(1, Ordering::SeqCst);
    if let Some(observed) = MAP_DROP_HANDLES.get(slot) {
        observed.store(handle, Ordering::SeqCst);
    }
    let status = crate::runtime::mimi_ordinary_map_owned_any_v2_drop(handle);
    if status != 0 {
        MAP_DROPS_SUCCEEDED.store(false, Ordering::SeqCst);
    }
    status
}

// SAFETY: called by compiled JIT drop glue matching `mimi_any_payload_v2_drop`'s C ABI.
unsafe extern "C" fn observe_payload_drop(token: i64) -> i32 {
    PAYLOAD_DROP_CALLS.fetch_add(1, Ordering::SeqCst);
    PAYLOAD_DROP_TOKEN.store(token, Ordering::SeqCst);
    let status = crate::runtime::mimi_any_payload_v2_drop(token);
    if status != 0 {
        PAYLOAD_DROP_SUCCEEDED.store(false, Ordering::SeqCst);
    }
    status
}

// SAFETY: called by compiled JIT drop glue matching `mimi_mir_list_drop_managed_any_v2`'s C ABI.
unsafe extern "C" fn observe_values_list_drop(list: *mut crate::runtime::MimiList) {
    VALUES_LIST_DROP_CALLS.fetch_add(1, Ordering::SeqCst);
    // SAFETY: generated glue transfers its live Values list exactly once.
    unsafe { crate::runtime::mimi_mir_list_drop_managed_any_v2(list) };
}

// SAFETY: called by compiled JIT trap path matching `mimi_runtime_abort`'s C ABI.
unsafe extern "C" fn observe_abort_before_runtime_drain(message: *const libc::c_char) -> ! {
    ABORT_CALLS.fetch_add(1, Ordering::SeqCst);
    // The runtime's error handler runs after its generic root drain. Sample
    // here first so the evidence proves emitted MIR cleanup itself settled the
    // source roots and Values temporary before the process-wide trap drain.
    PRE_ABORT_ROOTS_SETTLED.store(
        crate::runtime::ordinary_map_domain_live_counts_for_test()
            .is_ok_and(|counts| counts == crate::runtime::OrdinaryMapDomainLiveCounts::default()),
        Ordering::SeqCst,
    );
    PRE_ABORT_LISTS_SETTLED.store(
        crate::runtime::managed_any_v2_list_count_for_test() == Some(0),
        Ordering::SeqCst,
    );
    // SAFETY: this wrapper preserves the native runtime's non-returning trap
    // ABI and forwards the exact generated E0800 diagnostic.
    unsafe { crate::runtime::mimi_runtime_abort(message) }
}

// SAFETY: registered as runtime error handler accepting a nullable NUL-terminated C diagnostic pointer.
unsafe extern "C" fn abort_observer(message: *const libc::c_char) {
    let case = ACTIVE_CASE.load(Ordering::SeqCst);
    let expected_message = match case {
        VALUES_PUBLISH_FAILURE => "[E0800] ordinary Map/Any owner-v2 Values failed",
        SELECTED_OWNER_CLONE_FAILURE => "[E0800] ordinary Map/Any owner-v2 Values list get failed",
        NEW_ROOT_PUBLISH_FAILURE | SET_ROOT_PUBLISH_FAILURE => {
            "unexpected E0800 for recoverable Map Result publication failure"
        }
        _ => "invalid ValuesLocalIndexI32ResultBranch failure case",
    };
    let actual_message = if message.is_null() {
        String::new()
    } else {
        // SAFETY: runtime_abort supplies a live NUL-terminated message for the
        // duration of this synchronous callback.
        unsafe { std::ffi::CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned()
    };
    let exact_trap = actual_message == expected_message;
    let drop_calls = MAP_DROP_CALLS.load(Ordering::SeqCst);
    let handles = [
        MAP_DROP_HANDLES[0].load(Ordering::SeqCst),
        MAP_DROP_HANDLES[1].load(Ordering::SeqCst),
    ];
    let roots_settled = crate::runtime::ordinary_map_domain_live_counts_for_test()
        .is_ok_and(|counts| counts == crate::runtime::OrdinaryMapDomainLiveCounts::default());
    let lists_settled = crate::runtime::managed_any_v2_list_count_for_test() == Some(0);
    let expected_list_drops = usize::from(case == SELECTED_OWNER_CLONE_FAILURE);
    let expected_values_attempts = usize::from(case == VALUES_PUBLISH_FAILURE);
    let expected_selected_attempts = usize::from(case == SELECTED_OWNER_CLONE_FAILURE);
    let publication_ok = case != VALUES_PUBLISH_FAILURE
        || (VALUES_FAILURE_OBSERVED.load(Ordering::SeqCst)
            && VALUES_OUTPUT_ZERO.load(Ordering::SeqCst)
            && VALUES_SOURCE_UNCHANGED.load(Ordering::SeqCst));
    let selected_clone_ok = case != SELECTED_OWNER_CLONE_FAILURE
        || (SELECTED_GET_FAILURE_OBSERVED.load(Ordering::SeqCst)
            && SELECTED_GET_OUTPUTS_ZERO.load(Ordering::SeqCst)
            && SELECTED_SOURCE_UNCHANGED.load(Ordering::SeqCst));
    let exact_root_cleanup = drop_calls == 2
        && handles[0] != 0
        && handles[1] != 0
        && handles[0] != handles[1]
        && MAP_DROPS_SUCCEEDED.load(Ordering::SeqCst);
    let exact_list_cleanup = VALUES_LIST_DROP_CALLS.load(Ordering::SeqCst) == expected_list_drops;
    let baseline_exact = BASELINE_ROOTS.load(Ordering::SeqCst) == 2
        && BASELINE_MAP_HANDLES.load(Ordering::SeqCst) == 2
        && BASELINE_MANAGED_ANY.load(Ordering::SeqCst) > 0
        && BASELINE_OWNER_TOKENS.load(Ordering::SeqCst) > 0
        && BASELINE_LISTS.load(Ordering::SeqCst) == 0;
    let exact_attempts = VALUES_CALLS.load(Ordering::SeqCst) == 1
        && crate::runtime::ordinary_map_owned_any_v2_values_publish_attempts_for_test()
            == expected_values_attempts
        && SELECTED_GET_CALLS.load(Ordering::SeqCst) == expected_selected_attempts
        && crate::runtime::ordinary_map_owned_any_v2_values_unpack_owner_attempts_for_test()
            == expected_selected_attempts;
    let no_result_published = !NATIVE_RETURNED.load(Ordering::SeqCst);
    let valid = exact_trap
        && publication_ok
        && selected_clone_ok
        && exact_root_cleanup
        && exact_list_cleanup
        && PRE_ABORT_ROOTS_SETTLED.load(Ordering::SeqCst)
        && PRE_ABORT_LISTS_SETTLED.load(Ordering::SeqCst)
        && roots_settled
        && lists_settled
        && baseline_exact
        && exact_attempts
        && no_result_published;
    eprintln!(
        "values-result-branch-l3 case={case} exact_e0800={exact_trap} values_attempts={}/{} values_output_zero={} values_source_unchanged={} selected_attempts={}/{} selected_outputs_zero={} selected_source_unchanged={} roots_dropped={drop_calls} unique_root_handles={} values_lists_dropped={} pre_abort_roots_settled={} pre_abort_lists_settled={} roots_settled={roots_settled} lists_settled={lists_settled} baseline_roots={} baseline_map_handles={} baseline_managed_any={} baseline_any_owners={} baseline_lists={} result_published={} valid={valid}",
        crate::runtime::ordinary_map_owned_any_v2_values_publish_attempts_for_test(),
        expected_values_attempts,
        VALUES_OUTPUT_ZERO.load(Ordering::SeqCst),
        VALUES_SOURCE_UNCHANGED.load(Ordering::SeqCst),
        crate::runtime::ordinary_map_owned_any_v2_values_unpack_owner_attempts_for_test(),
        expected_selected_attempts,
        SELECTED_GET_OUTPUTS_ZERO.load(Ordering::SeqCst),
        SELECTED_SOURCE_UNCHANGED.load(Ordering::SeqCst),
        usize::from(handles[0] != 0 && handles[1] != 0 && handles[0] != handles[1]),
        VALUES_LIST_DROP_CALLS.load(Ordering::SeqCst),
        PRE_ABORT_ROOTS_SETTLED.load(Ordering::SeqCst),
        PRE_ABORT_LISTS_SETTLED.load(Ordering::SeqCst),
        BASELINE_ROOTS.load(Ordering::SeqCst),
        BASELINE_MAP_HANDLES.load(Ordering::SeqCst),
        BASELINE_MANAGED_ANY.load(Ordering::SeqCst),
        BASELINE_OWNER_TOKENS.load(Ordering::SeqCst),
        BASELINE_LISTS.load(Ordering::SeqCst),
        NATIVE_RETURNED.load(Ordering::SeqCst),
    );
    // std::process::exit lets Valgrind emit its final leak/error summary while
    // still avoiding unwinding across the C ABI.
    std::process::exit(if valid { 0 } else { 97 });
}

fn address_for_import(name: &str) -> Option<usize> {
    Some(match name {
        "mimi_ordinary_map_owned_any_v2_new" => observe_new_root as *const () as usize,
        "mimi_ordinary_map_owned_any_v2_set" => observe_generic_set as *const () as usize,
        "mimi_ordinary_map_owned_any_v2_set_i32_result" => {
            crate::runtime::mimi_ordinary_map_owned_any_v2_set_i32_result as *const () as usize
        }
        "mimi_ordinary_map_owned_any_v2_drop" => observe_map_drop as *const () as usize,
        "mimi_any_payload_v2_new" => crate::runtime::mimi_any_payload_v2_new as *const () as usize,
        "mimi_any_payload_v2_drop" => observe_payload_drop as *const () as usize,
        "mimi_ordinary_map_owned_any_v2_values_list" => {
            observe_values_publication as *const () as usize
        }
        "mimi_mir_list_get_managed_any_v2" => observe_selected_owner_clone as *const () as usize,
        "mimi_mir_list_drop_managed_any_v2" => observe_values_list_drop as *const () as usize,
        "mimi_mir_ordinary_map_domain_register_shutdown" => {
            crate::runtime::mimi_mir_ordinary_map_domain_register_shutdown as *const () as usize
        }
        "mimi_runtime_abort" => observe_abort_before_runtime_drain as *const () as usize,
        "mimi_mir_assert_no_live_managed_any_v2_lists" => {
            crate::runtime::mimi_mir_assert_no_live_managed_any_v2_lists as *const () as usize
        }
        "mimi_any_payload_v2_unpack" => {
            crate::runtime::mimi_any_payload_v2_unpack as *const () as usize
        }
        "mimi_string_free" => crate::runtime::mimi_string_free as *const () as usize,
        _ => return None,
    })
}

fn run_child_under_memcheck(case: &str) -> std::process::Output {
    let executable = std::env::current_exe().expect("test executable path");
    let test_module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .expect("unit-test path begins with the crate name");
    let child_test = format!("{test_module}::child");
    let mut command = std::process::Command::new("valgrind");
    command
        .args([
            "--tool=memcheck",
            "--leak-check=full",
            "--show-leak-kinds=all",
            "--errors-for-leak-kinds=definite,indirect,possible",
            "--error-exitcode=99",
        ])
        .arg(&executable)
        .args([
            "--exact",
            &child_test,
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(CHILD_ENV, case);
    let output = command.output().unwrap_or_else(|error| {
        panic!("start strict native Values branch Memcheck child: {error}")
    });
    if let Some(root) = std::env::var_os("MIMI_MAP_V2_VALUES_LOCAL_INDEX_I32_L3_ARTIFACT_DIR") {
        let directory = std::path::PathBuf::from(root)
            .join("native")
            .join("failure")
            .join(case);
        std::fs::create_dir_all(directory.parent().expect("failure parent directory"))
            .expect("create Values branch failure evidence parent");
        std::fs::create_dir(&directory).unwrap_or_else(|error| {
            panic!(
                "create fresh Values branch failure evidence {}: {error}",
                directory.display()
            )
        });
        let command_text = format!(
            "valgrind --tool=memcheck --leak-check=full --show-leak-kinds=all \
             --errors-for-leak-kinds=definite,indirect,possible --error-exitcode=99 \
             {} --exact {} --ignored --test-threads=1 --nocapture\n",
            executable.display(),
            child_test
        );
        write_new(&directory.join("command.txt"), command_text.as_bytes());
        write_new(
            &directory.join("environment.txt"),
            format!("{CHILD_ENV}={case}\n").as_bytes(),
        );
        write_new(&directory.join("stdout.bin"), &output.stdout);
        write_new(&directory.join("stderr.bin"), &output.stderr);
        write_new(
            &directory.join("process.status"),
            process_status(&output).as_bytes(),
        );
    }
    output
}

fn write_new(path: &std::path::Path, bytes: &[u8]) {
    use std::io::Write;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap_or_else(|error| panic!("create evidence file {}: {error}", path.display()));
    file.write_all(bytes)
        .unwrap_or_else(|error| panic!("write evidence file {}: {error}", path.display()));
}

fn process_status(output: &std::process::Output) -> String {
    match output.status.code() {
        Some(code) => format!("exit_code={code}\nsuccess={}\n", output.status.success()),
        None => format!(
            "exit_code=signal\nsuccess={}\nstatus={:?}\n",
            output.status.success(),
            output.status
        ),
    }
}

fn assert_root_publish_result(case: usize, result: i32) {
    let (expected_attempts, expected_map_drops, expected_set_calls) = match case {
        NEW_ROOT_PUBLISH_FAILURE => (1, 0, 0),
        SET_ROOT_PUBLISH_FAILURE => (2, 1, 1),
        _ => panic!("not a recoverable root publication case: {case}"),
    };
    let new_failed = case == NEW_ROOT_PUBLISH_FAILURE;
    let domains_settled = crate::runtime::ordinary_map_domain_live_counts_for_test()
        .is_ok_and(|counts| counts == crate::runtime::OrdinaryMapDomainLiveCounts::default())
        && crate::runtime::managed_any_v2_list_count_for_test() == Some(0);
    let valid = result == CAPACITY_STATUS
        && NATIVE_RETURNED.load(Ordering::SeqCst)
        && NATIVE_RESULT.load(Ordering::SeqCst) == CAPACITY_STATUS as i64
        && ABORT_CALLS.load(Ordering::SeqCst) == 0
        && NEW_CALLS.load(Ordering::SeqCst) == 1
        && NEW_STATUS.load(Ordering::SeqCst)
            == if new_failed {
                CAPACITY_STATUS as i64
            } else {
                0
            }
        && (if new_failed {
            NEW_OUTPUT.load(Ordering::SeqCst) == 0
        } else {
            NEW_OUTPUT.load(Ordering::SeqCst) != 0
        })
        && SET_CALLS.load(Ordering::SeqCst) == expected_set_calls
        && (if new_failed {
            SET_STATUS.load(Ordering::SeqCst) == i64::MIN
                && SET_OUTPUT.load(Ordering::SeqCst) == i64::MIN
                && !SET_SOURCE_UNCHANGED.load(Ordering::SeqCst)
        } else {
            SET_STATUS.load(Ordering::SeqCst) == CAPACITY_STATUS as i64
                && SET_OUTPUT.load(Ordering::SeqCst) == 0
                && SET_SOURCE_UNCHANGED.load(Ordering::SeqCst)
        })
        && ROOT_PUBLISH_ATTEMPTS_OBSERVED.load(Ordering::SeqCst) == expected_attempts
        && VALUES_CALLS.load(Ordering::SeqCst) == 0
        && SELECTED_GET_CALLS.load(Ordering::SeqCst) == 0
        && MAP_DROP_CALLS.load(Ordering::SeqCst) == expected_map_drops
        && (expected_map_drops == 0
            || (MAP_DROP_HANDLES[0].load(Ordering::SeqCst) == NEW_OUTPUT.load(Ordering::SeqCst)
                && MAP_DROPS_SUCCEEDED.load(Ordering::SeqCst)))
        && PAYLOAD_DROP_CALLS.load(Ordering::SeqCst) == usize::from(!new_failed)
        && (new_failed
            || (PAYLOAD_DROP_TOKEN.load(Ordering::SeqCst) != 0
                && PAYLOAD_DROP_SUCCEEDED.load(Ordering::SeqCst)))
        && domains_settled;
    eprintln!(
        "values-result-branch-l3 case={} exact_result={} returned={} abort_calls={} new_status={} new_output={} set_status={} set_output={} set_source_unchanged={} root_publish_attempts={} roots_dropped={} payload_drops={} payload_drop_token={} payload_drop_succeeded={} values_calls={} selected_calls={} final_domains_settled={domains_settled} valid={valid}",
        if new_failed { "new-publish" } else { "set-publish" },
        result == CAPACITY_STATUS,
        NATIVE_RETURNED.load(Ordering::SeqCst),
        ABORT_CALLS.load(Ordering::SeqCst),
        NEW_STATUS.load(Ordering::SeqCst),
        NEW_OUTPUT.load(Ordering::SeqCst),
        SET_STATUS.load(Ordering::SeqCst),
        SET_OUTPUT.load(Ordering::SeqCst),
        SET_SOURCE_UNCHANGED.load(Ordering::SeqCst),
        ROOT_PUBLISH_ATTEMPTS_OBSERVED.load(Ordering::SeqCst),
        MAP_DROP_CALLS.load(Ordering::SeqCst),
        PAYLOAD_DROP_CALLS.load(Ordering::SeqCst),
        PAYLOAD_DROP_TOKEN.load(Ordering::SeqCst),
        PAYLOAD_DROP_SUCCEEDED.load(Ordering::SeqCst),
        VALUES_CALLS.load(Ordering::SeqCst),
        SELECTED_GET_CALLS.load(Ordering::SeqCst),
    );
    assert!(
        valid,
        "recoverable New/Set publication Result contract failed"
    );
}

#[test]
#[ignore = "strict nested-Result Values native failure Memcheck L3"]
fn nested_result_values_publication_and_selected_owner_failures_are_memcheck_clean() {
    for case in [
        "values-publish",
        "selected-owner-clone",
        "new-publish",
        "set-publish",
    ] {
        let output = run_child_under_memcheck(case);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{stdout}\n{stderr}");
        assert_eq!(
            output.status.code(),
            Some(0),
            "{case}: native E0800 rollback or strict Memcheck failed:\n{combined}"
        );
        assert!(
            stdout.contains("running 1 test"),
            "{case}: child did not run the isolated native test:\n{combined}"
        );
        let expected_marker = if matches!(case, "values-publish" | "selected-owner-clone") {
            "exact_e0800=true"
        } else {
            "exact_result=true"
        };
        assert!(
            stderr.contains(expected_marker) && stderr.contains("valid=true"),
            "{case}: exact native failure contract was not observed:\n{combined}"
        );
        assert!(
            stderr.contains("ERROR SUMMARY: 0 errors"),
            "{case}: Memcheck found invalid memory use:\n{combined}"
        );
        assert!(
            stderr.contains("All heap blocks were freed -- no leaks are possible")
                || [
                    "definitely lost: 0 bytes",
                    "indirectly lost: 0 bytes",
                    "possibly lost: 0 bytes",
                ]
                .iter()
                .all(|entry| stderr.contains(entry)),
            "{case}: strict native cleanup leaked memory:\n{combined}"
        );
    }
}

#[test]
#[ignore = "isolated native nested-Result Values L3 failure subprocess"]
fn child() {
    let Ok(case) = std::env::var(CHILD_ENV) else {
        return;
    };
    let case_id = match case.as_str() {
        "values-publish" => VALUES_PUBLISH_FAILURE,
        "selected-owner-clone" => SELECTED_OWNER_CLONE_FAILURE,
        "new-publish" => NEW_ROOT_PUBLISH_FAILURE,
        "set-publish" => SET_ROOT_PUBLISH_FAILURE,
        _ => panic!("unknown branch L3 failure case {case}"),
    };
    reset_observations(case_id);
    let (checked, program, receipt) = branch_program();
    let context = Context::create();
    let mut generator = CodeGenerator::new(&context, "native_values_result_branch_l3_failure");
    generator
        .compile_checked_mir_native_with_values_local_index_i32_result_branch_route(
            &checked, &program, &receipt,
        )
        .unwrap_or_else(|errors| panic!("emit exact branch L3 native MIR: {errors:?}"));
    generator
        .module
        .verify()
        .expect("nested-Result Values native L3 LLVM verifies");
    let engine = generator
        .module
        .create_jit_execution_engine(inkwell::OptimizationLevel::None)
        .expect("nested-Result Values native L3 JIT engine");
    for declaration in generator.module.get_functions() {
        if declaration.count_basic_blocks() != 0 {
            continue;
        }
        let name = declaration
            .get_name()
            .to_str()
            .expect("runtime import has a UTF-8 symbol")
            .to_owned();
        if name.starts_with("llvm.") {
            continue;
        }
        let address = address_for_import(&name)
            .unwrap_or_else(|| panic!("unmapped native branch runtime import {name}"));
        assert_ne!(address, 0, "runtime mapping for {name}");
        engine.add_global_mapping(&declaration, address);
    }
    crate::runtime::mimi_runtime_set_error_handler(Some(abort_observer));
    let native = unsafe {
        // SAFETY: native module compiles `main` with C ABI `fn() -> i32`.
        engine.get_function::<unsafe extern "C" fn() -> i32>("main")
    }
    .expect("nested-Result Values branch native main");
    crate::codegen::assert_execution_engine_linked(&engine, "nested-Result Values branch L3");
    let result = match case_id {
        VALUES_PUBLISH_FAILURE => {
            crate::runtime::with_ordinary_map_owned_any_v2_values_publish_failure_for_test(
                0,
                || unsafe {
                    // SAFETY: `native` was linked into `engine` and takes no arguments.
                    native.call()
                },
            )
        }
        SELECTED_OWNER_CLONE_FAILURE => {
            crate::runtime::with_ordinary_map_owned_any_v2_values_unpack_owner_failure_for_test(
                0,
                || unsafe {
                    // SAFETY: `native` was linked into `engine` and takes no arguments.
                    native.call()
                },
            )
        }
        NEW_ROOT_PUBLISH_FAILURE => {
            crate::runtime::with_ordinary_map_owned_any_v2_root_publish_failure_for_test(0, || {
                let result = unsafe {
                    // SAFETY: `native` was linked into `engine` and takes no arguments.
                    native.call()
                };
                ROOT_PUBLISH_ATTEMPTS_OBSERVED.store(
                    crate::runtime::ordinary_map_owned_any_v2_root_publish_attempts_for_test(),
                    Ordering::SeqCst,
                );
                result
            })
        }
        SET_ROOT_PUBLISH_FAILURE => {
            crate::runtime::with_ordinary_map_owned_any_v2_root_publish_failure_for_test(1, || {
                let result = unsafe {
                    // SAFETY: `native` was linked into `engine` and takes no arguments.
                    native.call()
                };
                ROOT_PUBLISH_ATTEMPTS_OBSERVED.store(
                    crate::runtime::ordinary_map_owned_any_v2_root_publish_attempts_for_test(),
                    Ordering::SeqCst,
                );
                result
            })
        }
        _ => unreachable!(),
    };
    NATIVE_RETURNED.store(true, Ordering::SeqCst);
    NATIVE_RESULT.store(result as i64, Ordering::SeqCst);
    if matches!(case_id, NEW_ROOT_PUBLISH_FAILURE | SET_ROOT_PUBLISH_FAILURE) {
        assert_root_publish_result(case_id, result);
        return;
    }
    panic!("native branch unexpectedly returned {result} instead of trapping with E0800");
}
