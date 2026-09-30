use crate::ast::*;
use crate::core::checker::Checker;
use std::collections::HashMap;

mod helpers;
mod method;
mod simple;

impl<'a> Checker<'a> {
    pub(in crate::core) fn infer_call_expr(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        scopes: &mut Vec<HashMap<String, Type>>,
    ) -> Type {
        match callee.unlocated() {
            Expr::Ident(name) => self.check_call(name, args, scopes),
            Expr::Field(obj, method_name) => self.infer_method_call(obj, method_name, args, scopes),
            _ => {
                // 0.36.28: infer the callee expression itself first so its
                // own diagnostics surface instead of being masked by the
                // call-shape error — e.g. `x?.to_string()` on a plain i32
                // must report E0224 (`?.` requires Option/Result receiver)
                // as well as the final "callee must be a function name".
                // The callee's type is discarded: a non-Ident/Field callee
                // is never a valid callable, so the verdict stands.
                let _ = self.infer_expr(callee, scopes);
                self.emit_code(
                    crate::diagnostic::codes::E0223,
                    "callee must be a function name",
                );
                Type::Name("unknown".into(), vec![])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn located_ident(name: &str, line: usize, col: usize) -> Expr {
        Expr::Ident(name.into()).with_meta(crate::ast::AstNodeMeta::new(
            crate::span::Span::new(line, col, line, col + name.len()),
            crate::ast::AstOrigin::User,
        ))
    }

    fn assert_one_any_ingress_error(
        checker: &Checker<'_>,
        expected_line: usize,
        expected_col: usize,
    ) {
        assert_eq!(
            checker.errors.len(),
            1,
            "unexpected diagnostics: {:?}",
            checker.errors
        );
        let diagnostic = &checker.errors[0];
        assert_eq!(
            diagnostic.code.as_deref(),
            Some(crate::diagnostic::codes::E0432)
        );
        assert_eq!(diagnostic.span.start_line, expected_line);
        assert_eq!(diagnostic.span.start_col, expected_col);
    }

    #[test]
    fn generic_custom_any_parameter_uses_shared_ingress_guard() {
        let file = crate::tests::parse("func main() -> i32 { 0 }");
        let mut checker = Checker::new(&file);
        checker.funcs.insert(
            "custom_sink".into(),
            (
                vec![
                    Type::Name("Any".into(), vec![]),
                    Type::Name("T".into(), vec![]),
                ],
                Type::Name("unit".into(), vec![]),
            ),
        );
        checker.func_generics.insert(
            "custom_sink".into(),
            vec![crate::ast::GenericParam {
                meta: crate::ast::AstNodeMeta::synthetic(crate::ast::AstOrigin::RuntimeSystem(
                    "infer.call.tests.custom_any",
                )),
                name: "T".into(),
                bounds: vec![],
                kind: crate::ast::GenericKind::Free,
            }],
        );
        let mut scopes = vec![HashMap::from([
            ("token".into(), Type::Name("SystemToken".into(), vec![])),
            ("count".into(), Type::Name("i32".into(), vec![])),
        ])];
        let args = [located_ident("token", 7, 18), located_ident("count", 7, 25)];

        checker.check_call("custom_sink", &args, &mut scopes);

        assert_one_any_ingress_error(&checker, 7, 18);
    }

    #[test]
    fn local_callable_any_parameter_uses_shared_ingress_guard() {
        let file = crate::tests::parse("func main() -> i32 { 0 }");
        let mut checker = Checker::new(&file);
        let mut scopes = vec![HashMap::from([
            ("token".into(), Type::Name("SystemToken".into(), vec![])),
            (
                "local_sink".into(),
                Type::Func(
                    vec![Type::Name("Any".into(), vec![])],
                    Box::new(Type::Name("unit".into(), vec![])),
                ),
            ),
        ])];
        let args = [located_ident("token", 9, 20)];

        checker.check_call("local_sink", &args, &mut scopes);

        assert_one_any_ingress_error(&checker, 9, 20);
    }

    #[test]
    fn transparent_alias_shapes_keep_any_holes_visible_beside_typevars() {
        let file = crate::tests::parse(
            "type TokenPairs = List<(string, SystemToken)>\n\
             type OptionalAny = Option<Any>\n\
             type ResultAny = Result<Any, i32>\n\
             func main() -> i32 { 0 }",
        );
        let mut checker = Checker::new(&file);
        for item in &file.items {
            if let Item::Type(type_def) = item {
                checker
                    .types
                    .insert(type_def.name.clone(), type_def.clone());
            }
        }

        let alias_actual = Type::Name("TokenPairs".into(), vec![]);
        let nested_any_expected = Type::Name(
            "List".into(),
            vec![Type::Tuple(vec![
                Type::Name("string".into(), vec![]),
                Type::Name("Any".into(), vec![]),
            ])],
        );
        assert!(checker.linear_value_erased_by_any(&alias_actual, &nested_any_expected));

        let actual_generic_sibling = checker.unification.fresh_var();
        let expected_generic_sibling = checker.unification.fresh_var();
        let typevar_first_actual = Type::Tuple(vec![
            Type::TypeVar(actual_generic_sibling),
            Type::Option(Box::new(Type::Name("SystemToken".into(), vec![]))),
        ]);
        let alias_any_second_expected = Type::Tuple(vec![
            Type::TypeVar(expected_generic_sibling),
            Type::Name("OptionalAny".into(), vec![]),
        ]);
        assert!(
            checker.linear_value_erased_by_any(&typevar_first_actual, &alias_any_second_expected)
        );

        let actual_generic_sibling = checker.unification.fresh_var();
        let expected_generic_sibling = checker.unification.fresh_var();
        let alias_any_first_actual = Type::Tuple(vec![
            Type::Option(Box::new(Type::Name("SystemToken".into(), vec![]))),
            Type::TypeVar(actual_generic_sibling),
        ]);
        let alias_any_first_expected = Type::Tuple(vec![
            Type::Name("OptionalAny".into(), vec![]),
            Type::TypeVar(expected_generic_sibling),
        ]);
        assert!(
            checker.linear_value_erased_by_any(&alias_any_first_actual, &alias_any_first_expected)
        );

        let result_struct_actual = Type::Result(
            Box::new(Type::Name("SystemToken".into(), vec![])),
            Box::new(Type::Name("i32".into(), vec![])),
        );
        let result_alias_expected = Type::Name("ResultAny".into(), vec![]);
        assert!(checker.linear_value_erased_by_any(&result_struct_actual, &result_alias_expected));

        let result_name_actual = Type::Name(
            "Result".into(),
            vec![
                Type::Name("SystemToken".into(), vec![]),
                Type::Name("i32".into(), vec![]),
            ],
        );
        let result_struct_expected = Type::Result(
            Box::new(Type::Name("Any".into(), vec![])),
            Box::new(Type::Name("i32".into(), vec![])),
        );
        assert!(checker.linear_value_erased_by_any(&result_name_actual, &result_struct_expected));
    }
}
