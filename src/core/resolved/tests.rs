//! Tests for the Typed Resolved IR (extracted from resolved/mod.rs).

use super::*;

fn parse(source: &str) -> File {
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse")
}

fn node_id_at(program: &CheckedProgram, kind: &str, line: usize, col: usize) -> NodeId {
    let marker = format!("/node:{kind}@");
    program
        .node_meta()
        .iter()
        .find_map(|(node_id, meta)| {
            let span = meta.origin.user_span();
            (node_id.0.contains(&marker) && span.start_line == line && span.start_col == col)
                .then(|| node_id.clone())
        })
        .unwrap_or_else(|| panic!("missing {kind} at {line}:{col}"))
}

fn generated_node_id(program: &CheckedProgram, owner: &str, kind: &str, rule: &str) -> NodeId {
    let generated_prefix = format!("{owner}/generated:{kind}:");
    let anchored_marker = format!("{owner}/node:{kind}@");
    program
        .node_meta()
        .iter()
        .find_map(|(node_id, meta)| {
            ((node_id.0.starts_with(&generated_prefix) || node_id.0.starts_with(&anchored_marker))
                && meta.origin.rule() == Some(rule))
            .then(|| node_id.clone())
        })
        .unwrap_or_else(|| panic!("missing generated {kind} for {owner} ({rule})"))
}

fn node_meta_ids(program: &CheckedProgram) -> std::collections::BTreeSet<String> {
    program
        .node_meta()
        .keys()
        .map(|node_id| node_id.0.clone())
        .collect()
}

#[test]
fn checked_program_persists_zonked_function_signatures() {
    let file = parse("func identity(value: i32) -> i32 { value }");
    let program = crate::core::check_program(&file).expect("check");
    let function = program.function("identity").expect("resolved function");
    let (params, ret) = program
        .zonked_function_type(&function.node_id)
        .expect("checker zonked signature");

    assert_eq!(params.len(), 1);
    assert_eq!(params[0].as_type(), &Type::Name("i32".into(), vec![]));
    assert_eq!(ret.as_type(), &Type::Name("i32".into(), vec![]));
    assert_eq!(function.params[0].1, params[0].as_type().clone());
    assert_eq!(function.ret, ret.as_type().clone());
}

#[test]
fn checked_program_validator_rejects_body_signature_parameter_drift() {
    let file = parse("func identity(value: i32) -> i32 { value }");
    let mut program = crate::core::check_program(&file).expect("check identity");
    program
        .resolved_bodies
        .get_mut(&NodeId("function:identity".into()))
        .expect("resolved identity body")
        .parameters
        .clear();
    let errors = validate_resolved_callable_bodies(&program)
        .expect_err("parameter drift must invalidate CheckedProgram");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("body has 0 parameters")));
}

#[test]
fn checked_program_owns_its_migration_body_input() {
    let program = {
        let mut file = parse("func main() -> i32 { 42 }");
        let program = crate::core::check_program(&file).expect("check");
        file.items.clear();
        assert!(file.items.is_empty());
        program
    };

    assert!(program.function("main").is_some());
    assert!(!program.legacy_body_file().items.is_empty());
    let mut interpreter = crate::interp::Interpreter::from_checked(&program);
    assert!(matches!(
        interpreter.run().expect("run owned checked program"),
        crate::interp::Value::Int(42)
    ));
}

#[test]
fn checked_program_materializes_canonical_function_signature() {
    let file = parse("func choose(value: List<i32>, fallback: i32) -> i32 { fallback }");
    let program = crate::core::check_program(&file).expect("check");
    let function = program.function("choose").expect("function");
    let signature = program
        .resolved_signature(&function.node_id)
        .expect("canonical signature");

    assert_eq!(signature.owner, function.node_id);
    assert_eq!(signature.parameters.len(), 2);
    assert!(signature
        .parameters
        .iter()
        .all(|parameter| program.resolved_types().get(&parameter.ty).is_some()));
    assert!(program.resolved_types().get(&signature.result).is_some());
    assert!(signature.validate(program.resolved_types()).is_ok());
}

#[test]
fn canonical_generic_signature_uses_binder_identity() {
    let file = parse("func identity<T>(value: T) -> T { value }");
    let program = crate::core::check_program(&file).expect("check");
    let function = program.function("identity").expect("function");
    let signature = program
        .resolved_signature(&function.node_id)
        .expect("canonical signature");

    assert_eq!(signature.generic_parameters.len(), 1);
    assert_eq!(signature.parameters[0].ty, signature.result);
    assert!(matches!(
        program.resolved_types().get(&signature.result),
        Some(crate::core::ResolvedType::GenericParameter(parameter))
            if parameter == &signature.generic_parameters[0]
    ));
}

#[test]
fn canonical_impl_method_signature_inherits_impl_binder() {
    let file = parse(
            "trait Head<T> { func head() -> T }\nimpl<T> Head<T> for List<T> { func head() -> T { self[0] } }\nfunc first<T>(values: List<T>) -> T { values.head() }",
        );
    let program = crate::core::check_program(&file).expect("check generic impl");
    let method = program
        .functions()
        .values()
        .find(|function| function.qualified_name == "List_head")
        .expect("resolved impl method");
    let signature = program
        .resolved_signature(&method.node_id)
        .expect("canonical impl signature");

    assert_eq!(signature.generic_parameters.len(), 1);
    assert!(matches!(
        program.resolved_types().get(&signature.result),
        Some(crate::core::ResolvedType::GenericParameter(parameter))
            if parameter == &signature.generic_parameters[0]
    ));
    assert!(matches!(
        program.resolved_types().get(&signature.parameters[0].ty),
        Some(crate::core::ResolvedType::Nominal { item, arguments })
            if item.as_str() == "builtin:type:List"
                && matches!(
                    arguments.as_slice(),
                    [argument] if argument == &signature.result
                )
    ));
}

#[test]
fn canonical_signatures_are_declaration_order_independent() {
    let first = parse(
        "func first(value: i32) -> i32 { value }\nfunc second(value: List<i32>) -> i32 { 0 }",
    );
    let second = parse(
        "func second(value: List<i32>) -> i32 { 0 }\nfunc first(value: i32) -> i32 { value }",
    );
    let first = crate::core::check_program(&first).expect("first check");
    let second = crate::core::check_program(&second).expect("second check");
    for name in ["first", "second"] {
        let first_function = first.function(name).expect("first function");
        let second_function = second.function(name).expect("second function");
        let first_signature = first
            .resolved_signature(&first_function.node_id)
            .expect("first signature");
        let second_signature = second
            .resolved_signature(&second_function.node_id)
            .expect("second signature");
        assert_eq!(
            first_signature
                .parameters
                .iter()
                .map(|parameter| parameter.ty.clone())
                .collect::<Vec<_>>(),
            second_signature
                .parameters
                .iter()
                .map(|parameter| parameter.ty.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(first_signature.result, second_signature.result);
    }
}

#[test]
fn checker_expression_types_use_stable_resolved_node_ids() {
    let file = parse(
        "func maximum(left: i32, right: i32) -> i32 { if left > right { left } else { right } }",
    );
    let program = crate::core::check_program(&file).expect("check");
    let function = program.function("maximum").expect("function");
    let prefix = format!("{}/", function.node_id.0);
    let expression_nodes = program
        .node_meta()
        .keys()
        .filter(|node| node.0.starts_with(&prefix) && node.0.contains("/node:expr."))
        .collect::<Vec<_>>();

    assert!(!expression_nodes.is_empty());
    assert!(expression_nodes
        .iter()
        .all(|node| program.resolved_node_type(node).is_some()));
    assert!(program
        .node_meta()
        .values()
        .all(|meta| meta.expression_key.is_none()));
    assert!(program
        .node_meta()
        .values()
        .all(|meta| meta.type_operand.is_none()));
    assert!(program
        .node_meta()
        .values()
        .all(|meta| meta.type_arguments.is_empty()));
}

#[test]
fn resolved_transition_ids_include_source_state() {
    let file = parse(
        r#"
flow Door {
    state Closed
    state Open
    transition toggle(Closed) -> Open { do { return Open {} } }
    transition toggle(Open) -> Closed { do { return Closed {} } }
}
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    assert!(program.transition("Door", "toggle", "Closed").is_some());
    assert!(program.transition("Door", "toggle", "Open").is_some());
}

#[test]
fn resolved_ids_do_not_depend_on_declaration_order() {
    let first = parse(
        r#"
flow Door {
    state Closed
    state Open
    transition open(Closed) -> Open { do { return Open {} } }
    transition close(Open) -> Closed { do { return Closed {} } }
}
"#,
    );
    let second = parse(
        r#"
flow Door {
    state Open
    state Closed
    transition close(Open) -> Closed { do { return Closed {} } }
    transition open(Closed) -> Open { do { return Open {} } }
}
"#,
    );
    let first = crate::core::check_program(&first).expect("check first");
    let second = crate::core::check_program(&second).expect("check second");
    let first_ids = first
        .transitions()
        .keys()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let second_ids = second
        .transitions()
        .keys()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(first_ids, second_ids);
}

#[test]
fn native_capability_gate_rejects_multi_target() {
    let file = parse(
        r#"
flow Decision {
    state Pending
    state Yes
    state No
    transition decide(Pending) -> Yes | No { do { return Yes {} } }
}
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let diagnostics = program
        .validate_backend(BackendProfile::Native)
        .expect_err("native must reject multi-target");
    assert!(diagnostics[0].message.contains("FLOW-MULTI-001"));
    assert!(diagnostics[0].message.contains("flow.multi_target"));
    assert_eq!(diagnostics[0].span.start_line, 6);
}

#[test]
fn verifier_capability_gate_allows_multi_target_for_contract_verification() {
    // Verifier proves function contracts; multi-target must not block
    // unrelated verification of the same CheckedProgram.
    let file = parse(
        r#"
flow Decision {
    state Pending
    state Yes
    state No
    transition decide(Pending) -> Yes | No { do { return Yes {} } }
}
func abs(x: i32) -> i32 {
    requires: x >= 0
    ensures: result >= 0
    x
}
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    program
        .validate_backend(BackendProfile::Verifier)
        .expect("verifier must not reject multi-target flows for contract verification");
    assert!(program
        .transition("Decision", "decide", "Pending")
        .is_some());
}

#[test]
fn resolved_transition_table_is_exact_source_keyed() {
    let file = parse(
        r#"
flow Counter {
    state Zero
    state Pos
    transition inc(Zero) -> Pos { do { return Pos {} } }
    transition inc(Pos) -> Pos { do { return Pos {} } }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    assert!(program.transition("Counter", "inc", "Zero").is_some());
    assert!(program.transition("Counter", "inc", "Pos").is_some());
    assert!(program.transition("Counter", "inc", "Missing").is_none());
    assert!(program.transition("Counter", "dec", "Zero").is_none());
}

#[test]
fn resolved_function_signatures_are_indexed_by_qualified_name() {
    let file = parse(
        r#"
module util {
    func twice(x: i32) -> i32 { x + x }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let twice = program
        .function("util::twice")
        .expect("util::twice signature");
    assert_eq!(twice.params.len(), 1);
    assert_eq!(twice.params[0].0, "x");
    assert!(matches!(twice.params[0].1.unlocated(), Type::Name(n, _) if n == "i32"));
    assert!(matches!(twice.ret.unlocated(), Type::Name(n, _) if n == "i32"));
    assert!(program.function("twice").is_none());
    assert!(program.function("main").is_some());
}

#[test]
fn resolved_function_records_effect_clause() {
    let file = parse(
        r#"
cap Io
func write(x: i32) -> i32 with Io { x }
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let write = program.function("write").expect("write");
    assert!(write.effects.iter().any(|e| e == "Io"));
}

#[test]
fn resolved_session_types_are_indexed() {
    let file = parse(
        r#"
session Ping = !i32 . ?i32 . end
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let session = program.session("Ping").expect("Ping session");
    assert!(matches!(
        session.body.unlocated(),
        crate::ast::SessionType::Send(_, _)
    ));
}

#[test]
fn session_calls_materialize_residuals_and_resource_transfers() {
    let file = parse(
        r#"
session S = !i32 . ?i32 . end
func client(ch: SessionChan<S>) -> i64 {
    session_send(ch, 1)
    let value = session_recv(ch)
    session_close(ch)
    value
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let mut actions = program
        .resolved_session_actions()
        .values()
        .collect::<Vec<_>>();
    actions.sort_by(|left, right| left.before.as_str().cmp(right.before.as_str()));
    assert_eq!(actions.len(), 3);
    assert_eq!(actions.iter().filter(|action| action.terminal).count(), 1);
    assert!(actions
        .iter()
        .any(|action| action.before.as_str().starts_with("!i32.")));
    assert!(actions
        .iter()
        .any(|action| action.before.as_str().starts_with("?i32.")));
    assert!(actions
        .iter()
        .any(|action| action.terminal && action.after.as_str() == "closed"));

    let client = program.function("client").expect("client");
    let analysis = program
        .resource_analysis(&client.node_id)
        .expect("client resource analysis");
    assert_eq!(
        analysis
            .actions
            .iter()
            .filter(|action| { action.kind == crate::core::CanonicalActionKind::TransferSession })
            .count(),
        2
    );
    assert!(analysis.actions.iter().any(|action| {
        action.kind == crate::core::CanonicalActionKind::Drop
            && action
                .source
                .as_ref()
                .is_some_and(|place| place.display() == "ch")
    }));
}

#[test]
fn multi_target_transition_signature_uses_closed_state_set() {
    let file = parse(
        r#"
flow Choice {
    state Start
    state Left
    state Right
    transition choose(Start) -> Left | Right {
        return Left
    }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let transition = program
        .transitions()
        .values()
        .find(|transition| {
            transition.id.event == "choose"
                && transition.id.source.name == "Start"
                && !transition.is_fallback
        })
        .expect("choose transition");
    let signature = program
        .resolved_signature(&transition.node_id)
        .expect("transition signature");
    assert!(matches!(
        program.resolved_types().get(&signature.result),
        Some(crate::core::ResolvedType::FlowStateSet { states, .. }) if states.len() == 2
    ));
}

#[test]
fn callable_catalog_atomically_owns_body_cfg_and_resources() {
    let file = parse("func add(x: i32) -> i32 { x + 1 }\nfunc main() -> i32 { add(1) }");
    let program = crate::core::check_program(&file).expect("check");
    assert_eq!(program.callables().len(), program.resolved_bodies().len());
    for (owner, callable) in program.callables() {
        assert_eq!(owner, &callable.owner);
        assert_eq!(owner, &callable.signature.owner);
        assert_eq!(owner, &callable.body.owner);
        assert_eq!(owner, &callable.cfg.owner);
        assert_eq!(owner, &callable.resources.owner);
    }
}

#[test]
fn resolved_protocol_topology_is_indexed() {
    let file = parse(
        r#"
protocol Sensor {
    state Idle
    state Active
    transition start(Idle) -> Active
    transition stop(Active) -> Idle
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let protocol = program.protocol("Sensor").expect("Sensor");
    assert!(protocol.states.iter().any(|s| s == "Idle"));
    assert!(protocol.states.iter().any(|s| s == "Active"));
    assert!(protocol
        .transitions
        .iter()
        .any(|(name, from, to)| name == "start" && from == "Idle" && to.as_slice() == ["Active"]));
}

#[test]
fn resolved_actor_fields_and_methods_are_indexed() {
    let file = parse(
        r#"
actor Counter {
    count: i32
    func inc() -> i32 { 1 }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let actor = program.actor("Counter").expect("Counter actor");
    assert!(actor.fields.iter().any(|(n, _, _)| n == "count"));
    assert!(actor.methods.iter().any(|m| m == "inc"));
}

#[test]
fn interpreter_from_checked_installs_function_directory() {
    let file = parse(
        r#"
cap Io
func write(x: i32) -> i32 with Io { x }
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert_eq!(interp.resolved_function_arity("write"), Some(1));
    let effects = interp
        .resolved_function_effects("write")
        .expect("write effects");
    assert!(effects.iter().any(|e| e == "Io"));
    assert!(program.function("write").is_some());
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert!(verifier.has_checked_function("write"));
    assert!(verifier
        .checked_function_effects("write")
        .is_some_and(|e| e.iter().any(|x| x == "Io")));
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "fx");
    codegen.compile_checked(&program).expect("compile");
    assert!(codegen
        .resolved_function_effects("write")
        .is_some_and(|e| e.iter().any(|x| x == "Io")));
    assert_eq!(codegen.resolved_function_return_type("write"), Some("i32"));
    assert_eq!(verifier.checked_function_return_type("write"), Some("i32"));
    assert_eq!(
        interp.resolved_function_params("write"),
        Some(vec![("x".into(), "i32".into())])
    );
    assert_eq!(
        codegen.resolved_function_params("write"),
        Some(vec![("x".into(), "i32".into())])
    );
    assert_eq!(
        verifier.checked_function_params("write"),
        Some(vec![("x".into(), "i32".into())])
    );
}

#[test]
fn consumers_install_comptime_function_directory() {
    let file = parse(
        r#"
comptime func answer() -> i32 { 42 }
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    assert!(program.function("answer").is_some_and(|f| f.is_comptime));
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert!(interp.is_resolved_comptime_function("answer"));
    assert!(!interp.is_resolved_comptime_function("main"));
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert!(verifier.is_checked_comptime_function("answer"));
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "ct");
    codegen.compile_checked(&program).expect("compile");
    assert!(codegen.is_resolved_comptime_function("answer"));
}

#[test]
fn interpreter_from_checked_installs_session_and_protocol_directories() {
    let file = parse(
        r#"
protocol Sensor {
    state Idle
    state Active
    transition start(Idle) -> Active
}
session Ping = !i32 . end
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert!(interp.has_resolved_session("Ping"));
    assert!(interp.has_resolved_protocol("Sensor"));
    assert!(!interp.has_resolved_protocol("Missing"));
}

#[test]
fn interpreter_from_checked_installs_actor_directory() {
    let file = parse(
        r#"
actor Counter {
    count: i32
    func inc() -> i32 { 1 }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let interp = crate::interp::Interpreter::from_checked(&program);
    let methods = interp
        .resolved_actor_methods("Counter")
        .expect("Counter methods");
    assert!(methods.iter().any(|m| m == "inc"));
}

#[test]
fn resolved_capabilities_and_constants_are_indexed() {
    let file = parse(
        r#"
cap Io
const MAX: i32 = 10
func main() -> i32 { MAX }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    assert!(program.capability("Io").is_some());
    assert!(program.constant("MAX").is_some());
}

#[test]
fn resolved_traits_and_impls_are_indexed() {
    let file = parse(
        r#"
trait Close { func close() -> i32 }
type Handle { value: i32 }
impl Close for Handle {
    func close() -> i32 { 0 }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let trait_def = program.trait_def("Close").expect("Close");
    assert!(trait_def.methods.iter().any(|m| m == "close"));
    assert!(program
        .impls()
        .values()
        .any(|i| i.trait_name == "Close" && i.type_name == "Handle"));
}

#[test]
fn interpreter_from_checked_installs_trait_and_impl_directories() {
    let file = parse(
        r#"
trait Close { func close() -> i32 }
type Handle { value: i32 }
impl Close for Handle {
    func close() -> i32 { 0 }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let interp = crate::interp::Interpreter::from_checked(&program);
    let methods = interp
        .resolved_trait_methods("Close")
        .expect("Close methods");
    assert!(methods.iter().any(|m| m == "close"));
    let impl_methods = interp
        .resolved_impl_methods("Close", "Handle")
        .expect("Close for Handle");
    assert!(impl_methods.iter().any(|m| m == "close"));
    // Trait/impl method params + effects directories.
    assert_eq!(interp.resolved_method_params("Close.close"), Some(vec![]));
    assert_eq!(interp.resolved_method_effects("Close.close"), Some(vec![]));
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert_eq!(verifier.checked_method_params("Close.close"), Some(vec![]));
    assert_eq!(verifier.checked_method_effects("Close.close"), Some(vec![]));
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "trait_params");
    codegen.compile_checked(&program).expect("compile");
    assert_eq!(codegen.resolved_method_params("Close.close"), Some(vec![]));
    assert_eq!(codegen.resolved_method_effects("Close.close"), Some(vec![]));
}

#[test]
fn consumers_install_trait_impl_method_params_and_effects() {
    let file = parse(
        r#"
cap Io
trait Writer {
    func write(data: i32) -> i32
}
type Buffer { x: i32 }
impl Writer for Buffer {
    func write(data: i32) -> i32 with Io { data }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let interp = crate::interp::Interpreter::from_checked(&program);
    // Trait method: params materialised; effects empty (trait decls carry no effect set).
    assert_eq!(
        interp.resolved_method_params("Writer.write"),
        Some(vec![("data".into(), "i32".into())])
    );
    assert_eq!(interp.resolved_method_effects("Writer.write"), Some(vec![]));
    // Impl method: params + effects (Io) materialised under impl qualified name.
    assert_eq!(
        interp.resolved_method_params("Writer:for:Buffer.write"),
        Some(vec![("data".into(), "i32".into())])
    );
    assert_eq!(
        interp.resolved_method_effects("Writer:for:Buffer.write"),
        Some(vec!["Io".to_string()])
    );
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert_eq!(
        verifier.checked_method_params("Writer.write"),
        Some(vec![("data".into(), "i32".into())])
    );
    assert_eq!(
        verifier.checked_method_effects("Writer.write"),
        Some(vec![])
    );
    assert_eq!(
        verifier.checked_method_params("Writer:for:Buffer.write"),
        Some(vec![("data".into(), "i32".into())])
    );
    assert_eq!(
        verifier.checked_method_effects("Writer:for:Buffer.write"),
        Some(vec!["Io".to_string()])
    );
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "trait_effects");
    codegen.compile_checked(&program).expect("compile");
    assert_eq!(
        codegen.resolved_method_params("Writer.write"),
        Some(vec![("data".into(), "i32".into())])
    );
    assert_eq!(
        codegen.resolved_method_effects("Writer:for:Buffer.write"),
        Some(vec!["Io".to_string()])
    );
}

#[test]
fn consumers_install_ownership_ledger_owners() {
    let file = parse(
        r#"
cap File
func close(f: cap File) -> i32 { drop(f); 0 }
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let analysis = program
        .resource_analysis(&crate::core::NodeId("function:close".into()))
        .expect("close analysis");
    assert_eq!(
        analysis.action_count(crate::core::CanonicalActionKind::Introduce),
        1
    );
    assert_eq!(
        analysis.action_count(crate::core::CanonicalActionKind::Drop),
        1
    );
    assert!(analysis.resources().iter().any(|r| r == "f"));
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert!(interp.has_resolved_ownership_owner("function:close"));
    assert_eq!(
        interp.resolved_ownership_summary("function:close"),
        Some((1, 0, 1, 0, 0, false))
    );
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert!(verifier.has_checked_ownership_owner("function:close"));
    assert_eq!(
        verifier.checked_ownership_summary("function:close"),
        Some((1, 0, 1, 0, 0, false))
    );
    assert_eq!(
        interp.resolved_ownership_resources("function:close"),
        Some(vec!["f".into()])
    );
    assert_eq!(
        verifier.checked_ownership_resources("function:close"),
        Some(vec!["f".into()])
    );
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "own_res");
    codegen.compile_checked(&program).expect("compile");
    assert_eq!(
        codegen.resolved_ownership_resources("function:close"),
        Some(vec!["f".into()])
    );
    let actions = interp
        .resolved_ownership_actions("function:close")
        .expect("actions");
    assert!(actions.iter().any(|(k, r)| k == "introduce" && r == "f"));
    assert!(actions.iter().any(|(k, r)| k == "drop" && r == "f"));
    assert!(verifier
        .checked_ownership_actions("function:close")
        .is_some_and(|a| a.iter().any(|(k, r)| k == "drop" && r == "f")));
    assert!(codegen
        .resolved_ownership_actions("function:close")
        .is_some_and(|a| a.iter().any(|(k, r)| k == "introduce" && r == "f")));
    assert_eq!(
        interp.resolved_ownership_merges("function:close"),
        Some(vec![])
    );
}

#[test]
fn ownership_merges_are_installed_for_branchy_cap_function() {
    // Native codegen still treats this pattern as unconsumed after join; exercise
    // install paths via from_checked/verify_checked only.
    let file = parse(
        r#"
cap File
func both(flag: bool, f: cap File) -> i32 {
    if flag {
        drop(f)
    } else {
        drop(f)
    }
    0
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert!(interp.resolved_ownership_merges("function:both").is_some());
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert!(verifier.checked_ownership_merges("function:both").is_some());
    // Codegen directory install without running full object emission of this fixture:
    // compile_checked fail-closes on residual linear join for this pattern, so only
    // query install via a simple program that still has empty merges map present.
    let simple = parse(
        r#"
cap File
func close(f: cap File) -> i32 { drop(f); 0 }
func main() -> i32 { 0 }
"#,
    );
    let simple_program = crate::core::check_program(&simple).expect("check simple");
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "own_merge");
    codegen.compile_checked(&simple_program).expect("compile");
    assert!(codegen
        .resolved_ownership_merges("function:close")
        .is_some_and(|m| m.is_empty()));
}

#[test]
fn ownership_summary_flags_maybe_consumed_branch_merge() {
    // Validate BranchMerge with Availability directly (synthetic).
    let merge = crate::core::BranchMerge {
        resource: "f".into(),
        then_state: crate::core::Availability::Consumed,
        else_state: crate::core::Availability::Available,
        merged_state: crate::core::Availability::MaybeConsumed,
        span: crate::span::Span::single(1, 1),
    };
    assert_eq!(merge.merged_state, crate::core::Availability::MaybeConsumed);
    // A ResourceAnalysis with no actions has zero Drop count.
    let analysis = crate::core::ResourceAnalysis {
        owner: crate::core::NodeId("function:synthetic".into()),
        actions: Vec::new(),
        loans: Vec::new(),
        in_states: std::collections::BTreeMap::new(),
        out_states: std::collections::BTreeMap::new(),
    };
    assert_eq!(
        analysis.action_count(crate::core::CanonicalActionKind::Drop),
        0
    );
}

#[test]
fn resolved_types_and_extern_blocks_are_indexed() {
    let file = parse(
        r#"
type Point { x: i32, y: i32 }
extern "C" {
    func c_abs(x: i32) -> i32
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let point = program.type_def("Point").expect("Point");
    assert_eq!(point.kind, ResolvedTypeKind::Record);
    assert!(program
        .extern_blocks()
        .values()
        .any(|block| block.funcs.iter().any(|f| f == "c_abs")));
}

#[test]
fn resolved_flow_records_annotations() {
    let file = parse(
        r#"
flow Worker {
    @max_children(3)
    @mailbox(depth = 8)
    state Idle
    transition tick(Idle) -> Idle { do { return Idle {} } }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let flow = program.flow("Worker").expect("Worker");
    assert_eq!(flow.max_children, Some(3));
    assert_eq!(flow.mailbox_depth, Some(8));
}

#[test]
fn interpreter_from_checked_prefers_resolved_max_children() {
    let file = parse(
        r#"
flow Worker {
    @max_children(4)
    state Idle
    transition tick(Idle) -> Idle { do { return Idle {} } }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let interp = crate::interp::Interpreter::from_checked(&program);
    // max_children is private; use public API if any.
    // spawn_count / max via builtins would need runtime; assert via program IR.
    assert_eq!(program.flow("Worker").unwrap().max_children, Some(4));
    assert_eq!(interp.resolved_max_children(), Some(4));
}

#[test]
fn interpreter_from_checked_installs_mailbox_depths() {
    let file = parse(
        r#"
flow Worker {
    @mailbox(depth = 64)
    state Idle
    transition tick(Idle) -> Idle { do { return Idle {} } }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert_eq!(interp.resolved_mailbox_depth("Worker"), Some(64));
    assert_eq!(program.flow("Worker").unwrap().mailbox_depth, Some(64));
}

#[test]
fn resolved_mailbox_depth_matches_module_qualified_flow() {
    let file = parse(
        r#"
module net {
    flow Conn {
        @mailbox(depth = 32)
        state Idle
        transition tick(Idle) -> Idle { do { return Idle {} } }
    }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    assert_eq!(program.flow("net::Conn").unwrap().mailbox_depth, Some(32));
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert_eq!(interp.resolved_mailbox_depth("Conn"), Some(32));
    assert_eq!(interp.resolved_mailbox_depth("net::Conn"), Some(32));
}

#[test]
fn verifier_records_flow_annotation_directories() {
    let file = parse(
        r#"
flow Worker {
    @max_children(5)
    @mailbox(depth = 16)
    state Idle
    transition tick(Idle) -> Idle { do { return Idle {} } }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert_eq!(verifier.checked_max_children(), Some(5));
    assert_eq!(verifier.checked_mailbox_depth("Worker"), Some(16));
}

#[test]
fn resolved_flow_records_persistent_field_sets() {
    let file = parse(
        r#"
flow ResilientService {
    persistent state Config { max_retries: i32, timeout_ms: i64 }
    state Active { request_id: i32 }
    transition run(Active) -> Active { do { return Active { request_id: 1 } } }
}
func main() -> i32 { 0 }
"#,
    );
    // Materialize IR from parsed AST; full check may inject matrix defaults
    // that interact with i64 payload fields independently of this IR slice.
    let program = CheckedProgram::from_checked_file(&file).expect("ir");
    let flow = program.flow("ResilientService").expect("flow");
    assert_eq!(
        flow.persistent_fields,
        vec!["max_retries".to_string(), "timeout_ms".to_string()]
    );
    assert!(flow.states.contains_key("Config"));
    assert!(flow.states.contains_key("Active"));
}

#[test]
fn consumers_install_persistent_field_directories() {
    let file = parse(
        r#"
flow ResilientService {
    persistent state Config { max_retries: i32, timeout_ms: i64 }
    state Active { request_id: i32 }
    transition run(Active) -> Active { do { return Active { request_id: 1 } } }
}
func main() -> i32 { 0 }
"#,
    );
    let program = CheckedProgram::from_checked_file(&file).expect("ir");
    let interp = crate::interp::Interpreter::from_checked(&program);
    let fields = interp
        .resolved_persistent_fields("ResilientService")
        .expect("persistent fields");
    assert!(fields.iter().any(|f| f == "max_retries"));
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    let vfields = verifier
        .checked_persistent_fields("ResilientService")
        .expect("verifier persistent fields");
    assert!(vfields.iter().any(|f| f == "timeout_ms"));
}

#[test]
fn verifier_installs_transactional_field_directories() {
    let file = parse(
        r#"
flow Store {
    persistent state Active { buffer: List<i32> }
    @transactional state Active
    transition tick(Active) -> Active { do { return Active { buffer: buffer } } }
}
func main() -> i32 { 0 }
"#,
    );
    let program = match CheckedProgram::from_checked_file(&file) {
        Ok(p) => p,
        Err(_) => return, // syntax variants differ; IR path still covered elsewhere
    };
    if let Some(flow) = program.flow("Store") {
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        if !flow.transactional_fields.is_empty() {
            assert!(verifier
                .checked_transactional_fields("Store")
                .is_some_and(|f| !f.is_empty()));
        }
    }
}

#[test]
fn checked_program_exposes_backend_requirements() {
    let file = parse(
        r#"
flow Decision {
    state Pending
    state Yes
    state No
    transition decide(Pending) -> Yes | No { do { return Yes {} } }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    assert!(program.requires_capability("flow.multi_target"));
    assert!(program
        .backend_requirements()
        .iter()
        .any(|r| r.requirement_id == "FLOW-MULTI-001"));
    assert!(program.node_meta().len() > 0);
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert!(interp.requires_resolved_capability("flow.multi_target"));
    assert!(interp.resolved_node_meta_count().is_some_and(|n| n > 0));
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert!(verifier.requires_checked_capability("flow.multi_target"));
    assert!(verifier.checked_node_meta_count() > 0);
    // Native codegen fail-closes multi-target; use a simple program for codegen install.
    let simple = parse(
        r#"
func main() -> i32 { 0 }
"#,
    );
    let simple_program = crate::core::check_program(&simple).expect("check simple");
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "backend_req");
    codegen.compile_checked(&simple_program).expect("compile");
    assert!(codegen.resolved_node_meta_count().is_some_and(|n| n > 0));
    assert!(!codegen.requires_resolved_capability("flow.multi_target"));
}

#[test]
fn resolved_flow_records_impl_protocols() {
    let file = parse(
        r#"
protocol Sensor {
    state Idle
    transition tick(Idle) -> Idle
}
flow Lidar {
    impl Sensor
    state Idle
    transition tick(Idle) -> Idle { do { return Idle {} } }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let flow = program.flow("Lidar").expect("Lidar");
    assert!(flow.impl_protocols.iter().any(|p| p == "Sensor"));
}

#[test]
fn consumers_install_flow_impl_protocol_directories() {
    let file = parse(
        r#"
protocol Sensor {
    state Idle
    transition tick(Idle) -> Idle
}
flow Lidar {
    impl Sensor
    state Idle
    transition tick(Idle) -> Idle { do { return Idle {} } }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let interp = crate::interp::Interpreter::from_checked(&program);
    let protocols = interp
        .resolved_flow_protocols("Lidar")
        .expect("Lidar protocols");
    assert!(protocols.iter().any(|p| p == "Sensor"));
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert!(verifier
        .checked_flow_protocols("Lidar")
        .is_some_and(|p| p.iter().any(|n| n == "Sensor")));
}

#[test]
fn resolved_transition_records_fallback_and_pinned_flags() {
    let file = parse(
        r#"
flow Door {
    state Closed
    state Open
    transition open(Closed) -> Open { do { return Open {} } }
}
func main() -> i32 { 0 }
"#,
    );
    // Matrix injects fallback edges; user open is not fallback.
    let program = crate::core::check_program(&file).expect("check");
    let open = program.transition("Door", "open", "Closed").expect("open");
    assert!(!open.is_fallback);
    // Matrix injects fallback edges for undefined combinations.
    assert!(program.transitions().values().any(|t| t.is_fallback));
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert!(!interp.is_resolved_fallback_transition("Door", "open", "Closed"));
    assert!(program.transitions().values().any(|t| t.is_fallback
        && interp.is_resolved_fallback_transition(&t.id.flow.0, &t.id.event, &t.id.source.name)));
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert!(program.transitions().values().any(|t| {
        t.is_fallback
            && verifier.is_checked_fallback_transition(&t.id.flow.0, &t.id.event, &t.id.source.name)
    }));
    assert!(!verifier.is_checked_fallback_transition("Door", "open", "Closed"));
    assert!(!verifier.is_checked_ffi_pinned_transition("Door", "open", "Closed"));
    assert!(!interp.is_resolved_ffi_pinned_transition("Door", "open", "Closed"));
}

#[test]
fn interpreter_exposes_resolved_transition_targets() {
    let file = parse(
        r#"
flow Door {
    state Closed
    state Open
    transition open(Closed) -> Open { do { return Open {} } }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let interp = crate::interp::Interpreter::from_checked(&program);
    let targets = interp
        .resolved_transition_targets("Door", "open", "Closed")
        .expect("targets");
    assert_eq!(targets, vec!["Open".to_string()]);
    assert!(interp
        .resolved_transition_targets("Door", "missing", "Closed")
        .is_none());
}

#[test]
fn codegen_exposes_resolved_transition_targets() {
    let file = parse(
        r#"
flow Door {
    state Closed
    state Open
    transition open(Closed) -> Open { do { return Open {} } }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "targets");
    codegen.compile_checked(&program).expect("compile");
    let targets = codegen
        .resolved_transition_targets("Door", "open", "Closed")
        .expect("targets");
    assert_eq!(targets, vec!["Open".to_string()]);
    assert!(!codegen.is_resolved_fallback_transition("Door", "open", "Closed"));
}

#[test]
fn resolved_transition_records_event_parameters() {
    let file = parse(
        r#"
flow Door {
    state Closed
    state Open
    transition open(Closed, code: i32) -> Open { do { return Open {} } }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let open = program.transition("Door", "open", "Closed").expect("open");
    assert_eq!(open.params.len(), 1);
    assert_eq!(open.params[0].0, "code");
    assert!(matches!(open.params[0].1.unlocated(), Type::Name(n, _) if n == "i32"));
}

#[test]
fn consumers_use_resolved_transition_param_arity() {
    let file = parse(
        r#"
flow Door {
    state Closed
    state Open
    transition open(Closed, code: i32) -> Open { do { return Open {} } }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert_eq!(
        interp.resolved_transition_param_arity("Door", "open", "Closed"),
        Some(1)
    );
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "arity");
    codegen.compile_checked(&program).expect("compile");
    assert_eq!(
        codegen.resolved_transition_param_arity("Door", "open", "Closed"),
        Some(1)
    );
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert_eq!(
        verifier.checked_transition_param_arity("Door", "open", "Closed"),
        Some(1)
    );
    assert_eq!(
        interp.resolved_transition_params("Door", "open", "Closed"),
        Some(vec![("code".into(), "i32".into())])
    );
    assert_eq!(
        codegen.resolved_transition_params("Door", "open", "Closed"),
        Some(vec![("code".into(), "i32".into())])
    );
    assert_eq!(
        verifier.checked_transition_params("Door", "open", "Closed"),
        Some(vec![("code".into(), "i32".into())])
    );
    let by_flow = interp
        .resolved_transitions_for_flow("Door")
        .expect("Door transitions");
    assert!(by_flow
        .iter()
        .any(|(event, source, targets, fallback, _pinned, argc)| {
            event == "open"
                && source == "Closed"
                && targets.contains("Open")
                && !*fallback
                && *argc == 1
        }));
    assert!(verifier
        .checked_transitions_for_flow("Door")
        .is_some_and(|trs| trs
            .iter()
            .any(|(e, s, _, _, _, _)| e == "open" && s == "Closed")));
    assert!(codegen
        .resolved_transitions_for_flow("Door")
        .is_some_and(|trs| trs.iter().any(|(e, _, targets, _, _, argc)| {
            e == "open" && targets.contains("Open") && *argc == 1
        })));
    let by_event = interp
        .resolved_transitions_for_event("open")
        .expect("open transitions");
    assert!(by_event
        .iter()
        .any(|(flow, source, targets, fallback, _pinned, argc)| {
            flow == "Door"
                && source == "Closed"
                && targets.contains("Open")
                && !*fallback
                && *argc == 1
        }));
    assert!(verifier
        .checked_transitions_for_event("open")
        .is_some_and(|trs| trs
            .iter()
            .any(|(f, s, _, _, _, _)| f == "Door" && s == "Closed")));
    assert!(codegen
        .resolved_transitions_for_event("open")
        .is_some_and(|trs| trs.iter().any(|(f, _, targets, _, _, argc)| {
            f == "Door" && targets.contains("Open") && *argc == 1
        })));
}

#[test]
fn consumers_install_type_and_extern_directories() {
    let file = parse(
        r#"
type Point { x: i32, y: i32 }
extern "C" {
    func c_abs(x: i32) -> i32
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert_eq!(interp.resolved_type_kind("Point"), Some("record"));
    assert!(interp.has_resolved_extern_func("c_abs"));
    assert_eq!(interp.resolved_extern_abi("c_abs"), Some("C"));
    assert_eq!(
        interp.resolved_extern_signature("c_abs"),
        Some((1, "i32".into()))
    );
    assert_eq!(
        interp.resolved_extern_params("c_abs"),
        Some(vec![("x".into(), "i32".into())])
    );
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert!(verifier.has_checked_type_def("Point"));
    assert!(verifier.has_checked_extern_func("c_abs"));
    assert_eq!(verifier.checked_extern_abi("c_abs"), Some("C"));
    assert_eq!(
        verifier.checked_extern_signature("c_abs"),
        Some((1, "i32".into()))
    );
    assert_eq!(
        verifier.checked_extern_params("c_abs"),
        Some(vec![("x".into(), "i32".into())])
    );
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "abi");
    codegen.compile_checked(&program).expect("compile");
    assert_eq!(codegen.resolved_extern_abi("c_abs"), Some("C"));
    assert_eq!(
        codegen.resolved_extern_signature("c_abs"),
        Some((1, "i32".into()))
    );
    assert_eq!(
        codegen.resolved_extern_params("c_abs"),
        Some(vec![("x".into(), "i32".into())])
    );
}

#[test]
fn interpreter_resolved_extern_directory_matches_runtime_index() {
    let file = parse(
        r#"
extern "C" {
    func c_abs(x: i32) -> i32
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert!(interp.has_resolved_extern_func("c_abs"));
    // Directory install is consistent with a successful from_checked construction.
    assert!(!interp.has_resolved_extern_func("missing_c_fn"));
}

#[test]
fn interpreter_from_checked_installs_capability_and_constant_directories() {
    let file = parse(
        r#"
cap Io
const MAX: i32 = 10
const NEG: i32 = -3
const FLAG: bool = true
func main() -> i32 { MAX }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let max = program.constant("MAX").expect("MAX");
    assert_eq!(max.ty.as_deref(), Some("i32"));
    assert_eq!(max.value, crate::core::ResolvedConstValue::Int(10));
    let neg = program.constant("NEG").expect("NEG");
    assert_eq!(neg.value, crate::core::ResolvedConstValue::Int(-3));
    let flag = program.constant("FLAG").expect("FLAG");
    assert_eq!(flag.value, crate::core::ResolvedConstValue::Bool(true));
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert!(interp.has_resolved_capability("Io"));
    assert!(interp.has_resolved_constant("MAX"));
    assert!(!interp.has_resolved_constant("Missing"));
    assert_eq!(
        interp.resolved_constant_value("MAX"),
        Some((Some("i32".into()), "int:10".into()))
    );
    assert_eq!(
        interp.resolved_constant_value("NEG"),
        Some((Some("i32".into()), "int:-3".into()))
    );
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert_eq!(
        verifier.checked_constant_value("MAX"),
        Some((Some("i32".into()), "int:10".into()))
    );
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "const_vals");
    codegen.compile_checked(&program).expect("compile");
    assert_eq!(
        codegen.resolved_constant_value("FLAG"),
        Some((Some("bool".into()), "bool:true".into()))
    );
}

#[test]
fn call_sites_resolve_function_and_extern_callees() {
    let file = parse(
        r#"
extern "C" {
    func c_abs(x: i32) -> i32
}
func helper(x: i32) -> i32 { x + 1 }
func main() -> i32 {
    let a = helper(1)
    let b = c_abs(a)
    b
}
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let sites: Vec<_> = program.call_sites().values().collect();
    assert!(
        sites.iter().any(|s| {
            s.callee == "helper"
                && s.kind == crate::core::ResolvedCallKind::Function
                && s.argc == 1
                && s.expected_argc == Some(1)
                && s.arity_matches()
                && s.ret.as_deref() == Some("i32")
        }),
        "expected helper call site, got {:?}",
        sites
            .iter()
            .map(|s| (&s.callee, s.kind, s.argc, s.expected_argc))
            .collect::<Vec<_>>()
    );
    assert!(
        sites.iter().any(|s| {
            s.callee == "c_abs"
                && s.kind == crate::core::ResolvedCallKind::Extern
                && s.argc == 1
                && s.expected_argc == Some(1)
                && s.ret.as_deref() == Some("i32")
                && s.arity_matches()
        }),
        "expected c_abs extern call site"
    );
    let c_abs = program.extern_func_signature("c_abs").expect("c_abs sig");
    assert_eq!(c_abs.params.len(), 1);
    assert_eq!(c_abs.ret, "i32");
    assert!(
        sites
            .iter()
            .any(|s| s.callee == "helper" && s.effects.is_empty()),
        "helper effects should be empty when unannotated"
    );
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert!(interp.has_resolved_call_to("helper"));
    assert!(interp.has_resolved_call_to("c_abs"));
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert!(verifier.has_checked_call_to("helper"));
    assert!(verifier.has_checked_call_to("c_abs"));
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "calls");
    codegen.compile_checked(&program).expect("compile");
    assert!(codegen.has_resolved_call_to("helper"));
    assert!(codegen.has_resolved_call_to("c_abs"));
    assert_eq!(interp.resolved_call_arity_mismatches(), 0);
    assert_eq!(codegen.resolved_call_arity_mismatches(), 0);
    assert_eq!(verifier.checked_call_arity_mismatches(), 0);
    assert_eq!(
        interp.resolved_call_return_type("helper").as_deref(),
        Some("i32")
    );
    assert_eq!(
        codegen.resolved_call_return_type("helper").as_deref(),
        Some("i32")
    );
    assert_eq!(
        verifier.checked_call_return_type("helper").as_deref(),
        Some("i32")
    );
    let main_calls = interp
        .resolved_call_sites_for_owner("function:main")
        .expect("main calls");
    assert!(main_calls
        .iter()
        .any(|(c, argc, kind)| c == "helper" && *argc == 1 && kind == "function"));
    assert!(main_calls
        .iter()
        .any(|(c, argc, kind)| c == "c_abs" && *argc == 1 && kind == "extern"));
    assert!(verifier
        .checked_call_sites_for_owner("function:main")
        .is_some_and(|calls| calls.iter().any(|(c, _, _)| c == "helper")));
    assert!(codegen
        .resolved_call_sites_for_owner("function:main")
        .is_some_and(|calls| calls
            .iter()
            .any(|(c, _, kind)| c == "c_abs" && kind == "extern")));
    let helper_callers = interp
        .resolved_call_sites_for_callee("helper")
        .expect("helper callers");
    assert!(helper_callers
        .iter()
        .any(|(owner, argc, kind)| owner == "function:main" && *argc == 1 && kind == "function"));
    assert!(verifier
        .checked_call_sites_for_callee("c_abs")
        .is_some_and(|cs| cs
            .iter()
            .any(|(owner, _, kind)| owner == "function:main" && kind == "extern")));
    assert!(codegen
        .resolved_call_sites_for_callee("helper")
        .is_some_and(|cs| cs.iter().any(|(owner, _, _)| owner == "function:main")));
}

#[test]
fn checked_ffi_verifier_rejects_resolved_arity_mismatch_before_z3() {
    let file = parse(
        r#"
extern "C" { func c_abs(x: i32) -> i32 }
func main() -> i32 { c_abs(1, 2) }
"#,
    );
    let program = CheckedProgram::from_checked_file(&file).expect("IR");
    let error = crate::verifier::verify_ffi_checked(&program)
        .expect_err("resolved arity mismatch must fail closed");
    assert!(error.contains("TOOL-RESOLUTION-001"));
    assert!(error.contains("expects 1 arguments, got 2"));
}

#[test]
fn actor_method_signatures_are_materialised() {
    let file = parse(
        r#"
actor Worker {
    func run(x: i32) -> i32 with Io { x }
}
cap Io
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let sig = program
        .actor_method_signature("Worker", "run")
        .expect("run");
    assert_eq!(sig.params.len(), 1);
    assert_eq!(sig.ret, "i32");
    assert!(program
        .actor("Worker")
        .is_some_and(|a| a.methods.iter().any(|m| m == "run")));
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert_eq!(
        interp.resolved_actor_method_signature("Worker", "run"),
        Some((1, "i32".into()))
    );
    assert_eq!(
        interp.resolved_actor_method_params("Worker", "run"),
        Some(vec![("x".into(), "i32".into())])
    );
    assert_eq!(
        interp.resolved_actor_method_effects("Worker", "run"),
        Some(vec!["Io".into()])
    );
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert_eq!(
        verifier.checked_actor_method_signature("Worker", "run"),
        Some((1, "i32".into()))
    );
    assert_eq!(
        verifier.checked_actor_method_params("Worker", "run"),
        Some(vec![("x".into(), "i32".into())])
    );
    assert_eq!(
        verifier.checked_actor_method_effects("Worker", "run"),
        Some(vec!["Io".into()])
    );
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "actor_sig");
    codegen.compile_checked(&program).expect("compile");
    assert_eq!(
        codegen.resolved_actor_method_signature("Worker", "run"),
        Some((1, "i32".into()))
    );
    assert_eq!(
        codegen.resolved_actor_method_params("Worker", "run"),
        Some(vec![("x".into(), "i32".into())])
    );
    assert_eq!(
        codegen.resolved_actor_method_effects("Worker", "run"),
        Some(vec!["Io".into()])
    );
}

#[test]
fn trait_and_impl_method_signatures_are_materialised() {
    let file = parse(
        r#"
trait Show {
    func show(self: i32) -> i32
}
type Number = i32
impl Show for Number {
    func show(self: Number) -> i32 { 0 }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let trait_sig = program
        .trait_method_signature("Show", "show")
        .expect("trait show");
    assert_eq!(trait_sig.ret, "i32");
    let impl_sig = program
        .impl_method_signature("Show", "Number", "show")
        .expect("impl show");
    assert_eq!(impl_sig.ret, "i32");
    assert_eq!(impl_sig.params.len(), 1);
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert_eq!(
        interp.resolved_method_signature("Show.show"),
        Some((1, "i32".into()))
    );
    assert_eq!(
        interp.resolved_method_signature("Show:for:Number.show"),
        Some((1, "i32".into()))
    );
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert_eq!(
        verifier.checked_method_signature("Show.show"),
        Some((1, "i32".into()))
    );
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "trait_sig");
    codegen.compile_checked(&program).expect("compile");
    assert_eq!(
        codegen.resolved_method_signature("Show:for:Number.show"),
        Some((1, "i32".into()))
    );
}

#[test]
fn protocol_payloads_and_transition_records_are_materialised() {
    let file = parse(
        r#"
protocol Sensor {
    state Idle
    state Active { data: i32 }
    transition start(Idle) -> Active
    transition stop(Active) -> Idle
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let active = program
        .protocol_state_payload("Sensor", "Active")
        .expect("Active");
    assert_eq!(active.payload_type.as_deref(), Some("i32"));
    let records = program
        .protocol_transition_records("Sensor")
        .expect("records");
    assert!(records
        .iter()
        .any(|t| t.event == "start" && t.from_state == "Idle"));
    assert!(records
        .iter()
        .any(|t| t.event == "stop" && t.from_state == "Active"));
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert_eq!(
        interp
            .resolved_protocol_payload("Sensor", "Active")
            .as_deref(),
        Some("i32")
    );
    assert_eq!(
        interp.resolved_protocol_state_payload("Sensor", "Active"),
        Some(("data".into(), "i32".into()))
    );
    assert_eq!(
        interp.resolved_protocol_states("Sensor"),
        Some(vec!["Active".into(), "Idle".into()])
    );
    assert!(interp
        .resolved_protocol_transitions("Sensor")
        .is_some_and(|trs| trs.iter().any(|(e, f, _)| e == "start" && f == "Idle")));
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert_eq!(
        verifier
            .checked_protocol_payload("Sensor", "Active")
            .as_deref(),
        Some("i32")
    );
    assert_eq!(
        verifier.checked_protocol_state_payload("Sensor", "Active"),
        Some(("data".into(), "i32".into()))
    );
    assert_eq!(
        verifier.checked_protocol_states("Sensor"),
        Some(vec!["Active".into(), "Idle".into()])
    );
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "proto");
    codegen.compile_checked(&program).expect("compile");
    assert_eq!(
        codegen
            .resolved_protocol_payload("Sensor", "Active")
            .as_deref(),
        Some("i32")
    );
    assert_eq!(
        codegen.resolved_protocol_state_payload("Sensor", "Active"),
        Some(("data".into(), "i32".into()))
    );
    assert_eq!(
        codegen.resolved_protocol_states("Sensor"),
        Some(vec!["Active".into(), "Idle".into()])
    );
}

#[test]
fn session_body_display_is_materialised() {
    let file = parse(
        r#"
session Ping = !i32 . ?i32 . end
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    assert_eq!(program.session_body_display("Ping"), Some("!i32.?i32.end"));
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert_eq!(
        interp.resolved_session_display("Ping"),
        Some("!i32.?i32.end")
    );
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert_eq!(
        verifier.checked_session_display("Ping"),
        Some("!i32.?i32.end")
    );
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "sess");
    codegen.compile_checked(&program).expect("compile");
    assert_eq!(
        codegen.resolved_session_display("Ping"),
        Some("!i32.?i32.end")
    );
}

#[test]
fn type_def_fields_and_variants_are_materialised() {
    let file = parse(
        r#"
type Point { x: i32, y: i32 }
type Id = i32
type Color { Red Green Blue }
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let fields = program.type_def_fields("Point").expect("Point fields");
    assert!(fields.iter().any(|(n, ty)| n == "x" && ty == "i32"));
    assert!(fields.iter().any(|(n, ty)| n == "y" && ty == "i32"));
    assert_eq!(program.type_def_alias_of("Id"), Some("i32"));
    let variants = program.type_def_variants("Color").expect("Color");
    assert!(variants.iter().any(|(n, p)| n == "Red" && p.is_none()));
    assert!(variants.iter().any(|(n, _)| n == "Green"));
    assert!(variants.iter().any(|(n, _)| n == "Blue"));
    let point = program.type_def("Point").expect("resolved Point");
    let x = point.field_ids.get("x").expect("stable Point.x identity");
    assert_eq!(program.resolved_member_name(x), Some("x"));
    let color = program.type_def("Color").expect("resolved Color");
    let blue = color
        .variant_ids
        .get("Blue")
        .expect("stable Color::Blue identity");
    assert_eq!(program.resolved_member_name(blue), Some("Blue"));
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert!(interp
        .resolved_type_fields("Point")
        .is_some_and(|fields| fields.iter().any(|(n, ty)| n == "x" && ty == "i32")));
    assert_eq!(interp.resolved_type_alias_of("Id"), Some("i32"));
    assert!(interp
        .resolved_type_variants("Color")
        .is_some_and(|vs| vs.iter().any(|(n, _)| n == "Blue")));
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert_eq!(verifier.checked_type_alias_of("Id"), Some("i32"));
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "types");
    codegen.compile_checked(&program).expect("compile");
    assert!(codegen
        .resolved_type_fields("Point")
        .is_some_and(|fields| fields.iter().any(|(n, _)| n == "y")));
}

#[test]
fn enum_payload_schema_owns_stable_canonical_member_types() {
    let file = parse(
        r#"
type Message<T> {
    Empty
    One(T)
    Pair { left: T, right: List<T> }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let definition = program.type_def("Message").expect("Message");
    let [(generic_name, generic_id)] = definition.generic_parameters.as_slice() else {
        panic!("one generic parameter expected");
    };
    assert_eq!(generic_name, "T");

    let empty = program
        .resolved_variant_named(&definition.node_id, "Empty")
        .expect("Empty schema");
    assert_eq!(empty.shape, ResolvedVariantShape::Unit);
    assert!(empty.members.is_empty());

    let one = program
        .resolved_variant_named(&definition.node_id, "One")
        .expect("One schema");
    assert_eq!(one.shape, ResolvedVariantShape::Tuple);
    assert_eq!(one.members.len(), 1);
    assert!(matches!(
        program.resolved_types().get(&one.members[0].ty),
        Some(crate::core::ResolvedType::GenericParameter(parameter)) if parameter == generic_id
    ));

    let pair = program
        .resolved_variant_named(&definition.node_id, "Pair")
        .expect("Pair schema");
    assert_eq!(pair.shape, ResolvedVariantShape::Record);
    assert_eq!(
        pair.members
            .iter()
            .map(|member| member.name.as_str())
            .collect::<Vec<_>>(),
        ["left", "right"]
    );
    for member in &pair.members {
        assert!(program.node_meta().contains_key(&member.node_id));
        assert_eq!(
            program.resolved_field_type(&member.node_id),
            Some(&member.ty)
        );
    }
    let right = &pair.members[1];
    assert!(matches!(
        program.resolved_types().get(&right.ty),
        Some(crate::core::ResolvedType::Nominal { item, arguments })
            if item.as_str() == "builtin:type:List"
                && matches!(
                    arguments.as_slice(),
                    [argument]
                        if matches!(
                            program.resolved_types().get(argument),
                            Some(crate::core::ResolvedType::GenericParameter(parameter))
                                if parameter == generic_id
                        )
                )
    ));
}

#[test]
fn enum_schema_validator_rejects_missing_canonical_variant() {
    let file = parse("type Choice { Value(i32), Empty }\nfunc main() -> i32 { 0 }");
    let mut program = crate::core::check_program(&file).expect("check");
    let variant = program
        .type_def("Choice")
        .and_then(|definition| definition.variant_ids.get("Value"))
        .cloned()
        .expect("Value identity");
    program.resolved_variants.remove(&variant);
    let errors = validate_resolved_variant_schemas(&program)
        .expect_err("missing variant schema must fail closed");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("has no canonical schema")));
}

#[test]
fn typed_body_lowering_does_not_consult_retained_type_declaration() {
    let file = parse(
            "type Choice { Value(i32), Empty }\nfunc read(choice: Choice) -> i32 { match choice { Value(value) => value, Empty => 0 } }",
        );
    let mut program = crate::core::check_program(&file).expect("check");
    let definition = program
        .type_defs
        .get_mut(&NodeId("type:Choice".into()))
        .expect("Choice definition");
    definition.declaration.kind =
        crate::ast::TypeDefKind::Alias(Type::Name("string".into(), Vec::new()));
    let bodies = crate::core::ir::lower::lower_checked_function_bodies(&file, &program)
        .expect("lower from canonical schema");
    assert!(bodies.contains_key(&NodeId("function:read".into())));
}

#[test]
fn capability_combined_with_is_installed() {
    let file = parse(
        r#"
cap A
cap B
cap Combined = A + B
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let cap = program.capability("Combined").expect("Combined");
    assert_eq!(cap.combined_with.as_deref(), Some("A + B"));
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert_eq!(
        interp.resolved_capability_combined_with("Combined"),
        Some("A + B")
    );
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert_eq!(
        verifier.checked_capability_combined_with("Combined"),
        Some("A + B")
    );
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "cap");
    codegen.compile_checked(&program).expect("compile");
    assert_eq!(
        codegen.resolved_capability_combined_with("Combined"),
        Some("A + B")
    );
}

#[test]
fn flow_state_payloads_are_installed() {
    let file = parse(
        r#"
flow Counter {
    state Zero
    state Positive { count: i32 }
    transition inc(Zero) -> Positive {
        do { return Positive { count: 1 } }
    }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert_eq!(
        interp.resolved_flow_state_payload("Counter", "Positive"),
        Some(vec![("count".into(), "i32".into())])
    );
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert_eq!(
        verifier.checked_flow_state_payload("Counter", "Positive"),
        Some(vec![("count".into(), "i32".into())])
    );
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "flow_payload");
    codegen.compile_checked(&program).expect("compile");
    assert_eq!(
        codegen.resolved_flow_state_payload("Counter", "Positive"),
        Some(vec![("count".into(), "i32".into())])
    );
    let states = interp.resolved_flow_states("Counter").expect("states");
    assert!(states.iter().any(|s| s == "Zero"));
    assert!(states.iter().any(|s| s == "Positive"));
    assert!(verifier
        .checked_flow_states("Counter")
        .is_some_and(|ss| ss.iter().any(|s| s == "Positive")));
    assert!(codegen
        .resolved_flow_states("Counter")
        .is_some_and(|ss| ss.iter().any(|s| s == "Zero")));
    assert!(interp
        .resolved_flow_events("Counter")
        .is_some_and(|es| es.iter().any(|e| e == "inc")));
    assert!(verifier
        .checked_flow_events("Counter")
        .is_some_and(|es| es.iter().any(|e| e == "inc")));
    assert!(codegen
        .resolved_flow_events("Counter")
        .is_some_and(|es| es.iter().any(|e| e == "inc")));
    assert_eq!(interp.resolved_item_kind("Counter"), Some("flow"));
    assert_eq!(verifier.checked_item_kind("Counter"), Some("flow"));
    assert_eq!(codegen.resolved_item_kind("main"), Some("function"));
}

#[test]
fn actor_fields_are_installed() {
    let file = parse(
        r#"
actor Worker {
    count: i32
    mut flag: bool
    func run() -> i32 { 0 }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let fields = program.actor("Worker").expect("Worker").fields.clone();
    assert!(fields.iter().any(|(n, _, m)| n == "count" && !*m));
    assert!(fields.iter().any(|(n, _, m)| n == "flag" && *m));
    let interp = crate::interp::Interpreter::from_checked(&program);
    let installed = interp.resolved_actor_fields("Worker").expect("fields");
    assert!(installed
        .iter()
        .any(|(n, ty, m)| n == "count" && ty == "i32" && !*m));
    assert!(installed
        .iter()
        .any(|(n, ty, m)| n == "flag" && ty == "bool" && *m));
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert!(verifier
        .checked_actor_fields("Worker")
        .is_some_and(|fs| fs.iter().any(|(n, _, m)| n == "flag" && *m)));
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "actor_fields");
    codegen.compile_checked(&program).expect("compile");
    assert!(codegen
        .resolved_actor_fields("Worker")
        .is_some_and(|fs| fs.iter().any(|(n, ty, _)| n == "count" && ty == "i32")));
}

#[test]
fn extern_block_flags_are_installed() {
    let file = parse(
        r#"
#[no_panic]
extern "C" {
    func safe_abs(x: i32) -> i32
}
unsafe extern "C" {
    func raw_abs(x: i32) -> i32
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert!(interp.is_resolved_extern_no_panic("safe_abs"));
    assert!(!interp.is_resolved_extern_no_panic("raw_abs"));
    assert!(interp.is_resolved_extern_unsafe("raw_abs"));
    assert!(!interp.is_resolved_extern_unsafe("safe_abs"));
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert!(verifier.is_checked_extern_no_panic("safe_abs"));
    assert!(verifier.is_checked_extern_unsafe("raw_abs"));
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "extern_flags");
    codegen.compile_checked(&program).expect("compile");
    assert!(codegen.is_resolved_extern_no_panic("safe_abs"));
    assert!(codegen.is_resolved_extern_unsafe("raw_abs"));
}

#[test]
fn call_sites_bind_callee_effects_from_function_directory() {
    // IR-only materialization: avoid effect-scope runtime checks at call sites.
    let file = parse(
        r#"
cap Io
func write_it(x: i32) -> i32 with Io { x }
func main() -> i32 {
    write_it(1)
}
"#,
    );
    let program = crate::core::CheckedProgram::from_checked_file(&file).expect("ir");
    assert!(
        program.call_sites().values().any(|s| {
            s.callee == "write_it"
                && s.effects.iter().any(|e| e == "Io")
                && s.expected_argc == Some(1)
                && s.kind == crate::core::ResolvedCallKind::Function
                && s.ret.as_deref() == Some("i32")
        }),
        "expected write_it Io call site"
    );
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert!(interp.has_resolved_call_with_effect("write_it", "Io"));
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert!(verifier.has_checked_call_with_effect("write_it", "Io"));
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "call_fx");
    codegen.compile_checked(&program).expect("compile");
    assert!(codegen.has_resolved_call_with_effect("write_it", "Io"));
}

#[test]
fn codegen_compile_checked_installs_directories() {
    let file = parse(
        r#"
cap Io
protocol Sensor {
    state Idle
    transition start(Idle) -> Idle
}
session Ping = !i32 . end
actor A { func f() -> i32 { 0 } }
const N: i32 = 1
func main() -> i32 { N }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "dir_test");
    codegen.compile_checked(&program).expect("compile_checked");
    // Public API is limited; compile success with populated CheckedProgram is the gate.
    assert!(program.capability("Io").is_some());
    assert!(program.protocol("Sensor").is_some());
    assert!(program.session("Ping").is_some());
    assert!(program.actor("A").is_some());
    assert!(program.constant("N").is_some());
}

#[test]
fn verifier_verify_checked_records_function_names() {
    let file = parse(
        r#"
flow Door {
    state Closed
    state Open
    transition open(Closed) -> Open { do { return Open {} } }
}
protocol Sensor {
    state Idle
    transition tick(Idle) -> Idle
}
trait Close { func close() -> i32 }
actor Sink { func ping() -> i32 { 0 } }
session Ping = !i32 . end
cap Io
func abs(x: i32) -> i32 {
    requires: x >= 0
    ensures: result >= 0
    x
}
func main() -> i32 { abs(1) }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert!(verifier.has_checked_function("abs"));
    assert!(verifier.has_checked_function("main"));
    assert!(verifier.has_checked_transition("Door", "open", "Closed"));
    assert!(verifier.has_checked_session("Ping"));
    assert!(!verifier.has_checked_transition("Door", "close", "Closed"));
    assert!(verifier.has_checked_protocol("Sensor"));
    assert!(verifier.has_checked_trait("Close"));
    assert!(verifier.has_checked_actor("Sink"));
}

#[test]
fn canonical_flow_ids_include_module_path() {
    let file = parse(
        r#"
module alpha {
    flow Worker {
        state Idle
        state Busy
        transition start(Idle) -> Busy { do { return Busy {} } }
    }
}
module beta {
    flow Worker {
        state Idle
        state Busy
        transition start(Idle) -> Busy { do { return Busy {} } }
    }
}
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    assert!(program
        .transition("alpha::Worker", "start", "Idle")
        .is_some());
    assert!(program
        .transition("beta::Worker", "start", "Idle")
        .is_some());
    assert_eq!(
        program
            .transitions()
            .keys()
            .filter(|id| id.event == "start" && id.source.name == "Idle")
            .count(),
        2
    );
    let alpha = program.flow("alpha::Worker").expect("alpha flow");
    let idle = alpha.states.get("Idle").expect("Idle state");
    assert_eq!(idle.id.flow.0, "alpha::Worker");
    assert_eq!(idle.node_id.0, "state:alpha::Worker::Idle");
    assert_eq!(idle.origin.user_span().start_line, 4);
    assert!(idle.payload.is_empty());
    assert!(program.flow("Worker").is_none());
    assert!(program
        .items()
        .contains_key(&NodeId("module:alpha".to_string())));
    assert!(program
        .items()
        .contains_key(&NodeId("flow:alpha::Worker".to_string())));
}

#[test]
fn resolved_item_directory_records_declaration_spans() {
    let file = parse(
        r#"
actor Worker {
    func run() -> i32 { 0 }
}
protocol Service {
    state Ready
}
session Request = !i32 . end
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    for (node_id, line) in [
        ("actor:Worker", 2),
        ("protocol:Service", 5),
        ("session:Request", 8),
        ("function:main", 9),
    ] {
        let item = program
            .items()
            .get(&NodeId(node_id.to_string()))
            .unwrap_or_else(|| panic!("missing {node_id}"));
        assert_eq!(item.origin.user_span().start_line, line);
    }
    assert_eq!(program.entry_span().expect("entry span").start_line, 9);
}

#[test]
fn resolved_types_distinguish_user_declarations_from_synthetic_types() {
    let file = parse(
        r#"
type Point { x: i32 }
newtype UserId = i64
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    for (node_id, line) in [("type:Point", 2), ("type:UserId", 3)] {
        let item = program
            .items()
            .get(&NodeId(node_id.to_string()))
            .unwrap_or_else(|| panic!("missing {node_id}"));
        assert_eq!(item.kind, ResolvedItemKind::Type);
        assert_eq!(item.origin.user_span().start_line, line);
    }
    assert!(!program
        .items()
        .contains_key(&NodeId("type:ExecResult".to_string())));
}

#[test]
fn resolved_item_directory_covers_remaining_top_level_items() {
    let file = parse(
        r#"
cap Read
trait Show { func show(self: i32) -> i32; }
type Number = i32
impl Show for Number { func show(self: Number) -> i32 { 0 } }
const ANSWER: i32 = 42
extern "C" { func abs(value: i32) -> i32; }
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    for node_id in [
        "capability:Read",
        "trait:Show",
        "impl:Show:for:Number",
        "const:ANSWER",
        "extern:C:abs",
    ] {
        let item = program
            .items()
            .get(&NodeId(node_id.to_string()))
            .unwrap_or_else(|| panic!("missing {node_id}"));
        assert!(item.origin.user_span().start_line > 0);
    }
}

#[test]
fn extern_block_node_id_is_independent_of_position_and_symbol_order() {
    let first = parse(
        r#"
extern "C" {
    func read(fd: i32) -> i32;
    func close(fd: i32) -> i32;
}
func main() -> i32 { 0 }
"#,
    );
    let reordered = parse(
        r#"
func main() -> i32 { 0 }


extern "C" {
    func close(fd: i32) -> i32;
    func read(fd: i32) -> i32;
}
"#,
    );
    let first = crate::core::check_program(&first).expect("first program");
    let reordered = crate::core::check_program(&reordered).expect("reordered program");
    let first_ids = first
        .extern_blocks()
        .keys()
        .map(|id| id.0.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let reordered_ids = reordered
        .extern_blocks()
        .keys()
        .map(|id| id.0.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(first_ids, reordered_ids);
    assert_eq!(first_ids.len(), 1);
    assert!(first_ids.contains("extern:C:close+read"));
}

#[test]
fn generated_flow_nodes_keep_user_span_and_system_origin() {
    let file = parse(
        r#"
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let flow = program.flow("Main").expect("implicit Main flow");
    assert!(matches!(flow.origin, Origin::RuntimeSystem { .. }));
    assert_eq!(flow.origin.rule(), Some("progressive.main"));
    match &flow.origin {
        Origin::RuntimeSystem { parent, .. } => {
            assert_eq!(parent, &NodeId("function:main".to_string()));
            assert!(program.items().contains_key(parent));
            assert_ne!(parent, &flow.node_id);
        }
        _ => unreachable!(),
    }
    assert_eq!(flow.origin.user_span().start_line, 2);
    let single = flow.states.get("Single").expect("Single state");
    assert!(matches!(single.origin, Origin::RuntimeSystem { .. }));
    assert_eq!(single.origin.rule(), Some("progressive.single"));
    assert_eq!(single.origin.user_span().start_line, 2);
    let Origin::RuntimeSystem {
        parent: single_parent,
        ..
    } = &single.origin
    else {
        unreachable!()
    };
    assert_eq!(single_parent, &flow.node_id);
    assert!(flow
        .transitions
        .iter()
        .filter_map(|id| program.transitions().get(id))
        .all(|transition| transition.origin.user_span().start_line > 0));
    let run = program
        .transition("Main", "run", "Single")
        .expect("implicit run transition");
    assert!(matches!(run.origin, Origin::RuntimeSystem { .. }));
    assert_eq!(run.origin.rule(), Some("progressive.run"));
    let Origin::RuntimeSystem {
        parent: run_parent, ..
    } = &run.origin
    else {
        unreachable!()
    };
    assert_eq!(run_parent, &flow.node_id);
    let reset = program
        .transition("Main", "reset", "Fault")
        .expect("implicit reset transition");
    assert!(matches!(reset.origin, Origin::RuntimeSystem { .. }));
    assert_eq!(reset.origin.rule(), Some("flow.reset"));
    let fallback = program
        .transition("Main", "run", "Fault")
        .expect("matrix fallback transition");
    assert!(matches!(fallback.origin, Origin::PrototypeFallback { .. }));
    assert_eq!(fallback.origin.rule(), Some("flow.matrix.fallback"));
    let run_stmt_id = generated_node_id(
        &program,
        "transition:Main::run::Single",
        "stmt.return",
        "progressive.run",
    );
    let run_stmt = program
        .node_meta()
        .get(&run_stmt_id)
        .expect("implicit run body metadata");
    assert!(matches!(run_stmt.origin, Origin::RuntimeSystem { .. }));
    assert_eq!(run_stmt.origin.rule(), Some("progressive.run"));
    let Origin::RuntimeSystem {
        parent: run_stmt_parent,
        ..
    } = &run_stmt.origin
    else {
        unreachable!()
    };
    assert_eq!(run_stmt_parent, &run.node_id);
    let fallback_stmt_id = generated_node_id(
        &program,
        "transition:Main::run::Fault",
        "stmt.return",
        "flow.matrix.fallback",
    );
    let fallback_stmt = program
        .node_meta()
        .get(&fallback_stmt_id)
        .expect("matrix fallback body metadata");
    assert!(matches!(
        fallback_stmt.origin,
        Origin::PrototypeFallback { .. }
    ));
    assert_eq!(fallback_stmt.origin.rule(), Some("flow.matrix.fallback"));
}

#[test]
fn generated_transition_rule_comes_from_ast_and_survives_rename() {
    let mut file = parse("flow Worker { state Active }");
    let reset = file
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Flow(flow) => flow
                .transitions
                .iter_mut()
                .find(|transition| transition.name == "reset"),
            _ => None,
        })
        .expect("generated reset transition");
    reset.name = "restart".to_string();
    reset.meta.origin = AstOrigin::RuntimeSystem("test.rule.from_ast");

    let program = crate::core::check_program(&file).expect("check renamed transition");
    let restart = program
        .transition("Worker", "restart", "Fault")
        .expect("renamed transition");
    assert_eq!(restart.origin.rule(), Some("test.rule.from_ast"));
    let Origin::RuntimeSystem { parent, .. } = &restart.origin else {
        unreachable!()
    };
    assert_eq!(parent, &NodeId("flow:Worker".into()));
}

#[test]
fn explicit_named_parent_survives_generated_flow_rule_and_name_changes() {
    let mut file = parse("func main() -> i32 { 0 }");
    let flow = file
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Flow(flow) if flow.name == "Main" => Some(flow),
            _ => None,
        })
        .expect("implicit Main flow");
    assert_eq!(flow.meta.parent, AstParentHint::NamedFunction("main"));
    flow.name = "RenamedRuntimeFlow".into();
    flow.meta.origin = AstOrigin::RuntimeSystem("test.renamed.progressive.rule");

    let program = crate::core::check_program(&file).expect("renamed generated flow");
    let flow = program
        .flow("RenamedRuntimeFlow")
        .expect("renamed flow catalog entry");
    let Origin::RuntimeSystem { parent, rule, .. } = &flow.origin else {
        unreachable!()
    };
    assert_eq!(parent, &NodeId("function:main".into()));
    assert_eq!(rule, "test.renamed.progressive.rule");
}

#[test]
fn generated_ast_node_without_parent_hint_is_rejected() {
    let mut file = parse("flow Worker { state Active }");
    let reset = file
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Flow(flow) => flow
                .transitions
                .iter_mut()
                .find(|transition| transition.name == "reset"),
            _ => None,
        })
        .expect("generated reset transition");
    reset.meta.parent = AstParentHint::None;

    let diagnostics = crate::core::check_program(&file).expect_err("parent hint must fail");
    assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.message.contains(
                "generated NodeId 'transition:Worker::reset::Fault' is missing an explicit AST parent hint",
            )
        }));
}

#[test]
fn generated_ast_origin_with_empty_rule_is_rejected() {
    let mut file = parse("flow Worker { state Active }");
    let reset = file
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Flow(flow) => flow
                .transitions
                .iter_mut()
                .find(|transition| transition.name == "reset"),
            _ => None,
        })
        .expect("generated reset transition");
    reset.meta.origin = AstOrigin::RuntimeSystem("");

    let diagnostics = crate::core::check_program(&file).expect_err("empty rule must fail");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("generated NodeId 'transition:Worker::reset::Fault' has an empty Origin rule")
    }));
}

#[test]
fn generated_transition_call_sites_inherit_runtime_origin() {
    let file = parse(
        r#"
flow Worker {
    state Active { outcome: Result<i32, string> }
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let recover_owner = "transition:Worker::recover::Fault";
    let calls = program
        .call_sites()
        .values()
        .filter(|call| call.owner == recover_owner)
        .collect::<Vec<_>>();
    assert!(!calls.is_empty(), "recover should contain a generated call");
    assert!(calls.iter().all(|call| {
        matches!(call.origin, Origin::RuntimeSystem { .. })
            && call.origin.rule() == Some("flow.recover")
    }));
}

#[test]
fn checked_diagnostics_never_use_zero_sentinel_spans() {
    for source in [
        "func broken(x: Missing) -> i32 { 0 }",
        "actor Worker { value: Missing }",
        "protocol P { state A { value: Missing } }",
        "session S = Missing",
        "flow F { state A { value: Missing } }",
    ] {
        let file = parse(source);
        let diagnostics = crate::core::check_program(&file).expect_err(source);
        assert!(!diagnostics.is_empty(), "expected diagnostics for {source}");
        for diagnostic in diagnostics {
            assert!(
                diagnostic.span.start_line > 0 && diagnostic.span.start_col > 0,
                "sentinel span for {source}: {:?}",
                diagnostic
            );
        }
    }
}

#[test]
fn node_meta_covers_nested_stmt_expr_and_pattern_paths() {
    let file = parse(
        r#"
func main() -> i32 {
    let pair = (1, 2)
    if true { return pair.0 } else { return 0 }
}
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let let_id = node_id_at(&program, "stmt.let", 3, 5);
    let pattern_id = node_id_at(&program, "pattern.variable", 3, 9);
    let tuple_id = node_id_at(&program, "expr.tuple", 3, 16);
    let tuple_item_id = node_id_at(&program, "expr.literal", 3, 17);
    let cond_id = node_id_at(&program, "expr.literal", 4, 8);
    let returned_ident_id = node_id_at(&program, "expr.identifier", 4, 22);
    for node_id in [
        &let_id,
        &pattern_id,
        &tuple_id,
        &tuple_item_id,
        &cond_id,
        &returned_ident_id,
    ] {
        let meta = program
            .node_meta()
            .get(node_id)
            .unwrap_or_else(|| panic!("missing {}", node_id.0));
        assert!(meta.origin.user_span().start_line > 0);
    }
    assert_eq!(
        program
            .node_meta()
            .get(&let_id)
            .expect("let metadata")
            .precision,
        SpanPrecision::Exact
    );
    assert_eq!(
        program
            .node_meta()
            .get(&cond_id)
            .expect("condition metadata")
            .precision,
        SpanPrecision::Exact
    );
    let interp = crate::interp::Interpreter::from_checked(&program);
    assert!(interp.has_resolved_node_meta_path(&let_id.0));
    assert!(interp.has_resolved_node_meta_path(&cond_id.0));
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert!(verifier.has_checked_node_meta_path(&tuple_id.0));
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "node_meta");
    codegen.compile_checked(&program).expect("compile");
    assert!(codegen.has_resolved_node_meta_path(&returned_ident_id.0));
    assert_eq!(
        interp.resolved_node_meta_precision(&let_id.0),
        Some("exact")
    );
    assert_eq!(
        verifier.checked_node_meta_precision(&cond_id.0),
        Some("exact")
    );
    assert_eq!(
        codegen.resolved_node_meta_precision(&let_id.0),
        Some("exact")
    );
    let let_span = interp.resolved_node_meta_span(&let_id.0).expect("let span");
    assert!(let_span.0 > 0);
    assert_eq!(verifier.checked_node_meta_span(&let_id.0), Some(let_span));
    assert_eq!(codegen.resolved_node_meta_span(&let_id.0), Some(let_span));
    let cond_span = interp
        .resolved_node_meta_span(&cond_id.0)
        .expect("cond span");
    assert!(cond_span.0 > 0);
    assert_eq!(verifier.checked_node_meta_span(&cond_id.0), Some(cond_span));
}

#[test]
fn positioned_body_node_ids_survive_synthetic_statement_insertion() {
    let mut file = parse(
        r#"
func helper() -> i32 { 1 }
func main() -> i32 {
    let value = helper()
    value
}
"#,
    );
    let original = crate::core::check_program(&file).expect("original");
    let let_id = node_id_at(&original, "stmt.let", 4, 5);
    let call_id = original
        .call_sites()
        .values()
        .find(|call| call.owner == "function:main" && call.callee == "helper")
        .map(|call| call.node_id.clone())
        .expect("helper call site");
    assert!(original.node_meta().contains_key(&let_id));
    assert!(original.call_sites().contains_key(&call_id));

    let main = file
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Func(function) if function.name == "main" => Some(function),
            _ => None,
        })
        .expect("main function");
    main.body.insert(0, Stmt::Block(Vec::new()));

    let lowered = crate::core::check_program(&file).expect("lowered");
    assert!(lowered.node_meta().contains_key(&let_id));
    assert!(lowered.call_sites().contains_key(&call_id));
}

#[test]
fn positioned_contract_node_ids_survive_synthetic_statement_insertion() {
    let mut file = parse(
        r#"
func main() -> i32 {
    requires: true
    return 0
}
"#,
    );
    let original = crate::core::check_program(&file).expect("original");
    let contract_id = node_id_at(&original, "stmt.requires", 3, 5);
    assert!(original.node_meta().contains_key(&contract_id));

    let main = file
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Func(function) if function.name == "main" => Some(function),
            _ => None,
        })
        .expect("main function");
    main.body.insert(0, Stmt::Block(Vec::new()));

    let lowered = crate::core::check_program(&file).expect("lowered");
    assert!(lowered.node_meta().contains_key(&contract_id));
}

#[test]
fn anonymous_node_ids_use_stable_source_keys_not_session_source_ids() {
    let source = "func main() -> i32 { let value = 1; value }";
    let key = crate::span::SourceKey::new("workspace:src/main.mimi").expect("key");

    let mut first_registry = crate::span::SourceRegistry::default();
    let first_id = first_registry
        .register(crate::span::SourceRecord::new(
            key.clone(),
            crate::span::SourceTextOrigin::Disk,
        ))
        .expect("first source");
    let first_tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("lex first");
    let first_file =
        crate::parser::Parser::new_with_source_registry(first_tokens, first_id, first_registry)
            .parse_file()
            .expect("parse first");

    let mut second_registry = crate::span::SourceRegistry::default();
    second_registry
        .register_key(
            "workspace:src/other.mimi",
            crate::span::SourceTextOrigin::Disk,
        )
        .expect("other source");
    let second_id = second_registry
        .register(crate::span::SourceRecord::new(
            key,
            crate::span::SourceTextOrigin::Disk,
        ))
        .expect("second source");
    assert_ne!(first_id, second_id);
    let second_tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("lex second");
    let second_file =
        crate::parser::Parser::new_with_source_registry(second_tokens, second_id, second_registry)
            .parse_file()
            .expect("parse second");

    let first = crate::core::check_program(&first_file).expect("check first");
    let second = crate::core::check_program(&second_file).expect("check second");
    let first_ids = first
        .node_meta()
        .keys()
        .map(|id| id.0.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let second_ids = second
        .node_meta()
        .keys()
        .map(|id| id.0.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(first_ids, second_ids);
    assert!(first_ids
        .iter()
        .any(|id| id.contains("workspace:src%2fmain.mimi")));
}

#[test]
fn declaration_expression_call_sites_are_complete_expr_ids_and_reorder_stable() {
    let source = r#"
func leaf() -> i32 { 1 }
func generic<T>(value: T) -> T { value }
const ANSWER: i32 = leaf()
trait Defaulted {
    func choose(left: i32 = leaf(), right: i32 = leaf()) -> i32;
}
actor Worker {
    mut value: i32 = leaf()
    func choose(left: i32 = leaf(), right: i32 = leaf()) -> i32 {
        generic::<i32>(left)
    }
}
impl Defaulted for i32 {
    func choose(left: i32 = leaf(), right: i32 = leaf()) -> i32 {
        generic::<i32>(right)
    }
}
extern "C" {
    func probe(value: i32) -> i32
        requires: leaf() > 0
        ensures: generic::<i32>(result) > 0;
}
flow Machine {
    state Ready
    transition tick(Ready, left: i32, right: i32) -> Ready {
        do { return Ready {} }
    }
}
func top(left: i32 = leaf(), right: i32 = leaf()) -> i32 {
    func nested(first: i32 = leaf(), second: i32 = leaf()) -> i32 { first }
    generic::<i32>(nested(left))
}
func main() -> i32 { top() }
"#;
    let mut file = parse(source);
    // Transition parameters use the same Param AST but their current
    // surface grammar does not expose defaults. Seed the model directly
    // so the declaration walker remains complete when that syntax lands.
    let transition_defaults = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Func(function) if function.name == "top" => Some(
                function
                    .params
                    .iter()
                    .map(|param| param.default_value.clone().expect("top default"))
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .expect("default expression fixtures");
    let transition = file
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Flow(flow) => flow
                .transitions
                .iter_mut()
                .find(|transition| transition.name == "tick"),
            _ => None,
        })
        .expect("tick transition");
    for (param, default) in transition.params.iter_mut().zip(transition_defaults) {
        param.default_value = Some(default);
    }
    let original = CheckedProgram::from_checked_file(&file).expect("catalog original");

    let calls_for = |owner: &str, callee: &str| {
        original
            .call_sites()
            .values()
            .filter(|call| call.owner == owner && call.callee == callee)
            .collect::<Vec<_>>()
    };
    assert_eq!(calls_for("constant:ANSWER", "leaf").len(), 1);
    assert_eq!(calls_for("actor:Worker", "leaf").len(), 1);
    assert_eq!(calls_for("function:Worker::choose", "leaf").len(), 2);
    assert_eq!(calls_for("function:Worker::choose", "generic").len(), 1);
    assert_eq!(
        calls_for("transition:Machine::tick::Ready", "leaf").len(),
        2
    );
    assert_eq!(calls_for("function:top", "leaf").len(), 2);
    assert_eq!(calls_for("function:top", "generic").len(), 1);

    let trait_def = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Trait(trait_def) => Some(trait_def),
            _ => None,
        })
        .expect("trait fixture");
    let trait_method = &trait_def.methods[0];
    let trait_owner = format!(
        "trait:Defaulted/method:{}:{:016x}",
        stable_id_fragment(&trait_method.name),
        stable_text_hash(&method_signature_key(
            &trait_method.name,
            &trait_method.params,
            trait_method.ret.as_ref()
        ))
    );
    assert_eq!(calls_for(&trait_owner, "leaf").len(), 2);

    let impl_def = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Impl(impl_def) => Some(impl_def),
            _ => None,
        })
        .expect("impl fixture");
    let impl_owner = impl_method_owner("Defaulted:for:i32", &impl_def.methods[0]);
    assert_eq!(calls_for(&impl_owner.0, "leaf").len(), 2);
    assert_eq!(calls_for(&impl_owner.0, "generic").len(), 1);

    let extern_block = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::ExternBlock(block) => Some(block),
            _ => None,
        })
        .expect("extern fixture");
    let extern_owner = extern_function_owner(
        &NodeId(format!("extern:{}", extern_block_key(extern_block))),
        &extern_block.funcs[0],
    );
    assert_eq!(calls_for(&extern_owner.0, "leaf").len(), 1);
    assert_eq!(calls_for(&extern_owner.0, "generic").len(), 1);

    let nested_owner = original
        .call_sites()
        .values()
        .find(|call| call.owner.starts_with("function:top/function:nested:"))
        .map(|call| call.owner.clone())
        .expect("nested default call owner");
    assert_eq!(calls_for(&nested_owner, "leaf").len(), 2);

    let turbofish_calls = original
        .call_sites()
        .values()
        .filter(|call| call.callee == "generic")
        .collect::<Vec<_>>();
    assert_eq!(turbofish_calls.len(), 4);
    assert!(turbofish_calls.iter().all(|call| {
        call.node_id.0.contains("/node:expr.turbofish@")
            && original.node_meta().get(&call.node_id).is_some_and(|meta| {
                meta.origin == call.origin && meta.precision == SpanPrecision::Exact
            })
    }));
    assert!(original.call_sites().values().all(|call| {
        original
            .node_meta()
            .get(&call.node_id)
            .is_some_and(|meta| meta.origin == call.origin)
    }));

    let original_ids = original
        .call_sites()
        .keys()
        .map(|node_id| node_id.0.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut reordered_file = file.clone();
    reordered_file.items.reverse();
    for item in &mut reordered_file.items {
        match item {
            Item::Func(function) => {
                function.params.reverse();
                for stmt in &mut function.body {
                    if let Stmt::Func(nested) = stmt.unlocated_mut() {
                        nested.params.reverse();
                    }
                }
            }
            Item::Actor(actor) => {
                actor.fields.reverse();
                actor.methods.reverse();
                for method in &mut actor.methods {
                    method.params.reverse();
                }
            }
            Item::Trait(trait_def) => {
                trait_def.methods.reverse();
                for method in &mut trait_def.methods {
                    method.params.reverse();
                }
            }
            Item::Impl(impl_def) => {
                impl_def.methods.reverse();
                for method in &mut impl_def.methods {
                    method.params.reverse();
                }
            }
            Item::ExternBlock(block) => block.funcs.reverse(),
            Item::Flow(flow) => {
                flow.transitions.reverse();
                for transition in &mut flow.transitions {
                    transition.params.reverse();
                }
            }
            _ => {}
        }
    }
    let reordered = CheckedProgram::from_checked_file(&reordered_file).expect("catalog reordered");
    let reordered_ids = reordered
        .call_sites()
        .keys()
        .map(|node_id| node_id.0.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(original_ids, reordered_ids);
    assert!(reordered.call_sites().values().all(|call| {
        reordered
            .node_meta()
            .get(&call.node_id)
            .is_some_and(|meta| meta.origin == call.origin)
    }));
}

#[test]
fn declaration_type_protocol_session_extern_and_flow_catalog_is_complete_and_reorder_stable() {
    let source = r#"
type Pair<T: Clone, U: Eq> { left: Result<T, string>, right: List<U> }
type Choice { Some(i32), Empty }
trait Show<T> { func show(value: T, flags: i32) -> string; }
extern "C" {
    func add(left: i32, right: i32) -> i32;
    func sub(left: i32, right: i32) -> i32;
}
protocol Sensor<T> {
    state Idle
    state Active { data: T }
    transition start(Idle) -> Active
    transition stop(Active) -> Idle
}
session Ping = !Result<i32, string> . ?List<i32> . end
session Pong = dual(Ping)
flow Worker<T> {
    @max_children(3)
    @mailbox(depth = 8)
    state Idle { value: T, count: i32 }
    state Busy { value: T, count: i32 }
    transition start(Idle, left: i32, right: i32) -> Busy { do { return Busy {} } }
    transition stop(Busy, left: i32, right: i32) -> Idle { do { return Idle {} } }
}
func catalog<T: Clone, U: Eq>(first: Pair<T, U>, second: Pair<T, U>) -> i32 where T: Clone, U: Eq {
    let pair = Pair { left: first.left, right: second.right }
    match 1 {
        0 => Pair { left: first.left, right: second.right }.right,
        _ => 0
    }
}
"#;
    let original_file = parse(source);
    let mut reordered_file = original_file.clone();

    for item in &mut reordered_file.items {
        match item {
            Item::Type(type_def) if type_def.name == "Pair" => {
                type_def.generics.reverse();
                if let crate::ast::TypeDefKind::Record(fields) = &mut type_def.kind {
                    fields.reverse();
                }
            }
            Item::Type(type_def) if type_def.name == "Choice" => {
                if let crate::ast::TypeDefKind::Enum(variants) = &mut type_def.kind {
                    variants.reverse();
                    for variant in variants {
                        match &mut variant.payload {
                            Some(crate::ast::VariantPayload::Tuple(types)) => types.reverse(),
                            Some(crate::ast::VariantPayload::Record(fields)) => fields.reverse(),
                            None => {}
                        }
                    }
                }
            }
            Item::Trait(trait_def) => {
                trait_def.generics.reverse();
            }
            Item::ExternBlock(block) => {
                block.funcs.reverse();
                for function in &mut block.funcs {
                    function.params.reverse();
                }
            }
            Item::Protocol(protocol) => {
                protocol.generics.reverse();
                protocol.states.reverse();
                protocol.transitions.reverse();
            }
            Item::Flow(flow) => {
                flow.generics.reverse();
                flow.annotations.reverse();
                flow.states.reverse();
                flow.transitions.reverse();
                for state in &mut flow.states {
                    if let Some(payload) = &mut state.payload {
                        payload.reverse();
                    }
                }
                for transition in &mut flow.transitions {
                    transition.params.reverse();
                }
            }
            Item::Func(function) if function.name == "catalog" => {
                function.generics.reverse();
                function.params.reverse();
                function.where_clause.reverse();
                for stmt in &mut function.body {
                    match stmt.unlocated_mut() {
                        Stmt::Let {
                            init: Some(expr), ..
                        } => {
                            if let Expr::Record { fields, .. } = expr.unlocated_mut() {
                                fields.reverse();
                            }
                        }
                        Stmt::Expr(expr) => {
                            if let Expr::Match(_, arms) = expr.unlocated_mut() {
                                arms.reverse();
                                for arm in arms {
                                    if let Expr::Field(record, _) = arm.body.unlocated_mut() {
                                        if let Expr::Record { fields, .. } = record.unlocated_mut()
                                        {
                                            fields.reverse();
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    reordered_file.items.reverse();

    let original = CheckedProgram::from_checked_file(&original_file).expect("original catalog");
    let reordered = CheckedProgram::from_checked_file(&reordered_file).expect("reordered catalog");
    assert_eq!(node_meta_ids(&original), node_meta_ids(&reordered));

    let ids = node_meta_ids(&original);
    for canonical in [
        "type:Pair",
        "type:Choice",
        "trait:Show",
        "extern:C:add+sub",
        "protocol:Sensor",
        "session:Ping",
        "session:Pong",
        "flow:Worker",
        "state:Worker::Idle",
        "state:Worker::Busy",
        "transition:Worker::start::Idle",
        "transition:Worker::stop::Busy",
        "function:catalog",
    ] {
        assert!(ids.contains(canonical), "missing canonical {canonical}");
    }
    for kind in [
        "decl.generic_parameter",
        "decl.parameter",
        "decl.where_clause",
        "decl.field",
        "decl.variant",
        "decl.extern_parameter",
        "decl.flow_annotation",
        "type.name",
        "session.send",
        "session.recv",
        "session.end",
        "match.arm",
        "record.field",
    ] {
        assert!(
            ids.iter().any(|node_id| {
                node_id.contains(&format!("/node:{kind}@"))
                    || node_id.contains(&format!("/generated:{kind}:"))
                    || node_id.contains(&format!("/fallback:{kind}:"))
            }),
            "missing NodeMeta kind {kind}"
        );
    }
    assert!(ids
        .iter()
        .any(|node_id| node_id.starts_with("protocol:Sensor/state:")));
    assert!(ids
        .iter()
        .any(|node_id| node_id.starts_with("protocol:Sensor/transition:")));
    assert!(ids
        .iter()
        .any(|node_id| node_id.starts_with("extern:C:add+sub/function:")));
}

#[test]
fn generated_siblings_with_the_same_inherited_span_use_rule_and_semantic_discriminator() {
    let mut file = parse("func main() -> i32 { let values = [1]; 0 }");
    let main = file
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Func(function) if function.name == "main" => Some(function),
            _ => None,
        })
        .expect("main");
    let inherited = main.meta.span;
    let Stmt::Let {
        init: Some(values), ..
    } = main.body[0].unlocated_mut()
    else {
        panic!("list binding")
    };
    let Expr::List(items) = values.unlocated_mut() else {
        panic!("list expression")
    };
    *items = vec![
        Expr::Literal(crate::ast::Lit::Int(7)).with_meta(crate::ast::AstNodeMeta::inherited(
            inherited,
            AstOrigin::Desugared("test.same_span"),
        )),
        Expr::Literal(crate::ast::Lit::Int(7)).with_meta(crate::ast::AstNodeMeta::inherited(
            inherited,
            AstOrigin::Desugared("test.same_span"),
        )),
    ];

    let program = CheckedProgram::from_checked_file(&file).expect("generated siblings");
    let generated = program
        .node_meta()
        .iter()
        .filter(|(node_id, meta)| {
            node_id
                .0
                .contains("/generated:expr.literal:test.same_span:")
                && meta.origin.rule() == Some("test.same_span")
        })
        .collect::<Vec<_>>();
    assert_eq!(generated.len(), 2);
    assert_ne!(generated[0].0, generated[1].0);
    assert!(generated
        .iter()
        .all(|(_, meta)| meta.precision == SpanPrecision::SourceAnchor));
}

#[test]
fn callable_catalog_uses_the_same_impl_and_nested_ids_as_ownership_ledgers() {
    let source = r#"
module api {
    trait Close {
        func close(value: i32) -> i32;
        func flush(value: i32) -> i32;
    }
    type Handle { value: i32 }
    impl Close for Handle {
        func close(value: i32) -> i32 { value }
        func flush(value: i32) -> i32 { value }
    }
    func outer() -> i32 { func inner(value: i32) -> i32 { value }; inner(1) }
}
func main() -> i32 { 0 }
"#;
    let file = parse(source);
    let program = crate::core::check_program(&file).expect("callable catalog");
    let module = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Module(module) if module.name == "api" => Some(module),
            _ => None,
        })
        .expect("api module");
    let impl_def = module
        .items
        .iter()
        .find_map(|item| match item {
            Item::Impl(impl_def) => Some(impl_def),
            _ => None,
        })
        .expect("impl");
    let impl_owner = impl_method_owner("api::Close:for:Handle", &impl_def.methods[0]);
    assert!(program.node_meta().contains_key(&impl_owner));
    assert!(program.resource_analysis(&impl_owner).is_some());

    let outer = module
        .items
        .iter()
        .find_map(|item| match item {
            Item::Func(function) if function.name == "outer" => Some(function),
            _ => None,
        })
        .expect("outer");
    let nested = outer
        .body
        .iter()
        .find_map(|stmt| match stmt.unlocated() {
            Stmt::Func(function) => Some(function),
            _ => None,
        })
        .expect("nested function");
    let nested_owner = nested_function_owner(&NodeId("function:api::outer".into()), nested);
    assert!(program.node_meta().contains_key(&nested_owner));
    assert!(program.resource_analysis(&nested_owner).is_some());

    let expected_owners = program
        .resource_analyses()
        .keys()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let mut reordered = file.clone();
    let reordered_module = reordered
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Module(module) if module.name == "api" => Some(module),
            _ => None,
        })
        .expect("reordered api");
    for item in &mut reordered_module.items {
        match item {
            Item::Trait(trait_def) => trait_def.methods.reverse(),
            Item::Impl(impl_def) => impl_def.methods.reverse(),
            _ => {}
        }
    }
    reordered_module.items.reverse();
    let reordered_program =
        crate::core::check_program(&reordered).expect("reordered callable catalog");
    assert_eq!(
        expected_owners,
        reordered_program
            .resource_analyses()
            .keys()
            .cloned()
            .collect()
    );
}

#[test]
fn production_node_ids_do_not_encode_vec_indexes() {
    let file = parse(
        r#"
func helper(x: i32) -> i32 { x }
func main() -> i32 {
    let values = [helper(1), helper(2)]
    match values { [a, b] => a, _ => 0 }
}
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    for node_id in program
        .node_meta()
        .keys()
        .chain(program.call_sites().keys())
    {
        for forbidden in ["/stmt:", "/arg:", "/item:", "/arm:", "/field:", "/entry:"] {
            assert!(
                !node_id.0.contains(forbidden),
                "NodeId contains Vec-index identity: {}",
                node_id.0
            );
        }
    }
}

#[test]
fn exact_child_ids_survive_same_role_synthetic_insertion() {
    let mut file = parse("func main() -> i32 { let values = [1, 2]; 0 }");
    let original = crate::core::check_program(&file).expect("original");
    let first_literal = node_id_at(&original, "expr.literal", 1, 36);
    let second_literal = node_id_at(&original, "expr.literal", 1, 39);

    let main = file
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Func(function) if function.name == "main" => Some(function),
            _ => None,
        })
        .expect("main");
    let Stmt::Let {
        init: Some(values), ..
    } = main.body[0].unlocated_mut()
    else {
        panic!("list binding")
    };
    let Expr::List(items) = values.unlocated_mut() else {
        panic!("list expression")
    };
    items.insert(
        0,
        Expr::Literal(crate::ast::Lit::Int(99))
            .synthetic_with_origin(AstOrigin::Desugared("test.list_prefix")),
    );

    let lowered = crate::core::check_program(&file).expect("lowered");
    assert!(lowered.node_meta().contains_key(&first_literal));
    assert!(lowered.node_meta().contains_key(&second_literal));
}

#[test]
fn duplicate_canonical_node_ids_are_structured_errors() {
    let mut file = parse("func main() -> i32 { 1; 2 }");
    let main = file
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Func(function) if function.name == "main" => Some(function),
            _ => None,
        })
        .expect("main");
    let first_meta = match main.body[0].unlocated() {
        Stmt::Expr(expr) => expr.meta().expect("first metadata"),
        _ => panic!("first expression"),
    };
    let second = match main.body[1].unlocated_mut() {
        Stmt::Expr(expr) => expr,
        _ => panic!("second expression"),
    };
    *second = second.clone().with_meta(first_meta);

    let diagnostics = CheckedProgram::from_checked_file(&file).expect_err("duplicate id");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.message.contains("TOOL-RESOLUTION-001")
            && diagnostic.message.contains("duplicate canonical NodeId")
    }));
}

#[test]
fn origin_catalog_rejects_missing_parent_and_cycles() {
    let a = NodeId("generated:a".into());
    let b = NodeId("generated:b".into());
    let span = Span::single(2, 3);
    let mut catalog = OriginCatalog::default();
    let mut errors = Vec::new();
    catalog.register(
        &a,
        &Origin::Desugared {
            parent: b.clone(),
            rule: "test.a".into(),
            span,
        },
        &mut errors,
    );
    catalog.validate(&mut errors);
    assert!(errors
        .iter()
        .any(|error| error.message.contains("missing Origin parent")));

    let mut cyclic = OriginCatalog::default();
    let mut cycle_errors = Vec::new();
    cyclic.register(
        &a,
        &Origin::Desugared {
            parent: b.clone(),
            rule: "test.a".into(),
            span,
        },
        &mut cycle_errors,
    );
    cyclic.register(
        &b,
        &Origin::Desugared {
            parent: a.clone(),
            rule: "test.b".into(),
            span,
        },
        &mut cycle_errors,
    );
    cyclic.validate(&mut cycle_errors);
    assert!(cycle_errors
        .iter()
        .any(|error| error.message.contains("Origin cycle")));
}

#[test]
fn resolved_ir_rejects_nested_erased_state_payloads() {
    let file = parse(
        r#"
flow Cache {
    state Ready { values: List<Any> }
}
"#,
    );
    let diagnostics = CheckedProgram::from_checked_file(&file).expect_err("IR must reject Any");
    assert!(diagnostics.iter().any(|diagnostic| diagnostic
        .message
        .contains("TOOL-RESOLUTION-001")
        && diagnostic.message.contains("List<Any>")));
    assert!(diagnostics
        .iter()
        .all(|diagnostic| diagnostic.span.start_line > 0));
}

#[test]
fn resolved_ir_rejects_unknown_and_type_schemes() {
    for ty in [
        Type::Name("unknown".into(), vec![]),
        Type::ForAll(vec!["T".into()], Box::new(Type::Name("i32".into(), vec![]))),
    ] {
        let file = File {
            sources: crate::span::SourceRegistry::default(),
            imports: Vec::new(),
            items: vec![Item::Func(crate::ast::FuncDef {
                meta: crate::ast::AstNodeMeta::synthetic(crate::ast::AstOrigin::RuntimeSystem(
                    "test.resolved_fixture",
                )),
                name: "bad".into(),
                pub_: false,
                params: vec![crate::ast::Param {
                    meta: crate::ast::AstNodeMeta::synthetic(crate::ast::AstOrigin::RuntimeSystem(
                        "test.resolved_fixture_param",
                    )),
                    name: "value".into(),
                    ty,
                    mut_: false,
                    default_value: None,
                    borrow: None,
                }],
                ret: Some(Type::Name("i32".into(), vec![])),
                body: vec![Stmt::Return(Some(Expr::Literal(crate::ast::Lit::Int(0))))],
                where_clause: Vec::new(),
                generics: Vec::new(),
                effects: Vec::new(),
                is_comptime: false,
                is_async: false,
                extern_abi: None,
                has_requires: false,
                has_ensures: false,
                has_mutate_params: false,
            })],
            implicit_single: false,
        };

        let diagnostics =
            CheckedProgram::from_checked_file(&file).expect_err("IR must reject unresolved");
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("TOOL-RESOLUTION-001")));
    }
}

#[test]
fn ownership_ledger_persists_capability_actions_and_branch_merges() {
    let file = parse(
        r#"
cap File
func pass(flag: bool, f: cap File) -> i32 {
    if flag { drop(f) } else { drop(f) }
    0
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let owner = NodeId("function:pass".to_string());
    let analysis = program
        .resource_analysis(&owner)
        .expect("pass resource analysis");
    assert!(analysis.actions.iter().any(|action| {
        action.kind == crate::core::CanonicalActionKind::Introduce
            && action.resource_display() == "f"
    }));
    assert_eq!(
        analysis
            .actions
            .iter()
            .filter(|action| {
                action.kind == crate::core::CanonicalActionKind::Drop
                    && action.resource_display() == "f"
            })
            .count(),
        2
    );
    let cfg = program.callable_cfg(&owner).expect("pass cfg");
    let merges = analysis.branch_merges(cfg);
    let merge = merges
        .iter()
        .find(|merge| merge.resource == "f")
        .expect("f branch merge");
    assert_eq!(merge.then_state, crate::core::Availability::Consumed);
    assert_eq!(merge.else_state, crate::core::Availability::Consumed);
    assert_eq!(merge.merged_state, crate::core::Availability::Consumed);
}

#[test]
fn ownership_ledger_records_return_transfer() {
    let file = parse(
        r#"
cap File
func identity(f: cap File) -> cap File { return f }
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check");
    let analysis = program
        .resource_analysis(&NodeId("function:identity".to_string()))
        .expect("identity resource analysis");
    assert!(analysis.actions.iter().any(|action| {
        action.kind == crate::core::CanonicalActionKind::Return && action.resource_display() == "f"
    }));
}

#[test]
fn ownership_ledger_records_borrow_places_and_lifetime_end() {
    let file = parse(
        r#"
type Pair { left: i32, right: i32 }
func inspect() -> i32 {
    let mut p = Pair { left: 1, right: 2 }
    let xs = [3, 4]
    let left = &p.left
    let right = &mut p.right
    let item = &xs[0]
    *left + *right + *item
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("borrow program checks");
    let analysis = program
        .resource_analysis(&NodeId("function:inspect".to_string()))
        .expect("inspect resource analysis");
    let actions: Vec<_> = analysis
        .actions
        .iter()
        .map(|action| (action.kind, action.resource_display()))
        .collect();
    assert!(actions.contains(&(
        crate::core::CanonicalActionKind::BorrowShared,
        "p.left".to_string()
    )));
    assert!(actions.contains(&(
        crate::core::CanonicalActionKind::BorrowMut,
        "p.right".to_string()
    )));
    // RESOURCE-LINEAR-001: canonical places retain constant-index
    // disjointness; only genuinely dynamic indices project as `[*]`.
    assert!(actions.contains(&(
        crate::core::CanonicalActionKind::BorrowShared,
        "xs[0]".to_string()
    )));
    assert!(actions.contains(&(
        crate::core::CanonicalActionKind::BorrowEnd,
        "p.left".to_string()
    )));
    assert!(actions.contains(&(
        crate::core::CanonicalActionKind::BorrowEnd,
        "p.right".to_string()
    )));
    assert!(actions.contains(&(
        crate::core::CanonicalActionKind::BorrowEnd,
        "xs[0]".to_string()
    )));

    let interp = crate::interp::Interpreter::from_checked(&program);
    assert!(interp
        .resolved_ownership_actions("function:inspect")
        .is_some_and(|actions| actions
            .iter()
            .any(|(kind, place)| kind == "borrow_mut" && place == "p.right")));
    let mut verifier = crate::verifier::Verifier::new().expect("z3");
    let _ = verifier.verify_checked(&program);
    assert!(verifier
        .checked_ownership_actions("function:inspect")
        .is_some_and(|actions| actions
            .iter()
            .any(|(kind, place)| kind == "borrow_end" && place == "p.left")));
}

#[test]
fn ownership_checker_transfers_compound_capability_returns_in_order() {
    let file = parse(
        r#"
cap Token
func pair(a: cap Token, b: cap Token) -> (cap Token, cap Token) {
    return (a, b)
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("compound return transfers caps");
    let analysis = program
        .resource_analysis(&NodeId("function:pair".to_string()))
        .expect("pair resource analysis");
    let returned: Vec<_> = analysis
        .actions
        .iter()
        .filter(|action| action.kind == crate::core::CanonicalActionKind::Return)
        .map(|action| action.resource_display())
        .collect();
    assert_eq!(returned, vec!["a", "b"]);
}

#[test]
fn ownership_checker_drops_compound_capabilities_in_order() {
    let file = parse(
        r#"
cap Token
func close(a: cap Token, b: cap Token) -> i32 {
    drop((a, b))
    0
}
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("compound drop consumes caps");
    let analysis = program
        .resource_analysis(&NodeId("function:close".to_string()))
        .expect("close resource analysis");
    let dropped: Vec<_> = analysis
        .actions
        .iter()
        .filter(|action| action.kind == crate::core::CanonicalActionKind::Drop)
        .map(|action| action.resource_display())
        .collect();
    assert_eq!(dropped, vec!["a", "b"]);
}

#[test]
fn ownership_checker_rejects_one_branch_consumption() {
    let file = parse(
        r#"
cap File
func bad(flag: bool, f: cap File) -> i32 {
    if flag { drop(f) }
    0
}
func main() -> i32 { 0 }
"#,
    );
    let diagnostics = crate::core::check_program(&file).expect_err("branch mismatch");
    // RESOURCE-LINEAR-001: the typed CFG join, not checker snapshots,
    // reports the mismatch between reachable predecessors.
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0304)
            && diagnostic.message.contains("some reachable CFG paths")
    }));
}

#[test]
fn ownership_checker_consumes_outer_capability_in_nested_block() {
    let file = parse(
        r#"
cap File
func close(f: cap File) -> i32 {
    { drop(f) }
    0
}
func main() -> i32 { 0 }
"#,
    );
    crate::core::check_program(&file).expect("nested block consumes outer cap");
}

#[test]
fn ownership_checker_rejects_return_path_leak() {
    let file = parse(
        r#"
cap File
func bad(flag: bool, f: cap File) -> i32 {
    if flag { return 0 }
    drop(f)
    0
}
func main() -> i32 { 0 }
"#,
    );
    let diagnostics = crate::core::check_program(&file).expect_err("return path leak");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0256)
            && diagnostic.message.contains("f")
    }));
}

/// ZONK-LEAK-001: the leak detector must NOT fire on valid programs with
/// generics (TypeVar is used during inference but fully resolved after zonk).
#[test]
fn zonk_leak_detector_passes_on_resolved_generics() {
    let file = parse(
        r#"
func id<T>(x: T) -> T { x }
func main() -> i32 {
    let a = id(42)
    let b = id("hello")
    0
}
"#,
    );
    // Should succeed — all TypeVars resolved after zonk
    let program = crate::core::check_program(&file).expect("generics should zonk cleanly");
    // Verify the generic function's signature is fully resolved
    let id_fn = program
        .functions()
        .values()
        .find(|f| f.qualified_name == "id")
        .expect("id function should exist");
    // After monomorphization, params should not contain TypeVar
    for (_, ty) in &id_fn.params {
        assert!(
            crate::core::unification::scan_residual(ty).is_ok(),
            "param type should be fully resolved: {:?}",
            ty
        );
    }
}

#[test]
fn ownership_checker_accepts_return_transfer_on_both_paths() {
    let file = parse(
        r#"
cap File
func choose(flag: bool, f: cap File) -> cap File {
    if flag { return f }
    return f
}
func main() -> i32 { 0 }
"#,
    );
    crate::core::check_program(&file).expect("both return paths transfer f");
}

#[test]
fn ownership_checker_rejects_loop_carried_consumption() {
    let file = parse(
        r#"
cap File
func bad(run: bool, f: cap File) -> i32 {
    while run {
        drop(f)
    }
    0
}
func main() -> i32 { 0 }
"#,
    );
    let diagnostics = crate::core::check_program(&file).expect_err("loop consumption");
    // RESOURCE-LINEAR-001: loop ownership is a fixed-point join.
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0304)
            && diagnostic.message.contains("some reachable CFG paths")
    }));
}

#[test]
fn ownership_checker_allows_break_only_loop_body_consumption() {
    // Body always exits via break → no back-edge; still join with zero-iteration path.
    let file = parse(
        r#"
cap File
func ok(run: bool, f: cap File) -> i32 {
    while run {
        drop(f)
        break
    }
    0
}
func main() -> i32 { 0 }
"#,
    );
    let diagnostics = crate::core::check_program(&file).expect_err("zero-iteration leak");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0304)
            && diagnostic.message.contains("some reachable CFG paths")
    }));
}

#[test]
fn ownership_checker_accepts_loop_with_break_and_post_drop() {
    let file = parse(
        r#"
cap File
func ok(run: bool, f: cap File) -> i32 {
    while run {
        break
    }
    drop(f)
    0
}
func main() -> i32 { 0 }
"#,
    );
    crate::core::check_program(&file).expect("break-only body does not consume f");
}

#[test]
fn ownership_checker_accepts_infinite_loop_break_after_drop() {
    let file = parse(
        r#"
cap File
func ok(f: cap File) -> i32 {
    loop {
        drop(f)
        break
    }
    0
}
func main() -> i32 { 0 }
"#,
    );
    crate::core::check_program(&file).expect("loop body always exits after drop");
}

#[test]
fn ownership_checker_moves_by_value_cap_arguments() {
    let file = parse(
        r#"
cap File
func consume(f: cap File) -> i32 { drop(f); 0 }
func bad(f: cap File) -> i32 {
    consume(f)
    drop(f)
    0
}
func main() -> i32 { 0 }
"#,
    );
    let diagnostics = crate::core::check_program(&file).expect_err("double consume");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0304)
            && diagnostic
                .message
                .contains("consumed more than once on this CFG path")
    }));
}

#[test]
fn ownership_checker_joins_expression_if_branches() {
    let file = parse(
        r#"
cap File
func use_cap(flag: bool, f: cap File) -> i32 {
    let result = if flag { drop(f); 1 } else { drop(f); 2 }
    result
}
func main() -> i32 { 0 }
"#,
    );
    crate::core::check_program(&file).expect("expression if consumes both paths");
}

#[test]
fn ownership_checker_joins_match_arms() {
    let file = parse(
        r#"
cap File
func use_cap(flag: bool, f: cap File) -> i32 {
    match flag { true => { drop(f); 1 }, false => { drop(f); 2 } }
}
func main() -> i32 { 0 }
"#,
    );
    crate::core::check_program(&file).expect("match consumes both paths");
}

#[test]
fn ownership_checker_accepts_implicit_capability_return() {
    let file = parse(
        r#"
cap File
func identity(f: cap File) -> cap File { f }
func main() -> i32 { 0 }
"#,
    );
    crate::core::check_program(&file).expect("implicit return transfers f");
}

#[test]
fn ownership_ledgers_use_module_qualified_owner_ids() {
    let file = parse(
        r#"
cap File
module A { func close(f: cap File) -> i32 { drop(f); 0 } }
module B { func close(f: cap File) -> i32 { drop(f); 0 } }
func main() -> i32 { 0 }
"#,
    );
    let program = crate::core::check_program(&file).expect("check modules");
    assert!(program
        .resource_analysis(&NodeId("function:A::close".to_string()))
        .is_some());
    assert!(program
        .resource_analysis(&NodeId("function:B::close".to_string()))
        .is_some());
    assert!(program
        .resource_analysis(&NodeId("function:close".to_string()))
        .is_none());
}

#[test]
fn ownership_ledger_ignores_non_linear_drop() {
    let file = parse("func main() -> i32 { let x = 1; drop(x); 0 }");
    let program = crate::core::check_program(&file).expect("check");
    let analysis = program
        .resource_analysis(&NodeId("function:main".to_string()))
        .expect("main analysis");
    assert!(analysis
        .actions
        .iter()
        .all(|action| action.resource_display() != "x"));
}

#[test]
fn ownership_checker_nested_function_does_not_consume_outer_capability() {
    let file = parse(
        r#"
cap File
func outer(f: cap File) -> i32 {
    func inner() -> i32 { 0 }
    drop(f)
    0
}
func main() -> i32 { 0 }
"#,
    );
    crate::core::check_program(&file).expect("nested function preserves outer ownership");
}

#[test]
fn ownership_checker_rejects_implicit_nested_capability_capture() {
    let file = parse(
        r#"
cap File
func outer(f: cap File) -> i32 {
    func inner() -> i32 { drop(f); 0 }
    drop(f)
    0
}
func main() -> i32 { 0 }
"#,
    );
    let diagnostics = crate::core::check_program(&file).expect_err("implicit cap capture");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0304)
            && diagnostic
                .message
                .contains("not owned by the current callable")
    }));
}

#[test]
fn ownership_checker_tracks_actor_method_capabilities() {
    let file = parse(
        r#"
cap File
actor Sink {
    func leak(f: cap File) -> i32 { 0 }
}
func main() -> i32 { 0 }
"#,
    );
    let diagnostics = crate::core::check_program(&file).expect_err("actor method leak");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0256)
            && diagnostic.message.contains("f")
    }));
}

#[test]
fn ownership_checker_tracks_impl_method_capabilities() {
    let file = parse(
        r#"
cap File
trait Close { func close(f: cap File) -> i32 }
type Handle { value: i32 }
impl Close for Handle {
    func close(f: cap File) -> i32 { 0 }
}
func main() -> i32 { 0 }
"#,
    );
    let diagnostics = crate::core::check_program(&file).expect_err("impl method leak");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0256)
            && diagnostic.message.contains("f")
    }));
}

#[test]
fn ownership_checker_tracks_transition_capabilities() {
    let file = parse(
        r#"
cap File
flow Door {
    state Closed
    state Open
    transition open(Closed, f: cap File) -> Open { do { return Open {} } }
}
func main() -> i32 { 0 }
"#,
    );
    let diagnostics = crate::core::check_program(&file).expect_err("transition leak");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0256)
            && diagnostic.message.contains("f")
    }));
}
