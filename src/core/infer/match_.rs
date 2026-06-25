use crate::ast::*;
use crate::core::checker::Checker;
use crate::core::helpers::{fmt_type, is_bool};
use crate::diagnostic::Diagnostic;
use crate::span::Span;
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

        let all_variants = self.get_enum_variants(&subject_ty);
        let mut covered_variants: Vec<String> = Vec::new();
        let mut has_catchall = false;
        let mut result_ty: Option<Type> = None;

        for arm in arms {
            let (pattern_covered, is_catchall) =
                self.pattern_covers_variants(&arm.pat, &subject_ty);
            if is_catchall {
                has_catchall = true;
            }
            for variant in pattern_covered {
                if !covered_variants.contains(&variant) {
                    covered_variants.push(variant);
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

        if !all_variants.is_empty() && !has_catchall {
            for variant in &all_variants {
                if !covered_variants.contains(variant) {
                    self.errors.push(
                        Diagnostic::error_code(
                            crate::diagnostic::codes::E0215,
                            format!(
                                "match expression is not exhaustive: missing variant '{}' of '{}'",
                                variant,
                                fmt_type(&subject_ty)
                            ),
                            Span::single(self.current_line, self.current_col),
                        )
                        .with_help(format!(
                            "add an arm for '{}' or a wildcard '_ => ...' arm",
                            variant
                        )),
                    );
                }
            }
        } else if all_variants.is_empty() && !has_catchall {
            // D3: non-enum types (i32, string, etc.) without catch-all — warn
            let is_non_enum = matches!(
                &subject_ty,
                Type::Name(n, _) if matches!(n.as_str(), "i32" | "i64" | "f64" | "string")
            );
            if is_non_enum {
                self.errors.push(
                    Diagnostic::error_code(
                        crate::diagnostic::codes::E0215,
                        format!(
                            "match on {} type without wildcard '_ => ...' arm may be non-exhaustive",
                            fmt_type(&subject_ty)
                        ),
                        Span::single(self.current_line, self.current_col),
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
        match pat {
            Pattern::Wildcard => {
                // Wildcard covers all variants
                let all = self.get_enum_variants(subject_ty);
                (all, true)
            }
            Pattern::Variable(name) => {
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
            Pattern::Literal(lit) => {
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
            Pattern::Constructor(name, _) => {
                // Constructor pattern covers only that specific variant
                (vec![name.clone()], false)
            }
            Pattern::Tuple(pats) => {
                // Tuple pattern - handle both Type::Tuple and Type::Name("Tuple", args)
                let mut covered = Vec::new();
                let elem_types_opt = match subject_ty {
                    Type::Tuple(ts) => Some(ts.as_slice()),
                    Type::Name(n, args) if n == "Tuple" => Some(args.as_slice()),
                    _ => None,
                };
                if let Some(elem_types) = elem_types_opt {
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
                (covered, false)
            }
            Pattern::Array(_) | Pattern::Slice(_, _) => (Vec::new(), false),
        }
    }
}

fn same_type(a: &Type, b: &Type) -> bool {
    crate::core::helpers::same_type(a, b)
}
