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
    let main_meta = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Func(function) if function.name == "main" => Some(function.meta),
            _ => None,
        })
        .expect("has_top_level_main guaranteed a main function");
    file.items
        .insert(0, Item::Flow(make_implicit_main_flow(main_meta)));
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
fn make_implicit_main_flow(parent_meta: AstNodeMeta) -> FlowDef {
    let span = parent_meta.span;
    let run_origin = AstOrigin::RuntimeSystem("progressive.run");
    let run_result = Expr::Record {
        ty: Some("Single".to_string()),
        fields: vec![],
    }
    .with_meta(AstNodeMeta::new(span, run_origin));
    let run_body =
        vec![Stmt::Return(Some(run_result)).with_meta(AstNodeMeta::new(span, run_origin))];
    FlowDef {
        meta: AstNodeMeta::inherited(span, AstOrigin::RuntimeSystem("progressive.main"))
            .with_parent(AstParentHint::NamedFunction("main")),
        name: "Main".to_string(),
        pub_: false,
        generics: vec![],
        annotations: vec![],
        states: vec![StateDef {
            meta: AstNodeMeta::inherited(span, AstOrigin::RuntimeSystem("progressive.single")),
            name: "Single".to_string(),
            payload: None,
        }],
        transitions: vec![TransitionDef {
            meta: AstNodeMeta::inherited(span, run_origin),
            name: "run".to_string(),
            from_state: "Single".to_string(),
            params: vec![],
            to_states: vec!["Single".to_string()],
            body: Some(run_body),
            fails: None,
            is_fallback: false,
            is_ffi_pinned: false,
        }],
        impl_protocols: vec![],
        persistent_fields: vec![],
        transactional_fields: vec![],
        metadata_shadow_fields: vec![],
        fault_type: None,
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
                        pat:
                            Pattern {
                                kind: PatternKind::Variable(n),
                                ..
                            },
                        ..
                    } = stmt.unlocated()
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
    use crate::span::SourceId;

    const TEST_SOURCE: SourceId = SourceId::new(73);

    fn user_meta(line: usize) -> AstNodeMeta {
        AstNodeMeta::new(
            crate::span::Span::single(line, 1).with_source(TEST_SOURCE),
            AstOrigin::User,
        )
    }

    fn empty_file() -> File {
        File {
            sources: crate::span::SourceRegistry::default(),
            imports: vec![],
            items: vec![],
            implicit_single: false,
        }
    }

    #[test]
    fn script_with_main_gets_implicit_main() {
        let mut file = empty_file();
        file.items.push(Item::Func(FuncDef {
            meta: user_meta(1),
            name: "main".into(),
            pub_: false,
            params: vec![],
            ret: Some(Type::Name("i32".into(), vec![]).with_meta(user_meta(1))),
            body: vec![Stmt::Return(Some(Expr::Literal(Lit::Int(0))))],
            where_clause: vec![],
            generics: vec![],
            effects: vec![],
            is_comptime: false,
            is_async: false,
            extern_abi: None,
            has_requires: false,
            has_ensures: false,
            has_mutate_params: false,
        }));
        assert!(apply_progressive_typestate(&mut file));
        assert!(file.implicit_single);
        assert!(matches!(&file.items[0], Item::Flow(f) if f.name == "Main"));
        assert!(f_has_single(&file));

        let Item::Flow(flow) = &file.items[0] else {
            panic!("implicit Main flow")
        };
        assert_eq!(flow.meta.span, user_meta(1).span);
        assert_eq!(
            flow.meta.origin,
            AstOrigin::RuntimeSystem("progressive.main")
        );
        assert_eq!(
            flow.meta.parent,
            AstParentHint::NamedFunction("main"),
            "implicit Main records its causal user function explicitly"
        );
        assert_eq!(flow.states[0].meta.span.source_id, TEST_SOURCE);
        assert_eq!(
            flow.states[0].meta.origin,
            AstOrigin::RuntimeSystem("progressive.single")
        );
        assert_eq!(flow.states[0].meta.parent, AstParentHint::Enclosing);
        assert_eq!(flow.transitions[0].meta.span.source_id, TEST_SOURCE);
        assert_eq!(
            flow.transitions[0].meta.origin,
            AstOrigin::RuntimeSystem("progressive.run")
        );
        assert_eq!(flow.transitions[0].meta.parent, AstParentHint::Enclosing);
        assert!(flow.annotations.is_empty());

        let Item::Func(main) = &file.items[1] else {
            panic!("original main")
        };
        let ret_meta = main
            .ret
            .as_ref()
            .and_then(Type::meta)
            .expect("user return type metadata");
        assert_eq!(ret_meta.origin, AstOrigin::User);
        assert_eq!(ret_meta.span.source_id, TEST_SOURCE);
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
            meta: user_meta(1),
            name: "User".into(),
            pub_: false,
            generics: vec![],
            annotations: vec![],
            states: vec![StateDef {
                meta: user_meta(2),
                name: "A".into(),
                payload: None,
            }],
            transitions: vec![],
            impl_protocols: vec![],
            persistent_fields: vec![],
            transactional_fields: vec![],
            metadata_shadow_fields: vec![],
            fault_type: None,
        }));
        file.items.push(Item::Func(FuncDef {
            meta: user_meta(1),
            name: "main".into(),
            pub_: false,
            params: vec![],
            ret: Some(Type::Name("i32".into(), vec![]).with_meta(user_meta(1))),
            body: vec![],
            where_clause: vec![],
            generics: vec![],
            effects: vec![],
            is_comptime: false,
            is_async: false,
            extern_abi: None,
            has_requires: false,
            has_ensures: false,
            has_mutate_params: false,
        }));
        assert!(!apply_progressive_typestate(&mut file));
        assert!(!file.implicit_single);
        assert_eq!(file.items.len(), 2);
    }

    #[test]
    fn no_main_no_injection() {
        let mut file = empty_file();
        file.items.push(Item::Type(TypeDef {
            meta: AstNodeMeta::synthetic(AstOrigin::RuntimeSystem("test.fixture")),
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
