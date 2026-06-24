use super::*;
use crate::contracts;
use std::collections::HashMap;

#[test]
fn contract_extract_requires() {
    let text = "requires: amount > 0\nensures: result >= 0\nmath: amount * 2";
    let contract = contracts::extract_contracts(text);
    assert_eq!(contract.requires.len(), 1);
    assert_eq!(contract.requires[0], "amount > 0");
    assert_eq!(contract.ensures.len(), 1);
    assert_eq!(contract.ensures[0], "result >= 0");
    assert_eq!(contract.math.len(), 1);
    assert_eq!(contract.math[0], "amount * 2");
}

#[test]
fn contract_extract_multiple_requires() {
    let text = "requires: amount > 0\nrequires: balance >= amount\nensures: result >= 0";
    let contract = contracts::extract_contracts(text);
    assert_eq!(contract.requires.len(), 2);
    assert_eq!(contract.requires[0], "amount > 0");
    assert_eq!(contract.requires[1], "balance >= amount");
}

#[test]
fn contract_extract_empty() {
    let text = "just a description";
    let contract = contracts::extract_contracts(text);
    assert!(contract.requires.is_empty());
    assert!(contract.ensures.is_empty());
    assert!(contract.math.is_empty());
}

#[test]
fn contract_bind_to_function() {
    let src = r#"
func pay(amount: i32) -> i32 {
    mms {
        "requires: amount > 0"
    }
    amount
}

func main() -> i32 {
    pay(100)
}
"#;
    let file = parse(src);
    // Verify mms block exists in the parsed AST
    let func = file.items.iter().find_map(|item| {
        if let crate::ast::Item::Func(f) = item {
            if f.name == "pay" { Some(f) } else { None }
        } else { None }
    });
    assert!(func.is_some());
    let has_mms = func.expect("src/tests/v1_2_contract_extract.rs:57 unwrap failed").body.iter().any(|s| matches!(s, crate::ast::Stmt::MmsBlock { .. }));
    assert!(has_mms, "mms block should be present in parsed function body");
}

#[test]
fn contract_bind_and_check() {
    let src = r#"
func pay(amount: i32) -> i32 {
    mms {
        "requires: amount > 0"
    }
    amount
}

func main() -> i32 {
    pay(100)
}
"#;
    // Parse, bind contracts, then check
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("src/tests/v1_2_contract_extract.rs:76 unwrap failed");
    let mut file = crate::parser::Parser::new(tokens).parse_file().expect("src/tests/v1_2_contract_extract.rs:77 unwrap failed");
    let contracts_map = extract_contracts_from_file(&file);
    let errors = contracts::bind_contracts(&mut file, contracts_map);
    assert!(errors.is_empty(), "contract binding should not produce errors: {:?}", errors);
    // Should type-check successfully
    let result = crate::core::check(&file);
    assert!(result.is_ok(), "contract binding should not break type checking");
}

fn extract_contracts_from_file(file: &crate::ast::File) -> HashMap<String, contracts::Contract> {
    let mut result = HashMap::new();
    for item in &file.items {
        if let crate::ast::Item::Func(func) = item {
            let mut contract = contracts::Contract::default();
            for stmt in &func.body {
                if let crate::ast::Stmt::MmsBlock { content: text, .. } = stmt {
                    let c = contracts::extract_contracts(text);
                    contract.requires.extend(c.requires);
                    contract.ensures.extend(c.ensures);
                    contract.math.extend(c.math);
                }
            }
            if !contract.requires.is_empty() || !contract.ensures.is_empty() || !contract.math.is_empty() {
                result.insert(func.name.clone(), contract);
            }
        }
    }
    result
}
