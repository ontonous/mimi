//! 0.1.8 Phase C — TransitionEpoch runtime.
//!
//! Drives the shipped `mimi_flow_*` entry points. Not a reimplemented
//! protocol and not a hard-coded expected table: each assertion is a
//! property of the real pack / check / bump / unpack functions.

use crate::runtime::{
    mimi_flow_bump_epoch, mimi_flow_check_epoch, mimi_flow_drop, mimi_flow_epoch,
    mimi_flow_last_error, mimi_flow_pack, mimi_flow_pack_count, mimi_flow_reject_bare_record,
    mimi_flow_unpack, EPOCH_ERR_BARE_RECORD, EPOCH_ERR_STALE, EPOCH_INITIAL, EPOCH_OK,
};

#[test]
fn flow_epoch_pack_roundtrip() {
    let before = mimi_flow_pack_count();
    let h = mimi_flow_pack(42);
    assert_ne!(h, 0, "pack must return a non-null handle");
    assert_eq!(mimi_flow_pack_count(), before + 1);
    assert_eq!(mimi_flow_epoch(h), EPOCH_INITIAL as i64);
    assert_eq!(mimi_flow_unpack(h), 42);
    assert_eq!(mimi_flow_check_epoch(h, EPOCH_INITIAL as i64), EPOCH_OK);
    assert_eq!(mimi_flow_last_error(), EPOCH_OK);
    assert_eq!(mimi_flow_drop(h), EPOCH_OK);
}

#[test]
fn flow_epoch_stale_is_typed_error() {
    let h = mimi_flow_pack(7);
    let e = mimi_flow_epoch(h);
    // Peer holds the previous epoch.
    let rc = mimi_flow_check_epoch(h, e - 1);
    assert_eq!(
        rc, EPOCH_ERR_STALE,
        "old expected epoch must be a typed stale error, not success/UAF"
    );
    assert_eq!(mimi_flow_last_error(), EPOCH_ERR_STALE);
    // Live handle with the real epoch still works.
    assert_eq!(mimi_flow_check_epoch(h, e), EPOCH_OK);
    assert_eq!(mimi_flow_drop(h), EPOCH_OK);
}

#[test]
fn flow_epoch_recover_bump_is_new_epoch() {
    let h = mimi_flow_pack(1);
    let e0 = mimi_flow_epoch(h);
    let h2 = mimi_flow_bump_epoch(h);
    assert_ne!(h2, 0);
    let e1 = mimi_flow_epoch(h2);
    assert!(
        e1 > e0,
        "recover must publish a new TransitionEpoch, got {e1} after {e0}"
    );
    // Old handle bits are stale after recover.
    assert_eq!(mimi_flow_check_epoch(h, e0), EPOCH_ERR_STALE);
    assert_eq!(mimi_flow_last_error(), EPOCH_ERR_STALE);
    assert_eq!(mimi_flow_check_epoch(h2, e1), EPOCH_OK);
    assert_eq!(mimi_flow_drop(h2), EPOCH_OK);
}

#[test]
fn flow_epoch_ffi_bare_record_rejected() {
    let rc = mimi_flow_reject_bare_record(0xDEAD_BEEF);
    assert_eq!(rc, EPOCH_ERR_BARE_RECORD);
    assert_eq!(mimi_flow_last_error(), EPOCH_ERR_BARE_RECORD);
}

#[test]
fn flow_epoch_dropped_handle_is_stale() {
    let h = mimi_flow_pack(9);
    assert_eq!(mimi_flow_drop(h), EPOCH_OK);
    assert_eq!(
        mimi_flow_check_epoch(h, EPOCH_INITIAL as i64),
        EPOCH_ERR_STALE
    );
    assert_eq!(mimi_flow_epoch(h), 0);
    assert_eq!(mimi_flow_last_error(), EPOCH_ERR_STALE);
}
