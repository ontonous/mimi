//! Progressive Typestate (v0.29.22).
//!
//! Zero-cost entry for scripts without `flow`/`state`/`transition`:
//! the compiler injects an invisible `flow Main { state Single }` so every
//! program lives under the Flow paradigm. Explicit `flow` disables this mode
//! and may emit a migration diagnostic (W011).

use crate::ast::*;

/// Apply progressive Typestate wrapping to a freshly parsed file.
///
/// Called after parse and **before** transfer-matrix expansion so the
/// injected Main flow receives Fault / peer_fault / reset / recover like
/// any other flow.
///
/// Returns `true` when script mode was activated (implicit Single injected).
pub fn apply_progressive_typestate(file: &mut File) -> bool {
    if file_has_user_flow(file) {
        file.implicit_single = false;
        return false;
    }
    // Script mode only when there is a top-level main — pure protocol/session/type
    // libraries stay unwrapped (no invisible Main).
    if !has_top_level_main(file) {
        file.implicit_single = false;
        return false;
    }
    // Script mode: inject invisible Main / Single.
    file.implicit_single = true;
    file.items.insert(0, Item::Flow(make_implicit_main_flow()));
    true
}

fn file_has_user_flow(file: &File) -> bool {
    file.items.iter().any(|item| match item {
        Item::Flow(_) => true,
        Item::Module(m) => m.items.iter().any(|i| matches!(i, Item::Flow(_))),
        _ => false,
    })
}

/// `flow Main { state Single; transition run(Single) -> Single { do { return Single { } } } }`
fn make_implicit_main_flow() -> FlowDef {
    let run_body = vec![Stmt::Return(Some(Expr::Record {
        ty: Some("Single".to_string()),
        fields: vec![],
    }))];
    FlowDef {
        name: "Main".to_string(),
        pub_: false,
        generics: vec![],
        annotations: vec![],
        states: vec![StateDef {
            name: "Single".to_string(),
            payload: None,
        }],
        transitions: vec![TransitionDef {
            name: "run".to_string(),
            from_state: "Single".to_string(),
            params: vec![],
            to_states: vec!["Single".to_string()],
            body: Some(run_body),
            pos: (0, 0),
            is_fallback: false,
        }],
        impl_protocols: vec![],
        persistent_fields: vec![],
        transactional_fields: vec![],
    }
}

/// Whether the file has a top-level `main` function (migration diagnostic target).
pub fn has_top_level_main(file: &File) -> bool {
    file.items.iter().any(|item| match item {
        Item::Func(f) => f.name == "main",
        _ => false,
    })
}

/// Collect names of simple locals in `main` body for migration help text.
pub fn main_local_names(file: &File) -> Vec<String> {
    for item in &file.items {
        if let Item::Func(f) = item {
            if f.name == "main" {
                let mut names = Vec::new();
                for stmt in &f.body {
                    if let Stmt::Let {
                        pat: Pattern::Variable(n),
                        ..
                    } = stmt
                    {
                        if !names.contains(n) {
                            names.push(n.clone());
                        }
                    }
                }
                return names;
            }
        }
    }
    Vec::new()
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::*;

    fn empty_file() -> File {
        File {
            imports: vec![],
            items: vec![],
            implicit_single: false,
        }
    }

    #[test]
    fn script_with_main_gets_implicit_main() {
        let mut file = empty_file();
        file.items.push(Item::Func(FuncDef {
            name: "main".into(),
            pub_: false,
            params: vec![],
            ret: Some(Type::Name("i32".into(), vec![])),
            body: vec![Stmt::Return(Some(Expr::Literal(Lit::Int(0))))],
            where_clause: vec![],
            generics: vec![],
            effects: vec![],
            is_comptime: false,
            is_async: false,
            extern_abi: None,
            pos: (1, 1),
        }));
        assert!(apply_progressive_typestate(&mut file));
        assert!(file.implicit_single);
        assert!(matches!(&file.items[0], Item::Flow(f) if f.name == "Main"));
        assert!(f_has_single(&file));
    }

    fn f_has_single(file: &File) -> bool {
        match &file.items[0] {
            Item::Flow(f) => f.states.iter().any(|s| s.name == "Single"),
            _ => false,
        }
    }

    #[test]
    fn explicit_flow_skips_injection() {
        let mut file = empty_file();
        file.items.push(Item::Flow(FlowDef {
            name: "User".into(),
            pub_: false,
            generics: vec![],
            annotations: vec![],
            states: vec![StateDef { name: "A".into(), payload: None }],
            transitions: vec![],
            impl_protocols: vec![],
            persistent_fields: vec![],
            transactional_fields: vec![],
        }));
        file.items.push(Item::Func(FuncDef {
            name: "main".into(),
            pub_: false,
            params: vec![],
            ret: Some(Type::Name("i32".into(), vec![])),
            body: vec![],
            where_clause: vec![],
            generics: vec![],
            effects: vec![],
            is_comptime: false,
            is_async: false,
            extern_abi: None,
            pos: (1, 1),
        }));
        assert!(!apply_progressive_typestate(&mut file));
        assert!(!file.implicit_single);
        assert_eq!(file.items.len(), 2);
    }

    #[test]
    fn no_main_no_injection() {
        let mut file = empty_file();
        file.items.push(Item::Type(TypeDef {
            name: "T".into(),
            pub_: false,
            kind: TypeDefKind::Record(vec![]),
            generics: vec![],
            derives: vec![],
            attributes: vec![],
        }));
        assert!(!apply_progressive_typestate(&mut file));
        assert!(!file.implicit_single);
    }
}
