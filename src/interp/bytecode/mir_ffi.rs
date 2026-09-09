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
        self.validate_descriptor(descriptor, args)
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
            .call_abi(descriptor, args)
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
    ) -> Result<(), String> {
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
        if descriptor.arguments.len() != args.len() {
            return Err(format!(
                "canonical MIR FFI symbol '{}' expects {} arguments, got {}",
                descriptor.symbol,
                descriptor.arguments.len(),
                args.len()
            ));
        }
        if descriptor.parameter_conversions.len() != descriptor.arguments.len() {
            return Err("canonical FFI parameter conversion receipt arity mismatch".into());
        }
        for (index, (scalar, conversion)) in descriptor
            .arguments
            .iter()
            .zip(&descriptor.parameter_conversions)
            .enumerate()
        {
            if scalar_abi_class(scalar) != conversion.to {
                return Err(format!(
                    "canonical FFI argument {index} conversion target {:?} disagrees with declaration ABI {:?}",
                    conversion.to,
                    scalar_abi_class(scalar)
                ));
            }
        }
        let result_conversion = descriptor
            .result_conversion
            .as_ref()
            .ok_or_else(|| "canonical FFI result has no conversion receipt".to_owned())?;
        if scalar_abi_class(&descriptor.result) != result_conversion.from {
            return Err(
                "canonical FFI result conversion source disagrees with declaration ABI".into(),
            );
        }
        if descriptor.arguments.contains(&CanonicalFfiScalarType::Unit) {
            return Err("unit is not a canonical scalar FFI argument".into());
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
        Ok(())
    }

    fn call_abi(
        &mut self,
        descriptor: &CanonicalFfiDescriptor,
        args: &[Value],
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
        let mut storage: Vec<Box<dyn Any>> = Vec::with_capacity(args.len());
        let mut ffi_args = Vec::with_capacity(args.len());
        for (value, scalar) in args.iter().zip(&descriptor.arguments) {
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

fn apply_result_conversion(
    value: Value,
    conversion: Option<&crate::core::mir::MirFfiAbiConversion>,
) -> Result<Value, String> {
    let Some(conversion) = conversion else {
        return Ok(value);
    };
    if conversion.from == conversion.to {
        return Ok(value);
    }
    use crate::core::mir::types::MirAbiClass;
    match (conversion.from, conversion.to, value) {
        (
            MirAbiClass::Integer {
                bits: from,
                signed: true,
            },
            MirAbiClass::Integer {
                bits: to,
                signed: true,
            },
            Value::Int(value),
        ) if from == 64 && to == 32 => Ok(Value::Int((value as i32) as i64)),
        (
            MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
            MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
            Value::Int(value),
        ) => Ok(Value::Int(value)),
        (
            MirAbiClass::Float { bits: 64 },
            MirAbiClass::Integer { bits, signed: true },
            Value::Float(value),
        ) => Ok(Value::Int(if bits == 32 {
            (value as i32) as i64
        } else {
            value as i64
        })),
        (from, to, value) => Err(format!(
            "canonical MIR FFI result conversion from {from:?} to {to:?} received {value:?}"
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
    use crate::core::mir::types::MirAbiClass;
    match scalar {
        CanonicalFfiScalarType::I32 => MirAbiClass::Integer {
            bits: 32,
            signed: true,
        },
        CanonicalFfiScalarType::I64 => MirAbiClass::Integer {
            bits: 64,
            signed: true,
        },
        CanonicalFfiScalarType::Bool => MirAbiClass::Bool,
        CanonicalFfiScalarType::F64 => MirAbiClass::Float { bits: 64 },
        CanonicalFfiScalarType::Unit => MirAbiClass::Unit,
    }
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
        CanonicalFfiDescriptor {
            caller: "function:main".into(),
            instruction: "ffi-test-call".into(),
            callee: format!("extern:{symbol}"),
            symbol: symbol.into(),
            abi: "C".into(),
            arguments: vec![argument.clone()],
            parameter_conversions: vec![crate::core::mir::MirFfiAbiConversion {
                from: abi,
                to: abi,
            }],
            result: argument,
            result_conversion: Some(crate::core::mir::MirFfiAbiConversion { from: abi, to: abi }),
            argument_ids: vec![crate::core::mir::MirValueId::new("ffi-test-arg").unwrap()],
            requires: None,
            result_id: None,
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
}
