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
                // C2 complement (audit 2026-08-03): fold negative literals at
                // inference. `-2147483648` parses as Neg(2147483648); the
                // positive literal alone exceeds i32::MAX and value-aware
                // widening (literal.rs) promotes it to i64, breaking i32 call
                // sites (dual_actor_negative_int_flow_payload regressed with E0211).
                // The *folded* value is exactly i32::MIN, so keep it i32 —
                // matches pre-C2 typing and both backends' wrapped value.
                if let Expr::Literal(Lit::Int(v)) = e.unlocated() {
                    if let Some(neg) = v.checked_neg() {
                        if neg >= i32::MIN as i64 && neg <= i32::MAX as i64 {
                            return Type::Name("i32".into(), vec![]);
                        }
                    }
                }
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
                // The managed Any v1 profile admits exactly borrowed
                // equality against String. Keep the dynamic side in the
                // operand's original position; MIR lowering canonicalizes
                // the operand pair later without consuming the owner.
                let managed_any_string = (is_dynamic_any(&lt) && is_string(&rt))
                    || (is_string(&lt) && is_dynamic_any(&rt));
                let has_dynamic_any = is_dynamic_any(&lt) || is_dynamic_any(&rt);
                let compatible = if has_dynamic_any {
                    managed_any_string
                } else {
                    self.unification.unify(&lt, &rt).is_ok()
                        || common_numeric_type(&lt, &rt).is_some()
                };
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
                    // Range<T> carries the element type so resolved lowering
                    // can type the for-loop binding (`for i in 0..4` → i: i32).
                    // Previously bare "Range" left the element type unbound
                    // (0.34.19 CHECKER-GAP: binary sugar in resolved bodies).
                    Type::Name("Range".into(), vec![lt])
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
        }
    }
}

fn is_dynamic_any(ty: &Type) -> bool {
    matches!(ty.unlocated(), Type::Name(name, args) if name == "Any" && args.is_empty())
}

#[cfg(test)]
mod managed_any_string_equality_tests {
    use super::*;

    fn expression(name: &str) -> Expr {
        Expr::Ident(name.to_owned())
    }

    #[test]
    fn equality_accepts_any_string_in_both_operand_orders_only() {
        let file = crate::tests::parse("func main() -> i32 { 0 }");
        let mut checker = Checker::new(&file);
        let mut scopes = vec![HashMap::from([
            ("dynamic".into(), Type::Name("Any".into(), vec![])),
            ("text".into(), Type::Name("string".into(), vec![])),
            ("number".into(), Type::Name("i32".into(), vec![])),
        ])];

        for op in [BinOp::EqCmp, BinOp::NeCmp] {
            for (left, right) in [("dynamic", "text"), ("text", "dynamic")] {
                let result =
                    checker.infer_binary(op, &expression(left), &expression(right), &mut scopes);
                assert!(matches!(result, Type::Name(name, _) if name == "bool"));
                assert!(checker.errors.is_empty(), "{:?}", checker.errors);
            }
        }

        for (left, right) in [("dynamic", "number"), ("number", "dynamic")] {
            let _ = checker.infer_binary(
                BinOp::EqCmp,
                &expression(left),
                &expression(right),
                &mut scopes,
            );
        }
        assert_eq!(checker.errors.len(), 2, "{:?}", checker.errors);
        assert!(checker
            .errors
            .iter()
            .all(|error| error.code.as_deref() == Some(crate::diagnostic::codes::E0202)));
    }

    #[test]
    fn checker_source_accepts_both_any_string_orders_and_keeps_any_i32_closed() {
        let legal = crate::tests::parse(
            r#"
            func main() -> i32 {
                match map_new() {
                    Ok(empty) => {
                        match map_set(empty, "key", "value\0🧪") {
                            Ok(root) => {
                                let pair = map_get(root, "key")
                                let payload = pair.1
                                let eq_forward = payload == "value\0🧪"
                                let eq_reverse = "value\0🧪" == payload
                                let ne_forward = payload != "other"
                                let ne_reverse = "other" != payload
                                drop(pair)
                                drop(payload)
                                drop(empty)
                                drop(root)
                                if eq_forward && eq_reverse && ne_forward && ne_reverse { 0 } else { 1 }
                            },
                            Err(status) => {
                                drop(empty)
                                status
                            },
                        }
                    },
                    Err(status) => status,
                }
            }
            "#,
        );
        crate::core::check_program(&legal).expect("legal Any/String comparison source");

        let illegal = crate::tests::parse(
            r#"
            func main() -> i32 {
                match map_new() {
                    Ok(empty) => {
                        match map_set(empty, "key", "value") {
                            Ok(root) => {
                                let pair = map_get(root, "key")
                                let payload = pair.1
                                let bad_forward = payload == 1
                                let bad_reverse = 1 == payload
                                drop(pair)
                                drop(payload)
                                drop(empty)
                                drop(root)
                                if bad_forward || bad_reverse { 1 } else { 0 }
                            },
                            Err(status) => {
                                drop(empty)
                                status
                            },
                        }
                    },
                    Err(status) => status,
                }
            }
            "#,
        );
        let diagnostics = crate::core::check_program(&illegal)
            .expect_err("Any/I32 comparison must remain rejected");
        let equality_errors = diagnostics
            .iter()
            .filter(|diagnostic| {
                diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0202)
            })
            .count();
        assert_eq!(equality_errors, 2, "{diagnostics:?}");
    }
}
