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

/// Stable header for the line-oriented CLI evidence manifest.
pub const MIR_ROUTE_RECEIPT_MANIFEST_HEADER: &str = "mimi-mir-route-manifest-v1";

/// Ordered field names emitted by the CLI evidence manifest. Keeping the
/// field set beside the receipt lets consumers detect schema drift without
/// reverse-engineering the renderer.
pub const MIR_ROUTE_RECEIPT_MANIFEST_FIELDS: [&str; 9] = [
    "schema",
    "profile",
    "mir_digest",
    "type_desc_digest",
    "abi_digest",
    "ffi_digest",
    "ownership_digest",
    "flow_transition_digest",
    "root_owners",
];

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

impl CanonicalMirRouteReceipt {
    /// Validate the invariants required before a receipt is rendered as an
    /// evidence manifest. This remains a pure receipt check: it does not
    /// inspect source AST, infer types, or consult a backend.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != MIR_ROUTE_RECEIPT_SCHEMA {
            return Err(format!(
                "unexpected route receipt schema '{}', expected '{}'",
                self.schema, MIR_ROUTE_RECEIPT_SCHEMA
            ));
        }
        if self.profile.trim().is_empty()
            || self
                .profile
                .chars()
                .any(|character| character.is_control() || character == '=')
        {
            return Err("route receipt profile is empty or not manifest-safe".into());
        }
        for (name, value) in [
            ("mir_digest", self.mir_digest.as_str()),
            ("type_desc_digest", self.type_desc_digest.as_str()),
            ("abi_digest", self.abi_digest.as_str()),
            ("ffi_digest", self.ffi_digest.as_str()),
            ("ownership_digest", self.ownership_digest.as_str()),
            (
                "flow_transition_digest",
                self.flow_transition_digest.as_str(),
            ),
        ] {
            if value.len() != 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(format!(
                    "route receipt {name} must be a 64-character lowercase hex digest"
                ));
            }
        }
        if self.root_owners.iter().any(|owner| {
            owner.0.is_empty()
                || owner
                    .0
                    .chars()
                    .any(|character| character.is_control() || matches!(character, '=' | ','))
        }) {
            return Err("route receipt root owner is empty or not manifest-safe".into());
        }
        if self
            .root_owners
            .windows(2)
            .any(|owners| owners[0] >= owners[1])
        {
            return Err("route receipt root owners must be strictly sorted".into());
        }
        Ok(())
    }

    /// Render the validated receipt as the stable, line-oriented manifest
    /// shared by the CLI and evidence consumers.  Keeping field lookup beside
    /// the public field-order constant prevents a frontend or CLI edit from
    /// silently reordering or omitting a receipt value.
    pub fn manifest_text(&self) -> Result<String, String> {
        validate_manifest_field_schema()
            .map_err(|error| format!("invalid MIR route receipt: {error}"))?;
        self.validate()
            .map_err(|error| format!("invalid MIR route receipt: {error}"))?;
        let mut text = String::from(MIR_ROUTE_RECEIPT_MANIFEST_HEADER);
        text.push('\n');
        for field in MIR_ROUTE_RECEIPT_MANIFEST_FIELDS {
            let value = self.manifest_value(field).ok_or_else(|| {
                format!("invalid MIR route receipt: unknown manifest field '{field}'")
            })?;
            writeln!(text, "{field}={value}").expect("String write");
        }
        Ok(text)
    }

    fn manifest_value(&self, field: &str) -> Option<String> {
        Some(match field {
            "schema" => self.schema.to_owned(),
            "profile" => self.profile.clone(),
            "mir_digest" => self.mir_digest.clone(),
            "type_desc_digest" => self.type_desc_digest.clone(),
            "abi_digest" => self.abi_digest.clone(),
            "ffi_digest" => self.ffi_digest.clone(),
            "ownership_digest" => self.ownership_digest.clone(),
            "flow_transition_digest" => self.flow_transition_digest.clone(),
            "root_owners" => self
                .root_owners
                .iter()
                .map(|owner| owner.0.as_str())
                .collect::<Vec<_>>()
                .join(","),
            _ => return None,
        })
    }
}

fn validate_manifest_field_schema() -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for field in MIR_ROUTE_RECEIPT_MANIFEST_FIELDS {
        if field.trim().is_empty()
            || field
                .chars()
                .any(|character| character.is_control() || character == '=')
        {
            return Err(format!(
                "manifest field '{field}' is empty or not manifest-safe"
            ));
        }
        if !seen.insert(field) {
            return Err(format!("manifest field '{field}' is duplicated"));
        }
    }
    Ok(())
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
    // Flow transition bodies are executable MIR functions and are also
    // represented by transition contracts.  The receipt exposes root owners
    // as a set, so the shared identity must not repeat that owner merely
    // because it has two canonical tables.
    owners.dedup();
    owners
}

fn digest(text: String) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_receipt() -> CanonicalMirRouteReceipt {
        let digest = "a".repeat(64);
        CanonicalMirRouteReceipt {
            schema: MIR_ROUTE_RECEIPT_SCHEMA,
            profile: "test-v1".into(),
            mir_digest: digest.clone(),
            type_desc_digest: digest.clone(),
            abi_digest: digest.clone(),
            ffi_digest: digest.clone(),
            ownership_digest: digest.clone(),
            flow_transition_digest: digest,
            root_owners: vec![NodeId("function:main".into()), NodeId("function:z".into())],
        }
    }

    #[test]
    fn route_receipt_validation_accepts_canonical_shape() {
        assert!(valid_receipt().validate().is_ok());
        assert_eq!(MIR_ROUTE_RECEIPT_MANIFEST_FIELDS.len(), 9);
    }

    #[test]
    fn route_receipt_validation_rejects_schema_digest_profile_and_owner_drift() {
        let mut receipt = valid_receipt();
        receipt.schema = "future-schema";
        assert!(receipt.validate().is_err());

        let mut receipt = valid_receipt();
        receipt.mir_digest = "A".repeat(64);
        assert!(receipt.validate().is_err());

        let mut receipt = valid_receipt();
        receipt.profile = "bad=profile".into();
        assert!(receipt.validate().is_err());

        let mut receipt = valid_receipt();
        receipt.root_owners.reverse();
        assert!(receipt.validate().is_err());

        let mut receipt = valid_receipt();
        receipt.root_owners[0] = NodeId("function:owner=drift".into());
        assert!(receipt.validate().is_err());

        let mut receipt = valid_receipt();
        receipt.root_owners[0] = NodeId("function:owner,drift".into());
        assert!(receipt.validate().is_err());

        let mut receipt = valid_receipt();
        receipt.root_owners[1] = receipt.root_owners[0].clone();
        assert!(receipt.validate().is_err());
    }

    #[test]
    fn route_receipt_manifest_uses_declared_field_order_and_values() {
        let receipt = valid_receipt();
        let text = receipt.manifest_text().expect("valid receipt manifest");
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(
            lines.first().copied(),
            Some(MIR_ROUTE_RECEIPT_MANIFEST_HEADER)
        );
        let fields = lines[1..]
            .iter()
            .map(|line| line.split_once('=').expect("manifest key/value").0)
            .collect::<Vec<_>>();
        assert_eq!(fields, MIR_ROUTE_RECEIPT_MANIFEST_FIELDS.to_vec());
        assert_eq!(lines[1], "schema=mimi-mir-route-receipt-v1");
        assert_eq!(lines[2], "profile=test-v1");
        for line in &lines[3..9] {
            let (_, value) = line.split_once('=').expect("digest key/value");
            assert_eq!(value.len(), 64);
            assert!(value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
        }
        assert_eq!(lines[9], "root_owners=function:main,function:z");
    }

    #[test]
    fn route_receipt_manifest_rejects_unsafe_profile_and_owner_without_partial_output() {
        let mut receipt = valid_receipt();
        receipt.profile = "bad=profile".into();
        let error = receipt
            .manifest_text()
            .expect_err("unsafe profile rejection");
        assert_eq!(
            error,
            "invalid MIR route receipt: route receipt profile is empty or not manifest-safe"
        );

        let mut receipt = valid_receipt();
        receipt.root_owners[0] = NodeId("function:bad,owner".into());
        let error = receipt.manifest_text().expect_err("unsafe owner rejection");
        assert_eq!(
            error,
            "invalid MIR route receipt: route receipt root owner is empty or not manifest-safe"
        );
    }

    #[test]
    fn route_receipt_manifest_schema_is_unique_safe_and_unknown_lookup_is_fail_closed() {
        assert!(validate_manifest_field_schema().is_ok());
        let receipt = valid_receipt();
        assert_eq!(receipt.manifest_value("future_field"), None);
    }
}
