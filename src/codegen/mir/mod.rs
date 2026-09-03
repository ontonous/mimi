//! AST-free native consumer for the closed scalar/owned-String,
//! recursive-tuple, non-Copy-record, flat-record/flat-variant, scalar-List,
//! and local immutable-borrow
//! Canonical MIR slices.
//!
//! This module intentionally accepts only `MirProgram`.  It does not import
//! surface AST or `CheckedProgram`, and it never calls the legacy emitter.  A
//! small eligibility validator runs before LLVM declarations are created so a
//! shape is either covered by the scalar MIR contract or rejected explicitly.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use inkwell::basic_block::BasicBlock;
use inkwell::context::Context;
use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FunctionValue, PhiValue,
};
use inkwell::IntPredicate;

use crate::codegen::{call_try_basic_value, CodeGenerator};
use crate::core::ir::{ResolvedBinaryOp, ResolvedCallee, ResolvedLiteral, ResolvedUnaryOp};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{
    MirAbiClass, MirBuiltinContract, MirBuiltinKind, MirConversionKind, MirGlueContract,
    MirGlueKind, MirLayout, MirOwnership, MirTypeCatalog, MirTypeDesc, MirTypeKind,
};
use crate::core::mir::{
    MirAggregateKind, MirBlockId, MirFunction, MirInstructionKind, MirListOperation, MirProjection,
    MirSetOperation, MirSwitchArm, MirSwitchCase, MirTerminator, MirValueId,
};
use crate::diagnostic::Diagnostic;
use crate::span::Span;

mod abi;
mod eligibility;
mod validate;

use abi::{
    native_basic_type, native_copy_variant_payload_type, native_list_kind,
    native_non_copy_variant_payload_type, native_variant_abi, validate_native_non_copy_record_type,
    validate_native_product_type, validate_native_recursive_tuple_type, NativeVariantAbi,
};
pub use eligibility::validate_mir_native;
use eligibility::{instruction_kind, mir_symbol, native_symbol_fragment, NativeMirError};
use validate::NativeMirValidator;
mod aggregate;
mod calls;
mod control_flow;
mod ownership;
mod runtime_glue;
mod scalar;

struct NativeMirEmitter<'a, 'ctx> {
    generator: &'a mut CodeGenerator<'ctx>,
    program: &'a MirProgram,
    functions: BTreeMap<crate::core::NodeId, FunctionValue<'ctx>>,
}

impl<'a, 'ctx> NativeMirEmitter<'a, 'ctx> {
    fn new(generator: &'a mut CodeGenerator<'ctx>, program: &'a MirProgram) -> Self {
        Self {
            generator,
            program,
            functions: BTreeMap::new(),
        }
    }

    fn compile(mut self) -> Result<(), NativeMirError> {
        self.declare_canonical_runtime_helpers();
        self.declare_functions()?;
        let owners = self.program.functions().keys().cloned().collect::<Vec<_>>();
        for owner in owners {
            let function = self.program.functions().get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "function disappeared during native emission",
                )
            })?;
            let llvm_function = *self.functions.get(&owner).ok_or_else(|| {
                NativeMirError::new(owner.0.clone(), "LLVM function declaration is absent")
            })?;
            NativeMirFunctionEmitter::new(
                self.generator,
                self.program,
                &self.functions,
                function,
                llvm_function,
            )
            .emit()?;
        }
        self.generator
            .module
            .verify()
            .map_err(|error| NativeMirError::new("LLVM module", error.to_string()))?;
        Ok(())
    }

    /// Canonical-only runtime declarations belong to the MIR adapter, not to
    /// the legacy builtin registry.  Keeping this declaration local prevents
    /// an otherwise unrelated legacy LLVM golden from changing merely because
    /// a new MIR glue operation was added.
    fn declare_canonical_runtime_helpers(&self) {
        if self
            .generator
            .module
            .get_function("mimi_set_clone_scalar")
            .is_none()
        {
            let i64 = self.generator.context.i64_type();
            self.generator.module.add_function(
                "mimi_set_clone_scalar",
                i64.fn_type(&[BasicMetadataTypeEnum::IntType(i64)], false),
                Some(Linkage::External),
            );
        }

        if self
            .generator
            .module
            .get_function("mimi_mir_set_to_list_scalar")
            .is_none()
        {
            let i8 = self.generator.context.i8_type();
            let i64 = self.generator.context.i64_type();
            let ptr = self
                .generator
                .context
                .ptr_type(inkwell::AddressSpace::default());
            self.generator.module.add_function(
                "mimi_mir_set_to_list_scalar",
                ptr.fn_type(
                    &[
                        BasicMetadataTypeEnum::IntType(i64),
                        BasicMetadataTypeEnum::IntType(i8),
                    ],
                    false,
                ),
                Some(Linkage::External),
            );
        }

        if self
            .generator
            .module
            .get_function("mimi_mir_list_len_scalar")
            .is_none()
        {
            let i8 = self.generator.context.i8_type();
            let i32 = self.generator.context.i32_type();
            let ptr = self
                .generator
                .context
                .ptr_type(inkwell::AddressSpace::default());
            self.generator.module.add_function(
                "mimi_mir_list_len_scalar",
                i32.fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(ptr),
                        BasicMetadataTypeEnum::IntType(i8),
                    ],
                    false,
                ),
                Some(Linkage::External),
            );
        }
    }

    fn declare_functions(&mut self) -> Result<(), NativeMirError> {
        for (owner, function) in self.program.functions() {
            let symbol = mir_symbol(owner)
                .map_err(|message| NativeMirError::new(owner.0.clone(), message))?;
            let parameter_types = function
                .parameters
                .iter()
                .map(|parameter| {
                    let ty = function.values.get(parameter).ok_or_else(|| {
                        NativeMirError::new(owner.0.clone(), "parameter is absent from MIR values")
                    })?;
                    native_basic_type(self.generator.context, self.program.type_catalog(), &ty.ty)
                        .map(BasicMetadataTypeEnum::from)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let result_desc = self
                .program
                .type_catalog()
                .get(&function.result)
                .ok_or_else(|| NativeMirError::new(owner.0.clone(), "result TypeDesc is absent"))?;
            let function_type = if result_desc.abi == MirAbiClass::Unit {
                self.generator
                    .context
                    .void_type()
                    .fn_type(&parameter_types, false)
            } else {
                native_basic_type(
                    self.generator.context,
                    self.program.type_catalog(),
                    &function.result,
                )?
                .fn_type(&parameter_types, false)
            };
            let value =
                self.generator
                    .module
                    .add_function(&symbol, function_type, Some(Linkage::External));
            self.functions.insert(owner.clone(), value);
        }
        Ok(())
    }
}

struct NativeMirFunctionEmitter<'a, 'ctx> {
    generator: &'a mut CodeGenerator<'ctx>,
    program: &'a MirProgram,
    functions: &'a BTreeMap<crate::core::NodeId, FunctionValue<'ctx>>,
    function: &'a MirFunction,
    llvm_function: FunctionValue<'ctx>,
    blocks: BTreeMap<MirBlockId, BasicBlock<'ctx>>,
    values: HashMap<MirValueId, BasicValueEnum<'ctx>>,
    phis: HashMap<MirValueId, PhiValue<'ctx>>,
    pending_incoming: Vec<(MirValueId, NativePhiSource<'ctx>, BasicBlock<'ctx>)>,
}

enum NativePhiSource<'ctx> {
    Mir(MirValueId),
    Value(BasicValueEnum<'ctx>),
}

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    fn new(
        generator: &'a mut CodeGenerator<'ctx>,
        program: &'a MirProgram,
        functions: &'a BTreeMap<crate::core::NodeId, FunctionValue<'ctx>>,
        function: &'a MirFunction,
        llvm_function: FunctionValue<'ctx>,
    ) -> Self {
        Self {
            generator,
            program,
            functions,
            function,
            llvm_function,
            blocks: BTreeMap::new(),
            values: HashMap::new(),
            phis: HashMap::new(),
            pending_incoming: Vec::new(),
        }
    }

    fn emit(mut self) -> Result<(), NativeMirError> {
        self.create_blocks_and_parameters()?;
        let blocks = self.function.blocks.values().cloned().collect::<Vec<_>>();
        for block in &blocks {
            let llvm_block = *self
                .blocks
                .get(&block.id)
                .ok_or_else(|| NativeMirError::new(block.id.to_string(), "LLVM block is absent"))?;
            self.generator.builder.position_at_end(llvm_block);
            for instruction in &block.instructions {
                self.emit_instruction(&instruction.kind, instruction.id.as_str())?;
            }
            self.emit_terminator(&block.terminator, &block.id)?;
        }
        self.add_phi_incomings()?;
        Ok(())
    }

    fn create_blocks_and_parameters(&mut self) -> Result<(), NativeMirError> {
        for block in self.function.blocks.values() {
            let llvm_block = self
                .generator
                .context
                .append_basic_block(self.llvm_function, block.id.as_str());
            self.blocks.insert(block.id.clone(), llvm_block);
        }
        for (index, parameter) in self.function.parameters.iter().enumerate() {
            let value = self
                .llvm_function
                .get_nth_param(index as u32)
                .ok_or_else(|| {
                    NativeMirError::new(parameter.to_string(), "LLVM function parameter is absent")
                })?;
            self.values.insert(parameter.clone(), value);
        }
        for block in self.function.blocks.values() {
            let llvm_block = *self.blocks.get(&block.id).expect("created above");
            self.generator.builder.position_at_end(llvm_block);
            for parameter in &block.parameters {
                let info = self.function.values.get(&parameter.value).ok_or_else(|| {
                    NativeMirError::new(parameter.value.to_string(), "block parameter is absent")
                })?;
                let ty = native_basic_type(
                    self.generator.context,
                    self.program.type_catalog(),
                    &info.ty,
                )?;
                let phi = self
                    .generator
                    .builder
                    .build_phi(ty, parameter.value.as_str())
                    .map_err(|error| {
                        NativeMirError::new(parameter.value.to_string(), error.to_string())
                    })?;
                self.values
                    .insert(parameter.value.clone(), phi.as_basic_value());
                self.phis.insert(parameter.value.clone(), phi);
            }
        }
        Ok(())
    }

    fn emit_instruction(
        &mut self,
        instruction: &MirInstructionKind,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        match instruction {
            MirInstructionKind::Const { result, literal } => {
                let value = self.emit_const(result, literal, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Copy { result, source } => {
                let value = self.value(source, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Move { result, source } => {
                let value = self.value(source, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Clone { result, source } => {
                let source_ty = self.value_type(source, subject)?;
                let is_list = self
                    .program
                    .type_catalog()
                    .get(&source_ty)
                    .is_some_and(|desc| matches!(desc.layout, MirLayout::List { .. }));
                let is_set = self
                    .program
                    .type_catalog()
                    .get(&source_ty)
                    .is_some_and(|desc| matches!(desc.layout, MirLayout::Set { .. }));
                let value = if is_list {
                    self.emit_list_clone(source, subject)?
                } else if is_set {
                    self.emit_set_clone(source, subject)?
                } else {
                    self.emit_clone_value(self.value(source, subject)?, &source_ty, subject)?
                };
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Convert { result, source } => {
                let source_value = self.value(source, subject)?;
                let source_ty = self.value_type(source, subject)?;
                let result_ty = self.value_type(result, subject)?;
                let contract = self
                    .program
                    .type_catalog()
                    .validate_conversion(&source_ty, &result_ty)
                    .map_err(|message| NativeMirError::new(subject, message))?;
                let value = match contract.kind {
                    MirConversionKind::ScalarIdentity => source_value,
                    MirConversionKind::SignedI32ToI64 => {
                        let source = source_value.into_int_value();
                        self.generator
                            .builder
                            .build_int_s_extend(
                                source,
                                self.generator.context.i64_type(),
                                "mir_i32_to_i64",
                            )
                            .map(BasicValueEnum::from)
                            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                    }
                };
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Unary {
                result,
                op,
                operand,
            } => {
                let value = self.emit_unary(result, *op, operand, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Binary {
                result,
                op,
                left,
                right,
            } => {
                let value = self.emit_binary(result, *op, left, right, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Project {
                result,
                base,
                projection,
                list_index_contract,
            } => {
                let value = self.emit_project(
                    result,
                    base,
                    projection,
                    list_index_contract.as_ref(),
                    subject,
                )?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::MoveProject {
                result,
                base,
                projection,
            } => {
                let value = self.emit_move_project(result, base, projection, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Borrow {
                result,
                source,
                mutable,
            } => {
                let value = self.emit_borrow(result, source, *mutable, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::EndBorrow { borrow } => {
                self.emit_end_borrow(borrow, subject)?;
            }
            MirInstructionKind::Construct {
                result,
                kind,
                fields,
            } => {
                let value = self.emit_construct(result, kind, fields, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::UpdateRecord {
                result,
                base,
                kind,
                fields,
            } => {
                let value = self.emit_update_record(result, base, kind, fields, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::ConstructVariant {
                result,
                nominal,
                variant,
                fields,
            } => {
                let value =
                    self.emit_construct_variant(result, nominal, variant, fields, subject, false)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::ConstructVariantMove {
                result,
                nominal,
                variant,
                fields,
            } => {
                let value =
                    self.emit_construct_variant(result, nominal, variant, fields, subject, true)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::ConstructList { result, elements } => {
                let value = self.emit_list_construct(result, elements, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::ListOp {
                result,
                operation,
                list,
            } => {
                let value = self.emit_list_op(result, *operation, list, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::ConstructSet { result, elements } => {
                let value = self.emit_set_construct(result, elements, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::SetOp {
                result,
                operation,
                set,
                argument,
            } => {
                let value =
                    self.emit_set_op(result, *operation, set, argument.as_ref(), subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Drop { value } => {
                self.emit_drop(value, subject)?;
            }
            MirInstructionKind::BuiltinCall {
                result,
                kind,
                arguments,
            } => {
                let value = self.emit_builtin(result, *kind, arguments, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Call {
                result,
                callee,
                arguments,
                ..
            } => {
                self.emit_call(result.as_ref(), callee, arguments, subject)?;
            }
            MirInstructionKind::FlowTransition {
                result,
                transition,
                arguments,
            } => {
                self.emit_flow_transition(result, transition, arguments, subject)?;
            }
            MirInstructionKind::Nop => {}
            _ => {
                return Err(NativeMirError::new(
                    subject,
                    "unvalidated instruction reached native emitter",
                ))
            }
        }
        Ok(())
    }

    fn value(
        &self,
        id: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.values.get(id).copied().ok_or_else(|| {
            NativeMirError::new(
                subject,
                format!("value '{id}' is not available at native emission site"),
            )
        })
    }

    fn value_type(
        &self,
        id: &MirValueId,
        subject: &str,
    ) -> Result<crate::core::ResolvedTypeId, NativeMirError> {
        self.function
            .values
            .get(id)
            .map(|value| value.ty.clone())
            .ok_or_else(|| {
                NativeMirError::new(subject, format!("value '{id}' has no canonical type"))
            })
    }

    fn value_desc(
        &self,
        id: &MirValueId,
        subject: &str,
    ) -> Result<&crate::core::mir::types::MirTypeDesc, NativeMirError> {
        let ty = self.value_type(id, subject)?;
        self.program
            .type_catalog()
            .get(&ty)
            .ok_or_else(|| NativeMirError::new(subject, format!("value '{id}' has no TypeDesc")))
    }
}

#[cfg(test)]
mod tests {
    use super::CodeGenerator;
    use crate::core::mir::reference::{MirProgram, MirReferenceInterpreter, MirRuntimeValue};
    use crate::interp::bytecode::{compile_mir_program, BytecodeVM};
    use crate::interp::Value;
    use crate::lexer::Lexer;
    use crate::parser::Parser;
    use inkwell::context::Context;

    fn canonical_program(source: &str) -> MirProgram {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        MirProgram::from_checked_program(&checked).expect("canonical MIR")
    }

    #[test]
    fn silent_local_flow_transition_is_one_four_consumer_production_island() {
        let program = canonical_program(
            "flow Counter { state Zero { n: i32 } transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } } } func main() -> i32 { let c = Zero { n: 41 } let c2 = Counter::inc(c) c2.n }",
        );
        let transition = crate::core::NodeId("transition:Counter::inc::Zero".into());
        let contract = program
            .transitions()
            .get(&transition)
            .expect("implemented transition contract");
        assert_eq!(contract.targets.len(), 1);
        assert_eq!(
            contract.effect,
            crate::core::mir::MirTransitionEffect::SilentLocal
        );
        assert_eq!(contract.source, contract.parameters[0]);
        let state_desc = program
            .type_catalog()
            .get(&contract.source)
            .expect("Flow state TypeDesc");
        assert_eq!(
            state_desc.abi,
            crate::core::mir::types::MirAbiClass::Aggregate
        );
        assert_eq!(
            state_desc.ownership,
            crate::core::mir::types::MirOwnership::Linear
        );
        assert_eq!(
            state_desc.glue.move_out,
            crate::core::mir::types::MirGlueKind::Aggregate
        );
        assert_eq!(
            state_desc.glue.clone,
            crate::core::mir::types::MirGlueKind::Aggregate
        );
        assert_eq!(
            state_desc.glue.drop,
            crate::core::mir::types::MirGlueKind::Aggregate
        );
        assert!(state_desc.drop_plan.is_some());
        assert!(program.functions().contains_key(&transition));
        assert!(program.functions().values().any(|function| {
            function.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        crate::core::mir::MirInstructionKind::FlowTransition {
                            transition: ref owner,
                            ..
                        } if owner == &transition
                    )
                })
            })
        }));

        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Flow transition execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));

        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("Flow transition MIR bytecode"))
                .run_value()
                .expect("bytecode Flow transition execution");
        assert!(matches!(bytecode, Value::Int(42)));

        crate::verifier::validate_mir_capabilities(&program)
            .expect("verifier capability for Flow transition island");
        crate::verifier::verify_mir(&program, String::new())
            .expect("verifier consumes Flow transition MIR");

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_flow_transition_test");
        generator
            .compile_mir_native(&program)
            .expect("native Flow transition lowering");
        generator
            .module
            .verify()
            .expect("native Flow transition module verifies");
        assert!(generator.module.get_function("main").is_some());
        assert!(generator
            .module
            .get_function("__mimi_transition_Counter__inc__Zero")
            .is_some());
    }

    #[test]
    fn failing_flow_transition_is_rejected_before_any_backend() {
        let source =
            "flow F { state A { v: i32 } transition go(A) -> A fails string { return A { v: self.v + 1 } } } func main() -> i32 { 0 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("failing transition must remain outside the S8 island");
        let message = format!("{error:?}");
        assert!(
            message.contains("silent-local")
                || message.contains("FlowTransition")
                || message.contains("Lowering"),
            "missing fail-closed transition diagnostic: {message}"
        );
    }

    #[test]
    fn native_validator_rejects_before_llvm_declarations() {
        let program = canonical_program("func main() -> f64 { 1.0 }");
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_validator_test");

        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("unsupported MIR must fail before native emission");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("canonical MIR native backend rejected")
                && diagnostic.message.contains("ABI Float")
        }));
        assert!(
            generator.module.get_function("main").is_none(),
            "L2 requires validation before LLVM function declarations"
        );
    }

    #[test]
    fn native_validator_rejects_record_with_unsupported_child_before_llvm_declarations() {
        let program = canonical_program(
            "type Box { text: string, values: List<i32> }\nfunc main() -> i32 { let value = Box { text: \"x\", values: [1] }; drop(value); 0 }",
        );
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_record_validator_test");

        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("record with an unsupported child must fail closed in native slice");
        assert!(
            diagnostics.iter().any(|diagnostic| diagnostic
                .message
                .contains("outside the scalar/String/tuple ABI")),
            "missing unsupported-child rejection: {diagnostics:?}"
        );
        assert!(
            generator.module.get_function("main").is_none(),
            "non-Copy aggregate must be rejected before LLVM declarations"
        );
    }

    #[test]
    fn native_emitter_materializes_non_copy_option_string_clone_move_and_drop() {
        let program = canonical_program(
            "func make_some() -> Option<string> { Some(\"owned\") }\nfunc make_none() -> Option<string> { None }\nfunc main() -> i32 { let some = make_some(); let cloned = some; drop(cloned); drop(some); let none = make_none(); drop(none); 42 }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Option<string> execution");
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("Option<string> MIR bytecode"))
                .run_value()
                .expect("bytecode Option<string> execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(bytecode, Value::Int(42)));

        let make_some = program
            .functions()
            .get(&crate::core::NodeId("function:make_some".into()))
            .expect("make_some MIR");
        assert!(make_some.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::ConstructVariantMove { .. }
                )
            })
        }));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_option_string_test");
        generator
            .compile_mir_native(&program)
            .expect("Option<string> MIR should have a native glue contract");
        generator
            .module
            .verify()
            .expect("native Option<string> module verifies");
        assert!(generator.module.get_function("make_some").is_some());
        assert!(generator.module.get_function("make_none").is_some());
        assert!(generator.module.get_function("mimi_str_clone").is_some());
        assert!(generator.module.get_function("mimi_string_free").is_some());
    }

    #[test]
    fn native_emitter_consumes_shared_flat_copy_variant_contract() {
        for (source, expected, module_name) in [
            (
                include_str!("../../../tests/fixtures/mir_native_option_copy.mimi"),
                42,
                "mir_native_flat_copy_option_test",
            ),
            (
                include_str!("../../../tests/fixtures/mir_native_result_copy.mimi"),
                8,
                "mir_native_flat_copy_result_test",
            ),
        ] {
            let program = canonical_program(source);
            for ty in program
                .type_catalog()
                .iter()
                .filter_map(|(ty, descriptor)| {
                    matches!(
                        descriptor.layout,
                        crate::core::mir::types::MirLayout::Option { .. }
                            | crate::core::mir::types::MirLayout::Result { .. }
                    )
                    .then_some(ty.clone())
                })
            {
                program
                    .type_catalog()
                    .validate_flat_copy_variant(&ty)
                    .expect("shared flat Copy variant TypeDesc contract");
            }
            let owner = crate::core::NodeId("function:main".into());
            let reference = MirReferenceInterpreter::new(&program)
                .execute(&owner, &[])
                .expect("reference flat Copy variant execution");
            assert_eq!(reference, MirRuntimeValue::Int(expected));
            let bytecode = BytecodeVM::new(
                compile_mir_program(&program).expect("flat Copy variant MIR bytecode"),
            )
            .run_value()
            .expect("bytecode flat Copy variant execution");
            assert!(matches!(bytecode, Value::Int(value) if value == expected));

            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, module_name);
            generator
                .compile_mir_native(&program)
                .expect("native flat Copy variant lowering");
            generator
                .module
                .verify()
                .expect("native flat Copy variant module verifies");
            assert!(generator.module.get_function("main").is_some());
        }
    }

    #[test]
    fn native_emitter_materializes_option_string_switch_move_and_matches_oracles() {
        let program = canonical_program(
            "func consume_text(text: string) -> i32 { drop(text); 41 }\nfunc consume(value: Option<string>) -> i32 { match value { Some(text) => consume_text(text), None => 0 } }\nfunc discard(value: Option<string>) -> i32 { match value { Some(_) => 7, None => 8 } }\nfunc main() -> i32 { let first: Option<string> = Some(\"owned\"); let second: Option<string> = Some(\"discard\"); let a = consume(first); let b = discard(second); a + b }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference Option<string> switch-move execution");
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("Option<string> MIR bytecode"))
                .run_value()
                .expect("bytecode Option<string> switch-move execution");
        assert_eq!(reference, MirRuntimeValue::Int(48));
        assert!(matches!(bytecode, Value::Int(48)));
        let switch_move_count = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .filter(|block| {
                matches!(
                    block.terminator,
                    crate::core::mir::MirTerminator::SwitchMove { .. }
                )
            })
            .count();
        assert_eq!(switch_move_count, 2);

        let (option_string_ty, string_ty) = program
            .type_catalog()
            .iter()
            .find_map(|(id, descriptor)| match &descriptor.layout {
                crate::core::mir::types::MirLayout::Option { inner, .. }
                    if descriptor.ownership == crate::core::mir::types::MirOwnership::Move =>
                {
                    Some((id.clone(), inner.clone()))
                }
                _ => None,
            })
            .expect("canonical Option<string> TypeDesc");
        let (variant_abi, payload_ty) =
            super::native_variant_abi(program.type_catalog(), &option_string_ty, true)
                .expect("native variant ABI contract");
        assert_eq!(variant_abi.tag_field, 0);
        assert_eq!(variant_abi.payload_field, 1);
        assert_eq!(payload_ty, string_ty);
        let unsupported = super::native_variant_abi(program.type_catalog(), &string_ty, true)
            .expect_err("non-variant must not enter native variant ABI");
        assert!(unsupported
            .message
            .contains("native non-Copy Option<string> variant contract"));

        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_option_string_switch_move_test");
        generator
            .compile_mir_native(&program)
            .expect("Option<string> SwitchMove should have a native contract");
        generator
            .module
            .verify()
            .expect("native Option<string> SwitchMove module verifies");
        assert!(generator.module.get_function("consume").is_some());
        assert!(generator.module.get_function("discard").is_some());
    }

    #[test]
    fn native_validator_rejects_non_copy_variant_outside_promoted_contract_before_llvm_declarations(
    ) {
        let program = canonical_program(
            "func main() -> i32 { let value: Option<(string, i32)> = Some((\"owned\", 41)); drop(value); 42 }",
        );
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_option_string_rejected_test");

        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("nested variant payload must remain fail-closed");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic
                    .message
                    .contains("native non-Copy Option<string> variant contract")
            }),
            "missing promoted-contract rejection: {diagnostics:?}"
        );
        assert!(
            generator.module.get_function("main").is_none(),
            "unsupported variant must be rejected before LLVM declarations"
        );
    }

    #[test]
    fn native_validator_rejects_non_copy_switch_move_default_before_llvm_declarations() {
        let program = canonical_program(
            "func main() -> string { let value: Option<string> = Some(\"owned\"); match value { Some(text) => text, _ => \"fallback\" } }",
        );
        let owner = crate::core::NodeId("function:main".into());
        assert!(program.functions().get(&owner).is_some_and(|function| {
            function.blocks.values().any(|block| {
                matches!(
                    block.terminator,
                    crate::core::mir::MirTerminator::SwitchMove { .. }
                )
            })
        }));
        let context = Context::create();
        let mut generator =
            CodeGenerator::new(&context, "mir_native_option_string_switch_default_test");

        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("default consuming switch must remain fail-closed in native slice");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("requires explicit variant arms; default/literal cases are not covered")
        }));
        assert!(
            generator.module.get_function("main").is_none(),
            "unsupported consuming switch must be rejected before LLVM declarations"
        );
    }

    #[test]
    fn native_emitter_materializes_move_project_before_llvm_declarations() {
        let program = canonical_program(
            "type Named { name: string, count: i32 }\nfunc main() -> i32 { let value = Named { name: \"x\", count: 41 }; let name = value.name; drop(name); 42 }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference MoveProject execution");
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("MoveProject MIR bytecode"))
                .run_value()
                .expect("MIR bytecode MoveProject execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(bytecode, Value::Int(42)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_move_project_test");

        generator
            .compile_mir_native(&program)
            .expect("MoveProject should have a native transfer contract");
        assert!(
            generator.module.get_function("main").is_some(),
            "MoveProject native function should be declared"
        );
        assert!(generator.module.get_function("mimi_string_free").is_some());
        generator
            .module
            .verify()
            .expect("native MoveProject module verifies");
    }

    #[test]
    fn native_validator_rejects_non_copy_record_update_before_llvm_declarations() {
        let program = canonical_program(
            "type Box { text: string, count: i32 }\nfunc main() -> i32 { let value = Box { count: 1, text: \"x\" }; let updated = Box { count: 2, ..value }; drop(updated); 0 }",
        );
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_record_update_validator_test");

        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("non-Copy record update must fail closed in native slice");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("non-Copy record update requires an explicit transfer/update contract")
        }));
        assert!(
            generator.module.get_function("main").is_none(),
            "non-Copy record update must be rejected before LLVM declarations"
        );
    }

    #[test]
    fn native_record_update_matches_reference_and_mir_bytecode() {
        let source = "type Point { x: i32, enabled: bool }\nfunc main() -> i32 { let point = Point { enabled: true, x: 40 }; let updated = Point { x: 42, ..point }; if updated.enabled { updated.x } else { 0 } }";
        let program = canonical_program(source);
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference record update execution");
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("record update MIR bytecode"))
                .run_value()
                .expect("MIR bytecode record update execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(bytecode, Value::Int(42)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_record_update_test");
        generator
            .compile_mir_native(&program)
            .expect("flat Copy record update should have a native contract");
        assert!(generator.module.get_function("main").is_some());
        generator
            .module
            .verify()
            .expect("native record update module verifies");
        let update_count = program
            .functions()
            .get(&owner)
            .into_iter()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::UpdateRecord { .. }
                )
            })
            .count();
        assert_eq!(update_count, 1, "fixture must exercise UpdateRecord");
    }

    #[test]
    fn canonical_mir_rejects_non_scalar_list_before_any_backend() {
        let tokens = Lexer::new("func main() -> i32 { let values = [\"x\"]; drop(values); 0 }")
            .tokenize()
            .expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("non-scalar List must fail before any backend");
        let crate::core::mir::reference::MirProgramBuildError::Validation(errors) = error else {
            panic!("unexpected canonical List rejection: {error:?}");
        };
        assert!(errors
            .iter()
            .any(|error| error.message.contains("Copy scalar contract")));
    }

    #[test]
    fn native_emitter_accepts_scalar_list_projection_contract() {
        let program =
            canonical_program("func main() -> i32 { let values = [10, 20, 30]; values[1] }");
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_list_emitter_test");

        generator
            .compile_mir_native(&program)
            .expect("scalar List MIR should have a native contract");
        assert!(generator.module.get_function("main").is_some());
        assert!(generator
            .module
            .get_function("mimi_mir_list_get_scalar")
            .is_some());
        generator
            .module
            .verify()
            .expect("native List module verifies");
    }

    #[test]
    fn native_emitter_materializes_scalar_list_len_adapter() {
        let program = canonical_program(
            "func main() -> i32 { let values = [10, 20, 30]; let count = len(values); drop(values); count }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let function = program.functions().get(&owner).expect("List.len main MIR");
        assert!(function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::ListOp {
                        operation: crate::core::mir::MirListOperation::Len,
                        ..
                    }
                )
            })
        }));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_scalar_list_len_test");
        generator
            .compile_mir_native(&program)
            .expect("scalar List.len MIR should have a canonical ABI adapter");
        generator
            .module
            .verify()
            .expect("native List.len module verifies");
        assert!(generator.module.get_function("main").is_some());
        assert!(generator
            .module
            .get_function("mimi_mir_list_len_scalar")
            .is_some());
    }

    #[test]
    fn native_emitter_materializes_owned_string_clone_move_and_drop() {
        let program = canonical_program(
            "func make_text() -> string { \"canonical\" }\nfunc consume_text(text: string) -> i32 { drop(text); 41 }\nfunc main() -> i32 { let text = make_text(); let cloned = text; drop(cloned); consume_text(text) }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference owned string execution");
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("owned string MIR bytecode"))
                .run_value()
                .expect("MIR bytecode owned string execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        assert!(matches!(bytecode, Value::Int(41)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_owned_string_test");
        generator
            .compile_mir_native(&program)
            .expect("owned String MIR should have a native glue contract");
        generator
            .module
            .verify()
            .expect("native owned String module verifies");
        assert!(generator.module.get_function("make_text").is_some());
        assert!(generator.module.get_function("consume_text").is_some());
        assert!(generator.module.get_function("mimi_str_clone").is_some());
        assert!(generator.module.get_function("mimi_string_free").is_some());
    }

    #[test]
    fn native_emitter_materializes_recursive_owned_tuple_clone_move_and_drop() {
        let program = canonical_program(
            "func make_nested() -> ((string, i32), bool) { ((\"inner\", 41), true) }\nfunc consume_nested(value: ((string, i32), bool)) -> i32 { drop(value); 42 }\nfunc main() -> i32 { let value = make_nested(); let cloned = value; drop(cloned); consume_nested(value) }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference recursive tuple execution");
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("recursive tuple MIR bytecode"))
                .run_value()
                .expect("MIR bytecode recursive tuple execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(bytecode, Value::Int(42)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_recursive_tuple_test");
        generator
            .compile_mir_native(&program)
            .expect("recursive tuple MIR should have a native glue contract");
        generator
            .module
            .verify()
            .expect("native recursive tuple module verifies");
        assert!(generator.module.get_function("make_nested").is_some());
        assert!(generator.module.get_function("consume_nested").is_some());
        assert!(generator.module.get_function("mimi_str_clone").is_some());
        assert!(generator.module.get_function("mimi_string_free").is_some());
    }

    #[test]
    fn native_emitter_materializes_non_copy_record_clone_move_and_drop() {
        let program = canonical_program(
            "type Named { name: string, count: i32 }\nfunc make_named() -> Named { Named { count: 41, name: \"owned\" } }\nfunc consume_named(value: Named) -> i32 { drop(value); 42 }\nfunc main() -> i32 { let value = make_named(); let cloned = value; drop(cloned); consume_named(value) }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference non-Copy record execution");
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("non-Copy record MIR bytecode"))
                .run_value()
                .expect("MIR bytecode non-Copy record execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(bytecode, Value::Int(42)));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_non_copy_record_test");
        generator
            .compile_mir_native(&program)
            .expect("non-Copy record MIR should have a native glue contract");
        generator
            .module
            .verify()
            .expect("native non-Copy record module verifies");
        assert!(generator.module.get_function("make_named").is_some());
        assert!(generator.module.get_function("consume_named").is_some());
        assert!(generator.module.get_function("mimi_str_clone").is_some());
        assert!(generator.module.get_function("mimi_string_free").is_some());
    }

    #[test]
    fn native_emitter_materializes_scalar_set_handle_island() {
        let program = canonical_program(
            "func make_values() -> Set<i32> { let values: Set<i32> = {1, 2, 1}; values }\nfunc main() -> i32 { let values = make_values(); let inserted = values.insert(3); let present = inserted.contains(2); let nonempty = !inserted.is_empty(); let removed = inserted.remove(1); let size = removed.size(); let _present = present; let _nonempty = nonempty; size }",
        );
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference scalar Set execution");
        let bytecode =
            BytecodeVM::new(compile_mir_program(&program).expect("scalar Set MIR bytecode"))
                .run_value()
                .expect("bytecode scalar Set execution");
        assert_eq!(reference, MirRuntimeValue::Int(2));
        assert!(matches!(bytecode, Value::Int(2)));

        let set_ops = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::ConstructSet { .. }
                        | crate::core::mir::MirInstructionKind::SetOp { .. }
                )
            })
            .count();
        assert!(
            set_ops >= 5,
            "fixture must exercise construct and Set operations"
        );

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_scalar_set_test");
        generator
            .compile_mir_native(&program)
            .expect("scalar Set MIR should have a native handle contract");
        generator
            .module
            .verify()
            .expect("native scalar Set module verifies");
        for runtime in [
            "mimi_set_new",
            "mimi_set_clone_scalar",
            "mimi_set_insert",
            "mimi_set_remove",
            "mimi_set_size",
            "mimi_set_contains",
            "mimi_set_destroy",
        ] {
            assert!(
                generator.module.get_function(runtime).is_some(),
                "native Set island must declare {runtime}"
            );
        }
    }

    #[test]
    fn native_emitter_materializes_scalar_set_to_list_adapter() {
        let program = canonical_program(
            "func main() -> List<i32> { let values: Set<i32> = {3, 1, 2, 1}; values.to_list() }",
        );
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_scalar_set_to_list_test");
        assert!(
            generator
                .module
                .get_function("mimi_mir_set_to_list_scalar")
                .is_none(),
            "canonical Set.to_list adapter must not leak into legacy runtime registration"
        );
        generator
            .compile_mir_native(&program)
            .expect("scalar Set.to_list MIR should have a canonical ABI adapter");
        generator
            .module
            .verify()
            .expect("native scalar Set.to_list module verifies");
        assert!(generator
            .module
            .get_function("mimi_mir_set_to_list_scalar")
            .is_some());
        assert!(generator
            .module
            .get_function("mimi_mir_list_drop_scalar")
            .is_some());
    }

    #[test]
    fn native_emitter_consumes_materialized_scalar_generic_identity() {
        let program = canonical_program(
            "func identity<T>(value: T) -> T { value }\nfunc main() -> i32 { identity(41) }",
        );
        assert_eq!(program.instances().len(), 1);
        let reference = MirReferenceInterpreter::new(&program)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference generic identity execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));

        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_generic_identity_test");
        generator
            .compile_mir_native(&program)
            .expect("native backend must consume the specialized MIR target");
        generator
            .module
            .verify()
            .expect("native generic identity module verifies");
    }

    #[test]
    fn native_emitter_consumes_tuple_projection_receipt() {
        let program = canonical_program("func main() -> i32 { let pair = (40, 2); pair.0 }");
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_tuple_projection_test");
        generator
            .compile_mir_native(&program)
            .expect("native tuple projection should use the canonical receipt");
        generator
            .module
            .verify()
            .expect("native tuple projection module verifies");
    }

    #[test]
    fn native_validator_rejects_tuple_with_unsupported_child_before_llvm_declarations() {
        let program =
            canonical_program("func main() -> i32 { let value = (\"x\", [1]); drop(value); 0 }");
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_tuple_validator_test");

        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("tuple with List child must fail closed in native tuple slice");
        assert!(diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("outside the scalar/String/tuple ABI")));
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("recursive tuple contract")));
        assert!(
            generator.module.get_function("main").is_none(),
            "L2 requires unsupported tuple ABI to be rejected before LLVM declarations"
        );
    }

    #[test]
    fn native_validator_rejects_mixed_variant_payload_before_llvm_declarations() {
        let program = canonical_program(
            "func main() -> i64 { let value: Result<i64, bool> = Ok(41); match value { Ok(v) => v, Err(_) => (0 as i64) } }",
        );
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_variant_validator_test");

        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("mixed variant payloads must fail before native emission");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.message.contains("flat Copy variant contract")
                && diagnostic.message.contains("mixed payload ABI")
        }));
        assert!(
            generator.module.get_function("main").is_none(),
            "L2 requires variant validation before LLVM function declarations"
        );
    }
}
