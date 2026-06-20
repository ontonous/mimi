//! FFI boundary layer shared by the interpreter and codegen backends.

pub mod contract;
pub mod c_header;
pub mod py_bind;
pub mod runtime;
pub mod callback;
pub mod errno;

pub use contract::{FfiArgContract, FfiContract, FfiRetContract};
pub use runtime::{CAP_TABLE, SHARED_TABLE};
pub use callback::CALLBACK_TABLE;
pub use errno::Errno;

/// Map an ABI string (e.g., "C", "stdcall", "fastcall") to an LLVM calling
/// convention ID.
pub fn abi_to_llvm_call_conv(abi: &str) -> u32 {
    match abi {
        "C" | "cdecl" => 0,          // LLVM_CallingConv::C
        "stdcall" => 72,             // LLVM_CallingConv::X86_StdCall
        "fastcall" => 73,            // LLVM_CallingConv::X86_FastCall
        "thiscall" => 79,            // LLVM_CallingConv::X86_ThisCall
        "vectorcall" => 81,          // LLVM_CallingConv::X86_VectorCall
        "win64" => 77,               // LLVM_CallingConv::Win64
        "aarch64_vector" => 88,      // LLVM_CallingConv::AArch64_VectorCall
        "sysv64" => 99,              // LLVM_CallingConv::X86_64_SysV (x86-64 System V ABI)
        "arm_aapcs" => 97,           // LLVM_CallingConv::ARM_AAPCS
        "arm_aapcs_vfp" => 98,       // LLVM_CallingConv::ARM_AAPCS_VFP
        _ => 0,  // Default to C calling convention
    }
}
