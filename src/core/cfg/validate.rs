use std::collections::{BTreeSet, VecDeque};

use crate::diagnostic::Diagnostic;

use super::{CallableCfg, CfgSource, Terminator};

fn validate_source(
    owner: &crate::core::NodeId,
    role: &str,
    source: &CfgSource,
    errors: &mut Vec<Diagnostic>,
) {
    if source.node.0.trim().is_empty() {
        errors.push(Diagnostic::error(
            format!("CFG {role} for '{}' has an empty source NodeId", owner.0),
            source.span,
        ));
    }
    if source.span != source.origin.user_span() {
        errors.push(Diagnostic::error(
            format!(
                "CFG {role} source '{}' disagrees with its owned Origin span",
                source.node.0
            ),
            source.span,
        ));
    }
}

pub fn validate_cfg(cfg: &CallableCfg) -> Result<(), Vec<Diagnostic>> {
    let mut errors = Vec::new();
    if cfg.owner.0.trim().is_empty() {
        errors.push(Diagnostic::error(
            "CFG callable owner is empty".to_string(),
            crate::span::Span::UNKNOWN,
        ));
    }
    if !cfg.blocks.contains_key(&cfg.entry) {
        errors.push(Diagnostic::error(
            format!("CFG owner '{}' has no entry block", cfg.owner.0),
            crate::span::Span::UNKNOWN,
        ));
    }

    let mut point_nodes = BTreeSet::new();
    let mut terminator_nodes = Vec::new();
    for (id, block) in &cfg.blocks {
        validate_source(&cfg.owner, "block", &block.source, &mut errors);
        if id != &block.id {
            errors.push(Diagnostic::error(
                format!(
                    "CFG block key does not match block id '{}': owner {}",
                    id.0 .0, cfg.owner.0
                ),
                block.source.span,
            ));
        }
        let declared: BTreeSet<_> = block
            .terminator
            .outgoing_edges()
            .into_iter()
            .cloned()
            .collect();
        let actual: BTreeSet<_> = cfg
            .edges
            .values()
            .filter(|edge| edge.from == *id)
            .map(|edge| edge.id.clone())
            .collect();
        if declared != actual {
            errors.push(Diagnostic::error(
                format!(
                    "CFG block '{}' terminator edges do not match edge table",
                    id.0 .0
                ),
                block.source.span,
            ));
        }
        for point in &block.points {
            validate_source(&cfg.owner, "point", &point.source, &mut errors);
            if !point_nodes.insert(point.source.node.clone()) {
                errors.push(Diagnostic::error(
                    format!(
                        "CFG point source '{}' occurs more than once in callable '{}'",
                        point.source.node.0, cfg.owner.0
                    ),
                    point.source.span,
                ));
            }
        }
        match &block.terminator {
            Terminator::Branch { condition, .. } => {
                terminator_nodes.push((condition.clone(), block.source.span, "branch condition"))
            }
            Terminator::Match { scrutinee, .. } => {
                terminator_nodes.push((scrutinee.clone(), block.source.span, "match scrutinee"))
            }
            Terminator::Return {
                value: Some(value), ..
            } => terminator_nodes.push((value.clone(), block.source.span, "return value")),
            Terminator::Goto { .. }
            | Terminator::Return { value: None, .. }
            | Terminator::Break { .. }
            | Terminator::Continue { .. }
            | Terminator::Diverge
            | Terminator::Unreachable => {}
        }
    }

    for (id, edge) in &cfg.edges {
        validate_source(&cfg.owner, "edge", &edge.source, &mut errors);
        if id != &edge.id {
            errors.push(Diagnostic::error(
                format!("CFG edge key does not match edge id '{}'", id.0 .0),
                edge.source.span,
            ));
        }
        if !cfg.blocks.contains_key(&edge.from) || !cfg.blocks.contains_key(&edge.to) {
            errors.push(Diagnostic::error(
                format!("CFG edge '{}' references a missing block", edge.id.0 .0),
                edge.source.span,
            ));
        }
    }
    for (node, span, role) in terminator_nodes {
        if !point_nodes.contains(&node) {
            errors.push(Diagnostic::error(
                format!(
                    "CFG {role} '{}' does not reference a callable point",
                    node.0
                ),
                span,
            ));
        }
    }

    let mut reachable = BTreeSet::new();
    let mut queue = VecDeque::from([cfg.entry.clone()]);
    while let Some(block) = queue.pop_front() {
        if !reachable.insert(block.clone()) {
            continue;
        }
        for edge in cfg.successors(&block) {
            queue.push_back(edge.to.clone());
        }
    }
    if reachable != cfg.reachable {
        errors.push(Diagnostic::error(
            format!(
                "CFG owner '{}' has a stale reachable-block catalog",
                cfg.owner.0
            ),
            cfg.blocks
                .get(&cfg.entry)
                .map(|block| block.source.span)
                .unwrap_or(crate::span::Span::UNKNOWN),
        ));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
