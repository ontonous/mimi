use super::*;

impl<'a> Interpreter<'a> {
    pub(crate) fn match_pattern(&self, pat: &Pattern, value: &Value) -> Option<Vec<(String, Value)>> {
        let mut bindings = Vec::new();
        if self.match_pattern_inner(pat, value, &mut bindings) {
            Some(bindings)
        } else {
            None
        }
    }

    fn match_pattern_inner(&self, pat: &Pattern, value: &Value, bindings: &mut Vec<(String, Value)>) -> bool {
        match pat {
            Pattern::Wildcard => true,
            Pattern::Variable(name) => {
                // Check if this is a zero-arity constructor (enum variant without payload).
                // The parser produces Pattern::Variable for bare identifiers like `Red`,
                // but we must treat them as constructor patterns at runtime.
                // If the name matches a constructor AND the value actually IS that variant,
                // treat as constructor match. Otherwise, fall through to variable binding.
                if self.constructors.contains_key(name) {
                    if let Value::Variant(vname, _) = value {
                        if vname == name {
                            return true;
                        }
                    }
                    // Not actually a constructor match — bind the value to the variable instead.
                }
                bindings.push((name.clone(), value.clone()));
                true
            }
            Pattern::Literal(l) => {
                let expected = match l {
                    Lit::Int(v) => Value::Int(*v),
                    Lit::Float(v) => Value::Float(*v),
                    Lit::Bool(v) => Value::Bool(*v),
                    Lit::String(v) => Value::String(v.clone()),
                    Lit::FString(_) => return false, // f-strings can't be used in patterns
                    Lit::Unit => Value::Unit,
                };
                values_equal(value, &expected)
            }
            Pattern::Constructor(name, pats) => {
                match value {
                    Value::Variant(vname, vals) if vname == name => {
                        if pats.len() != vals.len() {
                            return false;
                        }
                        for (p, v) in pats.iter().zip(vals.iter()) {
                            if !self.match_pattern_inner(p, v, bindings) {
                                return false;
                            }
                        }
                        true
                    }
                    // Handle newtype pattern matching: UserId(v) matches Newtype(v)
                    Value::Newtype(inner) if pats.len() == 1 => {
                        self.match_pattern_inner(&pats[0], inner, bindings)
                    }
                    _ => false,
                }
            }
            Pattern::Tuple(pats) => {
                match value {
                    Value::Tuple(vals) if pats.len() == vals.len() => {
                        for (p, v) in pats.iter().zip(vals.iter()) {
                            if !self.match_pattern_inner(p, v, bindings) {
                                return false;
                            }
                        }
                        true
                    }
                    _ => false,
                }
            }
            Pattern::Array(pats) => {
                let vals = match value {
                    Value::Array(a) => a.as_slice(),
                    Value::List(l) => l.as_slice(),
                    Value::Slice { source, start, end } => &source[*start..*end],
                    _ => return false,
                };
                if pats.len() != vals.len() {
                    return false;
                }
                for (p, v) in pats.iter().zip(vals.iter()) {
                    if !self.match_pattern_inner(p, v, bindings) {
                        return false;
                    }
                }
                true
            }
            Pattern::Slice(pats, rest) => {
                let vals = match value {
                    Value::Array(a) => a.as_slice(),
                    Value::List(l) => l.as_slice(),
                    Value::Slice { source, start, end } => &source[*start..*end],
                    _ => return false,
                };
                if pats.len() > vals.len() {
                    return false;
                }
                // Match prefix patterns
                for (p, v) in pats.iter().zip(vals.iter()) {
                    if !self.match_pattern_inner(p, v, bindings) {
                        return false;
                    }
                }
                // Bind rest pattern to remaining elements
                if let Some(rest_pat) = rest {
                    let remaining: Vec<Value> = vals[pats.len()..].to_vec();
                    return self.match_pattern_inner(rest_pat, &Value::List(remaining), bindings);
                }
                true
            }
        }
    }
}
