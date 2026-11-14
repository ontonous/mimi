//! Verifier-only consumer for the canonical MIR literal FFI profile.
//!
//! This entry takes no `CheckedProgram`, Resolved expression, or surface AST.
//! Its only proof inputs are the sealed VerifierOnly MIR contract constants
//! and the private Checker receipt that binds them to FFI declarations,
//! callsites, owners, and spans.

use crate::core::mir::reference::{
    first_ffi_literal_callsite, MirFfiLiteralContractProfileV1, MIR_FFI_LITERAL_CONTRACT_PROFILE_V1,
};
use crate::core::mir::{MirContractExpr, MirContractKind};
use crate::verifier::{ProofArtifact, TrustedSubsetDomain, VerifStatus, VerificationResult};

pub(crate) fn verify_profile(
    profile: &MirFfiLiteralContractProfileV1,
    source_hash: &str,
) -> Result<Vec<VerificationResult>, String> {
    profile
        .validate()
        .map_err(|error| format!("MIR-COVERAGE-001: {error}"))?;
    let route_receipt = profile.route_receipt();
    if route_receipt.profile != MIR_FFI_LITERAL_CONTRACT_PROFILE_V1 {
        return Err(format!(
            "MIR-COVERAGE-001: unexpected literal FFI route profile '{}'",
            route_receipt.profile
        ));
    }

    let mut results = Vec::with_capacity(profile.declarations().len());
    for declaration in profile.declarations() {
        let function = profile
            .program()
            .functions()
            .get(&declaration.mir_owner)
            .ok_or_else(|| {
                format!(
                    "MIR-COVERAGE-001: canonical MIR function for extern '{}' is missing",
                    declaration.symbol
                )
            })?;
        let mut requires = None;
        let mut ensures = None;
        for contract in &function.contracts {
            let MirContractExpr::Bool(value) = &contract.condition else {
                return Err(format!(
                    "MIR-COVERAGE-001: literal FFI contract '{}' is not a MIR boolean constant",
                    contract.id.0
                ));
            };
            let slot = match contract.kind {
                MirContractKind::Requires => &mut requires,
                MirContractKind::Ensures => &mut ensures,
                MirContractKind::Invariant => {
                    return Err(format!(
                        "MIR-COVERAGE-001: FFI declaration '{}' contains an invariant root",
                        declaration.symbol
                    ));
                }
            };
            if slot.replace(*value).is_some() {
                return Err(format!(
                    "MIR-COVERAGE-001: FFI declaration '{}' repeats a {:?} root",
                    declaration.symbol, contract.kind
                ));
            }
        }
        if requires != declaration.requires.as_ref().map(|root| root.value)
            || ensures != declaration.ensures.as_ref().map(|root| root.value)
        {
            return Err(format!(
                "MIR-COVERAGE-001: canonical MIR roots disagree with Checker declaration '{}'",
                declaration.symbol
            ));
        }

        let status = if requires == Some(false) || ensures == Some(false) {
            VerifStatus::Disproven
        } else {
            VerifStatus::Proven
        };
        let status_message = if requires == Some(false) {
            format!(
                "extern '{}' has an unsatisfiable literal requires contract",
                declaration.symbol
            )
        } else if ensures == Some(false) {
            format!(
                "extern '{}' literal declaration ensures contract is disproven",
                declaration.symbol
            )
        } else {
            format!(
                "extern '{}' literal declaration contract is proven from checker-owned roots",
                declaration.symbol
            )
        };
        let callsite_sources = declaration
            .callsites
            .iter()
            .map(|site| {
                format!(
                    "{} (owner '{}', span {})",
                    site.node.0, site.owner.0, site.span
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let message = format!("{status_message}; Checker callsites: [{callsite_sources}]");
        let (_, diagnostic_span) = first_ffi_literal_callsite(declaration).ok_or_else(|| {
            format!(
                "MIR-COVERAGE-001: FFI declaration '{}' has no diagnostic callsite",
                declaration.symbol
            )
        })?;
        let diagnostic = (status != VerifStatus::Proven)
            .then(|| crate::diagnostic::Diagnostic::error(message.clone(), diagnostic_span));

        let mut artifact = ProofArtifact::new(
            "canonical-mir-literal-boolean-v1".to_string(),
            source_hash.to_string(),
        );
        artifact.engine = ProofArtifact::ENGINE_MIR.to_string();
        artifact.proof_profile = ProofArtifact::PROFILE_MIR_FFI_EXPERIMENTAL_V1.to_string();
        artifact.obligation_scope = ProofArtifact::SCOPE_LITERAL_FFI_DECLARATION_V1.to_string();
        artifact.integer_model = "literal-boolean".to_string();
        artifact.float_model = crate::core::mir::types::MIR_FFI_VERIFIER_FLOAT_MODEL.into();
        artifact.solver_version = "none (canonical MIR boolean constants)".to_string();
        artifact.mir_hash = route_receipt.mir_digest.clone();
        artifact.mir_route_receipt = Some(route_receipt.clone());

        results.push(VerificationResult {
            func_name: format!("extern {}", declaration.symbol),
            status,
            message,
            diagnostic,
            duration_us: 0,
            constraint_count: usize::from(declaration.requires.is_some())
                + usize::from(declaration.ensures.is_some()),
            artifact: Some(artifact),
            trusted_subset_domain: Some(TrustedSubsetDomain::Contract),
        });
    }
    Ok(results)
}
