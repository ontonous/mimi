use super::*;

fn parse(source: &str) -> crate::ast::File {
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse")
}

#[test]
fn cfg_if_loop_and_terminal_edges_validate() {
    let file = parse(
        r#"
func choose(mut x: i32) -> i32 {
    while x > 0 {
        if x == 2 { break }
        if x == 1 { continue }
        x = x - 1
    }
    if x == 0 { return 10 } else { return 20 }
}
"#,
    );
    let cfgs = lower_file(&file).expect("lower CFGs");
    let cfg = cfgs
        .get(&crate::core::NodeId("function:choose".into()))
        .expect("choose CFG");
    cfg.validate().expect("valid CFG");
    assert!(cfg
        .edges
        .values()
        .any(|edge| edge.kind == EdgeKind::Backedge));
    assert!(cfg.edges.values().any(|edge| edge.kind == EdgeKind::Break));
    assert!(cfg
        .edges
        .values()
        .any(|edge| edge.kind == EdgeKind::Continue));
    assert!(
        cfg.blocks
            .values()
            .filter(|block| matches!(block.terminator, Terminator::Return { .. }))
            .count()
            >= 2
    );
}

#[test]
fn cfg_ids_are_stable_when_unrelated_functions_reorder() {
    let first = parse(
        r#"
func helper() -> i32 { 1 }
func target(x: i32) -> i32 { if x > 0 { x } else { 0 } }
"#,
    );
    let mut second = first.clone();
    second.items.swap(0, 1);
    let owner = crate::core::NodeId("function:target".into());
    let left = lower_file(&first).expect("first CFGs");
    let right = lower_file(&second).expect("second CFGs");
    let left = left.get(&owner).expect("first target");
    let right = right.get(&owner).expect("second target");
    assert_eq!(
        left.blocks.keys().collect::<Vec<_>>(),
        right.blocks.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        left.edges.keys().collect::<Vec<_>>(),
        right.edges.keys().collect::<Vec<_>>()
    );
}

#[test]
fn cfg_catalog_covers_all_callable_kinds() {
    let file = parse(
        r#"
trait Close { func close() -> i32 }
type Handle { value: i32 }
impl Close for Handle { func close() -> i32 { 0 } }
actor Counter { count: i32 func inc() -> i32 { 1 } }
flow Door {
    state Closed
    state Open
    transition toggle(Closed) -> Open { do { return Open {} } }
}
func outer() -> i32 {
    func nested() -> i32 { 1 }
    nested()
}
"#,
    );
    let cfgs = lower_file(&file).expect("lower all callables");
    assert!(cfgs.contains_key(&crate::core::NodeId("function:Counter::inc".into())));
    assert!(cfgs
        .keys()
        .any(|owner| owner.0.starts_with("function:Close:for:Handle::close:")));
    assert!(cfgs.contains_key(&crate::core::NodeId(
        "transition:Door::toggle::Closed".into()
    )));
    assert!(cfgs
        .keys()
        .any(|owner| owner.0.starts_with("function:outer/function:nested:")));
}

#[test]
fn checked_program_persists_validated_cfgs() {
    let file = parse("func main() -> i32 { if true { 1 } else { 2 } }");
    let program = crate::core::check_program(&file).expect("checked program");
    let owner = crate::core::NodeId("function:main".into());
    let cfg = program.callable_cfg(&owner).expect("main CFG");
    assert!(program.callable_cfgs().contains_key(&owner));
    assert!(cfg.reachable.contains(&cfg.entry));
    cfg.validate().expect("persisted CFG validates");
}

#[test]
fn checked_cfg_is_owned_and_traces_resolved_control_nodes() {
    let program = {
        let file = parse(
            "func choose(flag: bool) -> i32 { if flag { 1 } else { 2 } }\nfunc main() -> i32 { choose(true) }",
        );
        crate::core::check_program(&file).expect("checked program")
    };
    let owner = crate::core::NodeId("function:choose".into());
    let body = program.resolved_body(&owner).expect("owned body");
    let cfg = program.callable_cfg(&owner).expect("owned CFG");
    assert_eq!(cfg.owner, body.owner);
    assert_eq!(
        cfg.block(&cfg.entry).map(|block| &block.source.node),
        Some(&body.root.node_id)
    );
    let result = body.root.result.as_deref().expect("resolved if result");
    let crate::core::ResolvedExprKind::If {
        condition,
        then_block,
        else_block,
    } = &result.kind
    else {
        panic!("resolved if result expected");
    };
    let branch = cfg
        .blocks
        .values()
        .find_map(|block| match &block.terminator {
            Terminator::Branch {
                condition,
                then_edge,
                else_edge,
            } => Some((condition, then_edge, else_edge)),
            _ => None,
        })
        .expect("typed branch");
    assert_eq!(branch.0, &condition.node_id);
    assert_eq!(
        cfg.edge(branch.1)
            .and_then(|edge| cfg.block(&edge.to))
            .map(|block| &block.source.node),
        Some(&then_block.node_id)
    );
    assert_eq!(
        cfg.edge(branch.2)
            .and_then(|edge| cfg.block(&edge.to))
            .map(|block| &block.source.node),
        Some(&else_block.node_id)
    );
    assert!(cfg.blocks.values().any(|block| {
        block
            .points
            .iter()
            .any(|point| point.source.node == result.node_id)
    }));
}

#[test]
fn checked_cfg_catalog_matches_owned_body_catalog_and_reorders_stably() {
    let first = parse(
        "func helper() -> i32 { 1 }\nfunc target(x: i32) -> i32 { if x > 0 { x } else { 0 } }",
    );
    let mut reordered = first.clone();
    reordered.items.swap(0, 1);
    let first = crate::core::check_program(&first).expect("first checked program");
    let reordered = crate::core::check_program(&reordered).expect("reordered checked program");
    assert_eq!(
        first.callable_cfgs().keys().collect::<Vec<_>>(),
        first.resolved_bodies().keys().collect::<Vec<_>>()
    );
    let owner = crate::core::NodeId("function:target".into());
    let left = first.callable_cfg(&owner).expect("first target CFG");
    let right = reordered
        .callable_cfg(&owner)
        .expect("reordered target CFG");
    assert_eq!(
        left.blocks.keys().collect::<Vec<_>>(),
        right.blocks.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        left.edges.keys().collect::<Vec<_>>(),
        right.edges.keys().collect::<Vec<_>>()
    );
}

#[test]
fn nested_control_flow_roles_disambiguate_inherited_spans() {
    let file = parse(
        r#"
func classify(value: i32) -> i32 {
    if value > 10 {
        2
    } else {
        if value > 0 { 1 } else { 0 }
    }
}
"#,
    );
    let cfgs = lower_file(&file).expect("nested controls have distinct semantic roles");
    let cfg = cfgs
        .get(&crate::core::NodeId("function:classify".into()))
        .expect("classify CFG");
    cfg.validate().expect("nested CFG validates");
    assert_eq!(
        cfg.blocks.len(),
        cfg.blocks.keys().collect::<BTreeSet<_>>().len()
    );
    assert_eq!(
        cfg.edges.len(),
        cfg.edges.keys().collect::<BTreeSet<_>>().len()
    );
}
