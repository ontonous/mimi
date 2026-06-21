use crate::ast::*;
use crate::core::checker::Checker;
use crate::core::helpers::{fmt_type, is_int, same_type};
use std::collections::HashMap;

impl<'a> Checker<'a> {
    pub(in crate::core) fn check_option_method(
        &mut self,
        method: &str,
        inner: &Type,
        args: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        match method {
            "unwrap" | "expect" => {
                if method == "expect" && !args.is_empty() {
                    self.infer_expr(&args[0], scopes);
                }
                (*inner).clone()
            }
            "unwrap_or" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "unwrap_or expects 1 argument",
                    );
                } else {
                    let default = self.infer_expr(&args[0], scopes);
                    if !same_type(&default, inner) {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!(
                                "unwrap_or expected {}, found {}",
                                fmt_type(inner),
                                fmt_type(&default)
                            ),
                        );
                    }
                }
                (*inner).clone()
            }
            "is_some" | "is_none" => Type::Name("bool".into(), vec![]),
            "ok_or" => Type::Result(
                Box::new((*inner).clone()),
                Box::new(Type::Name("unknown".into(), vec![])),
            ),
            "map" => Type::Option(Box::new(Type::Name("unknown".into(), vec![]))),
            "and_then" => Type::Name("unknown".into(), vec![]),
            "map_err" => Type::Option(Box::new((*inner).clone())),
            _ => {
                // Unknown methods are handled by the caller via trait dispatch; this is a fallback
                self.emit_code(
                    crate::diagnostic::codes::E0242,
                    format!("Option<{}> has no method '{}'", fmt_type(inner), method),
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    pub(in crate::core) fn check_result_method(
        &mut self,
        method: &str,
        ok_ty: &Type,
        err_ty: &Type,
        args: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        match method {
            "unwrap" | "expect" => {
                if method == "expect" && !args.is_empty() {
                    self.infer_expr(&args[0], scopes);
                }
                (*ok_ty).clone()
            }
            "unwrap_or" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "unwrap_or expects 1 argument",
                    );
                } else {
                    let default = self.infer_expr(&args[0], scopes);
                    if !same_type(&default, ok_ty) {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!(
                                "unwrap_or expected {}, found {}",
                                fmt_type(ok_ty),
                                fmt_type(&default)
                            ),
                        );
                    }
                }
                (*ok_ty).clone()
            }
            "is_ok" | "is_err" => Type::Name("bool".into(), vec![]),
            "map" => Type::Result(
                Box::new(Type::Name("unknown".into(), vec![])),
                Box::new((*err_ty).clone()),
            ),
            "and_then" => Type::Name("unknown".into(), vec![]),
            "map_err" => Type::Result(
                Box::new((*ok_ty).clone()),
                Box::new(Type::Name("unknown".into(), vec![])),
            ),
            _ => {
                // Unknown methods are handled by the caller via trait dispatch; this is a fallback
                self.emit_code(
                    crate::diagnostic::codes::E0242,
                    format!(
                        "Result<{}, {}> has no method '{}'",
                        fmt_type(ok_ty),
                        fmt_type(err_ty),
                        method
                    ),
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    pub(in crate::core) fn check_string_method(
        &mut self,
        method: &str,
        args: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        match method {
            "len" | "trim" | "to_upper" | "to_lower" => {
                if !args.is_empty() {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        format!("{} takes no arguments", method),
                    );
                }
                match method {
                    "len" => Type::Name("i32".into(), vec![]),
                    _ => Type::Name("string".into(), vec![]),
                }
            }
            "parse_int" => {
                if args.len() != 0 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "parse_int takes no arguments",
                    );
                }
                Type::Result(
                    Box::new(Type::Name("i32".into(), vec![])),
                    Box::new(Type::Name("string".into(), vec![])),
                )
            }
            "parse_float" => {
                if args.len() != 0 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "parse_float takes no arguments",
                    );
                }
                Type::Result(
                    Box::new(Type::Name("f64".into(), vec![])),
                    Box::new(Type::Name("string".into(), vec![])),
                )
            }
            "contains" | "starts_with" | "ends_with" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        format!("{} expects 1 argument", method),
                    );
                } else {
                    let t = self.infer_expr(&args[0], scopes);
                    if !same_type(&t, &Type::Name("string".into(), vec![])) {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects a string argument", method),
                        );
                    }
                }
                Type::Name("bool".into(), vec![])
            }
            "split" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "split expects 1 argument",
                    );
                } else {
                    let t = self.infer_expr(&args[0], scopes);
                    if !same_type(&t, &Type::Name("string".into(), vec![])) {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "split expects a string argument",
                        );
                    }
                }
                Type::Name("List".into(), vec![Type::Name("string".into(), vec![])])
            }
            "replace" => {
                if args.len() != 2 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "replace expects 2 arguments",
                    );
                } else {
                    for a in args {
                        let t = self.infer_expr(a, scopes);
                        if !same_type(&t, &Type::Name("string".into(), vec![])) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                "replace expects string arguments",
                            );
                        }
                    }
                }
                Type::Name("string".into(), vec![])
            }
            "repeat" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "repeat expects 1 argument",
                    );
                } else {
                    let t = self.infer_expr(&args[0], scopes);
                    if !is_int(&t) {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "repeat expects an integer argument",
                        );
                    }
                }
                Type::Name("string".into(), vec![])
            }
            "char_at" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "char_at expects 1 argument",
                    );
                } else {
                    let t = self.infer_expr(&args[0], scopes);
                    if !is_int(&t) {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "char_at expects an integer argument",
                        );
                    }
                }
                Type::Name("string".into(), vec![])
            }
            "substring" => {
                if args.len() != 2 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "substring expects 2 arguments",
                    );
                } else {
                    for a in args {
                        let t = self.infer_expr(a, scopes);
                        if !is_int(&t) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                "substring expects integer arguments",
                            );
                        }
                    }
                }
                Type::Name("string".into(), vec![])
            }
            "index_of" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "index_of expects 1 argument",
                    );
                } else {
                    let t = self.infer_expr(&args[0], scopes);
                    if !same_type(&t, &Type::Name("string".into(), vec![])) {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "index_of expects a string argument",
                        );
                    }
                }
                Type::Name("i32".into(), vec![])
            }
            _ => {
                self.emit_code(
                    crate::diagnostic::codes::E0242,
                    format!("string has no method '{}'", method),
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }

    pub(in crate::core) fn check_list_method(
        &mut self,
        method: &str,
        _args: &[Expr],
        _scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        match method {
            "len" => Type::Name("i32".into(), vec![]),
            _ => {
                self.emit_code(
                    crate::diagnostic::codes::E0242,
                    format!("List has no method '{}'", method),
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }
}
