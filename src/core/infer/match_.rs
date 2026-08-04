use crate::ast::*;
use crate::core::checker::Checker;
use crate::core::helpers::{fmt_type, is_bool};
use crate::diagnostic::Diagnostic;
use std::collections::HashMap;

impl<'a> Checker<'a> {
    pub(in crate::core) fn infer_match_expr(
        &mut self,
        subject: &Expr,
        arms: &[MatchArm],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        let subject_ty = self.infer_expr(subject, scopes);
        if arms.is_empty() {
            self.emit_code(
                crate::diagnostic::codes::E0213,
                "match expression must have at least one arm",
            );
            return Type::Name("unknown".into(), vec![]);
        }

        let mut all_variants = self.get_enum_variants(&subject_ty);
        // Audit 2026-08-05 fix 10 support: surface-spelled `Option<T>` /
        // `Result<T, E>` annotations resolve to `Type::Name("Option", _)` /
        // `Type::Name("Result", _)` (resolve_type does not normalize them),
        // which get_enum_variants does not recognize. Give those subjects
        // their canonical variants so exhaustiveness works for both
        // representations — matching `check_pattern`'s dual-form handling.
        if all_variants.is_empty() {
            match subject_ty.unlocated() {
                Type::Name(n, args) if n == "Option" && args.len() == 1 => {
                    all_variants = vec!["Some".into(), "None".into()];
                }
                Type::Name(n, args) if n == "Result" && args.len() == 2 => {
                    all_variants = vec!["Ok".into(), "Err".into()];
                }
                _ => {}
            }
        }
        // v0.31.25: Multi-target flow transition results — use tracked target
        // states for exhaustiveness checking instead of get_enum_variants
        // (flow states are TypeDefKind::Record, not Enum).
        let multi_target_states: Vec<String> = match subject.unlocated() {
            Expr::Ident(name) => self
                .multi_target_vars
                .get(name)
                .map(|types| {
                    types
                        .iter()
                        .filter_map(|t| match t.unlocated() {
                            Type::Name(n, _) => Some(n.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let effective_variants = if !multi_target_states.is_empty() {
            multi_target_states.clone()
        } else {
            all_variants.clone()
        };
        let mut covered_variants: Vec<String> = Vec::new();
        let mut has_catchall = false;
        let mut result_ty: Option<Type> = None;
        for arm in arms {
            let (pattern_covered, is_catchall) =
                self.pattern_covers_variants(&arm.pat, &subject_ty);
            // Audit 2026-08-05 fix 9: a guarded arm can fail at runtime (the
            // guard evaluates false), leaving its variants unmatched — guarded
            // arms must NOT count toward coverage. A variant stays covered only
            // if some UNguarded arm (or an unguarded wildcard) covers it; the
            // union over unguarded arms is position-independent.
            if arm.guard.is_none() {
                if is_catchall {
                    has_catchall = true;
                }
                for variant in pattern_covered {
                    if !covered_variants.contains(&variant) {
                        covered_variants.push(variant);
                    }
                }
            }

            scopes.push(HashMap::new());
            self.check_pattern(&arm.pat, &subject_ty, scopes);
            if let Some(guard) = &arm.guard {
                let gt = self.infer_expr(guard, scopes);
                if !is_bool(&gt) {
                    self.emit_code(
                        crate::diagnostic::codes::E0216,
                        format!("match guard must be bool, found {}", fmt_type(&gt)),
                    );
                }
            }
            let body_ty = self.infer_expr(&arm.body, scopes);
            scopes.pop();

            match &result_ty {
                None => result_ty = Some(body_ty),
                Some(rt) => {
                    // C2: use unification for match arm type consistency
                    if self.unification.unify(rt, &body_ty).is_err() {
                        self.emit_code(
                            crate::diagnostic::codes::E0214,
                            format!(
                                "match arm body type {} does not match previous {}",
                                fmt_type(&body_ty),
                                fmt_type(rt)
                            ),
                        );
                    }
                }
            }
        }

        if !effective_variants.is_empty() && !has_catchall {
            for variant in &effective_variants {
                if !covered_variants.contains(variant) {
                    self.errors.push(
                        Diagnostic::error_code(
                            crate::diagnostic::codes::E0215,
                            format!(
                                "match expression is not exhaustive: missing variant '{}' of '{}'",
                                variant,
                                fmt_type(&subject_ty)
                            ),
                            self.diagnostic_span(),
                        )
                        .with_help(format!(
                            "add an arm for '{}' or a wildcard '_ => ...' arm",
                            variant
                        )),
                    );
                }
            }
            // v0.31.25: Clear multi_target_vars after exhaustive match —
            // the variable is now safe to use (all states handled).
            if !multi_target_states.is_empty()
                && multi_target_states
                    .iter()
                    .all(|v| covered_variants.contains(v))
            {
                if let Expr::Ident(name) = subject.unlocated() {
                    self.multi_target_vars.remove(name);
                }
            }
        } else if effective_variants.is_empty() && !has_catchall {
            // D3 + audit 2026-08-05 fix 10: the no-wildcard guard used to fire
            // only for i32/i64/f64/string subjects; every other non-enum subject
            // (tuples, newtypes, records, refs, …) silently matched nothing when
            // no arm applied. Extend the diagnostic to all subject types except
            // unresolved inference surfaces, where exhaustiveness is not yet
            // decidable (TypeVar) or the type is an escape hatch / error poison
            // (Infer/TyErr/unknown) — emitting there would only add cascade noise.
            let exempt = match subject_ty.unlocated() {
                Type::TypeVar(_) | Type::Infer | Type::TyErr => true,
                Type::Name(n, _) if n == "unknown" => true,
                _ => false,
            };
            if !exempt {
                self.errors.push(
                    Diagnostic::error_code(
                        crate::diagnostic::codes::E0215,
                        format!(
                            "match on {} type without wildcard '_ => ...' arm may be non-exhaustive",
                            fmt_type(&subject_ty)
                        ),
                        self.diagnostic_span(),
                    )
                    .with_help("add a wildcard '_ => ...' arm to handle unmatched values"),
                );
            }
        }

        result_ty.unwrap_or_else(|| Type::Name("unknown".into(), vec![]))
    }

    /// Determine which variants a pattern covers.
    /// Returns (list of covered variant names, whether this is a catch-all pattern)
    pub(in crate::core) fn pattern_covers_variants(
        &self,
        pat: &Pattern,
        subject_ty: &Type,
    ) -> (Vec<String>, bool) {
        match &pat.kind {
            PatternKind::Wildcard => {
                // Wildcard covers all variants
                let all = self.get_enum_variants(subject_ty);
                (all, true)
            }
            PatternKind::Variable(name) => {
                // Variable pattern: if the name matches an enum variant of the
                // subject type, treat it as a constructor reference rather than
                // a catch-all binding.  This makes `match c { Red => … }` on
                // an enum type `Color { Red, Green, Blue }` count as covering
                // only the `Red` variant instead of all variants.
                let all = self.get_enum_variants(subject_ty);
                if all.contains(name) {
                    (vec![name.clone()], false)
                } else {
                    (all, true)
                }
            }
            PatternKind::Literal(lit) => {
                // Track literal coverage for bool (enum-like) and int/string types
                let covered = match lit {
                    Lit::Bool(true) => vec!["true".into()],
                    Lit::Bool(false) => vec!["false".into()],
                    Lit::Int(n) => {
                        // Track int literals as covered values
                        vec![format!("int:{}", n)]
                    }
                    Lit::String(s) => {
                        // Track string literals as covered values
                        vec![format!("str:{}", s)]
                    }
                    _ => Vec::new(),
                };
                (covered, false)
            }
            PatternKind::Constructor(name, _) => {
                // Constructor pattern covers only that specific variant.
                // Audit 2026-08-05 fix 10 support: a constructor pattern naming
                // the subject's OWN nominal type always matches (flow-state
                // record results, newtypes), so it is a catch-all for that
                // subject. Without this, exhaustive single-constructor matches
                // (e.g. `match u { Dead { tag } => …, Fault { … } => … }` where
                // `u`'s checker type is `Dead`, or `match u { UserId(v) => v }`)
                // would trip the extended no-wildcard guard.
                let self_covering = match subject_ty.unlocated() {
                    Type::Name(sname, _) => {
                        sname == name && self.flow_state_type_names.contains(name.as_str())
                    }
                    Type::Newtype(sname, _) => sname == name,
                    _ => false,
                };
                (vec![name.clone()], self_covering)
            }
            PatternKind::Tuple(pats) => {
                // Tuple pattern - handle both Type::Tuple and Type::Name("Tuple", args)
                let mut covered = Vec::new();
                let elem_types_opt = match subject_ty.unlocated() {
                    Type::Tuple(ts) => Some(ts.as_slice()),
                    Type::Name(n, args) if n == "Tuple" => Some(args.as_slice()),
                    _ => None,
                };
                // Audit 2026-08-05 fix 10 support: a tuple pattern is itself a
                // catch-all iff the arity matches and EVERY sub-pattern is a
                // catch-all (`(a, b)` / `(_, _)` bind any element values). The
                // old code discarded the sub-pattern catch-all flags and always
                // reported false.
                let mut all_catchall = false;
                if let Some(elem_types) = elem_types_opt {
                    if pats.len() == elem_types.len() {
                        all_catchall = true;
                        for (i, p) in pats.iter().enumerate() {
                            let (vars, sub_catchall) =
                                self.pattern_covers_variants(p, &elem_types[i]);
                            if !sub_catchall {
                                all_catchall = false;
                            }
                            for v in vars {
                                if !covered.contains(&v) {
                                    covered.push(v);
                                }
                            }
                        }
                    } else {
                        for (i, p) in pats.iter().enumerate() {
                            if i < elem_types.len() {
                                let (vars, _) = self.pattern_covers_variants(p, &elem_types[i]);
                                for v in vars {
                                    if !covered.contains(&v) {
                                        covered.push(v);
                                    }
                                }
                            }
                        }
                    }
                }
                (covered, all_catchall)
            }
            PatternKind::Array(_) | PatternKind::Slice(_, _) => (Vec::new(), false),
        }
    }
}
