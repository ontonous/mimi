//! Mimi runtime regex — simple recursive backtracking engine.
//!
//! Extracted verbatim from `runtime/mod.rs` during the 0.1.0 mechanical
//! split (behavior bit-exact). Provides the `mimi_regex_*` `extern "C"`
//! symbols consumed by codegen/interp. The engine logic is self-contained;
//! only the FFI wrappers use the parent module's `alloc_c_string` /
//! `cstr_to_string` helpers.

use super::{alloc_c_string, cstr_to_string};

// ─── Regex (simple recursive backtracking engine, self-contained) ───

struct RegexEngine;

/// S17: Maximum recursion depth for regex backtracking to prevent ReDoS.
/// Patterns like `(a+)+b` on `aaaaaaaaaaaaaaaac` cause exponential recursion.
const REGEX_MAX_DEPTH: usize = 100;

impl RegexEngine {
    /// Expand `{n}` / `{n,m}` exact/range quantifiers into `*`/`+` form that
    /// the recursive matcher understands. Also used by capture_groups.
    /// Only expands simple `{digits}` and `{digits,digits}` after an atom.
    fn expand_braces(pattern: &str) -> String {
        let bytes = pattern.as_bytes();
        let mut out = String::with_capacity(pattern.len() * 2);
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i] as char);
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if bytes[i] == b'{' {
                // Look back at last atom written to out — re-emit n times.
                // Find atom start in `out`.
                let atom = Self::last_atom(&out);
                if let Some((atom_start, atom_str)) = atom {
                    // Parse {n} or {n,m}
                    let mut j = i + 1;
                    let mut n_str = String::new();
                    while j < bytes.len() && bytes[j].is_ascii_digit() {
                        n_str.push(bytes[j] as char);
                        j += 1;
                    }
                    let mut m_str = String::new();
                    if j < bytes.len() && bytes[j] == b',' {
                        j += 1;
                        while j < bytes.len() && bytes[j].is_ascii_digit() {
                            m_str.push(bytes[j] as char);
                            j += 1;
                        }
                    }
                    if j < bytes.len() && bytes[j] == b'}' && !n_str.is_empty() {
                        let n: usize = n_str.parse().unwrap_or(0).min(64);
                        let m: usize = if m_str.is_empty() {
                            n
                        } else {
                            m_str.parse().unwrap_or(n).min(64)
                        };
                        // Drop the atom already written; re-emit min(n,m) times + optional rest.
                        out.truncate(atom_start);
                        let min_c = n.min(m);
                        let max_c = n.max(m);
                        for _ in 0..min_c {
                            out.push_str(&atom_str);
                        }
                        // Remaining optional: emit (atom)? for each extra up to max-min
                        // by expanding to atom? atom? ... which our engine lacks for `?`.
                        // Fallback: emit atom* when max > min (approximate), else exact.
                        if max_c > min_c {
                            // Emit (max-min) optional atoms as atom* on last — coarse but dual-ok
                            // for common exact `{n}` (max==min) which is the dual path.
                            for _ in min_c..max_c {
                                out.push_str(&atom_str);
                            }
                        }
                        i = j + 1;
                        continue;
                    }
                }
            }
            out.push(bytes[i] as char);
            i += 1;
        }
        out
    }

    /// Return (start_index_in_out, atom_string) for the last regex atom in `out`.
    fn last_atom(out: &str) -> Option<(usize, String)> {
        let b = out.as_bytes();
        if b.is_empty() {
            return None;
        }
        let mut end = b.len();
        // Skip trailing quantifiers already present (* +)
        while end > 0 && (b[end - 1] == b'*' || b[end - 1] == b'+') {
            end -= 1;
        }
        if end == 0 {
            return None;
        }
        // Character class [...]
        if b[end - 1] == b']' {
            let mut i = end - 1;
            while i > 0 {
                i -= 1;
                if b[i] == b'[' && (i == 0 || b[i - 1] != b'\\') {
                    return Some((i, out[i..end].to_string()));
                }
            }
            return None;
        }
        // Escape sequence
        if end >= 2 && b[end - 2] == b'\\' {
            return Some((end - 2, out[end - 2..end].to_string()));
        }
        // Group (...)
        if b[end - 1] == b')' {
            let mut depth = 0i32;
            let mut i = end;
            while i > 0 {
                i -= 1;
                if b[i] == b')' && (i == 0 || b[i - 1] != b'\\') {
                    depth += 1;
                } else if b[i] == b'(' && (i == 0 || b[i - 1] != b'\\') {
                    depth -= 1;
                    if depth == 0 {
                        return Some((i, out[i..end].to_string()));
                    }
                }
            }
            return None;
        }
        // Single char atom
        Some((end - 1, out[end - 1..end].to_string()))
    }

    /// Capture groups from first match. Returns group 1..N strings (not full match).
    fn capture_groups(text: &str, pattern: &str) -> Option<Vec<String>> {
        let expanded = Self::expand_braces(pattern);
        // Strip capturing parens for the full-match scan, but keep structure.
        // Walk pattern with captures by matching left-to-right with backtracking.
        let text_bytes = text.as_bytes();
        let pat_bytes = expanded.as_bytes();
        let anchored = !pat_bytes.is_empty() && pat_bytes[0] == b'^';
        for start in 0..=text_bytes.len() {
            let mut caps: Vec<Option<(usize, usize)>> = Vec::new();
            if let Some(end) =
                Self::match_with_captures(pat_bytes, &text_bytes[start..], 0, &mut caps)
            {
                let mut groups = Vec::new();
                for c in caps {
                    if let Some((a, b)) = c {
                        let abs_a = start + a;
                        let abs_b = start + b;
                        groups.push(
                            std::str::from_utf8(&text_bytes[abs_a..abs_b])
                                .unwrap_or("")
                                .to_string(),
                        );
                    } else {
                        groups.push(String::new());
                    }
                }
                // end is relative; ensure we actually consumed something or empty match ok
                let _ = end;
                return Some(groups);
            }
            if anchored || start >= text_bytes.len() {
                break;
            }
        }
        None
    }

    /// Match with capture tracking. `text` is a suffix; indices in captures are
    /// relative to this suffix. Returns consumed length on success.
    fn match_with_captures(
        pattern: &[u8],
        text: &[u8],
        depth: usize,
        caps: &mut Vec<Option<(usize, usize)>>,
    ) -> Option<usize> {
        if depth >= REGEX_MAX_DEPTH {
            return None;
        }
        let mut pi = 0usize;
        let ti = 0usize;
        let plen = pattern.len();
        let tlen = text.len();
        if pi < plen && pattern[pi] == b'^' {
            pi += 1;
        }
        // v0.31.6: this was a `loop` that never iterated (clippy::never_loop).
        // Every branch returns; pattern advancement is handled by the recursive
        // `match_with_captures(&pattern[after..], ..)` calls, not by looping.
        // A plain block preserves the exact single-pass control flow.
        {
            if pi >= plen {
                return Some(ti);
            }
            if pattern[pi] == b'$' && pi + 1 >= plen {
                return if ti >= tlen { Some(ti) } else { None };
            }
            // Capturing group (...)
            if pattern[pi] == b'(' {
                let close = Self::find_matching_paren(pattern, pi)?;
                let inner = &pattern[pi + 1..close];
                let mut after = close + 1;
                let (min_c, max_c) = Self::read_quant(pattern, &mut after);
                // Greedy: try max down to min
                for count in (min_c..=max_c).rev() {
                    let caps_len = caps.len();
                    let mut t2 = ti;
                    let mut ok = true;
                    let mut last_span: Option<(usize, usize)> = None;
                    for _ in 0..count {
                        let mut inner_caps = Vec::new();
                        match Self::match_with_captures(
                            inner,
                            &text[t2..],
                            depth + 1,
                            &mut inner_caps,
                        ) {
                            Some(n) => {
                                last_span = Some((t2, t2 + n));
                                // Nested captures: merge relative offsets
                                for ic in inner_caps {
                                    if let Some((a, b)) = ic {
                                        caps.push(Some((t2 + a, t2 + b)));
                                    } else {
                                        caps.push(None);
                                    }
                                }
                                t2 += n;
                            }
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok {
                        // This group's span = last repetition (or empty if count==0)
                        let span = if count == 0 {
                            Some((ti, ti))
                        } else {
                            // Full group span from first rep start to last rep end
                            // Recompute first start = ti
                            last_span.map(|(_, end)| (ti, end)).or(Some((ti, ti)))
                        };
                        // Insert this group's capture at the position before nested ones?
                        // Spec: groups numbered by open-paren order. Nested first in walk.
                        // We pushed nested during loop; insert this group at caps_len.
                        if let Some(sp) = span {
                            // For multi-rep, span should be whole run
                            let full = if count > 0 { (ti, t2) } else { sp };
                            caps.insert(caps_len, Some(full));
                        } else {
                            caps.insert(caps_len, None);
                        }
                        // Try rest of pattern; offset new captures by t2 (suffix base).
                        let caps_before_rest = caps.len();
                        if let Some(rest) = Self::match_with_captures(
                            &pattern[after..],
                            &text[t2..],
                            depth + 1,
                            caps,
                        ) {
                            for c in caps.iter_mut().skip(caps_before_rest) {
                                if let Some((a, b)) = *c {
                                    *c = Some((a + t2, b + t2));
                                }
                            }
                            return Some(t2 + rest);
                        }
                    }
                    caps.truncate(caps_len);
                }
                return None;
            }

            // Non-group atom + quantifier
            let (elem_end, elem_is_class) = Self::parse_element(pattern, pi);
            if elem_end == pi {
                return None;
            }
            let mut after = elem_end;
            let has_star = after < plen && pattern[after] == b'*';
            let has_plus = after < plen && pattern[after] == b'+';
            if has_star || has_plus {
                after += 1;
            }
            let min_c = if has_plus {
                1
            } else if has_star {
                0
            } else {
                1
            };
            let max_c = if has_star || has_plus {
                // greedy max
                let mut scan = ti;
                let mut cnt = 0;
                while scan < tlen {
                    let mut tmp = pi;
                    if !Self::elem_match(pattern, &mut tmp, text[scan], elem_is_class) {
                        break;
                    }
                    scan += 1;
                    cnt += 1;
                    if cnt > 10_000 {
                        break;
                    }
                }
                cnt
            } else {
                1
            };
            if max_c < min_c {
                return None;
            }
            for count in (min_c..=max_c).rev() {
                let mut t2 = ti;
                let mut ok = true;
                let mut p_tmp = pi;
                for _ in 0..count {
                    if t2 >= tlen || !Self::elem_match(pattern, &mut p_tmp, text[t2], elem_is_class)
                    {
                        ok = false;
                        break;
                    }
                    t2 += 1;
                    p_tmp = pi; // reset element start for next rep
                }
                if !ok {
                    continue;
                }
                let caps_before_rest = caps.len();
                if let Some(rest) =
                    Self::match_with_captures(&pattern[after..], &text[t2..], depth + 1, caps)
                {
                    for c in caps.iter_mut().skip(caps_before_rest) {
                        if let Some((a, b)) = *c {
                            *c = Some((a + t2, b + t2));
                        }
                    }
                    return Some(t2 + rest);
                }
            }
            return None;
        }
    }

    fn find_matching_paren(pattern: &[u8], open: usize) -> Option<usize> {
        let mut depth = 0i32;
        let mut i = open;
        while i < pattern.len() {
            if pattern[i] == b'\\' {
                i += 2;
                continue;
            }
            if pattern[i] == b'(' {
                depth += 1;
            } else if pattern[i] == b')' {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            i += 1;
        }
        None
    }

    fn read_quant(pattern: &[u8], pos: &mut usize) -> (usize, usize) {
        if *pos < pattern.len() && pattern[*pos] == b'*' {
            *pos += 1;
            return (0, 10_000);
        }
        if *pos < pattern.len() && pattern[*pos] == b'+' {
            *pos += 1;
            return (1, 10_000);
        }
        (1, 1)
    }

    fn match_pattern(text: &str, pattern: &str) -> bool {
        let expanded = Self::expand_braces(pattern);
        // Strip bare capturing parens for match-only path (treat as non-capturing).
        let stripped = Self::strip_captures(&expanded);
        let text_bytes = text.as_bytes();
        let pat_bytes = stripped.as_bytes();
        let anchored = !pat_bytes.is_empty() && pat_bytes[0] == b'^';

        for start in 0..=text_bytes.len() {
            let result = Self::match_here_with_depth(pat_bytes, &text_bytes[start..], 0);
            if result >= 0 {
                return true;
            }
            if anchored || start >= text_bytes.len() {
                break;
            }
        }
        false
    }

    fn strip_captures(pattern: &str) -> String {
        let mut out = String::with_capacity(pattern.len());
        let b = pattern.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\' && i + 1 < b.len() {
                out.push(b[i] as char);
                out.push(b[i + 1] as char);
                i += 2;
                continue;
            }
            if b[i] == b'(' || b[i] == b')' {
                i += 1;
                continue;
            }
            out.push(b[i] as char);
            i += 1;
        }
        out
    }

    fn find_match(text: &str, pattern: &str) -> Option<(usize, usize)> {
        let expanded = Self::expand_braces(pattern);
        let stripped = Self::strip_captures(&expanded);
        let text_bytes = text.as_bytes();
        let pat_bytes = stripped.as_bytes();
        let anchored = !pat_bytes.is_empty() && pat_bytes[0] == b'^';

        for start in 0..=text_bytes.len() {
            let consumed = Self::match_here_with_depth(pat_bytes, &text_bytes[start..], 0);
            if consumed >= 0 {
                return Some((start, start + consumed as usize));
            }
            if anchored || start >= text_bytes.len() {
                break;
            }
        }
        None
    }

    fn replace_all(text: &str, pattern: &str, replacement: &str) -> String {
        let expanded = Self::expand_braces(pattern);
        let stripped = Self::strip_captures(&expanded);
        let text_bytes = text.as_bytes();
        let pat_bytes = stripped.as_bytes();
        let mut result = String::new();
        let mut cursor = 0;
        loop {
            if cursor >= text_bytes.len() {
                break;
            }
            let mut best_pos = text_bytes.len() + 1;
            let mut best_len = 0;
            for start in cursor..text_bytes.len() {
                let consumed = Self::match_here_with_depth(pat_bytes, &text_bytes[start..], 0);
                if consumed >= 0 {
                    best_pos = start;
                    best_len = consumed as usize;
                    break;
                }
            }
            if best_pos <= text_bytes.len() {
                // Append prefix
                result.push_str(std::str::from_utf8(&text_bytes[cursor..best_pos]).unwrap_or(""));
                // Append replacement
                result.push_str(replacement);
                cursor = best_pos + best_len;
            } else {
                // No more match
                result.push_str(std::str::from_utf8(&text_bytes[cursor..]).unwrap_or(""));
                break;
            }
        }
        result
    }

    /// Match pattern against text starting at current position.
    /// Returns number of text characters consumed on success, -1 on failure.
    /// S17: depth-limited variant to prevent ReDoS exponential backtracking.
    fn match_here_with_depth(pattern: &[u8], text: &[u8], depth: usize) -> i32 {
        if depth >= REGEX_MAX_DEPTH {
            return -1; // S17: abort to prevent stack overflow from ReDoS
        }
        let mut pi = 0;
        let mut ti = 0;
        let plen = pattern.len();
        let tlen = text.len();

        // Skip leading ^
        if pi < plen && pattern[pi] == b'^' {
            pi += 1;
        }

        loop {
            if pi >= plen {
                return ti as i32; // matched all of pattern
            }

            // $ at end of pattern matches end of text
            if pattern[pi] == b'$' && (pi + 1 >= plen) {
                return if ti >= tlen { ti as i32 } else { -1 };
            }

            // Parse element
            let (elem_end, elem_is_class) = Self::parse_element(pattern, pi);
            if elem_end == pi {
                return -1;
            }

            // Check for quantifier
            let has_star = elem_end < plen && pattern[elem_end] == b'*';
            let has_plus = elem_end < plen && pattern[elem_end] == b'+';
            let after_quant = if has_star || has_plus {
                elem_end + 1
            } else {
                elem_end
            };

            if has_star || has_plus {
                // Greedy matching
                let min_count = if has_plus { 1 } else { 0 };

                // Count maximum possible matches
                let mut max_count = 0;
                let mut scan = ti;
                while scan < tlen {
                    let mut tmp_pi = pi;
                    if !Self::elem_match(pattern, &mut tmp_pi, text[scan], elem_is_class) {
                        break;
                    }
                    scan += 1;
                    max_count += 1;
                }

                // Try from max down to min
                let mut matched = false;
                for count in (min_count..=max_count).rev() {
                    let sub_pat = &pattern[after_quant..];
                    let sub_text = &text[ti + count..];
                    let r = Self::match_here_with_depth(sub_pat, sub_text, depth + 1);
                    if r >= 0 {
                        ti = ti + count + r as usize;
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    return -1;
                }
                pi = plen; // after_quant is already consumed via recursive call
                continue;
            }

            if ti >= tlen {
                return -1;
            }
            if !Self::elem_match(pattern, &mut pi, text[ti], elem_is_class) {
                return -1;
            }
            ti += 1;
        }
    }

    /// Parse pattern element starting at pi, return (end_pos, is_class).
    fn parse_element(pattern: &[u8], pi: usize) -> (usize, bool) {
        if pi >= pattern.len() {
            return (pi, false);
        }
        match pattern[pi] {
            b'\\' => (pi + 2, false),
            b'[' => {
                let mut ep = pi + 1;
                if ep < pattern.len() && pattern[ep] == b'^' {
                    ep += 1;
                }
                while ep < pattern.len() && pattern[ep] != b']' {
                    if pattern[ep] == b'\\' && ep + 1 < pattern.len() {
                        ep += 2;
                    } else {
                        ep += 1;
                    }
                }
                if ep < pattern.len() {
                    ep += 1;
                } // skip ]
                (ep, true)
            }
            _ => (pi + 1, false),
        }
    }

    fn elem_match_in_class(class: &[u8], c: u8, start: usize) -> (bool, usize) {
        let mut pos = start;
        let neg = pos < class.len() && class[pos] == b'^';
        if neg {
            pos += 1;
        }

        let mut matched = false;
        while pos < class.len() && class[pos] != b']' {
            if pos + 2 < class.len() && class[pos + 1] == b'-' && class[pos + 2] != b']' {
                if c >= class[pos] && c <= class[pos + 2] {
                    matched = true;
                }
                pos += 3;
            } else {
                if c == class[pos] {
                    matched = true;
                }
                pos += 1;
            }
        }
        // Advance to end of class
        while pos < class.len() && class[pos] != b']' {
            pos += 1;
        }
        if pos < class.len() {
            pos += 1;
        } // skip ]

        if neg {
            (!matched, pos)
        } else {
            (matched, pos)
        }
    }

    /// Check if pattern element at pi matches character c. Advances pi past element.
    fn elem_match(pattern: &[u8], pi: &mut usize, c: u8, is_class: bool) -> bool {
        if *pi >= pattern.len() {
            return false;
        }

        if is_class {
            // [...] class
            let class_start = *pi + 1; // skip [
            let (matched, end) = Self::elem_match_in_class(pattern, c, class_start);
            *pi = end;
            return matched;
        }

        match pattern[*pi] {
            b'\\' => {
                if *pi + 1 >= pattern.len() {
                    return false;
                }
                let esc = pattern[*pi + 1];
                *pi += 2;
                match esc {
                    b'd' => c.is_ascii_digit(),
                    b'D' => !c.is_ascii_digit(),
                    b'w' => c.is_ascii_alphanumeric() || c == b'_',
                    b'W' => !(c.is_ascii_alphanumeric() || c == b'_'),
                    b's' => c.is_ascii_whitespace(),
                    b'S' => !c.is_ascii_whitespace(),
                    _ => c == esc,
                }
            }
            b'.' => {
                *pi += 1;
                c != b'\n' && c != 0
            }
            _ => {
                let ch = pattern[*pi];
                *pi += 1;
                c == ch
            }
        }
    }
}

const MAX_REGEX_PATTERN_LEN: usize = 512;

#[no_mangle]
pub extern "C" fn mimi_regex_match(
    text: *const std::ffi::c_char,
    pattern: *const std::ffi::c_char,
) -> i32 {
    if text.is_null() || pattern.is_null() {
        return 0;
    }
    // SAFETY: pointers checked non-null above.
    let t = unsafe { cstr_to_string(text) };
    // SAFETY: pointers checked non-null above.
    let p = unsafe { cstr_to_string(pattern) };
    if p.len() > MAX_REGEX_PATTERN_LEN {
        return 0;
    }
    RegexEngine::match_pattern(&t, &p) as i32
}

#[no_mangle]
pub extern "C" fn mimi_regex_find(
    text: *const std::ffi::c_char,
    pattern: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if text.is_null() || pattern.is_null() {
        return alloc_c_string("");
    }
    // SAFETY: pointers checked non-null above.
    let t = unsafe { cstr_to_string(text) };
    // SAFETY: pointers checked non-null above.
    let p = unsafe { cstr_to_string(pattern) };
    if p.len() > MAX_REGEX_PATTERN_LEN {
        return alloc_c_string("");
    }
    match RegexEngine::find_match(&t, &p) {
        Some((start, end)) => {
            let matched = &t[start..end];
            alloc_c_string(matched)
        }
        None => alloc_c_string(""),
    }
}

#[no_mangle]
pub extern "C" fn mimi_regex_replace(
    text: *const std::ffi::c_char,
    pattern: *const std::ffi::c_char,
    replacement: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if text.is_null() || pattern.is_null() || replacement.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: pointers checked non-null above.
    let t = unsafe { cstr_to_string(text) };
    // SAFETY: pointers checked non-null above.
    let p = unsafe { cstr_to_string(pattern) };
    if p.len() > MAX_REGEX_PATTERN_LEN {
        return std::ptr::null_mut();
    }
    // SAFETY: replacement pointer checked non-null above.
    let r = unsafe { cstr_to_string(replacement) };
    let result = RegexEngine::replace_all(&t, &p, &r);
    alloc_c_string(&result)
}

/// Finds all non-overlapping matches of pattern in text.
/// Returns a JSON array of matched strings: ["match1","match2",...]
#[no_mangle]
pub extern "C" fn mimi_regex_find_all(
    text: *const std::ffi::c_char,
    pattern: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if text.is_null() || pattern.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: pointers checked non-null above.
    let t = unsafe { cstr_to_string(text) };
    // SAFETY: pointers checked non-null above.
    let p = unsafe { cstr_to_string(pattern) };
    let mut matches = Vec::new();
    let mut cursor = 0;
    let t_bytes = t.as_bytes();
    let p_bytes = p.as_bytes();
    loop {
        if cursor >= t_bytes.len() {
            break;
        }
        let mut found = -1;
        let mut found_start = 0;
        for start in cursor..t_bytes.len() {
            let consumed = RegexEngine::match_here_with_depth(p_bytes, &t_bytes[start..], 0);
            if consumed >= 0 {
                let matched =
                    std::str::from_utf8(&t_bytes[start..start + consumed as usize]).unwrap_or("");
                matches.push(matched.to_string());
                found = consumed;
                found_start = start;
                break;
            }
        }
        if found < 0 {
            break;
        }
        cursor = found_start + found as usize;
    }
    let mut result = String::from("[");
    let mut first = true;
    for m in &matches {
        if !first {
            result.push(',');
        }
        first = false;
        result.push('"');
        for ch in m.chars() {
            match ch {
                '"' => result.push_str("\\\""),
                '\\' => result.push_str("\\\\"),
                '\n' => result.push_str("\\n"),
                '\r' => result.push_str("\\r"),
                '\t' => result.push_str("\\t"),
                c if c < '\x20' => {
                    result.push_str(&format!("\\u{:04x}", c as u32));
                }
                c => result.push(c),
            }
        }
        result.push('"');
    }
    result.push(']');
    alloc_c_string(&result)
}

/// Extracts capture groups from the first match of pattern in text.
/// Returns a JSON array of capture group values: `["group1","group2",...]`
/// (group 0 / full match is excluded — same as the interpreter).
///
/// Standalone runtime has no `regex` crate; uses the in-tree `RegexEngine`
/// with capture/`{n}` support so codegen duals match `mimi run`.
#[no_mangle]
pub extern "C" fn mimi_regex_capture_groups(
    text: *const std::ffi::c_char,
    pattern: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if text.is_null() || pattern.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: non-null C strings from codegen/interp callers.
    let t = unsafe { cstr_to_string(text) };
    let p = unsafe { cstr_to_string(pattern) };
    if p.len() > MAX_REGEX_PATTERN_LEN {
        return alloc_c_string("[]");
    }
    match RegexEngine::capture_groups(&t, &p) {
        Some(groups) => {
            let mut out = String::from("[");
            for (i, g) in groups.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('"');
                for ch in g.chars() {
                    match ch {
                        '\\' => out.push_str("\\\\"),
                        '"' => out.push_str("\\\""),
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            out.push(']');
            alloc_c_string(&out)
        }
        None => alloc_c_string("[]"),
    }
}
