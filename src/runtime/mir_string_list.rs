//! Flat owning String Lists for canonical MIR. No scalar or legacy C-string ABI.

use super::{list_cap, list_string, mimi_list_free, mimi_list_push_i64, ListElementKind, MimiList};
use std::ffi::c_char;

#[cfg(test)]
std::thread_local! {
    static COPY_BUDGET: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    // Counts completed canonical drops and the initialized String boxes they
    // retired. This observes partial-clone rollback without changing the
    // production ABI or allocator behavior.
    static DROP_OBSERVATION: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

fn copy_checkpoint() -> bool {
    #[cfg(test)]
    {
        return COPY_BUDGET.with(|budget| match budget.get() {
            Some(0) => false,
            Some(left) => {
                budget.set(Some(left - 1));
                true
            }
            None => true,
        });
    }
    #[cfg(not(test))]
    true
}

fn valid_header(list: &MimiList) -> bool {
    list.element_kind == ListElementKind::String
        && list.string_abi == list_string::LIST_STRING_ABI_FAT
        && list.owns_data
        && list.len >= 0
        && (list.len == 0 || !list.data.is_null())
        && (!list.has_header || (!list.data.is_null() && list.len <= list_cap(list)))
}

/// Validate ABI v3 before interpreting any element as a length-bearing box.
/// The unsafe caller contract, rather than a mapped-page guess, proves that
/// the list/storage/boxes are live. Payload validity remains length-based.
unsafe fn valid_elements(list: &MimiList) -> bool {
    if !valid_header(list) {
        return false;
    }
    for index in 0..list.len as usize {
        let slot = unsafe { *list.data.add(index) };
        // Every canonical element is a live owning box, including "".
        if slot.is_null() {
            return false;
        }
        let Ok((payload, length)) = (unsafe { list_string::read_mimi_str(slot) }) else {
            return false;
        };
        if length < 0 || length > 64 * 1024 * 1024 || (payload.is_null() && length != 0) {
            return false;
        }
    }
    true
}

/// Allocate a uniquely owned empty canonical String List stamped ABI v3.
#[no_mangle]
pub extern "C" fn mimi_mir_list_new_string() -> *mut MimiList {
    // Allocate through the Rust global allocator so the ordinary MimiList
    // Box destructor remains the matching release, but report OOM to the
    // emitter instead of aborting before its moved-input cleanup can run.
    let list =
        unsafe { std::alloc::alloc(std::alloc::Layout::new::<MimiList>()) }.cast::<MimiList>();
    if !list.is_null() {
        unsafe { list.write(MimiList::new_string_list()) };
    }
    list
}

/// Append an independent ABI-v3 box by copying exactly `length` bytes.
/// The input String remains owned by the caller on both success and failure.
/// A zero result leaves the list unchanged and publishes no partial box.
///
/// # Safety
/// `list` is null or a uniquely owned live canonical String List. For positive
/// `length`, `payload` is readable for that many bytes. Existing list slots
/// are independent live owning ABI-v3 boxes. No concurrent access is allowed.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_list_push_string(
    list: *mut MimiList,
    payload: *const c_char,
    length: i64,
) -> i8 {
    if list.is_null() || !valid_header(unsafe { &*list }) || !copy_checkpoint() {
        return 0;
    }
    let Some(new_len) = (unsafe { (*list).len }).checked_add(1) else {
        return 0;
    };
    let boxed = list_string::box_mimi_str(payload, length);
    if boxed.is_null() {
        return 0;
    }
    // The shared growth routine owns the array allocation and writes an i64
    // slot. It temporarily sets I64; restore the exact String ABI even on a
    // rejected append so the caller can still release the original list.
    unsafe { mimi_list_push_i64(list, boxed as i64) };
    unsafe { (*list).element_kind = ListElementKind::String };
    if unsafe { (*list).len } != new_len {
        unsafe { list_string::free_mimi_str(boxed.cast()) };
        return 0;
    }
    1
}

/// Deep-clone the header, storage, boxes, and exact byte payloads. On failure
/// release every partial output owner; the complete source remains unchanged.
///
/// # Safety
/// `list` is null or a live canonical String List with live ABI-v3 owning
/// boxes and valid storage. It stays immutable during this call. Each result
/// slot owns an independent box/payload and must be dropped exactly once.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_list_clone_string(list: *const MimiList) -> *mut MimiList {
    if list.is_null() || !unsafe { valid_elements(&*list) } {
        return std::ptr::null_mut();
    }
    let clone = mimi_mir_list_new_string();
    if clone.is_null() {
        return clone;
    }
    for index in 0..unsafe { (*list).len } as usize {
        let slot = unsafe { *(*list).data.add(index) };
        let Ok((payload, length)) = (unsafe { list_string::read_mimi_str(slot) }) else {
            unsafe { mimi_mir_list_drop_string(clone) };
            return std::ptr::null_mut();
        };
        if unsafe { mimi_mir_list_push_string(clone, payload.cast_const(), length) } == 0 {
            unsafe { mimi_mir_list_drop_string(clone) };
            return std::ptr::null_mut();
        }
    }
    clone
}

/// Release every owning ABI-v3 box and payload, then the array and header.
///
/// # Safety
/// `list` is null or the unique live owner of a canonical String List. All
/// initialized slots are distinct live ABI-v3 owning boxes; no other owner
/// may free them. Invalid kind/ABI metadata traps before freeing any payload.
#[no_mangle]
pub unsafe extern "C" fn mimi_mir_list_drop_string(list: *mut MimiList) {
    if list.is_null() {
        return;
    }
    if !unsafe { valid_elements(&*list) } {
        super::mir_list_abort(b"[E0800] canonical owned String List ABI is invalid\0");
    }
    #[cfg(test)]
    let initialized_boxes = unsafe { (*list).len as usize };
    unsafe { mimi_list_free(list, true) };
    #[cfg(test)]
    DROP_OBSERVATION.with(|observation| {
        let (drops, boxes) = observation.get();
        observation.set((drops + 1, boxes + initialized_boxes));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn bytes(list: *const MimiList, index: usize) -> Vec<u8> {
        let slot = unsafe { *(*list).data.add(index) };
        let (payload, len) = unsafe { list_string::read_mimi_str(slot) }.expect("ABI-v3 box");
        unsafe { list_string::slot_bytes(payload, len) }.to_vec()
    }

    #[test]
    fn owned_string_list_v3_clone_preserves_nul_utf8_and_independent_payloads() {
        let list = mimi_mir_list_new_string();
        let inputs: &[&[u8]] = &[b"", "A\0雪".as_bytes(), "B\0木".as_bytes()];
        for input in inputs {
            assert_eq!(
                unsafe {
                    mimi_mir_list_push_string(list, input.as_ptr().cast(), input.len() as i64)
                },
                1
            );
        }
        let clone = unsafe { mimi_mir_list_clone_string(list) };
        assert!(!clone.is_null());
        for (index, input) in inputs.iter().enumerate() {
            assert_eq!(unsafe { bytes(clone, index) }, *input);
            assert_ne!(unsafe { *(*list).data.add(index) }, unsafe {
                *(*clone).data.add(index)
            });
        }
        let dropped_first = unsafe { mimi_mir_list_clone_string(list) };
        assert!(!dropped_first.is_null());
        unsafe { mimi_mir_list_drop_string(dropped_first) };
        for (index, input) in inputs.iter().enumerate() {
            assert_eq!(unsafe { bytes(list, index) }, *input);
        }
        unsafe { mimi_mir_list_drop_string(list) };
        for (index, input) in inputs.iter().enumerate() {
            assert_eq!(unsafe { bytes(clone, index) }, *input);
        }
        unsafe { mimi_mir_list_drop_string(clone) };
        let empty = mimi_mir_list_new_string();
        let copy = unsafe { mimi_mir_list_clone_string(empty) };
        assert!(!copy.is_null());
        unsafe {
            mimi_mir_list_drop_string(copy);
            mimi_mir_list_drop_string(empty);
        }
    }

    #[test]
    fn owned_string_list_rejects_legacy_abi_and_rolls_back_partial_clone() {
        let list = mimi_mir_list_new_string();
        for input in [b"one".as_slice(), b"two"] {
            assert_eq!(
                unsafe {
                    mimi_mir_list_push_string(list, input.as_ptr().cast(), input.len() as i64)
                },
                1
            );
        }
        for abi in [list_string::LIST_STRING_ABI_CSTR, 2] {
            unsafe { (*list).string_abi = abi };
            assert!(!unsafe { valid_elements(&*list) });
            assert!(unsafe { mimi_mir_list_clone_string(list) }.is_null());
            assert_eq!(
                unsafe { mimi_mir_list_push_string(list, std::ptr::null(), 0) },
                0
            );
        }
        unsafe { (*list).string_abi = list_string::LIST_STRING_ABI_FAT };
        DROP_OBSERVATION.with(|observation| observation.set((0, 0)));
        COPY_BUDGET.with(|budget| budget.set(Some(1)));
        let clone = unsafe { mimi_mir_list_clone_string(list) };
        COPY_BUDGET.with(|budget| budget.set(None));
        assert!(
            clone.is_null(),
            "second element fails after one successful copied owner"
        );
        assert_eq!(unsafe { bytes(list, 0) }, b"one");
        assert_eq!(unsafe { bytes(list, 1) }, b"two");
        assert_eq!(unsafe { (*list).len }, 2);
        assert_eq!(
            DROP_OBSERVATION.with(|observation| observation.get()),
            (1, 1),
            "the runtime must finish dropping the partial clone with exactly its one published box"
        );
        assert_eq!(
            unsafe { mimi_mir_list_push_string(list, std::ptr::null(), 1) },
            0
        );
        unsafe { mimi_mir_list_drop_string(list) };
        assert_eq!(
            DROP_OBSERVATION.with(|observation| observation.get()),
            (2, 3),
            "after the caller retires the source, both the partial clone and source boxes were dropped once"
        );
    }
}
