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
            let (pattern_covered, is_catchall) = self.pattern_covers_variants(&arm.pat, &subject_ty);
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
                    if !same_type(rt, &body_ty) {
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
                // For bool literals, cover the specific variant (true/false)
                let covered = match lit {
                    Lit::Bool(true) => vec!["true".into()],
                    Lit::Bool(false) => vec!["false".into()],
                    _ => Vec::new(),
                };
                (covered, false)
            }
            Pattern::Constructor(name, _) => {
                // Constructor pattern covers only that specific variant
                (vec![name.clone()], false)
            }
            Pattern::Tuple(pats) => {
                // Tuple pattern - for enum matching, this doesn't directly cover variants
                // but we need to handle nested tuple patterns that might contain constructors
                let mut covered = Vec::new();
                // For tuple patterns matching against enum types, we need the tuple element types
                if let Type::Tuple(elem_types) = subject_ty {
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
