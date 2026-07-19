use std::collections::{BTreeSet, VecDeque};

use crate::diagnostic::Diagnostic;

use super::CallableCfg;

pub fn validate_cfg(cfg: &CallableCfg) -> Result<(), Vec<Diagnostic>> {
    let mut errors = Vec::new();
    if !cfg.blocks.contains_key(&cfg.entry) {
        errors.push(Diagnostic::error(
            format!("CFG owner '{}' has no entry block", cfg.owner.0),
            crate::span::Span::UNKNOWN,
        ));
    }

    for (id, block) in &cfg.blocks {
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
    }

    for edge in cfg.edges.values() {
        if !cfg.blocks.contains_key(&edge.from) || !cfg.blocks.contains_key(&edge.to) {
            errors.push(Diagnostic::error(
                format!("CFG edge '{}' references a missing block", edge.id.0 .0),
                edge.source.span,
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
