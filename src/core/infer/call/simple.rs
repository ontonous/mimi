use crate::ast::*;
use crate::core::checker::Checker;
use crate::core::helpers::{
    fmt_type, is_bool, is_int, is_numeric, is_numeric_coercion, same_type, suggest_name,
    subst_type_params,
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
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "assert expects 1 argument");
                } else {
                    let t = self.infer_expr(&args[0], scopes);
                    if !is_bool(&t) {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            format!("assert expects bool, found {}", fmt_type(&t)),
                        );
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
                } else {
                    let t = self.infer_expr(&args[0], scopes);
                    if !is_numeric(&t) {
                        self.emit_code(
                            crate::diagnostic::codes::E0242,
                            "abs expects a numeric argument",
                        );
                    }
                }
                return Type::Name("unknown".into(), vec![]);
            }
            "push" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "push expects 2 arguments");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Name("unit".into(), vec![]);
            }
            "pop" => {
                if args.len() != 1 {
                    self.emit_code(crate::diagnostic::codes::E0242, "pop expects 1 argument");
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("unknown".into(), vec![]);
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
                return Type::Name("List".into(), vec![Type::Name("unknown".into(), vec![])]);
            }
            "parse" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        "parse expects 1 argument (source string)",
                    );
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("unknown".into(), vec![]);
            }
            "args" => {
                if !args.is_empty() {
                    self.emit_code(crate::diagnostic::codes::E0242, "args expects 0 arguments");
                }
                return Type::Name(
                    "List".into(),
                    vec![Type::Name("string".into(), vec![])],
                );
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
                    self.emit_code(crate::diagnostic::codes::E0242, "reduce expects 3 arguments");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                    self.infer_expr(&args[2], scopes);
                }
                return Type::Name("unknown".into(), vec![]);
            }
            "sort" | "sort_f64" | "sort_str" | "reverse" | "flatten" => {
                if args.len() != 1 {
                    self.emit_code(
                        crate::diagnostic::codes::E0242,
                        format!("{} expects 1 argument", name),
                    );
                } else {
                    self.infer_expr(&args[0], scopes);
                }
                return Type::Name("List".into(), vec![Type::Name("unknown".into(), vec![])]);
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
                return Type::Name("List".into(), vec![Type::Name("unknown".into(), vec![])]);
            }
            "has_key" => {
                if args.len() != 2 {
                    self.emit_code(crate::diagnostic::codes::E0242, "has_key expects 2 arguments");
                } else {
                    self.infer_expr(&args[0], scopes);
                    self.infer_expr(&args[1], scopes);
                }
                return Type::Name("bool".into(), vec![]);
            }
            "map_new" => {
                return Type::Name("unknown".into(), vec![]);
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
                return Type::Name("unknown".into(), vec![]);
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
                return Type::Name("unknown".into(), vec![]);
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
                return Type::Name("unknown".into(), vec![]);
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
                return Type::Name("unknown".into(), vec![]);
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
                return Type::Option(
                    Box::new(Type::Name("i32".into(), vec![])),
                );
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
                return Type::Result(
                    Box::new(Type::Name("i32".into(), vec![])),
                    Box::new(Type::Name("string".into(), vec![])),
                );
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
                return Type::Result(
                    Box::new(Type::Name("f64".into(), vec![])),
                    Box::new(Type::Name("string".into(), vec![])),
                );
            }
            "eprintln" => {
                for a in args {
                    self.infer_expr(a, scopes);
                }
                return Type::Name("unit".into(), vec![]);
            }
            "format" => {
                if args.is_empty() {
                    self.emit_code(crate::diagnostic::codes::E0242, "format expects at least 1 argument (template string)");
                } else {
                    let tpl = self.infer_expr(&args[0], scopes);
                    if !crate::core::helpers::is_string(&tpl) {
                        self.emit_code(crate::diagnostic::codes::E0242,
                            format!("format expects a string template as first argument, found {}", fmt_type(&tpl)));
                    }
                    for a in &args[1..] { self.infer_expr(a, scopes); }
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
                return Type::Name("string".into(), vec![]);
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
                        for (i, (arg, param_ty)) in args.iter().zip(param_types.iter()).enumerate() {
                            let arg_ty = self.infer_expr(arg, scopes);
                            if !same_type(&arg_ty, param_ty) && !is_numeric_coercion(param_ty, &arg_ty) {
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
                            self.emit_code(crate::diagnostic::codes::E0242, "Some expects 1 argument");
                        } else {
                            let inner = self.infer_expr(&args[0], scopes);
                            return Type::Option(Box::new(inner));
                        }
                        return Type::Option(Box::new(Type::Name("_".into(), vec![])));
                    }
                    "None" => {
                        if !args.is_empty() {
                            self.emit_code(crate::diagnostic::codes::E0242, "None expects 0 arguments");
                        }
                        return Type::Option(Box::new(Type::Name("_".into(), vec![])));
                    }
                    "Ok" => {
                        if args.len() != 1 {
                            self.emit_code(crate::diagnostic::codes::E0242, "Ok expects 1 argument");
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
                            self.emit_code(crate::diagnostic::codes::E0242, "Err expects 1 argument");
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
            let func_def_params: Option<&[Param]> = self.file.items.iter()
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
                                    self.emit_code(crate::diagnostic::codes::E0401,
                                        format!("function '{}' has no parameter named '{}'", name, n));
                                }
                            }
                            _ => {
                                while pos_idx < seen.len() && seen[pos_idx] { pos_idx += 1; }
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
                        let reordered_args: Vec<Expr> = reordered.iter().map(|e| (*e).clone()).collect();
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
                // Infer type parameters from argument types
                for (arg, param) in args.iter().zip(params.iter()) {
                    let at = self.infer_expr(arg, scopes);
                    self.infer_type_params(param, &at, &generics, &mut type_map);
                }

                // Check where constraints (before substitution)
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

                // Check arguments with substituted types
                for (i, (arg, param)) in args.iter().zip(params.iter()).enumerate() {
                    let at = self.infer_expr(arg, scopes);
                    let subst_param = subst_type_params(param, &generics, &type_map);
                    if !same_type(&at, &subst_param) && !is_numeric_coercion(&subst_param, &at) {
                        self.errors.push(
                            Diagnostic::error_code(
                                crate::diagnostic::codes::E0211,
                                format!("argument {} of '{}' expected {}, found {}", i + 1, name, fmt_type(&subst_param), fmt_type(&at)),
                                Span::single(self.current_line, self.current_col),
                            ).with_help(format!("argument {} has type '{}', but '{}' expects type '{}'", i + 1, fmt_type(&at), name, fmt_type(&subst_param)))
                        );
                    }
                }

                ret = subst_type_params(&ret, &generics, &type_map);
            } else {
                for (i, (arg, param)) in args.iter().zip(params.iter()).enumerate() {
                    let at = self.infer_expr(arg, scopes);
                    if !same_type(&at, param) && !is_numeric_coercion(param, &at) {
                        self.errors.push(
                            Diagnostic::error_code(
                                crate::diagnostic::codes::E0211,
                                format!("argument {} of '{}' expected {}, found {}", i + 1, name, fmt_type(param), fmt_type(&at)),
                                Span::single(self.current_line, self.current_col),
                            ).with_help(format!("argument {} has type '{}', but '{}' expects type '{}'", i + 1, fmt_type(&at), name, fmt_type(param)))
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
}
