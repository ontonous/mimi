//! Identity receipts for Canonical MIR consumer routes.
//!
//! A route receipt is an audit witness, not another semantic IR.  It is
//! computed from an already validated `MirProgram` and deliberately includes
//! the TypeDesc catalog, concrete instances, transition contracts, function
//! CFG/instructions, and ownership event streams.  Consumers may report or
//! compare the receipt, but they never use it to reconstruct frontend facts.

use std::fmt::Write as _;

use crate::core::mir::reference::MirProgram;
use crate::core::mir::MirValueId;
use crate::core::NodeId;

/// Schema version for the cross-consumer route receipt.
pub const MIR_ROUTE_RECEIPT_SCHEMA: &str = "mimi-mir-route-receipt-v1";

/// Schema prefix for the semantic MIR identity digest.
pub const MIR_IDENTITY_SCHEMA: &str = "mimi-canonical-mir-identity-v1";

/// Versioned contract used by every current MIR consumer before execution or
/// backend lowering. A route receipt records this separately from the MIR
/// digest so validator evolution cannot masquerade as a program identity
/// change.
pub const MIR_ROUTE_VALIDATOR_CONTRACT_ID: &str = "mimi-mir-route-validator-v1";

/// Immutable audit witness shared by reference, bytecode, native, and
/// verifier route tests.  The digest fields are independent so a report can
/// distinguish a TypeDesc/ownership drift from a whole-program MIR drift.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalMirRouteReceipt {
    pub schema: &'static str,
    pub profile: String,
    pub mir_digest: String,
    pub type_desc_digest: String,
    pub abi_digest: String,
    /// Digest of the checker-owned FFI receipt table, independent of the
    /// enclosing function/ownership graph.  Matrix and CLI evidence can
    /// compare the FFI boundary directly without parsing the whole MIR
    /// identity text.
    pub ffi_digest: String,
    pub ownership_digest: String,
    pub flow_transition_digest: String,
    pub root_owners: Vec<NodeId>,
}

impl MirProgram {
    /// Return the deterministic semantic digest of this complete MIR graph.
    ///
    /// This is the single identity algorithm used by proof artifacts and
    /// route receipts.  It is intentionally independent of source spans,
    /// public display names, backend ABI spellings, allocator addresses, and
    /// consumer execution order.
    pub fn canonical_digest(&self) -> String {
        digest(canonical_mir_text(self))
    }

    /// Produce a route receipt after the caller has passed the relevant MIR
    /// validity and island capability gates.
    pub fn route_receipt(&self, profile: impl Into<String>) -> CanonicalMirRouteReceipt {
        let type_desc_text = self.type_catalog().canonical_text();
        let abi_text = self.type_catalog().abi_canonical_text();
        let ffi_text = canonical_ffi_text(self);
        let ownership_text = canonical_ownership_text(self);
        CanonicalMirRouteReceipt {
            schema: MIR_ROUTE_RECEIPT_SCHEMA,
            profile: profile.into(),
            mir_digest: self.canonical_digest(),
            type_desc_digest: digest(type_desc_text),
            abi_digest: digest(abi_text),
            ffi_digest: digest(ffi_text),
            ownership_digest: digest(ownership_text),
            flow_transition_digest: digest(canonical_transition_text(self)),
            root_owners: canonical_root_owners(self),
        }
    }
}

fn canonical_mir_text(program: &MirProgram) -> String {
    let mut text = String::new();
    text.push_str(MIR_IDENTITY_SCHEMA);
    text.push('\n');
    text.push_str(&program.type_catalog().canonical_text());
    for instance in program.instances().values() {
        text.push_str("mir.instance ");
        text.push_str(instance.id.as_str());
        text.push(' ');
        text.push_str(instance.template.0.as_str());
        text.push_str(" -> ");
        text.push_str(instance.function.0.as_str());
        text.push('<');
        for (index, argument) in instance.arguments.iter().enumerate() {
            if index != 0 {
                text.push(',');
            }
            text.push_str(argument.as_str());
        }
        text.push_str("> contract=");
        text.push_str(&format!("{:?}\n", instance.contract));
    }
    for transition in program.transitions().values() {
        text.push_str(&transition.canonical_text());
    }
    text.push_str(&canonical_ffi_text(program));
    for function in program.functions().values() {
        text.push_str(&function.canonical_text());
    }
    text
}

fn canonical_ffi_text(program: &MirProgram) -> String {
    let mut text = String::new();
    for contract in program.ffi_calls().values() {
        text.push_str("mir.ffi ");
        text.push_str(contract.caller.0.as_str());
        text.push(' ');
        text.push_str(contract.instruction.as_str());
        text.push(' ');
        text.push_str(contract.callee.0.as_str());
        text.push_str(" symbol=");
        text.push_str(&contract.symbol);
        text.push_str(" abi=");
        text.push_str(&contract.abi);
        text.push_str(" args=");
        for argument in &contract.arguments {
            text.push_str(argument.as_str());
            text.push(',');
        }
        text.push_str(" parameter_types=");
        for parameter_type in &contract.parameter_types {
            text.push_str(parameter_type.as_str());
            text.push(',');
        }
        text.push_str(" parameter_conversions=");
        for conversion in &contract.parameter_conversions {
            write!(
                text,
                "{}->{};",
                conversion.from.canonical_text(),
                conversion.to.canonical_text()
            )
            .expect("String write");
        }
        text.push_str(" result=");
        text.push_str(
            contract
                .result
                .as_ref()
                .map(MirValueId::as_str)
                .unwrap_or("unit"),
        );
        text.push_str(" result_type=");
        text.push_str(contract.result_type.as_str());
        text.push_str(" result_conversion=");
        if let Some(conversion) = contract.result_conversion {
            write!(
                text,
                "{}->{}",
                conversion.from.canonical_text(),
                conversion.to.canonical_text()
            )
            .expect("String write");
        } else {
            text.push_str("none");
        }
        text.push_str(" requires=");
        text.push_str(
            contract
                .requires
                .as_ref()
                .map(|condition| condition.canonical_text())
                .as_deref()
                .unwrap_or("none"),
        );
        text.push_str(" ensures=");
        text.push_str(
            contract
                .ensures
                .as_ref()
                .map(|condition| condition.canonical_text())
                .as_deref()
                .unwrap_or("none"),
        );
        text.push('\n');
    }
    text
}

fn canonical_ownership_text(program: &MirProgram) -> String {
    let mut text = String::new();
    for function in program.functions().values() {
        text.push_str(&function.owner.0);
        text.push('\n');
        text.push_str(&function.ownership.canonical_text());
    }
    text
}

fn canonical_transition_text(program: &MirProgram) -> String {
    let mut text = String::from("mimi-flow-transition-contract-v1\n");
    for transition in program.transitions().values() {
        text.push_str(&transition.canonical_text());
    }
    text
}

fn canonical_root_owners(program: &MirProgram) -> Vec<NodeId> {
    let mut owners = program
        .functions()
        .keys()
        .chain(program.transitions().keys())
        .cloned()
        .collect::<Vec<_>>();
    owners.sort();
    owners
}

fn digest(text: String) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}
