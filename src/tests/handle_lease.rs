//! Map/Set operation-lease, C lifetime-pin, and generation tests.
//!
//! Drives the real runtime entry points (`mimi_map_*` / `mimi_set_*`),
//! not a reimplemented protocol.

use crate::runtime::{
    mimi_handle_last_error, mimi_map_begin_destroy, mimi_map_destroy, mimi_map_finish_destroy,
    mimi_map_generation, mimi_map_lease_acquire, mimi_map_lease_count, mimi_map_lease_release,
    mimi_map_new, mimi_map_set, mimi_map_size, mimi_map_try_size, mimi_set_begin_destroy,
    mimi_set_destroy, mimi_set_finish_destroy, mimi_set_lease_acquire, mimi_set_lease_count,
    mimi_set_lease_release, mimi_set_new, mimi_set_try_size, HANDLE_ERR_DESTROYED,
    HANDLE_ERR_STALE, HANDLE_OK,
};

#[test]
fn map_c_pin_defers_destroy_until_release() {
    let h = mimi_map_new();
    assert_ne!(h, 0);
    let key = c"k".as_ptr() as *const std::ffi::c_char;
    // SAFETY: `h` is the live map handle from `mimi_map_new` and `key` is a NUL-terminated literal outliving the call.
    unsafe { mimi_map_set(h, key, 7) };

    assert_eq!(mimi_map_lease_acquire(h), HANDLE_OK);
    assert_eq!(mimi_map_lease_count(h), 1);

    // Stop new leases. The object must stay allocated while the lease is held.
    assert_eq!(mimi_map_begin_destroy(h), HANDLE_OK);
    let gen = mimi_map_generation(h);
    assert!(gen >= 0, "generation still readable while lease is held");
    assert_eq!(mimi_map_lease_count(h), 1);
    // New acquire is refused; the in-flight lease is the only one.
    let rc = mimi_map_lease_acquire(h);
    assert_eq!(rc, HANDLE_ERR_DESTROYED);
    assert_eq!(mimi_handle_last_error(), HANDLE_ERR_DESTROYED);

    // Release the in-flight lease, then finish destroy (wait-zero + bump).
    assert_eq!(mimi_map_lease_release(h), HANDLE_OK);
    assert_eq!(mimi_map_finish_destroy(h), HANDLE_OK);

    let mut sz = 99i64;
    // SAFETY: destroyed handle — the lease/generation protocol resolves it to the typed HANDLE_ERR_STALE instead of a use-after-free.
    let err = unsafe { mimi_map_try_size(h, &mut sz) };
    assert_eq!(err, HANDLE_ERR_STALE);
    assert_eq!(mimi_handle_last_error(), HANDLE_ERR_STALE);
    assert_eq!(sz, 0);
}

#[test]
fn map_stale_generation_is_typed_error() {
    let h = mimi_map_new();
    let key = c"k".as_ptr() as *const std::ffi::c_char;
    // SAFETY: `h` is the live map handle from `mimi_map_new` and `key` is a NUL-terminated literal outliving the call.
    unsafe { mimi_map_set(h, key, 1) };
    // SAFETY: `h` is the live map handle created above, destroyed exactly once here.
    unsafe { mimi_map_destroy(h) };

    let mut sz = 99i64;
    // SAFETY: destroyed handle — the generation check resolves the stale handle to a typed error, never a dereference.
    let err = unsafe { mimi_map_try_size(h, &mut sz) };
    assert_eq!(
        err, HANDLE_ERR_STALE,
        "stale handle must be a typed error, not UAF / abort-as-success"
    );
    assert_eq!(mimi_handle_last_error(), HANDLE_ERR_STALE);
    // size() on a stale handle must not abort and must not report a live size.
    // SAFETY: stale handle — `mimi_map_size` resolves via the generation check and reports 0 without touching freed storage.
    let live_sz = mimi_map_size(h);
    assert_eq!(live_sz, 0);
}

#[test]
fn set_c_pin_defers_destroy_until_release() {
    let h = mimi_set_new();
    assert_ne!(h, 0);
    assert_eq!(mimi_set_lease_acquire(h), HANDLE_OK);
    assert_eq!(mimi_set_lease_count(h), 1);
    assert_eq!(mimi_set_begin_destroy(h), HANDLE_OK);
    assert_eq!(mimi_set_lease_acquire(h), HANDLE_ERR_DESTROYED);
    assert_eq!(mimi_set_lease_count(h), 1);
    assert_eq!(mimi_set_lease_release(h), HANDLE_OK);
    assert_eq!(mimi_set_finish_destroy(h), HANDLE_OK);

    let mut sz = 99i64;
    // SAFETY: destroyed handle — the lease/generation protocol resolves it to the typed HANDLE_ERR_STALE instead of a use-after-free.
    let err = unsafe { mimi_set_try_size(h, &mut sz) };
    assert_eq!(err, HANDLE_ERR_STALE);
    assert_eq!(mimi_handle_last_error(), HANDLE_ERR_STALE);
}

#[test]
fn set_stale_generation_is_typed_error() {
    let h = mimi_set_new();
    mimi_set_destroy(h);
    let mut sz = 99i64;
    // SAFETY: destroyed handle — the generation check resolves the stale handle to a typed error, never a dereference.
    let err = unsafe { mimi_set_try_size(h, &mut sz) };
    assert_eq!(err, HANDLE_ERR_STALE);
    assert_eq!(mimi_handle_last_error(), HANDLE_ERR_STALE);
}
