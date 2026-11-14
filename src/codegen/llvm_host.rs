//! Native initialization wrappers for the LLVM 18 host-only build.
//!
//! llvm-sys 181.3.0's `disable-alltargets-init` skips all of `wrappers/target.c`,
//! including the native wrappers used internally by Inkwell 0.9's JIT engines.
//! These four wrappers implement the X86 case of LLVM 18.1.3's inline helpers:
//! <https://github.com/llvm/llvm-project/blob/llvmorg-18.1.3/llvm/include/llvm-c/Target.h>.
//! They call real LLVM exports, preserve the upstream 0-success/1-failure ABI,
//! and report failure on unsupported hosts. No all-target wrappers are supplied.
//!
//! Inkwell holds its target initialization lock before calling these wrappers;
//! calling `Target::initialize_x86` here would recursively acquire that lock.

#![allow(non_snake_case)]

use llvm_sys::prelude::LLVMBool;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use llvm_sys::target::{
    LLVMInitializeX86AsmParser, LLVMInitializeX86AsmPrinter, LLVMInitializeX86Disassembler,
    LLVMInitializeX86Target, LLVMInitializeX86TargetInfo, LLVMInitializeX86TargetMC,
};

/// Initialize the native target's information, target and machine-code layer.
///
/// # Safety
/// The caller must serialize target initialization, as Inkwell does with its
/// target lock. The linked LLVM library must match the LLVM 18 build profile.
#[no_mangle]
pub unsafe extern "C" fn LLVM_InitializeNativeTarget() -> LLVMBool {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        // SAFETY: The caller holds the target initialization lock. On X86 hosts
        // these real LLVM 18 exports match the native helper's upstream order
        // and accept no pointers or other externally supplied arguments.
        unsafe {
            LLVMInitializeX86TargetInfo();
            LLVMInitializeX86Target();
            LLVMInitializeX86TargetMC();
        }
        0
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        1
    }
}

/// Initialize the native assembly parser.
///
/// # Safety
/// The caller must serialize target initialization and link matching LLVM 18.
#[no_mangle]
pub unsafe extern "C" fn LLVM_InitializeNativeAsmParser() -> LLVMBool {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        // SAFETY: The caller holds the initialization lock and this real LLVM 18
        // X86 parser initializer takes no arguments, matching the native helper.
        unsafe { LLVMInitializeX86AsmParser() };
        0
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        1
    }
}

/// Initialize the native assembly printer.
///
/// # Safety
/// The caller must serialize target initialization and link matching LLVM 18.
#[no_mangle]
pub unsafe extern "C" fn LLVM_InitializeNativeAsmPrinter() -> LLVMBool {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        // SAFETY: The caller holds the initialization lock and this real LLVM 18
        // X86 printer initializer takes no arguments, matching the native helper.
        unsafe { LLVMInitializeX86AsmPrinter() };
        0
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        1
    }
}

/// Initialize the native disassembler.
///
/// # Safety
/// The caller must serialize target initialization and link matching LLVM 18.
#[no_mangle]
pub unsafe extern "C" fn LLVM_InitializeNativeDisassembler() -> LLVMBool {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        // SAFETY: The caller holds the initialization lock and this real LLVM 18
        // X86 disassembler initializer takes no arguments, matching the helper.
        unsafe { LLVMInitializeX86Disassembler() };
        0
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        1
    }
}

#[cfg(all(test, any(target_arch = "x86", target_arch = "x86_64")))]
mod tests {
    use inkwell::context::Context;
    use inkwell::targets::{InitializationConfig, Target, TargetMachine};
    use inkwell::OptimizationLevel;

    #[test]
    fn llvm18_host_native_initializers_register_the_actual_host_target() {
        let (mut major, mut minor, mut patch) = (0, 0, 0);
        // SAFETY: All three outputs point to live, uniquely borrowed u32 values.
        unsafe { llvm_sys::core::LLVMGetVersion(&mut major, &mut minor, &mut patch) };
        assert_eq!(major, 18, "the host profile must execute against LLVM 18");
        Target::initialize_native(&InitializationConfig::default())
            .expect("all four native wrappers must report actual initialization success");
        let triple = TargetMachine::get_default_triple();
        Target::from_triple(&triple).expect("native initialization must register the actual host");
    }

    #[test]
    fn llvm18_host_jit_executes_i32_arguments_and_returns_43() {
        let context = Context::create();
        let module = context.create_module("llvm18_host_jit_i32_abi");
        let builder = context.create_builder();
        let i32_type = context.i32_type();
        let function = module.add_function(
            "increment",
            i32_type.fn_type(&[i32_type.into()], false),
            None,
        );
        let entry = context.append_basic_block(function, "entry");
        builder.position_at_end(entry);
        let input = function
            .get_first_param()
            .expect("one i32 argument")
            .into_int_value();
        let output = builder
            .build_int_add(input, i32_type.const_int(1, false), "incremented")
            .expect("valid i32 addition");
        builder
            .build_return(Some(&output))
            .expect("valid i32 return");
        module
            .verify()
            .expect("the JIT fixture must contain valid LLVM IR");
        let engine = module
            .create_jit_execution_engine(OptimizationLevel::None)
            .expect("Inkwell's native initialization path must create a real JIT engine");
        // SAFETY: The generated function has exactly the C i32(i32) ABI.
        type Increment = unsafe extern "C" fn(i32) -> i32;
        // SAFETY: The verified IR function matches Increment, the engine remains
        // alive during the call, and its function body only adds one to 42.
        let increment = unsafe {
            engine
                .get_function::<Increment>("increment")
                .expect("JIT function exists")
        };
        crate::codegen::assert_execution_engine_linked(&engine, "calling the host increment JIT");
        // SAFETY: `increment` has the checked i32(i32) ABI.
        let actual = unsafe { increment.call(42) };
        assert_eq!(actual, 43);
    }

    #[test]
    fn llvm18_mcjit_reports_unresolved_external_before_native_call() {
        let context = Context::create();
        let module = context.create_module("llvm18_host_jit_missing_external");
        let builder = context.create_builder();
        let i32_type = context.i32_type();
        let missing = module.add_function(
            "mimi_mir_test_deliberately_unresolved_import_0_1_11",
            i32_type.fn_type(&[], false),
            None,
        );
        let main = module.add_function("main", i32_type.fn_type(&[], false), None);
        let entry = context.append_basic_block(main, "entry");
        builder.position_at_end(entry);
        let call = builder
            .build_call(missing, &[], "missing")
            .expect("valid external call");
        let value = call
            .try_as_basic_value()
            .basic()
            .expect("i32 external result");
        builder
            .build_return(Some(&value.into_int_value()))
            .expect("valid main return");
        module
            .verify()
            .expect("the unresolved-symbol fixture must still be valid LLVM IR");

        let engine = module
            .create_jit_execution_engine(OptimizationLevel::None)
            .expect("create MCJIT engine for unresolved-symbol probe");
        assert!(
            engine.get_function_address("main").is_ok(),
            "MCJIT can expose the entry address even when an external relocation fails"
        );
        let message = crate::codegen::check_execution_engine_linked(&engine)
            .expect_err("checked lookup must reject unresolved relocations before native call");
        assert!(
            message.contains("mimi_mir_test_deliberately_unresolved_import_0_1_11"),
            "the unresolved symbol must be identified: {message}"
        );
    }
}
