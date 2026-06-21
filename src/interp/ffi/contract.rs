use super::super::*;
use crate::ast::*;
use crate::ffi::{FfiContract, FfiArgContract, Errno};

impl<'a> Interpreter<'a> {
    /// F7: Validate extern ABI — checks callback contract validity and
    /// argument count.  Unsupported-type errors are handled separately by
    /// `unsupported_ffi_arg_error` with richer context.
    pub(in crate::interp) fn verify_extern_abi(
        &self,
        extern_func: &ExternFunc,
        contract: &FfiContract,
    ) -> Result<(), Errno> {
        for (i, arg_contract) in contract.args.iter().enumerate() {
            if let FfiArgContract::Callback { param_types, .. } = arg_contract {
                if param_types.is_empty() {
                    return Err(Errno::Generic(format!(
                        "FFI safety: callback parameter {} of '{}' has zero parameters",
                        i + 1,
                        extern_func.name
                    )));
                }
            }
        }
        if contract.args.len() != extern_func.params.len() {
            return Err(Errno::Generic(format!(
                "FFI safety: contract has {} args but extern '{}' declares {} params",
                contract.args.len(),
                extern_func.name,
                extern_func.params.len()
            )));
        }
        Ok(())
    }

    /// Stage 4: Check precondition (requires) before the C call.
    pub(in crate::interp) fn verify_ffi_requires(
        &mut self,
        extern_func: &ExternFunc,
        contract: &FfiContract,
    ) -> Result<(), Errno> {
        if let Some(requires_expr) = &contract.requires {
            let result = self.eval_expr(requires_expr);
            match result {
                Ok(Value::Bool(true)) => { /* precondition holds */ }
                Ok(Value::Bool(false)) => {
                    return Err(Errno::Generic(format!(
                        "FFI contract violation: precondition of '{}' failed",
                        extern_func.name
                    )));
                }
                Ok(other) => {
                    return Err(Errno::Generic(format!(
                        "FFI contract error: precondition of '{}' must evaluate to bool, got {}",
                        extern_func.name, other
                    )));
                }
                Err(e) => {
                    return Err(Errno::Generic(format!(
                        "FFI contract error: failed to evaluate precondition of '{}': {}",
                        extern_func.name, e
                    )));
                }
            }
        }
        Ok(())
    }

    /// Stage 4: Check postcondition (ensures) after the C call.
    /// Binds 'result' to the return value for ensures evaluation.
    pub(in crate::interp) fn verify_ffi_ensures(
        &mut self,
        extern_func: &ExternFunc,
        contract: &FfiContract,
        return_value: &Value,
    ) -> Result<(), Errno> {
        if let Some(ensures_expr) = &contract.ensures {
            self.push_scope();
            self.env.last_mut().ok_or_else(|| Errno::Generic("FFI call: no scope after push (impossible)".to_string()))?.insert("result".to_string(), return_value.clone());
            let eval_result = self.eval_expr(ensures_expr);
            self.pop_scope();
            match eval_result {
                Ok(Value::Bool(true)) => { /* postcondition holds */ }
                Ok(Value::Bool(false)) => {
                    return Err(Errno::Generic(format!(
                        "FFI contract violation: postcondition of '{}' failed",
                        extern_func.name
                    )));
                }
                Ok(other) => {
                    return Err(Errno::Generic(format!(
                        "FFI contract error: postcondition of '{}' must evaluate to bool, got {}",
                        extern_func.name, other
                    )));
                }
                Err(e) => {
                    return Err(Errno::Generic(format!(
                        "FFI contract error: failed to evaluate postcondition of '{}': {}",
                        extern_func.name, e
                    )));
                }
            }
        }
        Ok(())
    }
}
