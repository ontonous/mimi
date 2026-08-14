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
        // 0.36.41: match arms are ALTERNATIVES — session residual analysis must
        // start each arm from the match-ENTRY state (branch-level reset), not
        // from the previous arm's advanced state. Each arm's residuals are
        // captured and merged after the loop: function-scope endpoints (present
        // at entry) must agree across every arm (divergent → E0425, mirrored
        // on Stmt::If's branch merge); endpoints bound INSIDE an arm pattern
        // are arm-local and are excluded from the merge.
        let pre_match_residuals = self.session_residuals.clone();
        let mut arm_residuals: Vec<HashMap<String, crate::ast::SessionType>> = Vec::new();
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

            self.session_residuals = pre_match_residuals.clone();
            scopes.push(HashMap::new());
            self.check_pattern(&arm.pat, &subject_ty, scopes);
            // 0.36.41: seed residuals for SessionChan bindings introduced by
            // the arm pattern (e.g. `Some(d)` where d: SessionChan<S>) — arm
            // bodies then get full protocol-order checking, and abandonment is
            // caught uniformly (E0425) instead of the untracked skeleton.
            self.seed_pattern_session_residuals(&arm.pat, scopes);
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
            arm_residuals.push(self.session_residuals.clone());

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

        // 0.36.41: match-arm residual merge — arms are alternatives, so the
        // post-match continuation may only assume residuals every arm agrees
        // on. Endpoints tracked at match entry are compared across all arms;
        // any arm that lacks one (transferred away / dropped it) or advances
        // it differently is a divergence → E0425 (fail-closed, mirrors the
        // Stmt::If branch merge). The first arm's map is the merged state
        // (like Stmt::If uses the then-branch after agreement).
        if arms.len() > 1 {
            for key in pre_match_residuals.keys() {
                let mut seen: Option<(&crate::ast::SessionType, usize)> = None;
                for (i, arm_r) in arm_residuals.iter().enumerate() {
                    match arm_r.get(key) {
                        Some(r) => {
                            if let Some((s, si)) = seen {
                                if s != r {
                                    self.emit_code(
                                        crate::diagnostic::codes::E0425,
                                        format!(
                                            "session endpoint '{}' has divergent residuals across \
                                             match arms: arm {} `{}` vs arm {} `{}`",
                                            key,
                                            i,
                                            crate::session::fmt_session(r),
                                            si,
                                            crate::session::fmt_session(s),
                                        ),
                                    );
                                }
                            } else {
                                seen = Some((r, i));
                            }
                        }
                        None => {
                            self.emit_code(
                                crate::diagnostic::codes::E0425,
                                format!(
                                    "session endpoint '{}' is dropped or transferred away in \
                                     match arm {}, while tracked at match entry",
                                    key, i,
                                ),
                            );
                        }
                    }
                }
            }
            self.session_residuals = arm_residuals
                .into_iter()
                .next()
                .unwrap_or(pre_match_residuals);
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

    /// 0.36.41: seed session residuals for SessionChan bindings introduced by
    /// an arm pattern (`Some(d)` where d : SessionChan<S>). `check_pattern`
    /// already places each binding's full type in the scope; bindings whose
    /// type is a session channel get their protocol residual seeded so the arm
    /// body receives full order checking (and abandonment surfaces as E0425).
    /// Mirrors the Let-binding seed in check_stmt.rs.
    fn seed_pattern_session_residuals(
        &mut self,
        pat: &Pattern,
        scopes: &mut [HashMap<String, Type>],
    ) {
        fn leaves<'p>(pat: &'p Pattern, out: &mut Vec<&'p Pattern>) {
            match &pat.kind {
                PatternKind::Variable(_) => out.push(pat),
                PatternKind::Constructor(_, fields) => {
                    for (_, p) in fields {
                        leaves(p, out);
                    }
                }
                PatternKind::Tuple(items)
                | PatternKind::Array(items)
                | PatternKind::Slice(items, _) => {
                    for p in items {
                        leaves(p, out);
                    }
                }
                _ => {}
            }
        }
        let mut vs = Vec::new();
        leaves(pat, &mut vs);
        for p in vs {
            if let PatternKind::Variable(name) = &p.kind {
                let ty = match scopes.last().and_then(|m| m.get(name)) {
                    Some(t) => t.clone(),
                    None => continue,
                };
                if let Type::Name(n, args) = ty.unlocated() {
                    if (n == "SessionChan" || n == "session_chan") && !args.is_empty() {
                        if let Type::Name(sname, _) = args[0].unlocated() {
                            if let Some(body) = self.session_types.get(sname).cloned() {
                                let resolved = crate::session::resolve(&body, &self.session_types)
                                    .unwrap_or(body);
                                self.session_residuals.insert(name.clone(), resolved);
                            }
                        }
                    }
                }
            }
        }
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
