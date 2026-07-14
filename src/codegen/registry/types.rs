use crate::ast::*;
use crate::codegen::types;
use crate::codegen::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::{BasicType, BasicTypeEnum, StructType};
use inkwell::values::BasicMetadataValueEnum;
use inkwell::values::BasicValueEnum;

/// P0-2: how a variant's payload maps onto the constructor function's
/// parameter shape. The uniform runtime representation is `{i32 tag, i64
/// payload}` so anything that doesn't fit in a single primitive must be
/// packed into a heap-allocated struct and ptrtoint-encoded into the
/// i64 slot (matching the existing single-struct payload encoding).
#[derive(Debug, Clone, Copy)]
enum PayloadKind<'ctx> {
    /// Variant carries no payload (e.g. `None`).
    None,
    /// Variant carries exactly one payload and it maps to a single
    /// non-struct LLVM primitive (e.g. `Circle(f64)` → f64).
    Single(BasicTypeEnum<'ctx>),
    /// Variant carries a struct payload OR a multi-arg tuple; the args
    /// are packed into an LLVM struct that the constructor takes by
    /// value and stores on the heap.
    Packed(StructType<'ctx>),
}

impl<'ctx> CodeGenerator<'ctx> {
    /// Classify a variant payload into the constructor parameter shape
    /// described by [`PayloadKind`]. Multi-arg tuples and struct
    /// payloads share the `Packed` branch so the existing
    /// `decode_payload_struct` match-side path keeps working.
    fn classify_variant_payload(&self, payload: &Option<VariantPayload>) -> PayloadKind<'ctx> {
        let Some(payload) = payload else {
            return PayloadKind::None;
        };
        match payload {
            VariantPayload::Tuple(types) if types.len() == 1 => {
                match self.llvm_type_for(&types[0]) {
                    Some(BasicTypeEnum::StructType(st)) => PayloadKind::Packed(st),
                    Some(ty) => PayloadKind::Single(ty),
                    None => {
                        // Unknown payload type: fall back to packed (i64, i64)
                        // so the caller still gets a defined function shape.
                        let i64_ty = BasicTypeEnum::IntType(self.context.i64_type());
                        PayloadKind::Packed(self.context.struct_type(&[i64_ty, i64_ty], false))
                    }
                }
            }
            VariantPayload::Tuple(types) => {
                // Multi-arg tuple: pack each arg's LLVM type into a struct.
                let mut field_tys: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(types.len());
                let mut all_known = true;
                for t in types {
                    if let Some(ty) = self.llvm_type_for(t) {
                        field_tys.push(ty);
                    } else {
                        all_known = false;
                        break;
                    }
                }
                if all_known && !field_tys.is_empty() {
                    PayloadKind::Packed(self.context.struct_type(&field_tys, false))
                } else {
                    let i64_ty = BasicTypeEnum::IntType(self.context.i64_type());
                    PayloadKind::Packed(self.context.struct_type(&[i64_ty, i64_ty], false))
                }
            }
            VariantPayload::Record(fields) => {
                let mut field_tys: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(fields.len());
                let mut all_known = true;
                for f in fields {
                    if let Some(ty) = self.llvm_type_for(&f.ty) {
                        field_tys.push(ty);
                    } else {
                        all_known = false;
                        break;
                    }
                }
                if all_known && !field_tys.is_empty() {
                    PayloadKind::Packed(self.context.struct_type(&field_tys, false))
                } else {
                    let i64_ty = BasicTypeEnum::IntType(self.context.i64_type());
                    PayloadKind::Packed(self.context.struct_type(&[i64_ty, i64_ty], false))
                }
            }
        }
    }
}

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn register_type_def(
        &mut self,
        t: &crate::ast::TypeDef,
    ) -> MimiResult<()> {
        let llvm_ty = match &t.kind {
            crate::ast::TypeDefKind::Record(fields) => {
                let mut field_tys = Vec::new();
                for f in fields {
                    // CG-C4: Use extern type mapping for #[repr(C)] record fields
                    // so i32 maps to LLVM i32 (4 bytes) instead of i64 (8 bytes),
                    // matching the C ABI struct layout.
                    let ty = if t.attributes.contains(&TypeAttribute::ReprC) {
                        crate::codegen::types::mimi_type_to_llvm_extern(self.context, &f.ty)
                    } else {
                        self.llvm_type_for(&f.ty)
                    }
                    .ok_or_else(|| {
                        CompileError::LlvmError(format!(
                            "cannot map record field '{}' type to LLVM",
                            crate::core::fmt_type(&f.ty)
                        ))
                    })?;
                    field_tys.push(ty);
                }
                BasicTypeEnum::StructType(self.context.struct_type(&field_tys, false))
            }
            crate::ast::TypeDefKind::Enum(variants) => {
                // Sort variants by name so ordinals are deterministic (not dependent on
                // declaration order). Matches the interp's string-based tag comparison.
                let mut sorted_variants: Vec<&crate::ast::Variant> = variants.iter().collect();
                sorted_variants.sort_by_key(|v| &v.name);
                if t.attributes.contains(&TypeAttribute::ReprC) {
                    // #[repr(C)] enums are plain i32 (matching C int / enum)
                    let enum_ty = BasicTypeEnum::IntType(self.context.i32_type());
                    // Register constructor functions for each variant
                    for (ordinal, v) in sorted_variants.iter().enumerate() {
                        let ctor_name = format!("{}_{}", t.name, v.name);
                        if self.module.get_function(&ctor_name).is_none() {
                            let fn_type = self.context.i32_type().fn_type(&[], false);
                            let ctor = self.module.add_function(
                                &ctor_name,
                                fn_type,
                                Some(inkwell::module::Linkage::Internal),
                            );
                            let entry = self.context.append_basic_block(ctor, "entry");
                            let prev_block = self.builder.get_insert_block();
                            self.builder.position_at_end(entry);
                            self.builder
                                .build_return(Some(
                                    &self.context.i32_type().const_int(ordinal as u64, false),
                                ))
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("ctor return error: {}", e))
                                })?;
                            if let Some(prev) = prev_block {
                                self.builder.position_at_end(prev);
                            }
                        }
                    }
                    enum_ty
                } else {
                    // Internal enum representation: i32 tag + i64 payload (uniform)
                    // Struct-typed payloads are ptrtoint-encoded into the i64 slot.
                    let payload_ty = BasicTypeEnum::IntType(self.context.i64_type());
                    let tag_ty = BasicTypeEnum::IntType(self.context.i32_type());
                    let enum_ty = BasicTypeEnum::StructType(
                        self.context.struct_type(&[tag_ty, payload_ty], false),
                    );
                    // Register the LLVM type BEFORE constructor generation so that
                    // classify_variant_payload for recursive variants (e.g. Add(Expr, Expr))
                    // resolves Expr → {i32, i64} instead of falling back to i64.
                    // Without this, the constructor stores Packed({i64, i64}) but match
                    // side decodes Packed({{i32, i64}, {i32, i64}}) → buffer over-read → SIGSEGV.
                    self.type_llvm.insert(t.name.clone(), enum_ty);
                    // Register constructor functions for each variant
                    let struct_ty = self.context.struct_type(
                        &[BasicTypeEnum::IntType(self.context.i32_type()), payload_ty],
                        false,
                    );
                    for (ordinal, v) in sorted_variants.iter().enumerate() {
                        let ctor_name = format!("{}_{}", t.name, v.name);
                        if self.module.get_function(&ctor_name).is_none() {
                            // P0-2: classify the variant payload so the
                            // constructor function is declared with the right
                            // parameter shape. Three cases:
                            //   - no payload: () -> {i32 tag, i64 payload}
                            //   - single primitive (non-struct): (T) -> ...
                            //     with the natural LLVM type T (f64, i64, ...);
                            //     the body bitcasts T -> i64 before storing.
                            //   - struct payload OR multi-arg tuple: the args
                            //     are packed into an LLVM struct, the ctor takes
                            //     that struct by value, mallocs+stores+ptrtoint
                            //     into the i64 payload slot (matches the
                            //     existing single-struct payload encoding, so
                            //     decode_payload_struct on the match side
                            //     continues to work without changes).
                            let payload_kind = self.classify_variant_payload(&v.payload);
                            let fn_type = match &payload_kind {
                                PayloadKind::None => struct_ty.fn_type(&[], false),
                                PayloadKind::Single(ty) => {
                                    let meta = types::basic_to_metadata(self.context, *ty);
                                    struct_ty.fn_type(&[meta], false)
                                }
                                PayloadKind::Packed(packed_ty) => {
                                    let meta = types::basic_to_metadata(
                                        self.context,
                                        BasicTypeEnum::StructType(*packed_ty),
                                    );
                                    struct_ty.fn_type(&[meta], false)
                                }
                            };
                            let ctor = self.module.add_function(
                                &ctor_name,
                                fn_type,
                                Some(inkwell::module::Linkage::Internal),
                            );
                            let entry = self.context.append_basic_block(ctor, "entry");
                            let prev_block = self.builder.get_insert_block();
                            self.builder.position_at_end(entry);
                            let alloca =
                                self.builder
                                    .build_alloca(struct_ty, &ctor_name)
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("alloca error: {}", e))
                                    })?;
                            let tag_gep = self
                                .gep()
                                .build_struct_gep(struct_ty, alloca, 0, "tag")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("gep error: {}", e))
                                })?;
                            self.builder
                                .build_store(
                                    tag_gep,
                                    self.context.i32_type().const_int(ordinal as u64, false),
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("store error: {}", e))
                                })?;
                            let payload_gep = self
                                .gep()
                                .build_struct_gep(struct_ty, alloca, 1, "payload")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("gep error: {}", e))
                                })?;
                            match &payload_kind {
                                PayloadKind::None => {}
                                PayloadKind::Single(ty) => {
                                    // P0-2: bitcast the natural type (e.g. f64)
                                    // to i64 so it fits in the uniform payload
                                    // slot. The match side reads it as i64 and
                                    // bitcasts back when binding to f64.
                                    let payload_arg = ctor.get_nth_param(0).ok_or_else(|| {
                                        CompileError::LlvmError("missing payload param".to_string())
                                    })?;
                                    let i64_type = self.context.i64_type();
                                    let i64_payload = if *ty == BasicTypeEnum::IntType(i64_type) {
                                        payload_arg
                                    } else if let BasicValueEnum::IntValue(iv) = payload_arg {
                                        // Integer narrower than i64: s_extend to i64
                                        // (preserves sign for negative i32 values).
                                        if iv.get_type().get_bit_width() < 64 {
                                            self.builder
                                                .build_int_s_extend(iv, i64_type, "payload_sext")
                                                .map_err(|e| {
                                                    CompileError::LlvmError(format!(
                                                        "s_ext payload: {}",
                                                        e
                                                    ))
                                                })?
                                                .into()
                                        } else {
                                            payload_arg
                                        }
                                    } else {
                                        // Float or pointer: bitcast is valid for same-width types
                                        self.builder
                                            .build_bit_cast(
                                                payload_arg,
                                                BasicTypeEnum::IntType(i64_type),
                                                "payload_bc",
                                            )
                                            .map_err(|e| {
                                                CompileError::LlvmError(format!(
                                                    "bitcast payload: {}",
                                                    e
                                                ))
                                            })?
                                    };
                                    self.builder.build_store(payload_gep, i64_payload).map_err(
                                        |e| {
                                            CompileError::LlvmError(format!("store payload: {}", e))
                                        },
                                    )?;
                                }
                                PayloadKind::Packed(packed_ty) => {
                                    let payload_arg = ctor.get_nth_param(0).ok_or_else(|| {
                                        CompileError::LlvmError(
                                            "missing packed payload param".to_string(),
                                        )
                                    })?;
                                    let malloc_fn = self
                                        .module
                                        .get_function("malloc")
                                        .ok_or_else(|| "malloc not declared".to_string())?;
                                    // Use size_of() on the StructType directly (not through
                                    // BasicTypeEnum, which may not expose size_of for structs).
                                    let payload_size = packed_ty
                                        .size_of()
                                        .and_then(|sv| sv.get_zero_extended_constant())
                                        .unwrap_or(32);
                                    let size_val = self
                                        .context
                                        .i64_type()
                                        .const_int(std::cmp::max(payload_size, 1), false);
                                    let malloc_call = self
                                        .builder
                                        .build_call(
                                            malloc_fn,
                                            &[BasicMetadataValueEnum::IntValue(size_val)],
                                            "payload_malloc",
                                        )
                                        .map_err(|e| {
                                            CompileError::LlvmError(format!("malloc error: {}", e))
                                        })?;
                                    let malloc_result =
                                        crate::codegen::call_try_basic_value(&malloc_call)
                                            .ok_or("malloc returned void")?
                                            .into_pointer_value();
                                    let typed_ptr = self
                                        .builder
                                        .build_pointer_cast(
                                            malloc_result,
                                            self.context.ptr_type(inkwell::AddressSpace::default()),
                                            "typed_ptr",
                                        )
                                        .map_err(|e| {
                                            CompileError::LlvmError(format!("ptr cast: {}", e))
                                        })?;
                                    self.builder.build_store(typed_ptr, payload_arg).map_err(
                                        |e| CompileError::LlvmError(format!("store packed: {}", e)),
                                    )?;
                                    let i8_ptr =
                                        self.context.ptr_type(inkwell::AddressSpace::default());
                                    let ptr_to_i8 = self
                                        .builder
                                        .build_pointer_cast(malloc_result, i8_ptr, "ptr_i8")
                                        .map_err(|e| {
                                            CompileError::LlvmError(format!("ptr cast: {}", e))
                                        })?;
                                    let int_val = self
                                        .builder
                                        .build_ptr_to_int(
                                            ptr_to_i8,
                                            self.context.i64_type(),
                                            "payload_int",
                                        )
                                        .map_err(|e| {
                                            CompileError::LlvmError(format!("ptr2int: {}", e))
                                        })?;
                                    self.builder.build_store(payload_gep, int_val).map_err(
                                        |e| CompileError::LlvmError(format!("store int: {}", e)),
                                    )?;
                                }
                            }
                            let loaded = self
                                .builder
                                .build_load(struct_ty, alloca, &ctor_name)
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("load error: {}", e))
                                })?;
                            self.builder.build_return(Some(&loaded)).map_err(|e| {
                                CompileError::LlvmError(format!("return error: {}", e))
                            })?;
                            if let Some(prev) = prev_block {
                                self.builder.position_at_end(prev);
                            }
                        }
                    }
                    enum_ty
                }
            }
            crate::ast::TypeDefKind::Alias(ty) => types::mimi_type_to_llvm(self.context, ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type())),
            crate::ast::TypeDefKind::Newtype(inner_ty) => {
                let llvm_ty = types::mimi_type_to_llvm(self.context, inner_ty)
                    .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                // Register newtype constructor as identity function: Name(value) -> value
                if self.module.get_function(&t.name).is_none() {
                    let meta_ty = types::basic_to_metadata(self.context, llvm_ty);
                    let fn_type = llvm_ty.fn_type(&[meta_ty], false);
                    let ctor = self.module.add_function(
                        &t.name,
                        fn_type,
                        Some(inkwell::module::Linkage::Internal),
                    );
                    let entry = self.context.append_basic_block(ctor, "entry");
                    let prev_block = self.builder.get_insert_block();
                    self.builder.position_at_end(entry);
                    let arg = ctor.get_nth_param(0).ok_or_else(|| {
                        CompileError::LlvmError("newtype ctor missing parameter".to_string())
                    })?;
                    self.builder.build_return(Some(&arg)).map_err(|e| {
                        CompileError::LlvmError(format!("newtype ctor return error: {}", e))
                    })?;
                    if let Some(prev) = prev_block {
                        self.builder.position_at_end(prev);
                    }
                }
                llvm_ty
            }
            crate::ast::TypeDefKind::Union(fields) => {
                // Represent union as a byte array large enough to hold the largest field
                let max_size = fields
                    .iter()
                    .map(|f| {
                        let llvm_ty = types::mimi_type_to_llvm(self.context, &f.ty)
                            .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                        llvm_ty
                            .size_of()
                            .and_then(|s| s.get_zero_extended_constant())
                            .unwrap_or(8)
                    })
                    .max()
                    .unwrap_or(8);
                let array_ty = self.context.i8_type().array_type(max_size as u32);
                BasicTypeEnum::ArrayType(array_ty)
            }
        };
        self.type_llvm.insert(t.name.clone(), llvm_ty);
        self.type_defs.insert(t.name.clone(), t.clone());
        // Track record types for FFI serialization
        if matches!(t.kind, crate::ast::TypeDefKind::Record(_)) {
            self.record_type_names.insert(t.name.clone());
            if t.attributes.contains(&TypeAttribute::ReprC) {
                self.repr_c_record_names.insert(t.name.clone());
            }
        }
        Ok(())
    }

    pub(in crate::codegen) fn register_actor_def(
        &mut self,
        actor: &crate::ast::ActorDef,
    ) -> MimiResult<()> {
        // Represent actor as a struct with fields
        let mut field_tys = Vec::new();
        for f in &actor.fields {
            let ty = self
                .llvm_type_for(&f.ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
            field_tys.push(ty);
        }
        let llvm_ty = BasicTypeEnum::StructType(self.context.struct_type(&field_tys, false));
        self.type_llvm.insert(actor.name.clone(), llvm_ty);

        // Also register as a type definition for field access
        let type_def = crate::ast::TypeDef {
            name: actor.name.clone(),
            pub_: actor.pub_,
            kind: crate::ast::TypeDefKind::Record(
                actor
                    .fields
                    .iter()
                    .map(|f| crate::ast::Field {
                        name: f.name.clone(),
                        ty: f.ty.clone(),
                    })
                    .collect(),
            ),
            generics: Vec::new(),
            derives: Vec::new(),
            attributes: Vec::new(),
        };
        self.type_defs.insert(actor.name.clone(), type_def);
        Ok(())
    }
}
