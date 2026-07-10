use super::super::*;

impl<'a> Interpreter<'a> {
    /// Suggest a similar variable or function name for an undefined identifier.
    pub(in crate::interp) fn suggest_similar(&self, name: &str) -> Option<String> {
        let mut candidates: Vec<String> = Vec::new();
        for scope in self.scope_env.env.iter().rev() {
            for var_name in scope.keys() {
                if levenshtein_distance(name, var_name) <= 2 && name != var_name {
                    candidates.push(var_name.clone());
                }
            }
        }
        for func_name in self.file.items.iter().filter_map(|item| {
            if let Item::Func(f) = item {
                Some(&f.name)
            } else {
                None
            }
        }) {
            if levenshtein_distance(name, func_name) <= 2 && name != func_name {
                candidates.push(func_name.clone());
            }
        }
        candidates.sort();
        candidates.dedup();
        candidates.first().cloned()
    }
}

/// Compute Levenshtein edit distance between two strings.
#[allow(clippy::needless_range_loop)]
pub(in crate::interp) fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();
    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    let mut prev = vec![0usize; b_len + 1];
    let mut curr = vec![0usize; b_len + 1];

    for j in 0..=b_len {
        prev[j] = j;
    }

    for (i, ca) in a_chars.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b_chars.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b_len]
}
