//! Wave-1 audit-fix regression tests — runtime_core.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).
//!
//! Ownership note: these regressions target `src/runtime/mod.rs` (the
//! runtime_core agent's exclusive file). Abort-paths (fail-loud json
//! accessors on bad input, `mimi_inject_fault`, `str_substring_clamp`
//! start>end) are covered by the central e2e abort harness; only
//! non-aborting paths are called directly here.

use crate::runtime as rt;
use std::ffi::{c_char, CStr};

/// Read + free a runtime-owned NUL-terminated string.
fn take_str(ptr: *mut c_char) -> String {
    assert!(!ptr.is_null(), "runtime returned NULL string");
    // SAFETY: runtime string results are NUL-terminated heap allocations.
    let s = unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    rt::mimi_string_free(ptr);
    s
}

fn cstr(bytes: &[u8]) -> *const c_char {
    bytes.as_ptr() as *const c_char
}

// ── Fix #4: new ptr+len string externs (VM char-semantics parity) ─────

#[test]
fn runtime_core_str_to_upper_lower_trim_unicode() {
    // to_upper / to_lower operate on the full UTF-8 string, not bytes.
    let s = "Straße"; // ß uppercases to "SS"
    let out = rt::mimi_str_to_upper(cstr(s.as_bytes()), s.len() as i64);
    assert_eq!(take_str(out), "STRASSE");
    let lo = rt::mimi_str_to_lower(cstr(b"HELLO"), 5);
    assert_eq!(take_str(lo), "hello");
    // trim is Unicode-aware (strips NBSP U+00A0, not just ASCII whitespace).
    let t = "\u{00A0}\t hi \n\u{00A0}";
    let tr = rt::mimi_str_trim(cstr(t.as_bytes()), t.len() as i64);
    assert_eq!(take_str(tr), "hi");
}

#[test]
fn runtime_core_str_substring_clamp_in_range_and_oob_clamps() {
    let s = "hello";
    let p = cstr(s.as_bytes());
    // Normal range.
    assert_eq!(take_str(rt::mimi_str_substring_clamp(p, 5, 1, 4)), "ell");
    // end beyond length clamps instead of aborting (VM function-form parity).
    assert_eq!(take_str(rt::mimi_str_substring_clamp(p, 5, 2, 99)), "llo");
    // start beyond length clamps to len → empty result.
    assert_eq!(take_str(rt::mimi_str_substring_clamp(p, 5, 99, 100)), "");
    // (start > end AFTER clamping aborts — central e2e harness covers it.)
}

// ── Fix #7: JSON \uXXXX surrogate pairs / malformed hex fail loud ─────

#[test]
fn runtime_core_json_deserialize_combines_surrogate_pair() {
    // U+1F600 = <D83D DE00> surrogate pair.
    let json = b"[\"\\ud83d\\ude00\"]\0";
    let mut len: i64 = -1;
    let buf = rt::mimi_json_deserialize(cstr(json), &mut len, 2);
    assert!(!buf.is_null());
    assert_eq!(len, 1);
    // SAFETY: buf is a boxed slice of `len` i64; element 0 is a string ptr.
    let s = unsafe {
        let ptr = *(buf as *mut i64) as *const c_char;
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    };
    assert_eq!(s, "\u{1F600}");
    rt::mimi_json_deserialize_free(buf, len, 2);
}

#[test]
fn runtime_core_json_deserialize_lone_surrogate_fails_parse() {
    // Lone high surrogate → parse failure (serde parity), null + len 0.
    let json = b"[\"\\ud800\"]\0";
    let mut len: i64 = -1;
    let buf = rt::mimi_json_deserialize(cstr(json), &mut len, 2);
    assert!(buf.is_null());
    assert_eq!(len, 0);
}

#[test]
fn runtime_core_json_deserialize_malformed_hex_fails_parse() {
    // Old code decoded "\uzzzz" via the "0000" fallback → NUL. Now it fails.
    let json = b"[\"\\uzzzz\"]\0";
    let mut len: i64 = -1;
    let buf = rt::mimi_json_deserialize(cstr(json), &mut len, 2);
    assert!(buf.is_null());
    assert_eq!(len, 0);
}

// ── Fix #6: tuple deserialize allocates "" (no NULL sentinel) ─────────

#[test]
fn runtime_core_tuple_deserialize_empty_string_allocates() {
    let json = b"[\"\",\"x\"]\0";
    let mut types = [2i64, 2i64];
    let mut out = [0i64, 0i64];
    let n = rt::mimi_tuple_deserialize(cstr(json), 2, types.as_mut_ptr(), out.as_mut_ptr());
    assert_eq!(n, 2);
    assert_ne!(out[0], 0, "empty string must allocate, not write NULL");
    assert_ne!(out[1], 0);
    // SAFETY: both slots are runtime-allocated C strings.
    unsafe {
        assert_eq!(
            CStr::from_ptr(out[0] as *const c_char).to_string_lossy(),
            ""
        );
        assert_eq!(
            CStr::from_ptr(out[1] as *const c_char).to_string_lossy(),
            "x"
        );
        libc::free(out[0] as *mut std::ffi::c_void);
        libc::free(out[1] as *mut std::ffi::c_void);
    }
}

// ── Fix #8: boxed-slice hand-off (set_to_list / json_deserialize) ─────

#[test]
fn runtime_core_set_to_list_round_trip_boxed_slice() {
    let h = rt::mimi_set_new();
    assert_ne!(h, 0);
    rt::mimi_set_insert(h, 1);
    rt::mimi_set_insert(h, 2);
    rt::mimi_set_insert(h, 3);
    let mut len: i64 = -1;
    let ptr = rt::mimi_set_to_list(h, &mut len);
    assert!(!ptr.is_null());
    assert_eq!(len, 3);
    // SAFETY: ptr is a boxed slice of exactly `len` handles.
    unsafe {
        let slice = std::slice::from_raw_parts(ptr, len as usize);
        let mut vals = slice.to_vec();
        vals.sort_unstable();
        assert_eq!(vals, vec![1, 2, 3]);
    }
    rt::mimi_set_list_free(ptr, len);
    rt::mimi_set_destroy(h);
}

#[test]
fn runtime_core_json_deserialize_free_no_capacity_assumption() {
    // Deserialize a mixed int array; the free path must not assume capacity.
    let json = b"[1,2,3]\0";
    let mut len: i64 = -1;
    let buf = rt::mimi_json_deserialize(cstr(json), &mut len, 0);
    assert!(!buf.is_null());
    assert_eq!(len, 3);
    // SAFETY: boxed slice of `len` i64.
    unsafe {
        let slice = std::slice::from_raw_parts(buf as *const i64, len as usize);
        assert_eq!(slice, &[1, 2, 3]);
    }
    rt::mimi_json_deserialize_free(buf, len, 0);
}

// ── Fix #1/#2: from_json list builders use Box + set element_kind ─────

#[test]
fn runtime_core_from_json_list_builders_set_element_kind() {
    // Option<product> builder: elements are heap packs → Record. The
    // regression asserts the struct is Box-allocated (mimi_list_free frees via
    // Box::from_raw — a malloc/Box mismatch would be UB here) and that
    // element_kind is initialized, not garbage.
    let l = rt::mimi_list_from_json_option_product_i64(b"[null,[7,8]]\0".as_ptr() as _, 2);
    assert!(!l.is_null());
    assert_eq!(
        rt::mimi_list_element_kind(l),
        rt::ListElementKind::Record as i8
    );
    rt::mimi_list_free(l, true);

    // Set<product> builder: elements are SetHandles → Set kind. (The element
    // handles are registry-owned; we free only the list shell. Deeper element
    // cleanup is exercised by the runtime-module unit tests, which can reach
    // the private data array.)
    let l2 = rt::mimi_list_from_json_set_product_i64(b"[[1,2],[3,4]]\0".as_ptr() as _, 2);
    assert!(!l2.is_null());
    assert_eq!(
        rt::mimi_list_element_kind(l2),
        rt::ListElementKind::Set as i8
    );
    rt::mimi_list_free(l2, false);

    // Invalid input still yields a Box-allocated list with a valid kind
    // (the old empty() path left element_kind uninitialized).
    let l3 = rt::mimi_list_from_json_option_product_i64(b"not json\0".as_ptr() as _, 2);
    assert!(!l3.is_null());
    assert_eq!(
        rt::mimi_list_element_kind(l3),
        rt::ListElementKind::Record as i8
    );
    rt::mimi_list_free(l3, true);
}

// ── Fix #9: fail-loud json accessors (happy paths only here) ──────────

#[test]
fn runtime_core_json_accessors_happy_paths() {
    let obj = b"{\"name\":\"mimi\",\"count\":3}\0";
    // json_get_string returns the value for a present key.
    let s = rt::json_get_string(cstr(obj), cstr(b"name\0"));
    assert_eq!(take_str(s), "mimi");
    // json_get_int parses integer values.
    assert_eq!(rt::json_get_int(cstr(obj), cstr(b"count\0")), 3);
    // json_has_key distinguishes present/missing without aborting.
    assert_eq!(rt::json_has_key(cstr(obj), cstr(b"name\0")), 1);
    assert_eq!(rt::json_has_key(cstr(obj), cstr(b"nope\0")), 0);
    // json_is_valid_json must NEVER abort: 1 valid, 0 malformed.
    assert_eq!(rt::mimi_is_valid_json(cstr(obj)), 1);
    assert_eq!(rt::mimi_is_valid_json(cstr(b"{oops\0")), 0);
    // json_array_length + json_get_element on a well-formed array.
    let arr = b"[10,\"twenty\",[3]]\0";
    assert_eq!(rt::json_array_length(cstr(arr)), 3);
    assert_eq!(take_str(rt::json_get_element(cstr(arr), 1)), "twenty");
    // (abort paths — parse error / missing key / wrong type / OOB index —
    // are asserted by the central e2e abort harness.)
}

// ── Fix #3: decode_result_err_string probes inner pointer ─────────────
// decode_result_err_string is a private helper; its unit coverage lives in
// src/runtime/mod.rs `audit_wave1_tests` (valid struct decode, unmapped-inner
// sentinel "", absurd-len no-read). Nothing here can reach it directly.
