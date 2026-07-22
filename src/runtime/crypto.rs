//! Mimi runtime crypto + formatting helpers — SHA-256, base64, and scalar
//! to-string / format functions.
//!
//! Extracted verbatim from `runtime/mod.rs` (the `Crypto operations` section)
//! during the 0.1.0 mechanical split (behavior bit-exact). Pure `extern "C"`
//! leaf: no crate-level Rust-path callers, only the parent module's
//! `alloc_c_string` / `cstr_to_string` helpers.

use std::ffi::CStr;

use super::{alloc_c_string, cstr_to_string};

// ─── Crypto operations ─────────────────────────────────────────

/// SHA-256 hash of a NUL-terminated C string — returns hex string (64 chars).
/// Pure Rust implementation, no external dependencies.
///
/// RT-H8 note: CStr stops at the first NUL. For binary data with embedded NULs,
/// use `mimi_sha256_n(data, len)` instead.
#[no_mangle]
pub extern "C" fn mimi_sha256(data: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    let input = if data.is_null() {
        b"".as_slice()
    } else {
        // SAFETY: `data` was checked non-null above.
        unsafe { CStr::from_ptr(data) }.to_bytes()
    };
    let hash = sha256_bytes(input);
    let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
    alloc_c_string(&hex)
}

/// SHA-256 of an explicit byte buffer (handles embedded NULs).
/// Returns a heap hex string (caller frees with mimi_string_free).
#[no_mangle]
pub extern "C" fn mimi_sha256_n(data: *const u8, len: i64) -> *mut std::ffi::c_char {
    if data.is_null() || len <= 0 {
        let hash = sha256_bytes(b"");
        let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
        return alloc_c_string(&hex);
    }
    const MAX: i64 = 64 * 1024 * 1024;
    if len > MAX {
        return std::ptr::null_mut();
    }
    // SAFETY: caller provides `len` readable bytes at `data`.
    let input = unsafe { std::slice::from_raw_parts(data, len as usize) };
    let hash = sha256_bytes(input);
    let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
    alloc_c_string(&hex)
}

pub fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let k: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    // Pre-processing: padding
    let original_len = data.len();
    let bit_len = (original_len as u64) * 8;
    let mut padded = data.to_vec();
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    // Process each 512-bit (64-byte) chunk
    for chunk in padded.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(k[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut result = [0u8; 32];
    for i in 0..8 {
        result[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    result
}

/// Base64 encode — returns allocated C string.
#[no_mangle]
pub extern "C" fn mimi_base64_encode(data: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    let input = if data.is_null() {
        b"".as_slice()
    } else {
        // SAFETY: `data` was checked non-null above.
        unsafe { CStr::from_ptr(data) }.to_bytes()
    };
    let encoded = base64_encode_bytes(input);
    alloc_c_string(&encoded)
}

pub fn base64_encode_bytes(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Base64 decode — returns Result<string, string>.
#[no_mangle]
pub extern "C" fn mimi_base64_decode(data: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    let input = if data.is_null() {
        ""
    } else {
        // SAFETY: `data` was checked non-null above.
        match unsafe { CStr::from_ptr(data) }.to_str() {
            Ok(s) => s,
            Err(_) => return alloc_c_string(""),
        }
    };
    match base64_decode_str(input) {
        Ok(s) => alloc_c_string(&s),
        Err(_) => alloc_c_string(""),
    }
}

#[allow(clippy::result_unit_err)]
pub fn base64_decode_str(input: &str) -> Result<String, ()> {
    const REV: [i8; 128] = {
        let mut table = [-1i8; 128];
        let mut i = 0;
        while i < 26 {
            table[(b'A' + i) as usize] = i as i8;
            i += 1;
        }
        while i < 52 {
            table[(b'a' + i - 26) as usize] = i as i8;
            i += 1;
        }
        while i < 62 {
            table[(b'0' + i - 52) as usize] = i as i8;
            i += 1;
        }
        table[b'+' as usize] = 62;
        table[b'/' as usize] = 63;
        table
    };
    let clean: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'=' && !b.is_ascii_whitespace())
        .collect();
    let mut output = Vec::new();
    for chunk in clean.chunks(4) {
        let mut buf = 0u32;
        let mut bits = 0;
        for &b in chunk {
            if b >= 128 || REV[b as usize] < 0 {
                return Err(());
            }
            buf = (buf << 6) | (REV[b as usize] as u32);
            bits += 6;
        }
        while bits >= 8 {
            bits -= 8;
            output.push((buf >> bits) as u8);
        }
    }
    String::from_utf8(output).map_err(|_| ())
}

#[no_mangle]
pub extern "C" fn mimi_to_string_i64(val: i64) -> *mut std::ffi::c_char {
    alloc_c_string(&val.to_string())
}

#[no_mangle]
pub extern "C" fn mimi_to_string_f64(val: f64) -> *mut std::ffi::c_char {
    alloc_c_string(&val.to_string())
}

#[no_mangle]
/// M15: template string formatting with up to 8 arguments ({}-placeholders).
/// If more than 8 args are needed, callers should concatenate intermediate results.
pub extern "C" fn mimi_str_format(
    num_args: i64,
    template: *const std::ffi::c_char,
    arg0: *const std::ffi::c_char,
    arg1: *const std::ffi::c_char,
    arg2: *const std::ffi::c_char,
    arg3: *const std::ffi::c_char,
    arg4: *const std::ffi::c_char,
    arg5: *const std::ffi::c_char,
    arg6: *const std::ffi::c_char,
    arg7: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    // SAFETY: `template` is used as a fallback if null; caller should pass a valid C string.
    let tmpl = unsafe { cstr_to_string(template) };
    let args = [arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7];
    let mut result = String::new();
    let mut rest = tmpl.as_str();
    let mut arg_idx = 0;
    while let Some(pos) = rest.find("{}") {
        result.push_str(&rest[..pos]);
        if arg_idx < num_args as usize && arg_idx < args.len() {
            // SAFETY: argument pointers are passed to `cstr_to_string` which handles null.
            let arg_str = unsafe { cstr_to_string(args[arg_idx]) };
            result.push_str(&arg_str);
            arg_idx += 1;
        } else {
            result.push_str("{}");
        }
        rest = &rest[pos + 2..];
    }
    result.push_str(rest);
    alloc_c_string(&result)
}
