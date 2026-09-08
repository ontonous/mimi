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
            .map_err(|error| {
                use crate::core::mir::MirFfiContractError;
                use crate::interp::InterpError;
                match error {
                    MirFfiContractError::Invalid(message) => InterpError::new(message),
                    MirFfiContractError::Violation => {
                        InterpError::contract_violation("FFI precondition failed")
                    }
                    MirFfiContractError::Overflow => {
                        InterpError::integer_overflow("integer overflow in FFI precondition")
                    }
                    MirFfiContractError::DivisionByZero => InterpError::div_by_zero(),
                }
            })?;
        }

        self.call_abi(descriptor, args)
            .map_err(crate::interp::InterpError::new)
    }

    fn call_abi(
        &mut self,
        descriptor: &CanonicalFfiDescriptor,
        args: &[Value],
    ) -> Result<Value, String> {
        if descriptor.abi != "C" {
            return Err(format!(
                "canonical MIR FFI ABI '{}' is outside the C scalar island",
                descriptor.abi
            ));
        }
        if descriptor.symbol.trim().is_empty() || descriptor.symbol.contains('\0') {
            return Err("canonical MIR FFI symbol is empty or contains NUL".into());
        }
        if descriptor.arguments.len() != args.len() {
            return Err(format!(
                "canonical MIR FFI symbol '{}' expects {} arguments, got {}",
                descriptor.symbol,
                descriptor.arguments.len(),
                args.len()
            ));
        }
        // libffi rejects void argument types while preparing the CIF. Reject
        // them here, before library loading or CIF construction can occur.
        if descriptor.arguments.contains(&CanonicalFfiScalarType::Unit) {
            return Err("unit is not a canonical scalar FFI argument".into());
        }
        if descriptor.argument_ids.len() != args.len() {
            return Err("canonical FFI argument identity arity mismatch".into());
        }

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
        Ok(result)
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

    fn descriptor(symbol: &str, argument: CanonicalFfiScalarType) -> CanonicalFfiDescriptor {
        CanonicalFfiDescriptor {
            caller: "function:main".into(),
            instruction: "ffi-test-call".into(),
            callee: format!("extern:{symbol}"),
            symbol: symbol.into(),
            abi: "C".into(),
            arguments: vec![argument.clone()],
            result: argument,
            argument_ids: vec![crate::core::mir::MirValueId::new("ffi-test-arg").unwrap()],
            requires: None,
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
            ("labs\0other", "C", vec![Value::Int(1)], "contains NUL"),
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
