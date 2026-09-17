//! Identity receipts for Canonical MIR consumer routes.
//!
//! A route receipt is an audit witness, not another semantic IR.  It is
//! computed from an already validated `MirProgram` and deliberately includes
//! the TypeDesc catalog, concrete instances, transition contracts, function
//! CFG/instructions, and ownership event streams.  Consumers may report or
//! compare the receipt, but they never use it to reconstruct frontend facts.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::core::mir::reference::MirProgram;
use crate::core::NodeId;

/// Schema version for the cross-consumer route receipt.
pub const MIR_ROUTE_RECEIPT_SCHEMA: &str = "mimi-mir-route-receipt-v1";

/// Stable profile used by the public AST-free bytecode adapter when the
/// caller supplies only an immutable `MirProgram` and no route manifest.
/// Keeping the profile beside the receipt schema prevents the adapter and its
/// evidence consumers from drifting into different provenance labels.
pub const MIR_BYTECODE_DIRECT_ROUTE_PROFILE: &str = "bytecode-direct-v1";

/// Stable profile used by the direct native `CodeGenerator::compile_checked`
/// entry.  The native preflight and final LLVM emission must carry the same
/// immutable MIR witness instead of silently manufacturing separate route
/// identities for bytecode, verifier, and native consumers.
pub const MIR_NATIVE_DIRECT_ROUTE_PROFILE: &str = "native-direct-v1";

/// Stable diagnostic identifiers for route receipt admission failures. Keep
/// these available through the MIR namespace while the diagnostic registry
/// remains the single source of truth for CLI/LSP code descriptions.
pub use crate::diagnostic::codes::{
    MIR_FFI_DECLARATION_BOUNDARY_ERROR_CODE, MIR_FFI_ROUTE_MANIFEST_ERROR_CODE,
    MIR_FFI_ROUTE_RECEIPT_ERROR_CODE, MIR_ROUTE_COVERAGE_ERROR_CODE, MIR_ROUTE_MANIFEST_ERROR_CODE,
    MIR_ROUTE_MATERIALIZATION_ERROR_CODE, MIR_ROUTE_RECEIPT_ERROR_CODE,
};

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
    /// Compare the immutable semantic identity carried by two route receipts.
    ///
    /// The consumer profile is invocation provenance (for example, native,
    /// bytecode, or verifier) rather than program identity, so it is
    /// intentionally excluded.  All schema, digest, and root-owner fields
    /// remain part of the comparison; a receipt from another canonical MIR
    /// graph must never be accepted as an equivalent replay.
    pub fn same_semantic_identity(&self, other: &CanonicalMirRouteReceipt) -> bool {
        self.schema == other.schema
            && self.mir_digest == other.mir_digest
            && self.type_desc_digest == other.type_desc_digest
            && self.abi_digest == other.abi_digest
            && self.ffi_digest == other.ffi_digest
            && self.ownership_digest == other.ownership_digest
            && self.flow_transition_digest == other.flow_transition_digest
            && self.root_owners == other.root_owners
    }

    /// Return the deterministic digest used when a consumer needs a cache
    /// identity for this receipt.  The profile remains excluded for the same
    /// reason as [`Self::same_semantic_identity`]: it records which consumer
    /// admitted the graph, while the framed schema/digest/owner tuple is the
    /// reusable semantic witness.  Length framing keeps owner identities from
    /// creating delimiter collisions.
    pub fn semantic_identity_digest(&self) -> String {
        let mut framed = String::from("mimi-proof-mir-route-cache-v1");
        let mut frame = |value: &str| {
            use std::fmt::Write as _;
            write!(framed, "\n{}:", value.len()).expect("writing a String cannot fail");
            framed.push_str(value);
        };
        frame(self.schema);
        frame(&self.mir_digest);
        frame(&self.type_desc_digest);
        frame(&self.abi_digest);
        frame(&self.ffi_digest);
        frame(&self.ownership_digest);
        frame(&self.flow_transition_digest);
        for owner in &self.root_owners {
            frame(&owner.0);
        }
        blake3::hash(framed.as_bytes()).to_hex().to_string()
    }

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
        if !manifest_atom_is_safe(&self.profile, false) {
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
        if self
            .root_owners
            .iter()
            .any(|owner| !manifest_atom_is_safe(&owner.0, true))
        {
            return Err("route receipt root owner is empty or not manifest-safe".into());
        }
        if self
            .root_owners
            .windows(2)
            .any(|owners| matches!(owners, [left, right] if left >= right))
        {
            return Err("route receipt root owners must be strictly sorted".into());
        }
        Ok(())
    }

    /// Validate this receipt against the immutable MIR graph it claims to
    /// describe. This compares the whole identity tuple, including the FFI
    /// and ABI sub-digests, so consumers cannot accept a locally edited
    /// receipt merely because its top-level digest was left unchanged.
    pub fn validate_against_program(&self, program: &MirProgram) -> Result<(), String> {
        self.validate()?;
        let actual = program.route_receipt(self.profile.clone());
        for (field, expected, observed) in [
            (
                "mir_digest",
                self.mir_digest.as_str(),
                actual.mir_digest.as_str(),
            ),
            (
                "type_desc_digest",
                self.type_desc_digest.as_str(),
                actual.type_desc_digest.as_str(),
            ),
            (
                "abi_digest",
                self.abi_digest.as_str(),
                actual.abi_digest.as_str(),
            ),
            (
                "ffi_digest",
                self.ffi_digest.as_str(),
                actual.ffi_digest.as_str(),
            ),
            (
                "ownership_digest",
                self.ownership_digest.as_str(),
                actual.ownership_digest.as_str(),
            ),
            (
                "flow_transition_digest",
                self.flow_transition_digest.as_str(),
                actual.flow_transition_digest.as_str(),
            ),
        ] {
            if expected != observed {
                return Err(format!(
                    "route receipt {field} {expected} does not match MIR input {observed}"
                ));
            }
        }
        if self.root_owners != actual.root_owners {
            return Err(format!(
                "route receipt root_owners {:?} do not match MIR input {:?}",
                self.root_owners, actual.root_owners
            ));
        }
        Ok(())
    }

    /// Compare this evidence manifest with a checker-owned receipt.
    ///
    /// Both values are validated before comparison so a structurally malformed
    /// manifest cannot be accepted merely because it happens to differ from
    /// the expected route.  Returning the changed field names gives CLI and
    /// evidence consumers one fail-closed comparison path without requiring
    /// them to duplicate receipt identity knowledge.
    pub fn verify_against_receipt(
        &self,
        expected: &CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate()
            .map_err(|error| format!("invalid route receipt: {error}"))?;
        expected
            .validate()
            .map_err(|error| format!("invalid expected route receipt: {error}"))?;
        let mut mismatches = Vec::new();
        if self.schema != expected.schema {
            mismatches.push("schema");
        }
        if self.profile != expected.profile {
            mismatches.push("profile");
        }
        if self.mir_digest != expected.mir_digest {
            mismatches.push("mir_digest");
        }
        if self.type_desc_digest != expected.type_desc_digest {
            mismatches.push("type_desc_digest");
        }
        if self.abi_digest != expected.abi_digest {
            mismatches.push("abi_digest");
        }
        if self.ffi_digest != expected.ffi_digest {
            mismatches.push("ffi_digest");
        }
        if self.ownership_digest != expected.ownership_digest {
            mismatches.push("ownership_digest");
        }
        if self.flow_transition_digest != expected.flow_transition_digest {
            mismatches.push("flow_transition_digest");
        }
        if self.root_owners != expected.root_owners {
            mismatches.push("root_owners");
        }
        if mismatches.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "route receipt does not match expected checker receipt: {}",
                mismatches.join(", ")
            ))
        }
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
            writeln!(text, "{field}={value}")
                .map_err(|_| "failed to render MIR route receipt manifest".to_string())?;
        }
        Ok(text)
    }

    /// Render and parse this receipt through the canonical evidence-manifest
    /// boundary, returning the reconstructed value only when the public
    /// fields survive the round trip exactly.  Every backend that admits a
    /// manifest can use this helper instead of maintaining its own pair of
    /// serializer/parser calls.
    pub fn manifest_round_trip(&self) -> Result<Self, String> {
        let manifest = self
            .manifest_text()
            .map_err(|error| format!("render: {error}"))?;
        let replayed = Self::from_manifest(&manifest).map_err(|error| format!("parse: {error}"))?;
        if replayed != *self {
            return Err("route receipt changed during manifest replay".into());
        }
        Ok(replayed)
    }

    /// Parse a line-oriented manifest and replay it through the canonical
    /// serializer/parser boundary before admitting the receipt.  This is the
    /// fallible counterpart used by AST-free consumers that receive a manifest
    /// directly instead of an already typed receipt.
    pub fn from_manifest_round_trip(text: &str) -> Result<Self, String> {
        let receipt = Self::from_manifest(text)?;
        receipt.manifest_round_trip()
    }

    /// Parse the line-oriented manifest emitted by [`Self::manifest_text`].
    ///
    /// This is intentionally a strict parser for evidence consumers: the
    /// versioned header, exact field set, field order, duplicate/unknown rows,
    /// and public value semantics are all checked before a map is returned.
    /// This keeps a parser from blessing a partially trusted receipt.
    pub fn parse_manifest(text: &str) -> Result<BTreeMap<String, String>, String> {
        validate_manifest_field_schema()
            .map_err(|error| format!("invalid MIR route manifest: {error}"))?;
        let mut lines = text.lines();
        if lines.next() != Some(MIR_ROUTE_RECEIPT_MANIFEST_HEADER) {
            return Err(format!(
                "invalid MIR route manifest: expected header '{MIR_ROUTE_RECEIPT_MANIFEST_HEADER}'"
            ));
        }

        let mut entries = BTreeMap::new();
        let mut seen = std::collections::BTreeSet::new();
        for (row, expected_field) in MIR_ROUTE_RECEIPT_MANIFEST_FIELDS.iter().enumerate() {
            let line = lines.next().ok_or_else(|| {
                format!("invalid MIR route manifest: missing field '{expected_field}'")
            })?;
            let (field, value) = line
                .split_once('=')
                .ok_or_else(|| format!("invalid MIR route manifest: row {row} is missing '='"))?;
            if !MIR_ROUTE_RECEIPT_MANIFEST_FIELDS.contains(&field) {
                return Err(format!(
                    "invalid MIR route manifest: unknown field '{field}' at row {row}"
                ));
            }
            if !seen.insert(field) {
                return Err(format!(
                    "invalid MIR route manifest: duplicate field '{field}' at row {row}"
                ));
            }
            if field != *expected_field {
                return Err(format!(
                    "invalid MIR route manifest: field '{field}' at row {row}, expected '{expected_field}'"
                ));
            }
            entries.insert(field.to_owned(), value.to_owned());
        }
        if let Some(line) = lines.next() {
            let field = line.split_once('=').map_or(line, |(field, _)| field);
            if MIR_ROUTE_RECEIPT_MANIFEST_FIELDS.contains(&field) {
                return Err(format!(
                    "invalid MIR route manifest: duplicate field '{field}' at row {}",
                    MIR_ROUTE_RECEIPT_MANIFEST_FIELDS.len()
                ));
            }
            return Err(format!(
                "invalid MIR route manifest: unknown field '{field}' at row {}",
                MIR_ROUTE_RECEIPT_MANIFEST_FIELDS.len()
            ));
        }
        validate_manifest_values(&entries)?;
        Ok(entries)
    }

    /// Parse and reconstitute a validated route receipt from its manifest.
    ///
    /// The conversion is deliberately versioned and lossless for the public
    /// receipt fields. A caller that needs to compare a CLI snapshot with a
    /// checked API receipt can therefore round-trip through one canonical
    /// value validator instead of duplicating digest or owner parsing.
    pub fn from_manifest(text: &str) -> Result<Self, String> {
        let entries = Self::parse_manifest(text)?;
        let profile = entries
            .get("profile")
            .cloned()
            .ok_or_else(|| "invalid MIR route manifest: missing field 'profile'".to_string())?;
        let digest = |field: &str| {
            entries
                .get(field)
                .cloned()
                .ok_or_else(|| format!("invalid MIR route manifest: missing field '{field}'"))
        };
        let root_owners = entries
            .get("root_owners")
            .ok_or_else(|| "invalid MIR route manifest: missing field 'root_owners'".to_string())?
            .split(',')
            .filter(|owner| !owner.is_empty())
            .map(|owner| NodeId(owner.to_owned()))
            .collect();
        Ok(Self {
            schema: MIR_ROUTE_RECEIPT_SCHEMA,
            profile,
            mir_digest: digest("mir_digest")?,
            type_desc_digest: digest("type_desc_digest")?,
            abi_digest: digest("abi_digest")?,
            ffi_digest: digest("ffi_digest")?,
            ownership_digest: digest("ownership_digest")?,
            flow_transition_digest: digest("flow_transition_digest")?,
            root_owners,
        })
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

/// Validate one scalar value before it crosses the line-oriented manifest or
/// canonical receipt boundary.  Profile values are standalone fields and may
/// contain commas; root-owner values are joined with commas and therefore
/// reject both delimiters.  Whitespace is rejected for both so a value cannot
/// acquire a different tokenization in a consumer that splits canonical text.
fn manifest_atom_is_safe(value: &str, reject_comma: bool) -> bool {
    !value.trim().is_empty()
        && !value.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || character == '='
                || (reject_comma && character == ',')
        })
}

fn validate_manifest_values(entries: &BTreeMap<String, String>) -> Result<(), String> {
    let schema = entries
        .get("schema")
        .ok_or_else(|| "invalid MIR route manifest: missing field 'schema'".to_string())?;
    if schema != MIR_ROUTE_RECEIPT_SCHEMA {
        return Err(format!(
            "invalid MIR route manifest: field 'schema' has unexpected value '{schema}'"
        ));
    }
    let profile = entries
        .get("profile")
        .ok_or_else(|| "invalid MIR route manifest: missing field 'profile'".to_string())?;
    if !manifest_atom_is_safe(profile, false) {
        return Err(
            "invalid MIR route manifest: field 'profile' is empty or not manifest-safe".into(),
        );
    }
    for field in [
        "mir_digest",
        "type_desc_digest",
        "abi_digest",
        "ffi_digest",
        "ownership_digest",
        "flow_transition_digest",
    ] {
        let value = entries
            .get(field)
            .ok_or_else(|| format!("invalid MIR route manifest: missing field '{field}'"))?;
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(format!(
                "invalid MIR route manifest: field '{field}' must be a 64-character lowercase hex digest"
            ));
        }
    }
    let owners = entries
        .get("root_owners")
        .ok_or_else(|| "invalid MIR route manifest: missing field 'root_owners'".to_string())?;
    let owners = if owners.is_empty() {
        Vec::new()
    } else {
        owners
            .split(',')
            .map(|owner| {
                if !manifest_atom_is_safe(owner, true) {
                    return Err(
                        "invalid MIR route manifest: root owner is empty or not manifest-safe"
                            .to_string(),
                    );
                }
                Ok(NodeId(owner.to_owned()))
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    if owners
        .windows(2)
        .any(|window| matches!(window, [left, right] if left >= right))
    {
        return Err("invalid MIR route manifest: root owners must be strictly sorted".into());
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
        text.push_str(&crate::core::mir::canonical_instance_contract_text(
            &instance.contract,
        ));
        text.push('\n');
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
    // Receipt identity must not depend on diagnostic provenance.  Consumers
    // still use `ffi_call_entries_in_source_order` when they need source-order
    // output, but the semantic digest uses only stable call-site identities so
    // a span remap cannot turn into a MIR identity change.
    let mut entries = program.ffi_calls().iter().collect::<Vec<_>>();
    entries.sort_by(|(left_key, left), (right_key, right)| {
        left.caller
            .cmp(&right.caller)
            .then_with(|| left.instruction.cmp(&right.instruction))
            .then_with(|| left_key.cmp(right_key))
    });
    for (instruction, contract) in entries {
        text.push_str("mir.ffi ");
        // Valid canonical programs keep the table key and checker-owned
        // receipt identity equal, so this marker is absent and preserves the
        // established digest.  A test-only forged table may change just the
        // key while leaving the receipt body untouched; retain that mismatch
        // in the digest instead of silently collapsing two table identities.
        if instruction != &contract.instruction {
            text.push_str("key_mismatch=");
            text.push_str(instruction.as_str());
            text.push(' ');
        }
        text.push_str(&contract.canonical_text());
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
    fn route_receipt_comparison_reports_identity_drift() {
        let receipt = valid_receipt();
        receipt
            .verify_against_receipt(&receipt)
            .expect("identical receipts must compare successfully");

        let mut profile = receipt.clone();
        profile.profile = "other-v1".into();
        assert!(
            receipt.same_semantic_identity(&profile),
            "consumer profile is provenance and must not change semantic identity"
        );
        assert_eq!(
            receipt.semantic_identity_digest(),
            profile.semantic_identity_digest(),
            "consumer profile is provenance and must not split semantic cache identity"
        );
        let mut mir_digest = receipt.clone();
        mir_digest.mir_digest = "b".repeat(64);
        assert!(!receipt.same_semantic_identity(&mir_digest));
        assert_ne!(
            receipt.semantic_identity_digest(),
            mir_digest.semantic_identity_digest()
        );
        let mut type_desc_digest = receipt.clone();
        type_desc_digest.type_desc_digest = "b".repeat(64);
        let mut abi_digest = receipt.clone();
        abi_digest.abi_digest = "b".repeat(64);
        let mut ffi_digest = receipt.clone();
        ffi_digest.ffi_digest = "b".repeat(64);
        let mut ownership_digest = receipt.clone();
        ownership_digest.ownership_digest = "b".repeat(64);
        let mut flow_transition_digest = receipt.clone();
        flow_transition_digest.flow_transition_digest = "b".repeat(64);
        let mut root_owners = receipt.clone();
        root_owners.root_owners = vec![NodeId("function:a".into()), NodeId("function:z".into())];
        for (field, drifted) in [
            ("profile", profile),
            ("mir_digest", mir_digest),
            ("type_desc_digest", type_desc_digest),
            ("abi_digest", abi_digest),
            ("ffi_digest", ffi_digest),
            ("ownership_digest", ownership_digest),
            ("flow_transition_digest", flow_transition_digest),
            ("root_owners", root_owners),
        ] {
            let error = receipt
                .verify_against_receipt(&drifted)
                .expect_err("receipt identity drift must fail closed");
            assert_eq!(
                error,
                format!("route receipt does not match expected checker receipt: {field}")
            );
        }

        let mut invalid = receipt.clone();
        invalid.profile = "bad=profile".into();
        let error = receipt
            .verify_against_receipt(&invalid)
            .expect_err("invalid expected receipt must fail before comparison");
        assert_eq!(
            error,
            "invalid expected route receipt: route receipt profile is empty or not manifest-safe"
        );
    }

    #[test]
    fn route_receipt_comparison_reports_joint_identity_drift_in_declared_order() {
        let receipt = valid_receipt();
        let mut drifted = receipt.clone();
        drifted.profile = "other-v1".into();
        drifted.mir_digest = "b".repeat(64);
        drifted.abi_digest = "b".repeat(64);
        drifted.ffi_digest = "b".repeat(64);
        drifted.ownership_digest = "b".repeat(64);
        drifted.root_owners = vec![NodeId("function:a".into()), NodeId("function:z".into())];

        let error = receipt
            .verify_against_receipt(&drifted)
            .expect_err("joint receipt identity drift must fail closed");
        assert_eq!(
            error,
            "route receipt does not match expected checker receipt: profile, mir_digest, abi_digest, ffi_digest, ownership_digest, root_owners"
        );
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
        receipt.profile = "bad profile".into();
        let error = receipt
            .manifest_text()
            .expect_err("whitespace in profile must be rejected");
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

        let mut receipt = valid_receipt();
        receipt.root_owners[0] = NodeId("function:bad owner".into());
        let error = receipt
            .manifest_text()
            .expect_err("whitespace in root owner must be rejected");
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

    #[test]
    fn route_receipt_manifest_parser_rejects_unknown_and_duplicate_rows() {
        let receipt = valid_receipt();
        let manifest = receipt.manifest_text().expect("valid receipt manifest");
        let mut rows = manifest.lines().map(str::to_owned).collect::<Vec<_>>();
        rows.insert(1, "future_field=unexpected".into());
        let unknown = CanonicalMirRouteReceipt::parse_manifest(&rows.join("\n"))
            .expect_err("unknown manifest field must fail closed");
        assert_eq!(
            unknown,
            "invalid MIR route manifest: unknown field 'future_field' at row 0"
        );

        let mut rows = manifest.lines().map(str::to_owned).collect::<Vec<_>>();
        rows.insert(1, rows[1].clone());
        let duplicate = CanonicalMirRouteReceipt::parse_manifest(&rows.join("\n"))
            .expect_err("duplicate manifest field must fail closed");
        assert_eq!(
            duplicate,
            "invalid MIR route manifest: duplicate field 'schema' at row 1"
        );
    }

    #[test]
    fn route_receipt_manifest_parser_rejects_reordered_rows_before_value_comparison() {
        let receipt = valid_receipt();
        let manifest = receipt.manifest_text().expect("valid receipt manifest");
        let mut rows = manifest.lines().map(str::to_owned).collect::<Vec<_>>();
        rows.swap(3, 4);
        let error = CanonicalMirRouteReceipt::parse_manifest(&rows.join("\n"))
            .expect_err("reordered manifest rows must fail closed");
        assert_eq!(
            error,
            "invalid MIR route manifest: field 'type_desc_digest' at row 2, expected 'mir_digest'"
        );
    }

    #[test]
    fn route_receipt_manifest_parser_rejects_value_semantic_drift() {
        let receipt = valid_receipt();
        let manifest = receipt.manifest_text().expect("valid receipt manifest");
        for (field, value, expected) in [
            (
                "schema",
                "mimi-mir-route-receipt-v2",
                "invalid MIR route manifest: field 'schema' has unexpected value 'mimi-mir-route-receipt-v2'",
            ),
            (
                "profile",
                "bad=profile",
                "invalid MIR route manifest: field 'profile' is empty or not manifest-safe",
            ),
            (
                "profile",
                "bad profile",
                "invalid MIR route manifest: field 'profile' is empty or not manifest-safe",
            ),
            (
                "profile",
                "bad\u{00a0}profile",
                "invalid MIR route manifest: field 'profile' is empty or not manifest-safe",
            ),
            (
                "mir_digest",
                "not-a-digest",
                "invalid MIR route manifest: field 'mir_digest' must be a 64-character lowercase hex digest",
            ),
            (
                "root_owners",
                "z-owner,a-owner",
                "invalid MIR route manifest: root owners must be strictly sorted",
            ),
            (
                "root_owners",
                "bad owner,function:z",
                "invalid MIR route manifest: root owner is empty or not manifest-safe",
            ),
            (
                "root_owners",
                "bad\u{2003}owner,function:z",
                "invalid MIR route manifest: root owner is empty or not manifest-safe",
            ),
        ] {
            let mutated = manifest
                .lines()
                .map(|line| {
                    line.strip_prefix(&format!("{field}="))
                        .map_or_else(|| line.to_owned(), |_| format!("{field}={value}"))
                })
                .collect::<Vec<_>>()
                .join("\n");
            let error = CanonicalMirRouteReceipt::parse_manifest(&mutated)
                .expect_err("manifest value drift must fail closed");
            assert_eq!(error, expected, "unexpected diagnostic for {field}");
        }
    }

    #[test]
    fn route_receipt_manifest_parser_normalizes_crlf_to_canonical_lf() {
        let receipt = valid_receipt();
        let manifest = receipt.manifest_text().expect("valid receipt manifest");
        let crlf = manifest.replace('\n', "\r\n");
        let parsed = CanonicalMirRouteReceipt::from_manifest(&crlf)
            .expect("CRLF manifest must preserve receipt semantics");
        assert_eq!(parsed, receipt);
        assert_eq!(
            parsed.manifest_text().expect("render parsed receipt"),
            manifest,
            "manifest rendering must use the canonical LF byte form"
        );
    }

    #[test]
    fn route_receipt_manifest_round_trips_to_the_same_receipt() {
        let receipt = valid_receipt();
        let manifest = receipt.manifest_text().expect("valid receipt manifest");
        assert_eq!(
            CanonicalMirRouteReceipt::from_manifest(&manifest),
            Ok(receipt.clone())
        );
        assert_eq!(
            CanonicalMirRouteReceipt::from_manifest_round_trip(&manifest),
            Ok(receipt.clone())
        );
        assert_eq!(receipt.manifest_round_trip(), Ok(receipt));
    }

    #[test]
    fn route_receipt_manifest_rejects_an_old_header_with_a_stable_hint() {
        let receipt = valid_receipt();
        let manifest = receipt
            .manifest_text()
            .expect("valid receipt manifest")
            .replacen(
                MIR_ROUTE_RECEIPT_MANIFEST_HEADER,
                "mimi-mir-route-manifest-v0",
                1,
            );
        assert_eq!(
            CanonicalMirRouteReceipt::from_manifest(&manifest),
            Err("invalid MIR route manifest: expected header 'mimi-mir-route-manifest-v1'".into())
        );
    }

    #[test]
    fn route_receipt_manifest_rejects_future_version_and_field_extensions() {
        let receipt = valid_receipt();
        let manifest = receipt.manifest_text().expect("valid receipt manifest");

        let future_header = manifest.replacen(
            MIR_ROUTE_RECEIPT_MANIFEST_HEADER,
            "mimi-mir-route-manifest-v2",
            1,
        );
        assert_eq!(
            CanonicalMirRouteReceipt::from_manifest(&future_header),
            Err("invalid MIR route manifest: expected header 'mimi-mir-route-manifest-v1'".into())
        );

        for insertion in [1, 5, 10] {
            let mut rows = manifest.lines().map(str::to_owned).collect::<Vec<_>>();
            rows.insert(insertion, "future_field=reserved".into());
            let error = CanonicalMirRouteReceipt::parse_manifest(&rows.join("\n"))
                .expect_err("future fields require an explicit schema version");
            let row = insertion.saturating_sub(1);
            assert_eq!(
                error,
                format!("invalid MIR route manifest: unknown field 'future_field' at row {row}")
            );
        }
    }

    #[test]
    fn route_receipt_manifest_enforces_field_completeness_and_empty_owner_round_trip() {
        let receipt = valid_receipt();
        let manifest = receipt.manifest_text().expect("valid receipt manifest");

        let missing_owner = manifest
            .lines()
            .filter(|line| !line.starts_with("root_owners="))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            CanonicalMirRouteReceipt::parse_manifest(&missing_owner),
            Err("invalid MIR route manifest: missing field 'root_owners'".into())
        );

        let trailing_future = format!("{manifest}future_field=reserved\n");
        assert_eq!(
            CanonicalMirRouteReceipt::parse_manifest(&trailing_future),
            Err("invalid MIR route manifest: unknown field 'future_field' at row 9".into())
        );

        let empty_owner_manifest =
            manifest.replace("root_owners=function:main,function:z", "root_owners=");
        let empty_owner_receipt = CanonicalMirRouteReceipt::from_manifest(&empty_owner_manifest)
            .expect("empty owner set is a valid manifest value");
        assert!(empty_owner_receipt.root_owners.is_empty());
        assert_eq!(
            empty_owner_receipt
                .manifest_text()
                .expect("render empty owner set"),
            empty_owner_manifest
        );
    }

    #[test]
    fn canonical_mir_instance_contract_uses_explicit_stable_spelling() {
        let source = include_str!("../../../tests/fixtures/mir_native_generic_list_len.mimi");
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex generic instance fixture");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse generic instance fixture");
        let checked = crate::core::check_program(&file).expect("check generic instance fixture");
        let program = MirProgram::from_checked_program(&checked)
            .expect("materialize generic instance fixture");
        let text = canonical_mir_text(&program);

        assert!(
            text.contains("contract=scalar_list_facade operation=len\n"),
            "canonical MIR must use the explicit instance contract spelling"
        );
        assert!(
            !text.contains("ScalarListFacade"),
            "canonical MIR must not inherit Rust Debug enum names"
        );
    }
}
