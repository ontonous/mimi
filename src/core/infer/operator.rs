use crate::ast::*;
use crate::core::borrow::BorrowState;
use crate::core::checker::Checker;
use crate::core::helpers::{fmt_type, is_bool, is_int, is_numeric, is_string, same_type, common_numeric_type};
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashMap;

impl<'a> Checker<'a> {
    pub(in crate::core) fn infer_unary(
        &mut self,
        op: UnOp,
        e: &Box<Expr>,
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
                            Span::single(self.current_line, self.current_col),
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
            UnOp::Ref => {
                // Check borrow rules: cannot borrow if already mutably borrowed
                if let Expr::Ident(name) = e.as_ref() {
                    if let Some(BorrowState::BorrowedMut { span }) = self.lookup_borrow(name) {
                        let borrow_span = *span;
                        self.errors.push(
                            Diagnostic::error_code(
                                crate::diagnostic::codes::E0302,
                                format!(
                                    "cannot borrow '{}' as immutable because it is already mutably borrowed",
                                    name
                                ),
                                Span::single(self.current_line, self.current_col),
                            )
                            .with_note("mutable borrow occurs here", borrow_span),
                        );
                    }
                    self.set_borrow(
                        name,
                        BorrowState::BorrowedImm {
                            span: Span::single(self.current_line, self.current_col),
                        },
                    );
                }
                Type::Ref(None, Box::new(t))
            }
            UnOp::RefMut => {
                // Check borrow rules: cannot &mut if already borrowed (imm or mut)
                if let Expr::Ident(name) = e.as_ref() {
                    if let Some(state) = self.lookup_borrow(name) {
                        match state {
                            BorrowState::Unborrowed => {}
                            BorrowState::BorrowedImm { span } => {
                                let borrow_span = *span;
                                self.errors.push(
                                    Diagnostic::error_code(
                                        crate::diagnostic::codes::E0300,
                                        format!(
                                            "cannot borrow '{}' as mutable because it is already immutably borrowed",
                                            name
                                        ),
                                        Span::single(self.current_line, self.current_col),
                                    )
                                    .with_note("immutable borrow occurs here", borrow_span),
                                );
                            }
                            BorrowState::BorrowedMut { span } => {
                                let borrow_span = *span;
                                self.errors.push(
                                    Diagnostic::error_code(
                                        crate::diagnostic::codes::E0301,
                                        format!(
                                            "cannot borrow '{}' as mutable because it is already mutably borrowed",
                                            name
                                        ),
                                        Span::single(self.current_line, self.current_col),
                                    )
                                    .with_note("mutable borrow occurs here", borrow_span),
                                );
                            }
                        }
                    }
                    self.set_borrow(
                        name,
                        BorrowState::BorrowedMut {
                            span: Span::single(self.current_line, self.current_col),
                        },
                    );
                }
                Type::RefMut(None, Box::new(t))
            }
            UnOp::Deref => match &t {
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
                        if let Expr::Literal(Lit::Int(0)) = r {
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
            BinOp::Mod
            | BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::BitXor
            | BinOp::Shl
            | BinOp::Shr => {
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
                            if let Expr::Literal(Lit::Int(0)) = r {
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
                let compatible = same_type(&lt, &rt) || common_numeric_type(&lt, &rt).is_some();
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
                let compatible = common_numeric_type(&lt, &rt).is_some()
                    || (is_string(&lt) && is_string(&rt));
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
                if !same_type(&lt, &rt) || !is_int(&lt) {
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
            BinOp::And | BinOp::Or => panic!("logical operators should be handled above"),
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
