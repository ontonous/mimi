use crate::ast::*;
use crate::core::checker::Checker;
use crate::core::helpers::{
    fmt_type, is_bool, is_int, is_json_serializable, is_numeric, is_numeric_coercion, suggest_name,
};
use crate::diagnostic::Diagnostic;
use std::collections::HashMap;

impl<'a> Checker<'a> {
    pub(in crate::core) fn check_call(
        &mut self,
        name: &str,
        args: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        // d1f43c22 shadow adjudication (2026-08-04): runtime resolution order
        // is local > global > builtin (VM compile_call: lookup_var →
        // func_table → builtin_table; codegen lower.rs mirrors it). The
        // builtin dispatch below must NOT fire when a local binding or a
        // user-defined global function shadows the name — otherwise the
        // checker types the call against the builtin signature while both
        // runtimes execute the user's definition (false-positive E02xx,
        // e.g. user `func len(x: i32)` rejected by the builtin `len` arm).
        let shadowed = scopes.iter().rev().any(|scope| scope.contains_key(name))
            || self.funcs.contains_key(name);

        // 0.31.24: Comptime purity enforcement — reject impure calls in comptime functions
        if self.in_comptime && !shadowed {
            if is_impure_builtin(name) {
                self.emit_code(
                    crate::diagnostic::codes::E0242,
                    format!(
                        "comptime function cannot call impure builtin '{}'; \
                         comptime functions must be pure (no I/O, FFI, or allocation)",
                        name
                    ),
                );
            }
            // Also reject extern "C" function calls (FFI)
            if self.extern_funcs.contains(name) {
                self.emit_code(
                    crate::diagnostic::codes::E0242,
                    format!(
                        "comptime function cannot call extern \"C\" function '{}'; \
                         comptime functions must be pure (no FFI calls)",
                        name
                    ),
                );
            }
        }
        // Builtins (only when not shadowed — see the shadow adjudication above).
        'builtin_dispatch: {
            if shadowed {
                break 'builtin_dispatch;
            }
            // U3 (0.35.45): contract-derived arity enforcement — a single
            // generic check driven by the canonical core::builtins::builtin_arity
            // table, so a newly-registered fixed-arity builtin is arity-checked
            // without a bespoke per-arm check. Variadic (usize::MAX) builtins
            // and special-cased arms (e.g. `log` 1–2 args) skip this and keep
            // their own precise rules below.
            if let Some(arity) = crate::core::builtins::builtin_arity(name) {
                if arity != usize::MAX && args.len() != arity {
                    for a in args {
                        self.infer_expr(a, scopes);
                    }
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        format!("{name} expects {arity} argument(s)"),
                    );
                    return Type::TyErr;
                }
            }
            match name {
                "unsafe_cast_protocol" => {
                    // 条款 11 escape hatch — typed at the call site by the
                    // expected dyn type (check_expr). Without a dyn context the
                    // target type is unknowable; a fresh Infer binds to the
                    // surrounding expectation and residual Infer is rejected by
                    // scan_residual, so the user must annotate the target.
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "unsafe_cast_protocol expects 1 argument (the concrete value to cast)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Infer;
                }
                "println" => {
                    for a in args {
                        self.infer_expr(a, scopes);
                    }
                    return Type::Name("unit".into(), vec![]);
                }
                "assert" => {
                    if args.len() != 1 && args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "assert expects 1 or 2 arguments (condition, optional message)",
                        );
                    } else {
                        let t = self.infer_expr(&args[0], scopes);
                        if !is_bool(&t) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!("assert expects bool, found {}", fmt_type(&t)),
                            );
                        }
                        if args.len() == 2 {
                            let msg_ty = self.infer_expr(&args[1], scopes);
                            if !crate::core::helpers::is_string(&msg_ty) {
                                self.emit_code(
                                    crate::diagnostic::codes::E0242,
                                    format!(
                                        "assert message must be a string, found {}",
                                        fmt_type(&msg_ty)
                                    ),
                                );
                            }
                        }
                    }
                    return Type::Name("unit".into(), vec![]);
                }
                "range" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "range expects 2 arguments",
                        );
                    } else {
                        let t1 = self.infer_expr(&args[0], scopes);
                        let t2 = self.infer_expr(&args[1], scopes);
                        if !is_int(&t1) || !is_int(&t2) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                "range expects integer arguments",
                            );
                        }
                    }
                    return Type::Name("List".into(), vec![Type::Name("i32".into(), vec![])]);
                }
                "sqrt" => {
                    if args.len() != 1 {
                        self.emit_code(crate::diagnostic::codes::E0242, "sqrt expects 1 argument");
                    } else {
                        let t = self.infer_expr(&args[0], scopes);
                        if !is_numeric(&t) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                "sqrt expects a numeric argument",
                            );
                        }
                    }
                    return Type::Name("f64".into(), vec![]);
                }
                "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "sinh" | "cosh" | "tanh"
                | "ln" | "log2" | "log10" | "exp" | "exp2" | "cbrt" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 1 argument", name),
                        );
                    } else {
                        let t = self.infer_expr(&args[0], scopes);
                        if !is_numeric(&t) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!("{} expects a numeric argument", name),
                            );
                        }
                    }
                    return Type::Name("f64".into(), vec![]);
                }
                "log" => {
                    if args.is_empty() || args.len() > 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "log expects 1 or 2 arguments",
                        );
                    } else {
                        let t1 = self.infer_expr(&args[0], scopes);
                        if !is_numeric(&t1) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                "log expects a numeric first argument",
                            );
                        }
                        if args.len() == 2 {
                            let t2 = self.infer_expr(&args[1], scopes);
                            if !is_numeric(&t2) {
                                self.emit_code(
                                    crate::diagnostic::codes::E0242,
                                    "log expects a numeric base argument",
                                );
                            }
                        }
                    }
                    return Type::Name("f64".into(), vec![]);
                }
                "atan2" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "atan2 expects 2 arguments",
                        );
                    } else {
                        let t1 = self.infer_expr(&args[0], scopes);
                        let t2 = self.infer_expr(&args[1], scopes);
                        if !is_numeric(&t1) || !is_numeric(&t2) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                "atan2 expects numeric arguments",
                            );
                        }
                    }
                    return Type::Name("f64".into(), vec![]);
                }
                "len" => {
                    if args.len() != 1 {
                        self.emit_code(crate::diagnostic::codes::E0242, "len expects 1 argument");
                    } else {
                        // T-H18: require List/string/Map/Set rather than any type.
                        let arg_ty = self.infer_expr(&args[0], scopes);
                        let ok = matches!(
                            &arg_ty,
                            Type::Name(n, _)
                                if n == "List"
                                    || n == "list"
                                    || n == "string"
                                    || n == "String"
                                    || n == "Map"
                                    || n == "map"
                                    || n == "Set"
                                    || n == "set"
                        );
                        if !ok {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!(
                                    "len expects List/string/Map/Set, found {}",
                                    crate::core::fmt_type(&arg_ty)
                                ),
                            );
                        }
                    }
                    return Type::Name("i32".into(), vec![]);
                }
                "to_string" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "to_string expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                // 0.39.136 usability: these conversion builtins were
                // registered in BOTH backends (codegen dispatch + VM registry)
                // and in the canonical arity/purity tables, but the checker's
                // builtin dispatch had no arms — every user call failed E0401.
                // Seven names were fully unusable as a result.
                "int_to_string" | "float_to_string" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{name} expects 1 argument"),
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "str_trim" | "str_to_upper" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{name} expects 1 argument"),
                        );
                    } else {
                        let t = self.infer_expr(&args[0], scopes);
                        if !crate::core::helpers::is_string(&t) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!("{name} expects a string, found {}", fmt_type(&t)),
                            );
                        }
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "str_starts_with" | "str_ends_with" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{name} expects 2 arguments"),
                        );
                    } else {
                        let t = self.infer_expr(&args[0], scopes);
                        if !crate::core::helpers::is_string(&t) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!("{name} expects a string receiver, found {}", fmt_type(&t)),
                            );
                        }
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                "string_to_int" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "string_to_int expects 1 argument",
                        );
                    } else {
                        let t = self.infer_expr(&args[0], scopes);
                        if !crate::core::helpers::is_string(&t) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!("string_to_int expects a string, found {}", fmt_type(&t)),
                            );
                        }
                    }
                    return Type::Tuple(vec![
                        Type::Name("bool".into(), vec![]),
                        Type::Name("i64".into(), vec![]),
                    ]);
                }
                // 0.39.136: `int`/`float` are registered aliases of
                // to_int/to_float (same VM impls, same canonical arity);
                // mirror both the typing and the native dispatch below.
                "int" => {
                    if args.len() != 1 {
                        self.emit_code(crate::diagnostic::codes::E0242, "int expects 1 argument");
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("i32".into(), vec![]);
                }
                "float" => {
                    if args.len() != 1 {
                        self.emit_code(crate::diagnostic::codes::E0242, "float expects 1 argument");
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("f64".into(), vec![]);
                }
                "to_int" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "to_int expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("i32".into(), vec![]);
                }
                "to_float" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "to_float expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("f64".into(), vec![]);
                }
                "abs" => {
                    if args.len() != 1 {
                        self.emit_code(crate::diagnostic::codes::E0242, "abs expects 1 argument");
                        return Type::Name("unknown".into(), vec![]);
                    }
                    let t = self.infer_expr(&args[0], scopes);
                    if !is_numeric(&t) {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "abs expects a numeric argument",
                        );
                        return Type::Name("unknown".into(), vec![]);
                    }
                    return t;
                }
                "push" => {
                    if args.len() != 2 {
                        self.emit_code(crate::diagnostic::codes::E0242, "push expects 2 arguments");
                    } else {
                        let list_ty = self.infer_expr(&args[0], scopes);
                        let value_ty = self.infer_expr(&args[1], scopes);
                        if let Type::Name(name, elements) = list_ty.unlocated() {
                            if name == "List" && elements.len() == 1 {
                                // T-1 (0.31.49): unify element type with value type.
                                // Previously `let _ =` silently dropped the result,
                                // allowing push(list_of_i32, "hello") to pass.
                                // Slice<X> is compatible with X (slice views are
                                // coerced to their target type at runtime).
                                let value_for_unify = match value_ty.unlocated() {
                                    Type::Slice(inner) => *inner.clone(),
                                    _ => value_ty.clone(),
                                };
                                if self
                                    .unification
                                    .unify(&elements[0], &value_for_unify)
                                    .is_err()
                                {
                                    self.emit_code(
                                        crate::diagnostic::codes::E0242,
                                        format!(
                                            "push expects value of type {}, found {}",
                                            fmt_type(&elements[0]),
                                            fmt_type(&value_ty)
                                        ),
                                    );
                                }
                            }
                        }
                    }
                    // push mutates in place; returns Unit (not List) so block-ending
                    // push() doesn't propagate the list as the block's return value.
                    return Type::Name("unit".into(), vec![]);
                }
                "pop" => {
                    if args.len() != 1 {
                        self.emit_code(crate::diagnostic::codes::E0242, "pop expects 1 argument");
                        return Type::Name("unknown".into(), vec![]);
                    }
                    let list_ty = self.infer_expr(&args[0], scopes);
                    let elem_ty = match list_ty.unlocated() {
                        Type::Name(n, inner) if n == "List" && inner.len() == 1 => inner[0].clone(),
                        _ => Type::Name("unknown".into(), vec![]),
                    };
                    return elem_ty;
                }
                "min" | "max" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 2 arguments", name),
                        );
                        return Type::Name("unknown".into(), vec![]);
                    }
                    let t1 = self.infer_expr(&args[0], scopes);
                    let t2 = self.infer_expr(&args[1], scopes);
                    // IF residual: unify so TypeVars resolve; return resolved type.
                    if self.unification.unify(&t1, &t2).is_err() {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!(
                                "{} expects matching types, found {} and {}",
                                name,
                                fmt_type(&t1),
                                fmt_type(&t2)
                            ),
                        );
                        return Type::Name("unknown".into(), vec![]);
                    }
                    return self.unification.zonk_or_unknown(&t1);
                }
                "contains" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "contains expects 2 arguments",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                // v0.31.6: function-call forms of the string builtins. These were
                // only recognized in method-call position (s.char_at(i)); the
                // verifier's contract language uses function syntax char_at(s, i) /
                // starts_with(s, p), which previously fell through to "undefined
                // function" and left a residual `unknown` type (TOOL-RESOLUTION-001).
                "char_at" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "char_at expects 2 arguments (string, index)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "starts_with" | "ends_with" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 2 arguments (string, string)", name),
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                "assert_eq" | "assert_ne" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 2 arguments", name),
                        );
                    } else {
                        let t1 = self.infer_expr(&args[0], scopes);
                        let t2 = self.infer_expr(&args[1], scopes);
                        if self.unification.unify(&t1, &t2).is_err() {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!(
                                    "{} expects matching types, found {} and {}",
                                    name,
                                    fmt_type(&t1),
                                    fmt_type(&t2)
                                ),
                            );
                        }
                    }
                    return Type::Name("unit".into(), vec![]);
                }
                "assert_approx_eq" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "assert_approx_eq expects 2 arguments",
                        );
                    } else {
                        let t1 = self.infer_expr(&args[0], scopes);
                        let t2 = self.infer_expr(&args[1], scopes);
                        if self.unification.unify(&t1, &t2).is_err() {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!(
                                    "assert_approx_eq expects matching types, found {} and {}",
                                    fmt_type(&t1),
                                    fmt_type(&t2)
                                ),
                            );
                        }
                    }
                    return Type::Name("unit".into(), vec![]);
                }
                "enumerate" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "enumerate expects 1 argument (list)",
                        );
                        return Type::Name("unknown".into(), vec![]);
                    }
                    let list_ty = self.infer_expr(&args[0], scopes);
                    let element = match list_ty.unlocated() {
                        Type::Name(name, elements) if name == "List" && elements.len() == 1 => {
                            elements[0].clone()
                        }
                        _ => {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!("enumerate expects a list, found {}", fmt_type(&list_ty)),
                            );
                            return Type::Name("unknown".into(), vec![]);
                        }
                    };
                    return Type::Name(
                        "List".into(),
                        vec![Type::Tuple(vec![Type::Name("i32".into(), vec![]), element])],
                    );
                }
                "exit" => {
                    if args.len() > 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "exit expects 0 or 1 argument (exit code)",
                        );
                    } else if args.len() == 1 {
                        let t = self.infer_expr(&args[0], scopes);
                        if !is_int(&t) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                "exit expects an integer exit code",
                            );
                        }
                    }
                    return Type::Name("unit".into(), vec![]);
                }
                "lexer" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "lexer expects 1 argument (source string)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "mms_parse" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "parse expects 1 argument (source string)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "args" => {
                    if !args.is_empty() {
                        self.emit_code(crate::diagnostic::codes::E0242, "args expects 0 arguments");
                    }
                    return Type::Name("List".into(), vec![Type::Name("string".into(), vec![])]);
                }
                "getenv" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "getenv expects 1 argument (name)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Result(
                        Box::new(Type::Name("string".into(), vec![])),
                        Box::new(Type::Name("string".into(), vec![])),
                    );
                }
                // Phase D (0.39.75): 收 cap 的 env API——get_env_guarded(name, t)。
                // t: SystemToken 能力门禁，被调用消费。
                "get_env_guarded" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "get_env_guarded expects 2 arguments (name, a SystemToken capability)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        let cap_ty = self.infer_expr(&args[1], scopes);
                        if cap_ty != Type::Name("SystemToken".into(), vec![]) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!(
                                    "get_env_guarded capability slot expects SystemToken, found {}",
                                    fmt_type(&cap_ty)
                                ),
                            );
                        }
                    }
                    return Type::Result(
                        Box::new(Type::Name("string".into(), vec![])),
                        Box::new(Type::Name("string".into(), vec![])),
                    );
                }
                "to_json" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "to_json expects 1 argument",
                        );
                    } else {
                        let arg_ty = self.infer_expr(&args[0], scopes);
                        // CG-H2 (audit): reject complex types at type-check time so the user
                        // gets a clear diagnostic instead of an opaque codegen error.
                        // to_json is only implemented in codegen for primitive scalars and strings.
                        if !is_json_serializable(&arg_ty) {
                            self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!(
                                "to_json: cannot serialize type `{}`; \
                                 only i32/i64/f64/bool/string, List<T>, Map/Set of scalars, \
                                 Option/Result, product tuples, and Record types with serializable fields are supported",
                                crate::core::helpers::fmt_type(&arg_ty)
                            ),
                        );
                        }
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "from_int" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "from_int expects 1 argument",
                        );
                    } else {
                        let t = self.infer_expr(&args[0], scopes);
                        if !is_int(&t) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                "from_int expects an integer argument",
                            );
                        }
                    }
                    return Type::Name("i32".into(), vec![]);
                }
                "input" => {
                    if !args.is_empty() {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "try_input_line" => {
                    if !args.is_empty() {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "try_input_line expects 0 arguments",
                        );
                    }
                    return Type::Result(
                        Box::new(Type::Name("string".into(), vec![])),
                        Box::new(Type::Name("string".into(), vec![])),
                    );
                }

                "map" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "map expects 2 arguments (list, closure)",
                        );
                    } else {
                        let list_ty = self.infer_expr(&args[0], scopes);
                        let elem_ty = match list_ty.unlocated() {
                            Type::Name(_, args) if args.len() == 1 => args[0].clone(),
                            _ => Type::Name("unknown".into(), vec![]),
                        };
                        let closure_ty = self.infer_expr(&args[1], scopes);
                        let ret_ty = match closure_ty.unlocated() {
                            Type::Func(_, ret) => ret.as_ref().clone(),
                            // P1-30: Non-function second argument is an error.
                            _ => {
                                self.emit_code(
                                    crate::diagnostic::codes::E0242,
                                    format!(
                                        "map expects a function as second argument, found {}",
                                        fmt_type(&closure_ty)
                                    ),
                                );
                                elem_ty.clone()
                            }
                        };
                        return Type::Name("List".into(), vec![ret_ty]);
                    }
                    return Type::Name("unknown".into(), vec![]);
                }
                "filter" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "filter expects 2 arguments (list, closure)",
                        );
                    } else {
                        let list_ty = self.infer_expr(&args[0], scopes);
                        let elem_ty = match list_ty.unlocated() {
                            Type::Name(_, args) if args.len() == 1 => args[0].clone(),
                            _ => Type::Name("unknown".into(), vec![]),
                        };
                        // P1-31: Validate that the predicate is a function.
                        let pred_ty = self.infer_expr(&args[1], scopes);
                        if !matches!(pred_ty.unlocated(), Type::Func(..)) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!(
                                "filter expects a predicate function as second argument, found {}",
                                fmt_type(&pred_ty)
                            ),
                            );
                        }
                        return Type::Name("List".into(), vec![elem_ty]);
                    }
                    return Type::Name("unknown".into(), vec![]);
                }
                "reduce" => {
                    if args.len() != 3 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "reduce expects 3 arguments",
                        );
                    } else {
                        let list_ty = self.infer_expr(&args[0], scopes);
                        let func_ty = self.infer_expr(&args[1], scopes);
                        let init_ty = self.infer_expr(&args[2], scopes);
                        // T-4 (0.31.49): validate reduce signature.
                        // reduce(list: List<T>, f: func(U, T) -> U, init: U) -> U
                        // The accumulator type U is independent of element type T.
                        let elem_ty = match list_ty.unlocated() {
                            Type::Name(n, inner) if n == "List" && inner.len() == 1 => {
                                inner[0].clone()
                            }
                            _ => Type::Name("unknown".into(), vec![]),
                        };
                        // Validate the reducer function signature: func(U, T) -> U.
                        if let Type::Func(params, ret) = func_ty.unlocated() {
                            if params.len() == 2 {
                                // Unify accumulator param with init type.
                                if self.unification.unify(&params[0], &init_ty).is_err() {
                                    self.emit_code(
                                        crate::diagnostic::codes::E0242,
                                        format!(
                                        "reduce init type {} does not match accumulator type {}",
                                        fmt_type(&init_ty),
                                        fmt_type(&params[0])
                                    ),
                                    );
                                }
                                // Unify element param with list element type.
                                if self.unification.unify(&params[1], &elem_ty).is_err() {
                                    self.emit_code(
                                    crate::diagnostic::codes::E0242,
                                    format!(
                                        "reduce function element type {} does not match list element type {}",
                                        fmt_type(&params[1]),
                                        fmt_type(&elem_ty)
                                    ),
                                );
                                }
                                // Unify return type with accumulator type.
                                if self.unification.unify(ret, &params[0]).is_err() {
                                    self.emit_code(
                                    crate::diagnostic::codes::E0242,
                                    format!(
                                        "reduce function return type {} does not match accumulator type {}",
                                        fmt_type(ret),
                                        fmt_type(&params[0])
                                    ),
                                );
                                }
                                return init_ty;
                            }
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!(
                                    "reduce expects a function with 2 parameters, found {}",
                                    params.len()
                                ),
                            );
                        } else {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!(
                                    "reduce expects a function as second argument, found {}",
                                    fmt_type(&func_ty)
                                ),
                            );
                        }
                        return init_ty;
                    }
                    return Type::Name("unknown".into(), vec![]);
                }
                "sort" | "reverse" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 1 argument", name),
                        );
                        return Type::Name("unknown".into(), vec![]);
                    }
                    let arg_ty = self.infer_expr(&args[0], scopes);
                    // Extract element type from input list to propagate to result
                    let elem_ty = match arg_ty.unlocated() {
                        Type::Name(n, inner) if n == "List" && inner.len() == 1 => inner[0].clone(),
                        _ => Type::Name("unknown".into(), vec![]),
                    };
                    return Type::Name("List".into(), vec![elem_ty]);
                }
                "flatten" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "flatten expects 1 argument",
                        );
                        return Type::Name("unknown".into(), vec![]);
                    }
                    let list_ty = self.infer_expr(&args[0], scopes);
                    let element = match list_ty.unlocated() {
                        Type::Name(name, outer) if name == "List" && outer.len() == 1 => {
                            match outer[0].unlocated() {
                                Type::Name(inner_name, inner)
                                    if inner_name == "List" && inner.len() == 1 =>
                                {
                                    inner[0].clone()
                                }
                                _ => outer[0].clone(),
                            }
                        }
                        _ => {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!("flatten expects a list, found {}", fmt_type(&list_ty)),
                            );
                            return Type::Name("unknown".into(), vec![]);
                        }
                    };
                    return Type::Name("List".into(), vec![element]);
                }
                "sort_f64" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "sort_f64 expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("List".into(), vec![Type::Name("f64".into(), vec![])]);
                }
                "sort_str" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "sort_str expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("List".into(), vec![Type::Name("string".into(), vec![])]);
                }
                "zip" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "zip expects 2 arguments (list, list)",
                        );
                        return Type::Name("unknown".into(), vec![]);
                    }
                    let left = self.infer_expr(&args[0], scopes);
                    let right = self.infer_expr(&args[1], scopes);
                    let element = |ty: &Type| match ty.unlocated() {
                        Type::Name(name, elements) if name == "List" && elements.len() == 1 => {
                            Some(elements[0].clone())
                        }
                        _ => None,
                    };
                    let (Some(left_element), Some(right_element)) =
                        (element(&left), element(&right))
                    else {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!(
                                "zip expects two lists, found {} and {}",
                                fmt_type(&left),
                                fmt_type(&right)
                            ),
                        );
                        return Type::Name("unknown".into(), vec![]);
                    };
                    return Type::Name(
                        "List".into(),
                        vec![Type::Tuple(vec![left_element, right_element])],
                    );
                }
                "sum" => {
                    if args.len() != 1 {
                        self.emit_code(crate::diagnostic::codes::E0242, "sum expects 1 argument");
                    } else {
                        // P1-32: Infer element type from the list instead of
                        // always returning i32. sum([1.5, 2.5]) should be f64.
                        let list_ty = self.infer_expr(&args[0], scopes);
                        let elem_ty = match list_ty.unlocated() {
                            Type::Name(_, type_args) if type_args.len() == 1 => {
                                type_args[0].clone()
                            }
                            _ => Type::Name("i32".into(), vec![]),
                        };
                        return elem_ty;
                    }
                    return Type::Name("i32".into(), vec![]);
                }
                "pow" => {
                    // V-7 (audit 2026-08-05, closed 2026-08-07): type pow by
                    // its arguments instead of hardcoding f64. Both backends
                    // already compute int×int as CHECKED i64 (VM checked_pow,
                    // codegen __mimi_pow_i64) — the old f64 static type was a
                    // lie that made codegen render `pow(2,60)` as a float
                    // while the VM printed the exact integer (L1 display
                    // divergence). int×int → i64 matches reality; anything
                    // with a float argument stays f64.
                    let mut both_int = false;
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 2 arguments", name),
                        );
                    } else {
                        let t1 = self.infer_expr(&args[0], scopes);
                        let t2 = self.infer_expr(&args[1], scopes);
                        both_int = is_int(&t1) && is_int(&t2);
                    }
                    return if both_int {
                        Type::Name("i64".into(), vec![])
                    } else {
                        Type::Name("f64".into(), vec![])
                    };
                }
                "floor" | "ceil" | "round" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 1 argument", name),
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("f64".into(), vec![]);
                }
                // SD-7 escape hatches: wrapping arithmetic (no overflow trap).
                "wrapping_add" | "wrapping_sub" | "wrapping_mul" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 2 arguments", name),
                        );
                    } else {
                        let t1 = self.infer_expr(&args[0], scopes);
                        let t2 = self.infer_expr(&args[1], scopes);
                        if !is_int(&t1) || !is_int(&t2) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!("{} expects integer arguments", name),
                            );
                        }
                    }
                    return Type::Name("i64".into(), vec![]);
                }
                // SD-9 support: float classification.
                "is_nan" | "is_infinite" | "is_finite" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 1 argument", name),
                        );
                    } else {
                        let t = self.infer_expr(&args[0], scopes);
                        if !is_numeric(&t) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!("{} expects a float argument", name),
                            );
                        }
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                // SD-10 escape hatches: explicit float comparison.
                "is_close" => {
                    if args.len() != 3 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "is_close expects 3 arguments (a, b, epsilon)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                        self.infer_expr(&args[2], scopes);
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                "f64_eq_exact" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "f64_eq_exact expects 2 arguments",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                "random" => {
                    return Type::Name("f64".into(), vec![]);
                }
                "pi" => {
                    return Type::Name("f64".into(), vec![]);
                }
                "now" | "timestamp" | "now_ms" | "timestamp_ms" => {
                    return Type::Name("i64".into(), vec![]);
                }
                "sleep" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "sleep expects 1 argument (milliseconds)",
                        );
                    } else {
                        let t = self.infer_expr(&args[0], scopes);
                        if !is_int(&t) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                "sleep expects an integer argument",
                            );
                        }
                    }
                    return Type::Name("unit".into(), vec![]);
                }
                "type_name" | "type_fields" | "type_variants" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 1 argument", name),
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "keys" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 1 argument", name),
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("List".into(), vec![Type::Name("string".into(), vec![])]);
                }
                "values" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 1 argument", name),
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("List".into(), vec![Type::Name("Any".into(), vec![])]);
                }
                "has_key" => {
                    // has_key can be called as a 2-arg global function (json, key)
                    // or as a 1-arg trait method (key) with implicit self.
                    // Only validate arg count for the 2-arg form; the 1-arg form
                    // is handled by trait method dispatch.
                    if args.len() == 2 {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    } else if args.len() == 1 {
                        self.infer_expr(&args[0], scopes);
                    } else {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "has_key expects 1 or 2 arguments",
                        );
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                "map_new" => {
                    return Type::Name("Record".into(), vec![]);
                }
                "map_get" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "map_get expects 2 arguments (map, key)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Tuple(vec![
                        Type::Name("bool".into(), vec![]),
                        Type::Name("Any".into(), vec![]),
                    ]);
                }
                "map_set" => {
                    if args.len() != 3 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "map_set expects 3 arguments (map, key, value)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                        self.infer_expr(&args[2], scopes);
                    }
                    return Type::Name("Record".into(), vec![]);
                }
                "map_remove" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "map_remove expects 2 arguments (map, key)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("Record".into(), vec![]);
                }
                "map_size" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "map_size expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("i32".into(), vec![]);
                }
                "map_from_list" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "map_from_list expects 1 argument (list of (key, value) tuples)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("Record".into(), vec![]);
                }
                // v0.28.20 — concurrency primitives.
                // 0.37 Phase C slice 1: handles/nominal types instead of bare
                // i64. Runtime representation remains i64, but the checker now
                // keeps Mutex / Channel / AtomicI32 / AtomicI64 / AtomicBool
                // distinct and rejects accidental mixing of handle families.
                "atomic_i32_new" | "atomic_i64_new" | "atomic_bool_new" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{name} expects 1 argument (a value for the atomic)"),
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return match name {
                        "atomic_i32_new" => Type::Name("AtomicI32".into(), vec![]),
                        "atomic_i64_new" => Type::Name("AtomicI64".into(), vec![]),
                        _ => Type::Name("AtomicBool".into(), vec![]),
                    };
                }
                "mutex_new" => {
                    // §2-#14 (audit 2026-08-05): arity is checked both here
                    // and by the generic builtin_arity table.
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "mutex_new expects 1 argument (the initial i64 payload)",
                        );
                    } else {
                        let t = self.infer_expr(&args[0], scopes);
                        if !is_int(&t) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!(
                                    "mutex_new expects an integer payload, found {}",
                                    fmt_type(&t)
                                ),
                            );
                        }
                    }
                    return Type::Name("Mutex".into(), vec![Type::Name("i64".into(), vec![])]);
                }
                "mutex_lock" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "mutex_lock expects 1 argument (a mutex handle)",
                        );
                    } else {
                        let t = self.infer_expr(&args[0], scopes);
                        if t != Type::Name("Mutex".into(), vec![Type::Name("i64".into(), vec![])]) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!("mutex_lock expects Mutex<i64>, found {}", fmt_type(&t)),
                            );
                        }
                    }
                    return Type::Name("MutexGuard".into(), vec![Type::Name("i64".into(), vec![])]);
                }
                // Actor handle queries remain raw i64 handles (actor type
                // identities are separate and already typed by method forms).
                "actor_mailbox_depth" | "actor_is_faulted" | "actor_is_muted" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{name} expects 1 argument"),
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("i64".into(), vec![]);
                }
                // Zero-argument handle factories: channel_new() creates a
                // fresh channel, actor_spawn_count()/actor_max_children()
                // query runtime-wide counters — none take a tracked handle,
                // so the 1-arg guard above must not apply to them.
                // Phase D (0.39.71): make_token() returns a globally unique
                // token id (i64).
                "channel_new" | "actor_spawn_count" | "actor_max_children" | "make_token"
                | "token_channel_new" => {
                    if args.len() != 0 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{name} expects 0 arguments (it queries runtime state, not a handle)"),
                        );
                    }
                    return match name {
                        "channel_new" => {
                            Type::Name("Channel".into(), vec![Type::Name("i64".into(), vec![])])
                        }
                        // Phase D (0.39.72): make_token 返回线性 SystemToken（运行时 i64 柄）。
                        "make_token" => Type::Name("SystemToken".into(), vec![]),
                        // Phase D (0.39.73): 线性 token 通道（跨任务 move，运行时 i64 柄）。
                        "token_channel_new" => Type::Name("TokenChannel".into(), vec![]),
                        _ => Type::Name("i64".into(), vec![]),
                    };
                }
                // Phase D (0.39.72): token_id(t: SystemToken) -> i64 — 消费 t、取唯一 id。
                "token_id" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "token_id expects 1 argument (a SystemToken)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("i64".into(), vec![]);
                }
                "broadcast" => {
                    // broadcast(list, method_name) -> List (Vec of Result / values)
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "broadcast expects 2 arguments (targets, method_name)".to_string(),
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("List".into(), vec![Type::Name("i64".into(), vec![])]);
                }
                // The dynamic string form cannot be compiled safely: codegen needs
                // the actor type at compile time. Keep the typed method form as the
                // single portable API instead of accepting an interpreter-only call.
                "spawn_detached" => {
                    for arg in args {
                        self.infer_expr(arg, scopes);
                    }
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "bare spawn_detached(name) is not portable; use ActorType.spawn_detached()"
                            .to_string(),
                    );
                    return Type::Name("i64".into(), vec![]);
                }
                // v0.29.38: assert_state(flow_instance, state_name) -> unit
                "assert_state" => {
                    for a in args {
                        self.infer_expr(a, scopes);
                    }
                    return Type::Name("unit".into(), vec![]);
                }
                // v0.29.38: inject_fault(flow_instance) -> Fault record
                "inject_fault" => {
                    for a in args {
                        self.infer_expr(a, scopes);
                    }
                    return Type::Name("Fault".into(), vec![]);
                }
                // v0.29.44: shadow memory tagging builtins
                "shadow_alloc" => {
                    for a in args {
                        self.infer_expr(a, scopes);
                    }
                    return Type::Name("i64".into(), vec![]);
                }
                "shadow_tag" => {
                    for a in args {
                        self.infer_expr(a, scopes);
                    }
                    return Type::Name("i32".into(), vec![]);
                }
                "shadow_check" => {
                    for a in args {
                        self.infer_expr(a, scopes);
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                "shadow_free" => {
                    for a in args {
                        self.infer_expr(a, scopes);
                    }
                    return Type::Name("unit".into(), vec![]);
                }
                // v0.29.48: test_sandbox(config) -> List<string>
                "test_sandbox" => {
                    for a in args {
                        self.infer_expr(a, scopes);
                    }
                    return Type::Name("List".into(), vec![Type::Name("string".into(), vec![])]);
                }
                "atomic_i32_load" | "atomic_i32_compare_exchange" | "atomic_i32_fetch_add" => {
                    if args.is_empty() {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects at least 1 argument", name),
                        );
                    } else {
                        let handle = self.infer_expr(&args[0], scopes);
                        if handle != Type::Name("AtomicI32".into(), vec![]) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!("{} expects AtomicI32, found {}", name, fmt_type(&handle)),
                            );
                        }
                        for a in &args[1..] {
                            let t = self.infer_expr(a, scopes);
                            if !is_int(&t) {
                                self.emit_code(
                                    crate::diagnostic::codes::E0242,
                                    format!(
                                        "{} value slot expects integer, found {}",
                                        name,
                                        fmt_type(&t)
                                    ),
                                );
                            }
                        }
                    }
                    return Type::Name("i32".into(), vec![]);
                }
                "atomic_i64_compare_exchange" => {
                    if args.len() != 3 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 3 arguments (handle, expected, desired)", name),
                        );
                    } else {
                        let handle = self.infer_expr(&args[0], scopes);
                        if handle != Type::Name("AtomicI64".into(), vec![]) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!("{} expects AtomicI64, found {}", name, fmt_type(&handle)),
                            );
                        }
                        for a in &args[1..] {
                            let t = self.infer_expr(a, scopes);
                            if !is_int(&t) {
                                self.emit_code(
                                    crate::diagnostic::codes::E0242,
                                    format!(
                                        "{} value slot expects integer, found {}",
                                        name,
                                        fmt_type(&t)
                                    ),
                                );
                            }
                        }
                    }
                    // compare_exchange returns 1/0 (same as AtomicI32 CAS).
                    return Type::Name("i32".into(), vec![]);
                }
                "atomic_i64_load" | "atomic_i64_fetch_add" => {
                    if args.is_empty() {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects at least 1 argument", name),
                        );
                    } else {
                        let handle = self.infer_expr(&args[0], scopes);
                        if handle != Type::Name("AtomicI64".into(), vec![]) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!("{} expects AtomicI64, found {}", name, fmt_type(&handle)),
                            );
                        }
                        for a in &args[1..] {
                            let t = self.infer_expr(a, scopes);
                            if !is_int(&t) {
                                self.emit_code(
                                    crate::diagnostic::codes::E0242,
                                    format!(
                                        "{} value slot expects integer, found {}",
                                        name,
                                        fmt_type(&t)
                                    ),
                                );
                            }
                        }
                    }
                    // fetch_add returns i64, not i32 (runtime ABI).
                    return Type::Name("i64".into(), vec![]);
                }
                "atomic_bool_compare_exchange" => {
                    if args.len() != 3 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 3 arguments (handle, expected, desired)", name),
                        );
                    } else {
                        let handle = self.infer_expr(&args[0], scopes);
                        if handle != Type::Name("AtomicBool".into(), vec![]) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!("{} expects AtomicBool, found {}", name, fmt_type(&handle)),
                            );
                        }
                        for a in &args[1..] {
                            let t = self.infer_expr(a, scopes);
                            if t != Type::Name("bool".into(), vec![])
                                && t != Type::Name("i32".into(), vec![])
                            {
                                self.emit_code(
                                    crate::diagnostic::codes::E0242,
                                    format!(
                                        "{} value slot expects bool or i32, found {}",
                                        name,
                                        fmt_type(&t)
                                    ),
                                );
                            }
                        }
                    }
                    // compare_exchange returns 1/0 (same as other CAS builtins).
                    return Type::Name("i32".into(), vec![]);
                }
                "atomic_bool_load" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 1 argument", name),
                        );
                    } else {
                        let handle = self.infer_expr(&args[0], scopes);
                        if handle != Type::Name("AtomicBool".into(), vec![]) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!("{} expects AtomicBool, found {}", name, fmt_type(&handle)),
                            );
                        }
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                "mutex_get" | "mutex_set" | "mutex_unlock" => {
                    if args.is_empty() {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects at least 1 argument", name),
                        );
                    } else {
                        let guard = self.infer_expr(&args[0], scopes);
                        let guard_ty =
                            Type::Name("MutexGuard".into(), vec![Type::Name("i64".into(), vec![])]);
                        if guard != guard_ty {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!(
                                    "{} expects MutexGuard<i64>, found {}",
                                    name,
                                    fmt_type(&guard)
                                ),
                            );
                        }
                        for a in &args[1..] {
                            let t = self.infer_expr(a, scopes);
                            if name == "mutex_set" && !is_int(&t) {
                                self.emit_code(
                                    crate::diagnostic::codes::E0242,
                                    format!(
                                        "mutex_set value slot expects integer, found {}",
                                        fmt_type(&t)
                                    ),
                                );
                            }
                        }
                    }
                    return if name == "mutex_get" {
                        Type::Name("i64".into(), vec![])
                    } else {
                        Type::Name("unit".into(), vec![])
                    };
                }
                "flow_pack" | "flow_epoch" | "flow_unpack" | "flow_bump_epoch" => {
                    if let Some(arg) = args.first() {
                        let t = self.infer_expr(arg, scopes);
                        if !is_int(&t) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!("{name} expects i64, found {}", fmt_type(&t)),
                            );
                        }
                    }
                    return Type::Name("i64".into(), vec![]);
                }
                "flow_drop" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "flow_drop expects 1 argument".to_string(),
                        );
                    } else {
                        let t = self.infer_expr(&args[0], scopes);
                        if !is_int(&t) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!("flow_drop expects i64, found {}", fmt_type(&t)),
                            );
                        }
                    }
                    return Type::Name("unit".into(), vec![]);
                }
                "flow_check_epoch" => {
                    for a in args {
                        let t = self.infer_expr(a, scopes);
                        if !is_int(&t) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!("flow_check_epoch expects i64, found {}", fmt_type(&t)),
                            );
                        }
                    }
                    return Type::Name("i64".into(), vec![]);
                }
                "flow_pack_count" | "flow_epoch_last_error" => {
                    return Type::Name("i64".into(), vec![]);
                }
                "channel_recv" | "channel_try_recv" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 1 argument", name),
                        );
                    } else {
                        let handle = self.infer_expr(&args[0], scopes);
                        let channel_ty =
                            Type::Name("Channel".into(), vec![Type::Name("i64".into(), vec![])]);
                        if handle != channel_ty {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!(
                                    "{} expects Channel<i64>, found {}",
                                    name,
                                    fmt_type(&handle)
                                ),
                            );
                        }
                    }
                    return Type::Name("i64".into(), vec![]);
                }
                // Phase D (0.39.73): token_channel_recv(ch: TokenChannel) -> SystemToken —
                // 返回一个全新 SystemToken 义务（须消费，CFG 追踪线性结果）。
                "token_channel_recv" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "token_channel_recv expects 1 argument (a TokenChannel)",
                        );
                    } else {
                        let handle = self.infer_expr(&args[0], scopes);
                        let ch_ty = Type::Name("TokenChannel".into(), vec![]);
                        if handle != ch_ty {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!(
                                    "token_channel_recv expects TokenChannel, found {}",
                                    fmt_type(&handle)
                                ),
                            );
                        }
                    }
                    return Type::Name("SystemToken".into(), vec![]);
                }
                // Phase D (0.39.73): token_channel_send(ch: TokenChannel, t: SystemToken) —
                // 通道被使用（不消费），t 被整体转移（move）进通道。
                "token_channel_send" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "token_channel_send expects 2 arguments (a TokenChannel, a SystemToken)",
                        );
                    } else {
                        let ch = self.infer_expr(&args[0], scopes);
                        let ch_ty = Type::Name("TokenChannel".into(), vec![]);
                        if ch != ch_ty {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!(
                                    "token_channel_send expects TokenChannel, found {}",
                                    fmt_type(&ch)
                                ),
                            );
                        }
                        let payload = self.infer_expr(&args[1], scopes);
                        let tok_ty = Type::Name("SystemToken".into(), vec![]);
                        if payload != tok_ty {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!(
                                    "token_channel_send value slot expects SystemToken, found {}",
                                    fmt_type(&payload)
                                ),
                            );
                        }
                    }
                    return Type::Name("unit".into(), vec![]);
                }
                "atomic_i32_store"
                | "atomic_i64_store"
                | "atomic_bool_store"
                | "atomic_i32_drop"
                | "atomic_i64_drop"
                | "atomic_bool_drop"
                | "mutex_drop"
                | "channel_send"
                | "channel_drop"
                | "actor_set_mailbox_depth"
                | "actor_set_max_children" => {
                    let mut arg_tys = Vec::with_capacity(args.len());
                    for a in args {
                        arg_tys.push(self.infer_expr(a, scopes));
                    }
                    let handle_ty = arg_tys.first().cloned().unwrap_or(Type::TyErr);
                    // Type-check the handle family, then the value slot.
                    if !args.is_empty() {
                        let expected = match name {
                            "atomic_i32_store" | "atomic_i32_drop" => {
                                Some(Type::Name("AtomicI32".into(), vec![]))
                            }
                            "atomic_i64_store" | "atomic_i64_drop" => {
                                Some(Type::Name("AtomicI64".into(), vec![]))
                            }
                            "atomic_bool_store" | "atomic_bool_drop" => {
                                Some(Type::Name("AtomicBool".into(), vec![]))
                            }
                            "mutex_drop" => Some(Type::Name(
                                "Mutex".into(),
                                vec![Type::Name("i64".into(), vec![])],
                            )),
                            "channel_send" | "channel_drop" => Some(Type::Name(
                                "Channel".into(),
                                vec![Type::Name("i64".into(), vec![])],
                            )),
                            _ => None,
                        };
                        if let Some(expected) = expected {
                            let actual = handle_ty;
                            if actual != expected {
                                self.emit_code(
                                    crate::diagnostic::codes::E0242,
                                    format!(
                                        "{} expects {}, found {}",
                                        name,
                                        fmt_type(&expected),
                                        fmt_type(&actual)
                                    ),
                                );
                            }
                        }
                    }
                    if args.len() > 1 {
                        let value_ty = &arg_tys[1];
                        if name == "channel_send" {
                            self.reject_narrow_across_channel_send(&args[1], scopes);
                            if self.is_flow_state_type(value_ty) {
                                self.emit_code(
                                    crate::diagnostic::codes::E0443,
                                    "bare Flow record cannot cross Channel; pack a TransitionEpoch with flow_pack".to_string(),
                                );
                            }
                        }
                        let (ok, what) = match name {
                            "atomic_bool_store" => {
                                (value_ty == &Type::Name("bool".into(), vec![]), "bool")
                            }
                            "atomic_i32_store" | "atomic_i64_store" | "channel_send"
                            | "mutex_set" => (is_int(value_ty), "integer"),
                            _ => (true, ""),
                        };
                        if !ok {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!(
                                    "{} value slot expects {}, found {}",
                                    name,
                                    what,
                                    fmt_type(value_ty)
                                ),
                            );
                        }
                    }
                    return Type::Name("unit".into(), vec![]);
                }
                // v0.29.19 — session endpoint ops with compile-time order checking.
                // 0.1.8 Phase E: the method surface is canonical; free
                // functions are deprecated for teaching and dogfood migration.
                "session_send" | "session_recv" | "session_close" => {
                    let method = match name {
                        "session_send" => "send",
                        "session_recv" => "recv",
                        "session_close" => "close",
                        _ => unreachable!(),
                    };
                    self.emit_warning_code(
                        crate::diagnostic::codes::W014,
                        format!(
                            "{} is deprecated; use the SessionChan method surface `endpoint.{}(...)`",
                            name, method
                        ),
                    );
                    return match name {
                        "session_send" => self.check_session_send(args, scopes),
                        "session_recv" => self.check_session_recv(args, scopes),
                        "session_close" => self.check_session_close(args, scopes),
                        _ => unreachable!(),
                    };
                }
                "session_open" => {
                    // session_open::<S>() returns SessionChan residual S.
                    for a in args {
                        self.infer_expr(a, scopes);
                    }
                    return Type::Name("SessionChan".into(), vec![]);
                }
                "session_pair" => {
                    // 0.36.38: the plain form returns the two opaque handles
                    // as a TUPLE (i64, i64) — the typed form (turbofish,
                    // method.rs) returns (SessionChan<S>, SessionChan<dual S>)
                    // with live residuals. Both share the {lo, hi} runtime
                    // shape (send on lo → recv on hi).
                    for a in args {
                        self.infer_expr(a, scopes);
                    }
                    return Type::Tuple(vec![
                        Type::Name("i64".into(), vec![]),
                        Type::Name("i64".into(), vec![]),
                    ]);
                }

                "print" => {
                    for a in args {
                        self.infer_expr(a, scopes);
                    }
                    return Type::Name("unit".into(), vec![]);
                }
                "ast_dump" | "ast_eval" => {
                    for a in args {
                        self.infer_expr(a, scopes);
                    }
                    // v0.34.10a (golden §7.6): return the registered "AST" type
                    // instead of "unknown" — unknown poisoned downstream
                    // unification; AST is a legal nominal type name that unifies
                    // with quote! results.
                    return Type::Name("AST".into(), vec![]);
                }
                "allocator_system" | "allocator_arena" | "allocator_bump" => {
                    return Type::Name("unknown".into(), vec![]);
                }
                "alloc" => {
                    for a in args {
                        self.infer_expr(a, scopes);
                    }
                    return Type::Name("unknown".into(), vec![]);
                }
                "arena_reset" | "bump_used" => {
                    for a in args {
                        self.infer_expr(a, scopes);
                    }
                    return Type::Name("unit".into(), vec![]);
                }
                "read_file" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "read_file expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Result(
                        Box::new(Type::Name("string".into(), vec![])),
                        Box::new(Type::Name("string".into(), vec![])),
                    );
                }
                // Phase D (0.39.75): 收 cap 的 fs API——read_file_guarded(path, t)。
                // t: SystemToken 作为能力门禁，被调用消费（每次授权一次受保护操作）。
                "read_file_guarded" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "read_file_guarded expects 2 arguments (path, a SystemToken capability)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        let cap_ty = self.infer_expr(&args[1], scopes);
                        if cap_ty != Type::Name("SystemToken".into(), vec![]) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!(
                                    "read_file_guarded capability slot expects SystemToken, found {}",
                                    fmt_type(&cap_ty)
                                ),
                            );
                        }
                    }
                    return Type::Result(
                        Box::new(Type::Name("string".into(), vec![])),
                        Box::new(Type::Name("string".into(), vec![])),
                    );
                }
                "write_file" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "write_file expects 2 arguments",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Result(
                        Box::new(Type::Name("unit".into(), vec![])),
                        Box::new(Type::Name("string".into(), vec![])),
                    );
                }
                "file_exists" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "file_exists expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                "listdir" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "listdir expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("List".into(), vec![Type::Name("string".into(), vec![])]);
                }
                "is_dir" | "is_file" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "is_dir/is_file expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                "path_join" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "path_join expects 2 arguments",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "path_ext" | "path_basename" | "path_dirname" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "path_ext/basename/dirname expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "walk_dir" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "walk_dir expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("List".into(), vec![Type::Name("string".into(), vec![])]);
                }
                "mkdir_p" | "remove_file" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "mkdir_p/remove_file expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                "exec" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "exec expects 1 argument (command)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("ExecResult".into(), vec![]);
                }
                "exec_safe" => {
                    if args.is_empty() {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "exec_safe expects at least 1 argument (program)",
                        );
                    } else {
                        for a in args {
                            self.infer_expr(a, scopes);
                        }
                    }
                    return Type::Name("ExecResult".into(), vec![]);
                }
                "exec_pipe" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "exec_pipe expects 1 argument (command)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "file_stat" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "file_stat expects 1 argument (path)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("StatResult".into(), vec![]);
                }
                "append_file" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "append_file expects 2 arguments (path, content)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                "set_env" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "set_env expects 2 arguments (key, value)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                "read_file_partial" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "read_file_partial expects 2 arguments (path, max_bytes)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "read_file_bytes" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "read_file_bytes expects 1 argument (path)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "write_file_bytes" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "write_file_bytes expects 2 arguments (path, data)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                "read_lines_each" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "read_lines_each expects 2 arguments (path, callback)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("i32".into(), vec![]);
                }
                "read_lines_json" | "read_lines_json_builtin" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "read_lines_json expects 1 argument (path)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "sha256" | "base64_encode" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "sha256/base64_encode expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "base64_decode" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "base64_decode expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Result(
                        Box::new(Type::Name("string".into(), vec![])),
                        Box::new(Type::Name("string".into(), vec![])),
                    );
                }
                "str_split" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "str_split expects 2 arguments (string, delimiter)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("List".into(), vec![Type::Name("string".into(), vec![])]);
                }
                "str_join" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "str_join expects 2 arguments (list, separator)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "str_trim" | "str_to_upper" | "str_to_lower" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 1 argument", name),
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "str_starts_with" | "str_ends_with" | "str_contains" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 2 arguments", name),
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                "regex_match" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "regex_match expects 2 arguments (text, pattern)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                "regex_find" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "regex_find expects 2 arguments (text, pattern)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "regex_replace" => {
                    if args.len() != 3 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "regex_replace expects 3 arguments (text, pattern, replacement)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                        self.infer_expr(&args[2], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "regex_find_all" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "regex_find_all expects 2 arguments (text, pattern)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "regex_capture_groups" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "regex_capture_groups expects 2 arguments (text, pattern)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "str_replace" => {
                    if args.len() != 3 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "str_replace expects 3 arguments",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                        self.infer_expr(&args[2], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "str_repeat" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "str_repeat expects 2 arguments",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "char_code" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "char_code expects 2 arguments (string, index)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("i64".into(), vec![]);
                }
                "chr" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "chr expects 1 argument (code point)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "str_char_at" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "str_char_at expects 2 arguments",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "str_substring" => {
                    if args.len() != 3 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "str_substring expects 3 arguments",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                        self.infer_expr(&args[2], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "str_count_substring" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "str_count_substring expects 2 arguments",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("i32".into(), vec![]);
                }
                "str_index_of" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "str_index_of expects 2 arguments",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Option(Box::new(Type::Name("i32".into(), vec![])));
                }
                "option_value_or" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "option_value_or expects 2 arguments",
                        );
                        return Type::Name("unknown".into(), vec![]);
                    }
                    // P1-33: Relate Option payload type with default type.
                    // Previously the Option was inferred and discarded, allowing
                    // option_value_or(Some(1), "not an int").
                    let opt_ty = self.infer_expr(&args[0], scopes);
                    let default_ty = self.infer_expr(&args[1], scopes);
                    let payload_ty = match opt_ty.unlocated() {
                        Type::Option(inner) => inner.as_ref().clone(),
                        Type::Name(n, type_args) if n == "Option" && type_args.len() == 1 => {
                            type_args[0].clone()
                        }
                        _ => default_ty.clone(),
                    };
                    if self.unification.unify(&payload_ty, &default_ty).is_err() {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!(
                            "option_value_or: Option payload type {} doesn't match default type {}",
                            fmt_type(&payload_ty),
                            fmt_type(&default_ty)
                        ),
                        );
                    }
                    return default_ty;
                }
                "str_parse_int" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "str_parse_int expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Tuple(vec![
                        Type::Name("bool".into(), vec![]),
                        Type::Name("i64".into(), vec![]),
                    ]);
                }
                "str_parse_float" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "str_parse_float expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Tuple(vec![
                        Type::Name("bool".into(), vec![]),
                        Type::Name("f64".into(), vec![]),
                    ]);
                }
                "eprintln" => {
                    for a in args {
                        self.infer_expr(a, scopes);
                    }
                    return Type::Name("unit".into(), vec![]);
                }
                "format" => {
                    if args.is_empty() {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "format expects at least 1 argument (template string)",
                        );
                    } else {
                        let tpl = self.infer_expr(&args[0], scopes);
                        if !crate::core::helpers::is_string(&tpl) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!(
                                    "format expects a string template as first argument, found {}",
                                    fmt_type(&tpl)
                                ),
                            );
                        }
                        for a in &args[1..] {
                            self.infer_expr(a, scopes);
                        }
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "str_to_c_str" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "str_to_c_str expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Tuple(vec![
                        Type::Name("i64".into(), vec![]),
                        Type::Name("i64".into(), vec![]),
                    ]);
                }
                "c_str_to_string" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "c_str_to_string expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "from_json" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "from_json expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("Record".into(), vec![]);
                }
                "json_is_valid" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "json_is_valid expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                "json_get_string" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "json_get_string expects 2 arguments",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "json_get_int" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "json_get_int expects 2 arguments",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("i32".into(), vec![]);
                }
                "json_array_length" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "json_array_length expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("i32".into(), vec![]);
                }
                "json_get_element" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "json_get_element expects 2 arguments",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "json_has_key" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "json_has_key expects 2 arguments",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                "socket" => {
                    if args.len() != 3 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "socket expects 3 arguments (domain, type, protocol)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                        self.infer_expr(&args[2], scopes);
                    }
                    return Type::Name("i64".into(), vec![]);
                }
                "connect" => {
                    if args.len() != 3 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "connect expects 3 arguments (fd, host, port)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                        self.infer_expr(&args[2], scopes);
                    }
                    return Type::Name("i64".into(), vec![]);
                }
                "bind" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "bind expects 2 arguments (fd, port)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("i64".into(), vec![]);
                }
                "listen" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "listen expects 2 arguments (fd, backlog)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("i64".into(), vec![]);
                }
                "accept" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "accept expects 1 argument (fd)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("i64".into(), vec![]);
                }
                "send" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "send expects 2 arguments (fd, data)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("i64".into(), vec![]);
                }
                "recv" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "recv expects 2 arguments (fd, buf_size)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "close_fd" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "close_fd expects 1 argument (fd)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("i64".into(), vec![]);
                }
                "http_get" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "http_get expects 1 argument (url)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                // Phase D (0.39.76): 收 cap 的 net API——http_get_guarded(url, t)。
                // t: SystemToken 能力门禁，被调用消费。
                "http_get_guarded" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "http_get_guarded expects 2 arguments (url, a SystemToken capability)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        let cap_ty = self.infer_expr(&args[1], scopes);
                        if cap_ty != Type::Name("SystemToken".into(), vec![]) {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                format!(
                                    "http_get_guarded capability slot expects SystemToken, found {}",
                                    fmt_type(&cap_ty)
                                ),
                            );
                        }
                    }
                    return Type::Name("string".into(), vec![]);
                }
                "http_post" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "http_post expects 2 arguments (url, body)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("string".into(), vec![]);
                }
                // Higher-order list operations (shared across backends)
                "map_list" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "map_list expects 2 arguments (list, fn)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        let fn_ty = self.infer_expr(&args[1], scopes);
                        if let Type::Func(_, ret_ty) = fn_ty.into_unlocated() {
                            return Type::Name("List".into(), vec![*ret_ty]);
                        }
                    }
                    return Type::Name("List".into(), vec![Type::Name("i32".into(), vec![])]);
                }
                "filter_list" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "filter_list expects 2 arguments (list, pred)",
                        );
                    } else {
                        let list_ty = self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                        return list_ty;
                    }
                    return Type::Name("List".into(), vec![Type::Name("i32".into(), vec![])]);
                }
                "reduce_list" => {
                    if args.len() != 3 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "reduce_list expects 3 arguments (list, fn, init)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                        let init_ty = self.infer_expr(&args[2], scopes);
                        return init_ty;
                    }
                    return Type::Name("i32".into(), vec![]);
                }
                "sort_list" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "sort_list expects 1 argument (list)",
                        );
                    } else {
                        let list_ty = self.infer_expr(&args[0], scopes);
                        return list_ty;
                    }
                    return Type::Name("List".into(), vec![Type::Name("i32".into(), vec![])]);
                }
                "find" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "find expects 2 arguments (list, target)",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Tuple(vec![
                        Type::Name("bool".into(), vec![]),
                        Type::Name("i32".into(), vec![]),
                    ]);
                }
                "any" | "all" => {
                    if args.len() != 2 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("{} expects 2 arguments (list, pred)", name),
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                        self.infer_expr(&args[1], scopes);
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                "is_empty" => {
                    if args.len() != 1 {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "is_empty expects 1 argument",
                        );
                    } else {
                        self.infer_expr(&args[0], scopes);
                    }
                    return Type::Name("bool".into(), vec![]);
                }
                _ => {}
            }
        } // 'builtin_dispatch

        // Local variables (including function parameters) shadow global
        // functions. Check scopes first before falling back to global function
        // signatures; otherwise a prelude parameter named `f` would incorrectly
        // resolve to a user-defined top-level function `f`.
        // CO-C1: instantiate ForAll so polymorphic let-bound closures get fresh TypeVars.
        if let Some(local_ty) = scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).cloned())
        {
            let resolved = self.unification.zonk_or_unknown(&local_ty);
            let local_ty = self.instantiate(&resolved);
            match local_ty.into_unlocated() {
                Type::Func(param_types, ret_ty) => {
                    if args.len() != param_types.len() {
                        self.emit_code(
                            crate::diagnostic::codes::E0257,
                            format!(
                                "closure '{}' expects {} arguments, got {}",
                                name,
                                param_types.len(),
                                args.len()
                            ),
                        );
                    } else {
                        for (i, (arg, param_ty)) in args.iter().zip(param_types.iter()).enumerate()
                        {
                            let arg_ty = self.infer_expr(arg, scopes);
                            // Audit 2026-08-05 (wave-1 fix 5): the E0432
                            // linear-into-generic rejection existed on direct
                            // global calls (below) and turbofish
                            // (method.rs:913) but not on this local-closure
                            // arm: `let f = generic_sink; f(cap_value)`
                            // unified T := cap through the fresh instantiation
                            // TypeVar and the callee's GenericParameter
                            // (is_linear() == false) silently discarded the
                            // value — an exactly-once escape. Mirror the
                            // global-call scan: reject linear argument types
                            // while the parameter still carries an unresolved
                            // generic binder (TypeVar originating from ForAll
                            // instantiation). Must run BEFORE unify binds the
                            // binder. Resolution failure is treated as an open
                            // binder (fail-closed).
                            if self.is_linear_surface_type(&arg_ty) {
                                let has_unresolved_binder =
                                    match self.unification.resolve_infer(param_ty) {
                                        Ok(resolved) => crate::core::type_folder::type_any(
                                            &resolved,
                                            &|candidate| matches!(candidate, Type::TypeVar(_)),
                                        ),
                                        Err(_) => true,
                                    };
                                if has_unresolved_binder {
                                    self.emit_code(
                                        crate::diagnostic::codes::E0432,
                                        format!(
                                            "linear type '{}' cannot be passed as generic argument {} of '{}'; \
                                             generic parameters are not linearly tracked (use a concrete function signature)",
                                            fmt_type(&arg_ty),
                                            i + 1,
                                            name
                                        ),
                                    );
                                }
                            }
                            let coerced = is_numeric_coercion(param_ty, &arg_ty);
                            if !coerced && self.unification.unify(param_ty, &arg_ty).is_err() {
                                self.emit_code(
                                    crate::diagnostic::codes::E0211,
                                    format!(
                                        "argument {} of closure '{}' expected {}, found {}",
                                        i + 1,
                                        name,
                                        fmt_type(param_ty),
                                        fmt_type(&arg_ty)
                                    ),
                                );
                            }
                        }
                    }
                    // Resolve return type after argument unification so TypeVars fill in.
                    return self.unification.zonk_or_unknown(&ret_ty);
                }
                _ => {
                    self.emit_code(
                        crate::diagnostic::codes::E0223,
                        format!("'{}' is not a function and cannot be called", name),
                    );
                    return Type::Name("unknown".into(), vec![]);
                }
            }
        }

        let (params, mut ret) = match self.funcs.get(name) {
            Some(sig) => sig.clone(),
            None => {
                // Try closure/lambda variable lookup: check if the name is a local
                // variable with a function type (let f = fn(x) { ... }; f(42))
                // CO-C1: instantiate ForAll before matching Func.
                let closure_sig: Option<(Vec<Type>, Type)> = {
                    let raw = scopes
                        .iter()
                        .rev()
                        .find_map(|scope| scope.get(name).cloned());
                    raw.map(|ty| {
                        let resolved = self.unification.zonk_or_unknown(&ty);
                        self.instantiate(&resolved)
                    })
                    .and_then(|ty| match ty.into_unlocated() {
                        Type::Func(params, ret) => Some((params, *ret)),
                        _ => None,
                    })
                };
                if let Some((param_types, ret_ty)) = closure_sig {
                    if args.len() != param_types.len() {
                        self.emit_code(
                            crate::diagnostic::codes::E0257,
                            format!(
                                "closure '{}' expects {} arguments, got {}",
                                name,
                                param_types.len(),
                                args.len()
                            ),
                        );
                    } else {
                        for (i, (arg, param_ty)) in args.iter().zip(param_types.iter()).enumerate()
                        {
                            let arg_ty = self.infer_expr(arg, scopes);
                            // IF-C1: strict unify rejects Any/_/Infer escape at call sites.
                            let coerced = is_numeric_coercion(param_ty, &arg_ty);
                            if !coerced && self.unification.unify(param_ty, &arg_ty).is_err() {
                                self.emit_code(
                                    crate::diagnostic::codes::E0211,
                                    format!(
                                        "argument {} of closure '{}' expected {}, found {}",
                                        i + 1,
                                        name,
                                        fmt_type(param_ty),
                                        fmt_type(&arg_ty)
                                    ),
                                );
                            }
                        }
                    }
                    return self.unification.zonk_or_unknown(&ret_ty);
                }
                // Try built-in Option/Result constructors as fallback.
                // IF-C2: never use Type::Name("_") / Infer as payload — those are escape
                // hatches that unify with anything. Fresh TypeVars freeze after first use.
                match name {
                    "Some" => {
                        if args.len() != 1 {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                "Some expects 1 argument",
                            );
                            return Type::Option(Box::new(self.fresh_var()));
                        } else {
                            let inner = self.infer_expr(&args[0], scopes);
                            return Type::Option(Box::new(inner));
                        }
                    }
                    "None" => {
                        if !args.is_empty() {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                "None expects 0 arguments",
                            );
                        }
                        return Type::Option(Box::new(self.fresh_var()));
                    }
                    "Ok" => {
                        if args.len() != 1 {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                "Ok expects 1 argument",
                            );
                            return Type::Result(
                                Box::new(self.fresh_var()),
                                Box::new(self.fresh_var()),
                            );
                        } else {
                            let inner = self.infer_expr(&args[0], scopes);
                            return Type::Result(Box::new(inner), Box::new(self.fresh_var()));
                        }
                    }
                    "Err" => {
                        if args.len() != 1 {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                "Err expects 1 argument",
                            );
                            return Type::Result(
                                Box::new(self.fresh_var()),
                                Box::new(self.fresh_var()),
                            );
                        } else {
                            let inner = self.infer_expr(&args[0], scopes);
                            return Type::Result(Box::new(self.fresh_var()), Box::new(inner));
                        }
                    }
                    _ => {}
                }
                // Try module-qualified lookup via use imports
                for module in self.use_imports.clone() {
                    let qualified = format!("{}::{}", module, name);
                    if self.funcs.contains_key(&qualified) {
                        // Recursively check with qualified name
                        return self.check_call(&qualified, args, scopes);
                    }
                }
                // Collect all known function names for "did you mean?" suggestions
                let candidates: Vec<String> = self.funcs.keys().cloned().collect();
                let suggestion = suggest_name(name, &candidates, 3);
                // 0.39.136 DX: when the name exists in an unimported stdlib
                // module, say so — "undefined function 'print_line'" without
                // "add `use std::io`" was the top merge-era confusion.
                if self.use_imports.is_empty()
                    || !self.use_imports.iter().any(|m| {
                        let qualified = format!("{m}::{name}");
                        self.funcs.contains_key(&qualified)
                    })
                {
                    if let Some(module) = crate::loader::stdlib_module_exporting(name) {
                        let already = self.use_imports.contains(&module);
                        let help = if already {
                            format!(
                                "'{name}' is exported by std::{module}, which is already                                  imported — check the spelling or argument count"
                            )
                        } else {
                            format!(
                                "'{name}' is available from std::{module} — add `use std::{module}`"
                            )
                        };
                        self.errors.push(
                            Diagnostic::error_code(
                                crate::diagnostic::codes::E0401,
                                format!("undefined function '{}'", name),
                                self.diagnostic_span(),
                            )
                            .with_help(help),
                        );
                        return Type::TyErr;
                    }
                }
                if let Some(suggested) = suggestion {
                    self.errors.push(
                        Diagnostic::error_code(
                            crate::diagnostic::codes::E0401,
                            format!("undefined function '{}'", name),
                            self.diagnostic_span(),
                        )
                        .with_help(format!("did you mean '{}'?", suggested)),
                    );
                } else {
                    self.emit_code(
                        crate::diagnostic::codes::E0401,
                        format!("undefined function '{}'", name),
                    );
                }
                return Type::Name("unknown".into(), vec![]);
            }
        };

        // Handle named arguments and default values in user function calls
        let has_named_args = args
            .iter()
            .any(|a| matches!(a.unlocated(), Expr::NamedArg(_, _)));
        if has_named_args || args.len() != params.len() {
            // Check if the function definition has param names (for named args) or defaults
            let func_def_params: Option<Vec<Param>> = self
                .file
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::Func(f) if f.name == name => Some(f.params.clone()),
                    _ => None,
                })
                .next()
                .or_else(|| self.nested_func_params.get(name).cloned());
            if let Some(func_params) = func_def_params {
                if func_params.len() == params.len() {
                    let mut reordered: Vec<&Expr> = vec![&Expr::Literal(Lit::Unit); params.len()];
                    let mut seen = vec![false; params.len()];
                    let mut pos_idx = 0;
                    // H-2 (audit 2026-08-05): mixed named/positional placement
                    // errors. Previously a positional arg past the last free
                    // slot was silently DROPPED (bypassing the arity check) and
                    // a named arg could overwrite an occupied slot — E0257 never
                    // fired and the lowered call disagreed with the checker.
                    // Every placement collision/overflow/missing slot is now a
                    // hard E0257 and the call is not re-checked with a
                    // corrupted argument vector.
                    let mut placement_error = false;
                    for arg in args {
                        match arg.unlocated() {
                            Expr::NamedArg(n, val) => {
                                if let Some(pos) = func_params.iter().position(|p| p.name == *n) {
                                    if seen[pos] {
                                        placement_error = true;
                                        self.emit_code(
                                            crate::diagnostic::codes::E0257,
                                            format!(
                                                "duplicate argument for parameter '{}' of function '{}' (slot already filled in mixed named/positional call)",
                                                n, name
                                            ),
                                        );
                                    } else {
                                        reordered[pos] = val;
                                        seen[pos] = true;
                                    }
                                } else {
                                    placement_error = true;
                                    self.emit_code(
                                        crate::diagnostic::codes::E0401,
                                        format!(
                                            "function '{}' has no parameter named '{}'",
                                            name, n
                                        ),
                                    );
                                }
                            }
                            _ => {
                                while pos_idx < seen.len() && seen[pos_idx] {
                                    pos_idx += 1;
                                }
                                if pos_idx < seen.len() {
                                    reordered[pos_idx] = arg;
                                    seen[pos_idx] = true;
                                    pos_idx += 1;
                                } else {
                                    placement_error = true;
                                    self.emit_code(
                                        crate::diagnostic::codes::E0257,
                                        format!(
                                            "function '{}' expects {} arguments, got more (extra positional argument has no free slot)",
                                            name,
                                            params.len()
                                        ),
                                    );
                                }
                            }
                        }
                    }
                    // Fill in default values for parameters that have them
                    let mut has_missing_defaults = false;
                    for (i, (seen, p)) in seen.iter().zip(func_params.iter()).enumerate() {
                        if let Some(ref default_expr) = p.default_value {
                            if !seen {
                                reordered[i] = default_expr;
                                has_missing_defaults = true;
                            }
                        }
                    }
                    // Enforce arity on the UNION of named + positional args:
                    // every slot must be filled by an argument or a default.
                    if has_named_args {
                        for (slot_seen, p) in seen.iter().zip(func_params.iter()) {
                            if !slot_seen && p.default_value.is_none() {
                                placement_error = true;
                                self.emit_code(
                                    crate::diagnostic::codes::E0257,
                                    format!(
                                        "function '{}' missing argument for parameter '{}'",
                                        name, p.name
                                    ),
                                );
                            }
                        }
                    }
                    if placement_error {
                        return Type::Name("unknown".into(), vec![]);
                    }
                    // Only recurse if we actually reordered or filled defaults
                    if has_named_args || (has_missing_defaults && args.len() < params.len()) {
                        let reordered_args: Vec<Expr> =
                            reordered.iter().map(|e| (*e).clone()).collect();
                        return self.check_call(name, &reordered_args, scopes);
                    }
                }
            }
        }

        if args.len() != params.len() {
            self.emit_code(
                crate::diagnostic::codes::E0257,
                format!(
                    "function '{}' expects {} arguments, got {}",
                    name,
                    params.len(),
                    args.len()
                ),
            );
        } else {
            // Q5/Q6 (0.34.25c): fail-closed mutate-place grammar. A `mutate`
            // parameter borrows its place exclusively — the argument must be a
            // variable (Ident) or a single-level field access (Ident.field,
            // incl. self.field). Nested places, literals and computed values
            // are rejected (E0434); two mutate args naming the same place are
            // rejected as an aliasing exclusive-borrow violation (E0435).
            self.check_mutate_place_grammar(name, args);
            // Generic calls instantiate one fresh variable per declared binder,
            // then feed every argument through the canonical unifier.
            let generics = self.func_generics.get(name).cloned().unwrap_or_default();

            if !generics.is_empty() {
                let (instantiated_params, instantiated_ret, generic_vars) =
                    self.instantiate_generic_signature(&params, &ret, &generics);
                let arg_tys: Vec<Type> = args
                    .iter()
                    .map(|argument| self.infer_expr(argument, scopes))
                    .collect();

                // §2.3 (0.34.21): linear capabilities (Cap/SessionChan/Flow
                // state) cannot be passed as generic arguments — generic
                // parameters are not linearly tracked (GenericParameter
                // is_linear() = false), so a linear value flowing through a
                // generic call would escape exactly-once enforcement. The
                // rejection is DEEP: `is_linear_surface_type` recurses through
                // type arguments, so containers carrying linear elements
                // (`List<cap>`/`Option<cap>`/`Map<K, cap>`) are rejected too.
                // AGENTS.md §0 H2 ruling (audit-type 2026-08-03): the earlier
                // "container pass-through is legal" exemption was proven to be
                // an exactly-once escape and is abolished.
                //
                // 0.36.39 (泛型×线性单态化切片 1): 线性黑盒直通——若该参数位置
                // 的调体对 T 的线性性零依赖（每条路径转移，或对 cap 类允 drop），
                // 则放行（实参按 call-site 具体类型追踪、返回绑定按实例化类型
                // 追踪，0.36.38 已实证）；否则维持 E0432 fail-closed。SessionChan
                // 及其任意嵌套走 transfer-only（中途 drop = E0425 弃置）。
                for (index, argument_ty) in arg_tys.iter().enumerate() {
                    if self.is_linear_surface_type(argument_ty) {
                        // 0.1.9 Phase A: `linear T` 参数 = 显式线性种类，定义时已做
                        // transfer-only 体校验；此处 kind 兼容，直接放行（不再依赖
                        // 调用点 blackbox）。
                        if self.param_uses_linear_kind(name, index) {
                            // 0.39.58: `linear drop T` 实例化必须可 drop——
                            // SessionChan（及其任意嵌套）不可 drop → 拒。
                            if self.param_uses_linear_drop_kind(name, index)
                                && self.surface_type_contains_session(argument_ty)
                            {
                                self.emit_code(
                                    crate::diagnostic::codes::E0432,
                                    format!(
                                        "linear type '{}' cannot instantiate `linear drop T` (argument {} of function '{}'): \
                                         `linear drop T` requires a drop-tolerant type, but SessionChan cannot be \
                                         dropped (only transferred/closed). Use `linear T` for transfer-only",
                                        fmt_type(argument_ty),
                                        index + 1,
                                        name
                                    ),
                                );
                            }
                            continue;
                        }
                        // 0.39.59 (Phase C 0.39.59-61): Free `T` + 线性实参 →
                        // 一律 E0432（种类不匹配 + 迁移提示），退役调用点体分析。
                        // Free `T` 只可实例化为非线性型；接线性实参须声明
                        // `linear T`（transfer-only）或 `linear drop T`（可 drop）。
                        self.emit_code(
                            crate::diagnostic::codes::E0432,
                            format!(
                                "linear type '{}' cannot be passed as generic argument {} of function '{}': \
                                 Free generic parameter `T` may only instantiate to non-linear types \
                                 (kind mismatch). Declare the parameter kind `linear T` (transfer-only body) \
                                 or `linear drop T` (drop-tolerant body), or use a concrete function \
                                 signature taking the linear type directly",
                                fmt_type(argument_ty),
                                index + 1,
                                name
                            ),
                        );
                    }
                }

                for (i, (actual, expected)) in
                    arg_tys.iter().zip(instantiated_params.iter()).enumerate()
                {
                    let coerced = is_numeric_coercion(expected, actual);
                    let unify_result = if coerced {
                        Ok(())
                    } else {
                        self.unification.constrain(expected, actual)
                    };
                    // C-1 (audit 2026-08-05): a bare-container parameter
                    // (`List`/`Set`/`Map` without args) must not accept a
                    // linear element; the unifier fails closed and we surface
                    // E0432 instead of the generic E0211.
                    if let Err(crate::core::unification::UnifyError::LinearContainerEscape(msg)) =
                        &unify_result
                    {
                        self.emit_code(
                            crate::diagnostic::codes::E0432,
                            format!(
                                "argument {} of '{}' carries a linear value into a bare container: {}",
                                i + 1,
                                name,
                                msg
                            ),
                        );
                        continue;
                    }
                    if unify_result.is_err() {
                        let expected = self
                            .unification
                            .resolve_infer(expected)
                            .unwrap_or_else(|_| expected.clone());
                        self.errors.push(
                            Diagnostic::error_code(
                                crate::diagnostic::codes::E0211,
                                format!(
                                    "argument {} of '{}' expected {}, found {}",
                                    i + 1,
                                    name,
                                    fmt_type(&expected),
                                    fmt_type(actual)
                                ),
                                self.diagnostic_span(),
                            )
                            .with_help(format!(
                                "all occurrences of a generic parameter must resolve to one type; argument {} has type '{}'",
                                i + 1,
                                fmt_type(actual)
                            )),
                        );
                    }
                }

                let mut type_map: HashMap<String, Type> = HashMap::new();
                for generic in &generics {
                    let variable = generic_vars
                        .get(&generic.name)
                        .expect("generic instantiation creates every declared binder");
                    match self.unification.zonk(variable) {
                        Ok(concrete) => {
                            type_map.insert(generic.name.clone(), concrete);
                        }
                        Err(crate::core::unification::ResolveError::UnboundVar(_)) => {
                            self.emit_code(
                                crate::diagnostic::codes::E0200,
                                format!(
                                    "cannot infer generic parameter '{}' of function '{}'",
                                    generic.name, name
                                ),
                            );
                        }
                        Err(error) => {
                            self.emit_code(
                                crate::diagnostic::codes::E0200,
                                format!(
                                    "failed to finalize generic parameter '{}' of function '{}': {}",
                                    generic.name, name, error
                                ),
                            );
                        }
                    }
                }

                // Check where constraints against the canonical substitutions.
                if let Some(clauses) = self.where_clauses.get(name).cloned() {
                    for (type_param, bounds) in clauses {
                        if let Some(concrete_type) = type_map.get(&type_param) {
                            for bound in &bounds {
                                if !self.type_implements_trait(concrete_type, bound) {
                                    self.emit_code(
                                        crate::diagnostic::codes::E0253,
                                        format!(
                                            "where constraint violated: type '{}' does not implement trait '{}' (required by function '{}')",
                                            fmt_type(concrete_type),
                                            bound,
                                            name
                                        ),
                                    );
                                }
                            }
                        }
                    }
                }

                // Check generic param bounds (e.g., <T: Clone>)
                for gp in &generics {
                    if !gp.bounds.is_empty() {
                        if let Some(concrete_type) = type_map.get(&gp.name) {
                            for bound in &gp.bounds {
                                if !self.type_implements_trait(concrete_type, bound) {
                                    self.emit_code(
                                        crate::diagnostic::codes::E0253,
                                        format!(
                                            "type '{}' does not implement trait '{}' (required by generic parameter '{}' of function '{}')",
                                            fmt_type(concrete_type),
                                            bound,
                                            gp.name,
                                            name
                                        ),
                                    );
                                }
                            }
                        }
                    }
                }

                ret = self
                    .unification
                    .resolve_infer(&instantiated_ret)
                    .unwrap_or_else(|_| Type::Name("unknown".into(), vec![]));
            } else {
                for (i, (arg, param)) in args.iter().zip(params.iter()).enumerate() {
                    let at = self.infer_expr(arg, scopes);
                    // v0.31.13: passing a session endpoint as a function argument
                    // moves it — consume the residual so scope-exit doesn't fire.
                    if let Some(key) = Self::place_key(arg) {
                        self.session_residuals.remove(&key);
                    }
                    // IF-C1: strict unify at call sites rejects Any/_/Infer escapes.
                    let coerced = is_numeric_coercion(param, &at);
                    let unify_result = if coerced {
                        Ok(())
                    } else {
                        self.unification.unify(param, &at)
                    };
                    // C-1 (audit 2026-08-05): non-generic callees with bare
                    // `List`/`Set`/`Map` parameters must not accept linear
                    // elements (the callee judges bare containers non-linear,
                    // dropping the capability). The unifier fails closed;
                    // surface E0432 instead of the generic E0211.
                    if let Err(crate::core::unification::UnifyError::LinearContainerEscape(msg)) =
                        &unify_result
                    {
                        self.emit_code(
                            crate::diagnostic::codes::E0432,
                            format!(
                                "argument {} of '{}' carries a linear value into a bare container: {}",
                                i + 1,
                                name,
                                msg
                            ),
                        );
                        continue;
                    }
                    if unify_result.is_err() {
                        self.errors.push(
                            Diagnostic::error_code(
                                crate::diagnostic::codes::E0211,
                                format!(
                                    "argument {} of '{}' expected {}, found {}",
                                    i + 1,
                                    name,
                                    fmt_type(param),
                                    fmt_type(&at)
                                ),
                                self.diagnostic_span(),
                            )
                            .with_help(format!(
                                "argument {} has type '{}', but '{}' expects type '{}'",
                                i + 1,
                                fmt_type(&at),
                                name,
                                fmt_type(param)
                            )),
                        );
                    }
                }
                // Check where constraints for non-generic functions. CK-H6: all entries.
                if let Some(clauses) = self.where_clauses.get(name).cloned() {
                    for (type_param, bounds) in clauses {
                        for (arg, param) in args.iter().zip(params.iter()) {
                            let at = self.infer_expr(arg, scopes);
                            if self.type_uses_type_param(param, &type_param) {
                                for bound in &bounds {
                                    if !self.type_implements_trait(&at, bound) {
                                        self.emit_code(
                                            crate::diagnostic::codes::E0253,
                                            format!(
                                                "where constraint violated: type '{}' does not implement trait '{}' (required by function '{}')",
                                                fmt_type(&at),
                                                bound,
                                                name
                                            ),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // v0.34.18c (§4.2): the `with` effect clause is abolished; the former
            // E0254 call-site effect-availability check (func_effects + has_effect)
            // is removed. Side-effect obligations are contracts + capability tokens.
        }
        ret
    }

    // ── 0.34.25c Q5/Q6: mutate-place grammar (fail-closed) ─────────────

    /// Q5: the only place forms accepted for a `mutate` argument — a variable
    /// (`x`) or a single-level field access (`obj.field`, incl. `self.field`).
    /// Returns the canonical place key, or `None` for anything else (nested
    /// places `a.b.c`, literals, computed values, indexing).
    fn mutate_place_of(expr: &Expr) -> Option<String> {
        match expr.unlocated() {
            Expr::Ident(name) => Some(name.clone()),
            Expr::Field(obj, field) => match obj.unlocated() {
                Expr::Ident(base) => Some(format!("{}.{}", base, field)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Q5/Q6: validate every `mutate` argument of a user-function call.
    /// Non-place arguments are rejected (E0434); two mutate arguments naming
    /// the same place are rejected (E0435, exclusive-borrow aliasing).
    fn check_mutate_place_grammar(&mut self, name: &str, args: &[Expr]) {
        let func_params: Option<&Vec<Param>> = self.nested_func_params.get(name).or_else(|| {
            self.file
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::Func(f) if f.name == name => Some(&f.params),
                    _ => None,
                })
                .next()
        });
        let Some(func_params) = func_params else {
            return;
        };
        let mut seen_places: Vec<String> = Vec::new();
        for (i, p) in func_params.iter().enumerate() {
            if !matches!(p.borrow, Some(ParamBorrow::Mutate)) {
                continue;
            }
            let Some(arg) = args.get(i) else { continue };
            match Self::mutate_place_of(arg) {
                Some(place) => {
                    if seen_places.contains(&place) {
                        self.errors.push(Diagnostic::error_code(
                            crate::diagnostic::codes::E0435,
                            format!(
                                "mutate argument {} of '{}' aliases place '{}' already borrowed \
                                 mutably in the same call (exclusive borrow violation)",
                                i + 1,
                                name,
                                place
                            ),
                            arg.meta()
                                .map(|m| m.span)
                                .unwrap_or_else(|| self.diagnostic_span()),
                        ));
                    } else {
                        seen_places.push(place);
                    }
                }
                None => {
                    self.errors.push(Diagnostic::error_code(
                        crate::diagnostic::codes::E0434,
                        format!(
                            "argument {} of '{}' is a mutate place and must be a variable or \
                             single-level field access (e.g. `x` or `obj.field`), found invalid place",
                            i + 1,
                            name
                        ),
                        arg.meta().map(|m| m.span).unwrap_or_else(|| self.diagnostic_span()),
                    ));
                }
            }
        }
    }

    // ── v0.29.19 Session Types order checking ─────────────────────────

    /// T-H3: stable residual key for Ident / nested Field places (`a.b.c`).
    fn place_key(expr: &Expr) -> Option<String> {
        match expr.unlocated() {
            Expr::Ident(n) => Some(n.clone()),
            Expr::Field(obj, f) => {
                let base = Self::place_key(obj)?;
                Some(format!("{}.{}", base, f))
            }
            _ => None,
        }
    }

    fn residual_for_var(&self, name: &str) -> Option<crate::ast::SessionType> {
        self.session_residuals.get(name).cloned()
    }

    fn set_residual(&mut self, name: &str, residual: crate::ast::SessionType) {
        self.session_residuals.insert(name.to_string(), residual);
    }

    /// Resolve residual for a channel expression.
    /// T-H3: track Ident and Field places (`ch`, `pair.left`) as residual keys.
    fn residual_of_expr(
        &mut self,
        expr: &Expr,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Option<(Option<String>, crate::ast::SessionType)> {
        let ty = self.infer_expr(expr, scopes);
        self.residual_of_preinferred_expr(expr, &ty)
    }

    /// Residual resolution for a receiver that has ALREADY been inferred by
    /// the method-call checker. Avoids a second `infer_expr` on the same
    /// SessionChan endpoint, which would otherwise consume/mis-track linear
    /// resources (0.1.8 Phase E method surface).
    fn residual_of_preinferred_expr(
        &mut self,
        expr: &Expr,
        ty: &Type,
    ) -> Option<(Option<String>, crate::ast::SessionType)> {
        let key = Self::place_key(expr);
        if let Some(ref v) = key {
            if let Some(r) = self.residual_for_var(v) {
                return Some((Some(v.clone()), r));
            }
            // v0.31.12: use-after-alias — the endpoint was consumed by `let b = a`.
            if self.consumed_session_vars.contains(v) {
                self.emit_code(
                    crate::diagnostic::codes::E0426,
                    format!(
                        "session endpoint '{}' was consumed by aliasing and cannot be used again",
                        v
                    ),
                );
                return None;
            }
            // Initialize residual from SessionChan<S> annotation if present
            // (0.36.38: SessionChan<dual X> — the hi end of session_pair::<S>()
            // — resolves X and dualizes).
            if let Some(resolved) = crate::session::residual_from_chan_type(ty, &self.session_types)
            {
                self.set_residual(v, resolved.clone());
                return Some((Some(v.clone()), resolved));
            }
        }
        // Untracked endpoint: no order check (best-effort skeleton).
        None
    }

    pub(in crate::core) fn check_session_send(
        &mut self,
        args: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        if args.len() != 2 {
            self.emit_code(
                crate::diagnostic::codes::E0242,
                "session_send expects 2 arguments (endpoint, value)".to_string(),
            );
            return Type::Name("unit".into(), vec![]);
        }
        if let Some((var, residual)) = self.residual_of_expr(&args[0], scopes) {
            let before = residual.clone();
            // 0.36.38 echo: a re-check re-visit of the SAME call-site (Assign
            // RHS expected-type re-check) must not advance the session twice;
            // the first visit already validated and advanced.
            if self.session_recorded_for_call(&residual).is_some() {
                self.infer_expr(&args[1], scopes);
                return Type::Name("unit".into(), vec![]);
            }
            match crate::session::apply_action(&residual, crate::session::SessionAction::Send) {
                Ok((next, expected_ty)) => {
                    if let Some(et) = expected_ty {
                        let actual = self.infer_expr(&args[1], scopes);
                        let et_r = self.resolve_type(&et);
                        if self.unification.unify(&et_r, &actual).is_err() {
                            self.emit_code(
                                crate::diagnostic::codes::E0414,
                                format!(
                                    "session_send: expected value of type {}, found {}",
                                    crate::core::fmt_type(&et_r),
                                    crate::core::fmt_type(&actual)
                                ),
                            );
                        }
                    } else {
                        self.infer_expr(&args[1], scopes);
                    }
                    if let Some(v) = var {
                        self.record_session_action(&v, before, next.clone(), false);
                        self.set_residual(&v, next);
                    }
                }
                Err(e) => {
                    self.emit_code(
                        crate::diagnostic::codes::E0414,
                        format!("session protocol order violation on send: {:?}", e),
                    );
                    self.infer_expr(&args[1], scopes);
                }
            }
        } else {
            for a in args {
                self.infer_expr(a, scopes);
            }
        }
        Type::Name("unit".into(), vec![])
    }

    pub(in crate::core) fn check_session_recv(
        &mut self,
        args: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        if args.len() != 1 {
            self.emit_code(
                crate::diagnostic::codes::E0242,
                "session_recv expects 1 argument (endpoint)".to_string(),
            );
            return Type::Name("unknown".into(), vec![]);
        }
        if let Some((var, residual)) = self.residual_of_expr(&args[0], scopes) {
            // 0.36.38 echo: re-inference of the same recv call-site echoes the
            // recorded payload type computed from the recorded BEFORE state
            // (pure) — no second advancement.
            if let Some(existing) = self.session_recorded_for_call(&residual) {
                if let Ok((_, payload_ty)) = crate::session::apply_action(
                    &existing.before,
                    crate::session::SessionAction::Recv,
                ) {
                    return payload_ty.unwrap_or_else(|| Type::Name("i64".into(), vec![]));
                }
                return Type::Name("i64".into(), vec![]);
            }
            let before = residual.clone();
            match crate::session::apply_action(&residual, crate::session::SessionAction::Recv) {
                Ok((next, payload_ty)) => {
                    if let Some(v) = var {
                        self.record_session_action(&v, before, next.clone(), false);
                        self.set_residual(&v, next);
                    }
                    return payload_ty.unwrap_or_else(|| Type::Name("i64".into(), vec![]));
                }
                Err(e) => {
                    self.emit_code(
                        crate::diagnostic::codes::E0414,
                        format!("session protocol order violation on recv: {:?}", e),
                    );
                    return Type::Name("unknown".into(), vec![]);
                }
            }
        }
        // v0.29.34: when the argument is a plain i64 channel handle (not a
        // SessionChan<S> variable), session_recv returns i64 to match the
        // runtime mimi_channel_recv() which returns i64. Previously returned
        // i32 which caused checker/codegen divergence (H3-fix).
        let _arg_ty = self.infer_expr(&args[0], scopes);
        // Runtime mimi_channel_recv returns i64 regardless of residual typing.
        Type::Name("i64".into(), vec![])
    }

    pub(in crate::core) fn check_session_close(
        &mut self,
        args: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        if args.len() != 1 {
            self.emit_code(
                crate::diagnostic::codes::E0242,
                "session_close expects 1 argument (endpoint)".to_string(),
            );
            return Type::Name("unit".into(), vec![]);
        }
        if let Some((var, residual)) = self.residual_of_expr(&args[0], scopes) {
            // 0.36.38 echo: re-inference of the same close call-site — the
            // close was validated (End reached) on the first visit.
            if self.session_recorded_for_call(&residual).is_some() {
                return Type::Name("unit".into(), vec![]);
            }
            let before = residual.clone();
            match crate::session::apply_action(&residual, crate::session::SessionAction::Close) {
                Ok((next, _)) => {
                    if let Some(v) = var {
                        self.record_session_action(&v, before, next.clone(), true);
                        self.set_residual(&v, next);
                    }
                }
                Err(e) => {
                    self.emit_code(
                        crate::diagnostic::codes::E0414,
                        format!("session protocol order violation on close: {:?}", e),
                    );
                }
            }
        } else {
            self.infer_expr(&args[0], scopes);
        }
        Type::Name("unit".into(), vec![])
    }

    /// 0.1.8 Phase E: SessionChan method surface (`ch.send(v)` / `ch.recv()` /
    /// `ch.close()`). The receiver has already been inferred by
    /// `infer_method_call`, so unlike the free `session_*` functions this
    /// uses `residual_of_preinferred_expr` and never re-infers the endpoint.
    pub(in crate::core) fn check_session_method(
        &mut self,
        obj: &Expr,
        obj_ty: &Type,
        method_name: &str,
        args: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        let residual_opt = self.residual_of_preinferred_expr(obj, obj_ty);
        match method_name {
            "send" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "send expects 1 argument (value)".to_string(),
                    );
                    for a in args {
                        self.infer_expr(a, scopes);
                    }
                    return Type::Name("unit".into(), vec![]);
                }
                if let Some((var, residual)) = residual_opt {
                    let before = residual.clone();
                    if self.session_recorded_for_call(&residual).is_some() {
                        self.infer_expr(&args[0], scopes);
                        return Type::Name("unit".into(), vec![]);
                    }
                    match crate::session::apply_action(
                        &residual,
                        crate::session::SessionAction::Send,
                    ) {
                        Ok((next, expected_ty)) => {
                            if let Some(et) = expected_ty {
                                let actual = self.infer_expr(&args[0], scopes);
                                let et_r = self.resolve_type(&et);
                                if self.unification.unify(&et_r, &actual).is_err() {
                                    self.emit_code(
                                        crate::diagnostic::codes::E0414,
                                        format!(
                                            "send: expected value of type {}, found {}",
                                            crate::core::fmt_type(&et_r),
                                            crate::core::fmt_type(&actual)
                                        ),
                                    );
                                }
                            } else {
                                self.infer_expr(&args[0], scopes);
                            }
                            if let Some(v) = var {
                                self.record_session_action(&v, before, next.clone(), false);
                                self.set_residual(&v, next);
                            }
                        }
                        Err(e) => {
                            self.emit_code(
                                crate::diagnostic::codes::E0414,
                                format!("session protocol order violation on send: {:?}", e),
                            );
                            self.infer_expr(&args[0], scopes);
                        }
                    }
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                Type::Name("unit".into(), vec![])
            }
            "recv" => {
                if !args.is_empty() {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "recv takes no arguments".to_string(),
                    );
                }
                if let Some((var, residual)) = residual_opt {
                    if let Some(existing) = self.session_recorded_for_call(&residual) {
                        if let Ok((_, payload_ty)) = crate::session::apply_action(
                            &existing.before,
                            crate::session::SessionAction::Recv,
                        ) {
                            return payload_ty.unwrap_or_else(|| Type::Name("i64".into(), vec![]));
                        }
                        return Type::Name("i64".into(), vec![]);
                    }
                    let before = residual.clone();
                    match crate::session::apply_action(
                        &residual,
                        crate::session::SessionAction::Recv,
                    ) {
                        Ok((next, payload_ty)) => {
                            if let Some(v) = var {
                                self.record_session_action(&v, before, next.clone(), false);
                                self.set_residual(&v, next);
                            }
                            payload_ty.unwrap_or_else(|| Type::Name("i64".into(), vec![]))
                        }
                        Err(e) => {
                            self.emit_code(
                                crate::diagnostic::codes::E0414,
                                format!("session protocol order violation on recv: {:?}", e),
                            );
                            Type::Name("unknown".into(), vec![])
                        }
                    }
                } else {
                    Type::Name("i64".into(), vec![])
                }
            }
            "close" => {
                if !args.is_empty() {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "close takes no arguments".to_string(),
                    );
                }
                if let Some((var, residual)) = residual_opt {
                    if self.session_recorded_for_call(&residual).is_some() {
                        return Type::Name("unit".into(), vec![]);
                    }
                    let before = residual.clone();
                    match crate::session::apply_action(
                        &residual,
                        crate::session::SessionAction::Close,
                    ) {
                        Ok((next, _)) => {
                            if let Some(v) = var {
                                self.record_session_action(&v, before, next.clone(), true);
                                self.set_residual(&v, next);
                            }
                        }
                        Err(e) => {
                            self.emit_code(
                                crate::diagnostic::codes::E0414,
                                format!("session protocol order violation on close: {:?}", e),
                            );
                        }
                    }
                }
                Type::Name("unit".into(), vec![])
            }
            _ => Type::Name("unknown".into(), vec![]),
        }
    }

    /// 0.36.38: re-inference of the SAME call-site (e.g. the Assign arm's
    /// expected-type re-check of the RHS) must NOT advance the session twice.
    /// If this exact call was already recorded and the CURRENT residual
    /// exactly matches the recorded AFTER state, the visit is a pure echo (the
    /// first visit already advanced the residual) — returns `Some(recorded)`.
    /// If the residual moved AWAY from the recorded state a genuine second
    /// execution happened (fail-closed TOOL-RESOLUTION-001 via
    /// `record_session_action` collision). Returns `None` on first visit.
    fn session_recorded_for_call(
        &self,
        current: &crate::ast::SessionType,
    ) -> Option<crate::core::checker::flow::CheckedSessionAction> {
        let key = self.current_call_expression.as_ref()?;
        let owner = self.current_callable_owner.as_ref()?;
        let existing = self.session_actions.get(owner)?.get(key)?.clone();
        if &existing.after == current {
            Some(existing)
        } else {
            // Genuine second execution: the residual is not where the first
            // visit left it — let the caller's record path surface the
            // collision error (fail-closed) instead of echoing.
            None
        }
    }

    fn record_session_action(
        &mut self,
        endpoint: &str,
        before: crate::ast::SessionType,
        after: crate::ast::SessionType,
        terminal: bool,
    ) {
        let (Some(owner), Some(call)) = (
            self.current_callable_owner.clone(),
            self.current_call_expression.clone(),
        ) else {
            self.errors.push(Diagnostic::error(
                "TOOL-RESOLUTION-001: checked session action has no callable/call identity",
                self.diagnostic_span(),
            ));
            return;
        };
        let action = crate::core::checker::flow::CheckedSessionAction {
            endpoint: endpoint.to_string(),
            before,
            after,
            terminal,
        };
        if self
            .session_actions
            .entry(owner)
            .or_default()
            .insert(call, action)
            .is_some()
        {
            self.errors.push(Diagnostic::error(
                "TOOL-RESOLUTION-001: one call advances a session more than once",
                self.diagnostic_span(),
            ));
        }
    }
}

/// 0.31.24: Check if a builtin function is impure (I/O, FFI, or allocation).
/// Comptime functions cannot call these.
///
/// ⚠ SYNC REQUIRED: This list must be kept in sync with the builtin
/// registry in src/codegen/builtins/mod.rs (compile_builtin_call dispatch).
/// When adding a new I/O/net/fs/env/time/process builtin to codegen,
/// add it here too. See test `comptime_purity_covers_codegen_builtins`.
fn is_impure_builtin(name: &str) -> bool {
    matches!(
        name,
        // ── I/O operations ──
        "println"
            | "print"
            | "print_line"
            | "print_raw"
            | "print_format"
            | "print_err"
            | "eprintln"
            | "input"
            | "input_line"
            | "try_input_line"
            | "input_int"
            | "input_float"
            | "input_bool"
            // ── File system operations ──
            | "fs_exists"
            | "fs_read"
            | "fs_write"
            | "fs_read_lines"
            | "fs_write_lines"
            | "fs_file_size"
            | "fs_listdir"
            | "fs_walk_dir"
            // Legacy/alias fs names (codegen registers both)
            | "read_file"
            | "write_file"
            | "file_exists"
            | "listdir"
            | "is_dir"
            | "is_file"
            | "walk_dir"
            | "mkdir_p"
            | "remove_file"
            | "file_stat"
            | "append_file"
            | "read_file_partial"
            | "read_file_bytes"
            | "write_file_bytes"
            | "read_lines_json"
            | "read_lines_json_builtin"
            // ── Network operations ──
            | "tcp_socket"
            | "tcp_connect"
            | "tcp_listen"
            | "tcp_accept"
            | "tcp_send"
            | "tcp_recv"
            | "fetch"
            | "fetch_post"
            // Legacy/alias net names
            | "socket"
            | "connect"
            | "listen"
            | "accept"
            | "send"
            | "recv"
            | "http_get"
            | "http_post"
            // ── Allocation ──
            | "alloc"
            | "arena_alloc"
            // ── Time (side-effect: reads system clock) ──
            | "timestamp"
            | "timestamp_ms"
            | "sleep_ms"
            // Alias time names (codegen registers both)
            | "now"
            | "now_ms"
            | "sleep"
            // ── Random (side-effect: reads RNG state) ──
            | "random_int"
            | "random"
            | "random_normal"
            | "random_uniform"
            | "random_exponential"
            | "random_bernoulli"
            // ── Environment (side-effect: reads/modifies process state) ──
            | "get_var"
            | "cli_args"
            | "get_var_or"
            | "has_var"
            | "get_int"
            | "get_float"
            | "arg_count"
            | "first_arg"
            | "getenv"
            | "set_env"
            // ── Process control / execution ──
            | "exit"
            | "exec"
            | "exec_safe"
            | "exec_pipe"
            | "flow_pack"
            | "flow_bump_epoch"
            | "flow_pack_count"
    )
}

#[cfg(test)]
mod tests {
    use super::is_impure_builtin;

    /// P1-37: Cross-check that all known impure codegen builtins are
    /// covered by is_impure_builtin. When adding a new I/O/net/fs/env/
    /// time/process builtin to codegen/builtins/mod.rs, add it here too.
    #[test]
    fn comptime_purity_covers_codegen_builtins() {
        // All impure builtins registered in codegen/builtins/mod.rs
        // (compile_builtin_call dispatch table).
        let known_impure: &[&str] = &[
            // I/O
            "println",
            "print",
            "eprintln",
            "input",
            "print_line",
            "print_raw",
            "print_format",
            "print_err",
            "input_line",
            "try_input_line",
            "input_int",
            "input_float",
            "input_bool",
            // File system
            "file_exists",
            "read_file",
            "write_file",
            "listdir",
            "is_dir",
            "is_file",
            "walk_dir",
            "mkdir_p",
            "remove_file",
            "file_stat",
            "append_file",
            "read_file_partial",
            "read_file_bytes",
            "write_file_bytes",
            "read_lines_json",
            "fs_exists",
            "fs_read",
            "fs_write",
            "fs_read_lines",
            "fs_write_lines",
            "fs_file_size",
            "fs_listdir",
            "fs_walk_dir",
            // Network
            "socket",
            "connect",
            "listen",
            "accept",
            "send",
            "recv",
            "tcp_socket",
            "tcp_connect",
            "tcp_listen",
            "tcp_accept",
            "tcp_send",
            "tcp_recv",
            "fetch",
            "fetch_post",
            "http_get",
            "http_post",
            // Time
            "now",
            "now_ms",
            "sleep",
            "timestamp",
            "timestamp_ms",
            "sleep_ms",
            // Random
            "random",
            "random_int",
            "random_normal",
            "random_uniform",
            "random_exponential",
            "random_bernoulli",
            // Environment
            "getenv",
            "set_env",
            "get_var",
            "cli_args",
            "get_var_or",
            "has_var",
            "get_int",
            "get_float",
            "arg_count",
            "first_arg",
            // Process
            "exit",
            "exec",
            "exec_safe",
            "exec_pipe",
            // Allocation
            "alloc",
            "arena_alloc",
            "flow_pack",
            "flow_bump_epoch",
            "flow_pack_count",
        ];
        for name in known_impure {
            assert!(
                is_impure_builtin(name),
                "comptime purity gate misses impure builtin '{}'",
                name
            );
        }
    }
}
