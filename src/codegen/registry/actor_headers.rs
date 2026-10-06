//! Canonical mailbox header slice for the retained native Actor adapter.
//! This consumes declaration identities and canonical type IDs, never an
//! ActorDef, FuncDef, Param, or reconstructed surface type.

use inkwell::types::BasicTypeEnum;
use inkwell::AddressSpace;

use crate::codegen::CodeGenerator;
use crate::core::{CheckedProgram, NodeId, PrimitiveType, ResolvedType, ResolvedTypeId};
use crate::error::{CompileError, MimiResult};

pub(in crate::codegen) struct CheckedActorMailboxHeader<'ctx> {
    pub method_id: i32,
    pub parameters: Vec<BasicTypeEnum<'ctx>>,
    pub result: BasicTypeEnum<'ctx>,
}

impl<'ctx> CodeGenerator<'ctx> {
    /// `None` is an unmigrated ABI shape, not a compile rejection. The
    /// existing header reader is retained until that shape earns its receipt.
    pub(in crate::codegen) fn checked_actor_mailbox_header(
        &self,
        program: &CheckedProgram,
        actor: &NodeId,
        method: &str,
    ) -> MimiResult<Option<CheckedActorMailboxHeader<'ctx>>> {
        let descriptor = match &self.checked_native_declarations {
            Some(catalog) => {
                let Some(declaration) = catalog.actors.get(actor) else {
                    return Err(CompileError::Unsupported(format!(
                        "checked native declarations have no Actor owner '{}'",
                        actor.0
                    )));
                };
                &declaration.descriptor
            }
            // Standalone codegen test harnesses do not install a checker-owned
            // declaration catalog. Keep their explicit legacy test fallback;
            // the production checked route always takes the catalog branch.
            None => {
                let Some(descriptor) = program.actor_descriptor(actor) else {
                    return Ok(None);
                };
                descriptor
            }
        };
        // Runs-Flow dispatch is a separate transition ABI and ownership edge.
        if descriptor.runs_flow.is_some() {
            return Ok(None);
        }
        descriptor.validate_against(program).map_err(|error| {
            CompileError::Unsupported(format!("invalid checked actor mailbox descriptor: {error}"))
        })?;
        let Some((ordinal, declaration)) = descriptor
            .methods
            .iter()
            .enumerate()
            .find(|(_, declaration)| declaration.name == method)
        else {
            return Err(CompileError::Unsupported(format!(
                "checked actor '{}' has no method '{method}'",
                actor.0,
            )));
        };
        if !declaration.signature.generic_parameters.is_empty() {
            return Ok(None);
        }
        let parameters = declaration
            .signature
            .parameters
            .iter()
            .skip(1)
            .map(|parameter| self.checked_mailbox_value_type(program, &parameter.ty, false))
            .collect::<Option<Vec<_>>>();
        let Some(parameters) = parameters else {
            return Ok(None);
        };
        let result = if declaration.has_explicit_result {
            let Some(result) =
                self.checked_mailbox_value_type(program, &declaration.signature.result, false)
            else {
                return Ok(None);
            };
            result
        } else {
            // Preserve the historical omitted-result annotation ABI.
            BasicTypeEnum::IntType(self.context.i64_type())
        };
        let method_id = i32::try_from(ordinal).map_err(|_| {
            CompileError::Unsupported("actor mailbox method count exceeds its i32 ABI".into())
        })?;
        Ok(Some(CheckedActorMailboxHeader {
            method_id,
            parameters,
            result,
        }))
    }

    /// Exact shared layouts only. Option/Result, closure, ownership wrappers
    /// and nominal tuple fields still need a canonical native ABI receipt.
    fn checked_mailbox_value_type(
        &self,
        program: &CheckedProgram,
        id: &ResolvedTypeId,
        product_field: bool,
    ) -> Option<BasicTypeEnum<'ctx>> {
        use PrimitiveType::*;
        match program.resolved_types().get(id)? {
            ResolvedType::Primitive(primitive) => Some(match primitive {
                I32 => self.context.i32_type().into(),
                I64 => self.context.i64_type().into(),
                Unit if !product_field => self.context.i64_type().into(),
                F32 => self.context.f32_type().into(),
                F64 => self.context.f64_type().into(),
                Bool => self.context.bool_type().into(),
                String => self
                    .context
                    .struct_type(
                        &[
                            self.context.ptr_type(AddressSpace::default()).into(),
                            self.context.i64_type().into(),
                        ],
                        false,
                    )
                    .into(),
                _ => return None,
            }),
            ResolvedType::DynamicAny { .. } => Some(self.context.i64_type().into()),
            ResolvedType::Tuple(elements) => {
                let fields = elements
                    .iter()
                    .map(|element| {
                        let field = self.checked_mailbox_value_type(program, element, true)?;
                        Some(crate::codegen::abi::layout::widen_product_field(
                            self.context,
                            field,
                        ))
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some(self.context.struct_type(&fields, false).into())
            }
            ResolvedType::Nominal {
                item, arguments, ..
            } => {
                match item.as_str() {
                    "builtin:type:List" => Some(self.list_struct_type().into()),
                    "builtin:type:Map" | "builtin:type:Set" | "builtin:type:Record" => {
                        Some(self.context.i64_type().into())
                    }
                    name if name.starts_with("actor:") && arguments.is_empty() => {
                        Some(self.context.ptr_type(AddressSpace::default()).into())
                    }
                    // A root nominal uses the already registered native layout
                    // (including alias/newtype and repr(C)); tuple's historic
                    // bare-name ABI differs, so it remains an explicit boundary.
                    name if !product_field && arguments.is_empty() => {
                        let name = name.strip_prefix("type:").unwrap_or(name);
                        self.type_llvm.get(name).copied()
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }
}
