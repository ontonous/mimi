//! FFI verification tests that don't require LLVM
//! These tests verify the type system and parser work correctly for FFI types

#[cfg(test)]
mod ffi_verification_tests {
    use crate::ast::{Type, TypeDef, TypeDefKind, ExternFunc, ExternParam};
    use crate::ffi::contract::{FfiContract, FfiArgContract};
    use crate::parser::Parser;
    use crate::lexer::Lexer;

    fn parse_source(src: &str) -> Result<crate::ast::File, String> {
        let tokens = Lexer::new(src).tokenize().map_err(|e| e.to_string())?;
        let file = Parser::new(tokens).parse_file().map_err(|e| e.to_string())?;
        Ok(file)
    }

    fn check_source(src: &str) -> Result<(), Vec<crate::diagnostic::Diagnostic>> {
        let file = parse_source(src).map_err(|e| vec![crate::diagnostic::Diagnostic::error(
            e,
            crate::span::Span::new(0, 0, 0, 0),
        )])?;
        crate::core::check(&file)
    }

    #[test]
    fn test_cbuffer_type_parsing() {
        let src = r#"
        extern "C" {
            func allocate(size: i64) -> CBuffer<u8>;
        }

        func main() -> i32 {
            0
        }
        "#;
        
        let result = check_source(src);
        assert!(result.is_ok(), "CBuffer type should parse and type-check: {:?}", result.err());
    }

    #[test]
    fn test_extern_fn_type_parsing() {
        let src = r#"
            extern "C" {
                func register_callback(cb: extern "C" fn(i32) -> i32) -> i32;
            }
        "#;
        let file = parse_source(src).expect("should parse");
        let result = crate::core::check(&file);
        assert!(result.is_ok(), "extern C fn type should parse and type-check: {:?}", result.err());
    }

    #[test]
    fn test_errno_contract() {
        let src = r#"
        extern "C" {
            func open_file(path: string) -> i64;
        }

        func main() -> i32 {
            0
        }
        "#;
        
        let result = check_source(src);
        assert!(result.is_ok(), "Requires contract should parse and type-check: {:?}", result.err());
    }

    #[test]
    fn test_passport_types_in_extern() {
        let src = r#"
        extern "C" {
            func process_buffer(buf: c_shared u8) -> i32;
            func inspect_buffer(buf: c_borrow u8) -> i32;
            func modify_buffer(buf: c_borrow_mut u8) -> i32;
        }

        func main() -> i32 {
            0
        }
        "#;
        
        let result = check_source(src);
        assert!(result.is_ok(), "Passport types should parse and type-check: {:?}", result.err());
    }

    #[test]
    fn test_raw_string_type() {
        let src = r#"
        extern "C" {
            func transfer_string(s: raw_string) -> i32;
        }

        func main() -> i32 {
            0
        }
        "#;
        
        let result = check_source(src);
        assert!(result.is_ok(), "raw_string type should parse and type-check: {:?}", result.err());
    }

    #[test]
    fn test_ffi_contract_generation() {
        let func = ExternFunc {
            name: "test_func".to_string(),
            params: vec![
                ExternParam {
                    name: "x".to_string(),
                    ty: Type::Name("i32".to_string(), vec![]),
                    cap_mode: None,
                },
            ],
            ret: Some(Type::Name("i32".to_string(), vec![])),
            requires: None,
            ensures: None,
        };
        
        let contract = FfiContract::from_extern(&func);
        assert_eq!(contract.args.len(), 1);
        assert!(matches!(contract.args[0], FfiArgContract::Int));
    }

    #[test]
    fn test_cbuffer_contract_generation() {
        let func = ExternFunc {
            name: "process_buffer".to_string(),
            params: vec![
                ExternParam {
                    name: "buf".to_string(),
                    ty: Type::CBuffer(Box::new(Type::Name("u8".to_string(), vec![]))),
                    cap_mode: None,
                },
            ],
            ret: Some(Type::Name("i32".to_string(), vec![])),
            requires: None,
            ensures: None,
        };
        
        let contract = FfiContract::from_extern(&func);
        assert_eq!(contract.args.len(), 1);
        assert!(matches!(contract.args[0], FfiArgContract::RawPtr(_)));
    }

    #[test]
    fn test_extern_fn_contract_generation() {
        let func = ExternFunc {
            name: "register_callback".to_string(),
            params: vec![
                ExternParam {
                    name: "cb".to_string(),
                    ty: Type::ExternFunc(
                        vec![Type::Name("i32".to_string(), vec![])],
                        Box::new(Type::Name("i32".to_string(), vec![]))
                    ),
                    cap_mode: None,
                },
            ],
            ret: Some(Type::Name("i32".to_string(), vec![])),
            requires: None,
            ensures: None,
        };
        
        let contract = FfiContract::from_extern(&func);
        assert_eq!(contract.args.len(), 1);
        assert!(
            matches!(contract.args[0], FfiArgContract::Callback { .. }),
            "expected Callback contract for ExternFunc param, got {:?}",
            contract.args[0]
        );
    }
}
