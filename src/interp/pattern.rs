use super::*;

impl<'a> Interpreter<'a> {
    /// Match a pattern in a matching context (match arm, while-let).
    /// Bare constructor names are treated as constructor patterns unless the
    /// name is currently bound as a variable (shadowing).
    pub(crate) fn match_pattern(
        &self,
        pat: &Pattern,
        value: &Value,
    ) -> Option<Vec<(String, Value)>> {
        self.match_pattern_with_mode(pat, value, true)
    }

    /// Match a pattern in a binding context (let, parasteps spawn).
    /// Bare constructor names are always bound as variables.
    pub(crate) fn match_pattern_bind(
        &self,
        pat: &Pattern,
        value: &Value,
    ) -> Option<Vec<(String, Value)>> {
        self.match_pattern_with_mode(pat, value, false)
    }

    fn match_pattern_with_mode(
        &self,
        pat: &Pattern,
        value: &Value,
        allow_constructor: bool,
    ) -> Option<Vec<(String, Value)>> {
        let mut bindings = Vec::new();
        if self.match_pattern_inner(pat, value, allow_constructor, &mut bindings) {
            Some(bindings)
        } else {
            None
        }
    }

    fn match_pattern_inner(
        &self,
        pat: &Pattern,
        value: &Value,
        allow_constructor: bool,
        bindings: &mut Vec<(String, Value)>,
    ) -> bool {
        match pat {
            Pattern::Wildcard => true,
            Pattern::Variable(name) => {
                // Bare identifiers like `Red` are parsed as Pattern::Variable, but at
                // runtime they often denote zero-arity enum constructors. In matching
                // contexts, treat a constructor name as a constructor pattern unless
                // the name is currently bound as a variable (shadowing). In binding
                // contexts (let, spawn), always bind it as a variable.
                let is_bound = self.lookup(name).is_some();
                if allow_constructor && !is_bound && self.constructors.contains_key(name) {
                    if let Value::Variant(vname, _) = value {
                        return vname == name;
                    }
                    return false;
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
                        // If field positions are known (named constructor pattern),
                        // reorder values to match the pattern's field order.
                        // This ensures Foo { x: 5, y: 6 } matches regardless of
                        // the declaration order of x and y in the type definition.
                        if let Some(positions) = self.variant_field_positions.get(name) {
                            // Named constructor pattern: match by field name
                            for (field_name, p) in pats.iter() {
                                // Skip positional placeholder names (_0, _1, ...)
                                if field_name.starts_with('_')
                                    && field_name[1..].parse::<usize>().is_ok()
                                {
                                    // Positional pattern — use index directly
                                    let idx: usize = field_name[1..].parse().unwrap_or(0);
                                    if idx >= vals.len() {
                                        return false;
                                    }
                                    if !self.match_pattern_inner(
                                        p,
                                        &vals[idx],
                                        allow_constructor,
                                        bindings,
                                    ) {
                                        return false;
                                    }
                                } else if let Some(&idx) = positions.get(field_name) {
                                    // Named field — look up position from type definition
                                    if idx >= vals.len() {
                                        return false;
                                    }
                                    if !self.match_pattern_inner(
                                        p,
                                        &vals[idx],
                                        allow_constructor,
                                        bindings,
                                    ) {
                                        return false;
                                    }
                                } else {
                                    // Unknown field name — pattern can't match
                                    return false;
                                }
                            }
                        } else {
                            // No field position info — match by position (legacy behavior)
                            for ((_, p), v) in pats.iter().zip(vals.iter()) {
                                if !self.match_pattern_inner(p, v, allow_constructor, bindings) {
                                    return false;
                                }
                            }
                        }
                        true
                    }
                    // Handle newtype pattern matching: UserId(v) matches Newtype(name, v)
                    Value::Newtype(_name, inner) if pats.len() == 1 => {
                        self.match_pattern_inner(&pats[0].1, inner, allow_constructor, bindings)
                    }
                    _ => false,
                }
            }
            Pattern::Tuple(pats) => match value {
                Value::Tuple(vals) if pats.len() == vals.len() => {
                    for (p, v) in pats.iter().zip(vals.iter()) {
                        if !self.match_pattern_inner(p, v, allow_constructor, bindings) {
                            return false;
                        }
                    }
                    true
                }
                _ => false,
            },
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
                    if !self.match_pattern_inner(p, v, allow_constructor, bindings) {
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
                    if !self.match_pattern_inner(p, v, allow_constructor, bindings) {
                        return false;
                    }
                }
                // Bind rest pattern to remaining elements
                if let Some(rest_pat) = rest {
                    let remaining: Vec<Value> = vals[pats.len()..].to_vec();
                    return self.match_pattern_inner(
                        rest_pat,
                        &Value::List(remaining),
                        allow_constructor,
                        bindings,
                    );
                }
                true
            }
        }
    }
}
