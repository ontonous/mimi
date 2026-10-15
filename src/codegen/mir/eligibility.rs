//! Native MIR admission and symbol eligibility.
//! This module owns only pre-emission admission plumbing; shape rules live in
//! the validator and TypeDesc ABI modules.

use super::*;
use crate::core::{ResolvedBlock, ResolvedExpr, ResolvedExprKind, ResolvedStmtKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeMirError {
    pub(super) subject: String,
    pub(super) message: String,
    pub(super) span: Span,
    /// Optional cross-layer diagnostic code.  Most native shape failures are
    /// ordinary backend diagnostics; route receipt failures additionally
    /// expose their stable code structurally instead of hiding it in text.
    pub(super) code: Option<&'static str>,
}

impl NativeMirError {
    pub(super) fn new(subject: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            message: message.into(),
            span: Span::UNKNOWN,
            code: None,
        }
    }

    pub(super) fn with_span(mut self, span: Span) -> Self {
        self.span = span;
        self
    }

    pub(super) fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }

    pub(super) fn diagnostic(self) -> Diagnostic {
        let message = format!(
            "canonical MIR native backend rejected {}: {}",
            self.subject, self.message
        );
        match self.code {
            Some(code) => Diagnostic::error_code(code, message, self.span).with_origin(
                crate::diagnostic::DiagnosticOrigin::runtime_system("mir.route"),
            ),
            None => Diagnostic::error(message, self.span),
        }
    }
}

/// Validate a Canonical MIR program against the native shape contract without
/// creating LLVM declarations.  The CLI uses this as part of the atomic
/// default-route capability gate so run/build/verify make the same route
/// decision before any production backend starts.
pub fn validate_mir_native(program: &MirProgram) -> Result<(), Vec<Diagnostic>> {
    if let Err(messages) = program.validate_ffi_source_census() {
        return Err(messages
            .into_iter()
            .map(|message| NativeMirError::new("FFI source census", message).diagnostic())
            .collect());
    }
    if let Err(messages) = validate_scalar_export_receipts(program) {
        return Err(messages
            .into_iter()
            .map(|message| NativeMirError::new("scalar export receipt", message).diagnostic())
            .collect());
    }
    NativeMirValidator::new(program)
        .validate()
        .map_err(|errors| errors.into_iter().map(NativeMirError::diagnostic).collect())
}

fn validate_scalar_export_receipts(program: &MirProgram) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    for (owner, declaration) in program.export_declarations() {
        let Some(function) = program.functions().get(owner) else {
            errors.push(format!("export owner '{}' is absent from MIR", owner.0));
            continue;
        };
        if declaration.owner != *owner
            || declaration.abi != "C"
            || declaration.public_symbol.trim().is_empty()
            || declaration.body_symbol != format!("$mimi.export.body.{}", declaration.public_symbol)
        {
            errors.push(format!(
                "export '{}' has an invalid symbol/ABI receipt",
                owner.0
            ));
        }
        if declaration.parameters.len() != function.parameters.len() {
            errors.push(format!(
                "export '{}' parameter receipt arity differs from MIR",
                owner.0
            ));
            continue;
        }
        for ((ty, kind), parameter) in declaration.parameters.iter().zip(&function.parameters) {
            let Some(value) = function.values.get(parameter) else {
                errors.push(format!("export '{}' parameter value is absent", owner.0));
                continue;
            };
            if ty != &value.ty || !export_kind_matches(program, ty, *kind, false) {
                errors.push(format!(
                    "export '{}' parameter ABI receipt is invalid",
                    owner.0
                ));
            }
        }
        if !export_kind_matches(program, &declaration.result.0, declaration.result.1, true)
            || declaration.result.0 != function.result
        {
            errors.push(format!(
                "export '{}' result ABI receipt is invalid",
                owner.0
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn export_kind_matches(
    program: &MirProgram,
    ty: &crate::core::ResolvedTypeId,
    kind: crate::core::ir::NativeExportScalarKind,
    result: bool,
) -> bool {
    let Some(descriptor) = program.type_catalog().get(ty) else {
        return false;
    };
    if kind.is_unit() {
        return result && descriptor.is_canonical_ffi_unit();
    }
    descriptor.is_canonical_ffi_scalar()
        && matches!(
            (kind, &descriptor.kind),
            (
                crate::core::ir::NativeExportScalarKind::I32,
                MirTypeKind::Primitive(crate::core::PrimitiveType::I32)
            ) | (
                crate::core::ir::NativeExportScalarKind::I64,
                MirTypeKind::Primitive(crate::core::PrimitiveType::I64)
            ) | (
                crate::core::ir::NativeExportScalarKind::F32,
                MirTypeKind::Primitive(crate::core::PrimitiveType::F32)
            ) | (
                crate::core::ir::NativeExportScalarKind::F64,
                MirTypeKind::Primitive(crate::core::PrimitiveType::F64)
            ) | (
                crate::core::ir::NativeExportScalarKind::Bool,
                MirTypeKind::Primitive(crate::core::PrimitiveType::Bool)
            )
        )
}

/// Compile a validated scalar/flat-aggregate MIR program directly to LLVM.
///
/// This is an explicit migration entry point. It is not used by the default
/// `build` path until the wider MIR shape and differential gates are closed.
impl<'ctx> CodeGenerator<'ctx> {
    pub fn compile_mir_native(&mut self, program: &MirProgram) -> Result<(), Vec<Diagnostic>> {
        self.compile_mir_native_inner(program, true)
    }

    /// Diagnostic component entry for L1/L2 tests. It still validates the
    /// complete MIR and runs the native backend, but deliberately does not
    /// claim that an owner-v2 candidate has crossed production readiness.
    #[cfg(test)]
    pub(crate) fn compile_mir_native_for_component(
        &mut self,
        program: &MirProgram,
    ) -> Result<(), Vec<Diagnostic>> {
        self.compile_mir_native_inner(program, false)
    }

    fn compile_mir_native_inner(
        &mut self,
        program: &MirProgram,
        enforce_production_readiness: bool,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        if enforce_production_readiness
            && !crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY
            && program.functions().values().any(|function| {
                function
                    .ownership
                    .has_checker_ordinary_map_owned_any_v2_actions()
            })
        {
            return Err(vec![NativeMirError::new(
                "mir-program",
                "ordinary Map/Any owner-v2 native emission is diagnostic-only while V2 production readiness is disabled",
            )
            .with_code(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
            .diagnostic()]);
        }
        let mir_digest = program.canonical_digest();
        if let Some(compiled_digest) = self.mir_native_compiled_digest.as_deref() {
            if compiled_digest == mir_digest {
                return Ok(());
            }
            return Err(vec![NativeMirError::new(
                "mir-program",
                "native generator already contains a different canonical MIR program",
            )
            .diagnostic()]);
        }
        validate_mir_native(program)?;

        NativeMirEmitter::new(self, program)
            .compile()
            .map_err(|error| vec![error.diagnostic()])?;
        self.mir_native_compiled_digest = Some(mir_digest);
        Ok(())
    }

    /// Compile canonical MIR after checking the route receipt supplied by the
    /// caller. This keeps native emission on the exact immutable graph whose
    /// receipt admitted the route, matching the bytecode consumer boundary.
    pub fn compile_mir_native_with_route_receipt(
        &mut self,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        if let Err(message) = receipt.validate_against_program(program) {
            return Err(vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt rejected: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]);
        }
        receipt.manifest_round_trip().map_err(|message| {
            vec![NativeMirError::new(
                "mir-program",
                format!(
                    "{}: canonical route receipt manifest replay failed: {message}",
                    crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                ),
            )
            .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
            .diagnostic()]
        })?;
        // Preserve the primary single-program diagnostic when a caller hands
        // this generator a receipt for a different MIR graph.  The digest is
        // the graph identity; the route anchor is checked only after this
        // mismatch has been ruled out, so diagnostics cannot depend on which
        // provenance field happened to be tampered with first.
        if self
            .mir_native_compiled_digest
            .as_deref()
            .is_some_and(|compiled_digest| compiled_digest != program.canonical_digest())
        {
            return self.compile_mir_native(program);
        }
        if let Some(bound) = self.mir_native_route_receipt.as_ref() {
            if !bound.same_semantic_identity(receipt) {
                return Err(vec![NativeMirError::new(
                    "mir-program",
                    format!(
                        "{}: canonical route receipt disagrees with native program anchor",
                        crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
                    ),
                )
                .with_code(crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE)
                .diagnostic()]);
            }
        }
        self.compile_mir_native(program)?;
        if self.mir_native_route_receipt.is_none() {
            self.mir_native_route_receipt = Some(receipt.clone());
        }
        Ok(())
    }

    /// Crate-owned CLI route that couples shared-object preflight to the
    /// CheckedProgram used to materialize the MIR graph. Raw MIR entry points
    /// intentionally cannot authorize host-visible shared-object emission.
    pub fn compile_checked_mir_native_with_route_receipt(
        &mut self,
        checked: &crate::core::CheckedProgram,
        program: &MirProgram,
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), Vec<Diagnostic>> {
        self.shared_dynamic_any_preflight_passed = false;
        checked.validate_backend(crate::core::BackendProfile::Native)?;
        if let Err(original_error) = self.preflight_native_dynamic_any_unpack(checked) {
            // Only this CheckedProgram+canonical route entry can authorize
            // the checked owner receipt. Legacy/resolved emitters keep E0722.
            if receipt.validate_against_program(program).is_err()
                || checked_managed_any_unpack_coverage(checked, program).is_err()
            {
                return Err(original_error);
            }
        }
        let result = self.compile_mir_native_with_route_receipt(program, receipt);
        if result.is_ok() && self.shared {
            self.shared_dynamic_any_preflight_passed = true;
        }
        result
    }

    /// Compile canonical MIR after parsing and checking a CLI route manifest.
    /// Keep this manifest boundary beside the typed receipt entry point so
    /// native emission cannot accept an unvalidated field extension.
    pub fn compile_mir_native_with_route_manifest(
        &mut self,
        program: &MirProgram,
        manifest: &str,
    ) -> Result<(), Vec<Diagnostic>> {
        let receipt =
            crate::core::mir::CanonicalMirRouteReceipt::from_manifest_round_trip(manifest)
                .map_err(|message| {
                    vec![NativeMirError::new(
                        "mir-program",
                        format!(
                            "{}: canonical route manifest rejected: {message}",
                            crate::core::mir::MIR_ROUTE_MANIFEST_ERROR_CODE
                        ),
                    )
                    .with_code(crate::core::mir::MIR_ROUTE_MANIFEST_ERROR_CODE)
                    .diagnostic()]
                })?;
        self.compile_mir_native_with_route_receipt(program, &receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::NativeMirError;
    use crate::core::mir::MIR_ROUTE_MANIFEST_ERROR_CODE;

    #[test]
    fn route_errors_keep_structured_code_when_rendered() {
        let diagnostic = NativeMirError::new("mir-program", "future field")
            .with_code(MIR_ROUTE_MANIFEST_ERROR_CODE)
            .diagnostic();

        assert_eq!(
            diagnostic.code.as_deref(),
            Some(MIR_ROUTE_MANIFEST_ERROR_CODE)
        );
        assert_eq!(
            diagnostic.to_string(),
            format!(
                "[{}] canonical MIR native backend rejected mir-program: future field",
                MIR_ROUTE_MANIFEST_ERROR_CODE
            )
        );
        let origin = diagnostic.origin.expect("route error has provenance");
        assert_eq!(
            origin.kind,
            crate::diagnostic::DiagnosticOriginKind::RuntimeSystem
        );
        assert_eq!(origin.rule.as_deref(), Some("mir.route"));
    }
}

pub(super) fn instruction_kind(
    instruction: &crate::core::mir::MirInstruction,
) -> &MirInstructionKind {
    &instruction.kind
}

pub(super) fn mir_symbol(owner: &crate::core::NodeId) -> Result<String, String> {
    let Some(symbol) = owner.0.strip_prefix("function:") else {
        let transition = owner
            .0
            .strip_prefix("transition:")
            .ok_or_else(|| "callable identity is not a function or transition owner".to_string())?;
        if transition.trim().is_empty() {
            return Err("transition symbol must not be empty".into());
        }
        return Ok(format!(
            "__mimi_transition_{}",
            native_symbol_fragment(transition)
        ));
    };
    if symbol.trim().is_empty() {
        return Err("only simple function symbols are in the native MIR slice".into());
    }
    if symbol.starts_with("mimi_") {
        return Err("function symbol collides with reserved runtime namespace".into());
    }
    if symbol.contains("::") {
        // Protocol method owners are checker-canonical identities such as
        // `Read:for:Counter::read:<hash>`.  They are not surface names and
        // therefore need a deterministic LLVM-safe spelling, shared by the
        // declaration and call maps.  Keep ordinary function symbols
        // unchanged for compatibility with existing native callers.
        Ok(format!("__mimi_method_{}", native_symbol_fragment(symbol)))
    } else {
        Ok(symbol.to_owned())
    }
}

pub(super) fn native_symbol_fragment(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

// The existing resolved/legacy E0722 gate remains unchanged. This scanner is
// local to the canonical entry point and checks all conversion positions in
// the CheckedProgram, including dormant bodies emitted by shared libraries.
#[derive(Default)]
struct CheckedManagedAnyUnpackScan {
    conversions: Vec<(crate::core::NodeId, crate::core::CheckedConversion)>,
}
impl CheckedManagedAnyUnpackScan {
    fn conversion(
        &mut self,
        conversion: &crate::core::CheckedConversion,
        origin: &crate::core::NodeId,
    ) {
        if conversion.kind == crate::core::CheckedConversionKind::DynamicAnyUnpack {
            self.conversions.push((origin.clone(), conversion.clone()));
        }
    }
    fn block(&mut self, block: &ResolvedBlock) {
        for statement in &block.statements {
            match &statement.kind {
                ResolvedStmtKind::Bind { initializer, .. } => {
                    if let Some(initializer) = initializer {
                        self.expression(initializer);
                    }
                }
                ResolvedStmtKind::Assign {
                    value, conversion, ..
                } => {
                    self.conversion(conversion, &statement.node_id);
                    self.expression(value);
                }
                ResolvedStmtKind::Return { value, conversion } => {
                    if let Some(value) = value {
                        self.expression(value);
                    }
                    if let Some(conversion) = conversion {
                        self.conversion(conversion, &statement.node_id);
                    }
                }
                ResolvedStmtKind::Break(value) => {
                    if let Some(value) = value {
                        self.expression(value);
                    }
                }
                ResolvedStmtKind::Expr(expression) => self.expression(expression),
                ResolvedStmtKind::While { condition, body } => {
                    self.expression(condition);
                    self.block(body);
                }
                ResolvedStmtKind::WhileLet {
                    initializer, body, ..
                }
                | ResolvedStmtKind::For {
                    iterable: initializer,
                    body,
                    ..
                } => {
                    self.expression(initializer);
                    self.block(body);
                }
                ResolvedStmtKind::IfLet {
                    initializer,
                    then_block,
                    else_block,
                    ..
                } => {
                    self.expression(initializer);
                    self.block(then_block);
                    if let Some(else_block) = else_block {
                        self.block(else_block);
                    }
                }
                ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                    self.block(body)
                }
                ResolvedStmtKind::Contract { condition, .. } => self.expression(condition),
                ResolvedStmtKind::Math(expressions) => {
                    for expression in expressions {
                        self.expression(expression);
                    }
                }
                ResolvedStmtKind::Pinned { value, body, .. } => {
                    self.expression(value);
                    self.block(body);
                }
                ResolvedStmtKind::Continue
                | ResolvedStmtKind::Drop(_)
                | ResolvedStmtKind::NestedCallable(_) => {}
            }
        }
        if let Some(result) = &block.result {
            self.expression(result);
        }
    }

    fn expression(&mut self, expression: &ResolvedExpr) {
        match &expression.kind {
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let crate::core::ir::ResolvedFStringPart::Interpolation(value) = part {
                        self.expression(value);
                    }
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                self.expression(value);
                if let crate::core::ir::ResolvedValueProjection::Index(index) = projection {
                    self.expression(index);
                }
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                self.expression(left);
                self.expression(right);
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::Try { value: operand, .. }
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand) => self.expression(operand),
            ResolvedExprKind::Cast { value, conversion } => {
                self.conversion(conversion, &expression.node_id);
                self.expression(value);
            }
            ResolvedExprKind::Call(call) => {
                for argument in &call.arguments {
                    self.conversion(&argument.conversion, &argument.value.node_id);
                    self.expression(&argument.value);
                }
            }
            ResolvedExprKind::Tuple(elements)
            | ResolvedExprKind::List(elements)
            | ResolvedExprKind::Set(elements) => {
                for element in elements {
                    self.expression(element);
                }
            }
            ResolvedExprKind::Map(entries) => {
                for (key, value) in entries {
                    self.expression(key);
                    self.expression(value);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                self.expression(value);
                self.expression(iterable);
                if let Some(guard) = guard {
                    self.expression(guard);
                }
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => self.expression(receiver),
            ResolvedExprKind::Record { fields, rest, .. } => {
                for field in fields {
                    self.conversion(&field.conversion, &field.value.node_id);
                    self.expression(&field.value);
                }
                if let Some(rest) = rest {
                    self.expression(rest);
                }
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => self.block(block),
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.expression(condition);
                self.block(then_block);
                self.block(else_block);
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.expression(scrutinee);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.expression(guard);
                    }
                    self.expression(&arm.body);
                }
            }
            ResolvedExprKind::Range { start, end } => {
                self.expression(start);
                self.expression(end);
            }
            ResolvedExprKind::Slice { target, start, end } => {
                self.expression(target);
                if let Some(start) = start {
                    self.expression(start);
                }
                if let Some(end) = end {
                    self.expression(end);
                }
            }
            ResolvedExprKind::Lambda(lambda) => self.block(&lambda.body),
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Load(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => {}
            ResolvedExprKind::Callable(_) => {}
        }
    }
}

pub(crate) fn checked_managed_any_unpack_coverage(
    checked: &crate::core::CheckedProgram,
    program: &MirProgram,
) -> Result<(), String> {
    let catalog = program.type_catalog();
    let mut expected = BTreeMap::new();
    for (owner, callable) in checked.callables() {
        if checked
            .functions()
            .get(owner)
            .is_some_and(|f| f.is_comptime)
        {
            continue;
        }
        let mut scan = CheckedManagedAnyUnpackScan::default();
        for expression in callable.body.place_inputs.values() {
            scan.expression(expression);
        }
        for expression in callable.body.default_values.values() {
            scan.expression(expression);
        }
        scan.block(&callable.body.root);
        for (origin, conversion) in scan.conversions {
            if !matches!(checked.resolved_types().get(&conversion.from),Some(crate::core::ResolvedType::DynamicAny { capability }) if capability == "type.dynamic_value")
            {
                return Err(
                    "checked Any conversion source has no exact dynamic-value type capability"
                        .into(),
                );
            }
            let operation = match checked.resolved_types().get(&conversion.to) {
                Some(crate::core::ResolvedType::Primitive(crate::core::PrimitiveType::I32)) => crate::core::mir::MirManagedAnyOperation::UnpackI32,
                Some(crate::core::ResolvedType::Primitive(crate::core::PrimitiveType::String)) => crate::core::mir::MirManagedAnyOperation::UnpackString,
                _ => return Err("checked Any conversion target is outside the v1 i32/String descriptor contract".into()),
            };
            let key = (owner.clone(), origin, conversion.from, conversion.to);
            if expected.insert(key, operation).is_some() {
                return Err(
                    "a checked conversion origin carries duplicate Any unpack obligations".into(),
                );
            }
        }
    }
    let mut actual = BTreeSet::new();
    for function in program.functions().values() {
        catalog.validate_managed_any_function(function)?;
        for instruction in function.blocks.values().flat_map(|b| &b.instructions) {
            let MirInstructionKind::ManagedAny {
                result,
                source,
                operation,
                contract,
            } = &instruction.kind
            else {
                continue;
            };
            if !matches!(
                operation,
                crate::core::mir::MirManagedAnyOperation::UnpackI32
                    | crate::core::mir::MirManagedAnyOperation::UnpackString
            ) {
                continue;
            }
            catalog.validate_managed_any_contract(
                function,
                source,
                result.as_ref(),
                *operation,
                contract.as_ref(),
            )?;
            let contract = contract
                .as_ref()
                .ok_or_else(|| "ManagedAny unpack has no receipt".to_string())?;
            let target = contract
                .target_type
                .as_ref()
                .ok_or_else(|| "ManagedAny unpack receipt has no target".to_string())?;
            // Source IDs intentionally differ: CheckedProgram retains the
            // exact DynamicAny TypeDesc; canonical lowering substitutes its
            // independently sealed generation-bearing ManagedAny TypeDesc.
            let matches = expected
                .iter()
                .filter(|((owner, origin, _, to), op)| {
                    owner == &function.owner
                        && origin == &contract.origin
                        && to == target
                        && *op == operation
                })
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            if matches.len() != 1
                || contract.source_type
                    != crate::core::mir::types::managed_any_i32_string_v1_type_id()
            {
                return Err("ManagedAny unpack has no unique exact checked (owner,origin,from,to) conversion".into());
            }
            if !actual.insert(matches[0].clone()) {
                return Err(
                    "one checked Any conversion is claimed by multiple owner receipts".into(),
                );
            }
        }
    }
    for receipt in program.ordinary_map_owned_any_v2_unpack_result_receipts()? {
        receipt.validate_against_program(program)?;
        let key = (
            receipt.owner.clone(),
            receipt.cast.clone(),
            receipt.conversion_from.clone(),
            receipt.conversion_to.clone(),
        );
        if !expected.contains_key(&key) {
            return Err(
                "AnyOwner-v2 unpack receipt has no exact checked (owner,origin,from,to) conversion"
                    .into(),
            );
        }
        if !actual.insert(key) {
            return Err("one checked Any conversion is claimed by multiple owner receipts".into());
        }
    }
    if actual.len() != expected.len() {
        return Err(
            "checked Any conversions do not have complete canonical owner receipt coverage".into(),
        );
    }
    Ok(())
}
