//! Unit tests for the low-level LLVM builder helpers introduced in v0.28.8.
//!
//! These tests verify that the thin wrappers around inkwell keep the same
//! semantics and produce valid in-memory LLVM instructions. They are cheap
//! (no object compilation / linking) and fast to run.

use super::*;
use crate::error::MimiResult;
use inkwell::context::Context;
use inkwell::types::BasicType;
use inkwell::values::BasicValue;
use std::collections::HashMap;

// The production fifth pass is canonical-only. Keep the old surface emitter
// reachable solely from this test-module adapter so low-level builder tests
// can continue to exercise compatibility code without adding a production
// `compile_func_legacy` call edge.
impl<'ctx> CodeGenerator<'ctx> {
    pub(crate) fn compile_legacy_test_function(
        &mut self,
        function: &FuncDef,
    ) -> crate::error::MimiResult<()> {
        self.compile_func_legacy(function)
    }

    pub(crate) fn compile_file_with_resolved(
        &mut self,
        program: &crate::core::CheckedProgram,
        eligible: Option<&std::collections::BTreeSet<crate::core::NodeId>>,
    ) -> crate::error::MimiResult<()> {
        self.shared_dynamic_any_preflight_passed = false;
        self.install_checked_declaration_names(program)?;
        self.compile_file_inner(
            program.legacy_body_file(crate::core::LegacyBodyConsumer::CodegenLegacyRemainder),
            Some((program, eligible)),
            true,
        )
    }

    // The following impl-body adapters are deliberately test-only. They keep
    // compatibility-registry tests able to exercise the retired surface AST
    // emitter without leaving a production call edge from registry/funcs.rs.
    pub(in crate::codegen) fn compile_impl_methods(&mut self) -> MimiResult<()> {
        for (type_name, trait_impls) in self.type_impls.clone() {
            for (trait_name, methods) in &trait_impls {
                for method in methods {
                    let mangled = format!("{}__{}__{}", type_name, trait_name, method.name);
                    let type_args_for_self = self
                        .impl_type_args
                        .get(&type_name)
                        .cloned()
                        .unwrap_or_default();
                    let self_type_name = if type_args_for_self.is_empty() {
                        Type::Name(type_name.clone(), vec![])
                    } else {
                        Type::Name(type_name.clone(), type_args_for_self.clone())
                    };
                    let self_ty = match type_name.as_str() {
                        "i32" | "i64" | "f64" | "bool" | "Record" | "Map" | "Any" => self_type_name,
                        _ => Type::Ref(None, Box::new(self_type_name)),
                    };
                    self.compile_impl_method_body_for_test(
                        method,
                        &mangled,
                        type_args_for_self,
                        self_ty,
                    )?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn compile_impl_method_body_for_test(
        &mut self,
        method: &FuncDef,
        symbol: &str,
        type_args_for_self: Vec<Type>,
        self_ty: Type,
    ) -> MimiResult<()> {
        let impl_method = self.impl_method_with_self_type(method, symbol, self_ty);
        let saved_type_map = self.type_map.clone();
        if !type_args_for_self.is_empty() {
            let mut identity_map: HashMap<String, Type> = HashMap::new();
            for argument in &type_args_for_self {
                if let Type::Name(name, _) = argument.unlocated() {
                    identity_map.insert(name.clone(), argument.clone());
                }
            }
            if !identity_map.is_empty() && self.type_map.is_empty() {
                self.type_map.extend(identity_map);
            }
        }
        let result = self.compile_func_legacy(&impl_method);
        self.type_map = saved_type_map;
        result
    }

    pub(crate) fn compile_checked_impl_method_instance_legacy_for_test(
        &mut self,
        instance: &FuncDef,
        type_map: &HashMap<String, Type>,
    ) -> MimiResult<()> {
        self.compile_generic_func(instance, type_map)
    }
}

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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
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
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "exact S8 direct native entry must not access retained legacy bodies"
    );
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
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
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "exact scalar collection direct native entry must not access retained legacy bodies"
    );
}

#[test]
fn compile_checked_routes_closed_map_any_owner_v1_without_legacy_access() {
    let profiles = [
        (
            "i32",
            r#"
        func main() -> i32 {
            let root = map_new()
            let updated = map_set(root, "answer", 42)
            let size = map_size(updated)
            println(size)
            drop(updated)
            0
        }
    "#,
            "mimi_mir_map_any_owner_v1_set_i32",
        ),
        (
            "string",
            r#"
        func string_map_size() -> i32 {
            let root = map_new()
            let updated = map_set(root, "answer", "a\0b雪")
            let size = map_size(updated)
            println(size)
            drop(updated)
            size
        }
        func main() -> i32 {
            let size = string_map_size()
            0
        }
            "#,
            "mimi_mir_map_any_owner_v1_set_string",
        ),
        (
            "remove",
            r#"
        func main() -> i32 {
            let root = map_new()
            let inserted = map_set(root, "remove", "a\0b雪")
            let with_keep = map_set(inserted, "keep", "stay")
            let removed = map_remove(with_keep, "remove")
            let size = map_size(removed)
            drop(removed)
            size
        }
    "#,
            "mimi_mir_map_any_owner_v1_remove",
        ),
        (
            "contains",
            r#"
        func main() -> i32 {
            let root = map_new()
            let inserted = map_set(root, "answer", 42)
            let present = has_key(inserted, "answer")
            let size = map_size(inserted)
            println(present)
            drop(inserted)
            size
        }
    "#,
            "mimi_mir_map_any_owner_v1_contains",
        ),
    ];
    for (profile, source, set_runtime) in profiles {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let admission = crate::core::mir::classify_canonical_mir_route_admission(&program);
        assert_eq!(
            admission.map_root,
            crate::core::mir::MapRootAdmission::OutsideProfile,
            "{profile} must not be admitted under the raw MapRoot identity: {admission:#?}"
        );
        assert_eq!(
            admission.map_any_owner,
            crate::core::mir::MapAnyOwnerAdmission::CompleteCoverage,
            "{profile} must have a complete independent MapAnyOwner v1 receipt: {admission:#?}"
        );
        assert!(crate::core::mir::CanonicalMirRouteProfile::MapAnyOwnerV1.is_admitted(admission));

        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let context = Context::create();
        let mut codegen = CodeGenerator::new(
            &context,
            &format!("map_any_owner_v1_{profile}_direct_route"),
        );
        codegen
            .compile_checked(&program)
            .expect("direct CheckedProgram native entry must route MapAnyOwner v1 through MIR");
        codegen
            .module
            .verify()
            .expect("valid direct MapAnyOwner v1 module");
        let native_ir = codegen.module.print_to_string().to_string();
        let result_abi = if profile == "contains" {
            "call i32"
        } else {
            "call i64"
        };
        assert!(
            native_ir.contains(&format!("{result_abi} @{set_runtime}")),
            "direct {profile} native route must call {set_runtime}; emitted LLVM IR:\n{native_ir}"
        );
        assert!(
            !native_ir.contains("call i64 @mimi_mir_map_root_"),
            "direct {profile} native route must not call the raw MapRoot runtime"
        );
        for runtime in [
            "mimi_mir_map_any_owner_v1_new",
            set_runtime,
            "mimi_mir_map_any_owner_v1_size",
            "mimi_mir_map_any_owner_v1_drop",
        ] {
            assert!(codegen.module.get_function(runtime).is_some(), "{runtime}");
        }
        if profile == "remove" {
            assert!(codegen
                .module
                .get_function("mimi_mir_map_any_owner_v1_remove")
                .is_some());
        }
        assert!(
            crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
            "direct {profile} MapAnyOwner v1 native compilation must not access retained legacy bodies"
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
    crate::core::CheckedProgram::reset_test_legacy_body_access();
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
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
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
        admission.ordinary_map_owner_lifecycle_i32_string_v1,
        crate::core::mir::OrdinaryMapOwnerLifecycleI32StringV1Admission::OutsideProfile
    );
    assert!(!admission.map_any_owner.is_candidate());

    crate::core::CheckedProgram::reset_test_legacy_body_access();
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
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "ordinary Map values native compilation must not access retained legacy bodies"
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
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
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn compile_checked_rejects_unmigrated_generic_body_without_legacy_owner() {
    let source = r#"
        func choose<T>(left: T, right: T) -> T { left }
        func main() -> i64 { choose(42, 43) }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "legacy_generic_owner");
    let error = codegen
        .compile_checked(&program)
        .expect_err("reachable generic compatibility must fail closed");
    assert_eq!(error[0].code.as_deref(), Some("MIR-COVERAGE-001"));
    assert!(error[0].message.contains("NITS"));
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "a rejected generic route must not enter the legacy body"
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
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
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "a rejected generic route must not read a legacy body"
    );
}

#[test]
fn codegen_shares_checker_ast_snapshot_for_progressive_comptime() {
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "shared_progressive_comptime");
    codegen
        .compile_file_with_resolved(&program, None)
        .expect("test-only compatibility adapter should retain its comptime input");
    let codegen_file = codegen
        .comptime_file
        .as_ref()
        .expect("legacy codegen should retain its comptime input");

    let checker_file =
        program.legacy_body_file(crate::core::LegacyBodyConsumer::CodegenLegacyRemainder);
    assert!(checker_file.implicit_single);
    assert!(
        std::sync::Arc::ptr_eq(codegen_file, &checker_file),
        "Codegen should share the immutable checked AST allocation"
    );
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
        "comptime fold should keep its codegen-normalized evaluation context"
    );
    assert_eq!(
        crate::core::CheckedProgram::test_legacy_body_access(),
        vec![
            crate::core::LegacyBodyConsumer::CodegenLegacyRemainder,
            crate::core::LegacyBodyConsumer::CodegenLegacyRemainder,
        ],
        "the test's pointer comparison may only cross the same tagged owner"
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "canonical_generic_identity");
    codegen
        .compile_checked(&program)
        .expect("bounded scalar generic identity must use the canonical MIR route");
    assert!(
        codegen.module.get_function("main").is_some(),
        "canonical generic identity route must emit main"
    );
    assert_eq!(
        crate::core::CheckedProgram::test_legacy_body_access(),
        Vec::<crate::core::LegacyBodyConsumer>::new(),
        "the admitted generic identity must not access a legacy body"
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "canonical_generic_identity_i64");
    codegen
        .compile_checked(&program)
        .expect("bounded i64 scalar generic identity must use the canonical MIR route");
    assert!(
        codegen.module.get_function("main").is_some(),
        "canonical i64 generic identity route must emit main"
    );
    assert_eq!(
        crate::core::CheckedProgram::test_legacy_body_access(),
        Vec::<crate::core::LegacyBodyConsumer>::new(),
        "the admitted i64 generic identity must not access a legacy body"
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
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "m3_canonical_flow_entry");
    codegen
        .compile_checked(&program)
        .expect("direct native entry must select the canonical retry profile");
    assert!(codegen.module.get_function("main").is_some());
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "recoverable Flow direct native entry must not read retained legacy bodies"
    );
}

#[test]
fn direct_native_entry_routes_recoverable_string_state_through_canonical_mir() {
    let source = include_str!("../../tests/fixtures/mir_m3_flow_retry_string_state.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "m3_canonical_string_flow_entry");
    codegen
        .compile_checked(&program)
        .expect("direct native entry must select the canonical String retry profile");
    assert!(codegen.module.get_function("main").is_some());
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "recoverable String Flow direct native entry must not read retained legacy bodies"
    );
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
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "m3_canonical_multifield_flow_entry");
    codegen
        .compile_checked(&program)
        .expect("direct native entry must select the canonical multifield Flow profile");
    assert!(codegen.module.get_function("main").is_some());
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "multifield Flow direct native entry must not access retained legacy bodies"
    );
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
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "r6_canonical_f64_flow_entry");
    codegen
        .compile_checked(&program)
        .expect("direct native entry must select canonical f64 Flow source receipt");
    assert!(codegen.module.get_function("main").is_some());
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "f64 Flow direct native entry must not access retained legacy bodies"
    );
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
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
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "a nested scalar FFI route must not touch the legacy body"
    );
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "nested_unit_result_rejected");
    let errors = codegen
        .compile_checked(&program)
        .expect_err("direct native entry must reject a nested Unit value");
    assert!(
        !errors.is_empty(),
        "the unsupported MIR ABI must be explicit"
    );
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "direct native rejection must not retry the legacy body"
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "s23_scalar_collection_materialization_error");
    let errors = codegen
        .compile_checked(&program)
        .expect_err("nested-assign construction failure must fail closed");
    assert!(errors.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some("MIR-COVERAGE-001")
            || diagnostic.code.as_deref() == Some("MIR-LOWERING-001")
    }));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "s15_flat_copy_record_route");
    codegen
        .compile_checked(&program)
        .expect("flat Copy record must use the canonical MIR native consumer");
    assert!(codegen.module.get_function("main").is_some());
    assert!(codegen.resolved_failed_functions().is_empty());
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "direct flat Copy record native route must not access retained legacy bodies"
    );
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "flat_record_protocol_default_route");
    codegen
        .compile_checked(&program)
        .expect("direct native API must use canonical protocol-method MIR");
    assert!(codegen.module.get_function("main").is_some());
    assert!(codegen.resolved_failed_functions().is_empty());
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "direct protocol-method native route must not access retained legacy bodies"
    );
}

#[test]
fn compile_checked_rejects_dynamic_protocol_method_without_legacy_owner() {
    let source = include_str!("../../tests/fixtures/mir_protocol_method_dyn.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "dyn_protocol_rejected");
    let errors = codegen
        .compile_checked(&program)
        .expect_err("dynamic protocol dispatch must fail closed before legacy codegen");
    assert!(errors.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    }));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn compile_checked_routes_multi_transition_flow_through_canonical_mir() {
    let source = include_str!("../../tests/real_world/flow_multi_transition_selfloops.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "flow_transition_canonical_owner");
    codegen
        .compile_checked(&program)
        .expect("multi-transition Flow must use canonical MIR native codegen");
    assert!(
        codegen.module.get_function("main").is_some(),
        "canonical multi-transition Flow must emit main"
    );
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "canonical multi-transition Flow must not access retained legacy bodies"
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "trait_impl_rejected");
    let errors = codegen
        .compile_checked(&program)
        .expect_err("trait implementation specialization must fail closed");
    assert!(errors.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    }));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn compile_checked_rejects_captured_nested_helper_without_legacy_owner() {
    let source = r#"
        func main() -> i32 {
            let offset = 1
            func nested_increment(value: i32) -> i32 { value + offset }
            nested_increment(41)
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "nested_helper_legacy_owner");
    let error = codegen
        .compile_checked(&program)
        .expect_err("captured nested helper must fail closed before legacy codegen");
    assert!(
        error
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("MIR-COVERAGE-001")),
        "captured nested helper coverage diagnostic: {error:?}"
    );
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "nested coverage rejection must not enter a legacy body"
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
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
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
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
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "export_body_precedes_map_any_owner");
    let error = codegen
        .compile_checked(&program)
        .expect_err("mixed scalar export and MapAnyOwner must fail closed");
    assert!(error.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    }));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
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
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
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
    assert_eq!(
        crate::core::CheckedProgram::test_legacy_body_access(),
        Vec::<crate::core::LegacyBodyConsumer>::new(),
        "closed Actor compilation must not read a legacy body"
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let errors = CodeGenerator::new(&context, "actor_body_rejected")
        .compile_checked(&program)
        .expect_err("contract-bearing Actor must fail closed outside ScalarActor");
    assert!(errors.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    }));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let errors = CodeGenerator::new(&context, "map_any_borrowed_rejected")
        .compile_checked(&program)
        .expect_err("borrowed Map/Any access must fail closed outside its receipt");
    assert!(errors.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    }));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn compile_checked_map_string_copy_keeps_typed_ingress_and_checks_failure() {
    let source = r#"
        func main() -> i32 {
            let original = map_new()
            let map = map_set(original, "k", "owned text")
            println(to_json(map))
            0
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "map_string_owner_ingress");
    codegen
        .compile_checked(&program)
        .expect("compile typed String ingress");
    codegen
        .module
        .verify()
        .expect("verify typed String ingress IR");
    let ir = codegen.module.print_to_string().to_string();
    assert!(ir.contains("call i32 @mimi_map_set_string_copy("), "{ir}");
    assert!(ir.contains("map_string_copy_succeeded"), "{ir}");
    assert!(ir.contains("Map String insertion failed"), "{ir}");
    assert!(!ir.contains("call i64 @mimi_any_string_clone("), "{ir}");
}

#[test]
fn legacy_map_set_uses_source_string_type_instead_of_pointer_integer_shape() {
    for (value, expects_string) in [
        (r#""A\0雪""#, true),
        ("aliased", true),
        ("message()", true),
        ("(worker, 16 as i64)", false),
    ] {
        // Export wrappers retain a real legacy body. The tuple has exactly
        // the String LLVM layout, but its pointer is an opaque actor handle.
        // Only inspect generated IR: do not execute a potential pointer read.
        let source = format!(
            r#"
            type InnerText = string
            type Text = InnerText
            func message() -> Text {{ "A\0雪" }}
            actor StringShapeWorker {{ func ping() -> i32 {{ 1 }} }}
            extern "C" func exercise() -> i32 {{
                let worker = StringShapeWorker.spawn()
                let aliased: Text = message()
                let root = map_new()
                let saved = map_set(root, "payload", {value})
                println(map_size(saved))
                0
            }}
            func main() -> i32 {{ exercise() }}
            "#
        );
        let tokens = crate::lexer::Lexer::new(&source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let program = crate::core::check_program(&file).expect("check");
        let context = Context::create();
        let mut codegen = CodeGenerator::new(&context, "legacy_map_static_string");
        codegen
            .compile_file_with_resolved(&program, None)
            .expect("compile legacy body through the test-only adapter");
        codegen.module.verify().expect("verify legacy Map IR");
        let ir = codegen.module.print_to_string().to_string();
        assert_eq!(
            ir.contains("call i32 @mimi_map_set_string_copy("),
            expects_string,
            "only a source-typed String can enter the owning ABI: {ir}"
        );
        assert!(!ir.contains("call i64 @mimi_any_string_clone("), "{ir}");
        if expects_string {
            assert!(ir.contains("map_string_copy_succeeded"), "{ir}");
            assert!(
                ir.contains("i64 5"),
                "preserve the exact UTF-8/NUL byte length: {ir}"
            );
        } else {
            assert!(
                ir.contains("map_set_struct_h"),
                "tuple must keep its product ABI: {ir}"
            );
            assert!(ir.contains("call void @mimi_map_set("), "{ir}");
        }
    }
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
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
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "the hard gate must run before touching legacy bodies"
    );

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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "unreachable_dynamic_any_unpack");
    codegen
        .compile_checked(&program)
        .expect("unreachable std::maps accessors must not block an unrelated scalar entry");
    assert!(codegen.module.get_function("main").is_some());

    std::fs::remove_dir_all(dir).expect("remove temporary Map/Any fixture directory");
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
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
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "the exported-body gate must run before the legacy export wrapper is emitted"
    );

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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
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
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "shared-library host-entry rejection must precede any body emission"
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "s30_option_string_default_route");
    codegen
        .compile_checked(&program)
        .expect("direct native entry must use the canonical Option<string> consumer");
    assert!(codegen.module.get_function("main").is_some());
    assert!(codegen.resolved_failed_functions().is_empty());
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "direct Option<string> native route must not access retained legacy bodies"
    );
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

        crate::core::CheckedProgram::reset_test_legacy_body_access();
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
        assert!(
            crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
            "{} direct native API must not access retained legacy bodies",
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

        crate::core::CheckedProgram::reset_test_legacy_body_access();
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
        assert!(
            crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
            "{name} must not enter the legacy code generator"
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

        crate::core::CheckedProgram::reset_test_legacy_body_access();
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
        assert!(
            crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
            "{} direct native API must not access retained legacy bodies",
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
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    codegen
        .compile_checked(&program)
        .expect("direct native entry must use the canonical nested Option tuple consumer");
    assert!(codegen.module.get_function("main").is_some());
    assert!(codegen.resolved_failed_functions().is_empty());
    assert!(
        crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
        "direct nested Option tuple native route must not access retained legacy bodies"
    );
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
    crate::core::CheckedProgram::reset_test_legacy_body_access();
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
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn checked_trait_slot_order_drives_legacy_vtable_without_trait_ast_cache() {
    let source = r#"
        trait Slots { func second() -> i32; func first() -> i32; }
        type Item { value: i32 }
        impl Slots for Item {
            func first() -> i32 { self.value }
            func second() -> i32 { self.value + 1 }
        }
        func main() -> i32 { let item = Item { value: 41 }; item.second() }
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
    let mut codegen = CodeGenerator::new(&context, "checked_slots");
    codegen
        .compile_file_with_resolved(&program, None)
        .expect("compatibility compilation");
    codegen.module.verify().expect("valid IR");
    let initializer = codegen
        .module
        .get_global(&implementation.vtable_symbol)
        .expect("vtable")
        .get_initializer()
        .expect("initializer")
        .to_string();
    let second = initializer.find(second_symbol).expect("second slot");
    let first = initializer.find(first_symbol).expect("first slot");
    assert!(
        second < first,
        "trait declaration order controls the ABI, not impl order"
    );
}

#[test]
fn checked_generic_trait_specializations_keep_distinct_method_and_vtable_symbols() {
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
    let mut codegen = CodeGenerator::new(&context, "checked_generic_marker");
    codegen
        .compile_file_with_resolved(&program, None)
        .expect("compile checked generic specializations");
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

    crate::core::CheckedProgram::reset_test_legacy_body_access();
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
    let user_flow_program = crate::core::check_program(&user_flow_file)
        .expect("check user Flow candidate");
    assert!(
        !crate::codegen::has_static_generic_trait_specialization_candidate(&user_flow_program),
        "the generated Main call-site exemption must not admit a user Flow"
    );

    crate::core::CheckedProgram::reset_test_legacy_body_access();
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
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
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
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
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
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "checked_string_trait_precedence");
    codegen
        .compile_file_with_resolved(&program, None)
        .expect("compile exact checked string method");
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
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "checked_string_builtin_precedence");
    codegen
        .compile_file_with_resolved(&program, None)
        .expect("compile exact builtin string call");
    codegen.module.verify().expect("valid IR");
    let body = codegen
        .module
        .get_function("choose")
        .expect("choose function")
        .to_string();
    assert!(
        body.contains("str_trim"),
        "an exact Builtin call must continue through the string builtin adapter"
    );
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
    let mut body_file = (*program
        .legacy_body_file(crate::core::LegacyBodyConsumer::CodegenLegacyRemainder))
    .clone();
    let mut reordered_impl_body = false;
    for item in &mut body_file.items {
        if let crate::ast::Item::Impl(implementation) = item {
            implementation.methods.swap(0, 1);
            reordered_impl_body = true;
        }
    }
    assert!(
        reordered_impl_body,
        "test carries implementation body payloads"
    );
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "checked_impl_owner_body");
    codegen
        .install_checked_declaration_names(&program)
        .expect("install checked declarations");
    let expected_targets = program
        .call_sites()
        .values()
        .filter_map(|site| match site.resolved_callee.as_ref()? {
            crate::core::ir::ResolvedCallee::ProtocolMethod { protocol, method } => Some((
                protocol.clone(),
                crate::core::NodeId(method.as_str().into()),
            )),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    let installed_targets = codegen
        .checked_trait_method_dispatch
        .call_targets
        .values()
        .flatten()
        .map(|target| (target.key.trait_owner.clone(), target.key.callable.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(expected_targets.len(), 2);
    assert_eq!(installed_targets, expected_targets);
    assert_eq!(codegen.checked_trait_method_dispatch.call_targets.len(), 2);
    assert_eq!(
        codegen
            .checked_trait_method_dispatch
            .call_targets
            .values()
            .map(Vec::len)
            .sum::<usize>(),
        2,
        "each checker-selected static call retains one exact metadata receipt"
    );
    for (node, site) in program.call_sites() {
        if matches!(
            site.resolved_callee.as_ref(),
            Some(crate::core::ir::ResolvedCallee::ProtocolMethod { .. })
        ) {
            let span = site.origin.user_span();
            let key = (
                span.source_id.raw(),
                span.start_line,
                span.start_col,
                span.end_line,
                span.end_col,
            );
            assert!(
                codegen
                    .checked_trait_method_dispatch
                    .call_targets
                    .contains_key(&key),
                "checked call node '{}' is indexed at its source span",
                node.0
            );
        }
    }
    for item in &body_file.items {
        if let crate::ast::Item::Type(definition) = item {
            codegen
                .register_type_def(definition)
                .expect("register type");
        }
    }
    codegen
        .install_checked_impl_method_bodies(&body_file, &program)
        .expect("owner-indexed body payloads");
    assert!(codegen.type_impls.is_empty());
    codegen
        .compile_checked_impl_methods(&program)
        .expect("compile from native method bindings");
    let checked_methods = program
        .native_declarations()
        .impls
        .values()
        .flat_map(|implementation| implementation.methods.iter())
        .collect::<Vec<_>>();
    let first_binding = checked_methods
        .iter()
        .find(|method| method.name == "first")
        .expect("checked first method binding");
    let second_binding = checked_methods
        .iter()
        .find(|method| method.name == "second")
        .expect("checked second method binding");
    let function = codegen
        .module
        .get_function(&first_binding.symbol)
        .expect("canonical checked first method symbol");
    assert!(function.count_basic_blocks() > 0);
    assert!(
        codegen
            .module
            .get_function(&second_binding.symbol)
            .expect("canonical checked second method symbol")
            .count_basic_blocks()
            > 0
    );
    let first_owner = first_binding.callable.clone();
    let first_call_site = program
        .call_sites()
        .values()
        .find(|site| {
            matches!(
                site.resolved_callee.as_ref(),
                Some(crate::core::ir::ResolvedCallee::ProtocolMethod { method, .. })
                    if method.as_str() == first_owner.0.as_str()
            )
        })
        .expect("resolved first method callsite");
    let first_call_meta = crate::ast::AstNodeMeta::inherited(
        first_call_site.origin.user_span(),
        crate::ast::AstOrigin::User,
    );
    assert_eq!(
        codegen.infer_impl_method_return_type("Counter", "first", Some(first_call_meta)),
        "i32",
        "call result typing reads the native signature, not the body AST"
    );
    codegen
        .checked_impl_method_bodies
        .get_mut(&first_owner)
        .expect("cached first method body")
        .0
        .ret = Some(crate::ast::Type::Name("string".into(), Vec::new()));
    assert_eq!(
        codegen.infer_impl_method_return_type("Counter", "first", Some(first_call_meta)),
        "i32",
        "changing the executable AST payload cannot rewrite the checked ABI result"
    );
}

#[test]
fn checked_dyn_trait_dispatch_uses_the_canonical_trait_slot_signature() {
    let source = r#"
        trait Read { func read() -> i32; }
        type Counter { value: i32 }
        impl Read for Counter { func read() -> i32 { self.value } }
        func read_dynamic(value: dyn Read) -> i32 { value.read() }
        func main() -> i32 {
            let counter = Counter { value: 41 };
            let dynamic: dyn Read = counter;
            read_dynamic(dynamic)
        }
    "#;
    let file =
        crate::parser::Parser::new(crate::lexer::Lexer::new(source).tokenize().expect("lex"))
            .parse_file()
            .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "checked_dyn_trait_slot_abi");
    codegen
        .compile_file_with_resolved(&program, None)
        .expect("compile canonical dynamic trait ABI");
    codegen
        .module
        .verify()
        .expect("valid dynamic trait call IR");
    let body = codegen
        .module
        .get_function("read_dynamic")
        .expect("dynamic method caller")
        .to_string();
    assert!(body.contains("dyn_call"), "call uses the checked slot ABI");
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
        crate::core::CheckedProgram::reset_test_legacy_body_access();
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
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
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

#[test]
fn static_generic_trait_route_keeps_list_string_outside_the_mir_glue_contract() {
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
    let file = crate::parser::Parser::new(
        crate::lexer::Lexer::new(source).tokenize().expect("lex"),
    )
    .parse_file()
    .expect("parse");
    let program = crate::core::check_program(&file).expect("check");
    assert!(
        !crate::codegen::has_static_generic_trait_specialization_candidate(&program),
        "List<i32> + List<string> must stay outside the bounded MIR candidate"
    );

    let excluded_sources = program
        .source_registry()
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<std::collections::HashSet<_>>();
    let error = crate::core::mir::reference::MirProgram::from_checked_program_excluding_sources(
        &program,
        &excluded_sources,
    )
    .expect_err("List<string> must stay outside the canonical List glue contract");
    let message = format!("{error:?}");
    assert!(
        message.contains("outside the canonical Copy scalar")
            || message.contains("List<string>"),
        "unexpected List<string> fail-closed diagnostic: {message}"
    );

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "generic_trait_list_string_fail_closed");
    let diagnostics = codegen
        .compile_checked(&program)
        .expect_err("unsupported List<string> generic-trait MIR must not fall back to legacy");
    assert!(!diagnostics.is_empty(), "unsupported trait route fails closed");
    assert!(codegen.module.get_function("main").is_none());
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    assert!(crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty());
}
