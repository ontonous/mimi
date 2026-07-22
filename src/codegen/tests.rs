//! Unit tests for the low-level LLVM builder helpers introduced in v0.28.8.
//!
//! These tests verify that the thin wrappers around inkwell keep the same
//! semantics and produce valid in-memory LLVM instructions. They are cheap
//! (no object compilation / linking) and fast to run.

use super::*;
use inkwell::context::Context;
use inkwell::types::BasicType;
use inkwell::values::BasicValue;

fn with_test_function<'ctx, F>(context: &'ctx Context, f: F)
where
    F: FnOnce(&CodeGenerator<'ctx>),
{
    let cg = CodeGenerator::new(context, "test_mod");
    let i64_ty = context.i64_type().as_basic_type_enum();
    let fn_type = i64_ty.fn_type(&[], false);
    let function = cg.module.add_function("test_fn", fn_type, None);
    let entry = context.append_basic_block(function, "entry");
    cg.builder.position_at_end(entry);
    f(&cg);
}

#[test]
fn runtime_functions_are_registered() {
    let context = Context::create();
    let cg = CodeGenerator::new(&context, "test_mod");
    assert!(cg.get_runtime_fn("mimi_now").is_ok());
    assert!(cg.get_runtime_fn("mimi_lexer_tokenize").is_ok());
    assert!(cg.get_runtime_fn("mimi_parse_source").is_ok());
    assert!(cg.get_runtime_fn("__does_not_exist").is_err());
}

#[test]
fn alloca_store_load_roundtrip() {
    let context = Context::create();
    with_test_function(&context, |cg| {
        let ptr = cg.build_alloca(cg.context.i64_type(), "x").unwrap();
        let val = cg.context.i64_type().const_int(42, false);
        cg.build_store(ptr, val).unwrap();
        let loaded = cg
            .build_load(cg.context.i64_type(), ptr, "loaded")
            .unwrap()
            .into_int_value();
        assert_eq!(loaded.get_type(), cg.context.i64_type());
        assert!(loaded.as_instruction_value().is_some());
    });
}

#[test]
fn entry_alloca_is_placed_at_function_entry() {
    let context = Context::create();
    with_test_function(&context, |cg| {
        let i64_ty = cg.context.i64_type().as_basic_type_enum();
        // Build a normal alloca at the current insertion point.
        let _mid = cg.build_alloca(i64_ty, "mid").unwrap();
        // entry_alloca must be inserted before `mid`, i.e. at the top of the block.
        let _top = cg.entry_alloca(i64_ty, "top").unwrap();
        let entry = cg.builder.get_insert_block().unwrap();
        let first = entry.get_first_instruction().unwrap();
        let name = first
            .get_name()
            .and_then(|n| n.to_str().ok())
            .expect("entry alloca should be named");
        assert_eq!(name, "top");
    });
}

#[test]
fn call_and_return_helpers() {
    let context = Context::create();
    with_test_function(&context, |cg| {
        let entry = cg.builder.get_insert_block().unwrap();
        let now_fn = cg.get_runtime_fn("mimi_now").unwrap();
        let call = cg.build_call(now_fn, &[], "now").unwrap();
        let value = call.try_as_basic_value_opt().unwrap();
        cg.build_return(Some(&value)).unwrap();
        assert!(entry.get_terminator().is_some());
    });
}

#[test]
fn branch_helpers_create_terminators() {
    let context = Context::create();
    with_test_function(&context, |cg| {
        let function = cg.current_function().unwrap();
        let entry = cg.builder.get_insert_block().unwrap();
        let bb1 = cg.context.append_basic_block(function, "bb1");
        let bb2 = cg.context.append_basic_block(function, "bb2");
        let bb3 = cg.context.append_basic_block(function, "bb3");

        cg.build_br(bb1).unwrap();
        assert!(entry.get_terminator().is_some());

        cg.builder.position_at_end(bb1);
        let cond = cg.context.bool_type().const_int(1, false);
        cg.build_cond_br(cond, bb2, bb3).unwrap();
        assert!(bb1.get_terminator().is_some());
    });
}

#[test]
fn gep_and_aggregate_helpers() {
    let context = Context::create();
    with_test_function(&context, |cg| {
        let i64_ty = cg.context.i64_type();
        let ptr_ty = cg.context.ptr_type(inkwell::AddressSpace::default());
        let struct_ty = cg.context.struct_type(
            &[i64_ty.as_basic_type_enum(), ptr_ty.as_basic_type_enum()],
            false,
        );

        let alloca = cg.build_alloca(struct_ty, "agg").unwrap();
        let zero = cg.context.i32_type().const_int(0, false);
        let gep = cg
            .build_in_bounds_gep(struct_ty, alloca, &[zero, zero], "field0_ptr")
            .unwrap();

        let stored = i64_ty.const_int(7, false);
        cg.build_store(gep, stored).unwrap();
        let loaded = cg
            .build_load(i64_ty, gep, "field0")
            .unwrap()
            .into_int_value();
        assert_eq!(loaded.get_type(), i64_ty);
        assert!(loaded.as_instruction_value().is_some());

        // extractvalue works on the loaded aggregate value.
        let agg_val = cg
            .build_load(struct_ty, alloca, "agg_val")
            .unwrap()
            .into_struct_value();
        let extracted = cg
            .build_extract_value(agg_val.into(), 0, "ext0")
            .unwrap()
            .into_int_value();
        assert_eq!(extracted.get_type(), i64_ty);
        assert!(extracted.as_instruction_value().is_some());
    });
}

#[test]
fn pointer_and_int_helpers() {
    let context = Context::create();
    with_test_function(&context, |cg| {
        let i64_ty = cg.context.i64_type();
        let ptr_ty = cg.context.ptr_type(inkwell::AddressSpace::default());

        let ptr = cg.build_alloca(i64_ty, "slot").unwrap();
        let int_val = cg.build_ptr_to_int(ptr, i64_ty, "ptr_as_int").unwrap();
        let round_trip = cg.build_int_to_ptr(int_val, ptr_ty, "int_as_ptr").unwrap();

        // Sanity: we got a pointer value back.
        assert_eq!(round_trip.get_type(), ptr_ty);

        // bit_cast between same-sized integer types should succeed.
        let i32_ty = cg.context.i32_type();
        let small = i32_ty.const_int(5, false);
        let cast = cg
            .build_bit_cast(small.into(), i32_ty.as_basic_type_enum(), "id_cast")
            .unwrap();
        assert_eq!(cast.into_int_value().get_zero_extended_constant(), Some(5));

        // pointercast to the same pointer type is a no-op but exercises the helper.
        let same = cg.build_pointer_cast(ptr, ptr_ty, "same_ptr").unwrap();
        assert_eq!(same.get_type(), ptr_ty);
    });
}

#[test]
fn extract_list_elem_type_parses_nested_generics() {
    use crate::ast::Type;

    let simple = extract_list_elem_type("List<i32>").unwrap();
    assert!(
        matches!(simple.unlocated(), Type::Name(name, args) if name == "i32" && args.is_empty())
    );

    let nested = extract_list_elem_type("List<List<string>>").unwrap();
    assert!(
        matches!(nested.unlocated(), Type::Name(name, args) if name == "List" && args.len() == 1)
    );

    assert!(extract_list_elem_type("i32").is_none());
    assert!(extract_list_elem_type("List<>").is_none());
}

#[test]
fn parse_inner_type_handles_generics() {
    use crate::ast::Type;

    assert!(matches!(
        parse_inner_type("i32"),
        Type::Name(name, args) if name == "i32" && args.is_empty()
    ));

    assert!(matches!(
        parse_inner_type("Map<string, i32>"),
        Type::Name(name, args) if name == "Map" && args.len() == 2
    ));

    assert!(matches!(
        parse_inner_type("List<Map<string, i64>>"),
        Type::Name(name, args) if name == "List" && args.len() == 1
    ));
}

#[test]
fn flow_matrix_generated_transition_function_types_share_lowering_origin() {
    use crate::ast::{AstNodeMeta, AstOrigin, FlowDef, Param, TransitionDef, Type};
    use crate::span::{SourceId, Span};

    let source = SourceId::new(91);
    let transition_span = Span::new(8, 3, 8, 42).with_source(source);
    let user_type_meta =
        AstNodeMeta::new(Span::new(8, 20, 8, 37).with_source(source), AstOrigin::User);
    let user_type = Type::Name(
        "List".into(),
        vec![Type::Option(Box::new(
            Type::Name("i32".into(), vec![]).with_meta(user_type_meta),
        ))
        .with_meta(user_type_meta)],
    )
    .with_meta(user_type_meta);
    let transition = TransitionDef {
        meta: AstNodeMeta::inherited(
            transition_span,
            AstOrigin::PrototypeFallback("flow.matrix.fallback"),
        ),
        name: "event".into(),
        from_state: "Ready".into(),
        params: vec![Param {
            meta: user_type_meta,
            name: "items".into(),
            ty: user_type,
            mut_: false,
            default_value: None,
            borrow: None,
        }],
        to_states: vec!["Fault".into()],
        body: None,
        fails: None,
        is_fallback: true,
        is_ffi_pinned: false,
    };
    let flow = FlowDef {
        meta: AstNodeMeta::new(transition_span, AstOrigin::User),
        name: "Worker".into(),
        pub_: false,
        generics: vec![],
        annotations: vec![],
        states: vec![],
        transitions: vec![],
        impl_protocols: vec![],
        persistent_fields: vec![],
        transactional_fields: vec![],
        metadata_shadow_fields: vec![],
    };

    let generated = CodeGenerator::<'static>::transition_to_func(&flow, &transition);
    let expected = AstNodeMeta::inherited(
        transition_span,
        AstOrigin::RuntimeSystem("codegen.transition_lowering"),
    );
    assert_eq!(generated.meta, expected);
    assert_eq!(generated.params[0].meta, expected);
    assert_eq!(generated.params[0].ty.meta(), Some(expected));
    assert_eq!(generated.params[1].meta, expected);
    assert_eq!(generated.params[1].ty.meta(), Some(expected));
    let Type::Name(_, args) = generated.params[1].ty.unlocated() else {
        panic!("generated List parameter")
    };
    assert_eq!(args[0].meta(), Some(expected));
    let Type::Option(inner) = args[0].unlocated() else {
        panic!("generated Option parameter")
    };
    assert_eq!(inner.meta(), Some(expected));
    assert_eq!(generated.ret.as_ref().and_then(Type::meta), Some(expected));
    assert_eq!(generated.params[1].ty, transition.params[0].ty);

    assert_eq!(transition.params[0].meta.origin, AstOrigin::User);
    assert_eq!(
        transition.params[0].ty.meta().unwrap().origin,
        AstOrigin::User
    );
}
