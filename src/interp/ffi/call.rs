// SD-4: fork() isolation removed. Signal guard (signal_guard.rs) replaces it.
// fork() in multi-threaded processes is POSIX UB (locked mutexes in other
// threads stay locked in the child). Signal guards are in-process, thread-safe,
// and don't require child process creation.
use super::super::*;
use crate::ffi::FfiRetContract;
use libffi::middle::{Cif, CodePtr};
use std::ffi::c_void;

// ===================== FFI Call Methods =====================

impl<'a> Interpreter<'a> {
    /// Call a C function via libffi (raw, standalone — no self access).
    ///
    /// SAFETY: `cif` and `code_ptr` must describe a valid C function and ABI.
    /// `ffi_args` must be valid libffi arguments whose lifetimes exceed the call.
    pub(in crate::interp) unsafe fn call_ffi_raw(
        cif: &Cif,
        code_ptr: CodePtr,
        ffi_args: &[libffi::middle::Arg],
        ret_contract: &FfiRetContract,
    ) -> i64 {
        match ret_contract {
            FfiRetContract::Unit => {
                cif.call::<()>(code_ptr, ffi_args);
                0i64
            }
            FfiRetContract::Float => {
                let val: f64 = cif.call(code_ptr, ffi_args);
                val.to_bits() as i64
            }
            _ => cif.call::<i64>(code_ptr, ffi_args),
        }
    }

    /// Call a C function that returns a struct by value, writing into a
    /// caller-provided buffer. Uses the low-level `raw::ffi_call` API to
    /// supply a custom return-value buffer of the struct's size.
    pub(in crate::interp) unsafe fn call_ffi_raw_struct(
        cif: &Cif,
        code_ptr: CodePtr,
        ffi_args: &[libffi::middle::Arg],
        rvalue: *mut c_void,
    ) {
        // SAFETY: rvalue must be a valid, writable buffer of sufficient
        // size for the struct return type. cif.as_raw_ptr() provides a
        // valid CIF descriptor for libffi.
        // IP-C6: reject null code pointers before calling into libffi.
        if code_ptr.as_ptr().is_null() {
            return;
        }
        let fn_ptr = unsafe { *code_ptr.as_safe_fun() };
        // SAFETY: ffi_call is called with a valid CIF, function pointer, return
        // buffer, and argument array; all lifetimes exceed this call.
        unsafe {
            libffi::raw::ffi_call(
                cif.as_raw_ptr(),
                Some(fn_ptr),
                rvalue,
                ffi_args.as_ptr() as *mut *mut c_void,
            );
        }
    }

    /// Call a C function without crash protection via libffi.
    pub(in crate::interp) fn call_ffi_direct(
        &self,
        cif: &Cif,
        code_ptr: CodePtr,
        ffi_args: &[libffi::middle::Arg],
        ret_contract: &FfiRetContract,
    ) -> Result<i64, String> {
        // SAFETY: call_ffi_raw is an unsafe fn; its contract is satisfied by the
        // valid CIF, code pointer, and argument slice passed by call_extern.
        unsafe { Ok(Self::call_ffi_raw(cif, code_ptr, ffi_args, ret_contract)) }
    }

    /// Whether the return contract is a process-local pointer that cannot
    /// be passed from a forked child to the parent as a raw address (R-C4).
    /// SD-4: retained for documentation; fork isolation is removed.
    #[allow(dead_code)]
    fn ret_contract_is_process_local_ptr(ret_contract: &FfiRetContract) -> bool {
        matches!(
            ret_contract,
            FfiRetContract::String
                | FfiRetContract::StringOwned
                | FfiRetContract::Json
                | FfiRetContract::RawPtr(_)
                | FfiRetContract::RawPtrMut(_)
                | FfiRetContract::CShared(_)
                | FfiRetContract::CBorrow(_)
                | FfiRetContract::CBorrowMut(_)
        )
    }
}
