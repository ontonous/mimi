use crate::ast::*;
use crate::core::checker::Checker;
use crate::core::helpers::{
    fmt_type, is_bool, is_int, is_numeric, is_numeric_coercion, same_type, subst_type_params,
    suggest_name,
};
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashMap;

impl<'a> Checker<'a> {
    pub(in crate::core) fn check_call(
        &mut self,
        name: &str,
        args: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        // Builtins
        match name {
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
                    self.emit_code(crate::diagnostic::codes::E0242, "range expects 2 arguments");
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
            "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "sinh" | "cosh" | "tanh" | "ln"
            | "log2" | "log10" | "exp" | "exp2" | "cbrt" => {
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
                    self.emit_code(crate::diagnostic::codes::E0242, "atan2 expects 2 arguments");
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
                    self.infer_expr(&args[0], scopes);
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
            "to_int" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "to_int expects 1 argument");
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
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
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
                let elem_ty = match &list_ty {
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
                } else {
                    let t1 = self.infer_expr(&args[0], scopes);
                    let t2 = self.infer_expr(&args[1], scopes);
                    if !same_type(&t1, &t2) {
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
                return Type::Name("unknown".into(), vec![]);
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
            "assert_eq" | "assert_ne" => {
                if args.len() != 2 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        format!("{} expects 2 arguments", name),
                    );
                } else {
                    let t1 = self.infer_expr(&args[0], scopes);
                    let t2 = self.infer_expr(&args[1], scopes);
                    if !same_type(&t1, &t2) {
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
                    if !same_type(&t1, &t2) {
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
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("List".into(), vec![Type::Name("unknown".into(), vec![])]);
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
            "to_json" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "to_json expects 1 argument",
                    );
                } else {
                    self.infer_expr(&args[0], scopes);
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
            "map" => {
                if args.len() != 2 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "map expects 2 arguments (list, closure)",
                    );
                } else {
                    let list_ty = self.infer_expr(&args[0], scopes);
                    let elem_ty = match &list_ty {
                        Type::Name(_, args) if args.len() == 1 => args[0].clone(),
                        _ => Type::Name("unknown".into(), vec![]),
                    };
                    let closure_ty = self.infer_expr(&args[1], scopes);
                    let ret_ty = match &closure_ty {
                        Type::Func(_, ret) => ret.as_ref().clone(),
                        _ => elem_ty.clone(),
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
                    let elem_ty = match &list_ty {
                        Type::Name(_, args) if args.len() == 1 => args[0].clone(),
                        _ => Type::Name("unknown".into(), vec![]),
                    };
                    self.infer_expr(&args[1], scopes);
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
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                    let init_ty = self.infer_expr(&args[2], scopes);
                    return init_ty;
                }
                return Type::Name("unknown".into(), vec![]);
            }
            "sort" | "reverse" | "flatten" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        format!("{} expects 1 argument", name),
                    );
                    return Type::Name("unknown".into(), vec![]);
                }
                let arg_ty = self.infer_expr(&args[0], scopes);
                // Extract element type from input list to propagate to result
                let elem_ty = match &arg_ty {
                    Type::Name(n, inner) if n == "List" && inner.len() == 1 => inner[0].clone(),
                    _ => Type::Name("unknown".into(), vec![]),
                };
                return Type::Name("List".into(), vec![elem_ty]);
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
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Name("List".into(), vec![Type::Name("unknown".into(), vec![])]);
            }
            "sum" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "sum expects 1 argument");
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("i32".into(), vec![]);
            }
            "pow" => {
                if args.len() != 2 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        format!("{} expects 2 arguments", name),
                    );
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Name("f64".into(), vec![]);
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
            "keys" | "values" => {
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
            "has_key" => {
                if args.len() != 2 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "has_key expects 2 arguments",
                    );
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
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
            // v0.28.20 — concurrency primitives; handle types are uniform i64.
            "atomic_i32_new" | "atomic_i32_drop" | "atomic_i64_new" | "atomic_i64_drop"
            | "atomic_bool_new" | "atomic_bool_drop" | "mutex_new" | "mutex_lock"
            | "channel_new" | "actor_mailbox_depth" | "actor_is_muted"
            | "actor_spawn_count" | "actor_max_children" => {
                for a in args {
                    self.infer_expr(a, scopes);
                }
                return Type::Name("i64".into(), vec![]);
            }
            "atomic_i32_load"
            | "atomic_i32_compare_exchange"
            | "atomic_i32_fetch_add"
            | "atomic_i64_fetch_add" => {
                if args.is_empty() {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        format!("{} expects at least 1 argument", name),
                    );
                } else {
                    self.infer_expr(&args[0], scopes);
                    for a in &args[1..] {
                        self.infer_expr(a, scopes);
                    }
                }
                return Type::Name("i32".into(), vec![]);
            }
            "atomic_i64_load" | "atomic_bool_load" | "mutex_get" | "channel_recv"
            | "channel_try_recv" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        format!("{} expects 1 argument", name),
                    );
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("i64".into(), vec![]);
            }
            "atomic_i32_store" | "atomic_i64_store" | "atomic_bool_store" | "mutex_set"
            | "mutex_unlock" | "mutex_drop" | "channel_send" | "channel_drop"
            | "actor_set_mailbox_depth" | "actor_set_max_children" => {
                for a in args {
                    self.infer_expr(a, scopes);
                }
                return Type::Name("unit".into(), vec![]);
            }
            // v0.29.19 — session endpoint ops with compile-time order checking.
            "session_send" => {
                return self.check_session_send(args, scopes);
            }
            "session_recv" => {
                return self.check_session_recv(args, scopes);
            }
            "session_close" => {
                return self.check_session_close(args, scopes);
            }
            "session_open" => {
                // session_open::<S>() / session_open() — returns SessionChan residual S.
                // Track residual when assigned to a variable (via let + pattern).
                for a in args {
                    self.infer_expr(a, scopes);
                }
                // Without turbofish we cannot know S; return opaque SessionChan.
                return Type::Name("SessionChan".into(), vec![]);
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
                return Type::Name("unknown".into(), vec![]);
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
                self.infer_expr(&args[0], scopes);
                return self.infer_expr(&args[1], scopes);
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
            _ => {}
        }

        // Local variables (including function parameters) shadow global
        // functions. Check scopes first before falling back to global function
        // signatures; otherwise a prelude parameter named `f` would incorrectly
        // resolve to a user-defined top-level function `f`.
        if let Some(local_ty) = scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).cloned())
        {
            match local_ty {
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
                    return *ret_ty;
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
                let closure_sig: Option<(Vec<Type>, Type)> = scopes
                    .iter()
                    .rev()
                    .find_map(|scope| scope.get(name).cloned())
                    .and_then(|ty| match ty {
                        Type::Func(params, ret) => Some((params, *ret)),
                        _ => None,
                    });
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
                            // C2: use unification for argument type checking
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
                    return ret_ty;
                }
                // Try built-in Option/Result constructors as fallback
                match name {
                    "Some" => {
                        if args.len() != 1 {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                "Some expects 1 argument",
                            );
                        } else {
                            let inner = self.infer_expr(&args[0], scopes);
                            return Type::Option(Box::new(inner));
                        }
                        return Type::Option(Box::new(Type::Name("_".into(), vec![])));
                    }
                    "None" => {
                        if !args.is_empty() {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                "None expects 0 arguments",
                            );
                        }
                        return Type::Option(Box::new(Type::Name("_".into(), vec![])));
                    }
                    "Ok" => {
                        if args.len() != 1 {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                "Ok expects 1 argument",
                            );
                        } else {
                            let inner = self.infer_expr(&args[0], scopes);
                            return Type::Result(
                                Box::new(inner),
                                Box::new(Type::Name("_".into(), vec![])),
                            );
                        }
                        return Type::Result(
                            Box::new(Type::Name("_".into(), vec![])),
                            Box::new(Type::Name("_".into(), vec![])),
                        );
                    }
                    "Err" => {
                        if args.len() != 1 {
                            self.emit_code(
                                crate::diagnostic::codes::E0242,
                                "Err expects 1 argument",
                            );
                        } else {
                            let inner = self.infer_expr(&args[0], scopes);
                            return Type::Result(
                                Box::new(Type::Name("_".into(), vec![])),
                                Box::new(inner),
                            );
                        }
                        return Type::Result(
                            Box::new(Type::Name("_".into(), vec![])),
                            Box::new(Type::Name("_".into(), vec![])),
                        );
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
                if let Some(suggested) = suggestion {
                    self.errors.push(
                        Diagnostic::error_code(
                            crate::diagnostic::codes::E0401,
                            format!("undefined function '{}'", name),
                            Span::single(self.current_line, self.current_col),
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
        let has_named_args = args.iter().any(|a| matches!(a, Expr::NamedArg(_, _)));
        if has_named_args || (!args.is_empty() && args.len() != params.len()) {
            // Check if the function definition has param names (for named args) or defaults
            let func_def_params: Option<&[Param]> = self
                .file
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::Func(f) if f.name == name => Some(f.params.as_slice()),
                    _ => None,
                })
                .next();
            if let Some(func_params) = func_def_params {
                if func_params.len() == params.len() {
                    let mut reordered: Vec<&Expr> = vec![&Expr::Literal(Lit::Unit); params.len()];
                    let mut seen = vec![false; params.len()];
                    let mut pos_idx = 0;
                    for arg in args {
                        match arg {
                            Expr::NamedArg(n, val) => {
                                if let Some(pos) = func_params.iter().position(|p| p.name == *n) {
                                    reordered[pos] = val;
                                    seen[pos] = true;
                                } else {
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
                                }
                            }
                        }
                    }
                    // Check for extra positional args that were dropped
                    if !has_named_args && pos_idx < args.len() {
                        // Some positional args were dropped — let normal arg count check handle it
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
            // Check if this is a generic function and build type param map
            let generics = self.func_generics.get(name).cloned().unwrap_or_default();
            let mut type_map: HashMap<String, Type> = HashMap::new();

            if !generics.is_empty() {
                // Infer type parameters from argument types (one pass)
                let mut arg_tys: Vec<Type> = Vec::with_capacity(args.len());
                for (arg, param) in args.iter().zip(params.iter()) {
                    let at = self.infer_expr(arg, scopes);
                    self.infer_type_params(param, &at, &generics, &mut type_map);
                    arg_tys.push(at);
                }

                // Check where constraints (before substitution)
                if let Some((type_param, bounds)) = self.where_clauses.get(name).cloned() {
                    for (at, param) in arg_tys.iter().zip(params.iter()) {
                        if self.type_uses_type_param(param, &type_param) {
                            for bound in &bounds {
                                if !self.type_implements_trait(at, bound) {
                                    self.emit_code(
                                        crate::diagnostic::codes::E0253,
                                        format!(
                                            "where constraint violated: type '{}' does not implement trait '{}' (required by function '{}')",
                                            fmt_type(at),
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

                // Check arguments with substituted types (reuse cached types)
                for (i, (at, param)) in arg_tys.iter().zip(params.iter()).enumerate() {
                    let subst_param = subst_type_params(param, &generics, &type_map);
                    // C2: use unification for generic argument type checking
                    let coerced = is_numeric_coercion(&subst_param, &at);
                    if !coerced && self.unification.unify(&subst_param, &at).is_err() {
                        self.errors.push(
                            Diagnostic::error_code(
                                crate::diagnostic::codes::E0211,
                                format!(
                                    "argument {} of '{}' expected {}, found {}",
                                    i + 1,
                                    name,
                                    fmt_type(&subst_param),
                                    fmt_type(&at)
                                ),
                                Span::single(self.current_line, self.current_col),
                            )
                            .with_help(format!(
                                "argument {} has type '{}', but '{}' expects type '{}'",
                                i + 1,
                                fmt_type(&at),
                                name,
                                fmt_type(&subst_param)
                            )),
                        );
                    }
                }

                ret = subst_type_params(&ret, &generics, &type_map);
            } else {
                for (i, (arg, param)) in args.iter().zip(params.iter()).enumerate() {
                    let at = self.infer_expr(arg, scopes);
                    // C2: use unification for non-generic argument type checking
                    let coerced = is_numeric_coercion(param, &at);
                    if !coerced && self.unification.unify(param, &at).is_err() {
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
                                Span::single(self.current_line, self.current_col),
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
                // Check where constraints for non-generic functions
                if let Some((type_param, bounds)) = self.where_clauses.get(name).cloned() {
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

            // Check effects
            if let Some(required_effects) = self.func_effects.get(name).cloned() {
                for effect in &required_effects {
                    if !self.has_effect(effect) {
                        self.emit_code(
                            crate::diagnostic::codes::E0254,
                            format!(
                                "effect '{}' required by function '{}' is not available in current scope",
                                effect, name
                            ),
                        );
                    }
                }
            }
        }
        ret
    }

    // ── v0.29.19 Session Types order checking ─────────────────────────

    fn session_chan_name(ty: &Type) -> Option<String> {
        crate::session::session_from_chan_type(ty)
    }

    fn residual_for_var(&self, name: &str) -> Option<crate::ast::SessionType> {
        self.session_residuals.get(name).cloned()
    }

    fn set_residual(&mut self, name: &str, residual: crate::ast::SessionType) {
        self.session_residuals.insert(name.to_string(), residual);
    }

    /// Resolve residual for a channel expression. Only Ident endpoints are tracked.
    fn residual_of_expr(
        &mut self,
        expr: &Expr,
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Option<(Option<String>, crate::ast::SessionType)> {
        let ty = self.infer_expr(expr, scopes);
        if let Expr::Ident(v) = expr {
            if let Some(r) = self.residual_for_var(v) {
                return Some((Some(v.clone()), r));
            }
            // Initialize residual from SessionChan<S> annotation if present.
            if let Some(sname) = Self::session_chan_name(&ty) {
                if let Some(body) = self.session_types.get(&sname).cloned() {
                    let resolved = crate::session::resolve(&body, &self.session_types)
                        .unwrap_or(body);
                    self.set_residual(v, resolved.clone());
                    return Some((Some(v.clone()), resolved));
                }
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
            match crate::session::apply_action(&residual, crate::session::SessionAction::Send) {
                Ok((next, expected_ty)) => {
                    if let Some(et) = expected_ty {
                        let actual = self.infer_expr(&args[1], scopes);
                        let et_r = self.resolve_type(&et);
                        if self.unification.unify(&et_r, &actual).is_err()
                            && !crate::core::helpers::same_type(&et_r, &actual)
                        {
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
            match crate::session::apply_action(&residual, crate::session::SessionAction::Recv) {
                Ok((next, payload_ty)) => {
                    if let Some(v) = var {
                        self.set_residual(&v, next);
                    }
                    return payload_ty.unwrap_or_else(|| Type::Name("unknown".into(), vec![]));
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
        self.infer_expr(&args[0], scopes);
        Type::Name("unknown".into(), vec![])
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
            match crate::session::apply_action(&residual, crate::session::SessionAction::Close) {
                Ok((next, _)) => {
                    if let Some(v) = var {
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

}
