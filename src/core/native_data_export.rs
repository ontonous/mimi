//! Checked scalar C data-symbol exports.
//!
//! These declarations are materialized once from the checked source and then
//! consumed by native emitters without reopening the source AST.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{Expr, File, Item, Lit, Type, UnOp};
use crate::diagnostic::Diagnostic;
use crate::span::Span;

use super::{CheckedProgram, NodeId, ResolvedConstValue};

pub const NATIVE_DATA_EXPORT_ERROR_CODE: &str = "NATIVE-DATA-EXPORT-001";
pub const NATIVE_DATA_EXPORT_TABLE_RECEIPT_SCHEMA: &str =
    "mimi-native-data-export-table-receipt-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeDataExportType {
    I32,
    I64,
    F32,
    F64,
    Bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NativeDataExportValue {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    Bool(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckedNativeDataExport {
    pub owner: NodeId,
    pub symbol: String,
    pub ty: NativeDataExportType,
    pub value: NativeDataExportValue,
    pub span: Span,
}

/// Source-bound witness for the native-only data-symbol table. The canonical
/// MIR receipt remains about executable functions; this companion receipt
/// covers the C-visible global objects emitted alongside that MIR graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeDataExportTableReceipt {
    pub schema: &'static str,
    pub export_count: usize,
    pub table_digest: String,
}

impl NativeDataExportTableReceipt {
    /// Combine this native data table with the selected MIR route identity.
    /// Length framing and a separate domain keep the combined build identity
    /// unambiguous without changing the shared MIR-receipt schema.
    pub fn route_identity_digest(&self, mir_route_identity_digest: &str) -> String {
        if self.export_count == 0 {
            return mir_route_identity_digest.to_owned();
        }
        let mut framed = String::from("mimi-native-build-route-identity-v1");
        frame(&mut framed, self.schema);
        frame(&mut framed, mir_route_identity_digest);
        frame(&mut framed, &self.table_digest);
        blake3::hash(framed.as_bytes()).to_hex().to_string()
    }
}

/// Build descriptors for the narrow C data ABI. Only explicitly typed scalar
/// literals are accepted; source computations and aggregates have no checked
/// descriptor and are rejected during CheckedProgram construction.
pub(crate) fn build_checked_native_data_exports(
    file: &File,
    program: &CheckedProgram,
) -> Result<Vec<CheckedNativeDataExport>, Vec<Diagnostic>> {
    let mut errors = Vec::new();
    let mut exports = Vec::new();
    let mut source_owners = BTreeSet::new();
    let mut symbols = BTreeMap::<String, Span>::new();

    for item in &file.items {
        let Item::Const {
            meta,
            name,
            ty,
            value,
            extern_abi: Some(abi),
            ..
        } = item
        else {
            continue;
        };
        let span = meta.span;
        let owner = NodeId(format!("constant:{name}"));
        if !source_owners.insert(owner.clone()) {
            errors.push(export_error(
                span,
                format!("duplicate exported data symbol '{name}'"),
            ));
            continue;
        }
        if let Some(previous_span) = symbols.insert(name.clone(), span) {
            errors.push(export_error(
                span,
                format!("duplicate exported data symbol '{name}' (previous declaration at {previous_span:?})"),
            ));
            continue;
        }
        if abi != "C" {
            errors.push(export_error(
                span,
                format!("data symbol '{name}' uses unsupported ABI '{abi}'; only extern \"C\" is supported"),
            ));
            continue;
        }

        let Some(constant) = program.constants().get(&owner) else {
            errors.push(export_error(
                span,
                format!("checked constant descriptor for exported symbol '{name}' is missing"),
            ));
            continue;
        };
        if constant.qualified_name != *name || constant.extern_abi.as_deref() != Some("C") {
            errors.push(export_error(
                span,
                format!("checked constant descriptor for exported symbol '{name}' does not match its declaration"),
            ));
            continue;
        }

        let Some(source_type) = ty.as_ref() else {
            errors.push(export_error(
                span,
                format!(
                    "exported data symbol '{name}' requires an explicit scalar type annotation"
                ),
            ));
            continue;
        };
        let Some(export_type) = scalar_type(source_type, program) else {
            errors.push(export_error(
                span,
                format!("exported data symbol '{name}' has unsupported type {}; supported types are i32, i64, f32, f64, and bool", crate::core::fmt_type(source_type)),
            ));
            continue;
        };
        let Some(source_value) = literal_value(value) else {
            errors.push(export_error(
                span,
                format!("exported data symbol '{name}' requires a scalar literal initializer"),
            ));
            continue;
        };
        let source_type_name = crate::core::fmt_type(source_type);
        if constant.ty.as_deref() != Some(source_type_name.as_str())
            || constant.value != source_value
        {
            errors.push(export_error(
                span,
                format!("checked constant descriptor for exported symbol '{name}' does not match its declared type or literal initializer"),
            ));
            continue;
        }
        let Some(export_value) = typed_value(export_type, &source_value) else {
            errors.push(export_error(
                span,
                format!("literal initializer for exported symbol '{name}' does not fit its declared type {source_type_name}"),
            ));
            continue;
        };

        if let Some(function) = function_symbol_collision(program, name) {
            errors.push(export_error(
                span,
                format!("exported data symbol '{name}' collides with function symbol '{function}'"),
            ));
            continue;
        }

        exports.push(CheckedNativeDataExport {
            owner,
            symbol: name.clone(),
            ty: export_type,
            value: export_value,
            span,
        });
    }

    for constant in program.constants().values() {
        if constant.extern_abi.is_some() && !source_owners.contains(&constant.node_id) {
            errors.push(export_error(
                constant.origin.user_span(),
                format!(
                    "checked exported constant '{}' has no source declaration",
                    constant.qualified_name
                ),
            ));
        }
    }

    if errors.is_empty() {
        exports.sort_by(|left, right| left.symbol.cmp(&right.symbol));
        Ok(exports)
    } else {
        Err(errors)
    }
}

/// Verify descriptor completeness and consistency before a native route emits
/// LLVM. This catches missing or altered checked records independently of the
/// emitter route selected for the function bodies.
pub fn validate_checked_native_data_exports(
    program: &CheckedProgram,
) -> Result<(), Vec<Diagnostic>> {
    let mut errors = Vec::new();
    let mut symbols = BTreeSet::new();

    for constant in program.constants().values() {
        match (
            constant.extern_abi.as_deref(),
            constant.native_data_export.as_ref(),
        ) {
            (None, None) => {}
            (None, Some(export)) => errors.push(export_error(
                export.span,
                format!(
                    "data export descriptor '{}' is attached to a non-exported constant",
                    export.symbol
                ),
            )),
            (Some(abi), None) => errors.push(export_error(
                constant.origin.user_span(),
                format!(
                    "checked descriptor for extern '{abi}' constant '{}' is missing",
                    constant.qualified_name
                ),
            )),
            (Some("C"), Some(export)) => {
                if export.span != constant.origin.user_span() {
                    errors.push(export_error(
                        constant.origin.user_span(),
                        format!(
                            "checked descriptor for exported data symbol '{}' does not match its source span",
                            export.symbol
                        ),
                    ));
                    continue;
                }
                if export.owner != constant.node_id
                    || export.symbol != constant.qualified_name
                    || constant
                        .ty
                        .as_deref()
                        .and_then(|surface_type| scalar_type_name(surface_type, program))
                        != Some(export.ty)
                    || !value_variant_matches_type(export.ty, &export.value)
                    || !value_matches_constant(export, &constant.value)
                {
                    errors.push(export_error(
                        export.span,
                        format!("checked descriptor for exported data symbol '{}' does not match its constant; descriptor value variant must match its declared type", export.symbol),
                    ));
                    continue;
                }
                if !symbols.insert(export.symbol.clone()) {
                    errors.push(export_error(
                        export.span,
                        format!("duplicate exported data symbol '{}'", export.symbol),
                    ));
                }
                if let Some(function) = function_symbol_collision(program, &export.symbol) {
                    errors.push(export_error(
                        export.span,
                        format!(
                            "exported data symbol '{}' collides with function symbol '{function}'",
                            export.symbol
                        ),
                    ));
                }
            }
            (Some(abi), Some(export)) => errors.push(export_error(
                export.span,
                format!(
                    "data export descriptor '{}' uses unsupported ABI '{abi}'",
                    export.symbol
                ),
            )),
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        errors.sort_by(|left, right| {
            left.span
                .start_line
                .cmp(&right.span.start_line)
                .then(left.span.start_col.cmp(&right.span.start_col))
                .then(left.message.cmp(&right.message))
        });
        Err(errors)
    }
}

/// Build the deterministic companion receipt from the complete checked
/// descriptor table. The source key and declaration span bind each exported
/// object to its checked source owner; the typed value is encoded by exact
/// integer or IEEE bit representation rather than display formatting.
pub fn checked_native_data_export_table_receipt(
    program: &CheckedProgram,
) -> Result<NativeDataExportTableReceipt, Vec<Diagnostic>> {
    validate_checked_native_data_exports(program)?;

    let mut exports = program
        .constants()
        .values()
        .filter_map(|constant| constant.native_data_export.as_ref())
        .collect::<Vec<_>>();
    exports.sort_by(|left, right| {
        left.symbol
            .cmp(&right.symbol)
            .then(left.owner.0.cmp(&right.owner.0))
    });

    let mut framed = String::from(NATIVE_DATA_EXPORT_TABLE_RECEIPT_SCHEMA);
    frame(&mut framed, &exports.len().to_string());
    let mut source_keys = BTreeMap::new();
    for export in &exports {
        let Some(source_key) = program.source_registry().key(export.span.source_id) else {
            return Err(vec![export_error(
                export.span,
                format!(
                    "exported data symbol '{}' has no registered stable source key",
                    export.symbol
                ),
            )]);
        };
        source_keys.insert(export.owner.clone(), source_key.as_str().to_owned());
    }
    for export in &exports {
        let source_key = source_keys
            .get(&export.owner)
            .expect("source keys were checked before receipt encoding");
        frame(&mut framed, &export.owner.0);
        frame(&mut framed, &export.symbol);
        frame(&mut framed, "C");
        frame(&mut framed, export.ty.as_type_name());
        frame(&mut framed, &canonical_value(&export.value));
        frame(&mut framed, source_key);
        frame(&mut framed, &export.span.start_line.to_string());
        frame(&mut framed, &export.span.start_col.to_string());
        frame(&mut framed, &export.span.end_line.to_string());
        frame(&mut framed, &export.span.end_col.to_string());
    }

    Ok(NativeDataExportTableReceipt {
        schema: NATIVE_DATA_EXPORT_TABLE_RECEIPT_SCHEMA,
        export_count: exports.len(),
        table_digest: blake3::hash(framed.as_bytes()).to_hex().to_string(),
    })
}

fn canonical_value(value: &NativeDataExportValue) -> String {
    match value {
        NativeDataExportValue::I32(value) => format!("i32:{value}"),
        NativeDataExportValue::I64(value) => format!("i64:{value}"),
        NativeDataExportValue::F32(value) => format!("f32:{:08x}", value.to_bits()),
        NativeDataExportValue::F64(value) => format!("f64:{:016x}", value.to_bits()),
        NativeDataExportValue::Bool(value) => format!("bool:{value}"),
    }
}

fn frame(output: &mut String, value: &str) {
    use std::fmt::Write as _;
    write!(output, "\n{}:", value.len()).expect("writing a String cannot fail");
    output.push_str(value);
}

fn scalar_type(ty: &Type, program: &CheckedProgram) -> Option<NativeDataExportType> {
    let Type::Name(name, arguments) = ty.unlocated() else {
        return None;
    };
    if !arguments.is_empty() {
        return None;
    }
    scalar_type_from_primitive(crate::core::resolved::transparent_alias_primitive(
        name, program,
    )?)
}

fn scalar_type_name(surface_type: &str, program: &CheckedProgram) -> Option<NativeDataExportType> {
    if surface_type.contains('<') || surface_type.contains('>') {
        return None;
    }
    scalar_type_from_primitive(crate::core::resolved::transparent_alias_primitive(
        surface_type,
        program,
    )?)
}

fn scalar_type_from_primitive(
    primitive: crate::core::PrimitiveType,
) -> Option<NativeDataExportType> {
    match primitive {
        crate::core::PrimitiveType::I32 => Some(NativeDataExportType::I32),
        crate::core::PrimitiveType::I64 => Some(NativeDataExportType::I64),
        crate::core::PrimitiveType::F32 => Some(NativeDataExportType::F32),
        crate::core::PrimitiveType::F64 => Some(NativeDataExportType::F64),
        crate::core::PrimitiveType::Bool => Some(NativeDataExportType::Bool),
        _ => None,
    }
}

fn literal_value(expr: &Expr) -> Option<ResolvedConstValue> {
    match expr.unlocated() {
        Expr::Literal(Lit::Int(value)) => Some(ResolvedConstValue::Int(*value)),
        Expr::Literal(Lit::Float(value)) => Some(ResolvedConstValue::Float(*value)),
        Expr::Literal(Lit::Bool(value)) => Some(ResolvedConstValue::Bool(*value)),
        Expr::Unary(UnOp::Neg, inner) => match inner.unlocated() {
            Expr::Literal(Lit::Int(value)) => value.checked_neg().map(ResolvedConstValue::Int),
            Expr::Literal(Lit::Float(value)) => Some(ResolvedConstValue::Float(-*value)),
            _ => None,
        },
        _ => None,
    }
}

fn typed_value(
    ty: NativeDataExportType,
    value: &ResolvedConstValue,
) -> Option<NativeDataExportValue> {
    match (ty, value) {
        (NativeDataExportType::I32, ResolvedConstValue::Int(value)) => {
            i32::try_from(*value).ok().map(NativeDataExportValue::I32)
        }
        (NativeDataExportType::I64, ResolvedConstValue::Int(value)) => {
            Some(NativeDataExportValue::I64(*value))
        }
        (NativeDataExportType::F32, ResolvedConstValue::Float(value)) if value.is_finite() => {
            let value = *value as f32;
            value
                .is_finite()
                .then_some(NativeDataExportValue::F32(value))
        }
        (NativeDataExportType::F64, ResolvedConstValue::Float(value)) if value.is_finite() => {
            Some(NativeDataExportValue::F64(*value))
        }
        (NativeDataExportType::Bool, ResolvedConstValue::Bool(value)) => {
            Some(NativeDataExportValue::Bool(*value))
        }
        _ => None,
    }
}

fn value_matches_constant(export: &CheckedNativeDataExport, value: &ResolvedConstValue) -> bool {
    match (&export.value, value) {
        (NativeDataExportValue::I32(exported), ResolvedConstValue::Int(actual)) => {
            i64::from(*exported) == *actual
        }
        (NativeDataExportValue::I64(exported), ResolvedConstValue::Int(actual)) => {
            exported == actual
        }
        (NativeDataExportValue::F32(exported), ResolvedConstValue::Float(actual)) => {
            actual.is_finite() && (*actual as f32).to_bits() == exported.to_bits()
        }
        (NativeDataExportValue::F64(exported), ResolvedConstValue::Float(actual)) => {
            exported.to_bits() == actual.to_bits()
        }
        (NativeDataExportValue::Bool(exported), ResolvedConstValue::Bool(actual)) => {
            exported == actual
        }
        _ => false,
    }
}

fn value_variant_matches_type(ty: NativeDataExportType, value: &NativeDataExportValue) -> bool {
    matches!(
        (ty, value),
        (NativeDataExportType::I32, NativeDataExportValue::I32(_))
            | (NativeDataExportType::I64, NativeDataExportValue::I64(_))
            | (NativeDataExportType::F32, NativeDataExportValue::F32(_))
            | (NativeDataExportType::F64, NativeDataExportValue::F64(_))
            | (NativeDataExportType::Bool, NativeDataExportValue::Bool(_))
    )
}

fn function_symbol_collision(program: &CheckedProgram, symbol: &str) -> Option<String> {
    let mut symbols = BTreeSet::new();
    for function in program.functions().values() {
        symbols.insert(function.qualified_name.clone());
        if let Some(short_name) = function.qualified_name.rsplit("::").next() {
            symbols.insert(short_name.to_owned());
        }
    }
    for declaration in program.native_declarations().exports.values() {
        symbols.insert(declaration.public_symbol.clone());
    }
    for block in program.extern_blocks().values() {
        for function in &block.signatures {
            symbols.insert(function.name.clone());
        }
    }
    symbols.contains(symbol).then(|| symbol.to_owned())
}

fn export_error(span: Span, message: String) -> Diagnostic {
    Diagnostic::error_code(NATIVE_DATA_EXPORT_ERROR_CODE, message, span)
}

impl NativeDataExportType {
    pub const fn as_type_name(self) -> &'static str {
        match self {
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::Bool => "bool",
        }
    }
}
