use crate::ast::*;
use crate::core::checker::Checker;
use crate::core::helpers::{common_numeric_type, fmt_type, is_bool, is_int, is_numeric, is_string};
use crate::diagnostic::Diagnostic;
use std::collections::HashMap;

impl<'a> Checker<'a> {
    pub(in crate::core) fn infer_unary(
        &mut self,
        op: UnOp,
        e: &Expr,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        let t = self.infer_expr(e, scopes);
        match op {
            UnOp::Neg => {
                if is_numeric(&t) {
                    t
                } else {
                    self.errors.push(
                        Diagnostic::error_code(
                            crate::diagnostic::codes::E0201,
                            format!("cannot negate {}", fmt_type(&t)),
                            self.diagnostic_span(),
                        )
                        .with_help("negation only works on numeric types (i32, i64, f64)"),
                    );
                    Type::Name("unknown".into(), vec![])
                }
            }
            UnOp::Not => {
                if is_bool(&t) {
                    t
                } else {
                    self.emit_code(
                        crate::diagnostic::codes::E0203,
                        format!("cannot apply ! to {}", fmt_type(&t)),
                    );
                    Type::Name("unknown".into(), vec![])
                }
            }
            // Loan conflicts and lifetimes are validated from the typed CFG after
            // checker zonk. Unary inference only constructs the reference type.
            UnOp::Ref => Type::Ref(None, Box::new(t)),
            UnOp::RefMut => Type::RefMut(None, Box::new(t)),
            UnOp::Deref => match t.unlocated() {
                Type::Ref(_, inner) | Type::RefMut(_, inner) => (**inner).clone(),
                _ => {
                    self.emit_code(
                        crate::diagnostic::codes::E0204,
                        format!("cannot dereference {}", fmt_type(&t)),
                    );
                    Type::Name("unknown".into(), vec![])
                }
            },
        }
    }

    pub(in crate::core) fn infer_binary(
        &mut self,
        op: BinOp,
        l: &Expr,
        r: &Expr,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        // short-circuit logic
        if op == BinOp::And || op == BinOp::Or {
            let lt = self.infer_expr(l, scopes);
            let rt = self.infer_expr(r, scopes);
            if !is_bool(&lt) || !is_bool(&rt) {
                self.emit_code(
                    crate::diagnostic::codes::E0202,
                    format!(
                        "logical operator requires bool operands, found {} and {}",
                        fmt_type(&lt),
                        fmt_type(&rt)
                    ),
                );
            }
            return Type::Name("bool".into(), vec![]);
        }

        let lt = self.infer_expr(l, scopes);
        let rt = self.infer_expr(r, scopes);

        match op {
            BinOp::Add => {
                // String concatenation: string + string -> string
                if is_string(&lt) && is_string(&rt) {
                    Type::Name("string".into(), vec![])
                } else if let Some(t) = common_numeric_type(&lt, &rt) {
                    t
                } else {
                    self.emit_code(
                        crate::diagnostic::codes::E0202,
                        format!(
                            "arithmetic operator requires matching numeric types, found {} and {}",
                            fmt_type(&lt),
                            fmt_type(&rt)
                        ),
                    );
                    Type::Name("unknown".into(), vec![])
                }
            }
            BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow => {
                if let Some(t) = common_numeric_type(&lt, &rt) {
                    // Static divide-by-zero detection
                    if op == BinOp::Div {
                        if let Expr::Literal(Lit::Int(0)) = r.unlocated() {
                            self.emit_code(
                                crate::diagnostic::codes::E0237,
                                "division by zero literal".to_string(),
                            );
                        }
                    }
                    t
                } else {
                    self.emit_code(
                        crate::diagnostic::codes::E0202,
                        format!(
                            "arithmetic operator requires matching numeric types, found {} and {}",
                            fmt_type(&lt),
                            fmt_type(&rt)
                        ),
                    );
                    Type::Name("unknown".into(), vec![])
                }
            }
            BinOp::Mod | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                if let Some(t) = common_numeric_type(&lt, &rt) {
                    if !is_int(&t) {
                        self.emit_code(
                            crate::diagnostic::codes::E0202,
                            format!(
                                "operator requires integer types, found {} and {}",
                                fmt_type(&lt),
                                fmt_type(&rt)
                            ),
                        );
                        Type::Name("unknown".into(), vec![])
                    } else {
                        // Static modulo-by-zero detection
                        if op == BinOp::Mod {
                            if let Expr::Literal(Lit::Int(0)) = r.unlocated() {
                                self.emit_code(
                                    crate::diagnostic::codes::E0238,
                                    "modulo by zero literal".to_string(),
                                );
                            }
                        }
                        t
                    }
                } else {
                    self.emit_code(
                        crate::diagnostic::codes::E0202,
                        format!(
                            "operator requires matching integer types, found {} and {}",
                            fmt_type(&lt),
                            fmt_type(&rt)
                        ),
                    );
                    Type::Name("unknown".into(), vec![])
                }
            }
            BinOp::EqCmp | BinOp::NeCmp => {
                // IF-H1: try unify first so TypeVars resolve; fall back to
                // numeric common type for i32/i64 mixed comparisons.
                let compatible = self.unification.unify(&lt, &rt).is_ok()
                    || common_numeric_type(&lt, &rt).is_some();
                if !compatible {
                    self.emit_code(
                        crate::diagnostic::codes::E0202,
                        format!(
                            "equality requires matching types, found {} and {}",
                            fmt_type(&lt),
                            fmt_type(&rt)
                        ),
                    );
                }
                Type::Name("bool".into(), vec![])
            }
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                let compatible =
                    common_numeric_type(&lt, &rt).is_some() || (is_string(&lt) && is_string(&rt));
                if !compatible {
                    self.emit_code(
                        crate::diagnostic::codes::E0202,
                        format!(
                            "comparison requires matching numeric or string types, found {} and {}",
                            fmt_type(&lt),
                            fmt_type(&rt)
                        ),
                    );
                }
                Type::Name("bool".into(), vec![])
            }
            BinOp::Range => {
                let range_ok = self.unification.unify(&lt, &rt).is_ok() && is_int(&lt);
                if !range_ok {
                    self.emit_code(
                        crate::diagnostic::codes::E0202,
                        format!(
                            "range requires matching integer types, found {} and {}",
                            fmt_type(&lt),
                            fmt_type(&rt)
                        ),
                    );
                    Type::Name("unknown".into(), vec![])
                } else {
                    Type::Name("Range".into(), vec![])
                }
            }
            BinOp::And | BinOp::Or => {
                // Logical operators are short-circuited at the expr level
                // before reaching infer_binary. If we get here it means the
                // dispatch missed a case. In debug builds the assertion fires;
                // in release we degrade to `unknown` instead of panicking so
                // the compiler can keep emitting diagnostics instead of ICE.
                mimi_debug_assert!(
                    false,
                    "logical operators should be handled before infer_binary"
                );
                Type::Name("unknown".into(), vec![])
            }
            BinOp::Assign => {
                self.emit_code(
                    crate::diagnostic::codes::E0224,
                    "assignment is not a valid expression in v0.2",
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }
}
