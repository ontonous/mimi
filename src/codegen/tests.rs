//! Unit tests for the low-level LLVM builder helpers introduced in v0.28.8.
//!
//! These tests verify that the thin wrappers around inkwell keep the same
//! semantics and produce valid in-memory LLVM instructions. They are cheap
//! (no object compilation / linking) and fast to run.

use super::*;
use inkwell::context::Context;
use inkwell::types::BasicType;
use inkwell::values::BasicValue;
use std::collections::HashMap;

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
fn legacy_string_and_all_text_fstring_globals_preserve_embedded_nul_bytes() {
    let context = Context::create();
    let mut cg = CodeGenerator::new(&context, "length_safe_string_globals");
    let function = cg
        .module
        .add_function("test_fn", context.void_type().fn_type(&[], false), None);
    let entry = context.append_basic_block(function, "entry");
    cg.builder.position_at_end(entry);

    let vars = HashMap::new();
    let text = "prefix\0left";
    cg.compile_literal_expr(&crate::ast::Lit::String(text.into()), &vars)
        .expect("string literal should lower");
    cg.compile_fstring(&[crate::ast::FStringPart::Text(text.into())], &vars)
        .expect("all-text f-string should lower");
    cg.builder
        .build_return(None)
        .expect("return from test function");

    let ir = cg.module.print_to_string().to_string();
    let nul_preserving_global = "c\"prefix\\00left\\00\"";
    assert_eq!(
        ir.matches(nul_preserving_global).count(),
        2,
        "ordinary string and all-text f-string must both retain the full byte sequence: {ir}"
    );
    cg.module
        .verify()
        .expect("generated string globals must form valid LLVM IR");
}

#[test]
fn extern_string_ensures_use_the_validated_mimi_result_value() {
    let file = crate::tests::parse_prod(
        r#"
        extern "C" {
            func foreign_text() -> string
                ensures: len(result) == 1;
        }
    "#,
    );
    let block = file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::ExternBlock(block) => Some(block),
            _ => None,
        })
        .expect("extern block");

    let context = Context::create();
    let mut cg = CodeGenerator::new(&context, "extern_string_ensures_result");
    cg.verify_contracts = true;
    cg.register_extern_block(block)
        .expect("register extern declaration");
    cg.generate_extern_fn("foreign_text")
        .expect("emit validated String return and ensures contract");

    let ir = cg.module.print_to_string().to_string();
    assert!(ir.contains("mimi_c_string_len_utf8"), "{ir}");
    assert!(ir.contains("mimi_string_to_cstr_copy"), "{ir}");
    assert!(
        ir.contains("result = alloca { ptr, i64 }"),
        "ensures must bind result as Mimi's pointer-and-length value, not raw C `char*`: {ir}"
    );
    cg.module
        .verify()
        .expect("extern wrapper must form valid LLVM IR");
}

#[test]
fn actor_runtime_handles_and_state_layouts_are_distinct() {
    let context = Context::create();
    let mut cg = CodeGenerator::new(&context, "actor_runtime_value_abi");
    let actor_name = "Counter".to_string();
    let actor_state = context.struct_type(&[context.i32_type().into()], false);
    let actor_ptr = context.ptr_type(inkwell::AddressSpace::default());
    cg.actor_names.insert(actor_name.clone());
    cg.type_llvm.insert(actor_name.clone(), actor_state.into());

    let actor_ty = crate::ast::Type::Name(actor_name.clone(), vec![]);
    assert_eq!(
        cg.llvm_type_for(&actor_ty),
        Some(BasicTypeEnum::PointerType(actor_ptr)),
        "actor value ABI is the opaque runtime handle"
    );
    assert_eq!(
        cg.type_llvm.get(&actor_name),
        Some(&BasicTypeEnum::StructType(actor_state)),
        "actor state remains available as its own field struct"
    );

    let product_ty = crate::ast::Type::Tuple(vec![
        crate::ast::Type::Name("i32".into(), vec![]),
        actor_ty.clone(),
        actor_ty,
    ]);
    let product = cg
        .llvm_type_for(&product_ty)
        .expect("actor product has a value layout")
        .into_struct_type();
    assert_eq!(
        product.get_field_types(),
        vec![
            BasicTypeEnum::IntType(context.i64_type()),
            BasicTypeEnum::PointerType(actor_ptr),
            BasicTypeEnum::PointerType(actor_ptr),
        ],
        "product ABI widens narrow integers and keeps actor handles as pointers"
    );
}

#[test]
fn checked_primitive_alias_layout_does_not_read_the_surface_target() {
    let source = r#"
type InnerText = string
type Text = InnerText
func main() -> i32 { 0 }
"#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let mut text_declaration = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Type(declaration) if declaration.name == "Text" => Some(declaration.clone()),
            _ => None,
        })
        .expect("Text alias header");
    let crate::ast::TypeDefKind::Alias(target) = &mut text_declaration.kind else {
        panic!("Text must be a transparent alias");
    };
    *target = Type::Name("i64".into(), vec![]);

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "checked_alias_layout");
    codegen
        .install_checked_declaration_names(&program)
        .expect("install checked declarations");
    assert_eq!(
        codegen.resolve_alias_type_name("Text"),
        "string",
        "Display/cast normalization uses the canonical terminal primitive"
    );
    assert!(codegen.map_type_is_statically_string(Type::Name("Text".into(), vec![])));
    assert_eq!(
        codegen.surface_ownership_class(&Type::Name("Text".into(), vec![])),
        crate::codegen::abi::ownership::OwnershipClass::StringBox,
        "alias ownership is canonical even before the compatibility TypeDef is registered"
    );
    assert_eq!(
        codegen.surface_ownership_class(&Type::Name(
            "List".into(),
            vec![Type::Name("Text".into(), vec![])],
        )),
        crate::codegen::abi::ownership::OwnershipClass::List(Box::new(
            crate::codegen::abi::ownership::OwnershipClass::StringBox
        )),
        "nested containers preserve the canonical StringBox alias obligation"
    );
    codegen
        .register_type_def(&text_declaration)
        .expect("install alias layout");

    let string_layout = codegen.type_llvm.get("Text").expect("Text LLVM layout");
    assert_eq!(
        *string_layout,
        BasicTypeEnum::StructType(context.struct_type(
            &[
                BasicTypeEnum::PointerType(context.ptr_type(inkwell::AddressSpace::default())),
                BasicTypeEnum::IntType(context.i64_type()),
            ],
            false,
        )),
        "the canonical alias chain ends at String despite the poisoned surface target"
    );
}

#[test]
fn checked_native_record_and_repr_c_layouts_ignore_surface_fields_and_attributes() {
    let source = r#"
type InnerText = string
type Payload { id: i32, text: InnerText }
#[repr(C)]
type CPair { value: i32, flag: bool }
func main() -> i32 { 0 }
"#;
    let file =
        crate::parser::Parser::new(crate::lexer::Lexer::new(source).tokenize().expect("lex"))
            .parse_file()
            .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let mut payload = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Type(declaration) if declaration.name == "Payload" => Some(declaration.clone()),
            _ => None,
        })
        .expect("Payload declaration");
    let TypeDefKind::Record(fields) = &mut payload.kind else {
        panic!("Payload must be a record");
    };
    fields[1].ty = Type::Name("i64".into(), vec![]);
    let mut c_pair = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Type(declaration) if declaration.name == "CPair" => Some(declaration.clone()),
            _ => None,
        })
        .expect("CPair declaration");
    let TypeDefKind::Record(fields) = &mut c_pair.kind else {
        panic!("CPair must be a record");
    };
    fields[1].ty = Type::Name("string".into(), vec![]);
    c_pair.attributes.clear();

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "checked_native_record_layouts");
    codegen
        .install_checked_declaration_names(&program)
        .expect("install checked declarations");
    codegen.register_type_def(&payload).expect("Payload layout");
    codegen.register_type_def(&c_pair).expect("CPair layout");

    let payload_fields = codegen
        .type_llvm
        .get("Payload")
        .expect("Payload layout")
        .into_struct_type()
        .get_field_types();
    assert_eq!(
        payload_fields,
        vec![
            BasicTypeEnum::IntType(context.i32_type()),
            BasicTypeEnum::StructType(context.struct_type(
                &[
                    BasicTypeEnum::PointerType(context.ptr_type(inkwell::AddressSpace::default())),
                    BasicTypeEnum::IntType(context.i64_type()),
                ],
                false,
            )),
        ],
        "record fields and the transparent string-alias ABI come from checked field IDs"
    );
    assert_eq!(
        codegen
            .type_llvm
            .get("CPair")
            .expect("CPair layout")
            .into_struct_type()
            .get_field_types(),
        vec![
            BasicTypeEnum::IntType(context.i32_type()),
            BasicTypeEnum::IntType(context.i8_type()),
        ],
        "repr(C) and bool's C width come from checked representation metadata"
    );
    assert!(codegen.repr_c_record_names.contains("CPair"));
}

#[test]
fn checked_native_enum_payload_tags_and_drop_ordinals_ignore_surface_payloads() {
    let source = r#"
type Outcome { Wrapped(string), Count(i32, i32), Only(i32), None }
func main() -> i32 { 0 }
"#;
    let file =
        crate::parser::Parser::new(crate::lexer::Lexer::new(source).tokenize().expect("lex"))
            .parse_file()
            .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let mut outcome = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Type(declaration) if declaration.name == "Outcome" => Some(declaration.clone()),
            _ => None,
        })
        .expect("Outcome declaration");
    let TypeDefKind::Enum(variants) = &mut outcome.kind else {
        panic!("Outcome must be an enum");
    };
    for variant in variants {
        match variant.name.as_str() {
            "Wrapped" => {
                variant.payload = Some(VariantPayload::Tuple(vec![Type::Name(
                    "i32".into(),
                    vec![],
                )]));
            }
            "Count" => variant.payload = None,
            "Only" => {
                variant.payload = Some(VariantPayload::Tuple(vec![
                    Type::Name("string".into(), vec![]),
                    Type::Name("string".into(), vec![]),
                ]));
            }
            "None" => {}
            _ => panic!("unexpected variant {}", variant.name),
        }
    }

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "checked_native_enum_layout");
    codegen
        .install_checked_declaration_names(&program)
        .expect("install checked declarations");
    codegen.register_type_def(&outcome).expect("Outcome layout");

    assert_eq!(
        codegen.enum_boxed_variant_ordinals("Outcome"),
        vec![0, 3],
        "sorted native tags and boxed-payload facts come from checked variant identities/types"
    );
    assert!(codegen.variant_payload_is_boxed("Wrapped"));
    assert!(codegen.variant_payload_is_boxed("Count"));
    assert!(!codegen.variant_payload_is_boxed("Only"));
}

#[test]
fn checked_native_flow_state_layout_ignores_surface_payload_type() {
    let source = r#"
flow Inventory {
    state Ready { id: i32, label: string }
}
func main() -> i32 { 0 }
"#;
    let file =
        crate::parser::Parser::new(crate::lexer::Lexer::new(source).tokenize().expect("lex"))
            .parse_file()
            .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let flow = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Flow(flow) if flow.name == "Inventory" => Some(flow),
            _ => None,
        })
        .expect("Inventory flow");
    let state = &flow.states[0];
    let mut surface_fields = state.payload.clone().unwrap_or_default();
    surface_fields[1].ty = Type::Name("i64".into(), vec![]);
    let declaration = TypeDef {
        meta: AstNodeMeta::inherited(state.meta.span, AstOrigin::RuntimeSystem("test-flow-state")),
        name: "flow::Inventory::Ready".into(),
        pub_: false,
        kind: TypeDefKind::Record(surface_fields),
        generics: vec![],
        derives: vec![],
        attributes: vec![],
    };

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "checked_native_flow_state");
    codegen
        .install_checked_declaration_names(&program)
        .expect("install checked declarations");
    codegen
        .register_type_def(&declaration)
        .expect("Flow state layout");
    assert_eq!(
        codegen
            .type_llvm
            .get("flow::Inventory::Ready")
            .expect("Flow state layout")
            .into_struct_type()
            .get_field_types(),
        vec![
            BasicTypeEnum::IntType(context.i32_type()),
            BasicTypeEnum::StructType(context.struct_type(
                &[
                    BasicTypeEnum::PointerType(context.ptr_type(inkwell::AddressSpace::default())),
                    BasicTypeEnum::IntType(context.i64_type()),
                ],
                false,
            )),
        ],
        "qualified Flow state ABI fields come from the canonical field catalog"
    );
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
fn compile_checked_routes_exact_s8_flow_through_canonical_mir() {
    let source = include_str!("../../tests/fixtures/mir_native_flow_transition.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    assert!(crate::core::mir::is_exact_s8_flow_transition(&program));

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "s10_s8_canonical_route");
    codegen
        .compile_checked(&program)
        .expect("exact S8 Flow must use the canonical MIR native consumer");
    assert!(codegen
        .module
        .get_function("__mimi_transition_Counter__inc__Zero")
        .is_some());
    assert!(codegen.resolved_failed_functions().is_empty());
}

#[test]
fn compile_checked_routes_exact_scalar_collection_through_canonical_mir() {
    let source = include_str!("../../tests/fixtures/mir_native_list_len.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    assert_eq!(
        crate::core::mir::classify_scalar_collection_admission(&program),
        crate::core::mir::ScalarCollectionAdmission::CompleteCoverage
    );
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&program)
        .expect("canonical MIR");
    assert!(crate::core::mir::contains_scalar_collection_candidate(
        &canonical
    ));
    crate::core::mir::validate_scalar_collection_island(&canonical)
        .expect("complete scalar collection island");

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "s12_scalar_collection_route");
    codegen
        .compile_checked(&program)
        .expect("exact scalar collection island must use canonical MIR native consumer");
    assert!(codegen
        .module
        .get_function("main")
        .is_some_and(|function| function.count_basic_blocks() > 0));
    assert!(codegen.resolved_failed_functions().is_empty());
}

#[test]
fn compile_checked_rejects_result_valued_map_any_outside_retired_v1_without_legacy_access() {
    let profiles = [
        (
            "i32",
            r#"
        func main() -> i32 {
            match map_new() {
                Ok(root) => {
                    match map_set(root, "answer", 42) {
                        Ok(updated) => {
                            let size = map_size(updated)
                            println(size)
                            drop(updated)
                            0
                        },
                        Err(status) => {
                            drop(root)
                            status
                        },
                    }
                },
                Err(status) => { status },
            }
        }
            "#,
        ),
        (
            "string",
            r#"
        func string_map_size() -> i32 {
            match map_new() {
                Ok(root) => {
                    match map_set(root, "answer", "a\0b雪") {
                        Ok(updated) => {
                            let size = map_size(updated)
                            println(size)
                            drop(updated)
                            size
                        },
                        Err(status) => {
                            drop(root)
                            status
                        },
                    }
                },
                Err(status) => { status },
            }
        }
        func main() -> i32 {
            let size = string_map_size()
            0
        }
            "#,
        ),
        (
            "remove",
            r#"
        func main() -> i32 {
            match map_new() {
                Ok(root) => {
                    match map_set(root, "remove", "a\0b雪") {
                        Ok(inserted) => {
                            match map_set(inserted, "keep", "stay") {
                                Ok(with_keep) => {
                                    let removed = map_remove(with_keep, "remove")
                                    let size = map_size(removed)
                                    drop(removed)
                                    size
                                },
                                Err(status) => {
                                    drop(inserted)
                                    status
                                },
                            }
                        },
                        Err(status) => {
                            drop(root)
                            status
                        },
                    }
                },
                Err(status) => { status },
            }
        }
            "#,
        ),
        (
            "contains",
            r#"
        func main() -> i32 {
            match map_new() {
                Ok(root) => {
                    match map_set(root, "answer", 42) {
                        Ok(inserted) => {
                            let present = has_key(inserted, "answer")
                            let size = map_size(inserted)
                            println(present)
                            drop(inserted)
                            size
                        },
                        Err(status) => { drop(root) status },
                    }
                },
                Err(status) => { status },
            }
        }
    "#,
        ),
    ];
    for (profile, source) in profiles {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let admission = crate::core::mir::classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.map_root,
            crate::core::mir::MapRootAdmission::OutsideProfile,
            "{profile} Result-valued construction must not enter the retired bare-Record MapRoot profile: {admission:#?}"
        );
        assert_eq!(
            admission.map_any_owner,
            crate::core::mir::MapAnyOwnerAdmission::OutsideProfile,
            "{profile} Result-unwrapping does not mint a retired bare-Record MapAnyOwner v1 receipt: {admission:#?}"
        );
        assert_eq!(
            admission.ordinary_map_owned_any_v2,
            crate::core::mir::OrdinaryMapOwnedAnyV2Admission::IncompleteCoverage,
            "{profile} must retain the independent incomplete V2 boundary instead of borrowing V1: {admission:#?}"
        );

        crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
        let context = Context::create();
        let mut codegen = CodeGenerator::new(
            &context,
            &format!("map_any_owner_v1_{profile}_result_fail_closed"),
        );
        let errors = codegen
            .compile_checked(&program)
            .expect_err("Result-valued MapAny outside the closed route must fail at MIR coverage");
        assert!(
            errors.iter().any(|diagnostic| {
                diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            }),
            "{profile} must report the canonical MIR coverage gate: {errors:?}"
        );
        assert!(
            codegen.module.get_function("main").is_none(),
            "{profile} must not leave a partially emitted compatibility function"
        );
        assert!(
            crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty(),
            "{profile} rejected MapAny route must not access the legacy codegen body"
        );
    }
}

#[test]
fn compile_checked_rejects_dynamic_map_key_without_legacy_owner() {
    let source = r#"
        func main() -> i32 {
            let root = map_new()
            let key = "answer"
            let updated = map_set(root, key, 42)
            drop(updated)
            0
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let admission = crate::core::mir::classify_canonical_mir_route_admission(&program);
    assert_eq!(
        admission.map_root,
        crate::core::mir::MapRootAdmission::OutsideProfile
    );
    assert_eq!(
        admission.map_any_owner,
        crate::core::mir::MapAnyOwnerAdmission::OutsideProfile
    );
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "map_dynamic_key_rejected");
    let errors = codegen
        .compile_checked(&program)
        .expect_err("dynamic Map keys outside the closed profile must fail closed");
    assert!(
        errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                || diagnostic
                    .message
                    .contains(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        }),
        "unsupported Map operation must fail at the canonical route gate: {errors:?}"
    );
}

#[test]
fn compile_checked_routes_ordinary_map_values_without_legacy_owner() {
    let source = r#"
        func main() -> i32 {
            let root = map_new()
            let values = values(root)
            drop(root)
            drop(values)
            0
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let admission = crate::core::mir::classify_canonical_mir_route_admission(&program);
    assert_eq!(
        admission.ordinary_map_owned_get_i32_string_v1,
        crate::core::mir::OrdinaryMapOwnedGetI32StringV1Admission::CompleteCoverage
    );
    assert_eq!(
        admission.ordinary_map_owned_any_v2,
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::OutsideProfile,
        "the incomplete V2 empty-root candidate must yield only to the fully receipted V1 Values route"
    );
    assert_eq!(
        admission.ordinary_map_owner_lifecycle_i32_string_v1,
        crate::core::mir::OrdinaryMapOwnerLifecycleI32StringV1Admission::OutsideProfile
    );
    assert!(!admission.map_any_owner.is_candidate());

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "ordinary_map_values_direct_route");
    codegen
        .compile_checked(&program)
        .expect("complete ordinary Map values must route through canonical MIR");
    codegen
        .module
        .verify()
        .expect("ordinary Map values native module must verify");
    assert!(codegen
        .module
        .get_function("mimi_mir_ordinary_map_owned_get_i32_string_v1_values")
        .is_some());
    let native_ir = codegen.module.print_to_string().to_string();
    assert!(
        native_ir.contains("@mimi_mir_ordinary_map_owned_get_i32_string_v1_values"),
        "ordinary Map values native route must call its owned-result runtime: {native_ir}"
    );

    // Keep a negative route fixture on a genuinely unsupported Map operation.
    // Typed map_get_i32 and values are both canonical owned-result operations;
    // `keys` remains outside the ordinary Map/Any closure and must therefore
    // retain an explicit fail-closed route.
    let get_source = r#"
        func main() -> i32 {
            let root = map_new()
            let pair = map_get_i32(root, "answer")
            let unsupported = keys(root)
            drop(pair)
            drop(unsupported)
            drop(root)
            0
        }
    "#;
    let get_tokens = crate::lexer::Lexer::new(get_source)
        .tokenize()
        .expect("lex typed GetI32 route fixture");
    let get_file = crate::parser::Parser::new(get_tokens)
        .parse_file()
        .expect("parse typed GetI32 route fixture");
    let get_program =
        crate::core::check_program(&get_file).expect("check typed GetI32 route fixture");
    let get_admission = crate::core::mir::classify_canonical_mir_route_admission(&get_program);
    assert!(get_admission
        .ordinary_map_owned_get_i32_string_v1
        .has_profile_signal());
    assert!(!get_admission.ordinary_map_owned_get_i32_string_v1_complete());
    assert_eq!(
        get_admission.ordinary_map_owner_lifecycle_i32_string_v1,
        crate::core::mir::OrdinaryMapOwnerLifecycleI32StringV1Admission::OutsideProfile
    );

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "ordinary_map_incomplete_compatibility");
    let errors = codegen
        .compile_checked(&get_program)
        .expect_err("incomplete ordinary Map coverage must fail closed");
    assert!(
        errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                || diagnostic
                    .message
                    .contains(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
        }),
        "unsupported Map operation must fail at the canonical route gate: {errors:?}"
    );
}

#[test]
fn ordinary_map_owner_lifecycle_observer_v1_native_uses_borrowed_json_and_owned_string_drop() {
    use crate::core::mir::CanonicalMirRouteProfile as Profile;

    let source = r#"
        func main() -> i32 {
            let root = map_new()
            let snapshot = to_json(root)
            println(root)
            drop(root)
            println(snapshot)
            0
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check observer fixture");
    let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked);
    assert!(Profile::OrdinaryMapOwnerLifecycleObserverV1.is_admitted(admission));
    assert!(
        !admission.ordinary_map_owned_any_v2.has_profile_signal(),
        "the isolated lifecycle observer profile must not admit broad V2 readiness"
    );

    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("materialize the selected lifecycle-v1 observer route");
    assert!(Profile::OrdinaryMapOwnerLifecycleObserverV1.is_materialized(&route));
    let receipt = route
        .ordinary_map_owner_lifecycle_observer_v1_route_receipt
        .as_ref()
        .expect("route materialization supplies its exact observer receipt");
    assert_eq!(
        receipt.profile,
        Profile::OrdinaryMapOwnerLifecycleObserverV1.as_str()
    );
    let observer_projection =
        crate::core::mir::validate_ordinary_map_owner_lifecycle_observer_v1_mir_route(
            &route.program,
        )
        .expect("Checker observer receipts bind one-to-one to MIR instructions");
    let observer_instruction_count = route
        .program
        .functions()
        .values()
        .flat_map(|function| function.blocks.values())
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                &instruction.kind,
                crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleObserverToJson {
                    ..
                } | crate::core::mir::MirInstructionKind::OrdinaryMapOwnerLifecycleObserverPrintln {
                    ..
                }
            )
        })
        .count();
    assert_eq!(observer_instruction_count, 2);
    assert_eq!(observer_projection.len(), observer_instruction_count);

    let context = Context::create();
    let mut no_receipt = CodeGenerator::new(&context, "ordinary_map_observer_without_receipt");
    let no_receipt_error = no_receipt
        .compile_mir_native(&route.program)
        .expect_err("observer MIR cannot enter native emission without the selected route receipt");
    assert!(no_receipt_error.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    }));
    assert!(no_receipt.module.get_function("main").is_none());
    assert!(no_receipt
        .module
        .get_function("mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_to_json")
        .is_none());

    let mismatched_receipt = route
        .program
        .route_receipt(crate::core::mir::MIR_BUILD_CANONICAL_ROUTE_PROFILE);
    let mut wrong_receipt = CodeGenerator::new(&context, "ordinary_map_observer_wrong_receipt");
    let wrong_receipt_error = wrong_receipt
        .compile_mir_native_with_route_receipt(&route.program, &mismatched_receipt)
        .expect_err("a general route receipt cannot authorize the observer instructions");
    assert!(wrong_receipt_error.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    }));
    assert!(wrong_receipt.module.get_function("main").is_none());

    let mut codegen = CodeGenerator::new(&context, "ordinary_map_observer_native");
    codegen
        .compile_mir_native_with_route_receipt(&route.program, receipt)
        .expect("native consumer accepts the selected observer receipt");
    codegen
        .module
        .verify()
        .expect("observer native LLVM module verifies");
    let native_ir = codegen.module.print_to_string().to_string();
    let call_count = |symbol: &str| {
        native_ir
            .lines()
            .filter(|line| line.contains("call ") && line.contains(&format!("@{symbol}(")))
            .count()
    };
    assert_eq!(
        call_count("mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_to_json"),
        2,
        "ToJson yields an owned String, while println performs an independent borrowed snapshot"
    );
    assert_eq!(call_count("strlen"), 2);
    assert_eq!(
        call_count("mimi_mir_ordinary_map_domain_own_string"),
        2,
        "ToJson owns its returned snapshot and println owns its temporary output buffer"
    );
    assert_eq!(
        call_count("mimi_print_bytes"),
        4,
        "each observer prints exact payload bytes and one newline"
    );
    assert_eq!(
        call_count("mimi_string_free"),
        3,
        "the Map-print scratch buffer, println's owned snapshot temporary, and scope-owned ToJson String are each freed once"
    );
    assert_eq!(
        call_count("mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_clone"),
        0,
        "observing a Map must not clone or consume its owner handle"
    );

    if crate::tests::can_link() {
        let native = crate::tests::link_and_observe_canonical_mir(&codegen)
            .expect("run native borrowed Map observer fixture");
        assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
        assert_eq!(native.stdout, "{}\n{}\n");
        assert_eq!(native.stderr, "");
    }
}

#[test]
fn ordinary_map_owner_lifecycle_native_size_uses_distinct_overflow_status() {
    let source = r#"
        func main() -> i32 {
            let root = map_new()
            let key = "capacity-probe"
            let inserted = map_set(root, key, 1)
            let size = map_size(inserted)
            drop(root)
            drop(inserted)
            size
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check Map size fixture");
    let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked);
    assert_eq!(
        admission.ordinary_map_owner_lifecycle_i32_string_v1,
        crate::core::mir::OrdinaryMapOwnerLifecycleI32StringV1Admission::CompleteCoverage,
        "a computed String key selects the distinct ordinary lifecycle size ABI"
    );
    assert_eq!(
        admission.ordinary_map_owned_any_v2,
        crate::core::mir::OrdinaryMapOwnedAnyV2Admission::OutsideProfile
    );

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "ordinary_map_size_status_native");
    codegen
        .compile_checked(&checked)
        .expect("ordinary Map lifecycle size uses the canonical native route");
    codegen
        .module
        .verify()
        .expect("ordinary Map lifecycle size native module verifies");
    let native_ir = codegen.module.print_to_string().to_string();

    assert!(native_ir.contains("@mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_size_status"));
    assert!(native_ir.contains("mir_ordinary_map_size_status_success"));
    assert!(native_ir.contains("mir_ordinary_map_size_status_overflow"));
    assert!(native_ir.contains("[E0802] ordinary Map owner lifecycle v1 size overflows i32"));
    assert!(native_ir.contains("[E0800] ordinary Map lifecycle Size failed"));
    assert!(
        !native_ir.contains("call i32 @mimi_mir_ordinary_map_owner_lifecycle_i32_string_v1_size(")
    );
}

#[test]
fn compile_checked_routes_generic_ignored_bool_binding_projection_without_legacy_access() {
    let source = r#"
        func choose<T>(left: T, right: T) -> T { let ignored = true; right }
        func main() -> i64 { choose(2147483690, 2147483691) }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    assert_eq!(
        crate::core::mir::classify_canonical_mir_route_admission(&program)
            .scalar_generic_identity_i64,
        crate::core::mir::ScalarGenericIdentityAdmission::CompleteCoverage,
        "the checker-owned bool literal binding preserves the selected-formal projection"
    );

    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let mir = crate::core::mir::reference::MirProgram::from_checked_program(&program)
        .expect("the exact generic bool-local body must lower to canonical MIR");
    assert_eq!(mir.instances().len(), 1, "one specialized generic instance");
    let instance = mir
        .instances()
        .values()
        .next()
        .expect("one concrete generic instance");
    assert!(matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarArgumentProjection {
            parameter_index: 1,
            parameter_count: 2,
        }
    ));
    let target = mir
        .functions()
        .get(&instance.function)
        .expect("materialized generic body");
    let concrete = instance.arguments.first().expect("concrete T substitution");
    crate::core::mir::validate_generic_argument_projection_shape(target, concrete, 1, 2)
        .expect("the MIR receipt admits only the unused bool binding and selected formal");
    assert_eq!(
        crate::core::mir::reference::MirReferenceInterpreter::new(&mir)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic projection"),
        crate::core::mir::reference::MirRuntimeValue::Int(2147483691)
    );
    let bytecode = crate::interp::bytecode::compile_mir_program(&mir)
        .expect("generic bool-local body compiles to AST-free bytecode");
    assert!(bytecode.ast.is_none());
    assert!(matches!(
        crate::interp::bytecode::BytecodeVM::new(bytecode)
            .run_value_for_component()
            .expect("bytecode generic bool-local projection"),
        crate::interp::Value::Int(2147483691)
    ));

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "canonical_generic_ignored_bool_binding");
    codegen
        .compile_checked(&program)
        .expect("default native route must emit the admitted generic projection");
    codegen
        .module
        .verify()
        .expect("native generic bool-local module verifies");
    assert!(codegen.module.get_function("main").is_some());
    let native = codegen
        .module
        .create_jit_execution_engine(inkwell::OptimizationLevel::None)
        .expect("native generic bool-local JIT engine");
    let finalizer_guard = codegen
        .module
        .get_function("mimi_mir_assert_no_live_managed_any_v2_lists")
        .expect("canonical main installs the managed-Any end-of-run guard");
    native.add_global_mapping(
        &finalizer_guard,
        crate::runtime::mimi_mir_assert_no_live_managed_any_v2_lists as *const () as usize,
    );
    type Main = unsafe extern "C" fn() -> i64;
    // SAFETY: `compile_checked` emitted `main` with the checked `i64 ()` ABI,
    // and its only runtime guard is mapped into this direct JIT engine above.
    let native_main = unsafe {
        native
            .get_function::<Main>("main")
            .expect("native generic main exists")
    };
    crate::codegen::assert_execution_engine_linked(&native, "calling native generic main");
    // SAFETY: `native_main` has the checked no-argument i64 ABI.
    let native_value = unsafe { native_main.call() };
    assert_eq!(
        native_value, 2147483691,
        "native MIR generic projection result"
    );
    assert!(
        crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty(),
        "the canonical generic projection must not invoke the legacy emitter"
    );
}

#[test]
fn compile_checked_routes_generic_ignored_integer_binding_projection_without_legacy_access() {
    let source = r#"
        func choose<T>(left: T, right: T) -> T { let ignored = 1; right }
        func main() -> i64 { choose(2147483690, 2147483691) }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    assert_eq!(
        crate::core::mir::classify_canonical_mir_route_admission(&program)
            .scalar_generic_identity_i64,
        crate::core::mir::ScalarGenericIdentityAdmission::CompleteCoverage,
        "the checker-owned unused integer literal preserves the selected-formal projection"
    );

    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let mir = crate::core::mir::reference::MirProgram::from_checked_program(&program)
        .expect("the exact generic integer-local body must lower to canonical MIR");
    assert_eq!(mir.instances().len(), 1, "one specialized generic instance");
    let instance = mir
        .instances()
        .values()
        .next()
        .expect("one concrete generic instance");
    assert!(matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarArgumentProjection {
            parameter_index: 1,
            parameter_count: 2,
        }
    ));
    let target = mir
        .functions()
        .get(&instance.function)
        .expect("materialized generic body");
    let concrete = instance.arguments.first().expect("concrete T substitution");
    crate::core::mir::validate_generic_argument_projection_shape(target, concrete, 1, 2)
        .expect("the MIR receipt admits only the unused integer literal and selected formal");
    assert_eq!(
        crate::core::mir::reference::MirReferenceInterpreter::new(&mir)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic projection"),
        crate::core::mir::reference::MirRuntimeValue::Int(2147483691)
    );
    let bytecode = crate::interp::bytecode::compile_mir_program(&mir)
        .expect("generic integer-local body compiles to AST-free bytecode");
    assert!(bytecode.ast.is_none());
    assert!(matches!(
        crate::interp::bytecode::BytecodeVM::new(bytecode)
            .run_value_for_component()
            .expect("bytecode generic integer-local projection"),
        crate::interp::Value::Int(2147483691)
    ));

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "canonical_generic_ignored_integer_binding");
    codegen
        .compile_checked(&program)
        .expect("default native route must emit the admitted generic projection");
    codegen
        .module
        .verify()
        .expect("native generic integer-local module verifies");
    assert!(codegen.module.get_function("main").is_some());
    let native = codegen
        .module
        .create_jit_execution_engine(inkwell::OptimizationLevel::None)
        .expect("native generic integer-local JIT engine");
    let finalizer_guard = codegen
        .module
        .get_function("mimi_mir_assert_no_live_managed_any_v2_lists")
        .expect("canonical main installs the managed-Any end-of-run guard");
    native.add_global_mapping(
        &finalizer_guard,
        crate::runtime::mimi_mir_assert_no_live_managed_any_v2_lists as *const () as usize,
    );
    type Main = unsafe extern "C" fn() -> i64;
    // SAFETY: `compile_checked` emitted `main` with the checked `i64 ()` ABI,
    // and its only runtime guard is mapped into this direct JIT engine above.
    let native_main = unsafe {
        native
            .get_function::<Main>("main")
            .expect("native generic main exists")
    };
    crate::codegen::assert_execution_engine_linked(&native, "calling native generic main");
    // SAFETY: `native_main` has the checked no-argument i64 ABI.
    let native_value = unsafe { native_main.call() };
    assert_eq!(
        native_value, 2147483691,
        "native MIR generic projection result"
    );
    assert!(
        crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty(),
        "the canonical generic projection must not invoke the legacy emitter"
    );
}

#[test]
fn compile_checked_rejects_used_generic_integer_side_binding_without_legacy_owner() {
    let source = r#"
        func choose<T>(left: T, right: T) -> T {
            let ignored = 1;
            let observed = ignored;
            right
        }
        func main() -> i64 { choose(2147483690, 2147483691) }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    assert_eq!(
        crate::core::mir::classify_canonical_mir_route_admission(&program)
            .scalar_generic_identity_i64,
        crate::core::mir::ScalarGenericIdentityAdmission::OutsideProfile,
        "a used integer local falls outside the single unused literal shape"
    );

    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "used_generic_integer_side_binding");
    let errors = codegen
        .compile_checked(&program)
        .expect_err("a used integer side binding remains outside the narrow scalar projection");
    assert!(errors.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            && diagnostic.message.contains("NITS")
    }));
    assert!(crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty());
}

#[test]
fn compile_checked_routes_generic_local_alias_projection_without_legacy_access() {
    let source = r#"
        func choose<T>(left: T, right: T) -> T { let chosen = right; chosen }
        func main() -> i32 { choose(42, 43) }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    assert_eq!(
        crate::core::mir::classify_canonical_mir_route_admission(&program)
            .scalar_generic_identity_i32,
        crate::core::mir::ScalarGenericIdentityAdmission::CompleteCoverage,
        "the Checker exact local alias shape belongs to the existing scalar projection slice"
    );

    let mir = crate::core::mir::reference::MirProgram::from_checked_program(&program)
        .expect("the exact generic local alias must lower to canonical MIR");
    assert_eq!(mir.instances().len(), 1, "one specialized generic instance");
    let instance = mir
        .instances()
        .values()
        .next()
        .expect("one concrete generic instance");
    assert!(matches!(
        &instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarArgumentProjection {
            parameter_index: 1,
            parameter_count: 2,
        }
    ));
    let target = mir
        .functions()
        .get(&instance.function)
        .expect("generic local alias instance body");
    let concrete = instance.arguments.first().expect("concrete T substitution");
    assert!(
        crate::core::mir::validate_generic_argument_projection_shape(target, concrete, 1, 2)
            .is_ok()
    );

    // The instance receipt is insufficient by itself: execution consumers
    // revalidate that the body is the exact Clone/Move/Clone alias chain.
    let mut forged_target = target.clone();
    let forged_source = forged_target.parameters[0].clone();
    let forged_entry = forged_target.entry.clone();
    let block = forged_target
        .blocks
        .get_mut(&forged_entry)
        .expect("generic instance entry block");
    let binding_result = match &block.instructions[1].kind {
        crate::core::mir::MirInstructionKind::Move { result, .. } => result.clone(),
        _ => panic!("the local alias lowering must contain its binding Move"),
    };
    block.instructions[1].kind = crate::core::mir::MirInstructionKind::Move {
        result: binding_result,
        source: forged_source,
    };
    let forged_error = crate::core::mir::validate_generic_argument_projection_shape(
        &forged_target,
        concrete,
        1,
        2,
    )
    .expect_err("a forged local binding source must not preserve the projection receipt");
    assert!(forged_error.contains("local alias"), "{forged_error}");

    assert_eq!(
        crate::core::mir::reference::MirReferenceInterpreter::new(&mir)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic local alias"),
        crate::core::mir::reference::MirRuntimeValue::Int(43)
    );
    let bytecode = crate::interp::bytecode::compile_mir_program(&mir)
        .expect("generic local alias compiles to AST-free bytecode");
    assert!(bytecode.ast.is_none());
    assert!(matches!(
        crate::interp::bytecode::BytecodeVM::new(bytecode)
            .run_value_for_component()
            .expect("bytecode generic local alias"),
        crate::interp::Value::Int(43)
    ));

    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "canonical_generic_local_alias");
    codegen
        .compile_checked(&program)
        .expect("the default route must emit the admitted local alias through native MIR");
    codegen
        .module
        .verify()
        .expect("native alias module verifies");
    assert!(
        codegen.module.get_function("main").is_some(),
        "canonical native route emits main"
    );
    assert!(
        crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty(),
        "canonical generic local alias must not invoke the legacy emitter"
    );
}

#[test]
fn compile_checked_routes_scalar_generic_first_argument_projection_without_legacy_access() {
    for (module_name, source) in [
        (
            "canonical_generic_first_projection_i32",
            "func choose<T>(left: T, right: T) -> T { left }\nfunc main() -> i32 { choose(41, 43) }",
        ),
        (
            "canonical_generic_first_projection_i64",
            "func choose<T>(left: T, right: T) -> T { left }\nfunc main() -> i64 { choose(2147483690, 2147483691) }",
        ),
        (
            "canonical_generic_second_projection_i32",
            "func choose<T>(left: T, right: T) -> T { right }\nfunc main() -> i32 { choose(41, 43) }",
        ),
        (
            "canonical_generic_third_projection_i64",
            "func choose<T>(a: T, b: T, c: T) -> T { c }\nfunc main() -> i64 { choose(2147483690, 4294967338, 6442450986) }",
        ),
    ] {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");

        crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
        let context = Context::create();
        let mut codegen = CodeGenerator::new(&context, module_name);
        codegen
            .compile_checked(&program)
            .expect("exact argument projection must use canonical MIR");
        codegen.module.verify().expect("LLVM module verifies");
        assert!(
            codegen.module.get_function("main").is_some(),
            "canonical argument projection route must emit main"
        );
        assert!(
            crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty(),
            "the admitted generic projection must not invoke the legacy emitter"
        );
    }
}

#[test]
fn compile_checked_preserves_baseline_generic_choose_result_without_legacy_access() {
    let source =
        "func choose<T>(left: T, right: T) -> T { left }\nfunc main() -> i32 { choose(42, 43) }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    assert_eq!(
        crate::core::mir::classify_canonical_mir_route_admission(&program)
            .scalar_generic_identity_i32,
        crate::core::mir::ScalarGenericIdentityAdmission::CompleteCoverage,
        "the baseline two-argument generic projection must select its exact MIR profile"
    );

    let mir = crate::core::mir::reference::MirProgram::from_checked_program(&program)
        .expect("baseline generic projection materializes canonical MIR");
    assert_eq!(
        crate::core::mir::reference::MirReferenceInterpreter::new(&mir)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic choose"),
        crate::core::mir::reference::MirRuntimeValue::Int(42),
        "the MIR route must return the selected first actual"
    );

    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "baseline_generic_choose_result");
    codegen
        .compile_checked(&program)
        .expect("direct checked-codegen API must emit the canonical generic projection");
    codegen
        .module
        .verify()
        .expect("direct checked-codegen module verifies");
    let native = codegen
        .module
        .create_jit_execution_engine(inkwell::OptimizationLevel::None)
        .expect("create direct checked-codegen JIT engine");
    let finalizer_guard = codegen
        .module
        .get_function("mimi_mir_assert_no_live_managed_any_v2_lists")
        .expect("canonical main installs the managed-Any end-of-run guard");
    native.add_global_mapping(
        &finalizer_guard,
        crate::runtime::mimi_mir_assert_no_live_managed_any_v2_lists as *const () as usize,
    );
    type Main = unsafe extern "C" fn() -> i32;
    // SAFETY: compile_checked emitted `main` with the checked no-argument i32 ABI.
    let native_main = unsafe {
        native
            .get_function::<Main>("main")
            .expect("native generic main exists")
    };
    crate::codegen::assert_execution_engine_linked(&native, "calling baseline generic main");
    // SAFETY: native_main has the checked no-argument i32 ABI.
    let native_value = unsafe { native_main.call() };
    assert_eq!(native_value, 42, "native generic choose result");
    assert!(
        crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty(),
        "baseline-positive generic choose must not invoke a legacy AST emitter"
    );
}

#[test]
fn compile_checked_rejects_unsupported_generic_projection_without_legacy_reentry() {
    let source = "func choose<T>(left: T, right: T) -> T { let selected = right; let alias = selected; alias }\nfunc main() -> i32 { choose(42, 43) }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program =
        crate::core::check_program(&file).expect("unsupported shape remains checker-valid");

    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "unsupported_generic_projection_no_legacy");
    let error = codegen
        .compile_checked(&program)
        .expect_err("unsupported multi-binding generic projection must fail closed");
    assert_eq!(error[0].code.as_deref(), Some("MIR-COVERAGE-001"));
    assert!(error[0].message.contains("NITS"));
    assert!(
        crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty(),
        "an unsupported generic body must not re-enter the retired AST emitter"
    );
}

#[test]
fn compile_checked_does_not_reenter_legacy_for_resolved_plain_scalar_body() {
    let source = r#"
        func add_one(value: i32) -> i32 { value + 1 }
        func choose<T>(left: T, right: T) -> T { left }
        func main() -> i32 { add_one(choose(40, 41)) }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "resolved_plain_scalar_no_legacy_reentry");
    let error = codegen
        .compile_checked(&program)
        .expect_err("reachable generic compatibility must fail closed");
    assert_eq!(error[0].code.as_deref(), Some("MIR-COVERAGE-001"));
    assert!(error[0].message.contains("NITS"));
    assert!(
        crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty(),
        "a rejected generic route must not invoke the legacy emitter"
    );
}

#[test]
fn native_checked_boundary_preserves_positive_rejection_and_bytecode_compatibility() {
    // Positive checked-native case: the old native fifth pass is not needed
    // for ordinary scalar bodies, and the canonical emitter still produces a
    // complete function body through the public CodeGenerator API.
    let positive_source = "func main() -> i32 { 42 }";
    let positive_file = crate::parser::Parser::new(
        crate::lexer::Lexer::new(positive_source)
            .tokenize()
            .expect("positive source lexes"),
    )
    .parse_file()
    .expect("positive source parses");
    let positive = crate::core::check_program(&positive_file).expect("positive source checks");

    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let context = Context::create();
    let mut native = CodeGenerator::new(&context, "codegen_legacy_remainder_positive");
    native
        .compile_checked(&positive)
        .expect("supported checked scalar still compiles natively");
    native
        .module
        .verify()
        .expect("positive LLVM module verifies");
    let main = native
        .module
        .get_function("main")
        .expect("native main exists");
    assert_ne!(main.count_basic_blocks(), 0, "native main has a body");
    assert!(
        native
            .module
            .print_to_string()
            .to_string()
            .contains("ret i32 42"),
        "the canonical native body preserves the scalar result"
    );
    assert!(
        crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty(),
        "supported checked-native compilation must not enter the native AST fifth pass"
    );

    // Negative native control: this managed generic Option shape is outside
    // the current checked-native profile and must report its existing MIR
    // boundary without invoking the native AST adapter. The same checked
    // source remains executable through the separately reachable default
    // Bytecode AST compatibility compiler; keeping that path alive is part of
    // the open CodegenLegacyRemainder owner.
    let compatibility_source =
        include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_rejected.mimi");
    let compatibility_file = crate::parser::Parser::new(
        crate::lexer::Lexer::new(compatibility_source)
            .tokenize()
            .expect("compatibility source lexes"),
    )
    .parse_file()
    .expect("compatibility source parses");
    let compatibility = crate::core::check_program(&compatibility_file)
        .expect("compatibility source remains checker-legal");

    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let context = Context::create();
    let mut native = CodeGenerator::new(&context, "codegen_legacy_remainder_negative");
    let errors = native
        .compile_checked(&compatibility)
        .expect_err("unsupported native generic projection remains outside the closed profile");
    assert!(
        errors.iter().any(|diagnostic| matches!(
            diagnostic.code.as_deref(),
            Some("MIR-COVERAGE-001" | "MIR-LOWERING-001")
        )),
        "native rejection keeps the explicit coverage boundary: {errors:?}"
    );
    assert!(
        crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty(),
        "an unsupported checked-native profile must not retry through the native AST fifth pass"
    );

    let mut bytecode = crate::interp::bytecode::BytecodeCompiler::new();
    bytecode.install_checked_program(&compatibility);
    let bytecode_program = bytecode
        .compile_file(&compatibility_file)
        .expect("default Bytecode AST compatibility still compiles this legal source");
    assert!(
        bytecode_program.ast.is_some(),
        "the default compatibility artifact must retain its AST-backed disposition"
    );
    let mut vm = crate::interp::bytecode::BytecodeVM::new(bytecode_program);
    assert!(
        matches!(
            vm.run_value_for_component()
                .expect("default Bytecode AST compatibility still executes the source"),
            crate::interp::Value::Int(41)
        ),
        "the compatibility route preserves the source result"
    );
}

#[test]
fn codegen_folds_progressive_comptime_from_typed_ir() {
    let source = r#"
        comptime func forty_two() -> i32 { 42 }
        func main() -> i32 { comptime { forty_two() } }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    assert!(file.implicit_single);
    let program = crate::core::check_program(&file).expect("check");

    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "typed_progressive_comptime");
    codegen
        .compile_checked(&program)
        .expect("production typed codegen should fold the comptime expression");
    assert!(
        codegen
            .module
            .get_function("main")
            .is_some_and(|function| function.count_basic_blocks() > 0),
        "progressive main should still be emitted"
    );
    assert!(
        codegen
            .module
            .print_to_string()
            .to_string()
            .contains("ret i32 42"),
        "typed comptime folding should preserve the result"
    );
    assert!(
        crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty(),
        "production typed comptime folding must not invoke legacy codegen"
    );
}

#[test]
fn compile_checked_routes_scalar_generic_identity_without_legacy_access() {
    let source = r#"
        func identity<T>(value: T) -> T { value }
        func main() -> i32 { identity(42) }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "canonical_generic_identity");
    codegen
        .compile_checked(&program)
        .expect("bounded scalar generic identity must use the canonical MIR route");
    assert!(
        codegen.module.get_function("main").is_some(),
        "canonical generic identity route must emit main"
    );
}

#[test]
fn compile_checked_routes_i64_scalar_generic_identity_without_legacy_access() {
    let source = r#"
        func identity<T>(value: T) -> T { value }
        func main() -> i64 { identity(2147483690) }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "canonical_generic_identity_i64");
    codegen
        .compile_checked(&program)
        .expect("bounded i64 scalar generic identity must use the canonical MIR route");
    assert!(
        codegen.module.get_function("main").is_some(),
        "canonical i64 generic identity route must emit main"
    );
}

#[test]
fn compile_checked_routes_record_list_chain_through_canonical_mir() {
    let source = include_str!("../../tests/fixtures/mir_m1_record_list_chain.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&program)
        .expect("record/list chain canonical MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("record/list chain verifier capability");
    crate::codegen::mir::validate_mir_native(&canonical)
        .expect("record/list chain native capability");

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "m1_record_list_chain_direct");
    codegen
        .compile_checked(&program)
        .expect("direct native API must use the canonical record/list MIR route");
    assert!(codegen.module.get_function("main").is_some());
    assert!(codegen.resolved_failed_functions().is_empty());
}

#[test]
fn direct_native_entry_routes_recoverable_retry_profile_through_canonical_mir() {
    let source = include_str!("../../tests/fixtures/mir_m3_flow_retry.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "m3_canonical_flow_entry");
    codegen
        .compile_checked(&program)
        .expect("direct native entry must select the canonical retry profile");
    assert!(codegen.module.get_function("main").is_some());
}

#[test]
fn direct_native_entry_routes_recoverable_string_state_through_canonical_mir() {
    let source = include_str!("../../tests/fixtures/mir_m3_flow_retry_string_state.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "m3_canonical_string_flow_entry");
    codegen
        .compile_checked(&program)
        .expect("direct native entry must select the canonical String retry profile");
    assert!(codegen.module.get_function("main").is_some());
}

#[test]
fn direct_native_entry_routes_multifield_flow_source_receipt_through_canonical_mir() {
    let source =
        include_str!("../../tests/fixtures/mir_m3_flow_multifield_string_source_receipt.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "m3_canonical_multifield_flow_entry");
    codegen
        .compile_checked(&program)
        .expect("direct native entry must select the canonical multifield Flow profile");
    assert!(codegen.module.get_function("main").is_some());
}

#[test]
fn direct_native_entry_routes_f64_flow_source_receipt_through_canonical_mir() {
    let source = include_str!("../../tests/fixtures/mir_r6_flow_f64_cross_state_receipt.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    assert!(crate::core::mir::classify_canonical_mir_route_admission(&program).flow_failure_retry);
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "r6_canonical_f64_flow_entry");
    codegen
        .compile_checked(&program)
        .expect("direct native entry must select canonical f64 Flow source receipt");
    assert!(codegen.module.get_function("main").is_some());
}

#[test]
fn direct_native_entry_routes_nested_scalar_ffi_through_mir_without_legacy_access() {
    let source = r#"
        extern "C" { func llabs(value: i64) -> i64; }
        func main() -> i64 {
            func local_abs(value: i64) -> i64 { llabs(value) }
            local_abs(-17 as i64)
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    assert!(
        crate::core::mir::classify_canonical_mir_route_admission(&program).scalar_ffi,
        "nested extern call is inside checker-owned scalar FFI admission"
    );

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "nested_scalar_ffi_receipted");
    codegen
        .compile_checked(&program)
        .expect("direct native entry must use the receipted nested helper MIR");
    assert!(
        codegen.module.get_function("main").is_some(),
        "direct native entry must emit the program entry point"
    );
    assert!(
        codegen.module.get_function("llabs").is_some(),
        "direct native entry must retain the checker-bound extern symbol"
    );
    assert!(
        codegen.module.get_functions().any(|function| function
            .get_name()
            .to_string_lossy()
            .contains("main/function:local_abs:")),
        "direct native entry must emit the identity-bound nested owner"
    );
    codegen
        .module
        .verify()
        .expect("valid nested scalar FFI module");
}

#[test]
fn direct_native_entry_rejects_nested_unit_result_without_legacy_access() {
    let source = r#"
        extern "C" { func notify(value: i32); }
        func main() {
            func helper(value: i32) { notify(value) }
            let done = helper(1)
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    assert!(
        crate::core::mir::classify_canonical_mir_route_admission(&program).scalar_ffi,
        "the declaration remains a scalar FFI candidate"
    );

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "nested_unit_result_rejected");
    let errors = codegen
        .compile_checked(&program)
        .expect_err("direct native entry must reject a nested Unit value");
    assert!(
        !errors.is_empty(),
        "the unsupported MIR ABI must be explicit"
    );
}

#[test]
fn direct_native_entry_rejects_complete_scalar_collection_materialization_failure() {
    // R6-1052 closure: a shape construction that cannot lower (a nested-block
    // assign outside the MIR Phase 0 scalar-assign face) is an explicit
    // coverage failure. Production codegen must not reopen the legacy body.
    let source = r#"
        func main() -> i32 {
            let mut total = 0
            let flag = true
            if flag { total = 5 }
            println(total)
            total
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    assert_eq!(
        crate::core::mir::classify_scalar_collection_admission(&program),
        crate::core::mir::ScalarCollectionAdmission::CompleteCoverage
    );

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "s23_scalar_collection_materialization_error");
    let errors = codegen
        .compile_checked(&program)
        .expect_err("nested-assign construction failure must fail closed");
    assert!(errors.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("MIR-COVERAGE-001")
            || diagnostic.code.as_deref() == Some("MIR-LOWERING-001")
    }));
}

#[test]
fn compile_checked_routes_exact_flat_copy_record_through_canonical_mir() {
    let source = include_str!("../../tests/fixtures/mir_native_record_update.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&program)
        .expect("canonical MIR");
    assert!(crate::core::mir::contains_flat_copy_record_candidate(
        &canonical
    ));
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("flat Copy record consumer capability");
    crate::codegen::mir::validate_mir_native(&canonical)
        .expect("flat Copy record native capability");

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "s15_flat_copy_record_route");
    codegen
        .compile_checked(&program)
        .expect("flat Copy record must use the canonical MIR native consumer");
    assert!(codegen.module.get_function("main").is_some());
    assert!(codegen.resolved_failed_functions().is_empty());
}

#[test]
fn compile_checked_routes_concrete_protocol_method_without_legacy_access() {
    let source = include_str!("../../tests/fixtures/mir_protocol_method.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&program)
        .expect("canonical protocol-method MIR");
    crate::verifier::validate_mir_capabilities(&canonical)
        .expect("protocol-method verifier capability");
    crate::codegen::mir::validate_mir_native(&canonical)
        .expect("protocol-method native capability");

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "flat_record_protocol_default_route");
    codegen
        .compile_checked(&program)
        .expect("direct native API must use canonical protocol-method MIR");
    assert!(codegen.module.get_function("main").is_some());
    assert!(codegen.resolved_failed_functions().is_empty());
}

#[test]
fn compile_checked_rejects_dynamic_protocol_method_without_legacy_owner() {
    let source = include_str!("../../tests/fixtures/mir_protocol_method_dyn.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "dyn_protocol_rejected");
    let errors = codegen
        .compile_checked(&program)
        .expect_err("dynamic protocol dispatch must fail closed before legacy codegen");
    assert!(errors.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    }));
}

#[test]
fn compile_checked_routes_multi_transition_flow_through_canonical_mir() {
    let source = include_str!("../../tests/real_world/flow_multi_transition_selfloops.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "flow_transition_canonical_owner");
    codegen
        .compile_checked(&program)
        .expect("multi-transition Flow must use canonical MIR native codegen");
    assert!(
        codegen.module.get_function("main").is_some(),
        "canonical multi-transition Flow must emit main"
    );
}

fn compile_native_fixture(
    source: &str,
    module_name: &str,
) -> Result<(), Vec<crate::diagnostic::Diagnostic>> {
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("fixture lexes");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("fixture parses");
    let program = crate::core::check_program(&file)
        .unwrap_or_else(|diagnostics| panic!("fixture checks: {diagnostics:?}"));
    let context = Context::create();
    let result = CodeGenerator::new(&context, module_name).compile_checked(&program);
    result
}

#[test]
fn native_flow_actor_transition_fails_closed_without_hijacking_flow_or_actor_methods() {
    let same_name_demo = include_str!("../../demos/13_actors.mimi");
    let same_name_diagnostics =
        compile_native_fixture(same_name_demo, "flow_actor_same_name_diagnostic")
            .expect_err("native backend must reject synthetic runs-Flow transition dispatch");
    assert!(
        same_name_diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0722)
                && diagnostic
                    .message
                    .contains("native LLVM backend does not support synthetic transition method")
                && diagnostic
                    .message
                    .contains("native actor mailboxes do not retain Flow state")
        }),
        "expected explicit native Flow-actor capability diagnostic, got: {same_name_diagnostics:?}"
    );

    let name_disjoint_actor = r#"
        flow Counter {
            state Zero { n: i32 }
            transition add(Zero, amount: i32) -> Zero {
                return Zero { n: self.n + amount }
            }
        }
        actor Worker runs Counter {}
        func main() -> i32 {
            let worker = Worker.spawn()
            worker.add(1)
            0
        }
    "#;
    let name_disjoint_diagnostics =
        compile_native_fixture(name_disjoint_actor, "flow_actor_name_disjoint_diagnostic")
            .expect_err("name-disjoint runs-Flow actor transition must fail closed too");
    assert!(
        name_disjoint_diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0722)
                && diagnostic.message.contains("Worker::add")
                && diagnostic.message.contains("runs Flow 'Counter'")
        }),
        "expected explicit native Flow-actor capability diagnostic, got: {name_disjoint_diagnostics:?}"
    );

    let self_transition = r#"
        flow Counter {
            state Zero { n: i32 }
            transition add(Zero, amount: i32) -> Zero {
                return Zero { n: self.n + amount }
            }
        }
        actor Worker runs Counter {
            func invoke(amount: i32) -> i32 {
                let next = self.add(amount)
                next.n
            }
        }
        func main() -> i32 {
            let worker = Worker.spawn()
            worker.invoke(1)
        }
    "#;
    let self_transition_diagnostics =
        compile_native_fixture(self_transition, "flow_actor_self_transition_diagnostic")
            .expect_err("native actor self-call to a synthetic transition must fail closed");
    assert!(
        self_transition_diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0722)
                && diagnostic.message.contains("Worker::add")
                && diagnostic.message.contains("runs Flow 'Counter'")
        }),
        "expected explicit native Flow-actor self-call diagnostic, got: {self_transition_diagnostics:?}"
    );

    let explicit_actor_method_precedence = r#"
        flow Job {
            state Ready { n: i32 }
            transition ping(Ready) -> Ready { return Ready { n: self.n + 1 } }
        }
        actor Runner runs Job {
            func ping() -> i32 { return 9 }
        }
        func main() -> i32 {
            let runner = Runner.spawn()
            runner.ping()
        }
    "#;
    compile_native_fixture(
        explicit_actor_method_precedence,
        "flow_actor_explicit_method_precedence",
    )
    .expect("explicit actor method must take precedence over a colliding Flow transition");

    let standalone_flow_transition = r#"
        flow Job {
            state Ready { n: i32 }
            transition advance(Ready) -> Ready { return Ready { n: self.n + 1 } }
        }
        func main() -> i32 {
            let before = Ready { n: 40 }
            let after = Job.advance(before)
            after.n
        }
    "#;
    compile_native_fixture(
        standalone_flow_transition,
        "standalone_flow_transition_control",
    )
    .expect("standalone Flow namespace transition must remain compilable natively");
}

#[test]
fn compile_checked_rejects_trait_impl_specialization_without_legacy_owner() {
    let source = r#"
        trait Computable {
            func compute() -> i32
        }

        type Data {
            value: i32
        }

        impl Computable for Data {
            func compute() -> i32 { 11 }
        }

        func choose<T>(left: T, right: T) -> T { left }
        func main() -> i32 { choose(7, 9) }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "trait_impl_rejected");
    let errors = codegen
        .compile_checked(&program)
        .expect_err("trait implementation specialization must fail closed");
    assert!(errors.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    }));
}

#[test]
fn compile_checked_routes_captured_nested_helper_without_legacy_owner() {
    let source = r#"
        func with_parameter(seed: i32) -> i32 {
            let mut offset = seed
            func nested_increment(value: i32) -> i32 { value + offset + seed }
            offset = 100
            nested_increment(40)
        }
        func main() -> i32 { with_parameter(1) }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let mir = crate::core::mir::reference::MirProgram::from_checked_program(&program)
        .expect("captured Copy i32 helper must lower with a declaration snapshot");
    assert_eq!(mir.nested_callable_scopes().len(), 1);
    assert_eq!(
        mir.nested_callable_scopes()
            .values()
            .next()
            .expect("nested scope receipt")
            .capture_slots
            .len(),
        2
    );
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "nested_helper_canonical_route");
    codegen
        .compile_checked(&program)
        .expect("captured nested helper uses the canonical MIR default route");
    codegen
        .module
        .verify()
        .expect("captured nested helper LLVM module");
    assert!(codegen.module.get_function("main").is_some());
    assert!(
        crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty(),
        "canonical nested capture must not invoke the legacy emitter"
    );
}

#[test]
fn compile_checked_routes_scalar_export_wrapper_without_legacy_access() {
    let source = r#"
        extern "C" func owner_tripwire_export(value: i32) -> i32 {
            value + 1
        }
        func main() -> i32 { 0 }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    assert!(crate::core::mir::classify_canonical_mir_route_admission(&program).scalar_export);

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "export_wrapper_legacy_owner");
    codegen
        .compile_checked(&program)
        .expect("scalar C export wrapper must compile canonically");
    assert!(codegen.module.get_function("main").is_some());
    assert!(codegen
        .module
        .get_function("owner_tripwire_export")
        .is_some());
    assert!(codegen
        .module
        .get_function(&CodeGenerator::export_body_symbol("owner_tripwire_export"))
        .is_some());
    assert_eq!(
        codegen
            .module
            .get_function("owner_tripwire_export")
            .expect("public scalar wrapper")
            .get_linkage(),
        inkwell::module::Linkage::External
    );
    assert_eq!(
        codegen
            .module
            .get_function(&CodeGenerator::export_body_symbol("owner_tripwire_export"))
            .expect("internal scalar body")
            .get_linkage(),
        inkwell::module::Linkage::Internal
    );
}

#[test]
fn compile_checked_routes_scalar_ffi_with_export_body_without_legacy_access() {
    let source = r#"
        extern "C" { func abs(value: i64) -> i64; }
        extern "C" func internal_export(value: i64) -> i64 { value + 1 }
        func main() -> i32 { println(abs(internal_export(-3))) 0 }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "scalar_ffi_export_compatibility");
    codegen
        .compile_checked(&program)
        .expect("mixed scalar import and export body must use canonical codegen");
    assert!(codegen.module.get_function("main").is_some());
    assert!(codegen.module.get_function("internal_export").is_some());
    assert!(codegen
        .module
        .get_function(&CodeGenerator::export_body_symbol("internal_export"))
        .is_some());
}

#[test]
fn compile_checked_rejects_export_map_any_owner_composition_without_legacy_access() {
    let source = r#"
        extern "C" func internal_export(value: i32) -> i32 { value + 1 }
        func main() -> i32 {
            let root = map_new()
            let inserted = map_set(root, "answer", 42)
            let size = map_size(inserted)
            drop(inserted)
            internal_export(size)
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    assert!(
        crate::core::mir::classify_canonical_mir_route_admission(&program).map_any_owner_complete()
    );

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "export_body_precedes_map_any_owner");
    let error = codegen
        .compile_checked(&program)
        .expect_err("mixed scalar export and MapAnyOwner must fail closed");
    assert!(error.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            && diagnostic
                .message
                .contains("MapAnyOwner v1 profile cannot be combined")
            && diagnostic.message.contains("scalar C export")
    }));
}

#[test]
fn compile_checked_routes_recursive_c_export_calls_to_internal_body() {
    let source = r#"
        extern "C" func recursive_export(value: i32) -> i32 {
            if value <= 1 { 1 } else { value * recursive_export(value - 1) }
        }
        // This source-level name collided with the old generated suffix.
        func recursive_export__mimi_export_body(value: bool) -> bool { value }
        func main() -> i32 {
            recursive_export(5)
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "recursive_export_internal_abi");
    codegen
        .compile_checked(&program)
        .expect("recursive C-export body must compile");

    let public_wrapper = codegen
        .module
        .get_function("recursive_export")
        .expect("public C wrapper");
    let internal_body = codegen
        .module
        .get_function(&CodeGenerator::export_body_symbol("recursive_export"))
        .expect("private Mimi body");
    let source_collision = codegen
        .module
        .get_function("recursive_export__mimi_export_body")
        .expect("legal source function formerly colliding with generated suffix");
    let main = codegen.module.get_function("main").expect("main body");
    assert_eq!(public_wrapper.count_basic_blocks(), 1);
    assert_eq!(
        public_wrapper.get_linkage(),
        inkwell::module::Linkage::External,
        "the ABI conversion wrapper owns the public symbol"
    );
    assert!(internal_body.count_basic_blocks() > 0);
    assert_eq!(
        internal_body.get_linkage(),
        inkwell::module::Linkage::Internal,
        "the implementation body is not part of the exported C ABI"
    );
    assert!(source_collision.count_basic_blocks() > 0);
    let body_symbol = CodeGenerator::export_body_symbol("recursive_export");
    let internal_text = internal_body.to_string();
    assert!(
        internal_text.contains("call i32") && internal_text.contains(&body_symbol),
        "recursive calls must use Mimi's internal ABI: {internal_body}"
    );
    assert!(
        !internal_body
            .to_string()
            .contains("call i32 @recursive_export("),
        "the body must not recurse through the C ABI wrapper: {internal_body}"
    );
    let main_text = main.to_string();
    assert!(
        main_text.contains("call i32") && main_text.contains(&body_symbol),
        "Mimi callers must use the internal body symbol: {main}"
    );
    codegen
        .module
        .verify()
        .expect("recursive export LLVM module must verify");
}

#[test]
fn compile_checked_routes_scalar_actor_through_canonical_mir_without_legacy_access() {
    let source = r#"
        actor OwnerTripwireWorker {
            value: i32
            func read() -> i32 { self.value }
        }
        func main() -> i32 {
            let worker = OwnerTripwireWorker.spawn()
            worker.read()
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let route = crate::core::mir::materialize_canonical_mir_route(&program, None)
        .expect("closed scalar Actor route");
    assert!(route.admission.scalar_actor);
    assert!(
        crate::core::mir::CanonicalMirRouteProfile::ScalarActor.is_materialized(&route),
        "the actor method body must be present in the selected canonical graph"
    );

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "actor_body_legacy_owner");
    codegen
        .compile_checked(&program)
        .expect("canonical scalar Actor method must remain compilable");
    assert!(codegen.module.get_function("main").is_some());
    let actor = route
        .program
        .actors()
        .iter()
        .next()
        .expect("one checker-owned Actor contract")
        .1;
    let worker_owner = &actor.methods.first().expect("read method").owner;
    assert!(
        route
            .program
            .actor_execution_plan()
            .expect("Actor execution plan")
            .tagged_functions
            .contains(worker_owner),
        "the Actor method must be in the tagged canonical worker graph"
    );
    let worker_symbol = format!(
        "__mimi_actor_status_{}",
        worker_owner
            .0
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    assert!(
        codegen.module.get_function(&worker_symbol).is_some(),
        "the checker-owned actor method must be emitted under its canonical status ABI"
    );
    assert!(
        codegen
            .module
            .get_function("OwnerTripwireWorker__read__method")
            .is_none(),
        "the legacy Actor worker must not shadow the canonical body"
    );
    codegen
        .module
        .verify()
        .expect("canonical scalar Actor module must verify");
}

#[test]
fn compile_checked_rejects_contract_bearing_actor_without_legacy_owner() {
    let source = r#"
        actor OwnerTripwireWorker {
            value: i32
            func read() -> i32 { self.value }
        }
        func guarded(value: i32) -> i32 {
            ensures: result == value
            value
        }
        func main() -> i32 {
            let worker = OwnerTripwireWorker.spawn()
            worker.read()
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let boundary = crate::core::mir::scalar_actor_contract_boundary_reason(&program)
        .expect("the contract-bearing callable must stay outside ScalarActor NoContracts");
    assert!(
        boundary.contains("guarded"),
        "unexpected boundary reason: {boundary}"
    );
    assert!(
        !crate::core::mir::classify_canonical_mir_route_admission(&program).scalar_actor,
        "a whole-program proof obligation must keep this Actor graph out of ScalarActor"
    );

    let context = Context::create();
    let errors = CodeGenerator::new(&context, "actor_body_rejected")
        .compile_checked(&program)
        .expect_err("contract-bearing Actor must fail closed outside ScalarActor");
    assert!(errors.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    }));
}

#[test]
fn compile_checked_rejects_map_any_borrowed_access_without_legacy_owner() {
    let source = r#"
        func main() -> i32 {
            let original = map_new()
            let map = map_set(original, "k", "borrowed text")
            let found = map_get(map, "k")
            let borrowed_values = values(map)
            println(found.0)
            println(borrowed_values.len())
            drop(borrowed_values)
            0
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    let context = Context::create();
    let errors = CodeGenerator::new(&context, "map_any_borrowed_rejected")
        .compile_checked(&program)
        .expect_err("borrowed Map/Any access must fail closed outside its receipt");
    assert!(errors.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    }));
}

#[test]
fn compile_checked_map_string_copy_keeps_typed_ingress_and_checks_failure() {
    let cases = [
        ("literal", "literal", "", "", r#""A\0雪""#),
        (
            "transparent alias",
            "alias",
            "type InnerText = string\ntype Text = InnerText",
            r#"let aliased: Text = "A\0雪""#,
            "aliased",
        ),
        (
            "helper return",
            "helper",
            "type InnerText = string\ntype Text = InnerText\nfunc message() -> Text { \"A\\0雪\" }",
            "",
            "message()",
        ),
    ];

    for (case_name, module_suffix, declarations, setup, value) in cases {
        let source = format!(
            r#"
            {declarations}
            func main() -> i32 {{
                {setup}
                let original = map_new()
                let map = map_set(original, "k", {value})
                println(to_json(map))
                0
            }}
            "#
        );
        let tokens = crate::lexer::Lexer::new(&source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
        let context = Context::create();
        let module_name = format!("map_string_owner_ingress_{module_suffix}");
        let mut codegen = CodeGenerator::new(&context, &module_name);
        codegen.compile_checked(&program).unwrap_or_else(|errors| {
            panic!("compile typed String ingress ({case_name}): {errors:?}")
        });
        codegen
            .module
            .verify()
            .unwrap_or_else(|error| panic!("verify typed String ingress ({case_name}): {error}"));
        let ir = codegen.module.print_to_string().to_string();
        assert!(
            ir.contains("call i32 @mimi_map_set_string_copy("),
            "{case_name}: {ir}"
        );
        assert!(
            ir.contains("map_string_copy_succeeded"),
            "{case_name}: {ir}"
        );
        assert!(
            ir.contains("Map String insertion failed"),
            "{case_name}: {ir}"
        );
        assert!(ir.contains("i64 5"), "preserve UTF-8/NUL byte length: {ir}");
        assert!(
            !ir.contains("call i64 @mimi_any_string_clone("),
            "{case_name}: {ir}"
        );
        assert!(
            crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty(),
            "{case_name} must not invoke the legacy body emitter"
        );
    }
}

#[test]
fn compile_checked_rejects_actor_tuple_map_payload_without_legacy_access() {
    let source = r#"
        actor StringShapeWorker { func ping() -> i32 { 1 } }
        func main() -> i32 {
            let worker = StringShapeWorker.spawn()
            let root = map_new()
            let saved = map_set(root, "payload", (worker, 16 as i64))
            drop(saved)
            0
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "actor_tuple_map_payload_rejected");
    let errors = codegen
        .compile_checked(&program)
        .expect_err("the actor-handle/i64 tuple has no canonical Map payload receipt");
    assert!(
        errors.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                && diagnostic.message.contains("record-valued map_set")
        }),
        "tuple Map payload must fail closed at the canonical route boundary: {errors:?}"
    );
    assert!(
        crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty(),
        "a rejected tuple payload must not invoke the legacy body emitter"
    );
}

fn checked_with_std_maps(source: &str) -> (std::path::PathBuf, crate::core::CheckedProgram) {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mimi-codegen-any-unpack-{}-{}",
        std::process::id(),
        NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create temporary Map/Any fixture directory");
    let path = dir.join("main.mimi");
    std::fs::write(&path, source).expect("write temporary Map/Any fixture");

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    loader.load_main(&path).expect("load std::maps fixture");
    let merged = loader.merge_all().expect("merge std::maps fixture");
    let program = crate::core::check_program(&merged).expect("check std::maps fixture");
    (dir, program)
}

fn checked_with_aliased_map_owner_helper(
    main_source: &str,
) -> (std::path::PathBuf, crate::core::CheckedProgram) {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mimi-codegen-aliased-map-owner-{}-{}",
        std::process::id(),
        NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    let apps = dir.join("apps");
    std::fs::create_dir_all(&apps).expect("create temporary module alias directory");
    std::fs::write(
        dir.join("mimi.toml"),
        "[package]\nname = \"map_alias_fixture\"\nversion = \"0.1.0\"\nentry = \"main.mimi\"\n",
    )
    .expect("write temporary module alias manifest");
    std::fs::write(
        apps.join("left.mimi"),
        r#"
use std::maps

pub func unsupported_map_read() -> i32 {
    let root = map_new()
    let pair = map_get_i32(root, "answer")
    let unsupported = keys(root)
    drop(pair)
    drop(unsupported)
    drop(root)
    0
}
"#,
    )
    .expect("write aliased Map helper module");
    let main_path = dir.join("main.mimi");
    std::fs::write(&main_path, main_source).expect("write aliased Map entry");

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    loader
        .load_main(&main_path)
        .expect("load aliased Map fixture");
    let merged = loader.merge_all().expect("merge aliased Map fixture");
    let program = crate::core::check_program(&merged).expect("check aliased Map fixture");
    (dir, program)
}

#[test]
fn compile_checked_rejects_incomplete_owned_get_reached_through_module_alias() {
    let (dir, program) = checked_with_aliased_map_owner_helper(
        r#"
use apps::left as left

func main() -> i32 { left.unsupported_map_read() }
"#,
    );
    let helper = program
        .functions()
        .values()
        .find(|function| function.qualified_name == "unsupported_map_read")
        .expect("aliased imported Map helper")
        .node_id
        .clone();
    let admission = crate::core::mir::classify_canonical_mir_route_admission(&program);
    assert!(admission
        .ordinary_map_owned_get_i32_string_v1
        .has_profile_signal());
    assert!(!admission.ordinary_map_owned_get_i32_string_v1_complete());
    assert!(program.call_sites().values().any(|site| {
        site.owner == "function:main" && site.target_function.as_ref() == Some(&helper)
    }));
    let reachable = super::resolved::reachable_callable_ids_from_roots(
        &program,
        &std::collections::BTreeSet::new(),
    );
    assert!(
        reachable.contains(&helper),
        "module-alias call must resolve to its exact Checker callable target"
    );
    assert!(
        reachable.len() < program.callables().len(),
        "a resolved direct-function alias must not degrade to the all-callable conservative fallback"
    );

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "aliased_incomplete_map_owned_get");
    let errors = codegen
        .compile_checked(&program)
        .expect_err("reachable incomplete owned-Get helper must fail closed");
    assert!(
        errors.iter().any(|error| {
            error.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                && error.message.contains("ordinary Map owned-get attempt")
        }),
        "unexpected route error: {errors:?}"
    );

    std::fs::remove_dir_all(dir).expect("remove aliased Map fixture directory");
}

#[test]
fn compile_checked_fails_closed_on_reachable_map_any_unpack() {
    let (dir, program) = checked_with_std_maps(
        r#"
use std::maps

func main() -> i32 {
    let m = new()
    let with_text = set(m, "key", "text")
    let observed = get_or_default(with_text, "key", 0)
    println(observed)
    0
}
"#,
    );

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "reachable_dynamic_any_unpack");
    let errors = codegen
        .compile_checked(&program)
        .expect_err("native compilation must not truncate a stored String into i32");
    assert_eq!(
        errors[0].code.as_deref(),
        Some(crate::diagnostic::codes::E0722)
    );
    assert!(errors[0].message.contains("reachable DynamicAnyUnpack"));
    assert!(errors[0]
        .message
        .contains("runtime Any value has no checked tag"));

    let context = Context::create();
    let mut strict = CodeGenerator::new(&context, "strict_dynamic_any_unpack");
    let strict_errors = strict
        .compile_resolved_native(&program)
        .expect_err("strict Resolved-native API must reject DynamicAnyUnpack too");
    assert!(strict_errors
        .iter()
        .any(|error| error.message.contains("DynamicAnyUnpack")));

    let entry = program
        .functions()
        .values()
        .find(|function| function.qualified_name == "main")
        .expect("Map/Any fixture main")
        .node_id
        .clone();
    let context = Context::create();
    let mut subset = CodeGenerator::new(&context, "subset_dynamic_any_unpack");
    let subset_errors = subset
        .compile_resolved_subset(&program, &std::collections::BTreeSet::from([entry]))
        .expect_err("public subset API must not bypass the reachable Any hard gate");
    assert!(subset_errors
        .iter()
        .any(|error| error.message.contains("DynamicAnyUnpack")));

    std::fs::remove_dir_all(dir).expect("remove temporary Map/Any fixture directory");
}

#[test]
fn compile_checked_ignores_unreachable_map_any_unpack_bodies() {
    let (dir, program) = checked_with_std_maps(
        r#"
use std::maps

func main() -> i32 { 42 }
"#,
    );

    let dormant_catalog_error =
        crate::core::mir::types::MirTypeCatalog::from_checked_program(&program)
            .expect_err("the dormant imported Map/Any catalog remains outside its closed profile");
    assert!(
        format!("{dormant_catalog_error:?}").contains("ordinary Map/Any"),
        "this regression must exercise the dormant Map catalog boundary"
    );

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "unreachable_dynamic_any_unpack");
    codegen
        .compile_checked(&program)
        .expect("unreachable std::maps accessors must not block an unrelated scalar entry");
    assert!(codegen.module.get_function("main").is_some());
    for function in program.functions().values() {
        if super::resolved::is_std_maps_callable(&program, &function.node_id) {
            assert!(
                codegen
                    .module
                    .get_function(&function.qualified_name)
                    .is_none(),
                "dormant maps callable '{}' must not even be declared",
                function.qualified_name
            );
        }
    }
    codegen
        .module
        .verify()
        .expect("verify scalar cohort module");

    let context = Context::create();
    let mut strict = CodeGenerator::new(&context, "strict_dormant_dynamic_any_unpack");
    let errors = strict
        .compile_resolved_native(&program)
        .expect_err("the public all-callable API must retain its dormant-body boundary");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("DynamicAnyUnpack")));
    assert!(strict.module.get_function("main").is_none());

    std::fs::remove_dir_all(dir).expect("remove temporary Map/Any fixture directory");
}

#[test]
fn reachable_map_import_cohort_keeps_generic_catalog_failure_closed() {
    let (dir, program) = checked_with_std_maps(
        r#"
use std::maps

func scalar_identity<T>(value: T) -> T { value }
func main() -> i32 { scalar_identity(42) }
"#,
    );

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "reachable_generic_map_import_cohort");
    let errors = codegen
        .compile_resolved_native_reachable(&program, &std::collections::BTreeSet::new())
        .expect_err("a reachable generic call must retain strict generic catalog validation");
    assert!(
        errors.iter().any(|error| {
            error.message.contains("MIR-COVERAGE-001/NITS")
                && error
                    .message
                    .contains("canonical generic TypeDesc catalog failed")
                && error.message.contains("ordinary Map/Any")
        }),
        "unexpected reachable generic catalog error: {errors:?}"
    );
    assert!(codegen.module.get_function("main").is_none());

    let context = Context::create();
    let mut checked = CodeGenerator::new(&context, "checked_reachable_generic_map_import_cohort");
    let errors = checked
        .compile_checked(&program)
        .expect_err("the default checked route must keep generic catalog validation strict");
    assert!(
        errors.iter().any(|error| {
            error.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                && error
                    .message
                    .contains("reachable generic call has no complete canonical instance receipt")
        }),
        "default checked route did not preserve strict generic failure: {errors:?}"
    );
    assert!(checked.module.get_function("main").is_none());

    std::fs::remove_dir_all(dir).expect("remove reachable generic Map import fixture directory");
}

#[test]
fn compile_checked_dormant_map_exception_does_not_hide_user_map_helpers() {
    let (dir, program) = checked_with_std_maps(
        r#"
func unsupported_user_map_helper() -> i32 {
    let root = map_new()
    let value = map_get_i32(root, "key")
    let unsupported = keys(root)
    drop(value)
    drop(unsupported)
    drop(root)
    0
}
func main() -> i32 { 0 }
"#,
    );
    let helper = program
        .functions()
        .values()
        .find(|function| {
            function
                .qualified_name
                .ends_with("unsupported_user_map_helper")
        })
        .expect("dormant user Map helper")
        .node_id
        .clone();
    let reachable = super::resolved::reachable_callable_ids_from_roots(
        &program,
        &std::collections::BTreeSet::new(),
    );
    assert!(!reachable.contains(&helper));
    assert!(!super::resolved::is_std_maps_callable(&program, &helper));

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "dormant_user_map_helper");
    let errors = codegen
        .compile_checked(&program)
        .expect_err("only exact dormant stdlib maps signals may bypass Map admission");
    assert!(
        errors.iter().any(|error| {
            error.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                && error.message.contains("ordinary Map owned-get attempt")
        }),
        "unexpected dormant-user Map error: {errors:?}"
    );
    assert!(codegen.module.get_function("main").is_none());

    std::fs::remove_dir_all(dir).expect("remove dormant user Map fixture directory");
}

#[test]
fn compile_checked_dormant_map_exception_still_validates_other_user_helpers() {
    let (dir, program) = checked_with_std_maps(
        r#"
use std::maps

func unsupported_user_helper(xs: Set<f64>) -> i32 { 0 }
func main() -> i32 { 42 }
"#,
    );

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "dormant_user_native_boundary");
    let errors = codegen
        .compile_checked(&program)
        .expect_err("dormant unrelated user helpers must keep strict native validation");
    assert!(
        errors.iter().any(|error| {
            error.message.contains("unsupported_user_helper") && error.message.contains("Set")
        }),
        "unexpected dormant-user boundary error: {errors:?}"
    );
    assert!(codegen.module.get_function("main").is_none());

    std::fs::remove_dir_all(dir).expect("remove dormant user native fixture directory");
}

#[test]
fn compile_checked_rejects_map_any_unpack_in_c_export_roots() {
    let (dir, program) = checked_with_std_maps(
        r#"
use std::maps

extern "C" func exported_wrong_tag() -> i32 {
    let m = new()
    let with_text = set(m, "key", "text")
    get_or_default(with_text, "key", 0)
}

func main() -> i32 { 0 }
"#,
    );

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "exported_dynamic_any_unpack");
    let errors = codegen
        .compile_checked(&program)
        .expect_err("a host-callable C export must be included in native safety roots");
    assert_eq!(
        errors[0].code.as_deref(),
        Some(crate::diagnostic::codes::E0722)
    );
    assert!(errors[0].message.contains("reachable DynamicAnyUnpack"));

    std::fs::remove_dir_all(dir).expect("remove temporary Map/Any fixture directory");
}

#[test]
fn shared_and_subset_native_entries_reject_map_any_unpack() {
    let (dir, program) = checked_with_std_maps(
        r#"
use std::maps

func host_callable_wrong_tag() -> i32 {
    let m = new()
    let with_text = set(m, "key", "text")
    get_or_default(with_text, "key", 0)
}

func main() -> i32 { 0 }
"#,
    );
    let helper = program
        .functions()
        .values()
        .find(|function| function.qualified_name == "host_callable_wrong_tag")
        .expect("host-callable fixture helper")
        .node_id
        .clone();

    let context = Context::create();
    let mut shared = CodeGenerator::new(&context, "shared_dynamic_any_unpack");
    shared.shared = true;
    let errors = shared
        .compile_checked(&program)
        .expect_err("every emitted shared-library function can be called by its host");
    assert_eq!(
        errors[0].code.as_deref(),
        Some(crate::diagnostic::codes::E0722)
    );

    let context = Context::create();
    let mut subset = CodeGenerator::new(&context, "subset_host_dynamic_any_unpack");
    let subset_errors = subset
        .compile_resolved_subset(&program, &std::collections::BTreeSet::from([helper]))
        .expect_err("a directly selected native function is itself an entry root");
    assert!(subset_errors
        .iter()
        .any(|error| error.code.as_deref() == Some(crate::diagnostic::codes::E0722)));
    assert!(
        subset
            .module
            .get_function("host_callable_wrong_tag")
            .is_none(),
        "the subset gate must reject before partially emitting the selected body"
    );

    std::fs::remove_dir_all(dir).expect("remove temporary Map/Any fixture directory");
}

#[test]
fn shared_object_serializers_require_shared_mode_dynamic_any_preflight() {
    let (dir, program) = checked_with_std_maps(
        r#"
use std::maps

func host_callable_wrong_tag() -> i32 {
    let m = new()
    let with_text = set(m, "key", "text")
    get_or_default(with_text, "key", 0)
}

func main() -> i32 { 0 }
"#,
    );

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "shared_object_preflight_provenance");
    codegen
        .compile_checked(&program)
        .expect("the ordinary executable entry cannot reach the unsafe helper");
    codegen.shared = true;

    let shared_output = dir.join("must-not-be-emitted-shared.o");
    let shared_error = codegen
        .compile_to_object_shared(&shared_output)
        .expect_err("shared serialization must not reuse the executable-root preflight");
    assert!(shared_error
        .to_string()
        .contains("requires a successful checked native compilation in shared mode"));
    assert!(
        !shared_output.exists(),
        "rejected shared serialization must not leave an object file"
    );

    let ordinary_output = dir.join("must-not-be-emitted-shared-mode.o");
    let ordinary_error = codegen
        .compile_to_object(&ordinary_output)
        .expect_err("compile_to_object also emits bare symbols while shared is enabled");
    assert!(ordinary_error
        .to_string()
        .contains("requires a successful checked native compilation in shared mode"));
    assert!(
        !ordinary_output.exists(),
        "rejected shared-mode serialization must not leave an object file"
    );

    std::fs::remove_dir_all(dir).expect("remove temporary Map/Any fixture directory");
}

#[test]
fn compile_checked_routes_exact_option_string_switch_through_canonical_mir() {
    let source = include_str!("../../tests/fixtures/mir_native_option_string_switch_move.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    assert_eq!(
        crate::core::mir::classify_option_string_variant_admission(&program),
        crate::core::mir::OptionStringVariantAdmission::CompleteCoverage
    );

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "s30_option_string_default_route");
    codegen
        .compile_checked(&program)
        .expect("direct native entry must use the canonical Option<string> consumer");
    assert!(codegen.module.get_function("main").is_some());
    assert!(codegen.resolved_failed_functions().is_empty());
}

#[test]
fn compile_checked_routes_generic_option_result_and_managed_result_profiles_without_legacy_access()
{
    use crate::core::mir::CanonicalMirRouteProfile as Profile;

    const CASES: &[(&str, Profile)] = &[
        (
            include_str!("../../tests/fixtures/mir_native_generic_option_predicate.mimi"),
            Profile::GenericOptionPredicate,
        ),
        (
            include_str!("../../tests/fixtures/mir_native_generic_option_unwrap.mimi"),
            Profile::GenericOptionProjection,
        ),
        (
            include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_or.mimi"),
            Profile::GenericOptionProjectionFallback,
        ),
        (
            include_str!("../../tests/fixtures/mir_native_generic_result_unwrap.mimi"),
            Profile::GenericResultProjection,
        ),
        (
            include_str!("../../tests/fixtures/mir_native_generic_result_unwrap_or.mimi"),
            Profile::GenericResultProjectionFallback,
        ),
        (
            include_str!("../../tests/fixtures/mir_result_list_i32_call_return.mimi"),
            Profile::ManagedResultCall,
        ),
    ];

    for (source, profile) in CASES {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let mut file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        crate::loader::merge_prelude_into(&mut file);
        let program = crate::core::check_program(&file)
            .unwrap_or_else(|diags| panic!("check {}: {diags:?}", profile.as_str()));
        let route = crate::core::mir::materialize_canonical_mir_route(&program, None)
            .unwrap_or_else(|error| panic!("materialize {}: {error:?}", profile.as_str()));
        assert!(
            profile.is_admitted(route.admission),
            "{} must have checker-owned complete admission",
            profile.as_str()
        );
        assert!(
            profile.is_materialized(&route),
            "{} must have its canonical operation receipt",
            profile.as_str()
        );

        let context = Context::create();
        let mut codegen = CodeGenerator::new(&context, profile.as_str());
        codegen
            .compile_checked(&program)
            .unwrap_or_else(|error| panic!("compile_checked {}: {error:?}", profile.as_str()));
        assert!(
            codegen.module.get_function("main").is_some(),
            "{} direct native API must emit main",
            profile.as_str()
        );
        assert!(
            codegen.resolved_failed_functions().is_empty(),
            "{} direct native API must not defer a resolved failure",
            profile.as_str()
        );
    }
}

#[test]
fn compile_checked_rejects_unmaterialized_migrated_candidates_without_legacy_fallback() {
    const CASES: &[(&str, &str)] = &[
        (
            "generic Option predicate unsupported payload",
            include_str!("../../tests/fixtures/mir_native_generic_option_predicate_rejected.mimi"),
        ),
        (
            "generic Option managed payload",
            include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_rejected.mimi"),
        ),
        (
            "generic Option fallback with an extra statement",
            include_str!("../../tests/fixtures/mir_native_generic_option_unwrap_or_rejected.mimi"),
        ),
        (
            "generic Result string error slot",
            include_str!("../../tests/fixtures/mir_native_generic_result_unwrap_rejected.mimi"),
        ),
        (
            "generic Result fallback with an extra statement",
            include_str!("../../tests/fixtures/mir_native_generic_result_unwrap_or_rejected.mimi"),
        ),
        (
            "managed Result float list payload",
            include_str!("../../tests/fixtures/mir_result_list_f64_call_rejected.mimi"),
        ),
    ];

    for (name, source) in CASES {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let mut file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        crate::loader::merge_prelude_into(&mut file);
        let program = crate::core::check_program(&file)
            .unwrap_or_else(|diags| panic!("check {name}: {diags:?}"));

        let context = Context::create();
        let mut codegen = CodeGenerator::new(&context, "mixed_generic_profile_rejected");
        let errors = match codegen.compile_checked(&program) {
            Ok(()) => panic!("{name} must fail closed at the MIR boundary"),
            Err(errors) => errors,
        };
        assert!(
            errors.iter().any(|diagnostic| matches!(
                diagnostic.code.as_deref(),
                Some("MIR-COVERAGE-001" | "MIR-LOWERING-001")
            )),
            "{name} must report a stable MIR boundary error: {errors:?}"
        );
    }
}

#[test]
fn compile_checked_routes_copy_option_and_result_profiles_without_legacy_access() {
    use crate::core::mir::CanonicalMirRouteProfile as Profile;

    const CASES: &[(&str, Profile)] = &[
        (
            include_str!("../../tests/fixtures/mir_native_option_i32_unwrap.mimi"),
            Profile::CopyOptionI32Variant,
        ),
        (
            include_str!("../../tests/fixtures/mir_native_option_bool_unwrap.mimi"),
            Profile::CopyOptionBoolVariant,
        ),
        (
            include_str!("../../tests/fixtures/mir_native_option_i64_unwrap.mimi"),
            Profile::CopyOptionI64Variant,
        ),
        (
            include_str!("../../tests/fixtures/mir_native_option_f64_unwrap.mimi"),
            Profile::CopyOptionF64Variant,
        ),
        (
            include_str!("../../tests/fixtures/mir_native_result_i32_unwrap.mimi"),
            Profile::CopyResultI32Variant,
        ),
    ];

    for (source, profile) in CASES {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file)
            .unwrap_or_else(|diags| panic!("check {}: {diags:?}", profile.as_str()));
        let route = crate::core::mir::materialize_canonical_mir_route(&program, None)
            .unwrap_or_else(|error| panic!("materialize {}: {error:?}", profile.as_str()));
        assert!(
            profile.is_admitted(route.admission),
            "{} must have checker-owned complete admission",
            profile.as_str()
        );
        assert!(
            profile.is_materialized(&route),
            "{} must have its canonical operation receipt",
            profile.as_str()
        );

        let context = Context::create();
        let mut codegen = CodeGenerator::new(&context, profile.as_str());
        codegen
            .compile_checked(&program)
            .unwrap_or_else(|error| panic!("compile_checked {}: {error:?}", profile.as_str()));
        assert!(
            codegen.module.get_function("main").is_some(),
            "{} direct native API must emit main",
            profile.as_str()
        );
        assert!(
            codegen.resolved_failed_functions().is_empty(),
            "{} direct native API must not defer a resolved failure",
            profile.as_str()
        );
    }
}

#[test]
fn compile_checked_routes_exact_nested_option_tuple_through_canonical_mir() {
    let source = include_str!("../../tests/real_world/mir_nested_tuple_option.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    assert_eq!(
        crate::core::mir::classify_option_nested_tuple_variant_admission(&program),
        crate::core::mir::OptionNestedTupleVariantAdmission::CompleteCoverage
    );

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "nested_option_tuple_default_route");
    codegen
        .compile_checked(&program)
        .expect("direct native entry must use the canonical nested Option tuple consumer");
    assert!(codegen.module.get_function("main").is_some());
    assert!(codegen.resolved_failed_functions().is_empty());
}

#[test]
fn direct_native_entry_rejects_non_exhaustive_option_string_switch_without_fallback() {
    let source =
        include_str!("../../tests/fixtures/mir_native_option_string_default_rejected.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "s30_option_string_default_reject");
    let errors = codegen
        .compile_checked(&program)
        .expect_err("direct native entry must reject the uncovered Option<string> shape");
    assert!(errors.iter().any(|error| {
        error.code.as_deref() == Some("MIR-CAPABILITY-001")
            && error.message.contains("Option<string>")
    }));
    assert!(codegen.module.get_function("main").is_none());
}

#[test]
fn direct_native_entry_rejects_a_mixed_flat_copy_record_graph_without_fallback() {
    let source = r#"
        type Point { x: i32 }

        func make_some() -> Result<string, i32> { Ok("owned") }

        func main() -> i32 {
            let point = Point { x: 1 }
            point.x
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "s15_flat_copy_record_reject");
    let errors = codegen.compile_checked(&program).expect_err(
        "a recognized mixed flat Copy-record graph must not re-enter legacy native codegen",
    );
    assert!(errors.iter().any(|error| {
        error.code.as_deref() == Some("MIR-CAPABILITY-001")
            || error.message.contains("canonical MIR")
    }));
    assert!(
        codegen.module.get_function("main").is_none(),
        "rejected canonical candidate must fail before legacy LLVM emission"
    );
}

#[test]
fn direct_native_entry_rejects_complete_record_materialization_failure() {
    let source = r#"
        type Point { x: i32 }

        func make_fn(p: Point) -> func(i32) -> i32 {
            fn(value: i32) -> i32 { value + 1 }
        }

        func main() -> i32 { 0 }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "s17_flat_copy_record_materialization_error");
    let errors = codegen
        .compile_checked(&program)
        .expect_err("complete record admission must not fall back after MIR construction failure");
    assert!(errors
        .iter()
        .any(|error| error.code.as_deref() == Some("MIR-LOWERING-001")));
    assert!(codegen.module.get_function("main").is_none());
}

#[test]
fn direct_native_entry_rejects_a_mixed_scalar_collection_candidate() {
    // R6-1076 restatement: the former pin rode on the island gate's String
    // print-face envelope, which this slice reduced to the canonical
    // StringHandle contract itself (the value/const arms are now
    // per-instruction contracts).  The narrowest managed value still
    // outside the envelope is the f64 leaf without its print face, so the
    // recognized mixed candidate keeps its cannot-re-enter-legacy
    // rejection.
    let source = "func main() -> i32 { let values = [1, 2, 3] let count = len(values) drop(values) let text = 1.5 drop(text) count }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "s12_scalar_collection_reject");
    let errors = codegen.compile_checked(&program).expect_err(
        "a recognized mixed collection candidate must not re-enter legacy native codegen",
    );
    assert!(errors
        .iter()
        .any(|error| error.code.as_deref() == Some("MIR-CAPABILITY-001")));
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
        persistent_fields: vec![],
        fault_type: None,
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

#[test]
fn checked_declaration_name_installation_does_not_open_legacy_body_owner() {
    let source = r#"
        cap A;
        cap B;
        cap Combined = A + B;
        cap Extended + B;
        trait Slots { func second() -> i32; func first() -> i32; }
        actor LaterActor { func get() -> i32 { 42 } }
        func main() -> i32 { 0 }
    "#;
    let file =
        crate::parser::Parser::new(crate::lexer::Lexer::new(source).tokenize().expect("lex"))
            .parse_file()
            .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "checked_names");
    codegen
        .install_checked_declaration_names(&program)
        .expect("install checked declarations");
    assert_eq!(codegen.cap_components["Combined"], ["A", "B"]);
    assert_eq!(codegen.cap_components["Extended"], ["Extended", "B"]);
    assert_eq!(codegen.cap_components["A"], ["A"]);
    assert_eq!(codegen.trait_method_names["Slots"], ["second", "first"]);
    let later = crate::ast::Type::Name("LaterActor".into(), vec![]);
    assert_eq!(
        codegen.llvm_type_for(&later),
        Some(BasicTypeEnum::PointerType(
            context.ptr_type(inkwell::AddressSpace::default()),
        )),
        "preseeded actor forward references use the handle ABI",
    );
    assert!(
        codegen.actor_defs.is_empty(),
        "installer never reconstructs ActorDef"
    );
}

#[test]
fn checked_generic_trait_specializations_install_exact_vtables_from_checked_catalog() {
    let source = r#"
        trait Marker<T> { func read() -> i32; }
        impl Marker<i32> for List<i32> { func read() -> i32 { 11 } }
        impl Marker<string> for List<string> { func read() -> i32 { 22 } }
        func main() -> i32 { 0 }
    "#;
    let file =
        crate::parser::Parser::new(crate::lexer::Lexer::new(source).tokenize().expect("lex"))
            .parse_file()
            .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let implementations = program
        .native_declarations()
        .impls
        .values()
        .filter(|implementation| implementation.trait_name == "Marker")
        .collect::<Vec<_>>();
    assert_eq!(
        implementations.len(),
        2,
        "both checked specializations survive"
    );
    assert_ne!(implementations[0].owner, implementations[1].owner);
    assert_ne!(
        implementations[0].vtable_symbol,
        implementations[1].vtable_symbol
    );
    let symbols = implementations
        .iter()
        .map(|implementation| {
            let [method] = implementation.methods.as_slice() else {
                panic!("Marker impl must have exactly one method");
            };
            method.symbol.clone()
        })
        .collect::<Vec<_>>();
    assert_ne!(
        symbols[0], symbols[1],
        "typed impl owners must not alias bodies"
    );

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "checked_generic_marker_vtables");
    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    codegen
        .install_checked_declaration_names(&program)
        .expect("install checked generic impl catalog");
    let function_type = context.i32_type().fn_type(&[], false);
    for symbol in &symbols {
        codegen.module.add_function(symbol, function_type, None);
    }
    codegen.type_impls.clear();
    let anchor = codegen.module.add_function(
        "vtable_anchor",
        context.void_type().fn_type(&[], false),
        None,
    );
    let entry = context.append_basic_block(anchor, "entry");
    codegen.builder.position_at_end(entry);
    codegen
        .compile_vtables()
        .expect("emit vtables from checked impl identities");
    codegen.builder.build_return(None).expect("anchor return");
    codegen.module.verify().expect("valid IR");
    assert!(
        !codegen.vtable_globals.contains_key("List__Marker"),
        "an ambiguous legacy lookup key must not silently choose one specialization",
    );
    for (implementation, method_symbol) in implementations.iter().zip(symbols) {
        assert!(codegen
            .vtable_globals
            .contains_key(&implementation.vtable_symbol));
        let initializer = codegen
            .module
            .get_global(&implementation.vtable_symbol)
            .expect("owner-qualified vtable global")
            .get_initializer()
            .expect("vtable initializer")
            .to_string();
        assert!(
            initializer.contains(&method_symbol),
            "each vtable slot must reference its own checked method body",
        );
    }
    assert!(crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty());
}

#[test]
fn checked_generic_protocol_method_stops_before_compatibility_abi_emission() {
    let source = r#"
        trait Echo<T> {
            func echo(value: T) -> T;
            func echo_local<U>(value: U) -> U;
        }
        type Holder { marker: i32 }
        impl<T> Echo<T> for Holder {
            func echo(value: T) -> T { value }
            func echo_local<U>(value: U) -> U { value }
        }
        func main() -> i32 {
            let holder = Holder { marker: 0 }
            let text = holder.echo("A\0雪")
            let local = holder.echo_local("B\0木")
            println(text.len())
            println(local.len())
            0
        }
    "#;
    let file =
        crate::parser::Parser::new(crate::lexer::Lexer::new(source).tokenize().expect("lex"))
            .parse_file()
            .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "generic_protocol_native_fail_closed");
    let diagnostics = codegen
        .compile_checked(&program)
        .expect_err("generic ProtocolMethod lacks a concrete MIR native ABI");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("NATIVE-GENERIC-PROTOCOL-ABI-001")
    }));
    assert!(codegen.module.get_function("main").is_none());
    for implementation in program.native_declarations().impls.values() {
        for method in &implementation.methods {
            assert!(
                codegen.module.get_function(&method.symbol).is_none(),
                "generic base body '{}' must not be emitted with the erased i64 ABI",
                method.symbol
            );
        }
    }
}

#[test]
fn canonical_mir_dispatches_generic_trait_impls_by_exact_method_owner() {
    let source = r#"
        trait Marker<T> { func read() -> i32; }
        impl Marker<i32> for List<i32> { func read() -> i32 { 11 } }
        impl Marker<i64> for List<i64> { func read() -> i32 { 22 } }
        func main() -> i32 {
            let numbers: List<i32> = [1]
            let wide: List<i64> = [2]
            println(wide.read())
            println(numbers.read())
            0
        }
    "#;
    let file =
        crate::parser::Parser::new(crate::lexer::Lexer::new(source).tokenize().expect("lex"))
            .parse_file()
            .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let synthetic_main = program.flow("Main").expect("parser-synthesized Main");
    assert!(matches!(
        &synthetic_main.origin,
        crate::core::Origin::RuntimeSystem { parent, rule, .. }
            if parent.0 == "function:main" && rule == "progressive.main"
    ));
    assert_eq!(synthetic_main.transitions.len(), 5);
    let implementations = program
        .native_declarations()
        .impls
        .values()
        .filter(|implementation| implementation.trait_name == "Marker")
        .collect::<Vec<_>>();
    assert_eq!(implementations.len(), 2);
    let method_owners = implementations
        .iter()
        .map(|implementation| {
            let [method] = implementation.methods.as_slice() else {
                panic!("Marker impl must have exactly one method");
            };
            method.callable.clone()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        method_owners.len(),
        2,
        "each checked impl has a distinct canonical callable owner"
    );

    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let mir = crate::core::mir::reference::MirProgram::from_checked_program(&program)
        .expect("concrete generic trait impls lower into canonical MIR");
    assert!(
        method_owners
            .iter()
            .all(|owner| mir.functions().contains_key(owner)),
        "every specialized implementation body must have its exact MIR function owner"
    );
    let main_owner = crate::core::NodeId("function:main".into());
    let main_mir = mir.functions().get(&main_owner).expect("main MIR");
    let call_owners = main_mir
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::ProtocolMethod { method, .. },
                ..
            } => Some(crate::core::NodeId(method.as_str().to_owned())),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        call_owners, method_owners,
        "each concrete List receiver must call its checker-selected impl owner"
    );

    let receipt =
        mir.route_receipt(crate::codegen::STATIC_GENERIC_TRAIT_SPECIALIZATION_ROUTE_PROFILE);
    assert_eq!(
        receipt.profile,
        crate::codegen::STATIC_GENERIC_TRAIT_SPECIALIZATION_ROUTE_PROFILE
    );
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&mir)
        .with_route_receipt(&receipt)
        .execute_with_output(&main_owner, &[])
        .expect("reference MIR dispatches both impls");
    assert_eq!(reference.output, "22\n11\n");
    assert_eq!(
        reference.value,
        crate::core::mir::reference::MirRuntimeValue::Int(0)
    );

    let bytecode = crate::interp::bytecode::compile_mir_program_with_route_receipt(&mir, &receipt)
        .expect("compile AST-free bytecode from receipted canonical MIR");
    let mut vm = crate::interp::bytecode::BytecodeVM::new(bytecode);
    vm.enable_stdout_capture();
    assert!(matches!(
        vm.run_value().expect("MIR bytecode execution"),
        crate::interp::Value::Int(0)
    ));
    assert_eq!(vm.take_stdout(), "22\n11\n");

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "mir_generic_trait_call_owners");
    codegen
        .compile_mir_native_with_route_receipt(&mir, &receipt)
        .expect("compile exact generic trait calls through receipted canonical MIR");
    codegen.module.verify().expect("valid IR");
    let native_ir = codegen.emit_ir();
    for owner in &method_owners {
        let symbol = crate::codegen::mir::native_symbol_for_owner(owner)
            .expect("canonical native method symbol");
        assert!(
            native_ir.contains(&symbol),
            "canonical native IR must retain the exact impl method {symbol}"
        );
    }
    if crate::tests::can_link() {
        let native = crate::tests::link_and_observe_canonical_mir(&codegen)
            .expect("execute canonical MIR native output");
        assert_eq!(native.stdout, "22\n11\n");
        assert_eq!(native.stderr, "");
    }

    assert!(crate::codegen::has_static_generic_trait_specialization_candidate(&program));

    let user_flow_source = format!(
        r#"
            flow UserFlow {{
                state Ready {{ value: i32 }}
                transition stay(Ready) -> Ready {{
                    return Ready {{ value: self.value }}
                }}
            }}
            {source}
        "#
    );
    let user_flow_file = crate::parser::Parser::new(
        crate::lexer::Lexer::new(&user_flow_source)
            .tokenize()
            .expect("lex user Flow candidate"),
    )
    .parse_file()
    .expect("parse user Flow candidate");
    let user_flow_program =
        crate::core::check_program(&user_flow_file).expect("check user Flow candidate");
    assert!(
        !crate::codegen::has_static_generic_trait_specialization_candidate(&user_flow_program),
        "the generated Main call-site exemption must not admit a user Flow"
    );

    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let context = Context::create();
    let mut checked_codegen = CodeGenerator::new(&context, "checked_mir_generic_trait_call_owners");
    checked_codegen
        .compile_checked(&program)
        .expect("public checked-program entry selects the canonical trait specialization route");
    checked_codegen
        .module
        .verify()
        .expect("valid checked MIR IR");
    let checked_native_ir = checked_codegen.emit_ir();
    for owner in &method_owners {
        let symbol = crate::codegen::mir::native_symbol_for_owner(owner)
            .expect("canonical native method symbol");
        assert!(
            checked_native_ir.contains(&symbol),
            "public checked-program native output must retain exact impl method {symbol}"
        );
    }
    if crate::tests::can_link() {
        let native = crate::tests::link_and_observe_canonical_mir(&checked_codegen)
            .expect("execute checked-program canonical native output");
        assert_eq!(native.stdout, "22\n11\n");
        assert_eq!(native.stderr, "");
    }
    assert!(crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty());

    // Redirecting a call to the other specialization must fail its receiver
    // ABI check before any MIR consumer can reinterpret the method spelling.
    let mut forged_functions = mir.functions().clone();
    let main = forged_functions
        .get_mut(&main_owner)
        .expect("mutable main MIR");
    let first_call = main
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::Call {
                    callee: crate::core::ir::ResolvedCallee::ProtocolMethod { .. },
                    ..
                }
            )
        })
        .expect("first checked protocol call");
    let current_method = match &first_call.kind {
        crate::core::mir::MirInstructionKind::Call {
            callee: crate::core::ir::ResolvedCallee::ProtocolMethod { method, .. },
            ..
        } => method.as_str(),
        _ => unreachable!(),
    };
    let other_owner = method_owners
        .iter()
        .find(|owner| owner.0 != current_method)
        .expect("other specialized method");
    let crate::core::mir::MirInstructionKind::Call { callee, .. } = &mut first_call.kind else {
        unreachable!();
    };
    let crate::core::ir::ResolvedCallee::ProtocolMethod { method, .. } = callee else {
        unreachable!();
    };
    *method = crate::core::ir::MethodId::new(other_owner.0.clone()).expect("method ID");
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        forged_functions,
        mir.type_catalog().clone(),
    )
    .expect_err("cross-specialization receiver ABI drift must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("protocol method argument 0 TypeDesc disagrees")
    }));
    assert!(crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty());
}

#[test]
fn static_generic_trait_route_uses_owned_string_list_glue_without_legacy_fallback() {
    let source = r#"
        trait Marker<T> { func read() -> i32; }
        impl Marker<i32> for List<i32> { func read() -> i32 { 11 } }
        impl Marker<string> for List<string> { func read() -> i32 { 22 } }
        func main() -> i32 {
            let numbers: List<i32> = [1]
            let words: List<string> = ["x"]
            let answer = words.read() + numbers.read()
            drop(words)
            answer
        }
    "#;
    let file =
        crate::parser::Parser::new(crate::lexer::Lexer::new(source).tokenize().expect("lex"))
            .parse_file()
            .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    assert!(
        crate::codegen::has_static_generic_trait_specialization_candidate(&program),
        "the bounded route admits exact List<i32> and List<string> impl owners"
    );

    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "generic_trait_list_string_owned_mir");
    codegen
        .compile_checked(&program)
        .expect("owned String List glue permits the exact specialized native route");
    codegen
        .module
        .verify()
        .expect("valid generic String List native IR");
    let ir = codegen.emit_ir();
    assert!(ir.contains("mimi_mir_list_new_string"), "{ir}");
    assert!(ir.contains("mimi_mir_list_drop_string"), "{ir}");
    assert!(codegen.module.get_function("main").is_some());
    assert!(crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty());
}

#[test]
fn checked_string_protocol_method_precedes_builtin_spelling_fallback() {
    let source = r#"
        trait TrimOverride { func trim() -> string; }
        impl TrimOverride for string { func trim() -> string { "from-trait" } }
        func choose(value: string) -> string { value.trim() }
        func main() -> i32 { 0 }
    "#;
    let file =
        crate::parser::Parser::new(crate::lexer::Lexer::new(source).tokenize().expect("lex"))
            .parse_file()
            .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let implementation = program
        .native_declarations()
        .impls
        .values()
        .find(|implementation| implementation.trait_name == "TrimOverride")
        .expect("checked TrimOverride impl");
    let [method] = implementation.methods.as_slice() else {
        panic!("TrimOverride must have exactly one checked method");
    };
    let expected_symbol = method.symbol.clone();
    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "checked_string_trait_precedence");
    codegen
        .compile_checked(&program)
        .expect("compile checked String protocol override through the public route");
    codegen.module.verify().expect("valid IR");
    let body = codegen
        .module
        .get_function("choose")
        .expect("choose function")
        .to_string();
    assert!(
        body.contains(&expected_symbol),
        "the exact checker-selected trait body must win over `trim`'s builtin spelling"
    );
    assert!(
        !body.contains("str_trim"),
        "a ProtocolMethod call must not be desugared to the string builtin"
    );
    assert!(crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty());
}

#[test]
fn checked_string_builtin_call_keeps_the_builtin_adapter() {
    let source = r#"
        func choose(value: string) -> string { value.trim() }
        func main() -> i32 { 0 }
    "#;
    let file =
        crate::parser::Parser::new(crate::lexer::Lexer::new(source).tokenize().expect("lex"))
            .parse_file()
            .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "checked_string_builtin_precedence");
    codegen
        .compile_checked(&program)
        .expect("compile checked String builtin through the public route");
    codegen.module.verify().expect("valid IR");
    let body = codegen
        .module
        .get_function("choose")
        .expect("choose function")
        .to_string();
    assert!(
        body.contains("str_trim"),
        "an exact Builtin call must continue through the resolved string builtin lowering"
    );
    assert!(crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty());
}

#[test]
fn checked_impl_bodies_compile_from_callable_owners_without_type_impls() {
    let source = r#"
        trait Measure { func first() -> i32; func second() -> i32; }
        type Counter { value: i32 }
        impl Measure for Counter {
            func second() -> i32 { self.value + 1 }
            func first() -> i32 { self.value }
        }
        func main() -> i32 {
            let counter = Counter { value: 40 };
            counter.first() + counter.second()
        }
    "#;
    let file =
        crate::parser::Parser::new(crate::lexer::Lexer::new(source).tokenize().expect("lex"))
            .parse_file()
            .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let implementation = program
        .native_declarations()
        .impls
        .values()
        .find(|implementation| {
            implementation.trait_name == "Measure" && implementation.type_name == "Counter"
        })
        .expect("checked Measure for Counter impl");
    let first_binding = implementation
        .methods
        .iter()
        .find(|method| method.name == "first")
        .expect("checked first method binding");
    let second_binding = implementation
        .methods
        .iter()
        .find(|method| method.name == "second")
        .expect("checked second method binding");
    assert_eq!(second_binding.ordinal, 0);
    assert_eq!(second_binding.trait_slots, vec![1]);
    assert_eq!(first_binding.ordinal, 1);
    assert_eq!(first_binding.trait_slots, vec![0]);
    assert_ne!(first_binding.callable, second_binding.callable);

    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let mir = crate::core::mir::reference::MirProgram::from_checked_program(&program)
        .expect("checked impl bodies lower into canonical MIR");
    crate::verifier::validate_mir_capabilities(&mir)
        .expect("static protocol-method verifier capability");
    crate::codegen::mir::validate_mir_native(&mir)
        .expect("static protocol-method native capability");

    let first_owner = first_binding.callable.clone();
    let second_owner = second_binding.callable.clone();
    let protocol_owner = implementation.trait_owner.clone();
    let mut expected_targets_by_call = std::collections::BTreeMap::new();
    for (node, site) in program.call_sites() {
        if let Some(crate::core::ir::ResolvedCallee::ProtocolMethod { protocol, method }) =
            site.resolved_callee.as_ref()
        {
            assert_eq!(protocol, &protocol_owner);
            let expected_owner = match site.callee.as_str() {
                "counter.first" => &first_owner,
                "counter.second" => &second_owner,
                other => panic!("unexpected static protocol call spelling '{other}'"),
            };
            assert_eq!(
                method.as_str(),
                expected_owner.0.as_str(),
                "checker must resolve '{}' to its exact impl callable owner",
                site.callee
            );
            assert_eq!(site.node_id, *node);
            expected_targets_by_call
                .insert(node.clone(), (protocol.clone(), expected_owner.clone()));
        }
    }
    assert_eq!(expected_targets_by_call.len(), 2);

    let main_owner = crate::core::NodeId("function:main".into());
    let main_mir = mir.functions().get(&main_owner).expect("main MIR");
    let mir_targets_by_call = main_mir
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| {
            let crate::core::mir::MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::ProtocolMethod { protocol, method },
                ..
            } = &instruction.kind
            else {
                return None;
            };
            let call_node = instruction.id.as_str().strip_prefix("inst:call:")?;
            Some((
                crate::core::NodeId(call_node.to_owned()),
                (
                    protocol.clone(),
                    crate::core::NodeId(method.as_str().to_owned()),
                ),
            ))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        mir_targets_by_call, expected_targets_by_call,
        "canonical MIR must retain each source call's exact checker-selected method owner"
    );

    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "checked_impl_owner_body_mir");
    codegen
        .compile_mir_native(&mir)
        .expect("compile both static methods through the canonical native consumer");
    codegen.module.verify().expect("valid canonical native IR");
    for binding in [first_binding, second_binding] {
        let mir_function = mir
            .functions()
            .get(&binding.callable)
            .expect("exact canonical method owner has a MIR body");
        assert_eq!(
            mir_function.result, binding.signature.result,
            "MIR method result must preserve the checker-owned ABI result"
        );
        let result_desc = mir
            .type_catalog()
            .get(&mir_function.result)
            .expect("canonical method result TypeDesc");
        assert_eq!(
            result_desc.kind,
            crate::core::mir::types::MirTypeKind::Primitive(crate::core::PrimitiveType::I32),
            "both checked method signatures retain the i32 ABI result"
        );

        let symbol = crate::codegen::mir::native_symbol_for_owner(&binding.callable)
            .expect("canonical native method symbol");
        let function = codegen
            .module
            .get_function(&symbol)
            .expect("canonical native method body");
        assert!(function.count_basic_blocks() > 0);
        assert_eq!(
            function.get_type().get_return_type(),
            Some(context.i32_type().as_basic_type_enum()),
            "native LLVM method ABI must not drift from its checked i32 result"
        );
    }
    assert!(crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty());
}

#[test]
fn checked_vtable_uses_native_impl_slot_receipt_without_ast_impl_map() {
    let source = r#"
        trait Slots { func second() -> i32; func first() -> i32; }
        type Item { value: i32 }
        impl Slots for Item {
            func first() -> i32 { self.value }
            func second() -> i32 { self.value + 1 }
        }
        func main() -> i32 { 0 }
    "#;
    let file =
        crate::parser::Parser::new(crate::lexer::Lexer::new(source).tokenize().expect("lex"))
            .parse_file()
            .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let implementation = program
        .native_declarations()
        .impls
        .values()
        .find(|implementation| implementation.trait_name == "Slots")
        .expect("checked Slots impl");
    let second_symbol = &implementation
        .methods
        .iter()
        .find(|method| method.trait_slots == vec![0])
        .expect("second method slot")
        .symbol;
    let first_symbol = &implementation
        .methods
        .iter()
        .find(|method| method.trait_slots == vec![1])
        .expect("first method slot")
        .symbol;
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "checked_native_vtable_receipt");
    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    codegen
        .install_checked_declaration_names(&program)
        .expect("install checked declarations");
    let function_type = context.i32_type().fn_type(&[], false);
    codegen
        .module
        .add_function(second_symbol, function_type, None);
    codegen
        .module
        .add_function(first_symbol, function_type, None);
    codegen.type_impls.clear();
    let anchor = codegen.module.add_function(
        "vtable_anchor",
        context.void_type().fn_type(&[], false),
        None,
    );
    let entry = context.append_basic_block(anchor, "entry");
    codegen.builder.position_at_end(entry);
    codegen
        .compile_vtables()
        .expect("checked vtable installation");
    codegen.builder.build_return(None).expect("anchor return");
    codegen.module.verify().expect("valid vtable IR");
    let vtable_type = codegen
        .vtable_types
        .get("Slots")
        .copied()
        .expect("Slots vtable layout");
    let erased_slot_pointer =
        BasicTypeEnum::PointerType(context.ptr_type(inkwell::AddressSpace::default()));
    assert_eq!(
        vtable_type.get_field_types(),
        vec![erased_slot_pointer, erased_slot_pointer],
        "the native vtable ABI has one erased function pointer per declared trait slot"
    );
    let initializer = codegen
        .module
        .get_global(&implementation.vtable_symbol)
        .expect("vtable global")
        .get_initializer()
        .expect("vtable initializer")
        .to_string();
    let second = initializer
        .find(second_symbol)
        .expect("first declared slot points at second");
    let first = initializer
        .find(first_symbol)
        .expect("second declared slot points at first");
    assert!(second < first, "slot identity is checker-receipt driven");
    assert!(crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty());
}

#[test]
fn checked_actor_mailbox_headers_emit_real_resolved_calls_without_actor_ast_cache() {
    for ty in ["i32", "string", "List<string>", "(i32, bool, string)"] {
        let source = format!(
            r#"
            actor Echo {{ func echo(value: {ty}) -> {ty} {{ value }} }}
            func call_actor(handle: Echo, value: {ty}) -> {ty} {{ handle.echo(value) }}
            func main() -> i32 {{ 0 }}
        "#
        );
        let file =
            crate::parser::Parser::new(crate::lexer::Lexer::new(&source).tokenize().expect("lex"))
                .parse_file()
                .expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let context = Context::create();
        let mut codegen = CodeGenerator::new(&context, "checked_actor_header_call");
        codegen
            .install_checked_declaration_names(&program)
            .expect("install checked declarations");
        assert!(codegen.actor_defs.is_empty());
        assert!(codegen.actor_method_ids.is_empty());
        let owner = program
            .function("call_actor")
            .expect("caller")
            .node_id
            .clone();
        let selected = std::collections::BTreeSet::from([owner]);
        let (count, _) = codegen
            .compile_resolved_subset(&program, &selected)
            .expect("real canonical mailbox caller");
        assert_eq!(count, 1, "typed caller must not defer to AST for {ty}");
        codegen.module.verify().expect("valid LLVM call/packing IR");
        let caller = codegen
            .module
            .get_function("call_actor")
            .expect("caller symbol");
        assert!(caller.to_string().contains("@mimi_actor_call"));
        assert!(
            codegen.actor_defs.is_empty(),
            "emitter must not synthesize ActorDef"
        );
    }
}

#[test]
fn checked_actor_tuple_text_alias_header_uses_transitive_alias_layout() {
    let source = r#"
        type InnerText = string
        type Text = InnerText
        actor Echo {
            func echo(value: (Text, i32)) -> (Text, i32) { value }
        }
    "#;
    let file =
        crate::parser::Parser::new(crate::lexer::Lexer::new(source).tokenize().expect("lex"))
            .parse_file()
            .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let context = Context::create();
    let mut typed_alias_layout = CodeGenerator::new(&context, "typed_alias_tuple_layout");
    typed_alias_layout
        .install_checked_declaration_names(&program)
        .expect("install checked declarations");
    assert!(typed_alias_layout.type_defs.is_empty());
    let alias_type = crate::ast::Type::Name("Text".into(), Vec::new());
    let alias_layout = typed_alias_layout
        .checked_alias_llvm_types
        .get("Text")
        .copied()
        .expect("checker target should materialize the closed String alias layout");
    assert_eq!(
        typed_alias_layout.tuple_field_type_for_actor_values(&alias_type),
        Some(alias_layout),
        "tuple alias lowering must not need the retained TypeDef AST map"
    );

    let mut codegen = CodeGenerator::new(&context, "actor_tuple_text_alias_header");
    codegen
        .install_checked_declaration_names(&program)
        .expect("install checked declarations");
    let actor = crate::core::NodeId("actor:Echo".into());
    let header = codegen
        .checked_actor_mailbox_header(&program, &actor, "echo")
        .expect("checked mailbox header")
        .expect("canonical tuple header");
    assert_eq!(header.parameters.len(), 1);
    let tuple_layout = header.parameters[0].into_struct_type();
    let widened_i32 = crate::codegen::abi::layout::widen_product_field(
        &context,
        BasicTypeEnum::IntType(context.i32_type()),
    );
    assert_eq!(
        tuple_layout.get_field_types(),
        vec![alias_layout, widened_i32],
        "the checker-owned tuple ABI must retain the transitively resolved String alias"
    );
    assert_eq!(
        header.result, header.parameters[0],
        "the checked mailbox result and argument share the tuple alias ABI"
    );
}

#[test]
fn checked_actor_mailbox_header_preserves_order_and_explicit_abi_boundaries() {
    let source = r#"
        actor Echo {
            func second(value: i32) -> i32 { value }
            func first(value: Option<List<i32>>) -> Option<List<i32>> { value }
            func implicit() { }
            func explicit_unit() -> () { }
        }
        func main() -> i32 { 0 }
    "#;
    let file =
        crate::parser::Parser::new(crate::lexer::Lexer::new(source).tokenize().expect("lex"))
            .parse_file()
            .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "mailbox_header_boundary");
    codegen
        .install_checked_declaration_names(&program)
        .expect("install checked declarations");
    let actor = crate::core::NodeId("actor:Echo".into());
    let second = codegen
        .checked_actor_mailbox_header(&program, &actor, "second")
        .expect("header")
        .expect("closed primitive header");
    assert_eq!(
        second.method_id, 0,
        "declaration order is the runtime dispatch ordinal"
    );
    assert_eq!(
        second.parameters,
        [BasicTypeEnum::IntType(context.i32_type())]
    );
    let implicit = codegen
        .checked_actor_mailbox_header(&program, &actor, "implicit")
        .expect("header")
        .expect("implicit sentinel");
    assert_eq!(implicit.result, BasicTypeEnum::IntType(context.i64_type()));
    assert_eq!(implicit.method_id, 2);
    let unit = codegen
        .checked_actor_mailbox_header(&program, &actor, "explicit_unit")
        .expect("header")
        .expect("unit header");
    assert_eq!(unit.result, BasicTypeEnum::IntType(context.i64_type()));
    assert!(
        codegen
            .checked_actor_mailbox_header(&program, &actor, "first")
            .expect("known unmigrated ABI")
            .is_none(),
        "Option<List> preserves compatibility disposition"
    );
    assert!(codegen
        .checked_actor_mailbox_header(&program, &actor, "absent")
        .is_err());

    let mut incomplete = CodeGenerator::new(&context, "mailbox_header_missing_catalog_actor");
    incomplete
        .install_checked_declaration_names(&program)
        .expect("install checked declarations");
    incomplete
        .checked_native_declarations
        .as_mut()
        .expect("production declaration catalog")
        .actors
        .remove(&actor);
    assert!(
        incomplete
            .checked_actor_mailbox_header(&program, &actor, "second")
            .is_err(),
        "an installed but incomplete native Actor schema must fail closed"
    );
}
