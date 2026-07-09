use crate::ast::*;
use crate::core::checker::Checker;
use crate::core::helpers::{fmt_type, is_int, same_type};
use std::collections::HashMap;

impl<'a> Checker<'a> {
    /// Infer the signature of a first-class function expression.
    /// Returns `Some((params, ret))` for `func` / `extern func` values.
    fn infer_callable_sig(
        &mut self,
        expr: &Expr,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Option<(Vec<Type>, Type)> {
        match self.infer_expr(expr, scopes) {
            Type::Func(params, ret) => Some((params, *ret)),
            Type::ExternFunc(params, ret) => Some((params, *ret)),
            _ => None,
        }
    }

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
            "ok_or" => {
                let err_ty = args
                    .first()
                    .map(|arg| self.infer_expr(arg, scopes))
                    .unwrap_or_else(|| Type::Name("unknown".into(), vec![]));
                Type::Result(Box::new((*inner).clone()), Box::new(err_ty))
            }
            "map" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "Option.map expects 1 argument",
                    );
                    return Type::Option(Box::new(Type::Name("unknown".into(), vec![])));
                }
                let (params, ret) = match self.infer_callable_sig(&args[0], scopes) {
                    Some(sig) => sig,
                    None => {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "Option.map expects a function argument",
                        );
                        return Type::Option(Box::new(Type::Name("unknown".into(), vec![])));
                    }
                };
                if let Some(first) = params.first() {
                    if !same_type(first, inner) {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!(
                                "Option.map function expects argument of type {}, found {}",
                                fmt_type(inner),
                                fmt_type(first)
                            ),
                        );
                    }
                }
                Type::Option(Box::new(ret))
            }
            "and_then" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "Option.and_then expects 1 argument",
                    );
                    return Type::Name("unknown".into(), vec![]);
                }
                let (params, ret) = match self.infer_callable_sig(&args[0], scopes) {
                    Some(sig) => sig,
                    None => {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "Option.and_then expects a function argument",
                        );
                        return Type::Name("unknown".into(), vec![]);
                    }
                };
                if let Some(first) = params.first() {
                    if !same_type(first, inner) {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!(
                                "Option.and_then function expects argument of type {}, found {}",
                                fmt_type(inner),
                                fmt_type(first)
                            ),
                        );
                    }
                }
                match &ret {
                    Type::Option(_) => ret,
                    Type::Name(name, args) if name == "Option" && args.len() == 1 => {
                        Type::Option(Box::new(args[0].clone()))
                    }
                    _ => {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!(
                                "Option.and_then function must return Option<_>, found {}",
                                fmt_type(&ret)
                            ),
                        );
                        Type::Option(Box::new(Type::Name("unknown".into(), vec![])))
                    }
                }
            }
            "map_err" => {
                // Option does not have map_err in Rust semantics; keep the legacy
                // behaviour of returning Option<T> so existing code does not break.
                Type::Option(Box::new((*inner).clone()))
            }
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
            "ok_or" => {
                // Result::ok_or is not a standard combinator; treat it as producing Option<T>.
                Type::Option(Box::new((*ok_ty).clone()))
            }
            "map" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "Result.map expects 1 argument",
                    );
                    return Type::Result(
                        Box::new(Type::Name("unknown".into(), vec![])),
                        Box::new((*err_ty).clone()),
                    );
                }
                let (params, ret) = match self.infer_callable_sig(&args[0], scopes) {
                    Some(sig) => sig,
                    None => {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "Result.map expects a function argument",
                        );
                        return Type::Result(
                            Box::new(Type::Name("unknown".into(), vec![])),
                            Box::new((*err_ty).clone()),
                        );
                    }
                };
                if let Some(first) = params.first() {
                    if !same_type(first, ok_ty) {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!(
                                "Result.map function expects argument of type {}, found {}",
                                fmt_type(ok_ty),
                                fmt_type(first)
                            ),
                        );
                    }
                }
                Type::Result(Box::new(ret), Box::new((*err_ty).clone()))
            }
            "and_then" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "Result.and_then expects 1 argument",
                    );
                    return Type::Name("unknown".into(), vec![]);
                }
                let (params, ret) = match self.infer_callable_sig(&args[0], scopes) {
                    Some(sig) => sig,
                    None => {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "Result.and_then expects a function argument",
                        );
                        return Type::Name("unknown".into(), vec![]);
                    }
                };
                if let Some(first) = params.first() {
                    if !same_type(first, ok_ty) {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!(
                                "Result.and_then function expects argument of type {}, found {}",
                                fmt_type(ok_ty),
                                fmt_type(first)
                            ),
                        );
                    }
                }
                match &ret {
                    Type::Result(_, _) => ret,
                    Type::Name(name, args) if name == "Result" && args.len() == 2 => {
                        Type::Result(Box::new(args[0].clone()), Box::new(args[1].clone()))
                    }
                    _ => {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!(
                                "Result.and_then function must return Result<_, _>, found {}",
                                fmt_type(&ret)
                            ),
                        );
                        Type::Result(
                            Box::new(Type::Name("unknown".into(), vec![])),
                            Box::new((*err_ty).clone()),
                        )
                    }
                }
            }
            "map_err" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "Result.map_err expects 1 argument",
                    );
                    return Type::Result(
                        Box::new((*ok_ty).clone()),
                        Box::new(Type::Name("unknown".into(), vec![])),
                    );
                }
                let (params, ret) = match self.infer_callable_sig(&args[0], scopes) {
                    Some(sig) => sig,
                    None => {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "Result.map_err expects a function argument",
                        );
                        return Type::Result(
                            Box::new((*ok_ty).clone()),
                            Box::new(Type::Name("unknown".into(), vec![])),
                        );
                    }
                };
                if let Some(first) = params.first() {
                    if !same_type(first, err_ty) {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!(
                                "Result.map_err function expects argument of type {}, found {}",
                                fmt_type(err_ty),
                                fmt_type(first)
                            ),
                        );
                    }
                }
                Type::Result(Box::new((*ok_ty).clone()), Box::new(ret))
            }
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
                if !args.is_empty() {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "parse_int takes no arguments",
                    );
                }
                Type::Result(
                    Box::new(Type::Name("i64".into(), vec![])),
                    Box::new(Type::Name("string".into(), vec![])),
                )
            }
            "parse_float" => {
                if !args.is_empty() {
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
                    self.emit_code(crate::diagnostic::codes::E0242, "split expects 1 argument");
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
                    self.emit_code(crate::diagnostic::codes::E0242, "repeat expects 1 argument");
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
                Type::Option(Box::new(Type::Name("i32".into(), vec![])))
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

    pub(in crate::core) fn check_set_method(
        &mut self,
        method: &str,
        inner: &Type,
        args: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        match method {
            "size" | "len" => Type::Name("i32".into(), vec![]),
            "is_empty" | "contains" => Type::Name("bool".into(), vec![]),
            "insert" | "remove" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        format!("set.{} expects 1 argument", method),
                    );
                } else {
                    let arg_ty = self.infer_expr(&args[0], scopes);
                    if !same_type(&arg_ty, inner) {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!(
                                "set.{} expected element type {}, found {}",
                                method,
                                fmt_type(inner),
                                fmt_type(&arg_ty)
                            ),
                        );
                    }
                }
                Type::Name("Set".into(), vec![(*inner).clone()])
            }
            "to_list" => Type::Name("List".into(), vec![(*inner).clone()]),
            _ => {
                self.emit_code(
                    crate::diagnostic::codes::E0242,
                    format!("Set<{}> has no method '{}'", fmt_type(inner), method),
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }
}
