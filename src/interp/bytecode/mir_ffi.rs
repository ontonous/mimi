//! AST-free scalar FFI runtime for Canonical MIR bytecode.
//!
//! This module is deliberately separate from `ffi_runtime`: that runtime
//! consumes surface `ExternFunc`/`FfiContract` declarations and is retained
//! for compatibility bytecode.  Canonical MIR bytecode receives a fully
//! materialized descriptor from the MIR adapter and only performs the physical
//! symbol lookup/call described by that receipt.

use std::any::Any;
use std::ffi::c_void;

use libffi::middle::{arg as ffi_arg, Cif, CodePtr, Type as FfiType};
use libloading::Library;

use super::instr::{CanonicalFfiDescriptor, CanonicalFfiScalarType};
use crate::interp::value::Value;

fn ffi_contract_runtime_error(
    error: crate::core::mir::MirFfiContractError,
    phase: &str,
) -> crate::interp::InterpError {
    use crate::interp::InterpError;

    match error {
        crate::core::mir::MirFfiContractError::Invalid(message) => InterpError::new(message),
        crate::core::mir::MirFfiContractError::Violation => {
            InterpError::contract_violation(format!("FFI {phase} failed"))
        }
        crate::core::mir::MirFfiContractError::Overflow => {
            InterpError::integer_overflow(format!("integer overflow in FFI {phase}"))
        }
        crate::core::mir::MirFfiContractError::DivisionByZero => {
            let mut error = InterpError::div_by_zero();
            error.ctx_mut().msg = format!("division by zero in FFI {phase}");
            error
        }
    }
}

/// Candidate system libc paths for the no-configuration scalar FFI profile.
fn default_libc_candidates() -> [&'static str; 5] {
    [
        "/lib/x86_64-linux-gnu/libc.so.6",
        "/usr/lib/x86_64-linux-gnu/libc.so.6",
        "/lib64/libc.so.6",
        "/usr/lib/libc.so.6",
        "/lib/libc.so.6",
    ]
}

/// AST-free dynamic library state owned by one bytecode VM.
pub(crate) struct CanonicalMirFfiRuntime {
    loaded_libs: Vec<(String, Library)>,
    pub(crate) verify_requires: bool,
}

impl CanonicalMirFfiRuntime {
    pub(crate) fn new() -> Self {
        Self {
            loaded_libs: Vec::new(),
            verify_requires: true,
        }
    }

    /// Execute one checker-owned scalar descriptor.
    ///
    /// The descriptor has already passed the MIR adapter's TypeDesc/layout
    /// checks.  The repeated runtime checks are intentional: a manually
    /// assembled `BytecodeProgram` must not turn a malformed descriptor into
    /// an unchecked libffi call.
    pub(crate) fn call(
        &mut self,
        descriptor: &CanonicalFfiDescriptor,
        args: &[Value],
    ) -> Result<Value, crate::interp::InterpError> {
        self.call_with_context(descriptor, args, None, None)
    }

    /// Execute a descriptor while binding it to the bytecode function that
    /// issued the canonical call. The direct `call` helper remains useful for
    /// focused runtime tests, but the VM has the actual frame owner and should
    /// use this contextual entry point so a forged descriptor cannot rewrite
    /// its caller provenance.
    pub(crate) fn call_from_caller(
        &mut self,
        descriptor: &CanonicalFfiDescriptor,
        args: &[Value],
        expected_caller: &str,
    ) -> Result<Value, crate::interp::InterpError> {
        self.call_with_context(descriptor, args, Some(expected_caller), None)
    }

    /// Execute a descriptor while binding both checker-owned provenance
    /// identities carried by the bytecode instruction.  The caller-only
    /// helper remains useful for direct runtime tests; production VM calls
    /// should provide the instruction identity as well.
    pub(crate) fn call_from_context(
        &mut self,
        descriptor: &CanonicalFfiDescriptor,
        args: &[Value],
        expected_caller: &str,
        expected_instruction: &str,
    ) -> Result<Value, crate::interp::InterpError> {
        self.call_with_context(
            descriptor,
            args,
            Some(expected_caller),
            Some(expected_instruction),
        )
    }

    fn call_with_context(
        &mut self,
        descriptor: &CanonicalFfiDescriptor,
        args: &[Value],
        expected_caller: Option<&str>,
        expected_instruction: Option<&str>,
    ) -> Result<Value, crate::interp::InterpError> {
        self.validate_descriptor(descriptor, args, expected_caller, expected_instruction)
            .map_err(crate::interp::InterpError::new)?;
        let converted_args = self
            .convert_arguments(descriptor, args)
            .map_err(crate::interp::InterpError::new)?;
        if let Some(condition) = descriptor
            .requires
            .as_ref()
            .filter(|_| self.verify_requires)
        {
            crate::core::mir::evaluate_ffi_requires(condition, |id| {
                let index = descriptor
                    .argument_ids
                    .iter()
                    .position(|argument| argument == id)
                    .ok_or_else(|| "FFI precondition references a non-argument".to_string())?;
                match args
                    .get(index)
                    .ok_or_else(|| "FFI precondition argument index is out of range".to_string())?
                {
                    Value::Int(value) => Ok(crate::core::mir::MirContractScalar::Int(*value)),
                    Value::Bool(value) => Ok(crate::core::mir::MirContractScalar::Bool(*value)),
                    _ => Err("FFI precondition argument is not an integer or bool".into()),
                }
            })
            .map_err(|error| ffi_contract_runtime_error(error, "precondition"))?;
        }

        let output = self
            .call_abi(descriptor, &converted_args)
            .map_err(crate::interp::InterpError::new)?;
        if let Some(condition) = descriptor.ensures.as_ref().filter(|_| self.verify_requires) {
            crate::core::mir::evaluate_ffi_ensures(condition, |id| {
                if let Some(index) = descriptor
                    .argument_ids
                    .iter()
                    .position(|argument| argument == id)
                {
                    return match args.get(index) {
                        Some(Value::Int(value)) => {
                            Ok(crate::core::mir::MirContractScalar::Int(*value))
                        }
                        Some(Value::Bool(value)) => {
                            Ok(crate::core::mir::MirContractScalar::Bool(*value))
                        }
                        _ => Err("FFI postcondition argument is not an integer or bool".into()),
                    };
                }
                if descriptor.result_id.as_ref() == Some(id) {
                    return match &output {
                        Value::Int(value) => Ok(crate::core::mir::MirContractScalar::Int(*value)),
                        Value::Bool(value) => Ok(crate::core::mir::MirContractScalar::Bool(*value)),
                        _ => Err("FFI postcondition result is not an integer or bool".into()),
                    };
                }
                Err("FFI postcondition references an unknown value".into())
            })
            .map_err(|error| ffi_contract_runtime_error(error, "postcondition"))?;
        }
        Ok(output)
    }

    fn convert_arguments(
        &self,
        descriptor: &CanonicalFfiDescriptor,
        args: &[Value],
    ) -> Result<Vec<Value>, String> {
        args.iter()
            .zip(&descriptor.parameter_conversions)
            .enumerate()
            .map(|(index, (value, conversion))| {
                apply_argument_conversion(value, conversion).map_err(|error| {
                    format!(
                        "canonical MIR FFI argument {index} for '{}' conversion failed: {error}",
                        descriptor.symbol
                    )
                })
            })
            .collect()
    }

    /// Validate descriptor invariants before touching a dynamic library or
    /// evaluating a contract predicate.
    ///
    /// call runs this before requires/ensures; malformed hand-built bytecode
    /// therefore fails at the descriptor boundary even when its predicate
    /// payload is malformed too.
    fn validate_descriptor(
        &self,
        descriptor: &CanonicalFfiDescriptor,
        args: &[Value],
        expected_caller: Option<&str>,
        expected_instruction: Option<&str>,
    ) -> Result<(), String> {
        if let Some(expected_caller) = expected_caller {
            if descriptor.caller != expected_caller {
                return Err(format!(
                    "canonical MIR FFI descriptor caller '{}' disagrees with bytecode caller '{}'",
                    descriptor.caller, expected_caller
                ));
            }
        }
        if let Some(expected_instruction) = expected_instruction {
            if descriptor.instruction != expected_instruction {
                return Err(format!(
                    "canonical MIR FFI descriptor instruction '{}' disagrees with bytecode instruction '{}'",
                    descriptor.instruction, expected_instruction
                ));
            }
        }
        if descriptor.abi != "C" {
            return Err(format!(
                "canonical MIR FFI ABI '{}' is outside the C scalar island",
                descriptor.abi
            ));
        }
        if let Err(message) =
            crate::core::mir::validate_ffi_symbol_manifest_safety(&descriptor.symbol)
        {
            return Err(format!("canonical MIR {message}"));
        }
        crate::core::mir::validate_ffi_symbol_matches_callee(
            &crate::core::NodeId(descriptor.callee.clone()),
            &descriptor.symbol,
        )
        .map_err(|message| format!("canonical MIR {message}"))?;
        if descriptor.arguments.len() != args.len() {
            return Err(format!(
                "canonical MIR FFI symbol '{}' expects {} arguments, got {}",
                descriptor.symbol,
                descriptor.arguments.len(),
                args.len()
            ));
        }
        let result_is_unit = matches!(descriptor.result, CanonicalFfiScalarType::Unit);
        if !result_is_unit && descriptor.result_id.is_none() {
            return Err("canonical FFI non-Unit result has no result identity".into());
        }
        if descriptor.result_id.is_none() && descriptor.result_conversion.is_some() {
            return Err("canonical FFI result conversion has no result identity".into());
        }
        if descriptor.result_id.is_some() && descriptor.result_conversion.is_none() {
            return Err("canonical FFI result identity has no conversion receipt".into());
        }
        if descriptor.parameter_conversions.len() != descriptor.arguments.len() {
            return Err("canonical FFI parameter conversion receipt arity mismatch".into());
        }
        if descriptor.arguments.contains(&CanonicalFfiScalarType::Unit) {
            return Err("unit is not a canonical scalar FFI argument".into());
        }
        for (index, (scalar, conversion)) in descriptor
            .arguments
            .iter()
            .zip(&descriptor.parameter_conversions)
            .enumerate()
        {
            if !conversion.is_supported_argument() {
                return Err(format!(
                    "canonical FFI argument {index} ABI conversion from {:?} to {:?} is unsupported",
                    conversion.from, conversion.to
                ));
            }
            if scalar_abi_class(scalar) != conversion.to {
                return Err(format!(
                    "canonical FFI argument {index} conversion target {:?} disagrees with declaration ABI {:?}",
                    conversion.to,
                    scalar_abi_class(scalar)
                ));
            }
        }
        if let Some(result_conversion) = descriptor.result_conversion.as_ref() {
            if !result_conversion.is_supported_result() {
                return Err(format!(
                    "canonical FFI result ABI conversion from {:?} to {:?} is unsupported",
                    result_conversion.from, result_conversion.to
                ));
            }
            if scalar_abi_class(&descriptor.result) != result_conversion.from {
                return Err(
                    "canonical FFI result conversion source disagrees with declaration ABI".into(),
                );
            }
        }
        if descriptor.argument_ids.len() != args.len() {
            return Err("canonical FFI argument identity arity mismatch".into());
        }
        if descriptor.result_id.as_ref().is_some_and(|result_id| {
            descriptor
                .argument_ids
                .iter()
                .any(|argument_id| argument_id == result_id)
        }) {
            return Err("canonical FFI result identity overlaps an argument identity".into());
        }
        let argument_abis = descriptor
            .parameter_conversions
            .iter()
            .map(|conversion| conversion.from)
            .collect::<Vec<_>>();
        crate::core::mir::validate_ffi_runtime_contracts(
            descriptor.requires.as_ref(),
            descriptor.ensures.as_ref(),
            &descriptor.argument_ids,
            &argument_abis,
            descriptor.result_id.as_ref(),
            descriptor
                .result_conversion
                .as_ref()
                .map_or(crate::core::mir::types::MirAbiClass::Unit, |conversion| {
                    conversion.to
                }),
        )?;
        Ok(())
    }

    fn call_abi(
        &mut self,
        descriptor: &CanonicalFfiDescriptor,
        converted_args: &[Value],
    ) -> Result<Value, String> {
        let lib_path = match std::env::var("MIMI_FFI_LIB") {
            Ok(path) => path,
            Err(_) => default_libc_candidates()
                .into_iter()
                .find(|candidate| std::path::Path::new(candidate).exists())
                .map(str::to_owned)
                .ok_or_else(|| {
                    "canonical MIR FFI needs MIMI_FFI_LIB or a discoverable system libc".to_owned()
                })?,
        };
        let lib_idx = if let Some(index) = self
            .loaded_libs
            .iter()
            .position(|(path, _)| path == &lib_path)
        {
            index
        } else {
            // SAFETY: libloading keeps the library handle alive in
            // `loaded_libs` for every symbol call made below.
            let library = unsafe {
                Library::new(&lib_path)
                    .map_err(|error| format!("failed to load '{}': {error}", lib_path))?
            };
            self.loaded_libs.push((lib_path.clone(), library));
            self.loaded_libs.len() - 1
        };

        let argument_types = descriptor
            .arguments
            .iter()
            .map(ffi_type)
            .collect::<Result<Vec<_>, _>>()?;
        let return_type = ffi_type(&descriptor.result)?;
        let cif = Cif::new(argument_types, return_type);

        // libffi::Arg stores an address into the typed value.  Keep every
        // boxed scalar alive and immovable until the synchronous call ends.
        let mut storage: Vec<Box<dyn Any>> = Vec::with_capacity(converted_args.len());
        let mut ffi_args = Vec::with_capacity(converted_args.len());
        for (value, scalar) in converted_args.iter().zip(&descriptor.arguments) {
            match scalar {
                CanonicalFfiScalarType::I32 => {
                    let number = match value {
                        Value::Int(number) => i32::try_from(*number).map_err(|_| {
                            format!(
                                "canonical MIR FFI argument for '{}' is outside i32",
                                descriptor.symbol
                            )
                        })?,
                        other => return Err(format!("expected i32 FFI argument, got {other:?}")),
                    };
                    storage.push(Box::new(number));
                    let typed = storage
                        .last()
                        .and_then(|value| value.downcast_ref::<i32>())
                        .ok_or_else(|| "canonical MIR FFI i32 storage lost its type".to_owned())?;
                    ffi_args.push(ffi_arg(typed));
                }
                CanonicalFfiScalarType::I64 => {
                    let number = match value {
                        Value::Int(number) => *number,
                        other => return Err(format!("expected i64 FFI argument, got {other:?}")),
                    };
                    storage.push(Box::new(number));
                    let typed = storage
                        .last()
                        .and_then(|value| value.downcast_ref::<i64>())
                        .ok_or_else(|| "canonical MIR FFI i64 storage lost its type".to_owned())?;
                    ffi_args.push(ffi_arg(typed));
                }
                CanonicalFfiScalarType::Bool => {
                    let boolean = match value {
                        Value::Bool(boolean) => u8::from(*boolean),
                        other => return Err(format!("expected bool FFI argument, got {other:?}")),
                    };
                    storage.push(Box::new(boolean));
                    let typed = storage
                        .last()
                        .and_then(|value| value.downcast_ref::<u8>())
                        .ok_or_else(|| "canonical MIR FFI bool storage lost its type".to_owned())?;
                    ffi_args.push(ffi_arg(typed));
                }
                CanonicalFfiScalarType::F64 => {
                    let number = match value {
                        Value::Float(number) => *number,
                        other => return Err(format!("expected f64 FFI argument, got {other:?}")),
                    };
                    storage.push(Box::new(number));
                    let typed = storage
                        .last()
                        .and_then(|value| value.downcast_ref::<f64>())
                        .ok_or_else(|| "canonical MIR FFI f64 storage lost its type".to_owned())?;
                    ffi_args.push(ffi_arg(typed));
                }
                CanonicalFfiScalarType::Unit => {
                    return Err("unit is not a canonical scalar FFI argument".into());
                }
            }
        }

        // SAFETY: `cif` matches the typed argument/return storage above;
        // `symbol` is looked up in the live library handle and the call is
        // synchronous, so all pointed-to storage remains valid.
        let raw_symbol: libloading::Symbol<*mut c_void> = unsafe {
            self.loaded_libs[lib_idx]
                .1
                .get(descriptor.symbol.as_bytes())
                .map_err(|error| {
                    format!(
                        "failed to find canonical MIR FFI symbol '{}': {error}",
                        descriptor.symbol
                    )
                })?
        };
        let code_ptr = CodePtr(*raw_symbol);
        // SAFETY: argument storage above matches every CIF type and stays
        // alive for the synchronous call; the library owns the live symbol.
        // As for native C FFI, the foreign declaration must accurately state
        // the symbol's ABI; that external contract is the program's obligation.
        let result = unsafe { call_typed(&cif, code_ptr, &ffi_args, &descriptor.result) }?;
        apply_result_conversion(result, descriptor.result_conversion.as_ref())
    }
}

fn apply_argument_conversion(
    value: &Value,
    conversion: &crate::core::mir::MirFfiAbiConversion,
) -> Result<Value, String> {
    use crate::core::mir::types::MirAbiClass;
    use crate::core::mir::MirFfiConversionKind;

    // `CanonicalMirFfiRuntime::validate_descriptor` owns conversion
    // admission.  This helper only applies a descriptor already cleared by
    // that preflight to the runtime value representation.
    let Some(kind) = conversion.kind() else {
        return Err(format!(
            "conversion from {:?} to {:?} received {value:?}",
            conversion.from, conversion.to
        ));
    };
    match kind {
        MirFfiConversionKind::Identity { abi } => match (abi, value) {
            (
                MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                },
                Value::Int(value),
            ) if i32::try_from(*value).is_err() => {
                Err("canonical MIR FFI argument is outside i32".into())
            }
            (MirAbiClass::Integer { bits: 32 | 64, .. }, Value::Int(_))
            | (MirAbiClass::Bool, Value::Bool(_))
            | (MirAbiClass::Float { bits: 64 }, Value::Float(_)) => Ok(value.clone()),
            (MirAbiClass::Integer { bits: 32, .. }, other) => {
                Err(format!("expected i32 FFI argument, got {other:?}"))
            }
            (MirAbiClass::Integer { bits: 64, .. }, other) => {
                Err(format!("expected i64 FFI argument, got {other:?}"))
            }
            (MirAbiClass::Bool, other) => Err(format!("expected bool FFI argument, got {other:?}")),
            (MirAbiClass::Float { bits: 64 }, other) => {
                Err(format!("expected f64 FFI argument, got {other:?}"))
            }
            (abi, value) => Err(format!(
                "identity conversion for {abi:?} received {value:?}"
            )),
        },
        MirFfiConversionKind::SignedIntegerWiden { from_bits, .. } => match value {
            Value::Int(value) if from_bits == 32 && i32::try_from(*value).is_err() => {
                Err("canonical MIR FFI argument is outside i32".into())
            }
            Value::Int(value) => Ok(Value::Int(*value)),
            other => Err(format!("expected i{from_bits} FFI argument, got {other:?}")),
        },
        MirFfiConversionKind::SignedIntegerToFloat { from_bits } => match value {
            Value::Int(value) if from_bits == 32 && i32::try_from(*value).is_err() => {
                Err("canonical MIR FFI argument is outside i32".into())
            }
            Value::Int(value) => Ok(Value::Float(*value as f64)),
            other => Err(format!("expected i{from_bits} FFI argument, got {other:?}")),
        },
        _ => Err(format!(
            "conversion from {:?} to {:?} is not an argument conversion",
            conversion.from, conversion.to
        )),
    }
}

fn apply_result_conversion(
    value: Value,
    conversion: Option<&crate::core::mir::MirFfiAbiConversion>,
) -> Result<Value, String> {
    let Some(conversion) = conversion else {
        return Ok(value);
    };
    // Descriptor preflight has already admitted the conversion; this helper
    // performs only the physical result conversion and its range guard.
    use crate::core::mir::MirFfiConversionKind;
    let Some(kind) = conversion.kind() else {
        return Err(format!(
            "canonical MIR FFI result conversion from {:?} to {:?} received {value:?}",
            conversion.from, conversion.to
        ));
    };
    match (kind, value) {
        (MirFfiConversionKind::Identity { .. }, value) => Ok(value),
        (MirFfiConversionKind::SignedIntegerNarrow { to_bits: 32, .. }, Value::Int(value)) => {
            i32::try_from(value)
                .map(|value| Value::Int(value as i64))
                .map_err(|_| "canonical MIR FFI result is outside i32".into())
        }
        (MirFfiConversionKind::FloatToSignedInteger { to_bits }, Value::Float(value)) => {
            let (lower, upper) = match to_bits {
                32 => (i32::MIN as f64, (i32::MAX as f64) + 1.0),
                64 => (i64::MIN as f64, 9_223_372_036_854_775_808.0),
                _ => {
                    return Err(format!(
                        "canonical MIR FFI result conversion target integer width {to_bits} is unsupported"
                    ))
                }
            };
            if !value.is_finite() || value < lower || value >= upper {
                return Err("canonical MIR FFI result is outside target integer range".into());
            }
            Ok(Value::Int(if to_bits == 32 {
                (value as i32) as i64
            } else {
                value as i64
            }))
        }
        (kind, value) => Err(format!(
            "canonical MIR FFI result conversion {:?} received {value:?}",
            kind
        )),
    }
}

fn ffi_type(scalar: &CanonicalFfiScalarType) -> Result<FfiType, String> {
    Ok(match scalar {
        CanonicalFfiScalarType::I32 => FfiType::i32(),
        CanonicalFfiScalarType::I64 => FfiType::i64(),
        CanonicalFfiScalarType::Bool => FfiType::u8(),
        CanonicalFfiScalarType::F64 => FfiType::f64(),
        CanonicalFfiScalarType::Unit => FfiType::void(),
    })
}

fn scalar_abi_class(scalar: &CanonicalFfiScalarType) -> crate::core::mir::types::MirAbiClass {
    scalar.abi_class()
}

// SAFETY: the caller must supply a live C function pointer, matching CIF,
// arguments with storage valid for that CIF, and the exact result ABI.
unsafe fn call_typed(
    cif: &Cif,
    code_ptr: CodePtr,
    args: &[libffi::middle::Arg],
    result: &CanonicalFfiScalarType,
) -> Result<Value, String> {
    Ok(match result {
        CanonicalFfiScalarType::I32 => Value::Int(cif.call::<i32>(code_ptr, args) as i64),
        CanonicalFfiScalarType::I64 => Value::Int(cif.call::<i64>(code_ptr, args)),
        CanonicalFfiScalarType::Bool => Value::Bool(cif.call::<u8>(code_ptr, args) != 0),
        CanonicalFfiScalarType::F64 => Value::Float(cif.call::<f64>(code_ptr, args)),
        CanonicalFfiScalarType::Unit => {
            cif.call::<()>(code_ptr, args);
            Value::Unit
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_ffi_result_receipt_applies_declared_float_to_mir_integer() {
        let conversion = crate::core::mir::MirFfiAbiConversion {
            from: crate::core::mir::types::MirAbiClass::Float { bits: 64 },
            to: crate::core::mir::types::MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
        };
        assert_eq!(
            apply_result_conversion(Value::Float(41.75), Some(&conversion)).unwrap(),
            Value::Int(41)
        );
        assert_eq!(
            apply_result_conversion(Value::Float(2_147_483_646.75), Some(&conversion)).unwrap(),
            Value::Int(2_147_483_646)
        );
        assert_eq!(
            apply_result_conversion(Value::Float(i32::MIN as f64), Some(&conversion)).unwrap(),
            Value::Int(i32::MIN as i64)
        );
        for value in [
            Value::Float(i32::MAX as f64 + 1.0),
            Value::Float(i32::MIN as f64 - 1.0),
            Value::Float(f64::NAN),
            Value::Float(f64::INFINITY),
        ] {
            assert!(
                apply_result_conversion(value, Some(&conversion)).is_err(),
                "out-of-range or non-finite f64 result must fail closed"
            );
        }

        let i64_conversion = crate::core::mir::MirFfiAbiConversion {
            from: crate::core::mir::types::MirAbiClass::Float { bits: 64 },
            to: crate::core::mir::types::MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
        };
        assert_eq!(
            apply_result_conversion(Value::Float(-41.75), Some(&i64_conversion)).unwrap(),
            Value::Int(-41)
        );
        for value in [
            Value::Float(9_223_372_036_854_775_808.0),
            Value::Float(-9_223_372_036_854_777_856.0),
            Value::Float(f64::NEG_INFINITY),
        ] {
            assert!(
                apply_result_conversion(value, Some(&i64_conversion)).is_err(),
                "out-of-range or non-finite i64 result must fail closed"
            );
        }

        let i64_to_i32 = crate::core::mir::MirFfiAbiConversion {
            from: crate::core::mir::types::MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
            to: crate::core::mir::types::MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
        };
        assert_eq!(
            apply_result_conversion(Value::Int(i32::MIN as i64), Some(&i64_to_i32)).unwrap(),
            Value::Int(i32::MIN as i64)
        );
        assert_eq!(
            apply_result_conversion(Value::Int(i32::MAX as i64), Some(&i64_to_i32)).unwrap(),
            Value::Int(i32::MAX as i64)
        );
        assert!(
            apply_result_conversion(Value::Int(i32::MAX as i64 + 1), Some(&i64_to_i32)).is_err()
        );
    }

    #[test]
    fn canonical_ffi_scalar_type_round_trips_mir_kind_and_abi() {
        use crate::core::mir::types::MirFfiScalarKind;

        for (kind, scalar) in [
            (MirFfiScalarKind::I32, CanonicalFfiScalarType::I32),
            (MirFfiScalarKind::I64, CanonicalFfiScalarType::I64),
            (MirFfiScalarKind::Bool, CanonicalFfiScalarType::Bool),
            (MirFfiScalarKind::F64, CanonicalFfiScalarType::F64),
            (MirFfiScalarKind::Unit, CanonicalFfiScalarType::Unit),
        ] {
            assert_eq!(CanonicalFfiScalarType::from_mir_kind(kind), scalar);
            assert_eq!(scalar.mir_kind(), kind);
            assert_eq!(scalar.abi_class(), kind.abi_class());
        }
    }

    #[test]
    fn scalar_ffi_argument_receipt_applies_mir_to_declaration_conversion() {
        use crate::core::mir::types::MirAbiClass;

        let i32_to_f64 = crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
            to: MirAbiClass::Float { bits: 64 },
        };
        assert_eq!(
            apply_argument_conversion(&Value::Int(7), &i32_to_f64).unwrap(),
            Value::Float(7.0)
        );
        assert!(apply_argument_conversion(&Value::Int(i64::MAX), &i32_to_f64).is_err());
        assert!(apply_argument_conversion(&Value::Float(7.0), &i32_to_f64).is_err());

        let i32_to_i64 = crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
            to: MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
        };
        assert_eq!(
            apply_argument_conversion(&Value::Int(-9), &i32_to_i64).unwrap(),
            Value::Int(-9)
        );
        assert!(apply_argument_conversion(&Value::Int(i64::MAX), &i32_to_i64).is_err());
    }

    fn descriptor(symbol: &str, argument: CanonicalFfiScalarType) -> CanonicalFfiDescriptor {
        let abi = match &argument {
            CanonicalFfiScalarType::I32 => crate::core::mir::types::MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
            CanonicalFfiScalarType::I64 => crate::core::mir::types::MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
            CanonicalFfiScalarType::Bool => crate::core::mir::types::MirAbiClass::Bool,
            CanonicalFfiScalarType::F64 => crate::core::mir::types::MirAbiClass::Float { bits: 64 },
            CanonicalFfiScalarType::Unit => crate::core::mir::types::MirAbiClass::Unit,
        };
        let result_id = (!matches!(argument, CanonicalFfiScalarType::Unit))
            .then(|| crate::core::mir::MirValueId::new("ffi-test-result").expect("result id"));
        let result_conversion = result_id
            .as_ref()
            .map(|_| crate::core::mir::MirFfiAbiConversion { from: abi, to: abi });
        CanonicalFfiDescriptor {
            caller: "function:main".into(),
            instruction: "ffi-test-call".into(),
            callee: format!("extern:C:test/function:{symbol}:0000000000000000"),
            symbol: symbol.into(),
            abi: "C".into(),
            arguments: vec![argument.clone()],
            parameter_conversions: vec![crate::core::mir::MirFfiAbiConversion {
                from: abi,
                to: abi,
            }],
            result: argument,
            result_conversion,
            argument_ids: vec![crate::core::mir::MirValueId::new("ffi-test-arg").unwrap()],
            requires: None,
            result_id,
            ensures: None,
        }
    }

    #[test]
    fn scalar_ffi_runtime_rejects_void_argument_before_libffi_preparation() {
        let mut runtime = CanonicalMirFfiRuntime::new();
        let error = runtime
            .call(
                &descriptor("unused", CanonicalFfiScalarType::Unit),
                &[Value::Unit],
            )
            .expect_err("void arguments must never reach libffi's CIF assertion");
        assert!(error
            .to_string()
            .contains("unit is not a canonical scalar FFI argument"));
        assert!(runtime.loaded_libs.is_empty());
    }

    #[test]
    fn scalar_ffi_runtime_rejects_invalid_descriptor_before_loading() {
        for (symbol, abi, args, expected) in [
            (
                "labs",
                "Rust",
                vec![Value::Int(1)],
                "outside the C scalar island",
            ),
            ("", "C", vec![Value::Int(1)], "symbol is empty"),
            (
                "labs\0other",
                "C",
                vec![Value::Int(1)],
                "contains a control character",
            ),
            (
                "labs other",
                "C",
                vec![Value::Int(1)],
                "contains whitespace or a manifest delimiter",
            ),
            (
                "labs=other",
                "C",
                vec![Value::Int(1)],
                "contains whitespace or a manifest delimiter",
            ),
            ("labs", "C", vec![], "expects 1 arguments, got 0"),
        ] {
            let mut runtime = CanonicalMirFfiRuntime::new();
            let mut call = descriptor(symbol, CanonicalFfiScalarType::I64);
            call.abi = abi.into();
            let error = runtime.call(&call, &args).expect_err("invalid descriptor");
            assert!(error.to_string().contains(expected), "{error}");
            assert!(runtime.loaded_libs.is_empty());
        }
    }

    #[test]
    fn scalar_ffi_runtime_rejects_descriptor_before_malformed_predicate() {
        let mut runtime = CanonicalMirFfiRuntime::new();
        let mut call = descriptor("bad symbol", CanonicalFfiScalarType::I64);
        call.requires = Some(crate::core::mir::MirContractExpr::Value(
            crate::core::mir::MirValueId::new("missing-predicate").expect("MIR value id"),
        ));
        let error = runtime
            .call(&call, &[Value::Int(1)])
            .expect_err("descriptor safety must precede predicate evaluation");
        assert!(
            error
                .to_string()
                .contains("FFI symbol is not manifest-safe"),
            "{error}"
        );
        assert!(runtime.loaded_libs.is_empty());
    }

    #[test]
    fn scalar_ffi_runtime_rejects_malformed_requires_before_library_load() {
        let mut runtime = CanonicalMirFfiRuntime::new();
        let mut call = descriptor("labs", CanonicalFfiScalarType::I64);
        call.requires = Some(crate::core::mir::MirContractExpr::Value(
            crate::core::mir::MirValueId::new("missing-predicate").expect("MIR value id"),
        ));
        let error = runtime
            .call(&call, &[Value::Int(1)])
            .expect_err("unknown requires identity must fail before the foreign call");
        assert!(error
            .to_string()
            .contains("extern requires value 'missing-predicate' is not a call argument"));
        assert!(runtime.loaded_libs.is_empty());
    }

    #[test]
    fn scalar_ffi_runtime_rejects_malformed_ensures_before_library_load() {
        let mut runtime = CanonicalMirFfiRuntime::new();
        let mut call = descriptor("labs", CanonicalFfiScalarType::I64);
        call.ensures = Some(crate::core::mir::MirContractExpr::Value(
            crate::core::mir::MirValueId::new("missing-predicate").expect("MIR value id"),
        ));
        let error = runtime
            .call(&call, &[Value::Int(1)])
            .expect_err("unknown ensures identity must fail before the foreign call");
        assert!(error.to_string().contains(
            "extern ensures value 'missing-predicate' is neither a call argument nor the call result"
        ));
        assert!(runtime.loaded_libs.is_empty());
    }

    #[test]
    fn scalar_ffi_runtime_predicates_follow_conversion_endpoints() {
        use crate::core::mir::types::MirAbiClass;
        use crate::core::mir::{MirContractBinaryOp as Op, MirContractExpr as Expr};

        let runtime = CanonicalMirFfiRuntime::new();
        let mut call = descriptor("labs", CanonicalFfiScalarType::F64);
        call.parameter_conversions[0] = crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
            to: MirAbiClass::Float { bits: 64 },
        };
        call.result_conversion = Some(crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Float { bits: 64 },
            to: MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
        });
        let argument = call.argument_ids[0].clone();
        let result = crate::core::mir::MirValueId::new("ffi-test-result").expect("result id");
        call.result_id = Some(result.clone());
        call.requires = Some(Expr::Binary {
            op: Op::GreaterEqual,
            left: Box::new(Expr::Value(argument)),
            right: Box::new(Expr::Int(0)),
        });
        call.ensures = Some(Expr::Binary {
            op: Op::GreaterEqual,
            left: Box::new(Expr::Value(result)),
            right: Box::new(Expr::Int(0)),
        });
        runtime
            .validate_descriptor(&call, &[Value::Int(1)], None, None)
            .expect("predicates must use MIR-side conversion endpoints");
    }

    #[test]
    fn scalar_ffi_runtime_rejects_unsupported_conversion_before_library_load() {
        use crate::core::mir::types::MirAbiClass;

        let mut runtime = CanonicalMirFfiRuntime::new();
        let mut argument_call = descriptor("labs", CanonicalFfiScalarType::I64);
        argument_call.parameter_conversions[0] = crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Bool,
            to: MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
        };
        let error = runtime
            .call(&argument_call, &[Value::Int(1)])
            .expect_err("unsupported argument conversion must fail at descriptor preflight");
        assert!(error
            .to_string()
            .contains("argument 0 ABI conversion from Bool to Integer"));
        assert!(runtime.loaded_libs.is_empty());

        let mut result_call = descriptor("labs", CanonicalFfiScalarType::I64);
        result_call.result_conversion = Some(crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
            to: MirAbiClass::Bool,
        });
        let error = runtime
            .call(&result_call, &[Value::Int(1)])
            .expect_err("unsupported result conversion must fail before the foreign call");
        assert!(
            error
                .to_string()
                .contains("result ABI conversion from Integer")
                && error.to_string().contains("to Bool"),
            "{error}"
        );
        assert!(runtime.loaded_libs.is_empty());
    }

    #[test]
    fn scalar_ffi_runtime_rejects_result_identity_alias_before_loading() {
        let mut runtime = CanonicalMirFfiRuntime::new();
        let mut call = descriptor("labs", CanonicalFfiScalarType::I64);
        call.result_id = Some(call.argument_ids[0].clone());
        let error = runtime
            .call(&call, &[Value::Int(1)])
            .expect_err("result identity must not alias an argument identity");
        assert!(error
            .to_string()
            .contains("result identity overlaps an argument identity"));
        assert!(runtime.loaded_libs.is_empty());
    }

    #[test]
    fn scalar_ffi_runtime_rejects_non_unit_missing_result_identity_before_loading() {
        let mut runtime = CanonicalMirFfiRuntime::new();
        let mut call = descriptor("labs", CanonicalFfiScalarType::I64);
        call.result_id = None;
        let error = runtime
            .call(&call, &[Value::Int(1)])
            .expect_err("non-Unit FFI results must carry their MIR result identity");
        assert!(error
            .to_string()
            .contains("non-Unit result has no result identity"));
        assert!(runtime.loaded_libs.is_empty());
    }

    #[test]
    fn scalar_ffi_runtime_accepts_unit_without_result_identity_or_conversion() {
        let runtime = CanonicalMirFfiRuntime::new();
        let mut call = descriptor("labs", CanonicalFfiScalarType::I64);
        call.result = CanonicalFfiScalarType::Unit;
        call.result_id = None;
        call.result_conversion = None;
        runtime
            .validate_descriptor(&call, &[Value::Int(1)], None, None)
            .expect("void FFI calls may omit a result identity and conversion receipt");
    }

    #[test]
    fn scalar_ffi_runtime_rejects_unit_conversion_without_result_identity() {
        let mut runtime = CanonicalMirFfiRuntime::new();
        let mut call = descriptor("labs", CanonicalFfiScalarType::I64);
        call.result = CanonicalFfiScalarType::Unit;
        call.result_id = None;
        let error = runtime
            .call(&call, &[Value::Int(1)])
            .expect_err("Unit conversion receipts must bind a Unit result identity");
        assert!(error
            .to_string()
            .contains("result conversion has no result identity"));
        assert!(runtime.loaded_libs.is_empty());
    }

    #[test]
    fn scalar_ffi_runtime_rejects_forged_manifest_safe_symbol_before_loading() {
        let mut runtime = CanonicalMirFfiRuntime::new();
        let mut call = descriptor("labs", CanonicalFfiScalarType::I64);
        // `abs` is a manifest-safe symbol, but the checker-owned callee
        // identity is still the declaration for `labs`.  A physical runtime
        // must not let a hand-built descriptor retarget the call merely by
        // changing the symbol spelling.
        call.symbol = "abs".into();
        let error = runtime
            .call(&call, &[Value::Int(1)])
            .expect_err("safe but mismatched symbol must fail identity preflight");
        assert!(
            error
                .to_string()
                .contains("symbol disagrees with canonical extern callee"),
            "{error}"
        );
        assert!(runtime.loaded_libs.is_empty());
    }

    #[test]
    fn scalar_ffi_runtime_rejects_forged_caller_with_vm_context_before_loading() {
        let mut runtime = CanonicalMirFfiRuntime::new();
        let call = descriptor("labs", CanonicalFfiScalarType::I64);
        let error = runtime
            .call_from_caller(&call, &[Value::Int(1)], "function:other")
            .expect_err("descriptor caller must match the executing bytecode frame");
        assert!(error
            .to_string()
            .contains("descriptor caller 'function:main' disagrees with bytecode caller"));
        assert!(runtime.loaded_libs.is_empty());
    }

    #[test]
    fn scalar_ffi_runtime_rejects_forged_instruction_with_vm_context_before_loading() {
        let mut runtime = CanonicalMirFfiRuntime::new();
        let call = descriptor("labs", CanonicalFfiScalarType::I64);
        let error = runtime
            .call_from_context(
                &call,
                &[Value::Int(1)],
                "function:main",
                "instruction:other",
            )
            .expect_err("descriptor instruction must match the executing bytecode call site");
        assert!(error.to_string().contains(
            "descriptor instruction 'ffi-test-call' disagrees with bytecode instruction"
        ));
        assert!(runtime.loaded_libs.is_empty());
    }

    #[test]
    fn scalar_ffi_runtime_rejects_wrong_argument_type_range_and_missing_symbol() {
        let _guard = crate::tests::FfiEnvLock::lock();
        let mut runtime = CanonicalMirFfiRuntime::new();
        for (symbol, scalar, value, expected) in [
            (
                "abs",
                CanonicalFfiScalarType::I32,
                Value::Int(i64::MAX),
                "outside i32",
            ),
            (
                "labs",
                CanonicalFfiScalarType::I64,
                Value::Bool(true),
                "expected i64",
            ),
            (
                "unused",
                CanonicalFfiScalarType::Bool,
                Value::Int(1),
                "expected bool",
            ),
            (
                "unused",
                CanonicalFfiScalarType::F64,
                Value::Int(1),
                "expected f64",
            ),
            (
                "mimi_canonical_ffi_missing_symbol",
                CanonicalFfiScalarType::I64,
                Value::Int(1),
                "failed to find canonical MIR FFI symbol",
            ),
        ] {
            let error = runtime
                .call(&descriptor(symbol, scalar), &[value])
                .expect_err("invalid arguments/symbol cannot execute");
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn scalar_ffi_runtime_validates_argument_conversion_before_predicates() {
        let mut runtime = CanonicalMirFfiRuntime::new();
        let mut call = descriptor("abs", CanonicalFfiScalarType::I32);
        call.requires = Some(crate::core::mir::MirContractExpr::Bool(false));
        let error = runtime
            .call(&call, &[Value::Int(i64::MAX)])
            .expect_err("invalid argument representation must precede a contract violation");
        assert!(error.to_string().contains("outside i32"), "{error}");
        assert!(runtime.loaded_libs.is_empty());
    }
}
