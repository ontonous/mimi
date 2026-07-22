// ===========================================================================
// v0.29.44 — Software Shadow Memory Tagging (MTE simulation)
// White-paper section 4.2: "软件层面的影子内存（Shadow Memory）"
//
// This module owns the thread-local `SHADOW_MAP` and all `mimi_shadow_*`
// extern "C" entry points (alloc / tag / check / free / dump).
// ===========================================================================

use std::collections::HashMap;

struct ShadowTagInfo {
    tag: u8,
    size: usize,
    label: String,
}

thread_local! {
    static SHADOW_MAP: std::cell::RefCell<HashMap<usize, ShadowTagInfo>> =
        std::cell::RefCell::new(HashMap::new());
}

/// v0.29.44: Allocate memory with a shadow tag.
/// Returns a pointer to the allocated memory, or null on failure.
/// The memory is tracked in the shadow map with the given tag and label.
#[no_mangle]
pub extern "C" fn mimi_shadow_alloc(
    size: usize,
    tag: u8,
    label: *const std::ffi::c_char,
) -> *mut u8 {
    let label_str = if label.is_null() {
        String::new()
    } else {
        unsafe { std::ffi::CStr::from_ptr(label) }
            .to_string_lossy()
            .into_owned()
    };
    let layout = match std::alloc::Layout::from_size_align(size, 8) {
        Ok(l) => l,
        Err(_) => return std::ptr::null_mut(),
    };
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        return ptr;
    }
    SHADOW_MAP.with(|m| {
        m.borrow_mut().insert(
            ptr as usize,
            ShadowTagInfo {
                tag,
                size,
                label: label_str,
            },
        );
    });
    ptr
}

/// v0.29.44: Tag an existing memory region with a shadow tag.
/// Returns 0 on success, -1 if the pointer is not in the shadow map.
#[no_mangle]
pub extern "C" fn mimi_shadow_tag(ptr: *const u8, tag: u8) -> i32 {
    if ptr.is_null() {
        return -1;
    }
    SHADOW_MAP.with(|m| {
        let mut m = m.borrow_mut();
        if let Some(info) = m.get_mut(&(ptr as usize)) {
            info.tag = tag;
            0
        } else {
            -1
        }
    })
}

/// v0.29.44: Check that a pointer's shadow tag matches the expected tag.
/// Returns 1 if tag matches, 0 if mismatch or pointer not tracked.
#[no_mangle]
pub extern "C" fn mimi_shadow_check(ptr: *const u8, expected_tag: u8) -> i32 {
    if ptr.is_null() {
        return 0;
    }
    SHADOW_MAP.with(|m| {
        let m = m.borrow();
        if let Some(info) = m.get(&(ptr as usize)) {
            if info.tag == expected_tag {
                1
            } else {
                0
            }
        } else {
            0
        }
    })
}

/// v0.29.44: Free shadow-tagged memory and remove from shadow map.
#[no_mangle]
pub extern "C" fn mimi_shadow_free(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    SHADOW_MAP.with(|m| {
        if let Some(info) = m.borrow_mut().remove(&(ptr as usize)) {
            // HIGH fix: use match instead of unwrap() on free path.
            // info.size was validated during shadow_alloc, so this should
            // always succeed — but defensive coding on free paths prevents
            // UB if the shadow map is corrupted.
            if let Ok(layout) = std::alloc::Layout::from_size_align(info.size, 8) {
                // SAFETY: ptr was allocated by shadow_alloc with the same
                // layout (size, align=8). dealloc with a mismatched layout
                // is UB, so we skip dealloc if layout reconstruction fails.
                unsafe { std::alloc::dealloc(ptr, layout) };
            }
        }
    });
}

/// v0.29.44: Dump the shadow map as a C string (for MemoryDump population).
/// Format: "ptr=0x... tag=N size=M label=...;ptr=0x... ..."
/// Returns a pointer valid until the next call.
#[no_mangle]
pub extern "C" fn mimi_shadow_dump() -> *const std::ffi::c_char {
    thread_local! {
        static DUMP_CSTR: std::cell::RefCell<Option<std::ffi::CString>> = const { std::cell::RefCell::new(None) };
    }
    DUMP_CSTR.with(|cstr_cell| {
        let mut buf = String::new();
        SHADOW_MAP.with(|m| {
            let map = m.borrow();
            for (ptr, info) in map.iter() {
                if !buf.is_empty() {
                    buf.push(';');
                }
                buf.push_str(&format!(
                    "ptr=0x{:x} tag={} size={} label={}",
                    ptr, info.tag, info.size, info.label
                ));
            }
        });
        *cstr_cell.borrow_mut() =
            Some(std::ffi::CString::new(buf).unwrap_or_else(|_| std::ffi::CString::default()));
        cstr_cell
            .borrow()
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null())
    })
}
