//! Mechanical alignment of the Checker scalar AST commitment with MIR.
use super::*;
use crate::core::cfg::BasicBlockId;
use crate::core::ownership::{
    OrdinaryMapOwnedAnyV2CfgFlowReceipt as Flow, OrdinaryMapOwnedAnyV2ScalarTailNodeKind as Source,
    OrdinaryMapOwnedAnyV2ScalarTailReceipt as Receipt,
};

pub(super) fn allows_unary(
    function: &MirFunction,
    result: &MirValueId,
    op: ResolvedUnaryOp,
    operand: &MirValueId,
) -> bool {
    let Some(flow) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
        .and_then(|receipt| receipt.flow.as_ref())
    else {
        return false;
    };
    let crate::core::ownership::OrdinaryMapOwnedAnyV2CfgFlowProof::SettledScalarBranchTailV1(tail) =
        &flow.proof
    else {
        return false;
    };
    // ZeroMiss's effectful stdout observation keeps its existing specialized
    // island gate. Its exact route, rather than the generic scalar envelope,
    // owns Boolean Not in that profile.
    tail.zero_miss_stdout.is_none()
        && function
            .ownership
            .checker_managed_any
            .as_ref()
            .and_then(|seal| seal.v2_scalar_tail.as_ref())
            == Some(tail)
        && tail.nodes.iter().any(|(node, source)| {
            result.as_str() == format!("expr:{}", node.0)
                && matches!(&source.kind, Source::Unary(expected, value)
                    if *expected == op && operand.as_str() == format!("expr:{}", value.0))
        })
}

pub(super) fn validate(
    function: &MirFunction,
    flow: &Flow,
    tail: &Receipt,
    blocks: &BTreeMap<BasicBlockId, MirBlockId>,
) -> Vec<MirValidationError> {
    let mut errors = Vec::new();
    let mut fail = |message: &str| {
        errors.push(MirValidationError {
            subject: function.owner.0.clone(),
            message: format!("ordinary Map/Any v2 CFG flow receipt {message}"),
        })
    };
    let Some(seal) = function.ownership.checker_managed_any.as_ref() else {
        fail("scalar-tail source AST has no private Checker commitment");
        return errors;
    };
    if seal.v2_scalar_tail.as_ref() != Some(tail) || tail.owner != function.owner {
        fail("scalar-tail source AST disagrees with its private Checker commitment");
        return errors;
    }
    let expression = |node: &NodeId| {
        MirValueId::new(format!("expr:{}", node.0)).expect("expr MirValueId prefix is non-empty")
    };
    let local = |node: &ResolvedLocalId| {
        MirValueId::new(format!("local:{}", node.0 .0))
            .expect("local MirValueId prefix is non-empty")
    };
    let locations = flow
        .reachable_blocks
        .iter()
        .flat_map(|(block, body)| {
            body.points
                .iter()
                .enumerate()
                .map(move |(rank, (node, _))| (node.clone(), (block.clone(), rank)))
        })
        .collect::<BTreeMap<_, _>>();
    let mut allowed = BTreeSet::new();
    for local_id in tail.owned_uses.keys() {
        if !function.values.contains_key(&local(local_id)) {
            fail("scalar-tail source owner has no MIR definition or parameter");
        }
    }
    for (binding, drop) in &tail.string_drops {
        let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        let value = local(binding);
        let source_point = flow
            .reachable_blocks
            .get(&flow.entry)
            .and_then(|block| block.points.get(drop.point_rank));
        let exact_source = source_point.is_some_and(|(point, kind)| {
            point == &drop.point && *kind == crate::core::cfg::CfgPointKind::ResourceAction
        }) && drop.point_rank < tail.first_condition_rank
            && drop.loads.iter().all(|(point, rank)| {
                locations.get(point) == Some(&(flow.entry.clone(), *rank))
                    && *rank < drop.point_rank
            })
            && tail.owned_uses.get(binding).is_some_and(|uses| {
                *uses
                    == drop
                        .loads
                        .iter()
                        .map(|(node, _)| node.clone())
                        .chain(std::iter::once(drop.point.clone()))
                        .collect()
            });
        let id = format!("inst:drop.{}:{}", drop.place_index, drop.point.0);
        let matches = seal.instructions.iter().filter(|(block, origin, instruction)| {
            Some(block) == blocks.get(&flow.entry) && origin == &drop.point
                && instruction.id.as_str() == id
                && matches!(&instruction.kind, MirInstructionKind::Drop { value: actual } if actual == &value)
        }).collect::<Vec<_>>();
        if !exact_source
            || drop.ty != string_type
            || function
                .values
                .get(&value)
                .is_none_or(|value| value.ty != string_type)
            || matches.len() != 1
        {
            fail(
                "scalar-tail source String Drop lacks its exact prefix point/index/local instruction",
            );
            continue;
        }
        let (drop_block, _, instruction) = matches[0];
        allowed.insert(instruction.id.clone());
        let Some(body) = function.blocks.get(drop_block) else {
            fail("scalar-tail source String Drop has no exact MIR block");
            continue;
        };
        let Some(drop_index) = body
            .instructions
            .iter()
            .position(|actual| actual == instruction)
        else {
            fail("scalar-tail source String Drop is absent from the MIR prefix");
            continue;
        };
        // Source point ranks alone cannot protect a resealed instruction
        // vector. Preserve the exact Drop's position relative to every source
        // operation on either side. This includes borrowed key/equality reads
        // whose source Load is elided and whose MIR origin is their consumer.
        let source_order = seal.instructions.iter().all(|(block, origin, actual)| {
            if actual.id == instruction.id {
                return true;
            }
            let Some((source_block, rank)) = locations.get(origin) else {
                return true;
            };
            if source_block != &flow.entry || block != drop_block {
                return true;
            }
            let Some(index) = body
                .instructions
                .iter()
                .position(|candidate| candidate == actual)
            else {
                return false;
            };
            if *rank < drop.point_rank {
                index < drop_index
            } else if *rank > drop.point_rank {
                index > drop_index
            } else {
                !mir_instruction_value_uses(&actual.kind).contains(&value)
            }
        });
        let actual_reads_before_drop = function.blocks.iter().all(|(block, body)| {
            body.instructions.iter().enumerate().all(|(index, actual)| {
                actual.id == instruction.id
                    || !mir_instruction_value_uses(&actual.kind).contains(&value)
                    || (block == drop_block && index < drop_index)
            }) && !mir_terminator_value_uses(&body.terminator).contains(&value)
        });
        if !source_order || !actual_reads_before_drop {
            fail(
                "scalar-tail source String Drop must follow every exact source operation and MIR read of its local",
            );
        }
    }
    for (node, source) in &tail.nodes {
        let result = expression(node);
        if function
            .values
            .get(&result)
            .is_none_or(|value| value.ty != source.ty)
        {
            fail("scalar-tail source expression has a different result type");
            continue;
        }
        let Some((source_block, _)) = locations.get(node) else {
            fail("scalar-tail source expression has no exact CFG point");
            continue;
        };
        let Some(mir_block) = blocks
            .get(source_block)
            .and_then(|id| function.blocks.get(id))
        else {
            fail("scalar-tail source expression has no MIR block");
            continue;
        };
        if matches!(&source.kind, Source::Join) {
            if mir_block.parameters.as_slice() != [MirBlockParameter { value: result }] {
                fail("scalar-tail source join has a different result parameter");
            }
            continue;
        }
        let producers = mir_block
            .instructions
            .iter()
            .filter(|instruction| lower::instruction_results(&instruction.kind).contains(&&result))
            .collect::<Vec<_>>();
        let [instruction] = producers.as_slice() else {
            fail("scalar-tail source expression has no unique MIR producer");
            continue;
        };
        let exact_origin = seal
            .instructions
            .iter()
            .filter(|(block, origin, sealed)| {
                block == &mir_block.id && origin == node && sealed.id == instruction.id
            })
            .count()
            == 1;
        let matches = match (&source.kind, &instruction.kind) {
            (
                Source::Literal(expected),
                MirInstructionKind::Const {
                    result: actual,
                    literal,
                },
            ) => actual == &result && literal == expected,
            (
                Source::Load(binding),
                MirInstructionKind::Clone {
                    result: actual,
                    source,
                },
            ) => {
                actual == &result
                    && source == &local(binding)
                    && function
                        .values
                        .get(source)
                        .is_some_and(|value| value.ty == tail.nodes[node].ty)
            }
            (
                Source::Unary(expected, operand),
                MirInstructionKind::Unary {
                    result: actual,
                    op,
                    operand: actual_operand,
                },
            ) => actual == &result && op == expected && actual_operand == &expression(operand),
            (
                Source::Binary(expected, left, right),
                MirInstructionKind::Binary {
                    result: actual,
                    op,
                    left: actual_left,
                    right: actual_right,
                },
            ) => {
                actual == &result
                    && op == expected
                    && actual_left == &expression(left)
                    && actual_right == &expression(right)
            }
            _ => false,
        };
        if !matches || !exact_origin {
            fail("scalar-tail source expression disagrees with its MIR operation or operands");
        }
        allowed.insert(instruction.id.clone());
    }
    for (binding, (point, initializer)) in &tail.bindings {
        let expected = MirInstructionKind::Move {
            result: local(binding),
            source: expression(initializer),
        };
        let matches = seal
            .instructions
            .iter()
            .filter(|(_, origin, instruction)| origin == point && instruction.kind == expected)
            .collect::<Vec<_>>();
        if let [(block, _, instruction)] = matches.as_slice() {
            if function
                .blocks
                .get(block)
                .is_none_or(|body| !body.instructions.contains(instruction))
            {
                fail("scalar-tail source binding is absent from MIR");
            }
            allowed.insert(instruction.id.clone());
        } else {
            fail("scalar-tail source binding has no exact scalar Move");
        }
    }
    let mut seen_selectors = BTreeSet::new();
    if let Some((call, argument)) = &tail.zero_miss_stdout {
        let exact_observation = function
            .ownership
            .checker_ordinary_map_owned_any_v2_cfg_receipt
            .as_ref()
            .and_then(|receipt| receipt.zero_miss_i32_observation_receipt.as_ref())
            .is_some_and(|observation| {
                observation.owner == tail.owner && observation.return_node == tail.return_node
            });
        let prints = seal.instructions.iter().filter(|(_, origin, instruction)| {
            origin == call && matches!(&instruction.kind, MirInstructionKind::BuiltinCall { result, kind: types::MirBuiltinKind::PrintlnBool, arguments, string_field_contract: None }
                if result == &expression(call) && arguments == &[expression(argument)])
        }).collect::<Vec<_>>();
        if exact_observation && prints.len() == 1 {
            allowed.insert(prints[0].2.id.clone());
        } else {
            fail("scalar-tail ZeroMiss stdout lacks its exact independent observation");
        }
    }
    for (construct, source) in &tail.selectors {
        let selector = &source.selector;
        let Some(block) = blocks
            .get(&selector.block)
            .and_then(|id| function.blocks.get(id))
        else {
            fail("scalar-tail source selector has no MIR block");
            continue;
        };
        if !matches!(&block.terminator, MirTerminator::Branch { condition, then_arguments, else_arguments, .. }
            if condition == &expression(&selector.condition) && then_arguments.is_empty() && else_arguments.is_empty())
        {
            fail("scalar-tail source selector operands differ from MIR");
        }
        seen_selectors.insert(block.id.clone());
        let Some((join, _)) = locations.get(construct) else {
            fail("scalar-tail source selector has no join");
            continue;
        };
        for (arm_source, value) in [
            (&source.then_source, &source.then_value),
            (&source.else_source, &source.else_value),
        ] {
            let edges = flow
                .reachable_edges
                .values()
                .filter(|edge| &edge.to == join && &edge.source.node == arm_source)
                .collect::<Vec<_>>();
            let [edge] = edges.as_slice() else {
                fail("scalar-tail source arm has no unique join edge");
                continue;
            };
            let arm = blocks
                .get(&edge.from)
                .and_then(|id| function.blocks.get(id));
            if !arm.is_some_and(|block| {
                matches!(&block.terminator, MirTerminator::Goto { target, arguments, .. }
                if Some(target) == blocks.get(join) && arguments == &[expression(value)])
            }) {
                fail("scalar-tail source arm result differs from its MIR edge argument");
            }
        }
    }
    let returns = function
        .blocks
        .values()
        .filter(|block| matches!(block.terminator, MirTerminator::Return { .. }))
        .collect::<Vec<_>>();
    if !matches!(returns.as_slice(), [block]
        if Some(&block.id) == blocks.get(&tail.return_block)
            && function.result == tail.return_type
            && matches!(&block.terminator, MirTerminator::Return { value: Some(value) } if value == &expression(&tail.return_node)))
    {
        fail("scalar-tail source return differs from the unique MIR return operand or type");
    }
    if seen_selectors.len()
        != function
            .blocks
            .values()
            .filter(|block| matches!(block.terminator, MirTerminator::Branch { .. }))
            .count()
    {
        fail("scalar-tail source selector census does not cover MIR");
    }
    // The tail contains exactly the scalar producers/bindings above. Even a
    // freshly resealed owner Drop or a scalar-looking opaque Call is rejected.
    for (block, origin, instruction) in &seal.instructions {
        if allowed.contains(&instruction.id) {
            continue;
        }
        // Local String settlements are authorized only by the exact AST Drop
        // above. A second instruction with a fresh ID and the same valid
        // source origin must not gain ownership authority from prefix location.
        if matches!(&instruction.kind, MirInstructionKind::Drop { value }
            if tail.string_drops.keys().any(|binding| local(binding) == *value))
        {
            fail("scalar-tail source String Drop has an additional unauthorized local settlement");
            continue;
        }
        let prefix = locations.get(origin).is_some_and(|(source_block, rank)| {
            source_block == &flow.entry
                && *rank < tail.first_condition_rank
                && blocks.get(source_block) == Some(block)
        });
        if !prefix {
            fail(&format!(
                "scalar-tail source suffix contains extra work or tail cleanup: block={} origin={} instruction={:?}",
                block, origin.0, instruction
            ));
        }
    }
    errors
}
