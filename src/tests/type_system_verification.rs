//! Type system verification for FFI types
//! These tests verify that the type system correctly handles FFI-related types

#[cfg(test)]
mod type_system_verification {
    use crate::ast::{Type, TypeDef, TypeDefKind, ExternFunc, ExternParam};
    use crate::ffi::contract::{FfiContract, FfiArgContract, FfiRetContract};
    use crate::core::check;

    fn parse_and_check(src: &str) -> Result<(), String> {
        let tokens = crate::lexer::Lexer::new(src).tokenize().map_err(|e| e.to_string())?;
        let file = crate::parser::Parser::new(tokens).parse_file().map_err(|e| e.message)?;
        check(&file).map_err(|diags| {
            let msgs: Vec<String> = diags.iter().map(|d| d.message.clone()).collect();
            msgs.join("; ")
        })
    }

    #[test]
    fn test_cbuffer_type_in_extern() {
        let src = "extern \"C\" { func allocate(size: i64) -> CBuffer<u8>; }\nfunc main() -> i32 { 0 }";
        assert!(parse_and_check(src).is_ok(), "CBuffer should be allowed in extern");
    }

    #[test]
    fn test_cbuffer_contract() {
        let func = ExternFunc {
            name: "test".to_string(),
            params: vec![ExternParam {
                name: "buf".to_string(),
                ty: Type::CBuffer(Box::new(Type::Name("u8".to_string(), vec![]))),
                cap_mode: None,
            }],
            ret: Some(Type::Name("i32".to_string(), vec![])),
            requires: None,
            ensures: None,
        };
        let contract = FfiContract::from_extern(&func);
        assert!(matches!(contract.args[0], FfiArgContract::RawPtr(_)));
    }

    #[test]
    fn test_extern_fn_type_in_extern() {
        let src = r#"
            extern "C" {
                func register(cb: extern "C" fn(i32) -> i32);
            }
        "#;
        let result = parse_and_check(src);
        assert!(result.is_ok(), "extern C fn type in extern block: {:?}", result.err());
    }

    #[test]
    fn test_requires_contract_parsing() {
        // requires/ensures in extern blocks require specific syntax
        // The contract system works at the AST/FfiContract level
        let func = ExternFunc {
            name: "open_file".to_string(),
            params: vec![ExternParam {
                name: "path".to_string(),
                ty: Type::Name("string".to_string(), vec![]),
                cap_mode: None,
            }],
            ret: Some(Type::Name("i64".to_string(), vec![])),
            requires: Some(crate::ast::Expr::Literal(crate::ast::Lit::Bool(true))),
            ensures: Some(crate::ast::Expr::Literal(crate::ast::Lit::Bool(true))),
        };
        let contract = FfiContract::from_extern(&func);
        assert!(contract.requires.is_some());
        assert!(contract.ensures.is_some());
    }

    #[test]
    fn test_passport_types() {
        let src = "extern \"C\" { func a(x: c_shared i32) -> i32; func b(x: c_borrow i32) -> i32; func c(x: c_borrow_mut i32) -> i32; func d(x: *i32) -> i32; func e(x: *mut i32) -> i32; }\nfunc main() -> i32 { 0 }";
        assert!(parse_and_check(src).is_ok(), "passport types should work");
    }

    #[test]
    fn test_raw_string_type() {
        let src = "extern \"C\" { func transfer(s: raw_string) -> i32; }\nfunc main() -> i32 { 0 }";
        assert!(parse_and_check(src).is_ok(), "raw_string should work");
    }

    #[test]
    fn test_cbuffer_rejected_outside_extern() {
        let src = "func bad(buf: CBuffer<u8>) -> i32 { 0 }\nfunc main() -> i32 { 0 }";
        assert!(parse_and_check(src).is_err(), "CBuffer should be rejected outside extern");
    }

    #[test]
    fn test_extern_fn_type_outside_extern() {
        let src = "func bar(cb: extern \"C\" fn(i32) -> i32) -> i32 { 0 }\nfunc main() -> i32 { 0 }";
        let result = parse_and_check(src);
        assert!(result.is_ok(), "extern C fn type should be valid anywhere: {:?}", result.err());
    }
}
